//! Fuel-bounded native execution and transfer to the verified interpreter.

use super::SliceKind;
use super::{BridgeCall, Layout, LoanBase};
use super::{
    CodeStore, EntryPin, Failure, NativeHome, NativeTier, Operation, Origin, RecoverySlot,
    ScalarOutcome, Site,
    abi::{Edge, ScalarContext, VECTOR_HEADER_BYTES, VECTOR_HEADER_WORDS, scalar_word},
    platform::Invocation,
};
use crate::bytecode::verified::{
    Activation, ExecutionLimits, ExecutionPosition, ExecutionStatus, FunctionId, HeapStats,
    InputSource, InstructionId, MAX_VEC_ELEMENTS, RecoveredValue, Register, RunError, Scalar,
    ScalarFrameSnapshot, ScalarRecoveryError, ScalarType, StatementArrival,
};
use crate::bytecode::verified::{OutputSink, TextFailure, TextMapOp, TextOp, TextTestOp};
use std::sync::Arc;
use std::{collections::HashMap, fmt};

const ENTRY: InstructionId = InstructionId(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    PreferNative,
    InterpreterOnly,
}

#[derive(Debug)]
pub enum Dispatch {
    Native(NativeActivation),
    Interpreter(Box<Activation>),
}

impl CodeStore {
    pub fn start(
        &self,
        arguments: &[Scalar],
        limits: ExecutionLimits,
        mode: ExecutionMode,
    ) -> Result<Dispatch, NativeRunError> {
        if mode == ExecutionMode::PreferNative
            && let Some(entry) = self.entry(self.module().entry())
        {
            return entry.start(arguments, limits).map(Dispatch::Native);
        }
        self.module()
            .start(arguments, limits)
            .map(|activation| Dispatch::Interpreter(Box::new(activation)))
            .map_err(NativeRunError::Guest)
    }
}

impl Dispatch {
    pub fn resume(&mut self, budget: usize) -> Result<ExecutionStatus, NativeRunError> {
        match self {
            Self::Native(activation) => activation.resume(budget),
            Self::Interpreter(activation) => Ok(activation.resume(budget)),
        }
    }
}

