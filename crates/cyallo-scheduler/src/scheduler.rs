use super::{Failure, StartError};
use cyallo_clock::{Clock, TimerService};
use cyallo_host::BlockingPool;
use std::{
    any::Any,
    collections::{BTreeMap, VecDeque},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context as PollContext, Poll, Wake, Waker},
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone)]
pub struct Config {
    pub processors: usize,
    pub quantum: usize,
    pub max_coroutines: usize,
    pub blocking_threads: usize,
    pub max_blocking_jobs: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            processors: thread::available_parallelism().map_or(1, usize::from),
            quantum: 1024,
            max_coroutines: 100_000,
            blocking_threads: 4,
            max_blocking_jobs: 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoroutineId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnError {
    Stopped,
    Limit,
    GroupClosed,
}
impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SpawnError {}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<T> {
    Completed(T),
    Failed(Failure),
    Cancelled,
    Stopped,
}

/// A bounded piece of a resumable computation. `Pending` must register a wakeup
/// (or call `yield_now`). Cleanup may also return Pending and use the same waker.
pub trait Coroutine: Send + 'static {
    type Output: Send + 'static;
    fn resume(&mut self, context: &mut Context) -> Poll<Self::Output>;
    fn cancel(&mut self, _context: &mut Context) -> Poll<()> {
        Poll::Ready(())
    }
}
type Payload = Box<dyn Any + Send>;
trait Machine: Send {
    fn resume(&mut self, cx: &mut Context) -> Poll<Payload>;
    fn cancel(&mut self, cx: &mut Context) -> Poll<()>;
}
struct Erased<C>(C);
impl<C: Coroutine> Machine for Erased<C> {
    fn resume(&mut self, cx: &mut Context) -> Poll<Payload> {
        self.0.resume(cx).map(|v| Box::new(v) as Payload)
    }
    fn cancel(&mut self, cx: &mut Context) -> Poll<()> {
        self.0.cancel(cx)
    }
}
struct FutureMachine<F>(Pin<Box<F>>);
impl<F: Future + Send + 'static> Coroutine for FutureMachine<F>
where
    F::Output: Send,
{
    type Output = F::Output;
    fn resume(&mut self, cx: &mut Context) -> Poll<Self::Output> {
        self.0
            .as_mut()
            .poll(&mut PollContext::from_waker(&cx.waker))
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Runnable,
    Running,
    Waiting,
    Finished,
}
struct Execution {
    state: State,
    machine: Option<Box<dyn Machine>>,
    outcome: Option<Outcome<Payload>>,
    notified: bool,
    joining: Vec<Waker>,
    observed: bool,
    failure: Option<Failure>,
}
struct C {
    id: CoroutineId,
    core: Weak<Core>,
    execution: Mutex<Execution>,
    done: Condvar,
    cancelled: AtomicBool,
    group: Option<Arc<GroupState>>,
}
impl C {
    fn notify(self: &Arc<Self>) {
        let enqueue = {
            let mut e = self.execution.lock().unwrap();
            match e.state {
                State::Running => {
                    e.notified = true;
                    false
                }
                State::Waiting => {
                    e.state = State::Runnable;
                    true
                }
                State::Runnable | State::Finished => false,
            }
        };
        if enqueue && let Some(core) = self.core.upgrade() {
            core.inject(self.id);
        }
    }
}
struct CWake(Weak<C>);
impl Wake for CWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        if let Some(c) = self.0.upgrade() {
            c.notify();
        }
    }
}

