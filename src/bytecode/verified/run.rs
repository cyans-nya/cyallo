use super::{
    BooleanOp, CallSite, CallTarget, Contract, DebugPlace, DynamicOp, Extremum, FloatOp,
    FrameLayout, Frozen, FunctionId, GlobalId, HostBindingId, HostFailure, InputKind, Instruction,
    InstructionId, IntegerOp, OutputPiece, OutputTemplate, Register, RegisterType, Scalar,
    ScalarType, Shape, StatementArrival, StatementBoundary, TextFailure, TextMapOp, TextOp,
    TextStyle, TextTestOp, ValueType, VerifiedFunction, VerifiedModule, array_len,
};
use crate::{
    Span,
    integer::{IntegerDivision, IntegerError},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// Total instructions the activation may execute across all slices.
    pub fuel: usize,
    /// Maximum simultaneous frames, including the entry.
    pub frames: usize,
}

/// A logical bytecode location in the activation's verified module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionPosition {
    pub function: FunctionId,
    pub instruction: InstructionId,
    pub span: Span,
}

/// Boundary reported to the caller after a slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Execution can resume at the next instruction.
    Paused(ExecutionPosition),
    Returned(Option<Scalar>),
    Failed(RunError),
}

/// Why a generator cannot be advanced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorFault {
    /// Resumed after it returned or was closed.
    Finished,
    AlreadyRunning,
    /// Resumed before `start`.
    NotStarted,
    /// Started twice.
    AlreadyStarted,
    /// The generator table is full.
    LimitExceeded,
}

