//! Session-owned concurrency capabilities and saved, single-commit operations.
use super::{RunResult, Session, Status, Value};
use crate::ir::Reg;
use cyallo_concurrent_values::{Handle, OwnedHandle, Payload, Resource, ResourceError};
use cyallo_ir::concurrent::Operation;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex},
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

#[derive(Debug, Clone)]
pub enum ConcurrencyError {
    Start(Arc<cyallo_scheduler::StartError>),
    Spawn(cyallo_scheduler::SpawnError),
    Resource(ResourceError),
    LocalValue,
    InvalidOperand,
    MissingContinuation,
    GlobalAccess,
    ExpectedCompleted,
    InputRequiresCapability,
}
impl PartialEq for ConcurrencyError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Start(a), Self::Start(b)) => Arc::ptr_eq(a, b),
            (Self::Spawn(a), Self::Spawn(b)) => a == b,
            (Self::Resource(a), Self::Resource(b)) => a == b,
            (Self::LocalValue, Self::LocalValue)
            | (Self::InvalidOperand, Self::InvalidOperand)
            | (Self::MissingContinuation, Self::MissingContinuation)
            | (Self::GlobalAccess, Self::GlobalAccess)
            | (Self::ExpectedCompleted, Self::ExpectedCompleted) => true,
            (Self::InputRequiresCapability, Self::InputRequiresCapability) => true,
            _ => false,
        }
    }
}
impl fmt::Display for ConcurrencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(e) => e.fmt(f), Self::Spawn(e) => e.fmt(f), Self::Resource(e) => e.fmt(f),
            Self::LocalValue => f.write_str("a local value cannot cross a coroutine boundary"),
            Self::InvalidOperand => f.write_str("invalid operand for a resolved concurrency operation"),
            Self::MissingContinuation => f.write_str("a waiting coroutine has no saved operation"),
            Self::GlobalAccess => f.write_str("a coroutine cannot access another coroutine's local globals; capture transferable values explicitly"),
            Self::ExpectedCompleted => f.write_str("task did not complete successfully"),
            Self::InputRequiresCapability => f.write_str("coroutine input requires an explicit I/O capability"),
        }
    }
}
impl std::error::Error for ConcurrencyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Start(e) => Some(e.as_ref()),
            Self::Spawn(e) => Some(e),
            Self::Resource(e) => Some(e),
            _ => None,
        }
    }
}
impl From<ResourceError> for ConcurrencyError {
    fn from(value: ResourceError) -> Self {
        Self::Resource(value)
    }
}
#[derive(Default)]
struct Signal {
    epoch: Mutex<u64>,
    changed: Condvar,
}
impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let mut epoch = self.epoch.lock().unwrap();
        *epoch = epoch.wrapping_add(1);
        self.changed.notify_all();
    }
}
type Completion = Pin<Box<dyn Future<Output = RunResult<Value>> + Send>>;
pub(super) enum CompletedOperation {
    Value(Value),
    Load(Result<crate::dynamic_modules::CompiledLoad, cyallo_modules::LoadError>),
    Calls(
        Result<crate::dynamic_modules::CompiledCalls, cyallo_shapes::DynamicError>,
        Box<super::dynamic::SavedAction>,
    ),
}
type SavedCompletion = Pin<Box<dyn Future<Output = RunResult<CompletedOperation>> + Send>>;
#[derive(Default, PartialEq, Eq)]
pub(super) enum EntryLifetime {
    #[default]
    Program,
    Repl,
}
struct Pending {
    destination: Reg,
    // Captured before registering/polling the effect, for failure attribution.
    span: cyallo_ast::Span,
    completion: SavedCompletion,
}
struct Shared {
    factory: cyallo_channel::Factory,
    signal: Arc<Signal>,
    output: Mutex<String>,
}
pub(super) struct State {
    // Only the host root owns the Runtime. A worker retains weak scheduler
    // capabilities, preventing Runtime -> C -> Session -> Runtime cycles.
    owner: Option<cyallo_scheduler::Runtime>,
    shared: Arc<Shared>,
    spawner: cyallo_scheduler::Spawner,
    clock: cyallo_scheduler::Clock,
    waker: Option<Waker>,
    observed: u64,
    pending: Option<Pending>,
}
impl Session {
    pub(super) fn ensure_concurrency(&mut self) -> RunResult<()> {
        if self.concurrency.is_none() {
            let owner = cyallo_scheduler::Runtime::new(self.context.concurrency.clone())
                .map_err(|e| ConcurrencyError::Start(Arc::new(e)))?;
            if self.execution_depth > 0 {
                self.execution_permit = Some(owner.enter().map_err(ConcurrencyError::Spawn)?);
            }
            self.concurrency = Some(State {
                spawner: owner.spawner(),
                clock: owner.clock(),
                owner: Some(owner),
                shared: Arc::new(Shared {
                    factory: cyallo_channel::Factory::new(self.context.max_cells),
                    signal: Arc::default(),
                    output: Mutex::default(),
                }),
                waker: None,
                observed: 0,
                pending: None,
            });
        }
        Ok(())
    }
    /// A synchronous host entry leases the same P budget as scheduler workers.
    /// Native callbacks nest explicitly; no thread-local executor state is used.
    pub(crate) fn enter_execution(&mut self) {
        if self.execution_depth == 0
            && !matches!(self.status, Status::Finished | Status::Failed(_))
            && let Some(owner) = self
                .concurrency
                .as_ref()
                .and_then(|state| state.owner.as_ref())
        {
            match owner.enter() {
                Ok(permit) => self.execution_permit = Some(permit),
                Err(error) => self.fail(
                    ConcurrencyError::Spawn(error),
                    self.current_span().unwrap_or_default(),
                ),
            }
        }
        self.execution_depth += 1;
    }
    pub(crate) fn leave_execution(&mut self) {
        self.execution_depth -= 1;
        if self.execution_depth == 0 {
            self.execution_permit.take();
            if self.entry_lifetime == EntryLifetime::Program
                && matches!(self.status, Status::Finished | Status::Failed(_))
                && let Some(owner) = self
                    .concurrency
                    .as_ref()
                    .and_then(|state| state.owner.as_ref())
            {
                owner.stop();
            }
            self.collect_output();
        }
    }
    /// Wait outside a P. The epoch was recorded before polling, so an early wake
    /// is retained even when completion races the transition to Waiting.
    pub fn wait_for_wakeup(&self, timeout: Duration) {
        if self.status != Status::Waiting {
            return;
        }
        if let Some(state) = &self.concurrency {
            let epoch = state.shared.signal.epoch.lock().unwrap();
            let _ = state
                .shared
                .signal
                .changed
                .wait_timeout_while(epoch, timeout, |epoch| *epoch == state.observed)
                .unwrap();
        }
    }
    pub(crate) fn keep_runtime_between_entries(&mut self) {
        self.entry_lifetime = EntryLifetime::Repl;
    }
    pub fn scheduler_stats(&self) -> Option<cyallo_scheduler::Stats> {
        self.concurrency
            .as_ref()?
            .owner
            .as_ref()
            .map(|owner| owner.stats())
    }
    pub(crate) fn poll_concurrent(&mut self) -> bool {
        if self.status != Status::Waiting {
            return true;
        }
        let Some(state) = self.concurrency.as_mut() else {
            self.fail(
                ConcurrencyError::MissingContinuation,
                self.current_span().unwrap_or_default(),
            );
            return false;
        };
        let Some(mut pending) = state.pending.take() else {
            self.fail(
                ConcurrencyError::MissingContinuation,
                self.current_span().unwrap_or_default(),
            );
            return false;
        };
        state.observed = *state.shared.signal.epoch.lock().unwrap();
        let waker = state
            .waker
            .clone()
            .unwrap_or_else(|| Waker::from(state.shared.signal.clone()));
        match pending
            .completion
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
        {
            Poll::Pending => {
                state.pending = Some(pending);
                false
            }
            Poll::Ready(Ok(CompletedOperation::Load(import))) => {
                self.status = Status::Paused;
                if let Err(error) = self.finish_load(pending.destination, import) {
                    self.fail(error, pending.span);
                }
                // Publication changes code snapshots. Exit any native driver before
                // entering the initializer or reloading code/metadata pointers.
                false
            }
            Poll::Ready(Ok(CompletedOperation::Calls(compiled, action))) => {
                self.status = Status::Paused;
                if let Err(error) = self.finish_dynamic(pending.destination, compiled, *action) {
                    self.fail(error, pending.span);
                }
                false
            }
            Poll::Ready(Ok(CompletedOperation::Value(value))) => {
                self.status = Status::Paused;
                self.set(pending.destination, value);
                true
            }
            Poll::Ready(Err(error)) => {
                self.fail(error, pending.span);
                false
            }
        }
    }
    fn park(&mut self, destination: Reg, completion: Completion) {
        self.park_operation(
            destination,
            Box::pin(async move { completion.await.map(CompletedOperation::Value) }),
        );
        self.poll_concurrent();
    }
    pub(super) fn import_spawner(&self) -> cyallo_scheduler::Spawner {
        self.concurrency.as_ref().unwrap().spawner.clone()
    }
    pub(super) fn park_operation(&mut self, destination: Reg, completion: SavedCompletion) {
        let frame = self.frames.last().unwrap();
        // execute() committed the successor PC before this operation began.
        let span = self.program.functions[frame.function].code[frame.pc - 1].span;
        self.concurrency.as_mut().unwrap().pending = Some(Pending {
            destination,
            span,
            completion,
        });
        self.status = Status::Waiting;
        let state = self.concurrency.as_mut().unwrap();
        state.observed = *state.shared.signal.epoch.lock().unwrap();
        let waker = state
            .waker
            .clone()
            .unwrap_or_else(|| Waker::from(state.shared.signal.clone()));
        // The continuation is installed before submission. Poll once to admit
        // lazy host work and register its waker, but never publish here: this
        // call can still be inside a native machine entry.
        let pending = state.pending.as_mut().unwrap();
        if let Poll::Ready(result) = pending
            .completion
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
        {
            pending.completion = Box::pin(std::future::ready(result));
            waker.wake_by_ref();
        }
    }
    pub(crate) fn resume_pending(&mut self) -> bool {
        if self.status != Status::Waiting {
            return true;
        }
        self.enter_execution();
        let ready = self.poll_concurrent();
        self.leave_execution();
        ready
    }
    fn capability(&self, value: &Value) -> RunResult<Handle> {
        match value {
            Value::Concurrent(handle) => Ok(handle.clone()),
            Value::Address(id) => self.capability(&self.heap[*id].value),
            _ => Err(ConcurrencyError::InvalidOperand.into()),
        }
    }
    pub(super) fn concurrent_output(&mut self, text: &str) {
        if let Some(state) = &self.concurrency {
            let mut output = state.shared.output.lock().unwrap();
            output.push_str(text);
            output.push('\n');
            drop(output);
            state.shared.signal.wake_by_ref();
        } else {
            self.context.output.push_str(text);
            self.context.output.push('\n');
        }
    }
    fn collect_output(&mut self) {
        if let Some(state) = &self.concurrency
            && state.owner.is_some()
        {
            self.context
                .output
                .push_str(&std::mem::take(&mut *state.shared.output.lock().unwrap()));
        }
    }
    fn detached(&self, value: Value) -> RunResult<Payload> {
        Ok(match value {
            Value::Uninitialized => Payload::Unit,
            Value::Number(n) => Payload::Number(n),
            Value::Integer(n) => Payload::Number(cyallo_ast::Number::integer(
                cyallo_ast::NumberKind::I64,
                n as i128,
            )?),
            Value::Real(n) => {
                Payload::Number(cyallo_ast::Number::ieee(cyallo_ast::NumberKind::F64, n))
            }
            Value::Boolean(n) => Payload::Boolean(n),
            Value::Char(n) => Payload::Char(n),
            Value::String(n) => Payload::String(n),
            Value::Array(ids) => Payload::Array(
                ids.into_iter()
                    .map(|id| self.detached(self.heap[id].value.clone()))
                    .collect::<RunResult<_>>()?,
            ),
            Value::Deque(ids) => Payload::Array(
                ids.into_iter()
                    .map(|id| self.detached(self.heap[id].value.clone()))
                    .collect::<RunResult<_>>()?,
            ),
            Value::ArraySnapshot(values) => Payload::Array(
                values
                    .into_iter()
                    .map(|v| self.detached(v))
                    .collect::<RunResult<_>>()?,
            ),
            Value::SparseArray(entries, length) => Payload::Sparse(
                entries
                    .into_iter()
                    .map(|(i, id)| Ok((i, self.detached(self.heap[id].value.clone())?)))
                    .collect::<RunResult<_>>()?,
                length,
            ),
            Value::SparseSnapshot(entries, length) => Payload::Sparse(
                entries
                    .into_iter()
                    .map(|(i, v)| Ok((i, self.detached(v)?)))
                    .collect::<RunResult<_>>()?,
                length,
            ),
            Value::Enum(variant, payload) => Payload::Enum(
                variant,
                payload
                    .map(|v| self.detached(*v).map(Box::new))
                    .transpose()?,
            ),
            Value::EnumStorage(..) | Value::NativeEnumStorage(..) => {
                self.detached(self.snapshot(&value))?
            }
            Value::Concurrent(handle) => Payload::Resource(OwnedHandle::new(handle)),
            // Managed graphs, borrows, erased local pointers and saved generators
            // require their own checked sharing contract. Never copy their IDs.
            _ => return Err(ConcurrencyError::LocalValue.into()),
        })
    }
    pub(super) fn execute_concurrent(
        &mut self,
        destination: Reg,
        operation: Operation,
        values: Vec<Value>,
    ) -> RunResult<()> {
        self.ensure_concurrency()?;
        let unit = || Value::ArraySnapshot(vec![]);
        let integer = |v: &Value| -> RunResult<u64> {
            let n = super::integer(v.clone())?;
            u64::try_from(n).map_err(|_| ConcurrencyError::InvalidOperand.into())
        };
        let value = match operation {
            Operation::Bounded | Operation::Unbounded => {
                let factory = &self.concurrency.as_ref().unwrap().shared.factory;
                let (sender, receiver) = if operation == Operation::Bounded {
                    let capacity = usize::try_from(integer(&values[0])?)
                        .map_err(|_| ConcurrencyError::InvalidOperand)?;
                    factory.bounded(capacity)
                } else {
                    factory.unbounded()
                };
                Value::ArraySnapshot(vec![
                    Value::Concurrent(Handle::new(Resource::Sender(sender))),
                    Value::Concurrent(Handle::new(Resource::Receiver(receiver))),
                ])
            }
            Operation::Clone => Value::Concurrent(
                self.capability(&values[0])?
                    .duplicate()
                    .map_err(ConcurrencyError::Resource)?,
            ),
            Operation::Close | Operation::IsClosed => {
                let h = self.capability(&values[0])?;
                let closed = if let Ok(sender) = h.sender() {
                    if operation == Operation::Close {
                        sender.close();
                    }
                    sender.is_closed()
                } else {
                    let receiver = h.receiver().map_err(ConcurrencyError::Resource)?;
                    if operation == Operation::Close {
                        receiver.close();
                    }
                    receiver.is_closed()
                };
                if operation == Operation::IsClosed {
                    Value::Boolean(closed)
                } else {
                    unit()
                }
            }
            Operation::Send | Operation::TrySend => {
                let sender = self
                    .capability(&values[0])?
                    .sender()
                    .map_err(ConcurrencyError::Resource)?;
                let payload = self.detached(values[1].clone())?;
                if operation == Operation::TrySend {
                    send_result(sender.try_send(payload))
                } else {
                    self.park(
                        destination,
                        Box::pin(async move { Ok(send_result(sender.send(payload).await)) }),
                    );
                    return Ok(());
                }
            }
            Operation::Receive | Operation::TryReceive => {
                let receiver = self
                    .capability(&values[0])?
                    .receiver()
                    .map_err(ConcurrencyError::Resource)?;
                if operation == Operation::TryReceive {
                    receive_result(receiver.try_recv())
                } else {
                    self.park(
                        destination,
                        Box::pin(async move { Ok(receive_result(receiver.recv().await)) }),
                    );
                    return Ok(());
                }
            }
            Operation::SendErrorValue
            | Operation::SendErrorClosed
            | Operation::SendErrorFull
            | Operation::SendErrorLimit => {
                let error = match &values[0] {
                    Value::Address(id) => &self.heap[*id].value,
                    v => v,
                };
                let Some((kind, value)) = error.as_send_error() else {
                    return Err(ConcurrencyError::InvalidOperand.into());
                };
                match operation {
                    Operation::SendErrorValue => value.clone(),
                    Operation::SendErrorClosed => {
                        Value::Boolean(kind == cyallo_channel::SendFailure::Closed)
                    }
                    Operation::SendErrorFull => {
                        Value::Boolean(kind == cyallo_channel::SendFailure::Full)
                    }
                    Operation::SendErrorLimit => {
                        Value::Boolean(kind == cyallo_channel::SendFailure::Limit)
                    }
                    _ => unreachable!(),
                }
            }
            Operation::ReceiveErrorClosed | Operation::ReceiveErrorEmpty => {
                let error = match &values[0] {
                    Value::Address(id) => &self.heap[*id].value,
                    v => v,
                };
                let Value::Enum(variant, _) = error else {
                    return Err(ConcurrencyError::InvalidOperand.into());
                };
                Value::Boolean(
                    *variant
                        == if operation == Operation::ReceiveErrorClosed {
                            cyallo_ast::VariantId::RECEIVE_CLOSED
                        } else {
                            cyallo_ast::VariantId::RECEIVE_EMPTY
                        },
                )
            }
            Operation::Clock => Value::Concurrent(Handle::new(Resource::Clock(
                self.concurrency.as_ref().unwrap().clock.clone(),
            ))),
            Operation::Sleep => {
                let clock = self
                    .capability(&values[0])?
                    .clock()
                    .map_err(ConcurrencyError::Resource)?;
                let sleep = clock.sleep(Duration::from_millis(integer(&values[1])?));
                self.park(
                    destination,
                    Box::pin(async move {
                        sleep.await;
                        Ok(Value::ArraySnapshot(vec![]))
                    }),
                );
                return Ok(());
            }
            Operation::Spawn => {
                let Value::Function(function, captures) = &values[0] else {
                    return Err(ConcurrencyError::InvalidOperand.into());
                };
                let captures = captures
                    .iter()
                    .map(|value| self.detached(value.clone()))
                    .collect::<RunResult<Vec<_>>>()?;
                let state = self.concurrency.as_ref().unwrap();
                let shared = state.shared.clone();
                let spawner = state.spawner.clone();
                let clock = state.clock.clone();
                let mut context = self.initial_context.clone();
                context.input.clear();
                context.output.clear();
                let mut session = Session::with_context(self.program.clone(), context)
                    .map_err(super::RuntimeError::DeferredCheck)?;
                session.frames.clear();
                session.worker_globals = Some(session.program.static_global_count());
                session.concurrency = Some(State {
                    owner: None,
                    shared,
                    spawner: spawner.clone(),
                    clock,
                    waker: None,
                    observed: 0,
                    pending: None,
                });
                session.call(
                    *function,
                    captures.into_iter().map(attached).collect(),
                    None,
                )?;
                let task = spawner
                    .spawn(Guest {
                        session,
                        cleanup: Cleanup::New,
                    })
                    .map_err(ConcurrencyError::Spawn)?;
                Value::Concurrent(Handle::new(Resource::Task(Some(task))))
            }
            Operation::Join => {
                let handle = self.capability(&values[0])?;
                let task = handle.take_task().map_err(ConcurrencyError::Resource)?;
                handle.release();
                self.park(
                    destination,
                    Box::pin(async move {
                        use cyallo_scheduler::Outcome;
                        Ok(Value::TaskOutcome(match task.await {
                            Outcome::Completed(value) => {
                                Outcome::Completed(Box::new(attached(value)))
                            }
                            Outcome::Failed(error) => Outcome::Failed(error),
                            Outcome::Cancelled => Outcome::Cancelled,
                            Outcome::Stopped => Outcome::Stopped,
                        }))
                    }),
                );
                return Ok(());
            }
            Operation::Cancel => {
                self.capability(&values[0])?
                    .cancel_task()
                    .map_err(ConcurrencyError::Resource)?;
                unit()
            }
            Operation::IsFinished => Value::Boolean(
                self.capability(&values[0])?
                    .task_finished()
                    .map_err(ConcurrencyError::Resource)?,
            ),
            Operation::Completed
            | Operation::IsCompleted
            | Operation::IsFailed
            | Operation::IsCancelled
            | Operation::IsStopped => {
                use cyallo_scheduler::Outcome;
                let Value::TaskOutcome(outcome) = &values[0] else {
                    return Err(ConcurrencyError::InvalidOperand.into());
                };
                match operation {
                    Operation::Completed => match outcome {
                        Outcome::Completed(value) => *value.clone(),
                        _ => return Err(ConcurrencyError::ExpectedCompleted.into()),
                    },
                    Operation::IsCompleted => {
                        Value::Boolean(matches!(outcome, Outcome::Completed(_)))
                    }
                    Operation::IsFailed => Value::Boolean(matches!(outcome, Outcome::Failed(_))),
                    Operation::IsCancelled => Value::Boolean(matches!(outcome, Outcome::Cancelled)),
                    Operation::IsStopped => Value::Boolean(matches!(outcome, Outcome::Stopped)),
                    _ => unreachable!(),
                }
            }
        };
        self.set(destination, value);
        Ok(())
    }
}
fn attached(value: Payload) -> Value {
    match value {
        Payload::Enum(variant, payload) => {
            Value::Enum(variant, payload.map(|v| Box::new(attached(*v))))
        }
        Payload::Unit => Value::ArraySnapshot(vec![]),
        Payload::Number(n) => Value::from_number(n),
        Payload::Boolean(n) => Value::Boolean(n),
        Payload::Char(n) => Value::Char(n),
        Payload::String(n) => Value::String(n),
        Payload::Array(values) => Value::ArraySnapshot(values.into_iter().map(attached).collect()),
        Payload::Sparse(values, length) => Value::SparseSnapshot(
            values.into_iter().map(|(i, v)| (i, attached(v))).collect(),
            length,
        ),
        Payload::Resource(handle) => Value::Concurrent(handle.into_handle()),
        Payload::Function(f, captures) => {
            Value::Function(f, captures.into_iter().map(attached).collect())
        }
        Payload::Erased(t, value) => Value::Erased(t, Box::new(attached(*value))),
    }
}
fn send_result(result: Result<(), cyallo_channel::SendError<Payload>>) -> Value {
    Value::result(match result {
        Ok(()) => Ok(Box::new(Value::ArraySnapshot(vec![]))),
        Err(error) => Err(Box::new(Value::send_error(
            error.kind,
            Box::new(attached(error.value)),
        ))),
    })
}
fn receive_result(result: Result<Payload, cyallo_channel::ReceiveError>) -> Value {
    Value::result(match result {
        Ok(value) => Ok(Box::new(attached(value))),
        Err(error) => Err(Box::new(Value::Enum(
            match error {
                cyallo_channel::ReceiveError::Closed => cyallo_ast::VariantId::RECEIVE_CLOSED,
                cyallo_channel::ReceiveError::Empty => cyallo_ast::VariantId::RECEIVE_EMPTY,
            },
            None,
        ))),
    })
}