/// C-scoped execution state; never kept by an OS callback. The waker is safe to
/// retain and invoke from any thread, including before `resume` returns.
pub struct Context {
    c: Arc<C>,
    core: Arc<Core>,
    pub(crate) waker: Waker,
    processor: usize,
    remaining: usize,
    failure: Option<Failure>,
}
impl Context {
    pub fn id(&self) -> CoroutineId {
        self.c.id
    }
    pub fn processor(&self) -> usize {
        self.processor
    }
    pub fn waker(&self) -> &Waker {
        &self.waker
    }
    pub fn is_cancelled(&self) -> bool {
        self.c.cancelled.load(Ordering::Acquire)
    }
    pub fn fuel(&self) -> usize {
        self.remaining
    }
    pub fn consume(&mut self, instructions: usize) -> bool {
        if instructions > self.remaining {
            return false;
        }
        self.remaining -= instructions;
        true
    }
    pub fn yield_now(&self) {
        self.waker.wake_by_ref();
    }
    /// Report a guest trap without unwinding Rust. The scheduler cancels the
    /// group's siblings and polls this coroutine's cleanup before joining it.
    pub fn fail(&mut self, error: impl std::error::Error + Send + Sync + 'static) {
        if self.failure.is_none() {
            self.failure = Some(Failure::Guest(Arc::new(error)));
        }
    }
    pub fn clock(&self) -> Clock {
        self.core.timer.clock()
    }
    pub fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> super::Operation<Result<T, cyallo_host::HostError>> {
        self.core.blocking.submit(work)
    }
    pub fn operation<T>(&self) -> (super::Operation<T>, super::Completer<T>) {
        self.core.operations.operation()
    }
    pub fn spawner(&self) -> Spawner {
        Spawner {
            core: Arc::downgrade(&self.core),
            group: self.c.group.clone(),
            member: Some(Arc::downgrade(&self.c)),
            preferred: Some(self.processor),
        }
    }
    pub fn poll<F: Future>(&self, future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut PollContext::from_waker(&self.waker))
    }
}