/// Why a storage path cannot be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFault {
    NegativeIndex,
    OutOfBounds,
    Mismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunError {
    ArgumentCount {
        expected: usize,
        actual: usize,
    },
    ArgumentType {
        index: usize,
        expected: ScalarType,
        actual: ScalarType,
    },
    FuelExhausted,
    FrameLimit,
    /// The entry routine returns a value an activation cannot hand back.
    EntryResultNotScalar,
    /// The entry routine takes a parameter that is not a scalar.
    EntryParameterNotScalar {
        index: usize,
    },
    IntegerOverflow {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    DivisionByZero {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    ZeroStep {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// Every task is waiting for another, or a lone task waits for a channel
    /// nobody can serve.
    Deadlock {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// A task captured something that cannot cross into another task.
    NotTransferable {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// The task table is full.
    TaskLimit {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// The channel table is full.
    ChannelLimit {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// A generator was advanced in a state that does not allow it.
    Generator {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
        fault: GeneratorFault,
    },
    /// `random` was asked for a bound that is not a positive finite number.
    RandomBound {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// A service operation failed.
    Service {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
        fault: ServiceFault,
    },
    /// An object operation failed.
    Object {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
        fault: ObjectFault,
    },
    /// A storage path does not lead anywhere in the value it is applied to.
    Path {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
        fault: PathFault,
    },
    /// An allocator refused to provide the block a counted object needs.
    AllocationRejected {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// An input instruction found no line left.
    InputExhausted {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    /// The line does not spell a value of the requested kind.
    InputInvalid {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
        kind: InputKind,
    },
    EnumVariant {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    MovedValue {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    HeapExhausted {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    UninitializedGlobal {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    IndexOutOfBounds {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    UninitializedElement {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    Text {
        failure: TextFailure,
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    ConversionOutOfRange {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    ReachedTrap {
        function: FunctionId,
        instruction: InstructionId,
        span: Span,
    },
    BoundCallFailed {
        binding: HostBindingId,
        position: ExecutionPosition,
        cause: HostFailure,
    },
    UnexpectedBoundFailure {
        binding: HostBindingId,
        position: ExecutionPosition,
        cause: HostFailure,
    },
    BoundResultType {
        binding: HostBindingId,
        position: ExecutionPosition,
        expected: ScalarType,
        actual: ScalarType,
    },
    BoundCallPanicked {
        binding: HostBindingId,
        position: ExecutionPosition,
    },
}

impl RunError {
    /// The source span of the failing instruction, when the failure has one.
    pub fn span(&self) -> Option<Span> {
        match self {
            Self::RandomBound { span, .. }
            | Self::IntegerOverflow { span, .. }
            | Self::DivisionByZero { span, .. }
            | Self::ZeroStep { span, .. }
            | Self::Deadlock { span, .. }
            | Self::NotTransferable { span, .. }
            | Self::TaskLimit { span, .. }
            | Self::ChannelLimit { span, .. }
            | Self::Generator { span, .. }
            | Self::Service { span, .. }
            | Self::Object { span, .. }
            | Self::Path { span, .. }
            | Self::AllocationRejected { span, .. }
            | Self::InputExhausted { span, .. }
            | Self::InputInvalid { span, .. }
            | Self::EnumVariant { span, .. }
            | Self::MovedValue { span, .. }
            | Self::HeapExhausted { span, .. }
            | Self::UninitializedGlobal { span, .. }
            | Self::IndexOutOfBounds { span, .. }
            | Self::UninitializedElement { span, .. }
            | Self::Text { span, .. }
            | Self::ConversionOutOfRange { span, .. }
            | Self::ReachedTrap { span, .. } => Some(*span),
            _ => None,
        }
    }
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArgumentCount { expected, actual } => {
                write!(f, "expected {expected} arguments, got {actual}")
            }
            Self::ArgumentType {
                index,
                expected,
                actual,
            } => write!(f, "argument {index} expects {expected:?}, got {actual:?}"),
            Self::FuelExhausted => f.write_str("scalar bytecode exhausted its total fuel"),
            Self::FrameLimit => f.write_str("scalar bytecode exhausted its call frame limit"),
            Self::EntryResultNotScalar => f.write_str("the entry routine does not return a scalar"),
            Self::EntryParameterNotScalar { index } => {
                write!(f, "entry parameter {index} is not a scalar")
            }
            Self::IntegerOverflow {
                function,
                instruction,
                ..
            } => write!(
                f,
                "integer overflow in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::DivisionByZero {
                function,
                instruction,
                ..
            } => write!(
                f,
                "division by zero in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::RandomBound { .. } => f.write_str("random bound must be a positive number"),
            Self::Service { fault, .. } => f.write_str(match fault {
                ServiceFault::InactiveContext => "the service context was disposed",
                ServiceFault::NotInjected => "the context has no provider for the service",
                ServiceFault::Unavailable => "the service provider was withdrawn",
                ServiceFault::ProviderConflict => "the service already has a provider here",
                ServiceFault::Limit => "too many service contexts or registrations",
            }),
            Self::Object { fault, .. } => f.write_str(match fault {
                ObjectFault::Missing => "the object has no such property",
                ObjectFault::Readonly => "the object is readonly",
                ObjectFault::Contract => "the write breaks the type a property is required to keep",
                ObjectFault::Type => "the erased value is not of the type it is read as",
            }),
            Self::Deadlock { .. } => {
                f.write_str("deadlock: the task waits for a channel that no other task can serve")
            }
            Self::NotTransferable { .. } => {
                f.write_str("a task cannot capture a value that is not transferable")
            }
            Self::TaskLimit { .. } => f.write_str("task limit exceeded"),
            Self::ChannelLimit { .. } => f.write_str("channel limit exceeded"),
            Self::Generator { fault, .. } => f.write_str(match fault {
                GeneratorFault::Finished => "cannot resume a completed or closed generator",
                GeneratorFault::AlreadyRunning => "generator is already running",
                GeneratorFault::NotStarted => "generator has not started; call start() first",
                GeneratorFault::AlreadyStarted => {
                    "generator has already started; continue it with resume()"
                }
                GeneratorFault::LimitExceeded => "generator limit exceeded",
            }),
            Self::Path { fault, .. } => f.write_str(match fault {
                PathFault::NegativeIndex => "storage path index must not be negative",
                PathFault::OutOfBounds => "storage path projection out of bounds",
                PathFault::Mismatch => "storage path does not fit the value it is applied to",
            }),
            Self::AllocationRejected { .. } => f.write_str("the allocator rejected the allocation"),
            Self::InputExhausted { .. } => f.write_str("INPUT reached end of input"),
            Self::InputInvalid { kind, .. } => match kind {
                InputKind::Text => f.write_str("INPUT line is not valid text"),
                InputKind::Integer => f.write_str("INPUT is not a valid integer"),
                InputKind::Float => f.write_str("INPUT is not a valid number"),
                InputKind::Bool => f.write_str("INPUT is not TRUE or FALSE"),
            },
            Self::ZeroStep {
                function,
                instruction,
                ..
            } => write!(
                f,
                "zero loop step in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::EnumVariant {
                function,
                instruction,
                ..
            } => write!(
                f,
                "enum payload read of another variant in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::MovedValue {
                function,
                instruction,
                ..
            } => write!(
                f,
                "read of a moved-out value in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::HeapExhausted {
                function,
                instruction,
                ..
            } => write!(
                f,
                "the heap is full in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::UninitializedGlobal {
                function,
                instruction,
                ..
            } => write!(
                f,
                "read of an unassigned module variable in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::IndexOutOfBounds {
                function,
                instruction,
                ..
            } => write!(
                f,
                "array index out of bounds in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::UninitializedElement {
                function,
                instruction,
                ..
            } => write!(
                f,
                "read of an uninitialized array element in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::Text {
                failure,
                function,
                instruction,
                ..
            } => write!(
                f,
                "text operation failed ({failure:?}) in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::ConversionOutOfRange {
                function,
                instruction,
                ..
            } => write!(
                f,
                "float is not representable as an integer in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::ReachedTrap {
                function,
                instruction,
                ..
            } => write!(
                f,
                "trap in function {} at instruction {}",
                function.0, instruction.0
            ),
            Self::BoundCallFailed { binding, cause, .. } => {
                write!(f, "bound call {} failed: {cause}", binding.0)
            }
            Self::UnexpectedBoundFailure { binding, cause, .. } => {
                write!(f, "infallible bound call {} failed: {cause}", binding.0)
            }
            Self::BoundResultType {
                binding,
                expected,
                actual,
                ..
            } => {
                write!(
                    f,
                    "bound call {} returned {actual:?}, expected {expected:?}",
                    binding.0
                )
            }
            Self::BoundCallPanicked { binding, .. } => {
                write!(f, "bound call {} panicked", binding.0)
            }
        }
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BoundCallFailed { cause, .. } | Self::UnexpectedBoundFailure { cause, .. } => {
                Some(cause)
            }
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Frame {
    function: FunctionId,
    pc: usize,
    arrival: StatementArrival,
    slots: Box<[Option<FrameValue>]>,
    destination: Option<Register>,
    /// The value a returned storage path is applied to, for a mapped call.
    mapped: Option<Box<FrameValue>>,
    /// The counted object whose allocator this frame deallocates for.
    finalizing: Option<usize>,
    /// The generator this frame is the body of while it runs, and the
    /// register of the resumer that receives the step.
    generator: Option<(usize, Register)>,
    /// What the returned value becomes in an erased result: its type, or none
    /// for a routine that returns nothing.
    wrap: Option<Option<ValueType>>,
    /// The loaded module whose namespace this frame builds.
    namespace: Option<usize>,
    /// The service context this frame runs the setup or cleanup of.
    scope: Option<usize>,
    /// The provider binding whose method this frame runs.
    lease: Option<usize>,
}

#[derive(Debug, Clone)]
enum FrameValue {
    Scalar(Scalar),
    Loan(BorrowedPlace),
    Text(Arc<str>),
    Composite(Composite),
    Closure(Arc<Closure>),
    /// A traced reference to the heap object at this index.
    Handle(usize),
    /// A storage path: its steps, or nothing for the missing path. Steps hold
    /// indices only, so a path owns no object.
    Path(Option<Arc<[PathStep]>>),
    /// A block an allocator handed out, by its serial number.
    Allocation(u64),
    /// The generator at this index of the generator table. Its owner releases
    /// it, and the suspended frame it holds is a root for the collector.
    Generator(usize),
    /// The sending end of the channel at this index of the channel table.
    Sender(usize),
    /// The receiving end of the channel at this index.
    Receiver(usize),
    /// The task at this index of the task table.
    Task(usize),
    /// A capability to sleep.
    Clock,
    /// The object whose properties are at this index of the heap.
    OpenObject(usize),
    /// The properties of an object, as the heap holds them.
    Properties(Box<ObjectData>),
    /// A value with the type it had when it was erased.
    Dynamic(Box<(ValueType, FrameValue)>),
    /// Why a module could not be loaded or an object not viewed.
    Failure(Arc<FailureData>),
    /// An owned value with its type: it owns what it holds.
    Any(Box<(ValueType, FrameValue)>),
    /// The service context at this index.
    Context(usize),
    /// The registration of the provider binding at this index.
    Registration(usize),
    /// The service key at this index of the linked keys.
    ServiceKey(usize),
    /// A live reference to a key's service in a context.
    ServiceRef(usize, usize),
    /// A native function, by declaration number.
    NativeCallable(u32),
    /// A retained reference to a field of a heap record: a place rooted in a
    /// heap object, which the collector keeps alive.
    Location(BorrowedPlace),
    /// A strong owner of the counted object at this index.
    Rc(usize),
    /// A weak reference to the counted object at this index.
    Weak(usize),
    /// The hole a value was moved out of.
    Moved,
    /// A shared object, erased to the trait the implementation belongs to.
    Object {
        heap: usize,
        implementation: u32,
    },
}

/// Why a service operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFault {
    /// The context was disposed.
    InactiveContext,
    /// The context sees no provider for the key.
    NotInjected,
    /// The provider was withdrawn.
    Unavailable,
    /// The key already has a provider in this realm.
    ProviderConflict,
    /// Too many contexts or registrations.
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceState {
    Active,
    Retiring,
    Inactive,
}

#[derive(Debug)]
struct ServiceBinding {
    owner: usize,
    key: usize,
    realm: u64,
    state: ServiceState,
    value: Option<FrameValue>,
    /// Calls in flight on the provider.
    leases: usize,
}

#[derive(Debug)]
struct ServiceView {
    parent: Option<usize>,
    realms: HashMap<usize, u64>,
    provided: HashMap<usize, usize>,
    admitted: HashMap<usize, usize>,
    state: ServiceState,
    /// An injected view sees only what it was admitted to.
    checked: bool,
    cleanups: Vec<FrameValue>,
}

/// Service contexts, providers and what depends on them.
#[derive(Debug, Default)]
struct Services {
    active: HashMap<(usize, u64), usize>,
    views: Vec<ServiceView>,
    bindings: Vec<ServiceBinding>,
    next_realm: u64,
    retirement_pending: bool,
}

const MAX_SERVICE_OBJECTS: usize = 1 << 20;

impl Services {
    fn new_view(&mut self, parent: Option<usize>) -> Option<usize> {
        if self.views.len() >= MAX_SERVICE_OBJECTS {
            return None;
        }
        let (realms, checked, admitted) = match parent {
            Some(parent) => (
                self.views[parent].realms.clone(),
                self.views[parent].checked,
                self.views[parent].admitted.clone(),
            ),
            None => (HashMap::new(), false, HashMap::new()),
        };
        self.views.push(ServiceView {
            parent,
            realms,
            provided: HashMap::new(),
            admitted,
            state: ServiceState::Active,
            checked,
            cleanups: Vec::new(),
        });
        Some(self.views.len() - 1)
    }

    fn realm(&self, view: usize, key: usize) -> u64 {
        self.views[view].realms.get(&key).copied().unwrap_or(0)
    }

    /// The binding a context resolves a key to, or why none.
    fn selected(&self, view: usize, key: usize, cleanup: bool) -> Result<usize, ServiceFault> {
        let v = &self.views[view];
        if v.state == ServiceState::Inactive || (v.state == ServiceState::Retiring && !cleanup) {
            return Err(ServiceFault::InactiveContext);
        }
        let own = v.provided.get(&key).copied().filter(|&binding| {
            self.bindings[binding].state == ServiceState::Active
                || (cleanup && self.bindings[binding].state == ServiceState::Retiring)
        });
        let admitted = v.admitted.get(&key).copied();
        let routed = self.active.get(&(key, self.realm(view, key))).copied();
        let binding = if v.checked {
            own.or(admitted)
        } else {
            own.or(routed)
        }
        .ok_or(ServiceFault::NotInjected)?;
        let state = self.bindings[binding].state;
        if state == ServiceState::Inactive
            || (state == ServiceState::Retiring
                && !(cleanup
                    && (admitted == Some(binding)
                        || v.provided.get(&key).copied() == Some(binding))))
        {
            return Err(ServiceFault::Unavailable);
        }
        Ok(binding)
    }

    fn collect_binding_cleanup(&mut self, binding: usize, plan: &mut Vec<(usize, FrameValue)>) {
        if self.bindings[binding].state != ServiceState::Active {
            return;
        }
        self.retirement_pending = true;
        self.bindings[binding].state = ServiceState::Retiring;
        let (key, realm) = (self.bindings[binding].key, self.bindings[binding].realm);
        self.active.remove(&(key, realm));
        let dependents: Vec<usize> = self
            .views
            .iter()
            .enumerate()
            .filter(|(_, view)| {
                view.admitted.get(&key).copied() == Some(binding)
                    && view.state == ServiceState::Active
            })
            .map(|(id, _)| id)
            .collect();
        for view in dependents {
            self.collect_view_cleanup(view, plan);
        }
    }

    fn collect_view_cleanup(&mut self, view: usize, plan: &mut Vec<(usize, FrameValue)>) {
        if self.views[view].state != ServiceState::Active {
            return;
        }
        self.retirement_pending = true;
        self.views[view].state = ServiceState::Retiring;
        let children: Vec<usize> = self
            .views
            .iter()
            .enumerate()
            .filter(|(_, v)| v.parent == Some(view) && v.state == ServiceState::Active)
            .map(|(id, _)| id)
            .collect();
        for child in children {
            self.collect_view_cleanup(child, plan);
        }
        let provided: Vec<usize> = self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.owner == view && b.state == ServiceState::Active)
            .map(|(id, _)| id)
            .collect();
        for binding in provided {
            self.collect_binding_cleanup(binding, plan);
        }
        for callback in self.views[view].cleanups.iter().rev() {
            plan.push((view, callback.clone()));
        }
    }

    /// Release views and providers that nothing needs any more. `scopes` are
    /// the contexts that frames are running setup or cleanup of.
    fn release_retired(&mut self, scopes: &HashSet<usize>) {
        if !self.retirement_pending {
            return;
        }
        for (id, view) in self.views.iter_mut().enumerate() {
            if view.state == ServiceState::Retiring && !scopes.contains(&id) {
                view.state = ServiceState::Inactive;
                view.cleanups.clear();
                view.admitted.clear();
            }
        }
        for binding in 0..self.bindings.len() {
            let b = &self.bindings[binding];
            if b.state == ServiceState::Retiring
                && b.leases == 0
                && !scopes.contains(&b.owner)
                && !self.views.iter().any(|view| {
                    view.state != ServiceState::Inactive
                        && view.admitted.values().any(|&admitted| admitted == binding)
                })
            {
                self.bindings[binding].value = None;
                self.bindings[binding].state = ServiceState::Inactive;
            }
        }
        self.retirement_pending = self
            .views
            .iter()
            .any(|view| view.state == ServiceState::Retiring)
            || self
                .bindings
                .iter()
                .any(|binding| binding.state == ServiceState::Retiring);
    }
}

/// The contexts frames are running setup or cleanup of, in every task.
fn service_scopes(stack: &[Frame], memory: &Memory) -> HashSet<usize> {
    stack
        .iter()
        .chain(
            memory
                .tasks
                .iter()
                .flatten()
                .flat_map(|task| task.stack.iter()),
        )
        .filter_map(|frame| frame.scope)
        .collect()
}

/// Why a load or a view failed, as the program can ask.
#[derive(Debug)]
struct FailureData {
    message: Box<str>,
    not_found: bool,
    denied: bool,
    check: bool,
}

impl FailureData {
    fn plain(message: &str) -> Arc<Self> {
        Arc::new(Self {
            message: message.into(),
            not_found: false,
            denied: false,
            check: false,
        })
    }
}

/// Check a view's properties and require them, through every alias.
fn view_object(data: &mut ObjectData, contract: &Contract) -> Result<(), ObjectFault> {
    let mut requirements = Vec::with_capacity(contract.fields.len());
    for (name, expected) in contract.fields.iter() {
        let at = data.find(name).ok_or(ObjectFault::Missing)?;
        let property = &data.properties[at];
        if *expected != ValueType::Dynamic && property.value.0 != *expected {
            return Err(ObjectFault::Type);
        }
        if !data.readonly
            && let Some(old) = property.required
            && old != ValueType::Dynamic
            && *expected != ValueType::Dynamic
            && old != *expected
        {
            return Err(ObjectFault::Contract);
        }
        requirements.push((at, *expected));
    }
    if !data.readonly {
        for (at, expected) in requirements {
            data.properties[at].required = Some(expected);
        }
    }
    Ok(())
}

/// The properties of an object. A readonly object accepts no writes.
#[derive(Debug, Clone)]
struct ObjectData {
    readonly: bool,
    properties: Vec<Property>,
}

#[derive(Debug, Clone)]
struct Property {
    name: Arc<str>,
    /// The value and the type it was stored with, never `Dynamic`.
    value: (ValueType, FrameValue),
    /// The type every write must keep, when a literal or a view requires one.
    required: Option<ValueType>,
}

impl ObjectData {
    fn find(&self, name: &str) -> Option<usize> {
        self.properties
            .iter()
            .position(|property| &*property.name == name)
    }
}

/// Why an object operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFault {
    /// The property does not exist.
    Missing,
    /// The object is readonly.
    Readonly,
    /// The write or view would break a required property's type.
    Contract,
    /// An erased value is not of the type it is read as.
    Type,
}

/// One projection of a storage path.
#[derive(Debug, Clone, Copy)]
enum PathStep {
    /// Into the contents of a cell, handle or counted owner.
    Target,
    Field(u32),
    Element(usize),
}

/// A routine and the values it captured by copy or move. The captures become
/// the routine's leading arguments.
#[derive(Debug)]
struct Closure {
    function: FunctionId,
    captures: Box<[FrameValue]>,
}

/// A tuple or enum value. Components are owned by their composite and are
/// released with it.
#[derive(Debug, Clone)]
enum Composite {
    Tuple(Box<[FrameValue]>),
    Enum {
        variant: u32,
        payload: Option<Box<FrameValue>>,
    },
    /// Row-major elements. A declared element stays uninitialized until stored.
    Array(Box<[Option<FrameValue>]>),
    /// A growable sequence of initialized elements.
    Vec(Vec<FrameValue>),
    /// A mutable cell.
    Cell(Box<FrameValue>),
}

/// Frames above this index may move as the stack grows. The owning logical
/// frame and its verified register remain stable until borrowers return.
#[derive(Debug, Clone)]
struct BorrowedPlace {
    root: Root,
    /// Projections from the owning register to the borrowed place.
    path: Box<[Step]>,
    /// The elements a slice covers, when the place is viewed as a slice.
    window: Option<Window>,
}

/// A run of elements of an array: where it starts and how many it spans.
#[derive(Debug, Clone, Copy)]
struct Window {
    start: usize,
    len: usize,
}

/// Most elements a vector may hold.
const MAX_VEC_ELEMENTS: usize = 1 << 22;
/// Most objects the heap may hold after a collection.
const MAX_HEAP_OBJECTS: usize = 1 << 22;
/// Live objects at which the first collection runs.
const INITIAL_COLLECTION_THRESHOLD: usize = 256;

/// Occupancy of an activation's heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    pub live: usize,
    pub peak: usize,
    pub collections: usize,
    /// Counted objects that still have an owner or a weak reference.
    pub counted: usize,
}

/// A mark-sweep heap of objects that handles refer to. Handles are indices,
/// so a collection never moves what a handle means.
#[derive(Debug)]
struct Heap {
    slots: Vec<Option<FrameValue>>,
    free: Vec<usize>,
    live: usize,
    peak: usize,
    collections: usize,
    threshold: usize,
    stress: bool,
}

impl Heap {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            peak: 0,
            collections: 0,
            threshold: INITIAL_COLLECTION_THRESHOLD,
            stress: false,
        }
    }

    fn should_collect(&self) -> bool {
        self.stress || self.live >= self.threshold
    }

    /// Store a value, or `None` when the heap is at its limit.
    fn allocate(&mut self, value: FrameValue) -> Option<usize> {
        if self.live >= MAX_HEAP_OBJECTS {
            return None;
        }
        let index = match self.free.pop() {
            Some(index) => {
                self.slots[index] = Some(value);
                index
            }
            None => {
                self.slots.push(Some(value));
                self.slots.len() - 1
            }
        };
        self.live += 1;
        self.peak = self.peak.max(self.live);
        Some(index)
    }
}

/// Everything an activation owns outside its frames.
#[derive(Debug)]
struct Memory {
    /// Module-level variables. They start uninitialized.
    globals: Vec<Option<FrameValue>>,
    heap: Heap,
    /// Counted objects, by index. A freed block leaves a hole.
    counted: Vec<Option<CountedBlock>>,
    free_counted: Vec<usize>,
    /// Counted objects whose last owner is gone and whose allocator has not
    /// yet been asked to take the block back.
    finalizers: VecDeque<usize>,
    /// The number of allocations handed out so far.
    allocations: u64,
    /// Generators, by index. A released generator leaves a hole.
    generators: Vec<Option<GeneratorSlot>>,
    free_generators: Vec<usize>,
    /// Channels, by index. A channel with no ends left leaves a hole.
    channels: Vec<Option<Channel>>,
    free_channels: Vec<usize>,
    /// Tasks, by index; index 0 is the entry task. The frames of the task that
    /// is running are on the activation's stack, not here.
    tasks: Vec<Option<TaskRecord>>,
    free_tasks: Vec<usize>,
    /// The task whose frames are on the stack.
    current_task: usize,
    /// The channel the running task is parked on, as a receiver.
    waiting: Option<usize>,
    /// When the running task's sleep ends.
    sleeping_until: Option<Instant>,
    /// The state of the pseudo-random generator.
    random_seed: u64,
    /// The namespace object of each module loaded so far, by load key.
    namespaces: HashMap<usize, FrameValue>,
    /// The adapter routine of each native function and argument types called
    /// through an erased value.
    adapters: HashMap<(u32, Vec<ValueType>), usize>,
    /// The loaded modules whose initializers have run.
    initialized: HashSet<usize>,
    services: Services,
}

/// More tasks than this exhaust the table.
const MAX_TASKS: usize = 1 << 16;

/// Instructions a task runs before another ready task gets a turn.
const QUANTUM: usize = 1000;

/// A task other than the one running, or one that has ended.
#[derive(Debug)]
struct TaskRecord {
    stack: Vec<Frame>,
    end: Option<TaskEnd>,
    cancelled: bool,
    waiting: Option<usize>,
    sleeping_until: Option<Instant>,
}

#[derive(Debug)]
enum TaskEnd {
    Completed(Option<FrameValue>),
    Failed,
    Cancelled,
}

/// A channel: a queue, a capacity and counted ends.
#[derive(Debug)]
struct Channel {
    capacity: Option<usize>,
    closed: bool,
    queue: VecDeque<FrameValue>,
    senders: usize,
    receivers: usize,
    /// Receivers parked on an empty queue. A rendezvous send may hand its value
    /// to one of them even though the channel has no buffer.
    waiting: usize,
}

impl Channel {
    /// Whether a send finds room: space in the buffer, or a receiver waiting.
    fn has_room(&self) -> bool {
        self.capacity
            .is_none_or(|capacity| self.queue.len() < capacity + self.waiting)
    }
}

/// More channels than this exhaust the table.
const MAX_CHANNELS: usize = 1 << 20;

/// Where a generator is in its life.
#[derive(Debug, Clone, Copy)]
enum GeneratorState {
    /// Made, not started.
    New,
    /// Suspended at a yield: the register that receives the next resume
    /// value, and the stack index the frame ran at.
    Suspended { input: Register, depth: usize },
    /// Its frame is on the stack.
    Running,
    /// Returned or closed.
    Done,
}

#[derive(Debug)]
struct GeneratorSlot {
    state: GeneratorState,
    /// The suspended frame; absent while it runs and once it is done.
    frame: Option<Frame>,
}

/// More generators than this exhaust the table.
const MAX_GENERATORS: usize = 1 << 20;

/// A counted object: its owners, its weak references and its payload, which
/// is gone once the last owner is released.
#[derive(Debug)]
struct CountedBlock {
    strong: usize,
    weak: usize,
    payload: Option<FrameValue>,
    /// The allocator the object came from, when it has one.
    allocator: Option<AllocatorState>,
    /// The deallocation routine is running; the block outlives it.
    finalizing: bool,
}

/// An allocator and the block it handed out for a counted object. The
/// routine gives the block back once the last owner is released.
#[derive(Debug)]
struct AllocatorState {
    state: Option<FrameValue>,
    deallocate: FunctionId,
    allocation: u64,
}

impl Memory {
    fn new(globals: usize) -> Self {
        Self {
            globals: vec![None; globals],
            heap: Heap::new(),
            counted: Vec::new(),
            free_counted: Vec::new(),
            finalizers: VecDeque::new(),
            allocations: 0,
            generators: Vec::new(),
            free_generators: Vec::new(),
            channels: Vec::new(),
            free_channels: Vec::new(),
            tasks: vec![Some(TaskRecord {
                stack: Vec::new(),
                end: None,
                cancelled: false,
                waiting: None,
                sleeping_until: None,
            })],
            free_tasks: Vec::new(),
            current_task: 0,
            waiting: None,
            sleeping_until: None,
            random_seed: 0x9e37_79b9_7f4a_7c15,
            namespaces: HashMap::new(),
            adapters: HashMap::new(),
            initialized: HashSet::new(),
            services: Services::default(),
        }
    }

    fn new_counted(&mut self, payload: FrameValue) -> usize {
        self.new_counted_in(payload, None)
    }

    fn new_counted_in(&mut self, payload: FrameValue, allocator: Option<AllocatorState>) -> usize {
        let block = CountedBlock {
            strong: 1,
            weak: 0,
            payload: Some(payload),
            allocator,
            finalizing: false,
        };
        match self.free_counted.pop() {
            Some(index) => {
                self.counted[index] = Some(block);
                index
            }
            None => {
                self.counted.push(Some(block));
                self.counted.len() - 1
            }
        }
    }

    fn block(&mut self, index: usize) -> &mut CountedBlock {
        match self.counted[index].as_mut() {
            Some(block) => block,
            None => unreachable!("a value refers to a freed counted object"),
        }
    }

    /// Release what a value owns. Counts fall, a payload is destroyed with its
    /// last owner, and the block is freed once nothing refers to it.
    fn drop_value(&mut self, value: FrameValue) {
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            match value {
                FrameValue::Rc(index) => {
                    let block = self.block(index);
                    block.strong -= 1;
                    if block.strong == 0 {
                        pending.extend(block.payload.take());
                        if block.allocator.is_some() {
                            block.finalizing = true;
                            self.finalizers.push_back(index);
                        }
                        self.free_if_unused(index);
                    }
                }
                FrameValue::Weak(index) => {
                    self.block(index).weak -= 1;
                    self.free_if_unused(index);
                }
                FrameValue::Sender(index) => {
                    let channel = self.channel(index);
                    channel.senders -= 1;
                    if channel.senders == 0 {
                        // Receivers still drain what was queued.
                        channel.closed = true;
                    }
                    self.free_channel_if_unused(index);
                }
                FrameValue::Receiver(index) => {
                    let channel = self.channel(index);
                    channel.receivers -= 1;
                    if channel.receivers == 0 {
                        // Nobody is left to read what was queued.
                        channel.closed = true;
                        pending.extend(channel.queue.drain(..));
                    }
                    self.free_channel_if_unused(index);
                }
                FrameValue::Any(erased) => pending.push(erased.1),
                FrameValue::Context(_)
                | FrameValue::Registration(_)
                | FrameValue::ServiceKey(_)
                | FrameValue::ServiceRef(..)
                | FrameValue::NativeCallable(_) => {}
                FrameValue::Generator(index) => {
                    self.close_generator(index);
                    self.generators[index] = None;
                    self.free_generators.push(index);
                }
                FrameValue::Composite(Composite::Tuple(fields)) => {
                    pending.extend(fields.into_vec())
                }
                FrameValue::Composite(Composite::Enum { payload, .. }) => {
                    pending.extend(payload.map(|payload| *payload));
                }
                FrameValue::Composite(Composite::Array(elements)) => {
                    pending.extend(elements.into_vec().into_iter().flatten());
                }
                FrameValue::Composite(Composite::Vec(items)) => pending.extend(items),
                FrameValue::Composite(Composite::Cell(inner)) => pending.push(*inner),
                FrameValue::Scalar(_)
                | FrameValue::Text(_)
                | FrameValue::Path(_)
                | FrameValue::Location(_)
                | FrameValue::Allocation(_)
                | FrameValue::Task(_)
                | FrameValue::Clock
                | FrameValue::OpenObject(_)
                | FrameValue::Properties(_)
                | FrameValue::Dynamic(_)
                | FrameValue::Failure(_)
                | FrameValue::Loan(_)
                | FrameValue::Closure(_)
                | FrameValue::Handle(_)
                | FrameValue::Object { .. }
                | FrameValue::Moved => {}
            }
        }
    }

    fn free_if_unused(&mut self, index: usize) {
        let block = self.block(index);
        if block.strong == 0 && block.weak == 0 && !block.finalizing {
            self.counted[index] = None;
            self.free_counted.push(index);
        }
    }

    /// Stop counting the running task as parked on a channel.
    fn stop_waiting(&mut self) {
        if let Some(id) = self.waiting.take()
            && let Some(Some(channel)) = self.channels.get_mut(id)
        {
            channel.waiting -= 1;
        }
    }

    fn live_tasks(&self) -> usize {
        self.tasks
            .iter()
            .flatten()
            .filter(|task| task.end.is_none())
            .count()
    }

    /// The earliest moment a sleeping task wakes, if any task sleeps.
    fn earliest_wake(&self) -> Option<Instant> {
        self.tasks
            .iter()
            .flatten()
            .filter(|task| task.end.is_none())
            .filter_map(|task| task.sleeping_until)
            .chain(self.sleeping_until)
            .min()
    }

    /// A new channel with one sender and one receiver, `None` when the table
    /// is full.
    fn new_channel(&mut self, capacity: Option<usize>) -> Option<usize> {
        let channel = Channel {
            capacity,
            closed: false,
            queue: VecDeque::new(),
            senders: 1,
            receivers: 1,
            waiting: 0,
        };
        if let Some(index) = self.free_channels.pop() {
            self.channels[index] = Some(channel);
            Some(index)
        } else if self.channels.len() < MAX_CHANNELS {
            self.channels.push(Some(channel));
            Some(self.channels.len() - 1)
        } else {
            None
        }
    }

    fn channel(&mut self, index: usize) -> &mut Channel {
        match self.channels[index].as_mut() {
            Some(channel) => channel,
            None => unreachable!("a value refers to a released channel"),
        }
    }

    fn free_channel_if_unused(&mut self, index: usize) {
        let channel = self.channel(index);
        if channel.senders == 0 && channel.receivers == 0 {
            self.channels[index] = None;
            self.free_channels.push(index);
        }
    }

    /// Keep a new generator's frame. `None` when the table is full.
    fn new_generator(&mut self, frame: Frame) -> Option<usize> {
        let slot = GeneratorSlot {
            state: GeneratorState::New,
            frame: Some(frame),
        };
        if let Some(index) = self.free_generators.pop() {
            self.generators[index] = Some(slot);
            Some(index)
        } else if self.generators.len() < MAX_GENERATORS {
            self.generators.push(Some(slot));
            Some(self.generators.len() - 1)
        } else {
            None
        }
    }

    fn generator(&mut self, index: usize) -> &mut GeneratorSlot {
        match self.generators[index].as_mut() {
            Some(slot) => slot,
            None => unreachable!("a value refers to a released generator"),
        }
    }

    /// Finish a generator: what its suspended frame owns is released.
    fn close_generator(&mut self, index: usize) {
        let slot = self.generator(index);
        slot.state = GeneratorState::Done;
        if let Some(frame) = slot.frame.take() {
            for value in frame.slots.into_vec().into_iter().flatten() {
                self.drop_value(value);
            }
        }
    }

    /// The deallocation routine of a block has returned: release the allocator
    /// and free the block once nothing refers to it.
    fn finish_finalizer(&mut self, index: usize) {
        let block = self.block(index);
        block.finalizing = false;
        let state = block.allocator.take().and_then(|allocator| allocator.state);
        if let Some(state) = state {
            self.drop_value(state);
        }
        self.free_if_unused(index);
    }

    /// A copy of a value. A counted owner or weak reference inside gains one,
    /// and a handle keeps its identity.
    fn clone_value(&mut self, value: &FrameValue) -> FrameValue {
        match value {
            FrameValue::Rc(block) => {
                self.block(*block).strong += 1;
                FrameValue::Rc(*block)
            }
            FrameValue::Weak(block) => {
                self.block(*block).weak += 1;
                FrameValue::Weak(*block)
            }
            FrameValue::Any(erased) => {
                FrameValue::Any(Box::new((erased.0, self.clone_value(&erased.1))))
            }
            FrameValue::Sender(index) => {
                self.channel(*index).senders += 1;
                FrameValue::Sender(*index)
            }
            FrameValue::Receiver(index) => {
                self.channel(*index).receivers += 1;
                FrameValue::Receiver(*index)
            }
            FrameValue::Composite(Composite::Tuple(fields)) => FrameValue::Composite(
                Composite::Tuple(fields.iter().map(|field| self.clone_value(field)).collect()),
            ),
            FrameValue::Composite(Composite::Enum { variant, payload }) => {
                FrameValue::Composite(Composite::Enum {
                    variant: *variant,
                    payload: payload
                        .as_deref()
                        .map(|payload| Box::new(self.clone_value(payload))),
                })
            }
            FrameValue::Composite(Composite::Array(elements)) => {
                FrameValue::Composite(Composite::Array(
                    elements
                        .iter()
                        .map(|element| element.as_ref().map(|element| self.clone_value(element)))
                        .collect(),
                ))
            }
            FrameValue::Composite(Composite::Vec(items)) => FrameValue::Composite(Composite::Vec(
                items.iter().map(|item| self.clone_value(item)).collect(),
            )),
            FrameValue::Composite(Composite::Cell(inner)) => {
                FrameValue::Composite(Composite::Cell(Box::new(self.clone_value(inner))))
            }
            FrameValue::Closure(closure) => FrameValue::Closure(Arc::new(Closure {
                function: closure.function,
                captures: closure
                    .captures
                    .iter()
                    .map(|capture| self.clone_value(capture))
                    .collect(),
            })),
            other => other.clone(),
        }
    }

    /// Free every object that no frame, variable or live object reaches.
    fn collect(&mut self, stack: &[Frame]) {
        let mut marks = vec![false; self.heap.slots.len()];
        let mut pending = Vec::new();
        for value in stack
            .iter()
            .flat_map(|frame| frame.slots.iter().flatten())
            .chain(self.globals.iter().flatten())
            .chain(
                self.counted
                    .iter()
                    .flatten()
                    .filter_map(|block| block.payload.as_ref()),
            )
            .chain(
                self.counted
                    .iter()
                    .flatten()
                    .filter_map(|block| block.allocator.as_ref()?.state.as_ref()),
            )
            .chain(
                self.generators
                    .iter()
                    .flatten()
                    .filter_map(|slot| slot.frame.as_ref())
                    .flat_map(|frame| frame.slots.iter().flatten()),
            )
            .chain(
                self.channels
                    .iter()
                    .flatten()
                    .flat_map(|channel| channel.queue.iter()),
            )
            .chain(self.namespaces.values())
            .chain(
                self.services
                    .bindings
                    .iter()
                    .filter_map(|binding| binding.value.as_ref()),
            )
            .chain(
                self.services
                    .views
                    .iter()
                    .flat_map(|view| view.cleanups.iter()),
            )
        {
            trace(value, &mut marks, &mut pending);
        }
        while let Some(object) = pending.pop() {
            if let Some(value) = &self.heap.slots[object] {
                trace(value, &mut marks, &mut pending);
            }
        }
        for (index, marked) in marks.iter().enumerate() {
            if !marked && self.heap.slots[index].take().is_some() {
                self.heap.free.push(index);
                self.heap.live -= 1;
            }
        }
        self.heap.collections += 1;
        self.heap.threshold = INITIAL_COLLECTION_THRESHOLD.max(self.heap.live * 2);
    }
}

/// Mark every object a value refers to. Objects found are queued so their own
/// contents are visited once.
fn trace(value: &FrameValue, marks: &mut [bool], pending: &mut Vec<usize>) {
    let mut mark = |object: usize| {
        if !std::mem::replace(&mut marks[object], true) {
            pending.push(object);
        }
    };
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            FrameValue::Handle(object)
            | FrameValue::OpenObject(object)
            | FrameValue::Object { heap: object, .. } => mark(*object),
            FrameValue::Properties(data) => {
                stack.extend(data.properties.iter().map(|property| &property.value.1));
            }
            FrameValue::Dynamic(erased) | FrameValue::Any(erased) => stack.push(&erased.1),
            FrameValue::Context(_)
            | FrameValue::Registration(_)
            | FrameValue::ServiceKey(_)
            | FrameValue::ServiceRef(..)
            | FrameValue::NativeCallable(_) => {}
            FrameValue::Loan(place) | FrameValue::Location(place) => {
                if let Root::Heap(object) = place.root {
                    mark(object);
                }
            }
            FrameValue::Composite(Composite::Tuple(fields)) => stack.extend(fields.iter()),
            FrameValue::Composite(Composite::Enum { payload, .. }) => {
                stack.extend(payload.as_deref());
            }
            FrameValue::Composite(Composite::Array(elements)) => {
                stack.extend(elements.iter().flatten());
            }
            FrameValue::Composite(Composite::Vec(items)) => stack.extend(items.iter()),
            FrameValue::Composite(Composite::Cell(inner)) => stack.push(inner),
            FrameValue::Closure(closure) => stack.extend(closure.captures.iter()),
            FrameValue::Scalar(_)
            | FrameValue::Text(_)
            | FrameValue::Failure(_)
            | FrameValue::Path(_)
            | FrameValue::Allocation(_)
            | FrameValue::Generator(_)
            | FrameValue::Sender(_)
            | FrameValue::Receiver(_)
            | FrameValue::Task(_)
            | FrameValue::Clock
            | FrameValue::Rc(_)
            | FrameValue::Weak(_)
            | FrameValue::Moved => {}
        }
    }
}

/// What a borrowed place starts from.
#[derive(Debug, Clone, Copy)]
enum Root {
    /// A register of the frame at this stack index.
    Frame { frame: usize, register: Register },
    /// A module-level variable.
    Global(GlobalId),
    /// The heap object at this index.
    Heap(usize),
    /// The payload of the counted object at this index.
    Counted(usize),
    /// The allocator state of the counted object at this index.
    Allocator(usize),
}

/// One projection step into a composite value.
#[derive(Debug, Clone, Copy)]
enum Step {
    Field(u32),
    Payload(u32),
    /// A checked element offset into a row-major array.
    Offset(usize),
}

/// Why a place cannot be read or written.
#[derive(Debug, Clone, Copy)]
enum PlaceFault {
    OtherVariant,
    UninitializedElement,
    MovedOut,
}

fn read_place(value: &FrameValue, path: &[Step]) -> Result<FrameValue, PlaceFault> {
    let Some((step, rest)) = path.split_first() else {
        return match value {
            FrameValue::Moved => Err(PlaceFault::MovedOut),
            value => Ok(value.clone()),
        };
    };
    match (step, value) {
        (Step::Field(index), FrameValue::Composite(Composite::Tuple(fields))) => {
            read_place(&fields[*index as usize], rest)
        }
        (
            Step::Payload(variant),
            FrameValue::Composite(Composite::Enum {
                variant: held,
                payload,
            }),
        ) => match payload {
            Some(payload) if held == variant => read_place(payload, rest),
            _ => Err(PlaceFault::OtherVariant),
        },
        (Step::Offset(offset), FrameValue::Composite(Composite::Array(elements))) => {
            match &elements[*offset] {
                Some(element) => read_place(element, rest),
                None => Err(PlaceFault::UninitializedElement),
            }
        }
        (Step::Offset(offset), FrameValue::Composite(Composite::Vec(items))) => {
            read_place(&items[*offset], rest)
        }
        _ => unreachable!("verified projection path"),
    }
}

fn place_ref<'a>(value: &'a FrameValue, path: &[Step]) -> Result<&'a FrameValue, PlaceFault> {
    let Some((step, rest)) = path.split_first() else {
        return match value {
            FrameValue::Moved => Err(PlaceFault::MovedOut),
            value => Ok(value),
        };
    };
    match (step, value) {
        (Step::Field(index), FrameValue::Composite(Composite::Tuple(fields))) => {
            place_ref(&fields[*index as usize], rest)
        }
        (
            Step::Payload(variant),
            FrameValue::Composite(Composite::Enum {
                variant: held,
                payload,
            }),
        ) => match payload {
            Some(payload) if held == variant => place_ref(payload, rest),
            _ => Err(PlaceFault::OtherVariant),
        },
        (Step::Offset(offset), FrameValue::Composite(Composite::Array(elements))) => {
            match &elements[*offset] {
                Some(element) => place_ref(element, rest),
                None => Err(PlaceFault::UninitializedElement),
            }
        }
        (Step::Offset(offset), FrameValue::Composite(Composite::Vec(items))) => {
            place_ref(&items[*offset], rest)
        }
        _ => unreachable!("verified projection path"),
    }
}

fn place_mut<'a>(
    value: &'a mut FrameValue,
    path: &[Step],
) -> Result<&'a mut FrameValue, PlaceFault> {
    let Some((step, rest)) = path.split_first() else {
        return Ok(value);
    };
    match (step, value) {
        (Step::Field(index), FrameValue::Composite(Composite::Tuple(fields))) => {
            place_mut(&mut fields[*index as usize], rest)
        }
        (
            Step::Payload(variant),
            FrameValue::Composite(Composite::Enum {
                variant: held,
                payload,
            }),
        ) => match payload {
            Some(payload) if held == variant => place_mut(payload, rest),
            _ => Err(PlaceFault::OtherVariant),
        },
        (Step::Offset(offset), FrameValue::Composite(Composite::Array(elements))) => {
            match &mut elements[*offset] {
                Some(element) => place_mut(element, rest),
                None => Err(PlaceFault::UninitializedElement),
            }
        }
        (Step::Offset(offset), FrameValue::Composite(Composite::Vec(items))) => {
            place_mut(&mut items[*offset], rest)
        }
        _ => unreachable!("verified projection path"),
    }
}

fn write_place(value: &mut FrameValue, path: &[Step], new: FrameValue) -> Result<(), PlaceFault> {
    let Some((step, rest)) = path.split_first() else {
        *value = new;
        return Ok(());
    };
    match (step, value) {
        (Step::Field(index), FrameValue::Composite(Composite::Tuple(fields))) => {
            write_place(&mut fields[*index as usize], rest, new)
        }
        (
            Step::Payload(variant),
            FrameValue::Composite(Composite::Enum {
                variant: held,
                payload,
            }),
        ) => match payload {
            Some(payload) if held == variant => write_place(payload, rest, new),
            _ => Err(PlaceFault::OtherVariant),
        },
        (Step::Offset(offset), FrameValue::Composite(Composite::Vec(items))) => {
            write_place(&mut items[*offset], rest, new)
        }
        (Step::Offset(offset), FrameValue::Composite(Composite::Array(elements))) => {
            let element = &mut elements[*offset];
            match (rest.is_empty(), element) {
                (true, element) => {
                    *element = Some(new);
                    Ok(())
                }
                (false, Some(element)) => write_place(element, rest, new),
                (false, None) => Err(PlaceFault::UninitializedElement),
            }
        }
        _ => unreachable!("verified projection path"),
    }
}

impl Frame {
    fn new(
        function: FunctionId,
        body: &VerifiedFunction,
        values: impl Iterator<Item = FrameValue>,
        destination: Option<Register>,
    ) -> Self {
        let layout = body.frame_layout();
        let mut frame = Self {
            function,
            pc: 0,
            arrival: StatementArrival::Entry,
            slots: vec![None; layout.slot_count()].into_boxed_slice(),
            destination,
            mapped: None,
            finalizing: None,
            generator: None,
            wrap: None,
            namespace: None,
            scope: None,
            lease: None,
        };
        for (&register, value) in body.parameters.iter().zip(values) {
            frame.set_value(layout, register, value);
        }
        frame
    }

    fn get(&self, layout: &FrameLayout, register: Register) -> Scalar {
        match self.value(layout, register) {
            FrameValue::Scalar(value) => value,
            FrameValue::Loan(_)
            | FrameValue::Text(_)
            | FrameValue::Composite(_)
            | FrameValue::Closure(_)
            | FrameValue::Handle(_)
            | FrameValue::Rc(_)
            | FrameValue::Weak(_)
            | FrameValue::Object { .. }
            | FrameValue::Path(_)
            | FrameValue::Location(_)
            | FrameValue::Allocation(_)
            | FrameValue::Generator(_)
            | FrameValue::Sender(_)
            | FrameValue::Receiver(_)
            | FrameValue::Task(_)
            | FrameValue::Clock
            | FrameValue::OpenObject(_)
            | FrameValue::Properties(_)
            | FrameValue::Dynamic(_)
            | FrameValue::Failure(_)
            | FrameValue::Any(_)
            | FrameValue::Context(_)
            | FrameValue::Registration(_)
            | FrameValue::ServiceKey(_)
            | FrameValue::ServiceRef(..)
            | FrameValue::NativeCallable(_)
            | FrameValue::Moved => {
                unreachable!("verified scalar operand")
            }
        }
    }

    fn loan(&self, layout: &FrameLayout, register: Register) -> BorrowedPlace {
        match self.value(layout, register) {
            FrameValue::Loan(place) => place,
            FrameValue::Scalar(_)
            | FrameValue::Text(_)
            | FrameValue::Composite(_)
            | FrameValue::Closure(_)
            | FrameValue::Handle(_)
            | FrameValue::Rc(_)
            | FrameValue::Weak(_)
            | FrameValue::Object { .. }
            | FrameValue::Path(_)
            | FrameValue::Location(_)
            | FrameValue::Allocation(_)
            | FrameValue::Generator(_)
            | FrameValue::Sender(_)
            | FrameValue::Receiver(_)
            | FrameValue::Task(_)
            | FrameValue::Clock
            | FrameValue::OpenObject(_)
            | FrameValue::Properties(_)
            | FrameValue::Dynamic(_)
            | FrameValue::Failure(_)
            | FrameValue::Any(_)
            | FrameValue::Context(_)
            | FrameValue::Registration(_)
            | FrameValue::ServiceKey(_)
            | FrameValue::ServiceRef(..)
            | FrameValue::NativeCallable(_)
            | FrameValue::Moved => {
                unreachable!("verified loan operand")
            }
        }
    }

    fn value(&self, layout: &FrameLayout, register: Register) -> FrameValue {
        let Some(value) = self.slots[slot(layout, register)].clone() else {
            // Verification requires initialization along every reachable path to a read.
            unreachable!("read of uninitialized verified storage");
        };
        value
    }

    fn slot_mut(&mut self, layout: &FrameLayout, register: Register) -> &mut Option<FrameValue> {
        &mut self.slots[slot(layout, register)]
    }

    fn composite(&self, layout: &FrameLayout, register: Register) -> &Composite {
        match &self.slots[slot(layout, register)] {
            Some(FrameValue::Composite(value)) => value,
            _ => unreachable!("verified composite operand"),
        }
    }

    fn text(&self, layout: &FrameLayout, register: Register) -> &Arc<str> {
        match &self.slots[slot(layout, register)] {
            Some(FrameValue::Text(text)) => text,
            _ => unreachable!("verified text operand"),
        }
    }

    fn composite_mut(&mut self, layout: &FrameLayout, register: Register) -> &mut Composite {
        match &mut self.slots[slot(layout, register)] {
            Some(FrameValue::Composite(value)) => value,
            _ => unreachable!("verified composite operand"),
        }
    }

    fn set(&mut self, layout: &FrameLayout, register: Register, value: Scalar) {
        self.set_value(layout, register, FrameValue::Scalar(value));
    }

    fn set_value(&mut self, layout: &FrameLayout, register: Register, value: FrameValue) {
        self.slots[slot(layout, register)] = Some(value);
    }

    fn clear(&mut self, layout: &FrameLayout, register: Register) {
        self.slots[slot(layout, register)] = None;
    }
}

fn slot(layout: &FrameLayout, register: Register) -> usize {
    let Some(slot) = layout.slot(register) else {
        // The layout assigns a slot to every register with a reachable lifetime.
        unreachable!("reachable register has no frame slot");
    };
    slot.index()
}

fn instruction_id(pc: usize) -> InstructionId {
    let Ok(id) = u32::try_from(pc) else {
        // The verifier checks the index of every instruction before publication.
        unreachable!("verified instruction index exceeds u32");
    };
    InstructionId(id)
}

/// One independent invocation. The activation owns its frames and retains the
/// immutable verified code across pauses.
#[derive(Debug)]
pub struct Activation {
    module: VerifiedModule,
    output: OutputSink,
    input: InputSource,
    /// Consecutive blocked attempts with no instruction finishing between.
    stalled: usize,
    /// Instructions the running task may still run before a turn passes.
    quantum: usize,
    /// The last resume stopped on an input instruction that has no line yet.
    awaiting_input: bool,
    memory: Memory,
    limits: ExecutionLimits,
    instructions_executed: usize,
    state: ExecutionState,
}

/// One projection step of a recovered loan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveredStep {
    Field(u32),
    Payload(u32),
    /// An element of an array, by row-major offset.
    Offset(usize),
}

/// A value rebuilt from native words: scalars and the tuples and enums of them.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveredValue {
    Scalar(Scalar),
    Text(Arc<str>),
    /// A fixed-size array of scalars; `None` is an element never written.
    Array(Vec<Option<Scalar>>),
    /// A loan of the whole register `register` of the frame at stack index `frame`.
    Loan {
        frame: usize,
        register: Register,
        /// Projections from the register to the borrowed place.
        path: Vec<RecoveredStep>,
    },
    Tuple(Vec<RecoveredValue>),
    Enum {
        variant: u32,
        payload: Option<Box<RecoveredValue>>,
    },
}

pub(crate) struct ScalarFrameSnapshot {
    pub function: FunctionId,
    pub next: InstructionId,
    pub arrival: StatementArrival,
    pub destination: Option<Register>,
    pub values: Vec<(Register, RecoveredValue)>,
}

impl VerifiedFunction {
    /// The interpreter value for a recovered native value of this type, if
    /// the shape agrees with it.
    fn recovered_value(&self, value: &RecoveredValue, ty: ValueType) -> Option<FrameValue> {
        match (value, ty) {
            (RecoveredValue::Scalar(scalar), ValueType::Scalar(expected))
                if scalar.ty() == expected =>
            {
                Some(FrameValue::Scalar(*scalar))
            }
            (RecoveredValue::Loan { .. }, _) => None,
            (RecoveredValue::Text(text), ValueType::Text) => Some(FrameValue::Text(text.clone())),
            (RecoveredValue::Array(items), ValueType::Composite(id)) => {
                let Shape::Array { element, bounds } = self.shape_table().get(usize::try_from(id.0).ok()?)? else {
                    return None;
                };
                let ValueType::Scalar(expected) = element else {
                    return None;
                };
                if super::array_len(bounds) != Some(items.len()) {
                    return None;
                }
                let items = items
                    .iter()
                    .map(|item| match item {
                        Some(scalar) if scalar.ty() == *expected => {
                            Some(Some(FrameValue::Scalar(*scalar)))
                        }
                        Some(_) => None,
                        None => Some(None),
                    })
                    .collect::<Option<Box<[_]>>>()?;
                Some(FrameValue::Composite(Composite::Array(items)))
            }
            (RecoveredValue::Tuple(values), ValueType::Composite(id)) => {
                let Shape::Tuple(fields) = self.shape_table().get(usize::try_from(id.0).ok()?)?
                else {
                    return None;
                };
                if fields.len() != values.len() {
                    return None;
                }
                let items = values
                    .iter()
                    .zip(fields.iter())
                    .map(|(value, &field)| self.recovered_value(value, field))
                    .collect::<Option<Box<[_]>>>()?;
                Some(FrameValue::Composite(Composite::Tuple(items)))
            }
            (RecoveredValue::Enum { variant, payload }, ValueType::Composite(id)) => {
                let Shape::Enum(variants) = self.shape_table().get(usize::try_from(id.0).ok()?)?
                else {
                    return None;
                };
                let declared = variants.get(usize::try_from(*variant).ok()?)?;
                let payload = match (payload, declared) {
                    (None, None) => None,
                    (Some(value), Some(ty)) => Some(Box::new(self.recovered_value(value, *ty)?)),
                    _ => return None,
                };
                Some(FrameValue::Composite(Composite::Enum {
                    variant: *variant,
                    payload,
                }))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarRecoveryError {
    InvalidLimits,
    InvalidPosition {
        function: FunctionId,
        instruction: InstructionId,
    },
    InvalidCall(FunctionId),
    InvalidValue {
        function: FunctionId,
        register: Register,
    },
}

impl fmt::Display for ScalarRecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => {
                f.write_str("native continuation exceeds verified execution limits")
            }
            Self::InvalidPosition {
                function,
                instruction,
            } => write!(f, "invalid recovery PC {instruction:?} in {function:?}"),
            Self::InvalidCall(function) => {
                write!(f, "invalid recovered caller or destination in {function:?}")
            }
            Self::InvalidValue { function, register } => write!(
                f,
                "invalid recovered value for {register:?} in {function:?}"
            ),
        }
    }
}

impl std::error::Error for ScalarRecoveryError {}

#[derive(Debug)]
enum ExecutionState {
    Active(Vec<Frame>),
    Returned(Option<Scalar>),
    Failed(RunError),
}

enum StepOutcome {
    Continue,
    /// The instruction could not finish yet and will be tried again.
    Blocked,
    /// An input instruction found no line and the source waits for one.
    AwaitInput,
    /// A native function is called through an erased value: the arguments with
    /// their types, and the register that receives the erased result.
    NativeCall {
        destination: Register,
        declaration: u32,
        arguments: Vec<(ValueType, FrameValue)>,
        position: (FunctionId, InstructionId, Span),
    },
    /// A module is to be loaded, and its namespace delivered to the register.
    Load {
        destination: Register,
        path: Arc<str>,
        span: Span,
    },
    Returned(Option<FrameValue>),
}

impl VerifiedModule {
    /// Give the running task's frames to the table and run another live task
    /// instead. `false` when no other task can run, and nothing changed.
    fn rotate(&self, stack: &mut Vec<Frame>, memory: &mut Memory, finished: bool) -> bool {
        let current = memory.current_task;
        let count = memory.tasks.len();
        for offset in 1..=count {
            let id = (current + offset) % count;
            if id == current {
                continue;
            }
            let Some(record) = memory.tasks[id].as_ref() else {
                continue;
            };
            if record.end.is_some() {
                continue;
            }
            if record.cancelled {
                self.cancel_task(memory, id);
                continue;
            }
            if !finished {
                let waiting = memory.waiting.take();
                let sleeping_until = memory.sleeping_until.take();
                if let Some(record) = memory.tasks[current].as_mut() {
                    record.stack = std::mem::take(stack);
                    record.waiting = waiting;
                    record.sleeping_until = sleeping_until;
                }
            }
            let Some(next) = memory.tasks[id].as_mut() else {
                unreachable!("a candidate task exists");
            };
            *stack = std::mem::take(&mut next.stack);
            memory.waiting = next.waiting.take();
            memory.sleeping_until = next.sleeping_until.take();
            memory.current_task = id;
            return true;
        }
        false
    }

    /// The running task has returned or failed: record how, release what its
    /// frames own, and run another task.
    fn end_task(&self, stack: &mut Vec<Frame>, memory: &mut Memory, end: TaskEnd) {
        memory.stop_waiting();
        memory.sleeping_until = None;
        self.release_frames(memory, std::mem::take(stack));
        let current = memory.current_task;
        if let Some(record) = memory.tasks[current].as_mut() {
            record.end = Some(end);
        }
        if !self.rotate(stack, memory, true) {
            unreachable!("the entry task outlives every other task");
        }
    }

    /// End a task that is not running because it was asked to stop.
    fn cancel_task(&self, memory: &mut Memory, id: usize) {
        let Some(record) = memory.tasks[id].as_mut() else {
            return;
        };
        let frames = std::mem::take(&mut record.stack);
        let waiting = record.waiting.take();
        record.sleeping_until = None;
        record.end = Some(TaskEnd::Cancelled);
        if let Some(channel) = waiting
            && let Some(Some(channel)) = memory.channels.get_mut(channel)
        {
            channel.waiting -= 1;
        }
        self.release_frames(memory, frames);
    }

    /// Release the parameters of frames that will not run again. Parameters
    /// are the values a frame owns outright; temporaries may alias them.
    fn release_frames(&self, memory: &mut Memory, frames: Vec<Frame>) {
        for mut frame in frames {
            let body = &self.functions[frame.function.index()];
            let layout = body.frame_layout();
            for &parameter in body.parameters.iter() {
                if let Some(value) = frame.slot_mut(layout, parameter).take() {
                    memory.drop_value(value);
                }
            }
        }
    }

    pub(crate) fn restore_scalars(
        &self,
        snapshots: Vec<ScalarFrameSnapshot>,
        limits: ExecutionLimits,
        instructions_executed: usize,
    ) -> Result<Activation, ScalarRecoveryError> {
        use super::StorageState;
        if snapshots.is_empty()
            || snapshots.len() > limits.frames
            || instructions_executed >= limits.fuel
        {
            return Err(ScalarRecoveryError::InvalidLimits);
        }
        let mut frames = Vec::with_capacity(snapshots.len());
        for (index, snapshot) in snapshots.iter().enumerate() {
            let position = ScalarRecoveryError::InvalidPosition {
                function: snapshot.function,
                instruction: snapshot.next,
            };
            let invalid_call = ScalarRecoveryError::InvalidCall(snapshot.function);
            let body = self.function(snapshot.function).ok_or(position)?;
            if !snapshot.arrival.reaches(snapshot.next, body.instructions()) {
                return Err(position);
            }
            if index == 0 && snapshot.destination.is_some() {
                return Err(invalid_call);
            }
            let state = if let Some(child) = snapshots.get(index + 1) {
                let StatementArrival::Next(from) = snapshot.arrival else {
                    return Err(invalid_call);
                };
                let Some(Instruction::Call(site)) = body.instructions().get(from.index()).copied()
                else {
                    return Err(invalid_call);
                };
                let call = &body.calls()[site.index()];
                if call.target != CallTarget::Direct(child.function)
                    || call.destination != child.destination
                {
                    return Err(invalid_call);
                }
                body.storage_before(from)
            } else {
                body.storage_before(snapshot.next)
            }
            .ok_or(position)?;

            let mut values: Vec<Option<FrameValue>> = vec![None; state.len()];
            for (register, value) in &snapshot.values {
                let register = *register;
                let error = ScalarRecoveryError::InvalidValue {
                    function: snapshot.function,
                    register,
                };
                let slot = values.get_mut(register.index()).ok_or(error)?;
                if let RecoveredValue::Loan {
                    frame,
                    register: owner,
                    path,
                } = value
                {
                    let owned = snapshots.get(*frame).and_then(|owner_frame| {
                        self.function(owner_frame.function)?
                            .registers()
                            .get(owner.index())
                    });
                    if !matches!(body.register_kind(register), Ok(RegisterType::Loan { .. }))
                        || owned.is_none()
                        || state[register.index()] != StorageState::Initialized
                    {
                        return Err(error);
                    }
                    let place = BorrowedPlace {
                        root: Root::Frame {
                            frame: *frame,
                            register: *owner,
                        },
                        path: path
                            .iter()
                            .map(|step| match step {
                                RecoveredStep::Field(field) => Step::Field(*field),
                                RecoveredStep::Payload(variant) => Step::Payload(*variant),
                                RecoveredStep::Offset(offset) => Step::Offset(*offset),
                            })
                            .collect(),
                        window: None,
                    };
                    if slot.replace(FrameValue::Loan(place)).is_some() {
                        return Err(error);
                    }
                    continue;
                }
                let ty = match body.register_kind(register) {
                    Ok(RegisterType::Text) => ValueType::Text,
                    Ok(RegisterType::Scalar(ty)) => ValueType::Scalar(ty),
                    Ok(RegisterType::Composite(shape)) => ValueType::Composite(shape),
                    _ => return Err(error),
                };
                let Some(value) = body.recovered_value(value, ty) else {
                    return Err(error);
                };
                if state[register.index()] != StorageState::Initialized
                    || slot.replace(value).is_some()
                {
                    return Err(error);
                }
            }
            let mut frame = Frame::new(
                snapshot.function,
                body,
                std::iter::empty(),
                snapshot.destination,
            );
            frame.pc = snapshot.next.index();
            frame.arrival = snapshot.arrival;
            for (index, (&state, value)) in state.iter().zip(values).enumerate() {
                let register = Register(u32::try_from(index).map_err(|_| position)?);
                if let Some(value) = value {
                    frame.set_value(body.frame_layout(), register, value);
                } else if matches!(
                    state,
                    StorageState::Initialized | StorageState::MaybeInitialized
                ) {
                    return Err(ScalarRecoveryError::InvalidValue {
                        function: snapshot.function,
                        register,
                    });
                }
            }
            frames.push(frame);
        }
        Ok(Activation {
            module: self.clone(),
            output: OutputSink::default(),
            input: InputSource::default(),
            stalled: 0,
            quantum: QUANTUM,
            awaiting_input: false,
            memory: Memory::new(self.globals.len()),
            limits,
            instructions_executed,
            state: ExecutionState::Active(frames),
        })
    }

    /// Prepare a fresh activation. Arguments are copied into its entry frame.
    ///
    /// Rejects incorrect argument counts and types or limits that cannot admit
    /// an entry frame and its first instruction.
    pub fn start(
        &self,
        arguments: &[Scalar],
        limits: ExecutionLimits,
    ) -> Result<Activation, RunError> {
        let entry = &self.functions[self.entry.index()];
        // An activation hands back at most a scalar, and takes only scalars.
        if matches!(entry.result, Some(result) if result.scalar().is_none()) {
            return Err(RunError::EntryResultNotScalar);
        }
        if let Some(index) = entry.parameters.iter().position(|&register| {
            !matches!(entry.registers[register.index()], RegisterType::Scalar(_))
        }) {
            return Err(RunError::EntryParameterNotScalar { index });
        }
        if arguments.len() != entry.parameters.len() {
            return Err(RunError::ArgumentCount {
                expected: entry.parameters.len(),
                actual: arguments.len(),
            });
        }
        for (index, (&register, &argument)) in entry.parameters.iter().zip(arguments).enumerate() {
            let RegisterType::Scalar(expected) = entry.registers[register.index()] else {
                unreachable!("verified entry parameter is scalar");
            };
            let actual = argument.ty();
            if expected != actual {
                return Err(RunError::ArgumentType {
                    index,
                    expected,
                    actual,
                });
            }
        }
        if limits.frames == 0 {
            return Err(RunError::FrameLimit);
        }
        if limits.fuel == 0 {
            return Err(RunError::FuelExhausted);
        }
        let entry = Frame::new(
            self.entry,
            entry,
            arguments.iter().copied().map(FrameValue::Scalar),
            None,
        );
        Ok(Activation {
            module: self.clone(),
            output: OutputSink::default(),
            input: InputSource::default(),
            stalled: 0,
            quantum: QUANTUM,
            awaiting_input: false,
            memory: Memory::new(self.globals.len()),
            limits,
            instructions_executed: 0,
            state: ExecutionState::Active(vec![entry]),
        })
    }

    /// A new activation of the entry routine that starts at an instruction and
    /// continues with the memory of an earlier one, so module variables and
    /// everything they refer to are kept. Nothing of the entry's frame is
    /// kept, so the instruction must begin a statement.
    pub(crate) fn continue_at(
        &self,
        memory: ActivationMemory,
        at: InstructionId,
        limits: ExecutionLimits,
        instructions_executed: usize,
    ) -> Result<Activation, RunError> {
        let mut activation = self.start(&[], limits)?;
        let mut memory = memory.0;
        memory.globals.resize(self.globals.len(), None);
        activation.memory = memory;
        activation.instructions_executed = instructions_executed;
        if let ExecutionState::Active(stack) = &mut activation.state
            && let Some(frame) = stack.first_mut()
        {
            frame.pc = at.index();
        }
        Ok(activation)
    }

    /// Run to return, failure, or the total fuel limit. Output lines go to
    /// standard output.
    pub fn run(
        &self,
        arguments: &[Scalar],
        limits: ExecutionLimits,
    ) -> Result<Option<Scalar>, RunError> {
        self.run_to(arguments, limits, OutputSink::default())
    }

    /// Like [`Self::run`], with output lines delivered to `sink`.
    pub fn run_to(
        &self,
        arguments: &[Scalar],
        limits: ExecutionLimits,
        sink: OutputSink,
    ) -> Result<Option<Scalar>, RunError> {
        let mut activation = self.start(arguments, limits)?;
        activation.set_output(sink);
        match activation.resume(limits.fuel) {
            ExecutionStatus::Returned(value) => Ok(value),
            ExecutionStatus::Failed(error) => Err(error),
            ExecutionStatus::Paused(_) => unreachable!("resume spent the entire fuel allowance"),
        }
    }

    fn step(
        &self,
        stack: &mut Vec<Frame>,
        memory: &mut Memory,
        max_frames: usize,
        output: &mut OutputSink,
        input: &mut InputSource,
    ) -> Result<StepOutcome, RunError> {
        let depth = stack.len();
        let Some(frame) = stack.last_mut() else {
            // An entry return transitions out of Active in the same step.
            unreachable!("active execution has no frame");
        };
        let function = frame.function;
        let body = &self.functions[function.index()];
        let layout = body.frame_layout();
        let pc = frame.pc;
        let from = instruction_id(pc);
        frame.pc += 1;
        frame.arrival = StatementArrival::Next(from);
        match body.instructions[pc] {
            Instruction::StorageLive(reg) | Instruction::StorageDead(reg) => {
                frame.clear(layout, reg);
            }
            Instruction::Constant(dst, value) => frame.set(layout, dst, value),
            Instruction::Copy(dst, src) => frame.set_value(layout, dst, frame.value(layout, src)),
            Instruction::Borrow(loan, owner) => {
                let place = BorrowedPlace {
                    root: Root::Frame {
                        frame: depth - 1,
                        register: owner,
                    },
                    path: Box::new([]),
                    window: None,
                };
                frame.set_value(layout, loan, FrameValue::Loan(place));
            }
            Instruction::TraitBox(dst, value, implementation) => {
                let value = frame.value(layout, value);
                if memory.heap.should_collect() {
                    memory.collect(stack);
                }
                let heap = memory.heap.allocate(value).ok_or(RunError::HeapExhausted {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                stack[depth - 1].set_value(
                    layout,
                    dst,
                    FrameValue::Object {
                        heap,
                        implementation: implementation.0,
                    },
                );
            }
            Instruction::GcNew(dst, value) => {
                let value = frame.value(layout, value);
                if memory.heap.should_collect() {
                    memory.collect(stack);
                }
                let handle = memory.heap.allocate(value).ok_or(RunError::HeapExhausted {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                stack[depth - 1].set_value(layout, dst, FrameValue::Handle(handle));
            }
            Instruction::GcBorrow(dst, handle) => {
                let root = match frame.value(layout, handle) {
                    FrameValue::Handle(object) => Root::Heap(object),
                    FrameValue::Rc(block) => Root::Counted(block),
                    _ => unreachable!("verified handle operand"),
                };
                let place = BorrowedPlace {
                    root,
                    path: Box::new([]),
                    window: None,
                };
                frame.set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::Move(dst, src) => {
                let value = frame.slot_mut(layout, src).take();
                let Some(value) = value else {
                    unreachable!("verified move source is initialized");
                };
                frame.set_value(layout, dst, value);
            }
            Instruction::Drop(reg) => {
                // A value that was already moved out leaves nothing to release.
                if let Some(value) = frame.slot_mut(layout, reg).take() {
                    memory.drop_value(value);
                }
            }
            Instruction::MoveOut(dst, loan) => {
                let place = frame.loan(layout, loan);
                let uninitialized = RunError::UninitializedGlobal {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                };
                let root = self
                    .root_value(stack, memory, &place.root)
                    .ok_or(uninitialized)?;
                let value = read_place(&root, &place.path)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let slot = self.root_slot(stack, memory, &place.root);
                match (slot, place.path.is_empty()) {
                    (slot, true) => *slot = Some(FrameValue::Moved),
                    (Some(root), false) => {
                        write_place(root, &place.path, FrameValue::Moved)
                            .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                    }
                    (None, false) => unreachable!("a place was read from this root"),
                }
                stack[depth - 1].set_value(layout, dst, value);
            }
            Instruction::RcNew(dst, value) => {
                let value = frame.value(layout, value);
                let block = memory.new_counted(value);
                frame.set_value(layout, dst, FrameValue::Rc(block));
            }
            Instruction::RcClone(dst, src)
            | Instruction::RcDowngrade(dst, src)
            | Instruction::RcStrongCount(dst, src) => {
                let held = self
                    .held_value(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Rc(block)) = held else {
                    unreachable!("verified counted operand");
                };
                let counts = memory.block(block);
                let result = match body.instructions[pc] {
                    Instruction::RcClone(..) => {
                        counts.strong += 1;
                        FrameValue::Rc(block)
                    }
                    Instruction::RcDowngrade(..) => {
                        counts.weak += 1;
                        FrameValue::Weak(block)
                    }
                    _ => FrameValue::Scalar(Scalar::U64(
                        u64::try_from(counts.strong).unwrap_or(u64::MAX),
                    )),
                };
                stack[depth - 1].set_value(layout, dst, result);
            }
            Instruction::RcUpgrade(dst, src) => {
                let held = self
                    .held_value(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Weak(block)) = held else {
                    unreachable!("verified weak operand");
                };
                let (some, none) = body.option_variants(dst);
                let counts = memory.block(block);
                let value = if counts.strong > 0 {
                    counts.strong += 1;
                    Composite::Enum {
                        variant: some,
                        payload: Some(Box::new(FrameValue::Rc(block))),
                    }
                } else {
                    Composite::Enum {
                        variant: none,
                        payload: None,
                    }
                };
                stack[depth - 1].set_value(layout, dst, FrameValue::Composite(value));
            }
            Instruction::EnumTake(dst, src, variant) => {
                let Composite::Enum { variant: held, .. } = frame.composite(layout, src) else {
                    unreachable!("verified enum operand");
                };
                if *held != variant {
                    return Err(RunError::EnumVariant {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                let Some(FrameValue::Composite(Composite::Enum {
                    payload: Some(payload),
                    ..
                })) = frame.slot_mut(layout, src).take()
                else {
                    unreachable!("verified enum carries a payload");
                };
                frame.set_value(layout, dst, *payload);
            }
            Instruction::BorrowGlobal(dst, global) => {
                let place = BorrowedPlace {
                    root: Root::Global(global),
                    path: Box::new([]),
                    window: None,
                };
                frame.set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::ReadLoan(dst, loan) => {
                let place = frame.loan(layout, loan);
                let uninitialized = RunError::UninitializedGlobal {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                };
                let root = self
                    .root_value(stack, memory, &place.root)
                    .ok_or(uninitialized)?;
                let value = read_place(&root, &place.path)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                stack[depth - 1].set_value(layout, dst, value);
            }
            Instruction::WriteLoan(loan, value) => {
                let place = frame.loan(layout, loan);
                let value = frame.value(layout, value);
                let slot = self.root_slot(stack, memory, &place.root);
                match (slot, place.path.is_empty()) {
                    (slot, true) => *slot = Some(value),
                    (Some(root), false) => write_place(root, &place.path, value)
                        .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?,
                    (None, false) => {
                        return Err(RunError::UninitializedGlobal {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        });
                    }
                }
            }
            Instruction::LoanField(dst, src, field) => {
                let place = project_base(body, frame, layout, depth, src, Step::Field(field));
                frame.set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::LoanPayload(dst, src, variant) => {
                let place = project_base(body, frame, layout, depth, src, Step::Payload(variant));
                // The variant must be held when the borrow is taken.
                let root = self.root_value(stack, memory, &place.root).ok_or(
                    RunError::UninitializedGlobal {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    },
                )?;
                read_place(&root, &place.path)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                stack[depth - 1].set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::Clone(dst, src) => {
                let held = self
                    .held_value(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?
                    .ok_or(RunError::UninitializedGlobal {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                let copy = memory.clone_value(&held);
                stack[depth - 1].set_value(layout, dst, copy);
            }
            Instruction::TextMap(op, dst, src) => {
                let text = frame.text(layout, src);
                let mapped = match op {
                    TextMapOp::Trim => text.trim().to_owned(),
                    TextMapOp::Upper => text.to_uppercase(),
                    TextMapOp::Lower => text.to_lowercase(),
                };
                frame.set_value(layout, dst, FrameValue::Text(mapped.into()));
            }
            Instruction::TextTest(op, dst, text, other) => {
                let (text, other) = (frame.text(layout, text), frame.text(layout, other));
                let result = match op {
                    TextTestOp::Contains => text.contains(&**other),
                    TextTestOp::StartsWith => text.starts_with(&**other),
                    TextTestOp::EndsWith => text.ends_with(&**other),
                };
                frame.set(layout, dst, Scalar::Bool(result));
            }
            Instruction::TextReplace(dst, text, list) => {
                let [from_text, to_text] = body.lists[list.index()][..] else {
                    unreachable!("verified two-entry list");
                };
                let replaced = frame.text(layout, text).replace(
                    &**frame.text(layout, from_text),
                    frame.text(layout, to_text),
                );
                frame.set_value(layout, dst, FrameValue::Text(replaced.into()));
            }
            Instruction::ToText(dst, src) => {
                let spelled = spell(&frame.value(layout, src), TextStyle::Plain);
                frame.set_value(layout, dst, FrameValue::Text(spelled.into()));
            }
            Instruction::LayoutForRc(dst) => {
                let size = FrameValue::Scalar(Scalar::U64(std::mem::size_of::<usize>() as u64));
                let shape = Composite::Tuple(Box::new([size.clone(), size]));
                frame.set_value(layout, dst, FrameValue::Composite(shape));
            }
            Instruction::Allocate(dst, _) => {
                let (ok, _) = body.allocation_result_runtime(dst);
                memory.allocations += 1;
                let allocated = Composite::Enum {
                    variant: ok,
                    payload: Some(Box::new(FrameValue::Allocation(memory.allocations))),
                };
                frame.set_value(layout, dst, FrameValue::Composite(allocated));
            }
            Instruction::Deallocate(_) => {}
            Instruction::RcNewIn(dst, list, deallocate) => {
                let &[allocator, allocated, value] = &*body.lists[list.index()] else {
                    unreachable!("verified allocator operands");
                };
                let (ok, failure) = body.allocation_result_runtime(allocated);
                let FrameValue::Composite(Composite::Enum { variant, payload }) =
                    frame.value(layout, allocated)
                else {
                    unreachable!("verified allocation result");
                };
                let allocator = frame.value(layout, allocator);
                let value = frame.value(layout, value);
                if variant == failure {
                    memory.drop_value(value);
                    memory.drop_value(allocator);
                    return Err(RunError::AllocationRejected {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                debug_assert_eq!(variant, ok);
                let Some(FrameValue::Allocation(allocation)) = payload.map(|payload| *payload)
                else {
                    unreachable!("verified allocation payload");
                };
                let block = memory.new_counted_in(
                    value,
                    Some(AllocatorState {
                        state: Some(allocator),
                        deallocate,
                        allocation,
                    }),
                );
                frame.set_value(layout, dst, FrameValue::Rc(block));
            }
            Instruction::LocationField(dst, owner, field) => {
                let held = self
                    .held_value(stack, memory, body, owner)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Handle(object)) = held else {
                    unreachable!("verified handle operand");
                };
                let place = BorrowedPlace {
                    root: Root::Heap(object),
                    path: Box::new([Step::Field(field)]),
                    window: None,
                };
                stack[depth - 1].set_value(layout, dst, FrameValue::Location(place));
            }
            Instruction::LocationLoad(dst, location) => {
                let held = self
                    .held_value(stack, memory, body, location)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Location(place)) = held else {
                    unreachable!("verified location operand");
                };
                let root = self.root_value(stack, memory, &place.root).ok_or(
                    RunError::UninitializedGlobal {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    },
                )?;
                let value = read_place(&root, &place.path)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let value = memory.clone_value(&value);
                stack[depth - 1].set_value(layout, dst, value);
            }
            Instruction::LocationStore(location, value) => {
                let held = self
                    .held_value(stack, memory, body, location)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Location(place)) = held else {
                    unreachable!("verified location operand");
                };
                let value = stack[depth - 1].value(layout, value);
                let Root::Heap(object) = place.root else {
                    unreachable!("a location is rooted in the heap");
                };
                let Some(root) = memory.heap.slots[object].as_mut() else {
                    unreachable!("a location keeps its object alive");
                };
                let old = read_place(root, &place.path)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                write_place(root, &place.path, value)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                memory.drop_value(old);
            }
            Instruction::PathNew(dst, _) => {
                frame.set_value(layout, dst, FrameValue::Path(Some(Arc::from([]))));
            }
            Instruction::PathMissing(dst) => frame.set_value(layout, dst, FrameValue::Path(None)),
            Instruction::PathTarget(dst, src)
            | Instruction::PathField(dst, src, _)
            | Instruction::PathElement(dst, src, _) => {
                let step = match body.instructions[pc] {
                    Instruction::PathField(_, _, field) => PathStep::Field(field),
                    Instruction::PathElement(_, _, index) => {
                        let index = scalar_index(frame.get(layout, index));
                        let fault = |fault| RunError::Path {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                            fault,
                        };
                        PathStep::Element(
                            usize::try_from(index).map_err(|_| fault(PathFault::NegativeIndex))?,
                        )
                    }
                    _ => PathStep::Target,
                };
                let FrameValue::Path(path) = frame.value(layout, src) else {
                    unreachable!("verified path operand");
                };
                let extended = path.map(|steps| {
                    steps
                        .iter()
                        .copied()
                        .chain(std::iter::once(step))
                        .collect::<Arc<[_]>>()
                });
                frame.set_value(layout, dst, FrameValue::Path(extended));
            }
            Instruction::Input(..) | Instruction::InputLoan(..) => {
                let (Instruction::Input(target, kind) | Instruction::InputLoan(target, kind)) =
                    body.instructions[pc]
                else {
                    unreachable!("matched input instruction");
                };
                let read_error = |exhausted: bool| {
                    let (instruction, span) = (from, body.spans[pc]);
                    if exhausted {
                        RunError::InputExhausted {
                            function,
                            instruction,
                            span,
                        }
                    } else {
                        RunError::InputInvalid {
                            function,
                            instruction,
                            span,
                            kind,
                        }
                    }
                };
                let line = match input.next_line() {
                    Some(line) => line,
                    None if input.waits => {
                        stack[depth - 1].pc -= 1;
                        return Ok(StepOutcome::AwaitInput);
                    }
                    None => return Err(read_error(true)),
                };
                let value = read_input(&line, kind).ok_or_else(|| read_error(false))?;
                if matches!(body.instructions[pc], Instruction::Input(..)) {
                    frame.set_value(layout, target, value);
                } else {
                    let place = frame.loan(layout, target);
                    let slot = self.root_slot(stack, memory, &place.root);
                    match (slot, place.path.is_empty()) {
                        (slot, true) => *slot = Some(value),
                        (Some(root), false) => write_place(root, &place.path, value)
                            .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?,
                        (None, false) => {
                            return Err(RunError::UninitializedGlobal {
                                function,
                                instruction: from,
                                span: body.spans[pc],
                            });
                        }
                    }
                }
            }
            Instruction::ParseInteger(dst, text) => {
                let (ok, err) = body.parse_variants_runtime(dst);
                let parsed = frame
                    .text(layout, text)
                    .parse::<i128>()
                    .ok()
                    .and_then(|value| i64::try_from(value).ok());
                let value = match parsed {
                    Some(value) => Composite::Enum {
                        variant: ok,
                        payload: Some(Box::new(FrameValue::Scalar(Scalar::I64(value)))),
                    },
                    None => Composite::Enum {
                        variant: err,
                        payload: Some(Box::new(FrameValue::Composite(Composite::Enum {
                            variant: 0,
                            payload: None,
                        }))),
                    },
                };
                frame.set_value(layout, dst, FrameValue::Composite(value));
            }
            Instruction::CountToInteger(dst, src) => {
                let Scalar::U64(value) = frame.get(layout, src) else {
                    unreachable!("verified count operand");
                };
                let value = i64::try_from(value).map_err(|_| RunError::ConversionOutOfRange {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                frame.set(layout, dst, Scalar::I64(value));
            }
            Instruction::IntegerToCount(dst, src) => {
                let Scalar::I64(value) = frame.get(layout, src) else {
                    unreachable!("verified integer operand");
                };
                let value = u64::try_from(value).map_err(|_| RunError::ConversionOutOfRange {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                frame.set(layout, dst, Scalar::U64(value));
            }
            Instruction::Abs(dst, src) => {
                let result = match frame.get(layout, src) {
                    Scalar::I64(value) => {
                        Scalar::I64(value.checked_abs().ok_or(RunError::IntegerOverflow {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        })?)
                    }
                    Scalar::F64(bits) => Scalar::F64(f64::from_bits(bits).abs().to_bits()),
                    _ => unreachable!("verified number operand"),
                };
                frame.set(layout, dst, result);
            }
            Instruction::Extremum(op, dst, lhs, rhs) => {
                let (Scalar::I64(lhs), Scalar::I64(rhs)) =
                    (frame.get(layout, lhs), frame.get(layout, rhs))
                else {
                    unreachable!("verified integer operands");
                };
                let value = match op {
                    Extremum::Min => lhs.min(rhs),
                    Extremum::Max => lhs.max(rhs),
                };
                frame.set(layout, dst, Scalar::I64(value));
            }
            Instruction::RangeNew(dst, start, end)
            | Instruction::RangeInclusive(dst, start, end) => {
                let (Scalar::I64(start), Scalar::I64(end)) =
                    (frame.get(layout, start), frame.get(layout, end))
                else {
                    unreachable!("verified integer operands");
                };
                let end = if matches!(body.instructions[pc], Instruction::RangeInclusive(..)) {
                    end.checked_add(1).ok_or(RunError::IntegerOverflow {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?
                } else {
                    end
                };
                let bounds = [start, end]
                    .into_iter()
                    .map(|bound| FrameValue::Scalar(Scalar::I64(bound)))
                    .collect();
                frame.set_value(layout, dst, FrameValue::Composite(Composite::Tuple(bounds)));
            }
            Instruction::VecFrom(dst, range) => {
                let Composite::Tuple(bounds) = frame.composite(layout, range) else {
                    unreachable!("verified range operand");
                };
                let [
                    FrameValue::Scalar(Scalar::I64(start)),
                    FrameValue::Scalar(Scalar::I64(end)),
                ] = bounds.as_ref()
                else {
                    unreachable!("verified integer range");
                };
                let count = end.saturating_sub(*start).max(0);
                let count = usize::try_from(count).unwrap_or(usize::MAX);
                if count > MAX_VEC_ELEMENTS {
                    return Err(RunError::HeapExhausted {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                let items = (*start..*end)
                    .map(|value| FrameValue::Scalar(Scalar::I64(value)))
                    .collect();
                frame.set_value(layout, dst, FrameValue::Composite(Composite::Vec(items)));
            }
            Instruction::VecNew(dst, list) => {
                let items = body.lists[list.index()]
                    .iter()
                    .map(|&item| frame.value(layout, item))
                    .collect();
                frame.set_value(layout, dst, FrameValue::Composite(Composite::Vec(items)));
            }
            Instruction::VecPush(target, value) => {
                let value = frame.value(layout, value);
                let container = self
                    .container_mut(stack, memory, body, target)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let FrameValue::Composite(Composite::Vec(items)) = container else {
                    unreachable!("verified vector operand");
                };
                if items.len() >= MAX_VEC_ELEMENTS {
                    return Err(RunError::HeapExhausted {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                items.push(value);
            }
            Instruction::VecLength(dst, vector) => {
                let held = self
                    .container_ref(stack, memory, body, vector)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?
                    .ok_or(RunError::UninitializedGlobal {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                let FrameValue::Composite(Composite::Vec(items)) = held else {
                    unreachable!("verified vector operand");
                };
                let length = u64::try_from(items.len()).unwrap_or(u64::MAX);
                stack[depth - 1].set(layout, dst, Scalar::U64(length));
            }
            Instruction::CellNew(dst, value) => {
                let value = frame.value(layout, value);
                frame.set_value(
                    layout,
                    dst,
                    FrameValue::Composite(Composite::Cell(Box::new(value))),
                );
            }
            Instruction::CellRead(dst, cell) => {
                let held = self
                    .container_ref(stack, memory, body, cell)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?
                    .ok_or(RunError::UninitializedGlobal {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                let FrameValue::Composite(Composite::Cell(inner)) = held else {
                    unreachable!("verified cell operand");
                };
                let value = (**inner).clone();
                stack[depth - 1].set_value(layout, dst, value);
            }
            Instruction::CellWrite(cell, value) => {
                let value = frame.value(layout, value);
                let container = self
                    .container_mut(stack, memory, body, cell)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let FrameValue::Composite(Composite::Cell(inner)) = container else {
                    unreachable!("verified cell operand");
                };
                **inner = value;
            }
            Instruction::CellReplace(dst, cell, value) => {
                let value = frame.value(layout, value);
                let container = self
                    .container_mut(stack, memory, body, cell)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let FrameValue::Composite(Composite::Cell(inner)) = container else {
                    unreachable!("verified cell operand");
                };
                let old = std::mem::replace(&mut **inner, value);
                stack[depth - 1].set_value(layout, dst, old);
            }
            Instruction::SliceOf(dst, src) => {
                let mut place = place_of(body, frame, layout, depth, src);
                let window = self
                    .sequence_window(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                place.window = window;
                stack[depth - 1].set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::SliceLength(dst, src) => {
                let window = self
                    .sequence_window(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let length = window.map_or(0, |window| window.len);
                let length = i64::try_from(length).unwrap_or(i64::MAX);
                stack[depth - 1].set(layout, dst, Scalar::I64(length));
            }
            Instruction::Sum(dst, src) => {
                let elements = self
                    .sequence_elements(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let mut total = 0i64;
                for element in elements {
                    let Some(FrameValue::Scalar(Scalar::I64(value))) = element else {
                        return Err(RunError::UninitializedElement {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        });
                    };
                    total = total.checked_add(value).ok_or(RunError::IntegerOverflow {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                }
                stack[depth - 1].set(layout, dst, Scalar::I64(total));
            }
            Instruction::LoanElement(dst, src, list)
                if body.projects_from_slice(src) || body.projects_from_vec(src) =>
            {
                let index = scalar_index(stack[depth - 1].get(layout, body.lists[list.index()][0]));
                let window = self
                    .sequence_window(stack, memory, body, src)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let offset = window.and_then(|window| {
                    usize::try_from(index)
                        .ok()
                        .filter(|&index| index < window.len)
                        .map(|index| window.start + index)
                });
                let offset = offset.ok_or(RunError::IndexOutOfBounds {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                let place = project_base(
                    body,
                    &stack[depth - 1],
                    layout,
                    depth,
                    src,
                    Step::Offset(offset),
                );
                stack[depth - 1].set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::LoanElement(dst, src, list) => {
                let offset =
                    element_offset(frame, layout, body.projected_array_bounds(src), list, body)
                        .ok_or(RunError::IndexOutOfBounds {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        })?;
                let place = project_base(body, frame, layout, depth, src, Step::Offset(offset));
                frame.set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::Integer(op, dst, lhs, rhs) => {
                let (Scalar::I64(lhs), Scalar::I64(rhs)) =
                    (frame.get(layout, lhs), frame.get(layout, rhs))
                else {
                    unreachable!("verified integer operands");
                };
                let value = evaluate(op, lhs, rhs).map_err(|error| {
                    let span = body.spans[pc];
                    match error {
                        IntegerError::Overflow => RunError::IntegerOverflow {
                            function,
                            instruction: from,
                            span,
                        },
                        IntegerError::DivisionByZero => RunError::DivisionByZero {
                            function,
                            instruction: from,
                            span,
                        },
                    }
                })?;
                frame.set(layout, dst, value);
            }
            Instruction::Jump(to) => {
                frame.pc = to.index();
                frame.arrival = StatementArrival::Jump(from);
            }
            Instruction::Branch(condition, yes, no) => {
                let Scalar::Bool(condition) = frame.get(layout, condition) else {
                    unreachable!("verified boolean condition");
                };
                frame.pc = if condition { yes.index() } else { no.index() };
                frame.arrival = StatementArrival::Branch {
                    from,
                    taken: condition,
                };
            }
            Instruction::Call(site) => {
                let call = &body.calls[site.index()];
                match call.target {
                    CallTarget::Direct(target) => {
                        if depth == max_frames {
                            return Err(RunError::FrameLimit);
                        }
                        let callee = &self.functions[target.index()];
                        let next = Frame::new(
                            target,
                            callee,
                            call.arguments.iter().map(|&reg| frame.value(layout, reg)),
                            call.destination,
                        );
                        stack.push(next);
                    }
                    CallTarget::Output(id) => {
                        let template = &body.outputs[id.index()];
                        let arguments: Vec<_> = call
                            .arguments
                            .iter()
                            .map(|&reg| frame.value(layout, reg))
                            .collect();
                        output.write_line(&render_output(template, &arguments));
                    }
                    CallTarget::Closure(callee) | CallTarget::Mapped(callee) => {
                        if depth == max_frames {
                            return Err(RunError::FrameLimit);
                        }
                        let FrameValue::Closure(closure) = frame.value(layout, callee) else {
                            unreachable!("verified closure operand");
                        };
                        let target = &self.functions[closure.function.index()];
                        if target.generator.is_some()
                            && !matches!(call.target, CallTarget::Mapped(_))
                        {
                            // Calling the closure of a generator body makes a
                            // generator over copies of what it captured.
                            let captures: Vec<_> = closure
                                .captures
                                .iter()
                                .map(|capture| memory.clone_value(capture))
                                .collect();
                            let values = captures
                                .into_iter()
                                .chain(call.arguments.iter().map(|&reg| frame.value(layout, reg)));
                            let suspended = Frame::new(closure.function, target, values, None);
                            let Some(id) = memory.new_generator(suspended) else {
                                return Err(RunError::Generator {
                                    function,
                                    instruction: from,
                                    span: body.spans[pc],
                                    fault: GeneratorFault::LimitExceeded,
                                });
                            };
                            if let Some(dst) = call.destination {
                                frame.set_value(layout, dst, FrameValue::Generator(id));
                            }
                            return Ok(StepOutcome::Continue);
                        }
                        let values = closure
                            .captures
                            .iter()
                            .cloned()
                            .chain(call.arguments.iter().map(|&reg| frame.value(layout, reg)));
                        let mut next =
                            Frame::new(closure.function, target, values, call.destination);
                        if matches!(call.target, CallTarget::Mapped(_)) {
                            next.mapped = Some(Box::new(frame.value(layout, call.arguments[0])));
                        }
                        stack.push(next);
                    }
                    CallTarget::Method { object, method } => {
                        if depth == max_frames {
                            return Err(RunError::FrameLimit);
                        }
                        // A service reference finds the provider the context sees
                        // now and holds it for the length of the call.
                        let mut lease = None;
                        let provider = match frame.value(layout, object) {
                            FrameValue::ServiceRef(view, key) => {
                                let scopes = service_scopes(stack, memory);
                                let binding = memory
                                    .services
                                    .selected(view, key, scopes.contains(&view))
                                    .map_err(|fault| RunError::Service {
                                        function,
                                        instruction: from,
                                        span: body.spans[pc],
                                        fault,
                                    })?;
                                memory.services.bindings[binding].leases += 1;
                                lease = Some(binding);
                                match memory.services.bindings[binding].value.clone() {
                                    Some(provider) => provider,
                                    None => unreachable!("an available provider has a value"),
                                }
                            }
                            other => other,
                        };
                        let frame = &mut stack[depth - 1];
                        let FrameValue::Object {
                            heap,
                            implementation,
                        } = provider
                        else {
                            unreachable!("verified object operand");
                        };
                        let target =
                            self.implementations[implementation as usize].methods[method as usize];
                        let callee = &self.functions[target.index()];
                        let by_value = match body.registers()[object.index()] {
                            RegisterType::Object(trait_id) | RegisterType::ServiceRef(trait_id) => {
                                body.traits[trait_id.index()].methods[method as usize].by_value
                            }
                            _ => unreachable!("verified object operand"),
                        };
                        let receiver = if by_value {
                            // The routine takes a copy of what the object holds.
                            let Some(held) = memory.heap.slots[heap].clone() else {
                                unreachable!("an object keeps its value");
                            };
                            held
                        } else {
                            // The routine borrows the receiver the object holds.
                            FrameValue::Loan(BorrowedPlace {
                                root: Root::Heap(heap),
                                path: Box::new([]),
                                window: None,
                            })
                        };
                        let values = std::iter::once(receiver)
                            .chain(call.arguments.iter().map(|&reg| frame.value(layout, reg)));
                        let mut next = Frame::new(target, callee, values, call.destination);
                        next.lease = lease;
                        stack.push(next);
                    }
                    CallTarget::Bound(binding) => self.call_bound(
                        binding,
                        call,
                        frame,
                        layout,
                        ExecutionPosition {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        },
                    )?,
                }
            }
            Instruction::Return(value) if frame.generator.is_some() => {
                let Some((id, resumer)) = frame.generator else {
                    unreachable!("checked above");
                };
                let Some(returned) = value.map(|reg| frame.value(layout, reg)) else {
                    unreachable!("a generator returns a value");
                };
                stack.pop();
                memory.generator(id).state = GeneratorState::Done;
                let caller = stack
                    .last_mut()
                    .expect("a generator runs above its resumer");
                let caller_body = &self.functions[caller.function.index()];
                let caller_layout = caller_body.frame_layout();
                caller.set_value(caller_layout, resumer, step(1, returned));
            }
            Instruction::ChannelBounded(dst, _) | Instruction::ChannelUnbounded(dst) => {
                let capacity = match body.instructions[pc] {
                    Instruction::ChannelBounded(_, capacity) => {
                        let Scalar::U64(capacity) = frame.get(layout, capacity) else {
                            unreachable!("verified capacity");
                        };
                        Some(usize::try_from(capacity).unwrap_or(usize::MAX))
                    }
                    _ => None,
                };
                let Some(id) = memory.new_channel(capacity) else {
                    return Err(RunError::ChannelLimit {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                };
                let ends =
                    Composite::Tuple(Box::new([FrameValue::Sender(id), FrameValue::Receiver(id)]));
                frame.set_value(layout, dst, FrameValue::Composite(ends));
            }
            Instruction::ChannelClone(dst, end) => {
                let held = self
                    .held_value(stack, memory, body, end)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(end) = held else {
                    unreachable!("verified channel end");
                };
                let copy = memory.clone_value(&end);
                stack[depth - 1].set_value(layout, dst, copy);
            }
            Instruction::ChannelClose(end) | Instruction::ChannelIsClosed(_, end) => {
                let held = self
                    .held_value(stack, memory, body, end)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let (id, receiving) = match held {
                    Some(FrameValue::Sender(id)) => (id, false),
                    Some(FrameValue::Receiver(id)) => (id, true),
                    _ => unreachable!("verified channel end"),
                };
                if let Instruction::ChannelIsClosed(dst, _) = body.instructions[pc] {
                    let closed = memory.channel(id).closed;
                    stack[depth - 1].set_value(
                        layout,
                        dst,
                        FrameValue::Scalar(Scalar::Bool(closed)),
                    );
                } else {
                    let channel = memory.channel(id);
                    channel.closed = true;
                    if receiving {
                        let discarded: Vec<_> = channel.queue.drain(..).collect();
                        for value in discarded {
                            memory.drop_value(value);
                        }
                    }
                }
            }
            Instruction::ChannelSend(dst, list, variants)
            | Instruction::ChannelTrySend(dst, list, variants) => {
                let &[sender, value] = &*body.lists[list.index()] else {
                    unreachable!("verified send operands");
                };
                let held = self
                    .held_value(stack, memory, body, sender)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Sender(id)) = held else {
                    unreachable!("verified sender");
                };
                let value = stack[depth - 1].value(layout, value);
                let blocking = matches!(body.instructions[pc], Instruction::ChannelSend(..));
                let channel = memory.channel(id);
                let outcome = if channel.closed {
                    Some(variants.failure(0))
                } else if channel.has_room() {
                    None
                } else if blocking {
                    stack[depth - 1].pc -= 1;
                    return Ok(StepOutcome::Blocked);
                } else {
                    Some(variants.failure(1))
                };
                let result = match outcome {
                    None => {
                        channel.queue.push_back(value);
                        Composite::Enum {
                            variant: variants.ok(),
                            payload: Some(Box::new(unit())),
                        }
                    }
                    Some(failure) => Composite::Enum {
                        variant: variants.err(),
                        payload: Some(Box::new(FrameValue::Composite(Composite::Enum {
                            variant: failure,
                            payload: Some(Box::new(value)),
                        }))),
                    },
                };
                stack[depth - 1].set_value(layout, dst, FrameValue::Composite(result));
            }
            Instruction::ChannelReceive(dst, end, variants)
            | Instruction::ChannelTryReceive(dst, end, variants) => {
                let held = self
                    .held_value(stack, memory, body, end)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Receiver(id)) = held else {
                    unreachable!("verified receiver");
                };
                let blocking = matches!(body.instructions[pc], Instruction::ChannelReceive(..));
                let channel = memory.channel(id);
                let queued = channel.queue.pop_front();
                let closed = channel.closed;
                if queued.is_none() && !closed && blocking {
                    // Park on the channel so a rendezvous send may hand over.
                    if memory.waiting.is_none() {
                        memory.channel(id).waiting += 1;
                        memory.waiting = Some(id);
                    }
                    stack[depth - 1].pc -= 1;
                    return Ok(StepOutcome::Blocked);
                }
                memory.stop_waiting();
                let result = match queued {
                    Some(value) => Composite::Enum {
                        variant: variants.ok(),
                        payload: Some(Box::new(value)),
                    },
                    None => Composite::Enum {
                        variant: variants.err(),
                        payload: Some(Box::new(FrameValue::Composite(Composite::Enum {
                            variant: variants.failure(if closed { 0 } else { 1 }),
                            payload: None,
                        }))),
                    },
                };
                stack[depth - 1].set_value(layout, dst, FrameValue::Composite(result));
            }
            Instruction::ObjectNew(dst, list, contract) => {
                let contract = &body.contracts[contract.index()];
                let values = &body.lists[list.index()];
                let properties = contract
                    .fields
                    .iter()
                    .zip(values.iter())
                    .map(|((name, ty), &register)| Property {
                        name: name.clone(),
                        value: erased_parts(frame.value(layout, register), *ty),
                        required: contract.required.then_some(*ty),
                    })
                    .collect();
                let data = FrameValue::Properties(Box::new(ObjectData {
                    readonly: false,
                    properties,
                }));
                if memory.heap.should_collect() {
                    memory.collect(stack);
                }
                let handle = memory.heap.allocate(data).ok_or(RunError::HeapExhausted {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                let object = FrameValue::OpenObject(handle);
                let value = if body.registers()[dst.index()].value() == Some(ValueType::Dynamic) {
                    FrameValue::Dynamic(Box::new((ValueType::OpenObject, object)))
                } else {
                    object
                };
                stack[depth - 1].set_value(layout, dst, value);
            }
            Instruction::ObjectGet(..)
            | Instruction::ObjectGetKey(..)
            | Instruction::ObjectSet(..)
            | Instruction::ObjectSetKey(..)
            | Instruction::ObjectDelete(..)
            | Instruction::ObjectDeleteKey(..)
            | Instruction::ObjectView(..)
            | Instruction::ObjectTryView(..)
            | Instruction::ObjectFreeze(..) => {
                let fault = |fault| RunError::Object {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                    fault,
                };
                let instruction = body.instructions[pc];
                let (object, name) = match instruction {
                    Instruction::ObjectGet(_, object, name)
                    | Instruction::ObjectDelete(_, object, name)
                    | Instruction::ObjectSet(object, name, _) => {
                        (object, Some(body.texts[name.index()].clone()))
                    }
                    Instruction::ObjectGetKey(_, object, key)
                    | Instruction::ObjectDeleteKey(_, object, key) => {
                        (object, Some(stack[depth - 1].text(layout, key).clone()))
                    }
                    Instruction::ObjectSetKey(object, list) => {
                        let key = body.lists[list.index()][0];
                        (object, Some(stack[depth - 1].text(layout, key).clone()))
                    }
                    Instruction::ObjectView(_, object, _)
                    | Instruction::ObjectTryView(_, object, _)
                    | Instruction::ObjectFreeze(object) => (object, None),
                    _ => unreachable!("matched object instruction"),
                };
                let held = self
                    .held_value(stack, memory, body, object)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let id = match held {
                    Some(FrameValue::OpenObject(id)) => id,
                    Some(FrameValue::Dynamic(erased)) => match erased.1 {
                        FrameValue::OpenObject(id) => id,
                        _ => return Err(fault(ObjectFault::Type)),
                    },
                    _ => unreachable!("verified object operand"),
                };
                let value_register = match instruction {
                    Instruction::ObjectSet(_, _, value) => Some(value),
                    Instruction::ObjectSetKey(_, list) => Some(body.lists[list.index()][1]),
                    _ => None,
                };
                let stored = value_register.map(|register| {
                    let ty = body.registers()[register.index()]
                        .value()
                        .unwrap_or(ValueType::Dynamic);
                    erased_parts(stack[depth - 1].value(layout, register), ty)
                });
                let Some(FrameValue::Properties(data)) = memory.heap.slots[id].as_mut() else {
                    unreachable!("an object handle names its properties");
                };
                match instruction {
                    Instruction::ObjectGet(dst, ..) | Instruction::ObjectGetKey(dst, ..) => {
                        let name = name.expect("a property name");
                        let at = data.find(&name).ok_or(fault(ObjectFault::Missing))?;
                        let (ty, value) = data.properties[at].value.clone();
                        let wanted = body.registers()[dst.index()].value();
                        let result = if wanted == Some(ValueType::Dynamic) {
                            FrameValue::Dynamic(Box::new((ty, memory.clone_value(&value))))
                        } else if wanted == Some(ty) {
                            memory.clone_value(&value)
                        } else {
                            return Err(fault(ObjectFault::Type));
                        };
                        stack[depth - 1].set_value(layout, dst, result);
                    }
                    Instruction::ObjectSet(..) | Instruction::ObjectSetKey(..) => {
                        let name = name.expect("a property name");
                        let Some(new) = stored else {
                            unreachable!("a write has a value");
                        };
                        if data.readonly {
                            return Err(fault(ObjectFault::Readonly));
                        }
                        match data.find(&name) {
                            Some(at) => {
                                let property = &mut data.properties[at];
                                if let Some(required) = property.required
                                    && required != ValueType::Dynamic
                                    && required != new.0
                                {
                                    return Err(fault(ObjectFault::Contract));
                                }
                                property.value = new;
                            }
                            None => data.properties.push(Property {
                                name,
                                value: new,
                                required: None,
                            }),
                        }
                    }
                    Instruction::ObjectDelete(dst, ..) | Instruction::ObjectDeleteKey(dst, ..) => {
                        let name = name.expect("a property name");
                        if data.readonly {
                            return Err(fault(ObjectFault::Readonly));
                        }
                        let existed = match data.find(&name) {
                            Some(at) => {
                                if data.properties[at].required.is_some() {
                                    return Err(fault(ObjectFault::Contract));
                                }
                                data.properties.remove(at);
                                true
                            }
                            None => false,
                        };
                        stack[depth - 1].set(layout, dst, Scalar::Bool(existed));
                    }
                    Instruction::ObjectView(dst, _, contract) => {
                        view_object(data, &body.contracts[contract.index()]).map_err(fault)?;
                        stack[depth - 1].set_value(layout, dst, FrameValue::OpenObject(id));
                    }
                    Instruction::ObjectTryView(dst, _, contract) => {
                        let outcome = view_object(data, &body.contracts[contract.index()]);
                        let (ok, err) = body.failure_result_runtime(dst);
                        let result = match outcome {
                            Ok(()) => {
                                let object = FrameValue::OpenObject(id);
                                let wrapped = matches!(
                                    body.enum_variants(dst),
                                    Ok(variants) if variants[ok as usize] == Some(ValueType::Dynamic)
                                );
                                Composite::Enum {
                                    variant: ok,
                                    payload: Some(Box::new(if wrapped {
                                        FrameValue::Dynamic(Box::new((
                                            ValueType::OpenObject,
                                            object,
                                        )))
                                    } else {
                                        object
                                    })),
                                }
                            }
                            Err(why) => Composite::Enum {
                                variant: err,
                                payload: Some(Box::new(FrameValue::Failure(FailureData::plain(
                                    match why {
                                        ObjectFault::Missing => {
                                            "the object lacks a required property"
                                        }
                                        ObjectFault::Type => {
                                            "a property has another type than required"
                                        }
                                        ObjectFault::Contract => {
                                            "the view conflicts with a required property"
                                        }
                                        ObjectFault::Readonly => "the object is readonly",
                                    },
                                )))),
                            },
                        };
                        stack[depth - 1].set_value(layout, dst, FrameValue::Composite(result));
                    }
                    _ => data.readonly = true,
                }
            }
            Instruction::Erase(dst, source) => {
                let ty = body.registers()[source.index()]
                    .value()
                    .unwrap_or(ValueType::Dynamic);
                let value = frame.value(layout, source);
                let erased = match value {
                    FrameValue::Dynamic(erased) => FrameValue::Dynamic(erased),
                    value => FrameValue::Dynamic(Box::new((ty, value))),
                };
                frame.set_value(layout, dst, erased);
            }
            Instruction::DynamicCast(dst, source) => {
                let FrameValue::Dynamic(erased) = frame.value(layout, source) else {
                    unreachable!("verified erased operand");
                };
                if body.registers()[dst.index()].value() != Some(erased.0) {
                    return Err(RunError::Object {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                        fault: ObjectFault::Type,
                    });
                }
                frame.set_value(layout, dst, erased.1);
            }
            Instruction::DynamicBinary(op, dst, lhs, rhs) => {
                let (FrameValue::Dynamic(left), FrameValue::Dynamic(right)) =
                    (frame.value(layout, lhs), frame.value(layout, rhs))
                else {
                    unreachable!("verified erased operands");
                };
                let result =
                    dynamic_binary(op, &left, &right).map_err(|failure| match failure {
                        DynamicFailure::Type => RunError::Object {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                            fault: ObjectFault::Type,
                        },
                        DynamicFailure::Overflow => RunError::IntegerOverflow {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        },
                        DynamicFailure::Zero => RunError::DivisionByZero {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        },
                    })?;
                let value = match (&result, body.registers()[dst.index()]) {
                    (
                        (ValueType::Scalar(ScalarType::Bool), FrameValue::Scalar(bool_)),
                        RegisterType::Scalar(_),
                    ) => FrameValue::Scalar(*bool_),
                    _ => FrameValue::Dynamic(Box::new(result)),
                };
                frame.set_value(layout, dst, value);
            }
            Instruction::DynamicUnary(op, dst, source) => {
                let FrameValue::Dynamic(operand) = frame.value(layout, source) else {
                    unreachable!("verified erased operand");
                };
                let result = match (op, &*operand) {
                    (
                        DynamicOp::Negate,
                        (
                            ValueType::Scalar(ScalarType::I64),
                            FrameValue::Scalar(Scalar::I64(value)),
                        ),
                    ) => value
                        .checked_neg()
                        .map(|value| {
                            (
                                ValueType::Scalar(ScalarType::I64),
                                FrameValue::Scalar(Scalar::I64(value)),
                            )
                        })
                        .ok_or(RunError::IntegerOverflow {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        })?,
                    (
                        DynamicOp::Negate,
                        (ValueType::Scalar(ScalarType::F64), FrameValue::Scalar(Scalar::F64(bits))),
                    ) => (
                        ValueType::Scalar(ScalarType::F64),
                        FrameValue::Scalar(Scalar::F64((-f64::from_bits(*bits)).to_bits())),
                    ),
                    (
                        DynamicOp::Not,
                        (
                            ValueType::Scalar(ScalarType::Bool),
                            FrameValue::Scalar(Scalar::Bool(value)),
                        ),
                    ) => (
                        ValueType::Scalar(ScalarType::Bool),
                        FrameValue::Scalar(Scalar::Bool(!value)),
                    ),
                    _ => {
                        return Err(RunError::Object {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                            fault: ObjectFault::Type,
                        });
                    }
                };
                frame.set_value(layout, dst, FrameValue::Dynamic(Box::new(result)));
            }
            Instruction::FailureFlag(dst, failure, which) => {
                let FrameValue::Failure(data) = frame.value(layout, failure) else {
                    unreachable!("verified failure operand");
                };
                let flag = match which {
                    0 => data.not_found,
                    1 => data.denied,
                    _ => data.check,
                };
                frame.set(layout, dst, Scalar::Bool(flag));
            }
            Instruction::FailureMessage(dst, failure) => {
                let FrameValue::Failure(data) = frame.value(layout, failure) else {
                    unreachable!("verified failure operand");
                };
                let text: Arc<str> = data.message.as_ref().into();
                frame.set_value(layout, dst, FrameValue::Text(text));
            }
            Instruction::ModuleLoad(destination, path) => {
                let path = frame.text(layout, path).clone();
                return Ok(StepOutcome::Load {
                    destination,
                    path,
                    span: body.spans[pc],
                });
            }
            Instruction::DynamicInvoke(dst, callee, list) => {
                let fault = || RunError::Object {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                    fault: ObjectFault::Type,
                };
                let FrameValue::Dynamic(erased) = frame.value(layout, callee) else {
                    unreachable!("verified erased callee");
                };
                if let (ValueType::NativeCallable, FrameValue::NativeCallable(declaration)) =
                    *erased
                {
                    let arguments = body.lists[list.index()]
                        .iter()
                        .map(|&register| {
                            let ty = body.registers()[register.index()]
                                .value()
                                .unwrap_or(ValueType::Dynamic);
                            erased_parts(frame.value(layout, register), ty)
                        })
                        .collect();
                    return Ok(StepOutcome::NativeCall {
                        destination: dst,
                        declaration,
                        arguments,
                        position: (function, from, body.spans[pc]),
                    });
                }
                let (ValueType::Function(signature), FrameValue::Closure(closure)) = *erased else {
                    return Err(fault());
                };
                let description = &body.signatures[signature.index()];
                let arguments = &body.lists[list.index()];
                if arguments.len() != description.parameters.len() {
                    return Err(fault());
                }
                let mut values = Vec::with_capacity(arguments.len());
                for (&register, &expected) in arguments.iter().zip(description.parameters.iter()) {
                    let ty = body.registers()[register.index()]
                        .value()
                        .unwrap_or(ValueType::Dynamic);
                    let (actual, value) = erased_parts(frame.value(layout, register), ty);
                    if expected == ValueType::Dynamic {
                        values.push(FrameValue::Dynamic(Box::new((actual, value))));
                    } else if actual == expected {
                        values.push(value);
                    } else {
                        return Err(fault());
                    }
                }
                if depth == max_frames {
                    return Err(RunError::FrameLimit);
                }
                let target = &self.functions[closure.function.index()];
                let all = closure.captures.iter().cloned().chain(values);
                let mut next = Frame::new(closure.function, target, all, Some(dst));
                next.wrap = Some(description.result);
                stack.push(next);
            }
            Instruction::Count(op, dst, lhs, rhs) => {
                let (Scalar::U64(a), Scalar::U64(b)) =
                    (frame.get(layout, lhs), frame.get(layout, rhs))
                else {
                    unreachable!("verified count operands");
                };
                let overflow = || RunError::IntegerOverflow {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                };
                let result = match op {
                    IntegerOp::Add => Scalar::U64(a.checked_add(b).ok_or_else(overflow)?),
                    IntegerOp::Subtract => Scalar::U64(a.checked_sub(b).ok_or_else(overflow)?),
                    IntegerOp::Multiply => Scalar::U64(a.checked_mul(b).ok_or_else(overflow)?),
                    IntegerOp::Divide | IntegerOp::Remainder => {
                        if b == 0 {
                            return Err(RunError::DivisionByZero {
                                function,
                                instruction: from,
                                span: body.spans[pc],
                            });
                        }
                        Scalar::U64(if op == IntegerOp::Divide {
                            a / b
                        } else {
                            a % b
                        })
                    }
                    IntegerOp::Less => Scalar::Bool(a < b),
                    IntegerOp::LessEqual => Scalar::Bool(a <= b),
                    IntegerOp::Greater => Scalar::Bool(a > b),
                    IntegerOp::GreaterEqual => Scalar::Bool(a >= b),
                    IntegerOp::Equal => Scalar::Bool(a == b),
                    IntegerOp::NotEqual => Scalar::Bool(a != b),
                };
                frame.set(layout, dst, result);
            }
            Instruction::Random(dst, bound) => {
                let bound = match frame.get(layout, bound) {
                    Scalar::I64(value) => value as f64,
                    Scalar::F64(bits) => f64::from_bits(bits),
                    _ => unreachable!("verified number operand"),
                };
                if !bound.is_finite() || bound <= 0.0 {
                    return Err(RunError::RandomBound {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                memory.random_seed = memory
                    .random_seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let value = (memory.random_seed >> 11) as f64 / 9_007_199_254_740_992.0 * bound;
                let value = if value < bound {
                    value
                } else {
                    f64::from_bits(bound.to_bits() - 1)
                };
                frame.set(layout, dst, Scalar::F64(value.to_bits()));
            }
            Instruction::NativeCallableConst(dst, declaration) => {
                frame.set_value(layout, dst, FrameValue::NativeCallable(declaration));
            }
            Instruction::ServiceContext(..)
            | Instruction::ServiceExtend(..)
            | Instruction::ServiceIsolate(..)
            | Instruction::ServiceKeyConst(..)
            | Instruction::ServiceReference(..)
            | Instruction::ServiceProvide(..)
            | Instruction::ServiceAvailable(..)
            | Instruction::ServiceInject(..)
            | Instruction::ServiceOnDispose(..)
            | Instruction::ServiceDispose(..) => {
                let fault = |fault| RunError::Service {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                    fault,
                };
                let instruction = body.instructions[pc];
                // Contexts, keys and registrations may be held directly or
                // through a loan of the place that holds them.
                let held = |memory: &Memory, register: Register, stack: &[Frame]| {
                    self.held_value(stack, memory, body, register)
                        .map_err(|fault| place_error(fault, function, from, body.spans[pc]))
                };
                let context_of = |value: Option<FrameValue>| match value {
                    Some(FrameValue::Context(view)) => view,
                    _ => unreachable!("verified context operand"),
                };
                let key_of = |value: Option<FrameValue>| match value {
                    Some(FrameValue::ServiceKey(key)) => key,
                    _ => unreachable!("verified service key operand"),
                };
                let mut plan: Vec<(usize, FrameValue)> = Vec::new();
                match instruction {
                    Instruction::ServiceContext(dst) => {
                        let Some(view) = memory.services.new_view(None) else {
                            return Err(fault(ServiceFault::Limit));
                        };
                        stack[depth - 1].set_value(layout, dst, FrameValue::Context(view));
                    }
                    Instruction::ServiceKeyConst(dst, key) => {
                        frame.set_value(layout, dst, FrameValue::ServiceKey(key as usize));
                    }
                    Instruction::ServiceReference(dst, ctx, key) => {
                        let view = context_of(held(memory, ctx, stack)?);
                        let key = key_of(held(memory, key, stack)?);
                        stack[depth - 1].set_value(layout, dst, FrameValue::ServiceRef(view, key));
                    }
                    Instruction::ServiceAvailable(dst, ctx, key) => {
                        let view = context_of(held(memory, ctx, stack)?);
                        let key = key_of(held(memory, key, stack)?);
                        let scopes = service_scopes(stack, memory);
                        let available = memory
                            .services
                            .selected(view, key, scopes.contains(&view))
                            .is_ok();
                        stack[depth - 1].set(layout, dst, Scalar::Bool(available));
                    }
                    Instruction::ServiceExtend(dst, ctx)
                    | Instruction::ServiceIsolate(dst, ctx, _) => {
                        let view = context_of(held(memory, ctx, stack)?);
                        if memory.services.views[view].state != ServiceState::Active {
                            return Err(fault(ServiceFault::InactiveContext));
                        }
                        let Some(child) = memory.services.new_view(Some(view)) else {
                            return Err(fault(ServiceFault::Limit));
                        };
                        if let Instruction::ServiceIsolate(_, _, key) = instruction {
                            let key = key_of(held(memory, key, stack)?);
                            memory.services.next_realm += 1;
                            let realm = memory.services.next_realm;
                            let child_view = &mut memory.services.views[child];
                            child_view.realms.insert(key, realm);
                            child_view.admitted.remove(&key);
                        }
                        stack[depth - 1].set_value(layout, dst, FrameValue::Context(child));
                    }
                    Instruction::ServiceProvide(dst, list) => {
                        let &[ctx, key, value] = &*body.lists[list.index()] else {
                            unreachable!("verified provide operands");
                        };
                        let view = context_of(held(memory, ctx, stack)?);
                        let key = key_of(held(memory, key, stack)?);
                        let value = stack[depth - 1].value(layout, value);
                        if memory.services.views[view].state != ServiceState::Active {
                            return Err(fault(ServiceFault::InactiveContext));
                        }
                        let realm = memory.services.realm(view, key);
                        if memory.services.active.contains_key(&(key, realm)) {
                            return Err(fault(ServiceFault::ProviderConflict));
                        }
                        if memory.services.bindings.len() >= MAX_SERVICE_OBJECTS {
                            return Err(fault(ServiceFault::Limit));
                        }
                        let binding = memory.services.bindings.len();
                        memory.services.bindings.push(ServiceBinding {
                            owner: view,
                            key,
                            realm,
                            state: ServiceState::Active,
                            value: Some(value),
                            leases: 0,
                        });
                        memory.services.views[view].provided.insert(key, binding);
                        memory.services.active.insert((key, realm), binding);
                        stack[depth - 1].set_value(layout, dst, FrameValue::Registration(binding));
                    }
                    Instruction::ServiceOnDispose(ctx, callback) => {
                        let view = context_of(held(memory, ctx, stack)?);
                        if memory.services.views[view].state != ServiceState::Active {
                            return Err(fault(ServiceFault::InactiveContext));
                        }
                        let callback = stack[depth - 1].value(layout, callback);
                        memory.services.views[view].cleanups.push(callback);
                    }
                    Instruction::ServiceInject(list) => {
                        let &[ctx, key, callback] = &*body.lists[list.index()] else {
                            unreachable!("verified inject operands");
                        };
                        let view = context_of(held(memory, ctx, stack)?);
                        let key = key_of(held(memory, key, stack)?);
                        let FrameValue::Closure(callback) =
                            stack[depth - 1].value(layout, callback)
                        else {
                            unreachable!("verified callback");
                        };
                        if memory.services.views[view].state != ServiceState::Active {
                            return Err(fault(ServiceFault::InactiveContext));
                        }
                        let scopes = service_scopes(stack, memory);
                        let binding = memory
                            .services
                            .selected(view, key, scopes.contains(&view))
                            .map_err(fault)?;
                        let Some(child) = memory.services.new_view(Some(view)) else {
                            return Err(fault(ServiceFault::Limit));
                        };
                        let child_view = &mut memory.services.views[child];
                        child_view.checked = true;
                        child_view.admitted.insert(key, binding);
                        if depth == max_frames {
                            return Err(RunError::FrameLimit);
                        }
                        let target = &self.functions[callback.function.index()];
                        let values = callback
                            .captures
                            .iter()
                            .cloned()
                            .chain(std::iter::once(FrameValue::Context(child)));
                        let mut next = Frame::new(callback.function, target, values, None);
                        next.scope = Some(child);
                        stack.push(next);
                    }
                    Instruction::ServiceDispose(target) => {
                        match held(memory, target, stack)? {
                            Some(FrameValue::Registration(binding)) => {
                                memory.services.collect_binding_cleanup(binding, &mut plan);
                            }
                            Some(FrameValue::Context(view)) => {
                                memory.services.collect_view_cleanup(view, &mut plan);
                            }
                            _ => unreachable!("verified disposal target"),
                        }
                        // Cleanups run in the order the plan was collected: the
                        // last pushed runs first.
                        let count = plan.len();
                        if stack.len() + count > max_frames {
                            return Err(RunError::FrameLimit);
                        }
                        for (view, callback) in plan.into_iter().rev() {
                            let FrameValue::Closure(callback) = callback else {
                                unreachable!("a cleanup is a closure");
                            };
                            let target = &self.functions[callback.function.index()];
                            let values = callback.captures.iter().cloned();
                            let mut next = Frame::new(callback.function, target, values, None);
                            next.scope = Some(view);
                            stack.push(next);
                        }
                        let scopes = service_scopes(stack, memory);
                        memory.services.release_retired(&scopes);
                    }
                    _ => unreachable!("matched service instruction"),
                }
            }
            Instruction::FrozenValue(dst, id) => {
                fn build(frozen: &Frozen) -> FrameValue {
                    match frozen {
                        Frozen::Scalar(scalar) => FrameValue::Scalar(*scalar),
                        Frozen::Text(text) => FrameValue::Text(text.clone()),
                        Frozen::Tuple(items) => FrameValue::Composite(Composite::Tuple(
                            items.iter().map(build).collect(),
                        )),
                    }
                }
                let value = build(&body.frozen[id.index()]);
                frame.set_value(layout, dst, value);
            }
            Instruction::AnyErase(dst, source) => {
                let ty = body.registers()[source.index()]
                    .value()
                    .unwrap_or(ValueType::Dynamic);
                let value = frame.value(layout, source);
                frame.set_value(layout, dst, FrameValue::Any(Box::new((ty, value))));
            }
            Instruction::AnyChecked(dst, source) => {
                let FrameValue::Any(erased) = frame.value(layout, source) else {
                    unreachable!("verified erased operand");
                };
                if body.registers()[dst.index()].value() != Some(erased.0) {
                    return Err(RunError::Object {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                        fault: ObjectFault::Type,
                    });
                }
                frame.set_value(layout, dst, erased.1);
            }
            Instruction::ClockNew(dst) => frame.set_value(layout, dst, FrameValue::Clock),
            Instruction::ClockClone(dst, _) => frame.set_value(layout, dst, FrameValue::Clock),
            Instruction::ClockSleep(_, duration) => {
                let Scalar::U64(milliseconds) = frame.get(layout, duration) else {
                    unreachable!("verified duration");
                };
                let now = Instant::now();
                let deadline = *memory.sleeping_until.get_or_insert_with(|| {
                    now + Duration::from_millis(milliseconds.min(u64::from(u32::MAX) * 1000))
                });
                if now < deadline {
                    stack[depth - 1].pc -= 1;
                    return Ok(StepOutcome::Blocked);
                }
                memory.sleeping_until = None;
            }
            Instruction::TaskSpawn(dst, closure) => {
                let FrameValue::Closure(closure) = frame.value(layout, closure) else {
                    unreachable!("verified closure operand");
                };
                let target = &self.functions[closure.function.index()];
                let captured: Vec<FrameValue> = closure.captures.to_vec();
                if !captured.iter().all(transferable) {
                    return Err(RunError::NotTransferable {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                let task = Frame::new(closure.function, target, captured.into_iter(), None);
                let record = TaskRecord {
                    stack: vec![task],
                    end: None,
                    cancelled: false,
                    waiting: None,
                    sleeping_until: None,
                };
                let id = if let Some(id) = memory.free_tasks.pop() {
                    memory.tasks[id] = Some(record);
                    id
                } else if memory.tasks.len() < MAX_TASKS {
                    memory.tasks.push(Some(record));
                    memory.tasks.len() - 1
                } else {
                    return Err(RunError::TaskLimit {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                };
                stack[depth - 1].set_value(layout, dst, FrameValue::Task(id));
            }
            Instruction::TaskJoin(dst, task)
            | Instruction::TaskIsFinished(dst, task)
            | Instruction::TaskCancel(task @ dst) => {
                let held = self
                    .held_value(stack, memory, body, task)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Task(id)) = held else {
                    unreachable!("verified task operand");
                };
                let finished = memory.tasks[id]
                    .as_ref()
                    .is_some_and(|task| task.end.is_some());
                match body.instructions[pc] {
                    Instruction::TaskIsFinished(..) => {
                        stack[depth - 1].set_value(
                            layout,
                            dst,
                            FrameValue::Scalar(Scalar::Bool(finished)),
                        );
                    }
                    Instruction::TaskCancel(..) => {
                        if let Some(record) = memory.tasks[id].as_mut() {
                            record.cancelled = true;
                        }
                    }
                    _ => {
                        if !finished {
                            stack[depth - 1].pc -= 1;
                            return Ok(StepOutcome::Blocked);
                        }
                        let Some(record) = memory.tasks[id].take() else {
                            unreachable!("a joined task exists");
                        };
                        memory.free_tasks.push(id);
                        let outcome = match record.end {
                            Some(TaskEnd::Completed(value)) => Composite::Enum {
                                variant: 0,
                                payload: Some(Box::new(value.unwrap_or_else(unit))),
                            },
                            Some(TaskEnd::Failed) => Composite::Enum {
                                variant: 1,
                                payload: None,
                            },
                            _ => Composite::Enum {
                                variant: 2,
                                payload: None,
                            },
                        };
                        stack[depth - 1].set_value(layout, dst, FrameValue::Composite(outcome));
                    }
                }
            }
            Instruction::SendErrorValue(dst, error) => {
                let held = self
                    .held_value(stack, memory, body, error)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Composite(Composite::Enum {
                    payload: Some(payload),
                    ..
                })) = held
                else {
                    unreachable!("verified send error");
                };
                stack[depth - 1].set_value(layout, dst, *payload);
            }
            Instruction::Yield(dst, value) => {
                let Some((id, resumer)) = frame.generator.take() else {
                    unreachable!("verified yield inside a generator");
                };
                let yielded = frame.value(layout, value);
                let Some(suspended) = stack.pop() else {
                    unreachable!("the yielding frame is on the stack");
                };
                let slot = memory.generator(id);
                slot.state = GeneratorState::Suspended {
                    input: dst,
                    depth: stack.len(),
                };
                slot.frame = Some(suspended);
                let caller = stack
                    .last_mut()
                    .expect("a generator runs above its resumer");
                let caller_body = &self.functions[caller.function.index()];
                let caller_layout = caller_body.frame_layout();
                caller.set_value(caller_layout, resumer, step(0, yielded));
            }
            Instruction::GeneratorStart(dst, generator)
            | Instruction::GeneratorNext(dst, generator)
            | Instruction::GeneratorResumeUnit(dst, generator)
            | Instruction::GeneratorResume(dst, generator, _) => {
                let supplied = match body.instructions[pc] {
                    Instruction::GeneratorResume(_, _, value) => {
                        Some(stack[depth - 1].value(layout, value))
                    }
                    _ => None,
                };
                let held = self
                    .held_value(stack, memory, body, generator)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Generator(id)) = held else {
                    unreachable!("verified generator operand");
                };
                let fault = |fault| RunError::Generator {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                    fault,
                };
                let instruction = body.instructions[pc];
                let state = memory.generator(id).state;
                let input = match (state, instruction) {
                    (GeneratorState::Running, _) => {
                        return Err(fault(GeneratorFault::AlreadyRunning));
                    }
                    (GeneratorState::Done, _) => return Err(fault(GeneratorFault::Finished)),
                    (GeneratorState::New, Instruction::GeneratorResumeUnit(..))
                    | (GeneratorState::New, Instruction::GeneratorResume(..)) => {
                        return Err(fault(GeneratorFault::NotStarted));
                    }
                    (GeneratorState::Suspended { .. }, Instruction::GeneratorStart(..)) => {
                        return Err(fault(GeneratorFault::AlreadyStarted));
                    }
                    (GeneratorState::New, _) => None,
                    (
                        GeneratorState::Suspended { input, depth },
                        Instruction::GeneratorResume(..),
                    ) => Some((input, depth, supplied.expect("a resume value"))),
                    (GeneratorState::Suspended { input, depth }, _) => Some((
                        input,
                        depth,
                        FrameValue::Composite(Composite::Tuple(Box::new([]))),
                    )),
                };
                if depth >= max_frames {
                    return Err(RunError::FrameLimit);
                }
                let slot = memory.generator(id);
                slot.state = GeneratorState::Running;
                let Some(mut resumed) = slot.frame.take() else {
                    unreachable!("an idle generator keeps its frame");
                };
                let resumed_layout = self.functions[resumed.function.index()].frame_layout();
                if let Some((register, from_depth, value)) = input {
                    resumed.set_value(resumed_layout, register, value);
                    // The frame runs at a new stack index: loans of its own
                    // registers follow it.
                    for held in resumed.slots.iter_mut().flatten() {
                        if let FrameValue::Loan(place) = held
                            && let Root::Frame {
                                frame: owner,
                                register,
                            } = place.root
                            && owner == from_depth
                        {
                            place.root = Root::Frame {
                                frame: depth,
                                register,
                            };
                        }
                    }
                }
                resumed.generator = Some((id, dst));
                stack.push(resumed);
            }
            Instruction::GeneratorClose(generator) => {
                let held = self
                    .held_value(stack, memory, body, generator)
                    .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                let Some(FrameValue::Generator(id)) = held else {
                    unreachable!("verified generator operand");
                };
                if matches!(memory.generator(id).state, GeneratorState::Running) {
                    return Err(RunError::Generator {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                        fault: GeneratorFault::AlreadyRunning,
                    });
                }
                memory.close_generator(id);
            }
            Instruction::Return(value) => {
                let mut value = value.map(|reg| frame.value(layout, reg));
                if let Some(wrapped) = frame.wrap.take() {
                    value = Some(match (wrapped, value) {
                        (Some(ty), Some(value)) => FrameValue::Dynamic(Box::new((ty, value))),
                        _ => FrameValue::Dynamic(Box::new((ValueType::Dynamic, unit()))),
                    });
                }
                if let Some(key) = frame.namespace.take()
                    && let Some(FrameValue::Composite(Composite::Enum {
                        payload: Some(namespace),
                        ..
                    })) = &value
                    && matches!(**namespace, FrameValue::Dynamic(_))
                {
                    memory.namespaces.insert(key, (**namespace).clone());
                }
                let destination = frame.destination;
                let receiver = frame.mapped.take();
                let finalizing = frame.finalizing.take();
                let leased = frame.lease.take();
                let scoped = frame.scope.is_some();
                stack.pop();
                if leased.is_some() || scoped {
                    if let Some(binding) = leased {
                        memory.services.bindings[binding].leases -= 1;
                    }
                    let scopes = service_scopes(stack, memory);
                    memory.services.release_retired(&scopes);
                }
                if let Some(block) = finalizing {
                    memory.finish_finalizer(block);
                }
                let Some(caller) = stack.last_mut() else {
                    return Ok(StepOutcome::Returned(value));
                };
                let caller_body = &self.functions[caller.function.index()];
                let layout = caller_body.frame_layout();
                if let Some(receiver) = receiver {
                    let (Some(dst), Some(FrameValue::Path(path))) = (destination, value) else {
                        unreachable!("verified mapped call");
                    };
                    let call = caller.pc - 1;
                    let (function, instruction, span) = (
                        caller.function,
                        instruction_id(call),
                        caller_body.spans[call],
                    );
                    let fail = |fault| RunError::Path {
                        function,
                        instruction,
                        span,
                        fault,
                    };
                    let (some, none) = caller_body.option_variants(dst);
                    let expected = caller_body.option_payload_runtime(dst);
                    let found = match &path {
                        None => None,
                        Some(steps) => {
                            let reached = apply_path(memory, &receiver, steps).map_err(fail)?;
                            if !conforms(reached, expected) {
                                return Err(fail(PathFault::Mismatch));
                            }
                            Some(reached.clone())
                        }
                    };
                    let option = match found {
                        Some(value) => Composite::Enum {
                            variant: some,
                            payload: Some(Box::new(memory.clone_value(&value))),
                        },
                        None => Composite::Enum {
                            variant: none,
                            payload: None,
                        },
                    };
                    caller.set_value(layout, dst, FrameValue::Composite(option));
                } else if let Some((dst, value)) = destination.zip(value) {
                    caller.set_value(layout, dst, value);
                }
            }
            Instruction::Tuple(dst, list) => {
                let fields = body.lists[list.index()]
                    .iter()
                    .map(|&field| frame.value(layout, field))
                    .collect();
                frame.set_value(layout, dst, FrameValue::Composite(Composite::Tuple(fields)));
            }
            Instruction::Extract(dst, src, field) => {
                let Composite::Tuple(fields) = frame.composite(layout, src) else {
                    unreachable!("verified tuple operand");
                };
                let value = fields[field as usize].clone();
                frame.set_value(layout, dst, value);
            }
            Instruction::EnumUnit(dst, variant) => frame.set_value(
                layout,
                dst,
                FrameValue::Composite(Composite::Enum {
                    variant,
                    payload: None,
                }),
            ),
            Instruction::EnumWrap(dst, variant, payload) => {
                let payload = Some(Box::new(frame.value(layout, payload)));
                frame.set_value(
                    layout,
                    dst,
                    FrameValue::Composite(Composite::Enum { variant, payload }),
                );
            }
            Instruction::EnumIs(dst, src, variant) => {
                let is = if matches!(body.registers()[src.index()], RegisterType::Loan { .. }) {
                    let place = frame.loan(layout, src);
                    let root = self.root_value(stack, memory, &place.root).ok_or(
                        RunError::UninitializedGlobal {
                            function,
                            instruction: from,
                            span: body.spans[pc],
                        },
                    )?;
                    let held = read_place(&root, &place.path)
                        .map_err(|fault| place_error(fault, function, from, body.spans[pc]))?;
                    let FrameValue::Composite(Composite::Enum { variant: held, .. }) = held else {
                        unreachable!("verified enum operand");
                    };
                    held == variant
                } else {
                    let Composite::Enum { variant: held, .. } = frame.composite(layout, src) else {
                        unreachable!("verified enum operand");
                    };
                    *held == variant
                };
                stack[depth - 1].set(layout, dst, Scalar::Bool(is));
            }
            Instruction::EnumExpect(src, variant) => {
                let Composite::Enum { variant: held, .. } = frame.composite(layout, src) else {
                    unreachable!("verified enum operand");
                };
                if *held != variant {
                    return Err(RunError::EnumVariant {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
            }
            Instruction::EnumPayload(dst, src, variant) => {
                let Composite::Enum {
                    variant: held,
                    payload,
                } = frame.composite(layout, src)
                else {
                    unreachable!("verified enum operand");
                };
                let Some(payload) = payload.as_deref().filter(|_| *held == variant) else {
                    return Err(RunError::EnumVariant {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                };
                let payload = payload.clone();
                frame.set_value(layout, dst, payload);
            }
            Instruction::Float(op, dst, lhs, rhs) => {
                let (Scalar::F64(lhs), Scalar::F64(rhs)) =
                    (frame.get(layout, lhs), frame.get(layout, rhs))
                else {
                    unreachable!("verified f64 operands");
                };
                let value = evaluate_float(op, f64::from_bits(lhs), f64::from_bits(rhs)).ok_or(
                    RunError::DivisionByZero {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    },
                )?;
                frame.set(layout, dst, value);
            }
            Instruction::RealDivide(dst, lhs, rhs) => {
                let (Scalar::I64(lhs), Scalar::I64(rhs)) =
                    (frame.get(layout, lhs), frame.get(layout, rhs))
                else {
                    unreachable!("verified integer operands");
                };
                if rhs == 0 {
                    return Err(RunError::DivisionByZero {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
                frame.set(layout, dst, Scalar::from_f64(lhs as f64 / rhs as f64));
            }
            Instruction::Boolean(op, dst, lhs, rhs) => {
                let (Scalar::Bool(lhs), Scalar::Bool(rhs)) =
                    (frame.get(layout, lhs), frame.get(layout, rhs))
                else {
                    unreachable!("verified boolean operands");
                };
                let equal = lhs == rhs;
                let value = if op == BooleanOp::Equal {
                    equal
                } else {
                    !equal
                };
                frame.set(layout, dst, Scalar::Bool(value));
            }
            Instruction::NegateInteger(dst, src) => {
                let Scalar::I64(value) = frame.get(layout, src) else {
                    unreachable!("verified integer operand");
                };
                let value = value.checked_neg().ok_or(RunError::IntegerOverflow {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                })?;
                frame.set(layout, dst, Scalar::I64(value));
            }
            Instruction::NegateFloat(dst, src) => {
                let Scalar::F64(bits) = frame.get(layout, src) else {
                    unreachable!("verified f64 operand");
                };
                frame.set(layout, dst, Scalar::from_f64(-f64::from_bits(bits)));
            }
            Instruction::Not(dst, src) => {
                let Scalar::Bool(value) = frame.get(layout, src) else {
                    unreachable!("verified boolean operand");
                };
                frame.set(layout, dst, Scalar::Bool(!value));
            }
            Instruction::IntegerToFloat(dst, src) => {
                let Scalar::I64(value) = frame.get(layout, src) else {
                    unreachable!("verified integer operand");
                };
                frame.set(layout, dst, Scalar::from_f64(value as f64));
            }
            Instruction::FloatToInteger(dst, src) => {
                let Scalar::F64(bits) = frame.get(layout, src) else {
                    unreachable!("verified f64 operand");
                };
                let value = float_to_integer(f64::from_bits(bits)).ok_or(
                    RunError::ConversionOutOfRange {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    },
                )?;
                frame.set(layout, dst, Scalar::I64(value));
            }
            Instruction::Closure(dst, target, list) => {
                let captures = body.lists[list.index()]
                    .iter()
                    .map(|&capture| frame.value(layout, capture))
                    .collect();
                let closure = Closure {
                    function: target,
                    captures,
                };
                frame.set_value(layout, dst, FrameValue::Closure(Arc::new(closure)));
            }
            Instruction::TextConstant(dst, text) => {
                let value = FrameValue::Text(body.texts[text.index()].clone());
                frame.set_value(layout, dst, value);
            }
            Instruction::TextConcat(dst, lhs, rhs) => {
                let joined = format!("{}{}", frame.text(layout, lhs), frame.text(layout, rhs));
                frame.set_value(layout, dst, FrameValue::Text(joined.into()));
            }
            Instruction::TextCompare(op, dst, lhs, rhs) => {
                let ordering = frame.text(layout, lhs).cmp(frame.text(layout, rhs));
                let value = match op {
                    TextOp::Equal => ordering.is_eq(),
                    TextOp::NotEqual => ordering.is_ne(),
                    TextOp::Less => ordering.is_lt(),
                    TextOp::LessEqual => ordering.is_le(),
                    TextOp::Greater => ordering.is_gt(),
                    TextOp::GreaterEqual => ordering.is_ge(),
                };
                frame.set(layout, dst, Scalar::Bool(value));
            }
            Instruction::TextLength(dst, text) => {
                let length = frame.text(layout, text).chars().count();
                // A string cannot have more characters than the address space.
                let length = i64::try_from(length).unwrap_or(i64::MAX);
                frame.set(layout, dst, Scalar::I64(length));
            }
            Instruction::TextLeft(dst, text, count) => {
                let Scalar::I64(count) = frame.get(layout, count) else {
                    unreachable!("verified integer operand");
                };
                let sliced = slice_left(frame.text(layout, text), count);
                let sliced = sliced
                    .map_err(|failure| text_error(failure, function, from, body.spans[pc]))?;
                frame.set_value(layout, dst, FrameValue::Text(sliced.into()));
            }
            Instruction::TextRight(dst, text, count) => {
                let Scalar::I64(count) = frame.get(layout, count) else {
                    unreachable!("verified integer operand");
                };
                let sliced = slice_right(frame.text(layout, text), count);
                let sliced = sliced
                    .map_err(|failure| text_error(failure, function, from, body.spans[pc]))?;
                frame.set_value(layout, dst, FrameValue::Text(sliced.into()));
            }
            Instruction::TextMid(dst, text, list) => {
                let [start, count] = body.lists[list.index()][..] else {
                    unreachable!("verified two-entry list");
                };
                let (Scalar::I64(start), Scalar::I64(count)) =
                    (frame.get(layout, start), frame.get(layout, count))
                else {
                    unreachable!("verified integer operands");
                };
                let sliced = slice_mid(frame.text(layout, text), start, count);
                let sliced = sliced
                    .map_err(|failure| text_error(failure, function, from, body.spans[pc]))?;
                frame.set_value(layout, dst, FrameValue::Text(sliced.into()));
            }
            Instruction::ArrayDeclare(dst) => {
                let (_, bounds) = body.array_bounds(dst);
                let len = array_len(bounds).unwrap_or(0);
                let elements = vec![None; len].into_boxed_slice();
                frame.set_value(
                    layout,
                    dst,
                    FrameValue::Composite(Composite::Array(elements)),
                );
            }
            Instruction::ArrayNew(dst, list) => {
                let elements = body.lists[list.index()]
                    .iter()
                    .map(|&item| Some(frame.value(layout, item)))
                    .collect();
                frame.set_value(
                    layout,
                    dst,
                    FrameValue::Composite(Composite::Array(elements)),
                );
            }
            Instruction::ArrayLoad(dst, array, list) if body.is_vec(array) => {
                let index = scalar_index(frame.get(layout, body.lists[list.index()][0]));
                let Composite::Vec(items) = frame.composite(layout, array) else {
                    unreachable!("verified vector operand");
                };
                let value = usize::try_from(index)
                    .ok()
                    .and_then(|index| items.get(index))
                    .cloned()
                    .ok_or(RunError::IndexOutOfBounds {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                frame.set_value(layout, dst, value);
            }
            Instruction::ArrayStore(array, list, value) if body.is_vec(array) => {
                let index = scalar_index(frame.get(layout, body.lists[list.index()][0]));
                let value = frame.value(layout, value);
                let Composite::Vec(items) = frame.composite_mut(layout, array) else {
                    unreachable!("verified vector operand");
                };
                let slot = usize::try_from(index)
                    .ok()
                    .and_then(|index| items.get_mut(index))
                    .ok_or(RunError::IndexOutOfBounds {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                *slot = value;
            }
            Instruction::ArrayLength(dst, array) if body.is_vec(array) => {
                let Composite::Vec(items) = frame.composite(layout, array) else {
                    unreachable!("verified vector operand");
                };
                let length = i64::try_from(items.len()).unwrap_or(i64::MAX);
                frame.set(layout, dst, Scalar::I64(length));
            }
            Instruction::ArrayLoad(dst, array, list) => {
                let offset = element_offset(frame, layout, body.array_bounds(array).1, list, body)
                    .ok_or(RunError::IndexOutOfBounds {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                let Composite::Array(elements) = frame.composite(layout, array) else {
                    unreachable!("verified array operand");
                };
                let value = elements[offset]
                    .clone()
                    .ok_or(RunError::UninitializedElement {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                frame.set_value(layout, dst, value);
            }
            Instruction::ArrayStore(array, list, value) => {
                let offset = element_offset(frame, layout, body.array_bounds(array).1, list, body)
                    .ok_or(RunError::IndexOutOfBounds {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    })?;
                let value = frame.value(layout, value);
                let Composite::Array(elements) = frame.composite_mut(layout, array) else {
                    unreachable!("verified array operand");
                };
                elements[offset] = Some(value);
            }
            Instruction::ArrayLength(dst, array) => {
                let Composite::Array(elements) = frame.composite(layout, array) else {
                    unreachable!("verified array operand");
                };
                let length = i64::try_from(elements.len()).unwrap_or(i64::MAX);
                frame.set(layout, dst, Scalar::I64(length));
            }
            Instruction::CheckStep(step) => {
                if frame.get(layout, step) == Scalar::I64(0) {
                    return Err(RunError::ZeroStep {
                        function,
                        instruction: from,
                        span: body.spans[pc],
                    });
                }
            }
            Instruction::Trap => {
                return Err(RunError::ReachedTrap {
                    function,
                    instruction: from,
                    span: body.spans[pc],
                });
            }
        }
        // Objects whose last owner went away during this instruction hand
        // their blocks back through their allocators, first released first.
        if !memory.finalizers.is_empty() {
            let blocks: Vec<usize> = memory.finalizers.drain(..).collect();
            for block in blocks.into_iter().rev() {
                if stack.len() >= max_frames {
                    return Err(RunError::FrameLimit);
                }
                let Some(allocator) = memory.block(block).allocator.as_ref() else {
                    unreachable!("a finalizing block has an allocator");
                };
                let (routine, allocation) = (allocator.deallocate, allocator.allocation);
                let target = &self.functions[routine.index()];
                let receiver = FrameValue::Loan(BorrowedPlace {
                    root: Root::Allocator(block),
                    path: Box::new([]),
                    window: None,
                });
                let values = [receiver, FrameValue::Allocation(allocation)];
                let mut next = Frame::new(routine, target, values.into_iter(), None);
                next.finalizing = Some(block);
                stack.push(next);
            }
        }
        Ok(StepOutcome::Continue)
    }

    /// The value an operand's place holds, borrowed in place. `None` when a
    /// module variable is not assigned yet.
    fn container_ref<'a>(
        &self,
        stack: &'a [Frame],
        memory: &'a Memory,
        body: &VerifiedFunction,
        source: Register,
    ) -> Result<Option<&'a FrameValue>, PlaceFault> {
        let Some(frame) = stack.last() else {
            unreachable!("active execution has a frame");
        };
        let layout = body.frame_layout();
        let place = place_of(body, frame, layout, stack.len(), source);
        let root = match place.root {
            Root::Frame { frame, register } => {
                let owner = &stack[frame];
                let owner_layout = self.functions[owner.function.index()].frame_layout();
                owner.slots[slot(owner_layout, register)].as_ref()
            }
            Root::Global(global) => memory.globals[global.index()].as_ref(),
            Root::Heap(object) => memory.heap.slots[object].as_ref(),
            Root::Counted(block) => memory.counted[block]
                .as_ref()
                .and_then(|block| block.payload.as_ref()),
            Root::Allocator(block) => memory.counted[block]
                .as_ref()
                .and_then(|block| block.allocator.as_ref()?.state.as_ref()),
        };
        match root {
            Some(root) => place_ref(root, &place.path).map(Some),
            None => Ok(None),
        }
    }

    /// Like [`Self::container_ref`], for mutation.
    fn container_mut<'a>(
        &self,
        stack: &'a mut [Frame],
        memory: &'a mut Memory,
        body: &VerifiedFunction,
        source: Register,
    ) -> Result<&'a mut FrameValue, PlaceFault> {
        let depth = stack.len();
        let place = {
            let Some(frame) = stack.last() else {
                unreachable!("active execution has a frame");
            };
            place_of(body, frame, body.frame_layout(), depth, source)
        };
        let root = self.root_slot(stack, memory, &place.root);
        match root {
            Some(root) => place_mut(root, &place.path),
            None => Err(PlaceFault::UninitializedElement),
        }
    }

    /// The window a sequence operand covers: its slice window, or all of the
    /// array or vector it is.
    fn sequence_window(
        &self,
        stack: &[Frame],
        memory: &Memory,
        body: &VerifiedFunction,
        source: Register,
    ) -> Result<Option<Window>, PlaceFault> {
        let Some(frame) = stack.last() else {
            unreachable!("active execution has a frame");
        };
        if body.projects_from_slice(source) {
            return Ok(frame.loan(body.frame_layout(), source).window);
        }
        if body.projects_from_vec(source) {
            let held = self
                .container_ref(stack, memory, body, source)?
                .ok_or(PlaceFault::UninitializedElement)?;
            let FrameValue::Composite(Composite::Vec(items)) = held else {
                unreachable!("verified vector operand");
            };
            return Ok(Some(Window {
                start: 0,
                len: items.len(),
            }));
        }
        Ok(array_len(body.projected_array_bounds(source)).map(|len| Window { start: 0, len }))
    }

    /// The elements of the array or slice an operand stands for.
    fn sequence_elements(
        &self,
        stack: &[Frame],
        memory: &Memory,
        body: &VerifiedFunction,
        source: Register,
    ) -> Result<Vec<Option<FrameValue>>, PlaceFault> {
        let Some(frame) = stack.last() else {
            unreachable!("active execution has a frame");
        };
        let layout = body.frame_layout();
        let _ = (frame, layout);
        let window = self.sequence_window(stack, memory, body, source)?;
        let held = self
            .held_value(stack, memory, body, source)?
            .ok_or(PlaceFault::UninitializedElement)?;
        let elements: Vec<Option<FrameValue>> = match held {
            FrameValue::Composite(Composite::Array(elements)) => elements.into_vec(),
            FrameValue::Composite(Composite::Vec(items)) => items.into_iter().map(Some).collect(),
            _ => unreachable!("verified sequence operand"),
        };
        let Some(window) = window else {
            return Ok(elements);
        };
        Ok(elements
            .into_iter()
            .skip(window.start)
            .take(window.len)
            .collect())
    }

    /// What a counted operand holds: the register's own value, or the value
    /// behind its loan. `None` when a module variable is not assigned yet.
    fn held_value(
        &self,
        stack: &[Frame],
        memory: &Memory,
        body: &VerifiedFunction,
        source: Register,
    ) -> Result<Option<FrameValue>, PlaceFault> {
        let Some(frame) = stack.last() else {
            unreachable!("active execution has a frame");
        };
        let layout = body.frame_layout();
        if matches!(body.registers()[source.index()], RegisterType::Loan { .. }) {
            let place = frame.loan(layout, source);
            match self.root_value(stack, memory, &place.root) {
                Some(root) => read_place(&root, &place.path).map(Some),
                None => Ok(None),
            }
        } else {
            Ok(Some(frame.value(layout, source)))
        }
    }

    /// A copy of the value at a place's root, or `None` when a global has
    /// not been assigned yet.
    fn root_value(&self, stack: &[Frame], memory: &Memory, root: &Root) -> Option<FrameValue> {
        match *root {
            Root::Frame { frame, register } => {
                let owner = &stack[frame];
                let layout = self.functions[owner.function.index()].frame_layout();
                Some(owner.value(layout, register))
            }
            Root::Global(global) => memory.globals[global.index()].clone(),
            Root::Heap(object) => memory.heap.slots[object].clone(),
            Root::Counted(block) => memory.counted[block]
                .as_ref()
                .and_then(|block| block.payload.clone()),
            Root::Allocator(block) => memory.counted[block]
                .as_ref()
                .and_then(|block| block.allocator.as_ref()?.state.clone()),
        }
    }

    fn root_slot<'a>(
        &self,
        stack: &'a mut [Frame],
        memory: &'a mut Memory,
        root: &Root,
    ) -> &'a mut Option<FrameValue> {
        match *root {
            Root::Frame { frame, register } => {
                let owner = &mut stack[frame];
                let layout = self.functions[owner.function.index()].frame_layout();
                owner.slot_mut(layout, register)
            }
            Root::Global(global) => &mut memory.globals[global.index()],
            Root::Heap(object) => &mut memory.heap.slots[object],
            Root::Counted(block) => match memory.counted[block].as_mut() {
                Some(block) => &mut block.payload,
                None => unreachable!("a loan outlived its counted object"),
            },
            Root::Allocator(block) => {
                match memory.counted[block]
                    .as_mut()
                    .and_then(|block| block.allocator.as_mut())
                {
                    Some(allocator) => &mut allocator.state,
                    None => unreachable!("a loan outlived its allocator"),
                }
            }
        }
    }

    fn call_bound(
        &self,
        id: HostBindingId,
        call: &CallSite,
        frame: &mut Frame,
        layout: &FrameLayout,
        position: ExecutionPosition,
    ) -> Result<(), RunError> {
        let binding = &self.host_bindings[id.index()];
        let arguments: Vec<_> = call
            .arguments
            .iter()
            .map(|&reg| frame.get(layout, reg))
            .collect();
        // The callback gets only copied values. On unwind this activation
        // terminates without reusing its result slot or replaying the call.
        let result = catch_unwind(AssertUnwindSafe(|| (binding.callback)(&arguments))).map_err(
            |payload| {
                // A user-supplied panic payload may itself panic on drop.
                if let Err(drop_panic) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                    std::mem::forget(drop_panic);
                }
                RunError::BoundCallPanicked {
                    binding: id,
                    position,
                }
            },
        )?;
        let value = result.map_err(|cause| {
            if binding.effects.may_fail {
                RunError::BoundCallFailed {
                    binding: id,
                    position,
                    cause,
                }
            } else {
                RunError::UnexpectedBoundFailure {
                    binding: id,
                    position,
                    cause,
                }
            }
        })?;
        if value.ty() != binding.result {
            return Err(RunError::BoundResultType {
                binding: id,
                position,
                expected: binding.result,
                actual: value.ty(),
            });
        }
        if let Some(destination) = call.destination {
            frame.set(layout, destination, value);
        }
        Ok(())
    }
}

/// Receives each completed output line, without its terminator. A line is
/// delivered exactly once, when its output instruction executes.
pub struct OutputSink(Box<dyn FnMut(&str) + Send>);

impl OutputSink {
    pub fn new(write: impl FnMut(&str) + Send + 'static) -> Self {
        Self(Box::new(write))
    }

    /// Print each line to standard output.
    pub fn stdout() -> Self {
        Self::new(|line| println!("{line}"))
    }

    pub(crate) fn write_line(&mut self, line: &str) {
        (self.0)(line);
    }
}

impl Default for OutputSink {
    fn default() -> Self {
        Self::stdout()
    }
}

impl fmt::Debug for OutputSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OutputSink")
    }
}

/// Supplies input lines, without their terminators. A line is consumed
/// exactly once, when its input instruction executes; `None` is the end of input.
pub struct InputSource {
    next: Box<dyn FnMut() -> Option<String> + Send>,
    /// When no line is left, wait for more instead of failing.
    waits: bool,
}

impl InputSource {
    pub fn new(next: impl FnMut() -> Option<String> + Send + 'static) -> Self {
        Self {
            next: Box::new(next),
            waits: false,
        }
    }

    /// Like [`Self::new`], but an empty source is not the end of input: the
    /// activation pauses on the input instruction until a line is available.
    pub fn waiting(next: impl FnMut() -> Option<String> + Send + 'static) -> Self {
        Self {
            next: Box::new(next),
            waits: true,
        }
    }

    /// Read lines from standard input.
    pub fn stdin() -> Self {
        Self::new(|| {
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => {
                    let kept = line.trim_end_matches(['\n', '\r']).len();
                    line.truncate(kept);
                    Some(line)
                }
            }
        })
    }

    /// Hand out the given lines in order, then end the input.
    pub fn lines(lines: impl IntoIterator<Item = String, IntoIter: Send + 'static>) -> Self {
        let mut lines = lines.into_iter();
        Self::new(move || lines.next())
    }

    fn next_line(&mut self) -> Option<String> {
        (self.next)()
    }
}

impl Default for InputSource {
    fn default() -> Self {
        Self::stdin()
    }
}

impl fmt::Debug for InputSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InputSource")
    }
}

/// The value one input line spells, if it spells one.
fn read_input(line: &str, kind: InputKind) -> Option<FrameValue> {
    Some(match kind {
        InputKind::Text => FrameValue::Text(line.into()),
        InputKind::Integer => FrameValue::Scalar(Scalar::I64(line.trim().parse().ok()?)),
        InputKind::Float => {
            let value: f64 = line.trim().parse().ok()?;
            if !value.is_finite() {
                return None;
            }
            FrameValue::Scalar(Scalar::F64(value.to_bits()))
        }
        InputKind::Bool => match line.trim().to_ascii_uppercase().as_str() {
            "TRUE" => FrameValue::Scalar(Scalar::Bool(true)),
            "FALSE" => FrameValue::Scalar(Scalar::Bool(false)),
            _ => return None,
        },
    })
}

/// A value and its type as an object stores them: an erased value is opened,
/// so what is kept is never `Dynamic`.
fn erased_parts(value: FrameValue, ty: ValueType) -> (ValueType, FrameValue) {
    match value {
        FrameValue::Dynamic(erased) => *erased,
        value => (ty, value),
    }
}

enum DynamicFailure {
    Type,
    Overflow,
    Zero,
}

/// A binary operator on two erased values of the same type.
fn dynamic_binary(
    op: DynamicOp,
    left: &(ValueType, FrameValue),
    right: &(ValueType, FrameValue),
) -> Result<(ValueType, FrameValue), DynamicFailure> {
    use super::DynamicOp::*;
    let int = ValueType::Scalar(ScalarType::I64);
    let float = ValueType::Scalar(ScalarType::F64);
    let boolean = ValueType::Scalar(ScalarType::Bool);
    let flag = |value: bool| (boolean, FrameValue::Scalar(Scalar::Bool(value)));
    match (&left.1, &right.1) {
        (FrameValue::Scalar(Scalar::I64(a)), FrameValue::Scalar(Scalar::I64(b))) => {
            let number = |value: Option<i64>| {
                value
                    .map(|value| (int, FrameValue::Scalar(Scalar::I64(value))))
                    .ok_or(DynamicFailure::Overflow)
            };
            match op {
                Add => number(a.checked_add(*b)),
                Subtract => number(a.checked_sub(*b)),
                Multiply => number(a.checked_mul(*b)),
                Divide | Remainder if *b == 0 => Err(DynamicFailure::Zero),
                Divide => number(a.checked_div(*b)),
                Remainder => number(a.checked_rem(*b)),
                Equal => Ok(flag(a == b)),
                NotEqual => Ok(flag(a != b)),
                Less => Ok(flag(a < b)),
                Greater => Ok(flag(a > b)),
                LessEqual => Ok(flag(a <= b)),
                GreaterEqual => Ok(flag(a >= b)),
                _ => Err(DynamicFailure::Type),
            }
        }
        (FrameValue::Scalar(Scalar::F64(a)), FrameValue::Scalar(Scalar::F64(b))) => {
            let (a, b) = (f64::from_bits(*a), f64::from_bits(*b));
            let number = |value: f64| (float, FrameValue::Scalar(Scalar::F64(value.to_bits())));
            match op {
                Add => Ok(number(a + b)),
                Subtract => Ok(number(a - b)),
                Multiply => Ok(number(a * b)),
                Divide => Ok(number(a / b)),
                Equal => Ok(flag(a == b)),
                NotEqual => Ok(flag(a != b)),
                Less => Ok(flag(a < b)),
                Greater => Ok(flag(a > b)),
                LessEqual => Ok(flag(a <= b)),
                GreaterEqual => Ok(flag(a >= b)),
                _ => Err(DynamicFailure::Type),
            }
        }
        (FrameValue::Scalar(Scalar::Bool(a)), FrameValue::Scalar(Scalar::Bool(b))) => match op {
            And => Ok(flag(*a && *b)),
            Or => Ok(flag(*a || *b)),
            Equal => Ok(flag(a == b)),
            NotEqual => Ok(flag(a != b)),
            _ => Err(DynamicFailure::Type),
        },
        (FrameValue::Text(a), FrameValue::Text(b)) => match op {
            Equal => Ok(flag(a == b)),
            NotEqual => Ok(flag(a != b)),
            Less => Ok(flag(a < b)),
            Greater => Ok(flag(a > b)),
            LessEqual => Ok(flag(a <= b)),
            GreaterEqual => Ok(flag(a >= b)),
            _ => Err(DynamicFailure::Type),
        },
        (FrameValue::OpenObject(a), FrameValue::OpenObject(b)) => match op {
            Equal => Ok(flag(a == b)),
            NotEqual => Ok(flag(a != b)),
            _ => Err(DynamicFailure::Type),
        },
        _ => Err(DynamicFailure::Type),
    }
}

/// Whether a value may move into another task: plain data and channel ends.
fn transferable(value: &FrameValue) -> bool {
    match value {
        FrameValue::Scalar(_)
        | FrameValue::Text(_)
        | FrameValue::Sender(_)
        | FrameValue::Receiver(_)
        | FrameValue::Clock => true,
        FrameValue::Composite(Composite::Tuple(fields)) => fields.iter().all(transferable),
        FrameValue::Composite(Composite::Enum { payload, .. }) => {
            payload.as_deref().is_none_or(transferable)
        }
        FrameValue::Composite(Composite::Array(elements)) => {
            elements.iter().flatten().all(transferable)
        }
        FrameValue::Composite(Composite::Vec(items)) => items.iter().all(transferable),
        FrameValue::Composite(Composite::Cell(inner)) => transferable(inner),
        _ => false,
    }
}

/// What an activation leaves behind: module variables, the collected heap and
/// the counted objects, channels and tasks.
pub(crate) struct ActivationMemory(Memory);

/// An index operand as a signed integer: counts index too, and one too large
/// to be an integer is beyond every sequence.
fn scalar_index(scalar: Scalar) -> i64 {
    match scalar {
        Scalar::I64(index) => index,
        Scalar::U64(index) => i64::try_from(index).unwrap_or(i64::MAX),
        _ => unreachable!("verified index operand"),
    }
}

/// A value as a debugger shows it. Handles show what they point at, a few
/// levels deep, and long sequences are cut.
fn render_value(value: &FrameValue, memory: &Memory, depth: usize) -> String {
    const LIMIT: usize = 32;
    let again = |value: &FrameValue| render_value(value, memory, depth + 1);
    let list = |items: &mut dyn Iterator<Item = String>, total: usize| {
        let mut shown: Vec<String> = items.take(LIMIT).collect();
        if total > LIMIT {
            shown.push("…".into());
        }
        shown.join(", ")
    };
    match value {
        FrameValue::Scalar(Scalar::I64(value)) => value.to_string(),
        FrameValue::Scalar(Scalar::U64(value)) => value.to_string(),
        FrameValue::Scalar(Scalar::F64(bits)) => f64::from_bits(*bits).to_string(),
        FrameValue::Scalar(Scalar::Bool(value)) => value.to_string(),
        FrameValue::Text(text) => text.to_string(),
        FrameValue::Composite(Composite::Tuple(fields)) => {
            format!("({})", list(&mut fields.iter().map(again), fields.len()))
        }
        FrameValue::Composite(Composite::Enum { variant, payload }) => match payload {
            Some(payload) => format!("#{variant}({})", again(payload)),
            None => format!("#{variant}"),
        },
        FrameValue::Composite(Composite::Array(items)) => format!(
            "[{}]",
            list(
                &mut items
                    .iter()
                    .map(|item| item.as_ref().map_or("?".to_owned(), again)),
                items.len()
            )
        ),
        FrameValue::Composite(Composite::Vec(items)) => {
            format!("[{}]", list(&mut items.iter().map(again), items.len()))
        }
        FrameValue::Composite(Composite::Cell(inner)) => format!("cell({})", again(inner)),
        FrameValue::Handle(object) if depth < 4 => match memory.heap.slots.get(*object) {
            Some(Some(pointee)) => format!("gc({})", again(pointee)),
            _ => "gc(?)".to_owned(),
        },
        FrameValue::Handle(_) => "gc(…)".to_owned(),
        FrameValue::Rc(block) if depth < 4 => match memory.counted.get(*block) {
            Some(Some(counted)) => match &counted.payload {
                Some(payload) => format!("rc({})", again(payload)),
                None => "rc(released)".to_owned(),
            },
            _ => "rc(?)".to_owned(),
        },
        FrameValue::Rc(_) => "rc(…)".to_owned(),
        FrameValue::Dynamic(erased) | FrameValue::Any(erased) => again(&erased.1),
        FrameValue::Closure(_) => "<function>".to_owned(),
        FrameValue::OpenObject(_) => "<object>".to_owned(),
        FrameValue::Moved => "<moved>".to_owned(),
        _ => "<value>".to_owned(),
    }
}

/// The unit value: a tuple without fields.
fn unit() -> FrameValue {
    FrameValue::Composite(Composite::Tuple(Box::new([])))
}

/// A step of a generator: the yielded value in variant 0, the returned
/// value in variant 1.
fn step(variant: u32, value: FrameValue) -> FrameValue {
    FrameValue::Composite(Composite::Enum {
        variant,
        payload: Some(Box::new(value)),
    })
}

/// The value a storage path reaches from a root value.
fn apply_path<'a>(
    memory: &'a Memory,
    root: &'a FrameValue,
    steps: &[PathStep],
) -> Result<&'a FrameValue, PathFault> {
    let mut cursor = root;
    for step in steps {
        cursor = match (step, cursor) {
            (PathStep::Target, FrameValue::Handle(object)) => memory
                .heap
                .slots
                .get(*object)
                .and_then(Option::as_ref)
                .ok_or(PathFault::Mismatch)?,
            (PathStep::Target, FrameValue::Rc(block)) => memory
                .counted
                .get(*block)
                .and_then(Option::as_ref)
                .and_then(|block| block.payload.as_ref())
                .ok_or(PathFault::Mismatch)?,
            (PathStep::Target, FrameValue::Composite(Composite::Cell(inner))) => inner,
            (PathStep::Field(field), FrameValue::Composite(Composite::Tuple(fields))) => {
                fields.get(*field as usize).ok_or(PathFault::Mismatch)?
            }
            (PathStep::Element(index), FrameValue::Composite(Composite::Vec(items))) => {
                items.get(*index).ok_or(PathFault::OutOfBounds)?
            }
            (PathStep::Element(index), FrameValue::Composite(Composite::Array(elements))) => {
                elements
                    .get(*index)
                    .and_then(Option::as_ref)
                    .ok_or(PathFault::OutOfBounds)?
            }
            _ => return Err(PathFault::Mismatch),
        };
    }
    Ok(cursor)
}

/// Whether a value has the kind of a type. A storage path is checked when it
/// is applied, because it names its target by position only.
fn conforms(value: &FrameValue, ty: ValueType) -> bool {
    match (value, ty) {
        (FrameValue::Scalar(scalar), ValueType::Scalar(expected)) => scalar.ty() == expected,
        (FrameValue::Text(_), ValueType::Text)
        | (FrameValue::Composite(_), ValueType::Composite(_))
        | (FrameValue::Closure(_), ValueType::Function(_))
        | (FrameValue::Handle(_), ValueType::Gc(_))
        | (FrameValue::Rc(_), ValueType::Rc(_))
        | (FrameValue::Weak(_), ValueType::Weak(_))
        | (FrameValue::Object { .. }, ValueType::Object(_))
        | (FrameValue::Path(_), ValueType::Path)
        | (FrameValue::Location(_), ValueType::Location(_))
        | (FrameValue::Allocation(_), ValueType::Allocation)
        | (FrameValue::OpenObject(_), ValueType::OpenObject)
        | (FrameValue::Dynamic(_), ValueType::Dynamic) => true,
        _ => false,
    }
}

/// How a scalar or string is written in text.
fn spell(value: &FrameValue, style: TextStyle) -> String {
    match (style, value) {
        (_, FrameValue::Dynamic(erased)) => spell(&erased.1, style),
        (_, FrameValue::OpenObject(_)) => "object".to_owned(),
        (_, FrameValue::Text(text)) => text.to_string(),
        (_, FrameValue::Scalar(Scalar::I64(value))) => value.to_string(),
        (_, FrameValue::Scalar(Scalar::U64(value))) => value.to_string(),
        (_, FrameValue::Scalar(Scalar::F64(bits))) => f64::from_bits(*bits).to_string(),
        (TextStyle::Caie, FrameValue::Scalar(Scalar::Bool(value))) => {
            if *value { "TRUE" } else { "FALSE" }.to_owned()
        }
        (TextStyle::Plain, FrameValue::Scalar(Scalar::Bool(value))) => {
            if *value { "true" } else { "false" }.to_owned()
        }
        (
            _,
            FrameValue::Loan(_)
            | FrameValue::Composite(_)
            | FrameValue::Closure(_)
            | FrameValue::Handle(_)
            | FrameValue::Rc(_)
            | FrameValue::Weak(_)
            | FrameValue::Object { .. }
            | FrameValue::Path(_)
            | FrameValue::Location(_)
            | FrameValue::Allocation(_)
            | FrameValue::Generator(_)
            | FrameValue::Sender(_)
            | FrameValue::Receiver(_)
            | FrameValue::Task(_)
            | FrameValue::Clock
            | FrameValue::Properties(_)
            | FrameValue::Failure(_)
            | FrameValue::Any(_)
            | FrameValue::Context(_)
            | FrameValue::Registration(_)
            | FrameValue::ServiceKey(_)
            | FrameValue::ServiceRef(..)
            | FrameValue::NativeCallable(_)
            | FrameValue::Moved,
        ) => unreachable!("verified text operand is a scalar or string"),
    }
}

/// The plain spelling of a scalar or string, as `ToText` makes it.
pub(crate) fn spell_recovered(value: &RecoveredValue) -> Option<String> {
    match value {
        RecoveredValue::Scalar(scalar) => {
            Some(spell(&FrameValue::Scalar(*scalar), TextStyle::Plain))
        }
        RecoveredValue::Text(text) => Some(text.to_string()),
        _ => None,
    }
}

/// The line an output call prints for arguments recovered from native words.
pub(crate) fn render_recovered_output(
    template: &OutputTemplate,
    arguments: &[RecoveredValue],
) -> Option<String> {
    let values = arguments
        .iter()
        .map(|argument| match argument {
            RecoveredValue::Scalar(scalar) => Some(FrameValue::Scalar(*scalar)),
            RecoveredValue::Text(text) => Some(FrameValue::Text(text.clone())),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(render_output(template, &values))
}

fn render_output(template: &OutputTemplate, arguments: &[FrameValue]) -> String {
    let mut line = String::new();
    for piece in template.pieces.iter() {
        match piece {
            OutputPiece::Text(text) => line.push_str(text),
            OutputPiece::Argument(index) => {
                line.push_str(&spell(&arguments[*index], template.style))
            }
        }
    }
    line
}

impl Activation {
    /// Collect before every allocation, to expose a handle that is not
    /// reachable from a root. Set before the first slice.
    pub fn set_gc_stress(&mut self, stress: bool) {
        self.memory.heap.stress = stress;
    }

    /// Heap occupancy so far.
    pub fn heap_stats(&self) -> HeapStats {
        HeapStats {
            live: self.memory.heap.live,
            peak: self.memory.heap.peak,
            collections: self.memory.heap.collections,
            counted: self.memory.counted.iter().flatten().count(),
        }
    }

    /// Take the memory out of a finished or abandoned activation, to continue
    /// with it in the next one.
    pub(crate) fn into_memory(self) -> ActivationMemory {
        ActivationMemory(self.memory)
    }

    /// The state the pseudo-random generator starts from.
    pub fn set_random_seed(&mut self, seed: u64) {
        self.memory.random_seed = seed;
    }

    /// Stop waiting for input: an input instruction that finds no line fails
    /// with the end of input.
    pub fn end_input(&mut self) {
        self.input.waits = false;
    }

    /// Whether the last slice stopped on an input instruction with no line.
    pub fn awaiting_input(&self) -> bool {
        self.awaiting_input
    }

    /// Replace the origin of input lines. Set before the first slice.
    pub fn set_input(&mut self, source: InputSource) {
        self.input = source;
    }

    /// Replace the destination of output lines. Set before the first slice.
    pub fn set_output(&mut self, sink: OutputSink) {
        self.output = sink;
    }

    /// Instructions started across all slices, including a failing instruction.
    /// The routines of the running task's frames, outermost first.
    pub fn call_stack(&self) -> Vec<FunctionId> {
        match &self.state {
            ExecutionState::Active(stack) => stack.iter().map(|frame| frame.function).collect(),
            _ => Vec::new(),
        }
    }

    /// The source variables of the innermost frame that hold a value now, with
    /// the module variables when that frame is not the entry's.
    pub fn frame_variables(&self) -> Vec<(String, String)> {
        let ExecutionState::Active(stack) = &self.state else {
            return Vec::new();
        };
        let Some(frame) = stack.last() else {
            return Vec::new();
        };
        let body = &self.module.functions[frame.function.index()];
        let layout = body.frame_layout();
        let live = body.initialized_at(instruction_id(frame.pc));
        let mut variables: Vec<(String, String)> = body
            .debug_locals()
            .iter()
            .map(|local| {
                // A variable with no value yet, or none any more, shows as such.
                let value = match local.place {
                    DebugPlace::Register(register) if live.contains(&register) => {
                        frame.slots[slot(layout, register)].as_ref()
                    }
                    DebugPlace::Module(global) => self.memory.globals[global.index()].as_ref(),
                    DebugPlace::Register(_) => None,
                };
                let shown = value.map_or_else(
                    || "uninitialized".to_owned(),
                    |value| render_value(value, &self.memory, 0),
                );
                (local.name.to_string(), shown)
            })
            .collect();
        if frame.function != self.module.entry {
            variables.extend(
                self.module_variables()
                    .into_iter()
                    .map(|(name, value)| (format!("global::{name}"), value)),
            );
        }
        variables
    }

    /// The variables of the entry routine that hold a value, by their source
    /// names: the module-level ones, and the ones its frame keeps in registers
    /// while it is still running.
    pub fn module_variables(&self) -> Vec<(String, String)> {
        let entry = &self.module.functions[self.module.entry.index()];
        let frame = match &self.state {
            ExecutionState::Active(stack) => {
                stack.first().filter(|f| f.function == self.module.entry)
            }
            _ => None,
        };
        let live = frame.map(|frame| entry.initialized_at(instruction_id(frame.pc)));
        entry
            .debug_locals()
            .iter()
            .filter_map(|local| {
                let value = match local.place {
                    DebugPlace::Module(global) => {
                        self.memory.globals.get(global.index())?.as_ref()?
                    }
                    DebugPlace::Register(register) => {
                        let (frame, live) = (frame?, live.as_ref()?);
                        if !live.contains(&register) {
                            return None;
                        }
                        frame.slots[slot(entry.frame_layout(), register)].as_ref()?
                    }
                };
                Some((local.name.to_string(), render_value(value, &self.memory, 0)))
            })
            .collect()
    }

    /// Frames on the running task's stack.
    pub fn depth(&self) -> usize {
        match &self.state {
            ExecutionState::Active(stack) => stack.len(),
            _ => 0,
        }
    }

    pub fn instructions_executed(&self) -> usize {
        self.instructions_executed
    }

    /// Source statements reached at this pause, in source order on the taken edge.
    /// Reading debugger metadata spends no fuel.
    pub fn statement_boundaries(&self) -> impl Iterator<Item = &StatementBoundary> {
        let frame = match &self.state {
            ExecutionState::Active(stack) => stack.last(),
            ExecutionState::Returned(_) | ExecutionState::Failed(_) => None,
        };
        frame.into_iter().flat_map(|frame| {
            self.module.functions[frame.function.index()]
                .statement_boundaries(instruction_id(frame.pc), frame.arrival)
        })
    }

    /// Inspect the next instruction or terminal outcome without executing.
    pub fn status(&self) -> ExecutionStatus {
        match &self.state {
            ExecutionState::Active(stack) => {
                let Some(frame) = stack.last() else {
                    // An entry return transitions out of Active in the same step.
                    unreachable!("active execution has no frame");
                };
                let body = &self.module.functions[frame.function.index()];
                ExecutionStatus::Paused(ExecutionPosition {
                    function: frame.function,
                    instruction: instruction_id(frame.pc),
                    span: body.spans[frame.pc],
                })
            }
            ExecutionState::Returned(value) => ExecutionStatus::Returned(*value),
            ExecutionState::Failed(error) => ExecutionStatus::Failed(*error),
        }
    }

    /// Call a native function through an erased value: compile an adapter for
    /// the argument types once, add it to the running code, and run it with the
    /// arguments. The erased result lands in the register.
    #[allow(clippy::too_many_arguments)]
    fn perform_native_call(
        module: &mut VerifiedModule,
        memory: &mut Memory,
        stack: &mut Vec<Frame>,
        frames: usize,
        destination: Register,
        declaration: u32,
        arguments: Vec<(ValueType, FrameValue)>,
        position: (FunctionId, InstructionId, Span),
    ) -> Result<(), RunError> {
        let (function, instruction, span) = position;
        let refuse = |fault| RunError::Object {
            function,
            instruction,
            span,
            fault,
        };
        let types: Vec<ValueType> = arguments.iter().map(|(ty, _)| *ty).collect();
        let key = (declaration, types.clone());
        if !memory.adapters.contains_key(&key) {
            // Only plain data has a checked type to compile an adapter for.
            let mut checked = Vec::with_capacity(types.len());
            for ty in &types {
                checked.push(match ty {
                    ValueType::Scalar(ScalarType::I64) => cyallo_ast::Type::Integer,
                    ValueType::Scalar(ScalarType::F64) => cyallo_ast::Type::Real,
                    ValueType::Scalar(ScalarType::Bool) => cyallo_ast::Type::Boolean,
                    ValueType::Text => cyallo_ast::Type::String,
                    _ => return Err(refuse(ObjectFault::Type)),
                });
            }
            let Some(program) = module.source.clone() else {
                return Err(refuse(ObjectFault::Type));
            };
            let request = crate::dynamic_modules::CallRequest {
                target: cyallo_ir::dynamic::CallableTarget::Native(cyallo_ast::DeclarationId(
                    declaration,
                )),
                arguments: checked,
                expected: None,
                span,
            };
            let compiled = crate::dynamic_modules::specialize_calls(program, vec![request])
                .map_err(|_| refuse(ObjectFault::Type))?;
            let Some(adapter) = compiled.adapters.first() else {
                return Err(refuse(ObjectFault::Type));
            };
            let routine = adapter.function;
            let mut roots: Vec<usize> = module.roots.to_vec();
            if !roots.contains(&routine) {
                roots.push(routine);
            }
            let next =
                VerifiedModule::lower_extended(compiled.program, module.entry.index(), &roots)
                    .map_err(|_| refuse(ObjectFault::Type))?;
            module
                .is_extended_by(&next)
                .map_err(|_| refuse(ObjectFault::Type))?;
            memory.globals.resize(next.globals.len(), None);
            *module = next;
            memory.adapters.insert(key.clone(), routine);
        }
        let routine = memory.adapters[&key];
        let Some(id) = module.lowered(routine) else {
            return Err(refuse(ObjectFault::Type));
        };
        if stack.len() >= frames {
            return Err(RunError::FrameLimit);
        }
        let target = &module.functions[id.index()];
        let result = target.result;
        let values = arguments.into_iter().map(|(_, value)| value);
        let mut next = Frame::new(id, target, values, Some(destination));
        next.wrap = Some(result);
        stack.push(next);
        Ok(())
    }

    /// Load the module at the path, extend the running code with it, and run
    /// what builds its namespace; the register receives that, or why not.
    fn perform_load(
        module: &mut VerifiedModule,
        memory: &mut Memory,
        stack: &mut Vec<Frame>,
        frames: usize,
        destination: Register,
        path: &str,
        span: Span,
    ) -> Result<(), RunError> {
        let top = stack.len() - 1;
        let function = stack[top].function.index();
        let deliver = |module: &VerifiedModule,
                       stack: &mut Vec<Frame>,
                       result: Result<FrameValue, Arc<FailureData>>| {
            let body = &module.functions[function];
            let (ok, err) = body.failure_result_runtime(destination);
            let composite = match result {
                Ok(value) => Composite::Enum {
                    variant: ok,
                    payload: Some(Box::new(value)),
                },
                Err(failure) => Composite::Enum {
                    variant: err,
                    payload: Some(Box::new(FrameValue::Failure(failure))),
                },
            };
            stack[top].set_value(
                body.frame_layout(),
                destination,
                FrameValue::Composite(composite),
            );
        };
        let failed = |error: &cyallo_modules::LoadError| {
            Arc::new(FailureData {
                message: error.to_string().into(),
                not_found: error.is_not_found(),
                denied: error.is_denied(),
                check: error.is_check_error(),
            })
        };
        let Some(program) = module.source.clone() else {
            deliver(
                module,
                stack,
                Err(FailureData::plain("this program cannot load modules")),
            );
            return Ok(());
        };
        let Some(loader) = program.module_loader.clone() else {
            deliver(
                module,
                stack,
                Err(failed(&cyallo_modules::LoadError::NoReadGrant)),
            );
            return Ok(());
        };
        let Some(plan) = program.dynamic_plan.as_ref() else {
            deliver(
                module,
                stack,
                Err(FailureData::plain("the program has no compile plan")),
            );
            return Ok(());
        };
        let request = match plan.request(span, path) {
            Ok(request) => request,
            Err(error) => {
                deliver(module, stack, Err(failed(&error)));
                return Ok(());
            }
        };
        let imported = match crate::dynamic_modules::load(program.clone(), loader, request) {
            Ok(imported) => imported,
            Err(error) => {
                deliver(module, stack, Err(failed(&error)));
                return Ok(());
            }
        };
        // The builder and the initializers are reached by nothing else, so each
        // load lowers them as extra roots, after every earlier root.
        let mut roots: Vec<usize> = module.roots.to_vec();
        for routine in
            std::iter::once(imported.factory).chain(imported.initializers.iter().copied())
        {
            if !roots.contains(&routine) {
                roots.push(routine);
            }
        }
        if let Some(next_program) = imported.program {
            let next =
                match VerifiedModule::lower_extended(next_program, module.entry.index(), &roots) {
                    Ok(next) => next,
                    Err(error) => {
                        let why = FailureData::plain(&error.to_string());
                        deliver(module, stack, Err(why));
                        return Ok(());
                    }
                };
            if let Err(reason) = module.is_extended_by(&next) {
                let why = FailureData::plain(&format!(
                    "the loaded module would change running code: {reason}"
                ));
                deliver(module, stack, Err(why));
                return Ok(());
            }
            memory.globals.resize(next.globals.len(), None);
            *module = next;
        }
        if let Some(cached) = memory.namespaces.get(&imported.key).cloned() {
            let copy = memory.clone_value(&cached);
            deliver(module, stack, Ok(copy));
            return Ok(());
        }
        let Some(builder) = module.lowered(imported.factory) else {
            let why = FailureData::plain("the namespace builder was not lowered");
            deliver(module, stack, Err(why));
            return Ok(());
        };
        let mut pushed = vec![];
        let target = &module.functions[builder.index()];
        let mut factory = Frame::new(builder, target, std::iter::empty(), Some(destination));
        factory.namespace = Some(imported.key);
        pushed.push(factory);
        for initializer in imported.initializers.into_iter().rev() {
            if memory.initialized.insert(initializer) {
                let Some(id) = module.lowered(initializer) else {
                    continue;
                };
                let target = &module.functions[id.index()];
                pushed.push(Frame::new(id, target, std::iter::empty(), None));
            }
        }
        if stack.len() + pushed.len() > frames {
            return Err(RunError::FrameLimit);
        }
        stack.extend(pushed);
        Ok(())
    }

    /// Advance by at most `budget` instructions. A zero budget only reports the
    /// current status. Terminal outcomes are retained across subsequent calls.
    pub fn resume(&mut self, budget: usize) -> ExecutionStatus {
        let ExecutionState::Active(stack) = &mut self.state else {
            return self.status();
        };
        self.awaiting_input = false;
        let available = self.limits.fuel - self.instructions_executed;
        for _ in 0..budget.min(available) {
            self.instructions_executed += 1;
            let outcome = self.module.step(
                stack,
                &mut self.memory,
                self.limits.frames,
                &mut self.output,
                &mut self.input,
            );
            let entry = self.memory.current_task == 0;
            // A module load changes the code under the running frames, so it
            // happens here, between instructions.
            let outcome = match outcome {
                Ok(StepOutcome::NativeCall {
                    destination,
                    declaration,
                    arguments,
                    position,
                }) => Self::perform_native_call(
                    &mut self.module,
                    &mut self.memory,
                    stack,
                    self.limits.frames,
                    destination,
                    declaration,
                    arguments,
                    position,
                )
                .map(|()| StepOutcome::Continue),
                Ok(StepOutcome::Load {
                    destination,
                    path,
                    span,
                }) => Self::perform_load(
                    &mut self.module,
                    &mut self.memory,
                    stack,
                    self.limits.frames,
                    destination,
                    &path,
                    span,
                )
                .map(|()| StepOutcome::Continue),
                other => other,
            };
            match outcome {
                Ok(StepOutcome::Continue) => {
                    self.stalled = 0;
                    self.quantum -= 1;
                    if self.quantum == 0 {
                        self.quantum = QUANTUM;
                        self.module.rotate(stack, &mut self.memory, false);
                    }
                }
                Ok(StepOutcome::Blocked) => {
                    self.stalled += 1;
                    if self.stalled > self.memory.live_tasks() {
                        // Every live task was tried and none could go on.
                        if let Some(wake) = self.memory.earliest_wake() {
                            std::thread::sleep(wake.saturating_duration_since(Instant::now()));
                            self.stalled = 0;
                        } else {
                            let Some(frame) = stack.last() else {
                                unreachable!("active execution has a frame");
                            };
                            let body = &self.module.functions[frame.function.index()];
                            self.state = ExecutionState::Failed(RunError::Deadlock {
                                function: frame.function,
                                instruction: instruction_id(frame.pc),
                                span: body.spans[frame.pc],
                            });
                            return self.status();
                        }
                    } else {
                        self.module.rotate(stack, &mut self.memory, false);
                    }
                }
                Ok(StepOutcome::AwaitInput) => {
                    // Nothing ran: give the instruction back to the budget.
                    self.instructions_executed -= 1;
                    self.awaiting_input = true;
                    return self.status();
                }
                Ok(StepOutcome::Load { .. } | StepOutcome::NativeCall { .. }) => {
                    unreachable!("a load or call was just performed")
                }
                Ok(StepOutcome::Returned(value)) if entry => {
                    let scalar = value.map(|value| match value {
                        FrameValue::Scalar(value) => value,
                        _ => unreachable!("verified entry result is scalar"),
                    });
                    self.state = ExecutionState::Returned(scalar);
                    return self.status();
                }
                Err(error) if entry => {
                    self.state = ExecutionState::Failed(error);
                    return self.status();
                }
                Ok(StepOutcome::Returned(value)) => {
                    self.module
                        .end_task(stack, &mut self.memory, TaskEnd::Completed(value));
                    self.stalled = 0;
                }
                Err(_) => {
                    self.module
                        .end_task(stack, &mut self.memory, TaskEnd::Failed);
                    self.stalled = 0;
                }
            }
        }
        if self.instructions_executed == self.limits.fuel {
            self.state = ExecutionState::Failed(RunError::FuelExhausted);
        }
        self.status()
    }
}

fn place_error(
    fault: PlaceFault,
    function: FunctionId,
    instruction: InstructionId,
    span: Span,
) -> RunError {
    match fault {
        PlaceFault::OtherVariant => RunError::EnumVariant {
            function,
            instruction,
            span,
        },
        PlaceFault::MovedOut => RunError::MovedValue {
            function,
            instruction,
            span,
        },
        PlaceFault::UninitializedElement => RunError::UninitializedElement {
            function,
            instruction,
            span,
        },
    }
}

/// The place a register stands for: its loan's place, or the register itself.
fn place_of(
    body: &VerifiedFunction,
    frame: &Frame,
    layout: &FrameLayout,
    depth: usize,
    source: Register,
) -> BorrowedPlace {
    if matches!(body.registers()[source.index()], RegisterType::Loan { .. }) {
        frame.loan(layout, source)
    } else {
        BorrowedPlace {
            root: Root::Frame {
                frame: depth - 1,
                register: source,
            },
            path: Box::new([]),
            window: None,
        }
    }
}

/// The borrowed place of a projection: the base loan's place, or the owning
/// register itself, extended by one step.
fn project_base(
    body: &VerifiedFunction,
    frame: &Frame,
    layout: &FrameLayout,
    depth: usize,
    source: Register,
    step: Step,
) -> BorrowedPlace {
    let mut place = if matches!(body.registers()[source.index()], RegisterType::Loan { .. }) {
        frame.loan(layout, source)
    } else {
        BorrowedPlace {
            root: Root::Frame {
                frame: depth - 1,
                register: source,
            },
            path: Box::new([]),
            window: None,
        }
    };
    let mut path = place.path.into_vec();
    path.push(step);
    place.path = path.into_boxed_slice();
    place.window = None;
    place
}

fn text_error(
    failure: TextFailure,
    function: FunctionId,
    instruction: InstructionId,
    span: Span,
) -> RunError {
    RunError::Text {
        failure,
        function,
        instruction,
        span,
    }
}

pub(crate) fn slice_left(text: &str, count: i64) -> Result<String, TextFailure> {
    let count = usize::try_from(count).map_err(|_| TextFailure::NegativeLength)?;
    let chars: Vec<char> = text.chars().collect();
    chars
        .get(..count)
        .map(|part| part.iter().collect())
        .ok_or(TextFailure::RangeOutOfBounds)
}

pub(crate) fn slice_right(text: &str, count: i64) -> Result<String, TextFailure> {
    let count = usize::try_from(count).map_err(|_| TextFailure::NegativeLength)?;
    let chars: Vec<char> = text.chars().collect();
    let start = chars
        .len()
        .checked_sub(count)
        .ok_or(TextFailure::RangeOutOfBounds)?;
    Ok(chars[start..].iter().collect())
}

/// `start` is one-based, as in the CAIE `MID`.
pub(crate) fn slice_mid(text: &str, start: i64, count: i64) -> Result<String, TextFailure> {
    let start = start
        .checked_sub(1)
        .and_then(|start| usize::try_from(start).ok())
        .ok_or(TextFailure::IndexOutOfBounds)?;
    let count = usize::try_from(count).map_err(|_| TextFailure::NegativeLength)?;
    let end = start
        .checked_add(count)
        .ok_or(TextFailure::RangeOutOfBounds)?;
    let chars: Vec<char> = text.chars().collect();
    chars
        .get(start..end)
        .map(|part| part.iter().collect())
        .ok_or(TextFailure::RangeOutOfBounds)
}

/// Row-major element offset, or `None` when an index leaves its bounds.
fn element_offset(
    frame: &Frame,
    layout: &FrameLayout,
    bounds: &[(i64, i64)],
    list: super::OperandListId,
    body: &VerifiedFunction,
) -> Option<usize> {
    let mut offset = 0usize;
    for (&(lower, upper), &index) in bounds.iter().zip(body.lists[list.index()].iter()) {
        let index = scalar_index(frame.get(layout, index));
        if index < lower || index > upper {
            return None;
        }
        let extent = usize::try_from(upper - lower + 1).ok()?;
        let position = usize::try_from(index - lower).ok()?;
        offset = offset.checked_mul(extent)?.checked_add(position)?;
    }
    Some(offset)
}

/// `None` is a checked division by zero. Everything else is IEEE-754.
fn evaluate_float(op: FloatOp, lhs: f64, rhs: f64) -> Option<Scalar> {
    Some(match op {
        FloatOp::Add => Scalar::from_f64(lhs + rhs),
        FloatOp::Subtract => Scalar::from_f64(lhs - rhs),
        FloatOp::Multiply => Scalar::from_f64(lhs * rhs),
        FloatOp::Divide => Scalar::from_f64(lhs / rhs),
        FloatOp::CheckedDivide if rhs == 0.0 => return None,
        FloatOp::CheckedDivide => Scalar::from_f64(lhs / rhs),
        FloatOp::Remainder => Scalar::from_f64(lhs % rhs),
        FloatOp::Less => Scalar::Bool(lhs < rhs),
        FloatOp::LessEqual => Scalar::Bool(lhs <= rhs),
        FloatOp::Greater => Scalar::Bool(lhs > rhs),
        FloatOp::GreaterEqual => Scalar::Bool(lhs >= rhs),
        FloatOp::Equal => Scalar::Bool(lhs == rhs),
        FloatOp::NotEqual => Scalar::Bool(lhs != rhs),
    })
}

/// Truncation toward zero. The upper bound is exclusive because 2^63 is not
/// an `i64`, and rounding it down would admit it.
fn float_to_integer(value: f64) -> Option<i64> {
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let truncated = value.trunc();
    (-LIMIT..LIMIT)
        .contains(&truncated)
        .then_some(truncated as i64)
}

fn evaluate(op: IntegerOp, lhs: i64, rhs: i64) -> Result<Scalar, IntegerError> {
    let checked = |result: Option<i64>| result.ok_or(IntegerError::Overflow);
    Ok(match op {
        IntegerOp::Add => Scalar::I64(checked(lhs.checked_add(rhs))?),
        IntegerOp::Subtract => Scalar::I64(checked(lhs.checked_sub(rhs))?),
        IntegerOp::Multiply => Scalar::I64(checked(lhs.checked_mul(rhs))?),
        IntegerOp::Divide => Scalar::I64(IntegerDivision::Quotient.evaluate(lhs, rhs)?),
        IntegerOp::Remainder => Scalar::I64(IntegerDivision::Remainder.evaluate(lhs, rhs)?),
        IntegerOp::Less => Scalar::Bool(lhs < rhs),
        IntegerOp::LessEqual => Scalar::Bool(lhs <= rhs),
        IntegerOp::Greater => Scalar::Bool(lhs > rhs),
        IntegerOp::GreaterEqual => Scalar::Bool(lhs >= rhs),
        IntegerOp::Equal => Scalar::Bool(lhs == rhs),
        IntegerOp::NotEqual => Scalar::Bool(lhs != rhs),
    })
}