struct Guest {
    session: Session,
    cleanup: Cleanup,
}
enum Cleanup {
    New,
    Running,
    Complete,
}
impl Guest {
    fn prepare(&mut self, cx: &cyallo_scheduler::Context) {
        let state = self.session.concurrency.as_mut().unwrap();
        state.waker = Some(cx.waker().clone());
        state.spawner = cx.spawner();
    }
    fn unwind(&mut self) -> RunResult<()> {
        self.session.concurrency.as_mut().unwrap().pending.take();
        self.session.prepare_generator_unwind();
        while let Some(frame) = self.session.frames.last() {
            let function = &self.session.program.functions[frame.function];
            let locals: Vec<_> = function
                .locals
                .iter()
                .zip(&function.local_storage)
                .filter_map(|(local, storage)| {
                    if cyallo_ir::ownership::needs_drop(&local.ty)
                        && let crate::ir::LocalStorage::Cell(slot) = storage
                    {
                        Some(frame.locals[*slot])
                    } else {
                        None
                    }
                })
                .collect();
            let mut live = vec![];
            function.resume.live_at(function, frame.pc, &mut live);
            let values: Vec<_> = function
                .registers
                .iter()
                .enumerate()
                .filter_map(|(r, ty)| {
                    if cyallo_ir::ownership::needs_drop(ty)
                        && live
                            .get(r / 64)
                            .is_some_and(|bits| bits & (1 << (r % 64)) != 0)
                    {
                        Some(frame.registers[r].clone())
                    } else {
                        None
                    }
                })
                .collect();
            for cell in locals {
                self.session.drop_cell(cell)?;
            }
            for value in values {
                self.session.drop_value(value)?;
            }
            let mut frame = self.session.frames.pop().unwrap();
            let cells = std::mem::take(&mut frame.owned_cells);
            let lease = frame.service_lease;
            drop(frame);
            self.session.release_cells(cells);
            if let Some(lease) = lease {
                self.session.release_service_lease(lease);
            }
        }
        self.session.close_remaining_generators()?;
        self.session.status = Status::Paused;
        self.session.start_memory_finalizer()?;
        Ok(())
    }
}
impl Drop for Guest {
    fn drop(&mut self) {
        // Abrupt stop and rejected submission release owned capabilities too.
        // This schedules user finalizers but does not execute guest code.
        if !self.session.frames.is_empty() {
            let _ = self.unwind();
        }
    }
}
impl cyallo_scheduler::Coroutine for Guest {
    type Output = Payload;
    fn resume(&mut self, cx: &mut cyallo_scheduler::Context) -> Poll<Payload> {
        self.prepare(cx);
        let before = self.session.instruction_count();
        self.session.run(cx.fuel());
        cx.consume((self.session.instruction_count() - before) as usize);
        match self.session.status.clone() {
            Status::Finished => {
                let value = self
                    .session
                    .final_value
                    .take()
                    .unwrap_or(Value::Uninitialized);
                match self.session.detached(value) {
                    Ok(value) => Poll::Ready(value),
                    Err(error) => {
                        cx.fail(super::RuntimeFailure::new(
                            error,
                            self.session.current_span().unwrap_or_default(),
                        ));
                        Poll::Pending
                    }
                }
            }
            Status::Failed(error) => {
                cx.fail(error);
                Poll::Pending
            }
            Status::Waiting => Poll::Pending,
            Status::WaitingForInput => {
                cx.fail(super::RuntimeFailure::new(
                    ConcurrencyError::InputRequiresCapability.into(),
                    self.session.current_span().unwrap_or_default(),
                ));
                Poll::Pending
            }
            Status::Paused => {
                cx.yield_now();
                Poll::Pending
            }
        }
    }
    fn cancel(&mut self, cx: &mut cyallo_scheduler::Context) -> Poll<()> {
        self.prepare(cx);
        if matches!(self.cleanup, Cleanup::New) {
            self.cleanup = Cleanup::Running;
            if let Err(error) = self.unwind() {
                cx.fail(super::RuntimeFailure::new(
                    error,
                    self.session.current_span().unwrap_or_default(),
                ));
                self.cleanup = Cleanup::Complete;
            }
        }
        if matches!(self.cleanup, Cleanup::Complete) {
            return Poll::Ready(());
        }
        if self.session.frames.is_empty() {
            self.cleanup = Cleanup::Complete;
            return Poll::Ready(());
        }
        let before = self.session.instruction_count();
        self.session.run(cx.fuel());
        cx.consume((self.session.instruction_count() - before) as usize);
        match self.session.status.clone() {
            Status::Waiting => Poll::Pending,
            Status::Paused => {
                cx.yield_now();
                Poll::Pending
            }
            Status::Failed(error) => {
                cx.fail(error);
                self.cleanup = Cleanup::Complete;
                Poll::Ready(())
            }
            _ => {
                self.cleanup = Cleanup::Complete;
                Poll::Ready(())
            }
        }
    }
}