// P owns its run queue. It is independent of whichever T currently leases it.
struct P {
    queue: Mutex<VecDeque<CoroutineId>>,
    owner: AtomicU64,
    dispatches: AtomicU64,
}
struct Scheduling {
    inject: VecDeque<CoroutineId>,
    idle: Vec<usize>,
    epoch: u64,
    external_waiters: usize,
}
struct Core {
    config: Config,
    processors: Vec<P>,
    scheduling: Mutex<Scheduling>,
    wake: Condvar,
    tasks: Mutex<BTreeMap<CoroutineId, Arc<C>>>,
    next: AtomicU64,
    next_thread: AtomicU64,
    stopped: AtomicBool,
    draining: AtomicBool,
    running: AtomicUsize,
    peak_running: AtomicUsize,
    failures: Mutex<Vec<(CoroutineId, Failure)>>,
    timer: TimerService,
    blocking: BlockingPool,
    operations: Arc<super::OperationRegistry>,
}
impl Core {
    fn inject(&self, id: CoroutineId) {
        let mut s = self.scheduling.lock().unwrap();
        s.inject.push_back(id);
        s.epoch = s.epoch.wrapping_add(1);
        self.wake.notify_all();
    }
    fn spawn<M: Machine + 'static>(
        self: &Arc<Self>,
        machine: M,
        group: Option<Arc<GroupState>>,
        member: Option<&Weak<C>>,
        preferred: Option<usize>,
    ) -> Result<Arc<C>, SpawnError> {
        let mut tasks = self.tasks.lock().unwrap();
        if self.stopped.load(Ordering::Acquire) || self.draining.load(Ordering::Acquire) {
            return Err(SpawnError::Stopped);
        }
        if tasks.len() >= self.config.max_coroutines {
            return Err(SpawnError::Limit);
        }
        if let Some(g) = &group {
            let mut s = g.state.lock().unwrap();
            let admitted = member.and_then(Weak::upgrade).is_some_and(|c| {
                c.group.as_ref().is_some_and(|other| Arc::ptr_eq(other, g))
                    && c.execution.lock().unwrap().state != State::Finished
            });
            if s.cancelling || (s.sealed && !admitted) {
                return Err(SpawnError::GroupClosed);
            }
            s.active += 1;
        }
        let id = CoroutineId(
            self.next
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect("coroutine identity space exhausted"),
        );
        let c = Arc::new(C {
            id,
            core: Arc::downgrade(self),
            execution: Mutex::new(Execution {
                state: State::Runnable,
                machine: Some(Box::new(machine)),
                outcome: None,
                notified: false,
                joining: vec![],
                observed: false,
                failure: None,
            }),
            done: Condvar::new(),
            cancelled: AtomicBool::new(false),
            group,
        });
        tasks.insert(id, c.clone());
        drop(tasks);
        // Processor preference is explicit in the spawner capability.
        if let Some(p) = preferred {
            self.processors[p].queue.lock().unwrap().push_back(id);
            let mut s = self.scheduling.lock().unwrap();
            s.epoch = s.epoch.wrapping_add(1);
            self.wake.notify_all();
        } else {
            self.inject(id);
        }
        Ok(c)
    }
    fn next(&self, p: usize) -> Option<CoroutineId> {
        let processor = &self.processors[p];
        if processor
            .dispatches
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(64)
            && let Some(id) = self.scheduling.lock().unwrap().inject.pop_front()
        {
            return Some(id);
        }
        if let Some(id) = processor.queue.lock().unwrap().pop_front() {
            return Some(id);
        }
        if let Some(id) = self.scheduling.lock().unwrap().inject.pop_front() {
            return Some(id);
        }
        for offset in 1..self.processors.len() {
            let victim = (p + offset) % self.processors.len();
            let mut queue = self.processors[victim].queue.lock().unwrap();
            if let Some(id) = queue.pop_front() {
                let count = queue.len() / 2;
                let stolen: Vec<_> = queue.drain(..count).collect();
                drop(queue);
                self.processors[p].queue.lock().unwrap().extend(stolen);
                return Some(id);
            }
        }
        None
    }
    fn finish(&self, c: &Arc<C>, mut outcome: Outcome<Payload>, machine: Box<dyn Machine>) {
        // User destructors must run without scheduler/task locks.
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(machine))) {
            let error = Failure::Destructor(payload.into());
            outcome = Outcome::Failed(match outcome {
                Outcome::Failed(cause) => Failure::Cleanup {
                    cause: Box::new(cause),
                    cleanup: Box::new(error),
                },
                _ => error,
            });
        }
        let failure = match &outcome {
            Outcome::Failed(e) => Some(e.clone()),
            _ => None,
        };
        let joining = {
            let mut e = c.execution.lock().unwrap();
            e.state = State::Finished;
            e.outcome = Some(outcome);
            c.done.notify_all();
            std::mem::take(&mut e.joining)
        };
        self.tasks.lock().unwrap().remove(&c.id);
        if let Some(error) = &failure {
            self.fail_group(c, error);
        }
        if let Some(g) = &c.group {
            let joining = {
                let mut s = g.state.lock().unwrap();
                s.active -= 1;
                g.done.notify_all();
                if s.active == 0 {
                    std::mem::take(&mut s.joining)
                } else {
                    vec![]
                }
            };
            for wake in joining {
                wake.wake();
            }
        }
        if let Some(error) = failure {
            self.failures.lock().unwrap().push((c.id, error));
        }
        for w in joining {
            w.wake();
        }
        let mut scheduling = self.scheduling.lock().unwrap();
        scheduling.epoch = scheduling.epoch.wrapping_add(1);
        self.wake.notify_all();
    }
    fn fail_group(&self, c: &C, error: &Failure) {
        let Some(g) = &c.group else {
            return;
        };
        {
            let mut state = g.state.lock().unwrap();
            if !state.failures.iter().any(|(id, _)| *id == c.id) {
                state.failures.push((c.id, error.clone()));
            }
            state.cancelling = true;
        }
        let siblings: Vec<_> = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .filter(|other| other.group.as_ref().is_some_and(|v| Arc::ptr_eq(v, g)))
            .cloned()
            .collect();
        for other in siblings {
            other.cancelled.store(true, Ordering::Release);
            other.notify();
        }
    }
    fn execute(self: &Arc<Self>, id: CoroutineId, p: usize) {
        let Some(c) = self.tasks.lock().unwrap().get(&id).cloned() else {
            return;
        };
        let Some(mut machine) = ({
            let mut e = c.execution.lock().unwrap();
            if e.state != State::Runnable {
                return;
            }
            e.state = State::Running;
            e.notified = false;
            e.machine.take()
        }) else {
            return;
        };
        let running = self.running.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_running.fetch_max(running, Ordering::Relaxed);
        let mut cx = Context {
            waker: Waker::from(Arc::new(CWake(Arc::downgrade(&c)))),
            c: c.clone(),
            core: self.clone(),
            processor: p,
            remaining: self.config.quantum,
            failure: None,
        };
        let cancelling = c.cancelled.load(Ordering::Acquire);
        let result = catch_unwind(AssertUnwindSafe(|| {
            if cancelling {
                machine.cancel(&mut cx).map(|()| None)
            } else {
                machine.resume(&mut cx).map(Some)
            }
        }))
        .map_err(|payload| Failure::Panic(payload.into()));
        let result = if let Some(error) = cx.failure.take() {
            Err(error)
        } else {
            result
        };
        self.running.fetch_sub(1, Ordering::AcqRel);
        if self.stopped.load(Ordering::Acquire) {
            self.finish(&c, Outcome::Stopped, machine);
            return;
        }
        match result {
            Ok(Poll::Ready(Some(value))) => self.finish(&c, Outcome::Completed(value), machine),
            Ok(Poll::Ready(None)) => {
                let failure = c.execution.lock().unwrap().failure.take();
                self.finish(
                    &c,
                    failure.map_or(Outcome::Cancelled, Outcome::Failed),
                    machine,
                );
            }
            Err(error) if cancelling => {
                let failure = c.execution.lock().unwrap().failure.take();
                let error = failure.map_or_else(
                    || error.clone(),
                    |original| Failure::Cleanup {
                        cause: Box::new(original),
                        cleanup: Box::new(error.clone()),
                    },
                );
                self.finish(&c, Outcome::Failed(error), machine);
            }
            Err(error) => {
                self.fail_group(&c, &error);
                c.cancelled.store(true, Ordering::Release);
                {
                    let mut e = c.execution.lock().unwrap();
                    e.failure = Some(error);
                    e.machine = Some(machine);
                    e.state = State::Runnable;
                }
                self.inject(id);
            }
            Ok(Poll::Pending) => {
                let again = {
                    let mut e = c.execution.lock().unwrap();
                    e.machine = Some(machine);
                    // Cancellation arriving during a normal resume must wake it;
                    // cleanup itself is allowed to park without a busy loop.
                    let again = e.notified || (!cancelling && c.cancelled.load(Ordering::Acquire));
                    e.state = if again {
                        State::Runnable
                    } else {
                        State::Waiting
                    };
                    again
                };
                if again {
                    self.processors[p].queue.lock().unwrap().push_back(id);
                }
            }
        }
    }
    fn stop(&self) {
        {
            let mut s = self.scheduling.lock().unwrap();
            self.stopped.store(true, Ordering::Release);
            s.epoch = s.epoch.wrapping_add(1);
            self.wake.notify_all();
        }
        self.blocking.stop();
        self.timer.stop();
        let pending: Vec<_> = self.tasks.lock().unwrap().values().cloned().collect();
        for c in pending {
            let machine = {
                let mut e = c.execution.lock().unwrap();
                if matches!(e.state, State::Running | State::Finished) {
                    None
                } else {
                    e.state = State::Running;
                    e.machine.take()
                }
            };
            if let Some(machine) = machine {
                self.finish(&c, Outcome::Stopped, machine);
            }
        }
    }
}

