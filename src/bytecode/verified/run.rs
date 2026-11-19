use super::{
    BooleanOp, CallSite, CallTarget, FloatOp, FrameLayout, FunctionId, GlobalId, HostBindingId,
    HostFailure, Instruction, InstructionId, IntegerOp, OutputPiece, OutputTemplate, Register,
    RegisterType, Scalar, ScalarType, StatementArrival, StatementBoundary, TextFailure, TextOp,
    TextStyle, VerifiedFunction, VerifiedModule, array_len,
};
use crate::{
    Span,
    integer::{IntegerDivision, IntegerError},
};
use std::{
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
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
    EnumVariant {
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
}

/// Frames above this index may move as the stack grows. The owning logical
/// frame and its verified register remain stable until borrowers return.
#[derive(Debug, Clone)]
struct BorrowedPlace {
    root: Root,
    /// Projections from the owning register to the borrowed place.
    path: Box<[Step]>,
}

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
}

impl Memory {
    fn new(globals: usize) -> Self {
        Self {
            globals: vec![None; globals],
            heap: Heap::new(),
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
            FrameValue::Handle(object) => mark(*object),
            FrameValue::Loan(place) => {
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
            FrameValue::Closure(closure) => stack.extend(closure.captures.iter()),
            FrameValue::Scalar(_) | FrameValue::Text(_) => {}
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
}

fn read_place(value: &FrameValue, path: &[Step]) -> Result<FrameValue, PlaceFault> {
    let Some((step, rest)) = path.split_first() else {
        return Ok(value.clone());
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
            | FrameValue::Handle(_) => {
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
            | FrameValue::Handle(_) => {
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
    memory: Memory,
    limits: ExecutionLimits,
    instructions_executed: usize,
    state: ExecutionState,
}

pub(crate) struct ScalarFrameSnapshot {
    pub function: FunctionId,
    pub next: InstructionId,
    pub arrival: StatementArrival,
    pub destination: Option<Register>,
    pub values: Vec<(Register, Scalar)>,
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
    Returned(Option<Scalar>),
}

impl VerifiedModule {
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

            let mut values = vec![None; state.len()];
            for &(register, value) in &snapshot.values {
                let error = ScalarRecoveryError::InvalidValue {
                    function: snapshot.function,
                    register,
                };
                let slot = values.get_mut(register.index()).ok_or(error)?;
                if state[register.index()] != StorageState::Initialized
                    || body.register_type(register) != Ok(value.ty())
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
                    frame.set(body.frame_layout(), register, value);
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
            memory: Memory::new(self.globals.len()),
            limits,
            instructions_executed: 0,
            state: ExecutionState::Active(vec![entry]),
        })
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
                };
                frame.set_value(layout, loan, FrameValue::Loan(place));
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
                let FrameValue::Handle(object) = frame.value(layout, handle) else {
                    unreachable!("verified handle operand");
                };
                let place = BorrowedPlace {
                    root: Root::Heap(object),
                    path: Box::new([]),
                };
                frame.set_value(layout, dst, FrameValue::Loan(place));
            }
            Instruction::BorrowGlobal(dst, global) => {
                let place = BorrowedPlace {
                    root: Root::Global(global),
                    path: Box::new([]),
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
                    CallTarget::Closure(callee) => {
                        if depth == max_frames {
                            return Err(RunError::FrameLimit);
                        }
                        let FrameValue::Closure(closure) = frame.value(layout, callee) else {
                            unreachable!("verified closure operand");
                        };
                        let target = &self.functions[closure.function.index()];
                        let values = closure
                            .captures
                            .iter()
                            .cloned()
                            .chain(call.arguments.iter().map(|&reg| frame.value(layout, reg)));
                        let next = Frame::new(closure.function, target, values, call.destination);
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
            Instruction::Return(value) => {
                let value = value.map(|reg| frame.value(layout, reg));
                let destination = frame.destination;
                stack.pop();
                let Some(caller) = stack.last_mut() else {
                    return Ok(StepOutcome::Returned(value.map(|value| match value {
                        FrameValue::Scalar(value) => value,
                        _ => unreachable!("verified entry result is scalar"),
                    })));
                };
                if let Some((dst, value)) = destination.zip(value) {
                    let layout = self.functions[caller.function.index()].frame_layout();
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
        Ok(StepOutcome::Continue)
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

    fn write_line(&mut self, line: &str) {
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

fn render_output(template: &OutputTemplate, arguments: &[FrameValue]) -> String {
    let mut line = String::new();
    for piece in template.pieces.iter() {
        match piece {
            OutputPiece::Text(text) => line.push_str(text),
            OutputPiece::Argument(index) => match (template.style, &arguments[*index]) {
                (_, FrameValue::Text(text)) => line.push_str(text),
                (_, FrameValue::Scalar(Scalar::I64(value))) => line.push_str(&value.to_string()),
                (_, FrameValue::Scalar(Scalar::F64(bits))) => {
                    line.push_str(&f64::from_bits(*bits).to_string());
                }
                (TextStyle::Caie, FrameValue::Scalar(Scalar::Bool(value))) => {
                    line.push_str(if *value { "TRUE" } else { "FALSE" });
                }
                (TextStyle::Plain, FrameValue::Scalar(Scalar::Bool(value))) => {
                    line.push_str(if *value { "true" } else { "false" });
                }
                (
                    _,
                    FrameValue::Loan(_)
                    | FrameValue::Composite(_)
                    | FrameValue::Closure(_)
                    | FrameValue::Handle(_),
                ) => {
                    unreachable!("verified output argument is a scalar or text")
                }
            },
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
        }
    }

    /// Replace the destination of output lines. Set before the first slice.
    pub fn set_output(&mut self, sink: OutputSink) {
        self.output = sink;
    }

    /// Instructions started across all slices, including a failing instruction.
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

    /// Advance by at most `budget` instructions. A zero budget only reports the
    /// current status. Terminal outcomes are retained across subsequent calls.
    pub fn resume(&mut self, budget: usize) -> ExecutionStatus {
        let ExecutionState::Active(stack) = &mut self.state else {
            return self.status();
        };
        let available = self.limits.fuel - self.instructions_executed;
        for _ in 0..budget.min(available) {
            self.instructions_executed += 1;
            self.state = match self.module.step(
                stack,
                &mut self.memory,
                self.limits.frames,
                &mut self.output,
            ) {
                Ok(StepOutcome::Continue) => continue,
                Ok(StepOutcome::Returned(value)) => ExecutionState::Returned(value),
                Err(error) => ExecutionState::Failed(error),
            };
            return self.status();
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
        PlaceFault::UninitializedElement => RunError::UninitializedElement {
            function,
            instruction,
            span,
        },
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
        }
    };
    let mut path = place.path.into_vec();
    path.push(step);
    place.path = path.into_boxed_slice();
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

fn slice_left(text: &str, count: i64) -> Result<String, TextFailure> {
    let count = usize::try_from(count).map_err(|_| TextFailure::NegativeLength)?;
    let chars: Vec<char> = text.chars().collect();
    chars
        .get(..count)
        .map(|part| part.iter().collect())
        .ok_or(TextFailure::RangeOutOfBounds)
}

fn slice_right(text: &str, count: i64) -> Result<String, TextFailure> {
    let count = usize::try_from(count).map_err(|_| TextFailure::NegativeLength)?;
    let chars: Vec<char> = text.chars().collect();
    let start = chars
        .len()
        .checked_sub(count)
        .ok_or(TextFailure::RangeOutOfBounds)?;
    Ok(chars[start..].iter().collect())
}

/// `start` is one-based, as in the CAIE `MID`.
fn slice_mid(text: &str, start: i64, count: i64) -> Result<String, TextFailure> {
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
        let Scalar::I64(index) = frame.get(layout, index) else {
            unreachable!("verified integer index");
        };
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