impl EntryPin {
    pub fn start(
        &self,
        arguments: &[Scalar],
        limits: ExecutionLimits,
    ) -> Result<NativeActivation, NativeRunError> {
        if self.plan().uses_shared_runtime() {
            let activation = self
                .module()
                .start_at(self.plan().function(), arguments, limits)
                .map_err(NativeRunError::Guest)?;
            return Ok(NativeActivation::new(
                State::Shared(Box::new(SharedActivation {
                    activation,
                    entry: self.clone(),
                    entries: self.runtime_snapshot(),
                    slots: Vec::new(),
                })),
                limits,
            ));
        }
        if self.plan().result_layout().is_some() {
            return Err(NativeRunError::Guest(RunError::EntryResultNotScalar));
        }
        let parameters = self.plan().arguments();
        if let Some(index) = parameters.iter().position(|home| home.layout.is_some()) {
            return Err(NativeRunError::Guest(RunError::EntryParameterNotScalar {
                index,
            }));
        }
        if parameters.len() != arguments.len() {
            return Err(NativeRunError::Guest(RunError::ArgumentCount {
                expected: parameters.len(),
                actual: arguments.len(),
            }));
        }
        for (index, (home, &argument)) in parameters.iter().zip(arguments).enumerate() {
            let actual = argument.ty();
            if home.ty != actual {
                return Err(NativeRunError::Guest(RunError::ArgumentType {
                    index,
                    expected: home.ty,
                    actual,
                }));
            }
        }
        if limits.frames == 0 {
            return Err(NativeRunError::Guest(RunError::FrameLimit));
        }
        if limits.fuel == 0 {
            return Err(NativeRunError::Guest(RunError::FuelExhausted));
        }
        let mut frame = Frame::new(self.clone(), None)?;
        for (&home, &argument) in parameters.iter().zip(arguments) {
            frame.store(home, argument)?;
        }
        Ok(NativeActivation::new(
            State::Active(Box::new(WordActivation {
                stack: vec![frame],
                heap: Heap::default(),
                output: OutputSink::default(),
                input: InputSource::default(),
                gc_stress: false,
                random_seed: None,
            })),
            limits,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryEvent {
    pub origin: Origin,
    pub tier: NativeTier,
}

#[derive(Debug)]
pub struct NativeActivation {
    state: State,
    limits: ExecutionLimits,
    spent: usize,
    last_entry: Option<EntryEvent>,
}

#[derive(Debug)]
enum State {
    Active(Box<WordActivation>),
    Shared(Box<SharedActivation>),
    Terminal(ExecutionStatus),
    Broken(NativeRunError),
}

#[derive(Debug)]
struct WordActivation {
    stack: Vec<Frame>,
    /// Strings and collection buffers held by native frames.
    heap: Heap,
    output: OutputSink,
    input: InputSource,
    gc_stress: bool,
    random_seed: Option<u64>,
}

#[derive(Debug)]
struct Frame {
    entry: EntryPin,
    slots: Box<[u64]>,
    position: Position,
    destination: Option<NativeHome>,
}

#[derive(Debug)]
enum Position {
    Ready {
        next: InstructionId,
        arrival: StatementArrival,
    },
    Waiting {
        call: InstructionId,
        successor: InstructionId,
    },
}

impl Frame {
    fn new(entry: EntryPin, destination: Option<NativeHome>) -> Result<Self, NativeRunError> {
        let plan = entry.plan();
        let bytes = usize::try_from(plan.frame_bytes()).map_err(|_| NativeRunError::WordWidth)?;
        Ok(Self {
            entry,
            slots: vec![0; bytes / size_of::<u64>()].into_boxed_slice(),
            position: Position::Ready {
                next: ENTRY,
                arrival: StatementArrival::Entry,
            },
            destination,
        })
    }

    fn slot(&self, home: NativeHome) -> Result<usize, NativeRunError> {
        let offset = usize::try_from(home.offset.0).map_err(|_| NativeRunError::WordWidth)?;
        let index = offset / size_of::<u64>();
        if offset % size_of::<u64>() != 0 || index >= self.slots.len() {
            return Err(NativeRunError::InvalidHome(home.register));
        }
        Ok(index)
    }

    fn store(&mut self, home: NativeHome, value: Scalar) -> Result<(), NativeRunError> {
        if home.layout.is_some() || home.ty != value.ty() {
            return Err(NativeRunError::InvalidHome(home.register));
        }
        let index = self.slot(home)?;
        self.slots[index] = scalar_word(value);
        Ok(())
    }

    fn load(&self, home: NativeHome) -> Result<Scalar, NativeRunError> {
        if home.layout.is_some() {
            return Err(NativeRunError::InvalidHome(home.register));
        }
        decode(home.ty, self.slots[self.slot(home)?])
    }

    /// The words of a composite home, or of a scalar one.
    fn words(&self, home: NativeHome) -> Result<&[u64], NativeRunError> {
        let start = self.slot(home)?;
        let count = match home.layout {
            Some(id) => self
                .entry
                .plan()
                .layout(id)
                .ok_or(NativeRunError::InvalidHome(home.register))?
                .words() as usize,
            None => 1,
        };
        self.slots
            .get(start..start + count)
            .ok_or(NativeRunError::InvalidHome(home.register))
    }

    fn put(&mut self, home: NativeHome, word: u64) -> Result<(), NativeRunError> {
        let index = self.slot(home)?;
        self.slots[index] = word;
        Ok(())
    }

    fn text(&self, home: NativeHome, texts: &[Arc<str>]) -> Result<Arc<str>, NativeRunError> {
        let word = self.slots[self.slot(home)?];
        usize::try_from(word)
            .ok()
            .and_then(|index| texts.get(index))
            .cloned()
            .ok_or(NativeRunError::InvalidHome(home.register))
    }

    fn read(&self, home: NativeHome, heap: &Heap) -> Result<RecoveredValue, NativeRunError> {
        let Some(id) = home.layout else {
            return self.load(home).map(RecoveredValue::Scalar);
        };
        let layout = self
            .entry
            .plan()
            .layout(id)
            .ok_or(NativeRunError::InvalidHome(home.register))?;
        decode_layout(layout, self.words(home)?, heap)
    }

    fn snapshot(
        &self,
        stack: &[Frame],
        index: usize,
        heap: &Heap,
    ) -> Result<ScalarFrameSnapshot, NativeRunError> {
        let (pc, next, arrival) = match self.position {
            Position::Ready { next, arrival } => (next, next, arrival),
            Position::Waiting { call, successor } => {
                (call, successor, StatementArrival::Next(call))
            }
        };
        let plan = self.entry.plan();
        let site = plan
            .site(pc)
            .ok_or(NativeRunError::InvalidPc(u64::from(pc.0)))?;
        let mut values = Vec::new();
        for &slot in &site.recover {
            if let RecoverySlot::Initialized(home) = slot {
                let value = match self.read(home, heap)? {
                    RecoveredValue::Loan { .. } => recover_loan(stack, index, home, heap)?,
                    other => other,
                };
                values.push((home.register, value));
            }
        }
        Ok(ScalarFrameSnapshot {
            function: plan.function(),
            next,
            arrival,
            destination: self.destination.map(|home| home.register),
            values,
        })
    }
}

impl NativeActivation {
    fn new(state: State, limits: ExecutionLimits) -> Self {
        Self {
            state,
            limits,
            spent: 0,
            last_entry: None,
        }
    }

    /// Where the program's output goes. Set before the first slice.
    pub fn set_output(&mut self, sink: OutputSink) {
        match &mut self.state {
            State::Shared(shared) => shared.activation.set_output(sink),
            State::Active(word) => word.output = sink,
            _ => {}
        }
    }

    /// Replace the source of input lines.
    pub fn set_input(&mut self, source: InputSource) {
        match &mut self.state {
            State::Shared(shared) => shared.activation.set_input(source),
            State::Active(word) => word.input = source,
            _ => {}
        }
    }

    /// Let an empty input source report end of input.
    pub fn end_input(&mut self) {
        match &mut self.state {
            State::Shared(shared) => shared.activation.end_input(),
            State::Active(word) => word.input.end_input(),
            _ => {}
        }
    }

    /// Whether the last slice paused for a line of input.
    pub fn awaiting_input(&self) -> bool {
        matches!(&self.state, State::Shared(shared) if shared.activation.awaiting_input())
    }

    /// Collect before guest allocations to exercise the root set.
    pub fn set_gc_stress(&mut self, stress: bool) {
        match &mut self.state {
            State::Shared(shared) => shared.activation.set_gc_stress(stress),
            State::Active(word) => word.gc_stress = stress,
            _ => {}
        }
    }

    /// Initialize this invocation's pseudo-random state.
    pub fn set_random_seed(&mut self, seed: u64) {
        match &mut self.state {
            State::Shared(shared) => shared.activation.set_random_seed(seed),
            State::Active(word) => word.random_seed = Some(seed),
            _ => {}
        }
    }

    /// Occupancy and collection counts for the managed guest heap.
    pub fn heap_stats(&self) -> HeapStats {
        match &self.state {
            State::Shared(shared) => shared.activation.heap_stats(),
            _ => HeapStats {
                live: 0,
                peak: 0,
                collections: 0,
                counted: 0,
            },
        }
    }

    pub fn instructions_executed(&self) -> usize {
        match &self.state {
            State::Shared(shared) => shared.activation.instructions_executed(),
            _ => self.spent,
        }
    }

    pub fn last_entry(&self) -> Option<EntryEvent> {
        self.last_entry
    }

    pub fn status(&self) -> Result<ExecutionStatus, NativeRunError> {
        match &self.state {
            State::Terminal(status) => Ok(*status),
            State::Broken(error) => Err(*error),
            State::Shared(shared) => Ok(shared.activation.status()),
            State::Active(word) => {
                let frame = word.stack.last().ok_or(NativeRunError::InvalidActivation)?;
                let Position::Ready { next, .. } = frame.position else {
                    return Err(NativeRunError::InvalidActivation);
                };
                let site = frame
                    .entry
                    .plan()
                    .site(next)
                    .ok_or(NativeRunError::InvalidPc(u64::from(next.0)))?;
                Ok(ExecutionStatus::Paused(position(site.origin)))
            }
        }
    }

    pub fn resume(&mut self, budget: usize) -> Result<ExecutionStatus, NativeRunError> {
        if let State::Shared(shared) = &mut self.state {
            let result = shared.resume(budget, &mut self.last_entry);
            if let Err(error) = result {
                self.spent = shared.activation.instructions_executed();
                self.state = State::Broken(error);
            }
            return result;
        }
        let State::Active(word) = &mut self.state else {
            return self.status();
        };
        let mut available = budget.min(self.limits.fuel - self.spent);
        while available != 0 {
            match step(
                &mut word.stack,
                &mut Runtime {
                    heap: &mut word.heap,
                    output: &mut word.output,
                },
                self.limits.frames,
                &mut available,
                &mut self.spent,
                &mut self.last_entry,
            ) {
                Ok(Step::Continue) => {}
                Ok(Step::Terminal(status)) => {
                    self.state = State::Terminal(status);
                    return Ok(status);
                }
                Err(error) => {
                    self.state = State::Broken(error);
                    return Err(error);
                }
            }
        }
        if self.spent == self.limits.fuel {
            self.state = State::Terminal(ExecutionStatus::Failed(RunError::FuelExhausted));
        }
        self.status()
    }

    /// Consume a paused native activation and resume the same invocation in
    /// verified bytecode. Suspended callers use their pre-call storage maps.
    pub fn recover(self) -> Result<Activation, NativeRunError> {
        let WordActivation {
            stack,
            heap,
            output,
            input,
            gc_stress,
            random_seed,
        } = match self.state {
            State::Shared(shared)
                if matches!(shared.activation.status(), ExecutionStatus::Paused(_)) =>
            {
                return Ok(shared.activation);
            }
            State::Active(word) => *word,
            _ => return Err(NativeRunError::InvalidActivation),
        };
        let root = stack.first().ok_or(NativeRunError::InvalidActivation)?;
        let snapshots = (0..stack.len())
            .map(|index| stack[index].snapshot(&stack, index, &heap))
            .collect::<Result<Vec<_>, _>>()?;
        let mut activation = root
            .entry
            .module()
            .restore_scalars(snapshots, self.limits, self.spent)
            .map_err(NativeRunError::Recovery)?;
        activation.set_output(output);
        activation.set_input(input);
        activation.set_gc_stress(gc_stress);
        if let Some(seed) = random_seed {
            activation.set_random_seed(seed);
        }
        Ok(activation)
    }
}

enum Step {
    Continue,
    Terminal(ExecutionStatus),
}

#[derive(Debug)]
struct SharedActivation {
    activation: Activation,
    entry: EntryPin,
    entries: HashMap<FunctionId, EntryPin>,
    slots: Vec<u64>,
}

impl SharedActivation {
    fn resume(
        &mut self,
        budget: usize,
        entry_event: &mut Option<EntryEvent>,
    ) -> Result<ExecutionStatus, NativeRunError> {
        let start = self.activation.instructions_executed();
        while matches!(self.activation.status(), ExecutionStatus::Paused(_)) {
            let used = self.activation.instructions_executed() - start;
            let available = (budget - used).min(self.activation.native_fuel());
            if available == 0 {
                break;
            }
            let native = self
                .activation
                .native_position(self.entry.module())
                .and_then(|(position, arrival)| {
                    Some((
                        self.entries.get(&position.function)?,
                        position.instruction,
                        arrival,
                    ))
                });
            if let Some((entry, next, arrival)) = native {
                run_section(
                    &mut self.activation,
                    &mut self.slots,
                    entry,
                    next,
                    arrival,
                    available,
                    entry_event,
                )?;
            } else {
                self.activation.resume(1);
            }
            if self.activation.awaiting_input() {
                break;
            }
        }
        Ok(self.activation.status())
    }
}

fn initialized_scalars(site: &Site) -> impl Iterator<Item = NativeHome> + '_ {
    site.recover.iter().filter_map(|slot| match slot {
        RecoverySlot::Initialized(home) => Some(*home),
        RecoverySlot::Uninitialized(_) => None,
    })
}

fn run_section(
    activation: &mut Activation,
    slots: &mut Vec<u64>,
    entry: &EntryPin,
    next: InstructionId,
    arrival: StatementArrival,
    budget: usize,
    entry_event: &mut Option<EntryEvent>,
) -> Result<(), NativeRunError> {
    let plan = entry.plan();
    let invalid_home = |home: NativeHome| NativeRunError::InvalidHome(home.register);
    let slot = |home: NativeHome| home.offset.0 as usize / size_of::<u64>();
    slots.resize(plan.frame_bytes() as usize / size_of::<u64>(), 0);
    let start = plan
        .site(next)
        .ok_or(NativeRunError::InvalidPc(u64::from(next.0)))?;
    for home in initialized_scalars(start) {
        let value = activation
            .native_scalar(home.register)
            .filter(|value| value.ty() == home.ty)
            .ok_or_else(|| invalid_home(home))?;
        *slots
            .get_mut(slot(home))
            .ok_or_else(|| invalid_home(home))? = scalar_word(value);
    }

    let exit = invoke(entry, slots, next, arrival, budget)?;
    *entry_event = Some(EntryEvent {
        origin: start.origin,
        tier: entry.tier(),
    });
    let site = plan
        .site(exit.pc)
        .ok_or(NativeRunError::InvalidPc(u64::from(exit.pc.0)))?;
    let bridge = match exit.outcome {
        ScalarOutcome::Failed => {
            activation.fail_from_native(decode_failure(site, exit.context.failure)?, exit.used);
            return Ok(());
        }
        ScalarOutcome::Paused if exit.used == budget => false,
        ScalarOutcome::Bridge if site.operation == Operation::Runtime => true,
        _ => return Err(NativeRunError::InvalidOutcome(exit.outcome as u64)),
    };
    let values = initialized_scalars(site)
        .map(|home| {
            let value = *slots.get(slot(home)).ok_or_else(|| invalid_home(home))?;
            Ok((home.register, decode(home.ty, value)?))
        })
        .collect::<Result<Vec<_>, NativeRunError>>()?;
    let arrival = decode_edge(exit.context.predecessor, exit.context.edge)?;
    // The emitter reserved the bridge's unit of fuel. The reference step
    // charges it only if the operation runs, including a failing instruction.
    activation
        .resume_from_native(
            exit.pc,
            arrival,
            initialized_scalars(start).map(|home| home.register),
            &values,
            exit.used - usize::from(bridge),
        )
        .ok_or(NativeRunError::InvalidEdge)?;
    if bridge {
        activation.resume(1);
    }
    Ok(())
}

struct NativeExit {
    context: ScalarContext,
    outcome: ScalarOutcome,
    pc: InstructionId,
    used: usize,
    result: u64,
}

fn invoke(
    pin: &EntryPin,
    slots: &mut [u64],
    next: InstructionId,
    arrival: StatementArrival,
    budget: usize,
) -> Result<NativeExit, NativeRunError> {
    let input_budget = u64::try_from(budget).map_err(|_| NativeRunError::WordWidth)?;
    let (predecessor, edge) = encode_edge(arrival);
    let mut context = ScalarContext {
        slots: std::ptr::null_mut(),
        budget: input_budget,
        next: u64::from(next.0),
        failure: 0,
        links: std::ptr::null(),
        callee: 0,
        predecessor,
        edge,
    };
    let mut result = 0;
    let raw = pin.executable().invoke(Invocation {
        context: &mut context,
        slots,
        links: pin.links(),
        result: &mut result,
    });
    let used = input_budget
        .checked_sub(context.budget)
        .filter(|used| *used != 0)
        .ok_or(NativeRunError::InvalidBudget)?;
    let used = usize::try_from(used).map_err(|_| NativeRunError::WordWidth)?;
    let pc = InstructionId(
        u32::try_from(context.next).map_err(|_| NativeRunError::InvalidPc(context.next))?,
    );
    Ok(NativeExit {
        context,
        result,
        outcome: outcome(raw)?,
        pc,
        used,
    })
}

/// What native frames refer to by index or address: strings, and the
/// buffers of arrays and vectors, which the activation owns until released.
#[derive(Debug, Default)]
struct Heap {
    texts: Vec<Arc<str>>,
    buffers: std::collections::HashMap<u64, Box<[u64]>>,
}

impl Heap {
    /// An independent copy of the buffer at `address`.
    fn copy_buffer(&mut self, address: u64) -> Result<u64, NativeRunError> {
        let copy = self
            .buffers
            .get(&address)
            .ok_or(NativeRunError::InvalidActivation)?
            .clone();
        Ok(self.insert_buffer(copy))
    }

    /// A buffer of `count` elements of two words each, and its address.
    fn allocate(&mut self, count: usize) -> u64 {
        let buffer = vec![0u64; count * 2].into_boxed_slice();
        self.insert_buffer(buffer)
    }

    fn insert_buffer(&mut self, mut buffer: Box<[u64]>) -> u64 {
        let address = buffer.as_mut_ptr() as u64;
        self.buffers.insert(address, buffer);
        address
    }

    fn new_vector(&mut self, length: usize, elements: impl Iterator<Item = u64>) -> u64 {
        let mut buffer = Vec::with_capacity(VECTOR_HEADER_WORDS + length);
        buffer.extend([length as u64, length as u64]);
        buffer.extend(elements);
        self.insert_buffer(buffer.into_boxed_slice())
    }

    fn vector_buffer(&self, address: u64) -> Result<&[u64], NativeRunError> {
        let buffer = self
            .buffers
            .get(&address)
            .ok_or(NativeRunError::InvalidActivation)?;
        match &buffer[..] {
            [length, capacity, data @ ..]
                if *length <= *capacity && *capacity == data.len() as u64 =>
            {
                Ok(buffer)
            }
            _ => Err(NativeRunError::InvalidActivation),
        }
    }

    fn vector_elements(&self, address: u64) -> Result<&[u64], NativeRunError> {
        let buffer = self.vector_buffer(address)?;
        Ok(&buffer[VECTOR_HEADER_WORDS..VECTOR_HEADER_WORDS + buffer[0] as usize])
    }

    /// Append to a validated vector below its element limit, returning the new
    /// address if growth moved the buffer.
    fn push_vector(&mut self, address: u64, value: u64) -> Result<Option<u64>, NativeRunError> {
        let buffer = self.vector_buffer(address)?;
        let length = buffer[0] as usize;
        let capacity = buffer[1] as usize;
        if length < capacity {
            let buffer = self
                .buffers
                .get_mut(&address)
                .ok_or(NativeRunError::InvalidActivation)?;
            buffer[VECTOR_HEADER_WORDS + length] = value;
            buffer[0] += 1;
            return Ok(None);
        }
        let capacity = (capacity * 2).clamp(1, MAX_VEC_ELEMENTS);
        let mut next = vec![0; VECTOR_HEADER_WORDS + capacity].into_boxed_slice();
        let end = VECTOR_HEADER_WORDS + length;
        next[VECTOR_HEADER_WORDS..end].copy_from_slice(&buffer[VECTOR_HEADER_WORDS..end]);
        next[0] = length as u64 + 1;
        next[1] = capacity as u64;
        next[end] = value;
        let next = self.insert_buffer(next);
        self.buffers.remove(&address);
        Ok(Some(next))
    }
}

/// What native code reaches through bridges.
struct Runtime<'a> {
    heap: &'a mut Heap,
    output: &'a mut OutputSink,
}

fn step(
    stack: &mut Vec<Frame>,
    runtime: &mut Runtime<'_>,
    max_frames: usize,
    available: &mut usize,
    spent: &mut usize,
    entry_event: &mut Option<EntryEvent>,
) -> Result<Step, NativeRunError> {
    let depth = stack.len();
    let frame = stack.last_mut().ok_or(NativeRunError::InvalidActivation)?;
    let Position::Ready { next, arrival } = frame.position else {
        return Err(NativeRunError::InvalidActivation);
    };
    let pin = frame.entry.clone();
    let plan = pin.plan();
    let entry_site = plan
        .site(next)
        .ok_or(NativeRunError::InvalidPc(u64::from(next.0)))?;
    let NativeExit {
        context,
        outcome,
        pc,
        used,
        result,
    } = invoke(&pin, &mut frame.slots, next, arrival, *available)?;
    *entry_event = Some(EntryEvent {
        origin: entry_site.origin,
        tier: pin.tier(),
    });
    *available -= used;
    *spent += used;
    let site = plan
        .site(pc)
        .ok_or(NativeRunError::InvalidPc(context.next))?;
    match outcome {
        ScalarOutcome::Paused => {
            if *available != 0 {
                return Err(NativeRunError::InvalidBudget);
            }
            let arrival = decode_edge(context.predecessor, context.edge)?;
            let body = pin
                .module()
                .function(plan.function())
                .ok_or(NativeRunError::InvalidActivation)?;
            if !arrival.reaches(pc, body.instructions()) {
                return Err(NativeRunError::InvalidEdge);
            }
            frame.position = Position::Ready { next: pc, arrival };
        }
        ScalarOutcome::Bridge => {
            let Operation::Bridge(index) = site.operation else {
                return Err(NativeRunError::InvalidPc(context.next));
            };
            let bridge = plan
                .bridge(index)
                .ok_or(NativeRunError::InvalidPc(context.next))?;
            if let Some(error) = perform(bridge, stack, runtime, site.origin)? {
                return Ok(Step::Terminal(ExecutionStatus::Failed(error)));
            }
            let frame = stack.last_mut().ok_or(NativeRunError::InvalidActivation)?;
            frame.position = Position::Ready {
                next: InstructionId(pc.0 + 1),
                arrival: StatementArrival::Next(pc),
            };
        }
        ScalarOutcome::Call => {
            let call = plan
                .call(pc)
                .ok_or(NativeRunError::InvalidPc(context.next))?;
            let entry = pin
                .dependency_at(pc, context.callee)
                .ok_or(NativeRunError::InvalidCallee(pc))?;
            if depth == max_frames {
                return Ok(Step::Terminal(ExecutionStatus::Failed(
                    RunError::FrameLimit,
                )));
            }
            let mut callee = Frame::new(entry.clone(), call.destination)?;
            for argument in &call.arguments {
                if argument.caller.layout.is_some() {
                    let mut words = frame.words(argument.caller)?.to_vec();
                    if let Some(id) = argument.caller.layout
                        && plan.layout(id).is_some_and(Layout::owns_buffer)
                    {
                        // Owned buffers are passed by value.
                        words[0] = runtime.heap.copy_buffer(words[0])?;
                    }
                    let start = callee.slot(argument.callee)?;
                    callee
                        .slots
                        .get_mut(start..start + words.len())
                        .ok_or(NativeRunError::InvalidHome(argument.callee.register))?
                        .copy_from_slice(&words);
                } else {
                    callee.store(argument.callee, frame.load(argument.caller)?)?;
                }
            }
            frame.position = Position::Waiting {
                call: pc,
                successor: call.successor,
            };
            stack.push(callee);
        }
        ScalarOutcome::Returned => {
            if !matches!(site.operation, Operation::Return(_)) {
                return Err(NativeRunError::InvalidPc(context.next));
            }
            let value = plan.result().map(|ty| decode(ty, result)).transpose()?;
            // A composite result stays in the callee's frame, at the returned home.
            let composite = match (&site.operation, plan.result_layout()) {
                (Operation::Return(Some(home)), Some(_)) => Some(frame.words(*home)?.to_vec()),
                _ => None,
            };
            let destination = frame.destination;
            stack.pop();
            let Some(caller) = stack.last_mut() else {
                if composite.is_some() {
                    return Err(NativeRunError::InvalidActivation);
                }
                return Ok(Step::Terminal(ExecutionStatus::Returned(value)));
            };
            let Position::Waiting { call, successor } = caller.position else {
                return Err(NativeRunError::InvalidActivation);
            };
            if let Some(home) = destination {
                if let Some(words) = composite {
                    let start = caller.slot(home)?;
                    caller
                        .slots
                        .get_mut(start..start + words.len())
                        .ok_or(NativeRunError::InvalidHome(home.register))?
                        .copy_from_slice(&words);
                } else {
                    caller.store(
                        home,
                        value.ok_or(NativeRunError::InvalidHome(home.register))?,
                    )?;
                }
            }
            caller.position = Position::Ready {
                next: successor,
                arrival: StatementArrival::Next(call),
            };
        }
        ScalarOutcome::Failed => {
            return Ok(Step::Terminal(ExecutionStatus::Failed(decode_failure(
                site,
                context.failure,
            )?)));
        }
    }
    Ok(Step::Continue)
}

fn decode_failure(site: &Site, raw: u64) -> Result<RunError, NativeRunError> {
    let failure = site
        .failures
        .iter()
        .copied()
        .find(|failure| *failure as u64 == raw)
        .ok_or(NativeRunError::InvalidFailure(raw))?;
    let Origin {
        function,
        instruction,
        span,
        ..
    } = site.origin;
    let error = match failure {
        Failure::Overflow => RunError::IntegerOverflow {
            function,
            instruction,
            span,
        },
        Failure::DivisionByZero => RunError::DivisionByZero {
            function,
            instruction,
            span,
        },
        Failure::ConversionOutOfRange => RunError::ConversionOutOfRange {
            function,
            instruction,
            span,
        },
        Failure::IndexOutOfBounds => RunError::IndexOutOfBounds {
            function,
            instruction,
            span,
        },
        Failure::UninitializedElement => RunError::UninitializedElement {
            function,
            instruction,
            span,
        },
        Failure::ZeroStep => RunError::ZeroStep {
            function,
            instruction,
            span,
        },
        Failure::EnumVariant => RunError::EnumVariant {
            function,
            instruction,
            span,
        },
        Failure::Trap => RunError::ReachedTrap {
            function,
            instruction,
            span,
        },
    };
    Ok(error)
}

fn encode_edge(arrival: StatementArrival) -> (u64, u64) {
    let (from, edge) = match arrival {
        StatementArrival::Entry => (ENTRY, Edge::Entry),
        StatementArrival::Next(pc) => (pc, Edge::Next),
        StatementArrival::Jump(pc) => (pc, Edge::Jump),
        StatementArrival::Branch { from, taken: true } => (from, Edge::Yes),
        StatementArrival::Branch { from, taken: false } => (from, Edge::No),
    };
    (u64::from(from.0), edge as u64)
}

fn decode_edge(from: u64, edge: u64) -> Result<StatementArrival, NativeRunError> {
    let from = InstructionId(u32::try_from(from).map_err(|_| NativeRunError::InvalidEdge)?);
    if edge == Edge::Entry as u64 && from == ENTRY {
        Ok(StatementArrival::Entry)
    } else if edge == Edge::Next as u64 {
        Ok(StatementArrival::Next(from))
    } else if edge == Edge::Jump as u64 {
        Ok(StatementArrival::Jump(from))
    } else if edge == Edge::Yes as u64 || edge == Edge::No as u64 {
        Ok(StatementArrival::Branch {
            from,
            taken: edge == Edge::Yes as u64,
        })
    } else {
        Err(NativeRunError::InvalidEdge)
    }
}

fn outcome(raw: u64) -> Result<ScalarOutcome, NativeRunError> {
    [
        ScalarOutcome::Returned,
        ScalarOutcome::Paused,
        ScalarOutcome::Failed,
        ScalarOutcome::Call,
        ScalarOutcome::Bridge,
    ]
    .into_iter()
    .find(|outcome| *outcome as u64 == raw)
    .ok_or(NativeRunError::InvalidOutcome(raw))
}

fn position(origin: Origin) -> ExecutionPosition {
    ExecutionPosition {
        function: origin.function,
        instruction: origin.instruction,
        span: origin.span,
    }
}

/// Resolve a spilled buffer home, including a loan into another frame.
fn buffer_slot(stack: &[Frame], base: LoanBase) -> Result<(usize, usize), NativeRunError> {
    let index = stack
        .len()
        .checked_sub(1)
        .ok_or(NativeRunError::InvalidActivation)?;
    let frame = &stack[index];
    match base {
        LoanBase::Owner(offset) => Ok((index, offset.0 as usize / size_of::<u64>())),
        LoanBase::Loan(home) => {
            let address = frame.slots[frame.slot(home)?];
            let (owner, _) = resolve_loan(stack, index, home.register)?;
            let slots = &stack[owner].slots;
            let offset = address
                .checked_sub(slots.as_ptr() as u64)
                .and_then(|offset| usize::try_from(offset).ok())
                .filter(|&offset| {
                    offset % size_of::<u64>() == 0 && offset / size_of::<u64>() < slots.len()
                })
                .ok_or(NativeRunError::InvalidHome(home.register))?;
            Ok((owner, offset / size_of::<u64>()))
        }
    }
}

/// A reborrow from a vector loan can survive a push through that loan.
/// On growth, update the spilled element loans into the old buffer.
fn relocate_vector_loans(
    stack: &mut [Frame],
    pc: InstructionId,
    old: u64,
    new: u64,
    length: usize,
) -> Result<(), NativeRunError> {
    let first = u64::from(VECTOR_HEADER_BYTES);
    let elements = first..first + (length * size_of::<u64>()) as u64;
    let depth = stack.len();
    for (index, frame) in stack.iter_mut().enumerate() {
        let pc = match frame.position {
            _ if index + 1 == depth => pc,
            Position::Waiting { call, .. } => call,
            _ => return Err(NativeRunError::InvalidActivation),
        };
        let plan = frame.entry.plan();
        let site = plan
            .site(pc)
            .ok_or(NativeRunError::InvalidPc(u64::from(pc.0)))?;
        for &slot in &site.recover {
            if let RecoverySlot::Initialized(home) = slot
                && home.layout.and_then(|id| plan.layout(id)) == Some(&Layout::Loan)
            {
                let index = frame.slot(home)?;
                let pointer = &mut frame.slots[index];
                if let Some(offset) = pointer.checked_sub(old)
                    && elements.contains(&offset)
                {
                    *pointer = new + offset;
                }
            }
        }
    }
    Ok(())
}

/// Run a bridge call for the frame. `Some` is a failure of the program.
fn perform(
    bridge: &BridgeCall,
    stack: &mut [Frame],
    runtime: &mut Runtime<'_>,
    origin: Origin,
) -> Result<Option<RunError>, NativeRunError> {
    let frame = stack.last_mut().ok_or(NativeRunError::InvalidActivation)?;
    let heap_exhausted = || RunError::HeapExhausted {
        function: origin.function,
        instruction: origin.instruction,
        span: origin.span,
    };
    let intern = |runtime: &mut Runtime<'_>, text: Arc<str>| -> u64 {
        runtime.heap.texts.push(text);
        (runtime.heap.texts.len() - 1) as u64
    };
    match bridge {
        BridgeCall::NewArray {
            destination,
            count,
            elements,
        } => {
            let address = runtime.heap.allocate(*count as usize);
            if let Some(elements) = elements {
                let buffer = runtime
                    .heap
                    .buffers
                    .get_mut(&address)
                    .ok_or(NativeRunError::InvalidActivation)?;
                for (index, &home) in elements.iter().enumerate() {
                    buffer[index * 2] = 1;
                    buffer[index * 2 + 1] = frame.slots[frame.slot(home)?];
                }
            }
            frame.put(*destination, address)?;
        }
        BridgeCall::CopyBuffer {
            destination,
            source,
        } => {
            let (owner, slot) = buffer_slot(stack, *source)?;
            let word = stack[owner].slots[slot];
            let address = runtime.heap.copy_buffer(word)?;
            let frame = stack.last_mut().ok_or(NativeRunError::InvalidActivation)?;
            frame.put(*destination, address)?;
        }
        BridgeCall::ReleaseBuffer(home) => {
            let word = frame.slots[frame.slot(*home)?];
            runtime.heap.buffers.remove(&word);
        }
        BridgeCall::NewVec {
            destination,
            elements,
        } => {
            let elements = elements
                .iter()
                .map(|&home| frame.load(home).map(scalar_word))
                .collect::<Result<Vec<_>, _>>()?;
            let address = runtime
                .heap
                .new_vector(elements.len(), elements.into_iter());
            frame.put(*destination, address)?;
        }
        BridgeCall::VecFromRange { destination, range } => {
            let [start, end] = *frame.words(*range)? else {
                return Err(NativeRunError::InvalidHome(range.register));
            };
            let (start, end) = (start as i64, end as i64);
            let length = usize::try_from(end.saturating_sub(start).max(0)).unwrap_or(usize::MAX);
            if length > MAX_VEC_ELEMENTS {
                return Ok(Some(heap_exhausted()));
            }
            let address = runtime
                .heap
                .new_vector(length, (start..end).map(|item| item as u64));
            frame.put(*destination, address)?;
        }
        BridgeCall::VecPush { target, value } => {
            let value = scalar_word(frame.load(*value)?);
            let (owner, slot) = buffer_slot(stack, *target)?;
            let address = stack[owner].slots[slot];
            let length = runtime.heap.vector_elements(address)?.len();
            if length >= MAX_VEC_ELEMENTS {
                return Ok(Some(heap_exhausted()));
            }
            if let Some(next) = runtime.heap.push_vector(address, value)? {
                stack[owner].slots[slot] = next;
                relocate_vector_loans(stack, origin.instruction, address, next, length)?;
            }
        }
        BridgeCall::TextConstant { destination, text } => {
            let module = frame.entry.module();
            let body = module
                .function(origin.function)
                .ok_or(NativeRunError::InvalidActivation)?;
            let value = body
                .texts()
                .get(text.0 as usize)
                .cloned()
                .ok_or(NativeRunError::InvalidActivation)?;
            let word = intern(runtime, value);
            frame.put(*destination, word)?;
        }
        BridgeCall::TextConcat {
            destination,
            left,
            right,
        } => {
            let joined = format!(
                "{}{}",
                frame.text(*left, &runtime.heap.texts)?,
                frame.text(*right, &runtime.heap.texts)?
            );
            let word = intern(runtime, joined.into());
            frame.put(*destination, word)?;
        }
        BridgeCall::TextCompare {
            operation,
            destination,
            left,
            right,
        } => {
            let ordering = frame
                .text(*left, &runtime.heap.texts)?
                .cmp(&frame.text(*right, &runtime.heap.texts)?);
            let value = match operation {
                TextOp::Equal => ordering.is_eq(),
                TextOp::NotEqual => ordering.is_ne(),
                TextOp::Less => ordering.is_lt(),
                TextOp::LessEqual => ordering.is_le(),
                TextOp::Greater => ordering.is_gt(),
                TextOp::GreaterEqual => ordering.is_ge(),
            };
            frame.put(*destination, u64::from(value))?;
        }
        BridgeCall::TextLength {
            destination,
            source,
        } => {
            let length = frame.text(*source, &runtime.heap.texts)?.chars().count();
            let length = i64::try_from(length).unwrap_or(i64::MAX);
            frame.put(*destination, scalar_word(Scalar::I64(length)))?;
        }
        BridgeCall::TextSlice {
            kind,
            destination,
            source,
            first,
            second,
        } => {
            let text = frame.text(*source, &runtime.heap.texts)?;
            let int = |home: NativeHome| -> Result<i64, NativeRunError> {
                match frame.load(home)? {
                    Scalar::I64(value) => Ok(value),
                    _ => Err(NativeRunError::InvalidHome(home.register)),
                }
            };
            let sliced: Result<String, TextFailure> = match kind {
                SliceKind::Left => crate::bytecode::verified::slice_left(&text, int(*first)?),
                SliceKind::Right => crate::bytecode::verified::slice_right(&text, int(*first)?),
                SliceKind::Mid => {
                    let second = second.ok_or(NativeRunError::InvalidActivation)?;
                    crate::bytecode::verified::slice_mid(&text, int(*first)?, int(second)?)
                }
            };
            match sliced {
                Ok(value) => {
                    let word = intern(runtime, value.into());
                    frame.put(*destination, word)?;
                }
                Err(failure) => {
                    return Ok(Some(RunError::Text {
                        failure,
                        function: origin.function,
                        instruction: origin.instruction,
                        span: origin.span,
                    }));
                }
            }
        }
        BridgeCall::TextMap {
            operation,
            destination,
            source,
        } => {
            let text = frame.text(*source, &runtime.heap.texts)?;
            let mapped = match operation {
                TextMapOp::Trim => text.trim().to_owned(),
                TextMapOp::Upper => text.to_uppercase(),
                TextMapOp::Lower => text.to_lowercase(),
            };
            let word = intern(runtime, mapped.into());
            frame.put(*destination, word)?;
        }
        BridgeCall::TextTest {
            operation,
            destination,
            text,
            other,
        } => {
            let (text, other) = (
                frame.text(*text, &runtime.heap.texts)?,
                frame.text(*other, &runtime.heap.texts)?,
            );
            let result = match operation {
                TextTestOp::Contains => text.contains(&*other),
                TextTestOp::StartsWith => text.starts_with(&*other),
                TextTestOp::EndsWith => text.ends_with(&*other),
            };
            frame.put(*destination, u64::from(result))?;
        }
        BridgeCall::TextReplace {
            destination,
            text,
            from,
            to,
        } => {
            let replaced = frame.text(*text, &runtime.heap.texts)?.replace(
                &*frame.text(*from, &runtime.heap.texts)?,
                &frame.text(*to, &runtime.heap.texts)?,
            );
            let word = intern(runtime, replaced.into());
            frame.put(*destination, word)?;
        }
        BridgeCall::ToText {
            destination,
            source,
        } => {
            let value = frame.read(*source, runtime.heap)?;
            let spelled = crate::bytecode::verified::spell_recovered(&value)
                .ok_or(NativeRunError::InvalidHome(source.register))?;
            let word = intern(runtime, spelled.into());
            frame.put(*destination, word)?;
        }
        BridgeCall::Print {
            template,
            arguments,
        } => {
            let module = frame.entry.module();
            let body = module
                .function(origin.function)
                .ok_or(NativeRunError::InvalidActivation)?;
            let template = body
                .outputs()
                .get(template.0 as usize)
                .ok_or(NativeRunError::InvalidActivation)?;
            let values = arguments
                .iter()
                .map(|&home| frame.read(home, runtime.heap))
                .collect::<Result<Vec<_>, _>>()?;
            let line = crate::bytecode::verified::render_recovered_output(template, &values)
                .ok_or(NativeRunError::InvalidActivation)?;
            runtime.output.write_line(&line);
        }
    }
    Ok(None)
}

fn decode(ty: ScalarType, word: u64) -> Result<Scalar, NativeRunError> {
    match (ty, word) {
        (ScalarType::I64, word) => Ok(Scalar::I64(i64::from_ne_bytes(word.to_ne_bytes()))),
        (ScalarType::F64, word) => Ok(Scalar::F64(word)),
        (ScalarType::U64, word) => Ok(Scalar::U64(word)),
        (ScalarType::Bool, 0) => Ok(Scalar::Bool(false)),
        (ScalarType::Bool, 1) => Ok(Scalar::Bool(true)),
        (ScalarType::Bool, word) => Err(NativeRunError::InvalidBoolean(word)),
    }
}

/// A loan register rebuilt for the interpreter: its owner from the static
/// origin, and the projection steps from where its address lies in the
/// owner's words.
fn recover_loan(
    stack: &[Frame],
    index: usize,
    home: NativeHome,
    heap: &Heap,
) -> Result<RecoveredValue, NativeRunError> {
    // An element loan carries a tag in its low bit; see `ElementLoan`.
    let tagged = {
        let frame = stack.get(index).ok_or(NativeRunError::InvalidActivation)?;
        frame.slots[frame.slot(home)?] & 1 == 1
    };
    use super::{RecoverySlot, prepare::layout_of_top};
    use crate::bytecode::verified::{RecoveredStep, RegisterType};
    let invalid = NativeRunError::InvalidHome(home.register);
    let frame = &stack[index];
    let pointer = frame.slots[frame.slot(home)?];
    let (owner_frame, owner) = resolve_loan(stack, index, home.register)?;
    let holder = &stack[owner_frame];
    let plan = holder.entry.plan();
    let pc = match holder.position {
        Position::Ready { next, .. } => next,
        Position::Waiting { call, .. } => call,
    };
    let owner_home = plan
        .site(pc)
        .and_then(|site| {
            site.recover.iter().find_map(|slot| match slot {
                RecoverySlot::Initialized(candidate) | RecoverySlot::Uninitialized(candidate)
                    if candidate.register == owner =>
                {
                    Some(*candidate)
                }
                _ => None,
            })
        })
        .ok_or(invalid)?;
    if tagged {
        // The owner is an array; the loan's address says which element.
        let invalid = NativeRunError::InvalidHome(home.register);
        let buffer = holder.slots[holder.slot(owner_home)?];
        let element = (pointer & !1)
            .checked_sub(8)
            .and_then(|address| address.checked_sub(buffer))
            .filter(|offset| offset % 16 == 0)
            .ok_or(invalid)?
            / 16;
        return Ok(RecoveredValue::Loan {
            frame: owner_frame,
            register: owner,
            path: vec![crate::bytecode::verified::RecoveredStep::Offset(
                usize::try_from(element).map_err(|_| invalid)?,
            )],
        });
    }
    let base = holder.slots.as_ptr() as u64 + u64::from(owner_home.offset.0);
    let owner_layout = match owner_home.layout {
        Some(id) => plan.layout(id).ok_or(invalid)?.clone(),
        None => Layout::Scalar(owner_home.ty),
    };
    if matches!(owner_layout, Layout::Vec(_)) && pointer != base {
        let buffer = holder.slots[holder.slot(owner_home)?];
        let elements = heap.vector_elements(buffer)?;
        let offset = pointer
            .checked_sub(buffer + u64::from(VECTOR_HEADER_BYTES))
            .filter(|offset| offset % size_of::<u64>() as u64 == 0)
            .ok_or(invalid)?
            / size_of::<u64>() as u64;
        let offset = usize::try_from(offset)
            .ok()
            .filter(|&offset| offset < elements.len())
            .ok_or(invalid)?;
        return Ok(RecoveredValue::Loan {
            frame: owner_frame,
            register: owner,
            path: vec![RecoveredStep::Offset(offset)],
        });
    }
    let delta = pointer.checked_sub(base).ok_or(invalid)?;
    if delta % 8 != 0 {
        return Err(invalid);
    }
    let body = frame
        .entry
        .module()
        .function(frame.entry.plan().function())
        .ok_or(NativeRunError::InvalidActivation)?;
    let Some(RegisterType::Loan { pointee, .. }) =
        body.registers().get(home.register.0 as usize).copied()
    else {
        return Err(invalid);
    };
    let target = layout_of_top(body, pointee).ok_or(invalid)?;
    let start = holder.slot(owner_home)?;
    let words = holder
        .slots
        .get(start..start + owner_layout.words() as usize)
        .ok_or(invalid)?;
    fn walk(
        layout: &Layout,
        words: &[u64],
        offset: u64,
        target: &Layout,
    ) -> Option<Vec<RecoveredStep>> {
        if offset == 0 && layout == target {
            return Some(Vec::new());
        }
        match layout {
            Layout::Tuple(fields) => {
                let mut start = 0u64;
                for (index, field) in fields.iter().enumerate() {
                    let size = u64::from(field.words());
                    if offset < start + size {
                        let slice = words.get(start as usize..(start + size) as usize)?;
                        let mut path = walk(field, slice, offset - start, target)?;
                        path.insert(0, RecoveredStep::Field(u32::try_from(index).ok()?));
                        return Some(path);
                    }
                    start += size;
                }
                None
            }
            Layout::Enum(variants) if offset >= 1 => {
                let variant = u32::try_from(*words.first()?).ok()?;
                let payload = variants.get(variant as usize)?.as_ref()?;
                let slice = words.get(1..1 + payload.words() as usize)?;
                let mut path = walk(payload, slice, offset - 1, target)?;
                path.insert(0, RecoveredStep::Payload(variant));
                Some(path)
            }
            _ => None,
        }
    }
    let path = walk(&owner_layout, words, delta / 8, &target).ok_or(invalid)?;
    Ok(RecoveredValue::Loan {
        frame: owner_frame,
        register: owner,
        path,
    })
}

/// The frame and register that own the place a loan register borrows, from
/// the loan's static origin. A loan received as a parameter belongs to what
/// the caller passed.
fn resolve_loan(
    stack: &[Frame],
    index: usize,
    register: Register,
) -> Result<(usize, Register), NativeRunError> {
    use crate::bytecode::verified::{LoanOrigin, RegisterType};
    let frame = stack.get(index).ok_or(NativeRunError::InvalidActivation)?;
    let plan = frame.entry.plan();
    let body = frame
        .entry
        .module()
        .function(plan.function())
        .ok_or(NativeRunError::InvalidActivation)?;
    let Some(RegisterType::Loan { origin, .. }) =
        body.registers().get(register.0 as usize).copied()
    else {
        return Err(NativeRunError::InvalidHome(register));
    };
    match origin {
        LoanOrigin::Local(owner) => Ok((index, owner)),
        LoanOrigin::Parameter {
            register: parameter,
        } => {
            let position = body
                .parameters()
                .iter()
                .position(|&candidate| candidate == parameter)
                .ok_or(NativeRunError::InvalidHome(parameter))?;
            let caller = index
                .checked_sub(1)
                .ok_or(NativeRunError::InvalidHome(parameter))?;
            let Position::Waiting { call, .. } = stack[caller].position else {
                return Err(NativeRunError::InvalidActivation);
            };
            let call = stack[caller]
                .entry
                .plan()
                .call(call)
                .ok_or(NativeRunError::InvalidCallee(call))?;
            let argument = call
                .arguments
                .get(position)
                .ok_or(NativeRunError::InvalidHome(parameter))?;
            resolve_loan(stack, caller, argument.caller.register)
        }
        _ => Err(NativeRunError::InvalidHome(register)),
    }
}

fn decode_layout(
    layout: &Layout,
    words: &[u64],
    heap: &Heap,
) -> Result<RecoveredValue, NativeRunError> {
    match layout {
        Layout::RuntimeValue => Err(NativeRunError::InvalidActivation),
        Layout::Array { element, count } => {
            let address = *words.first().ok_or(NativeRunError::InvalidActivation)?;
            let buffer = heap
                .buffers
                .get(&address)
                .ok_or(NativeRunError::InvalidActivation)?;
            if buffer.len() != *count as usize * 2 {
                return Err(NativeRunError::InvalidActivation);
            }
            let items = buffer
                .chunks(2)
                .map(|pair| match pair[0] {
                    0 => Ok(None),
                    _ => decode(*element, pair[1]).map(Some),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(RecoveredValue::Array(items))
        }
        Layout::Vec(element) => {
            let address = *words.first().ok_or(NativeRunError::InvalidActivation)?;
            let items = heap
                .vector_elements(address)?
                .iter()
                .map(|&word| decode(*element, word))
                .collect::<Result<_, _>>()?;
            Ok(RecoveredValue::Vec(items))
        }
        Layout::Text => {
            let word = *words.first().ok_or(NativeRunError::InvalidActivation)?;
            usize::try_from(word)
                .ok()
                .and_then(|index| heap.texts.get(index))
                .map(|text| RecoveredValue::Text(text.clone()))
                .ok_or(NativeRunError::InvalidActivation)
        }
        // The owner is found from the loan's static origin by the caller.
        Layout::Loan => Ok(RecoveredValue::Loan {
            frame: usize::MAX,
            register: Register(0),
            path: Vec::new(),
        }),
        Layout::Scalar(ty) => {
            let word = *words.first().ok_or(NativeRunError::InvalidActivation)?;
            decode(*ty, word).map(RecoveredValue::Scalar)
        }
        Layout::Tuple(fields) => {
            let mut at = 0usize;
            let mut values = Vec::with_capacity(fields.len());
            for field in fields.iter() {
                let count = field.words() as usize;
                let slice = words
                    .get(at..at + count)
                    .ok_or(NativeRunError::InvalidActivation)?;
                values.push(decode_layout(field, slice, heap)?);
                at += count;
            }
            Ok(RecoveredValue::Tuple(values))
        }
        Layout::Enum(variants) => {
            let tag = *words.first().ok_or(NativeRunError::InvalidActivation)?;
            let variant = u32::try_from(tag).map_err(|_| NativeRunError::InvalidVariant(tag))?;
            let declared = variants
                .get(variant as usize)
                .ok_or(NativeRunError::InvalidVariant(tag))?;
            let payload = match declared {
                Some(layout) => {
                    let slice = words
                        .get(1..1 + layout.words() as usize)
                        .ok_or(NativeRunError::InvalidActivation)?;
                    Some(Box::new(decode_layout(layout, slice, heap)?))
                }
                None => None,
            };
            Ok(RecoveredValue::Enum { variant, payload })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRunError {
    InvalidVariant(u64),
    Guest(RunError),
    WordWidth,
    InvalidHome(Register),
    InvalidBoolean(u64),
    InvalidActivation,
    InvalidOutcome(u64),
    InvalidBudget,
    InvalidPc(u64),
    InvalidEdge,
    InvalidCallee(InstructionId),
    InvalidFailure(u64),
    Recovery(ScalarRecoveryError),
}

impl fmt::Display for NativeRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Guest(error) => error.fmt(f),
            Self::Recovery(error) => error.fmt(f),
            other => write!(f, "invalid native execution: {other:?}"),
        }
    }
}

impl std::error::Error for NativeRunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Guest(error) => Some(error),
            Self::Recovery(error) => Some(error),
            _ => None,
        }
    }
}