struct Owner {
    core: Arc<Core>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.core.stop();
        let current = thread::current().id();
        for thread in self.threads.get_mut().unwrap().drain(..) {
            if thread.thread().id() != current {
                let _ = thread.join();
            }
        }
        let pending = std::mem::take(&mut *self.core.tasks.lock().unwrap());
        for (_, c) in pending {
            let machine = c.execution.lock().unwrap().machine.take();
            if let Some(machine) = machine {
                self.core.finish(&c, Outcome::Stopped, machine);
            }
        }
    }
}
#[derive(Clone)]
pub struct Runtime {
    owner: Arc<Owner>,
}
/// A host T holds this permit while driving a guest VM continuation.
pub struct ExecutionPermit {
    core: Arc<Core>,
    processor: usize,
    thread: u64,
}
impl ExecutionPermit {
    pub fn processor(&self) -> usize {
        self.processor
    }
}
impl Drop for ExecutionPermit {
    fn drop(&mut self) {
        self.core.running.fetch_sub(1, Ordering::AcqRel);
        let mut state = self.core.scheduling.lock().unwrap();
        assert_eq!(
            self.core.processors[self.processor]
                .owner
                .swap(0, Ordering::AcqRel),
            self.thread
        );
        state.idle.push(self.processor);
        state.epoch = state.epoch.wrapping_add(1);
        self.core.wake.notify_all();
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub processors: usize,
    pub running: usize,
    pub peak_running: usize,
    pub coroutines: usize,
}
impl Runtime {
    pub fn new(config: Config) -> Result<Self, StartError> {
        if config.processors == 0
            || config.quantum == 0
            || config.max_coroutines == 0
            || config.blocking_threads == 0
            || config.max_blocking_jobs == 0
        {
            return Err(StartError::ZeroLimit);
        }
        let operations = Arc::new(super::OperationRegistry::default());
        let blocking = BlockingPool::with_limit(
            config.blocking_threads,
            config.max_blocking_jobs,
            operations.clone(),
        )
        .map_err(StartError::Host)?;
        let next_thread = config.processors as u64 + 1;
        let core = Arc::new(Core {
            processors: (0..config.processors)
                .map(|_| P {
                    queue: Mutex::new(VecDeque::new()),
                    owner: AtomicU64::new(0),
                    dispatches: AtomicU64::new(0),
                })
                .collect(),
            scheduling: Mutex::new(Scheduling {
                inject: VecDeque::new(),
                idle: (0..config.processors).collect(),
                epoch: 0,
                external_waiters: 0,
            }),
            config,
            wake: Condvar::new(),
            tasks: Mutex::new(BTreeMap::new()),
            next: AtomicU64::new(1),
            next_thread: AtomicU64::new(next_thread),
            stopped: AtomicBool::new(false),
            draining: AtomicBool::new(false),
            running: AtomicUsize::new(0),
            peak_running: AtomicUsize::new(0),
            failures: Mutex::new(vec![]),
            timer: TimerService::try_new().map_err(StartError::Timer)?,
            blocking,
            operations,
        });
        let owner = Arc::new(Owner {
            core: core.clone(),
            threads: Mutex::new(vec![]),
        });
        for t in 1..=core.config.processors {
            let core = core.clone();
            let thread = thread::Builder::new()
                .name(format!("cyallo-t-{t}"))
                .spawn(move || run_t(core, t as u64))
                .map_err(StartError::Thread)?;
            owner.threads.lock().unwrap().push(thread);
        }
        Ok(Self { owner })
    }
    pub fn spawner(&self) -> Spawner {
        Spawner {
            core: Arc::downgrade(&self.owner.core),
            group: None,
            member: None,
            preferred: None,
        }
    }
    /// Lease a P for synchronous VM execution. A scheduled C already owns a P
    /// through its Context and must not acquire another permit.
    pub fn enter(&self) -> Result<ExecutionPermit, SpawnError> {
        let core = &self.owner.core;
        let thread = core
            .next_thread
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("execution thread identity space exhausted");
        let mut state = core.scheduling.lock().unwrap();
        state.external_waiters += 1;
        core.wake.notify_all();
        while state.idle.is_empty() && !core.stopped.load(Ordering::Acquire) {
            state = core.wake.wait(state).unwrap();
        }
        state.external_waiters -= 1;
        if core.stopped.load(Ordering::Acquire) {
            core.wake.notify_all();
            return Err(SpawnError::Stopped);
        }
        let processor = state.idle.pop().unwrap();
        assert_eq!(
            core.processors[processor]
                .owner
                .swap(thread, Ordering::AcqRel),
            0
        );
        let running = core.running.fetch_add(1, Ordering::AcqRel) + 1;
        core.peak_running.fetch_max(running, Ordering::Relaxed);
        core.wake.notify_all();
        Ok(ExecutionPermit {
            core: core.clone(),
            processor,
            thread,
        })
    }
    pub fn spawn<C: Coroutine>(&self, c: C) -> Result<Task<C::Output>, SpawnError> {
        self.spawner().spawn(c)
    }
    pub fn spawn_future<F: Future + Send + 'static>(
        &self,
        future: F,
    ) -> Result<Task<F::Output>, SpawnError>
    where
        F::Output: Send,
    {
        self.spawn(FutureMachine(Box::pin(future)))
    }
    pub fn clock(&self) -> Clock {
        self.owner.core.timer.clock()
    }
    pub fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> super::Operation<Result<T, cyallo_host::HostError>> {
        self.owner.core.blocking.submit(work)
    }
    pub fn operation<T>(&self) -> (super::Operation<T>, super::Completer<T>) {
        self.owner.core.operations.operation()
    }
    pub fn group(&self) -> Group {
        Group {
            spawner: Spawner {
                core: Arc::downgrade(&self.owner.core),
                group: Some(Arc::new(GroupState {
                    state: Mutex::new(GroupData {
                        active: 0,
                        sealed: false,
                        cancelling: false,
                        failures: vec![],
                        joining: vec![],
                    }),
                    done: Condvar::new(),
                })),
                member: None,
                preferred: None,
            },
        }
    }
    pub fn stats(&self) -> Stats {
        let c = &self.owner.core;
        Stats {
            processors: c.processors.len(),
            running: c.running.load(Ordering::Acquire),
            peak_running: c.peak_running.load(Ordering::Relaxed),
            coroutines: c.tasks.lock().unwrap().len(),
        }
    }
    pub fn failures(&self) -> Vec<(CoroutineId, Failure)> {
        self.owner.core.failures.lock().unwrap().clone()
    }
    /// Host-side graceful shutdown. New external submissions are rejected by
    /// cancellation; cleanup is polled on processors until it completes.
    pub fn cancel_all(&self) {
        let core = &self.owner.core;
        let tasks = {
            let tasks = core.tasks.lock().unwrap();
            core.draining.store(true, Ordering::Release);
            tasks.values().cloned().collect::<Vec<_>>()
        };
        for c in tasks {
            c.cancelled.store(true, Ordering::Release);
            c.notify();
        }
    }
    pub fn stop(&self) {
        self.owner.core.stop();
    }
    pub fn cancel_and_wait(&self) {
        self.cancel_all();
        self.wait();
    }
    /// Host-side blocking drain. Coroutines should await join handles instead.
    pub fn wait(&self) {
        let core = &self.owner.core;
        let mut s = core.scheduling.lock().unwrap();
        while !core.tasks.lock().unwrap().is_empty() {
            s = core.wake.wait(s).unwrap();
        }
    }
}
impl Default for Runtime {
    fn default() -> Self {
        Self::new(Config::default()).expect("valid runtime configuration")
    }
}

fn run_t(core: Arc<Core>, t: u64) {
    loop {
        let p = {
            let mut s = core.scheduling.lock().unwrap();
            loop {
                if core.stopped.load(Ordering::Acquire) {
                    return;
                }
                if s.external_waiters == 0
                    && let Some(p) = s.idle.pop()
                {
                    break p;
                }
                s = core.wake.wait(s).unwrap();
            }
        };
        assert_eq!(core.processors[p].owner.swap(t, Ordering::AcqRel), 0);
        loop {
            if core.stopped.load(Ordering::Acquire) {
                break;
            }
            let epoch = core.scheduling.lock().unwrap().epoch;
            if let Some(c) = core.next(p) {
                core.execute(c, p);
                let mut state = core.scheduling.lock().unwrap();
                if state.external_waiters != 0 {
                    assert_eq!(core.processors[p].owner.swap(0, Ordering::AcqRel), t);
                    state.idle.push(p);
                    core.wake.notify_all();
                    break;
                }
                continue;
            }
            let mut s = core.scheduling.lock().unwrap();
            if s.epoch != epoch {
                continue;
            }
            assert_eq!(core.processors[p].owner.swap(0, Ordering::AcqRel), t);
            s.idle.push(p);
            core.wake.notify_all();
            while s.epoch == epoch && !core.stopped.load(Ordering::Acquire) {
                s = core.wake.wait(s).unwrap();
            }
            break;
        }
        if core.stopped.load(Ordering::Acquire) {
            if core.processors[p].owner.load(Ordering::Acquire) == t {
                core.processors[p].owner.store(0, Ordering::Release);
            }
            return;
        }
    }
}

#[derive(Clone)]
pub struct Spawner {
    core: Weak<Core>,
    group: Option<Arc<GroupState>>,
    member: Option<Weak<C>>,
    preferred: Option<usize>,
}
impl Spawner {
    /// Admits owned host work without retaining an execution permit.
    pub fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<super::Operation<Result<T, cyallo_host::HostError>>, cyallo_host::HostError> {
        let core = self.core.upgrade().ok_or(cyallo_host::HostError::Stopped)?;
        Ok(core.blocking.submit(work))
    }
    pub fn spawn<C: Coroutine>(&self, c: C) -> Result<Task<C::Output>, SpawnError> {
        let core = self.core.upgrade().ok_or(SpawnError::Stopped)?;
        let c = core.spawn(
            Erased(c),
            self.group.clone(),
            self.member.as_ref(),
            self.preferred,
        )?;
        Ok(Task {
            c,
            _output: std::marker::PhantomData,
        })
    }
    pub fn spawn_future<F: Future + Send + 'static>(
        &self,
        future: F,
    ) -> Result<Task<F::Output>, SpawnError>
    where
        F::Output: Send,
    {
        self.spawn(FutureMachine(Box::pin(future)))
    }
}
pub struct Task<T> {
    c: Arc<C>,
    _output: std::marker::PhantomData<T>,
}
#[derive(Clone)]
pub struct Cancellation {
    c: Weak<C>,
}
impl Cancellation {
    pub fn cancel(&self) -> bool {
        let Some(c) = self.c.upgrade() else {
            return false;
        };
        if c.execution.lock().unwrap().state == State::Finished {
            return false;
        }
        c.cancelled.store(true, Ordering::Release);
        c.notify();
        true
    }
}
impl<T: Send + 'static> Task<T> {
    pub fn id(&self) -> CoroutineId {
        self.c.id
    }
    pub fn cancel(&self) {
        self.c.cancelled.store(true, Ordering::Release);
        self.c.notify();
    }
    pub fn cancel_handle(&self) -> Cancellation {
        Cancellation {
            c: Arc::downgrade(&self.c),
        }
    }
    pub fn is_finished(&self) -> bool {
        self.c.execution.lock().unwrap().state == State::Finished
    }
    /// Blocking host join. Await this handle when already inside a coroutine.
    pub fn join(self) -> Outcome<T> {
        let mut e = self.c.execution.lock().unwrap();
        while e.state != State::Finished {
            e = self.c.done.wait(e).unwrap();
        }
        e.observed = true;
        decode(e.outcome.take().expect("single-consumer join"))
    }
    pub fn cancel_and_join(self) -> Outcome<T> {
        self.cancel();
        self.join()
    }
}
fn decode<T: Send + 'static>(outcome: Outcome<Payload>) -> Outcome<T> {
    match outcome {
        Outcome::Completed(value) => {
            Outcome::Completed(*value.downcast::<T>().expect("task output type"))
        }
        Outcome::Failed(e) => Outcome::Failed(e),
        Outcome::Cancelled => Outcome::Cancelled,
        Outcome::Stopped => Outcome::Stopped,
    }
}
impl<T: Send + 'static> Future for Task<T> {
    type Output = Outcome<T>;
    fn poll(self: Pin<&mut Self>, cx: &mut PollContext<'_>) -> Poll<Self::Output> {
        let next = cx.waker().clone();
        let mut e = self.c.execution.lock().unwrap();
        if e.state == State::Finished {
            e.observed = true;
            Poll::Ready(decode(e.outcome.take().expect("single-consumer join")))
        } else {
            if !e.joining.iter().any(|w| w.will_wake(cx.waker())) {
                e.joining.push(next);
            }
            Poll::Pending
        }
    }
}
struct GroupData {
    active: usize,
    sealed: bool,
    cancelling: bool,
    failures: Vec<(CoroutineId, Failure)>,
    joining: Vec<Waker>,
}
struct GroupState {
    state: Mutex<GroupData>,
    done: Condvar,
}
pub struct Group {
    spawner: Spawner,
}
pub struct GroupWait {
    state: Arc<GroupState>,
}
impl Future for GroupWait {
    type Output = Vec<(CoroutineId, Failure)>;
    fn poll(self: Pin<&mut Self>, cx: &mut PollContext<'_>) -> Poll<Self::Output> {
        let next = cx.waker().clone();
        let mut s = self.state.state.lock().unwrap();
        if s.active == 0 {
            return Poll::Ready(s.failures.clone());
        }
        if !s.joining.iter().any(|w| w.will_wake(cx.waker())) {
            s.joining.push(next);
        }
        Poll::Pending
    }
}
impl Group {
    pub fn spawn<C: Coroutine>(&self, c: C) -> Result<Task<C::Output>, SpawnError> {
        self.spawner.spawn(c)
    }
    pub fn spawn_future<F: Future + Send + 'static>(
        &self,
        future: F,
    ) -> Result<Task<F::Output>, SpawnError>
    where
        F::Output: Send,
    {
        self.spawner.spawn_future(future)
    }
    pub fn wait(&self) -> Vec<(CoroutineId, Failure)> {
        let g = self.spawner.group.as_ref().unwrap();
        let mut s = g.state.lock().unwrap();
        s.sealed = true;
        while s.active != 0 {
            s = g.done.wait(s).unwrap();
        }
        s.failures.clone()
    }
    /// Seal external submissions and wait without occupying a T or a P.
    /// Already admitted group members can still spawn children until they exit.
    pub fn wait_async(&self) -> GroupWait {
        let state = self.spawner.group.as_ref().unwrap().clone();
        state.state.lock().unwrap().sealed = true;
        GroupWait { state }
    }
    pub fn cancel_and_wait_async(&self) -> GroupWait {
        self.cancel();
        self.wait_async()
    }
    pub fn cancel(&self) {
        let g = self.spawner.group.as_ref().unwrap();
        g.state.lock().unwrap().cancelling = true;
        if let Some(core) = self.spawner.core.upgrade() {
            let tasks: Vec<_> = core
                .tasks
                .lock()
                .unwrap()
                .values()
                .filter(|c| c.group.as_ref().is_some_and(|v| Arc::ptr_eq(v, g)))
                .cloned()
                .collect();
            for c in tasks {
                c.cancelled.store(true, Ordering::Release);
                c.notify();
            }
        }
    }
    pub fn cancel_and_wait(&self) -> Vec<(CoroutineId, Failure)> {
        self.cancel();
        self.wait()
    }
}
