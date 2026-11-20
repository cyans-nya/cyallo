//! Fuel-bounded native execution and transfer to the verified interpreter.

use super::SliceKind;
use super::{BridgeCall, Layout};
use super::{
    CodeStore, EntryPin, Failure, NativeHome, NativeTier, Operation, Origin, RecoverySlot,
    ScalarOutcome,
    abi::{Edge, ScalarContext, scalar_word},
    platform::Invocation,
};
use crate::bytecode::verified::{
    Activation, ExecutionLimits, ExecutionPosition, ExecutionStatus, InstructionId, RecoveredValue,
    Register, RunError, Scalar, ScalarFrameSnapshot, ScalarRecoveryError, ScalarType,
    StatementArrival,
};
use crate::bytecode::verified::{OutputSink, TextFailure, TextMapOp, TextOp, TextTestOp};
use std::fmt;
use std::sync::Arc;

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
        let parameters = self.plan().arguments();
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
        Ok(NativeActivation {
            texts: Vec::new(),
            output: OutputSink::default(),
            state: State::Active(vec![frame]),
            limits,
            spent: 0,
            last_entry: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryEvent {
    pub origin: Origin,
    pub tier: NativeTier,
}

#[derive(Debug)]
pub struct NativeActivation {
    /// Strings held by native frames, which carry indices into this table.
    texts: Vec<Arc<str>>,
    output: OutputSink,
    state: State,
    limits: ExecutionLimits,
    spent: usize,
    last_entry: Option<EntryEvent>,
}

#[derive(Debug)]
enum State {
    Active(Vec<Frame>),
    Terminal(ExecutionStatus),
    Broken(NativeRunError),
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

    fn read(&self, home: NativeHome, texts: &[Arc<str>]) -> Result<RecoveredValue, NativeRunError> {
        let Some(id) = home.layout else {
            return self.load(home).map(RecoveredValue::Scalar);
        };
        let layout = self
            .entry
            .plan()
            .layout(id)
            .ok_or(NativeRunError::InvalidHome(home.register))?;
        decode_layout(layout, self.words(home)?, texts)
    }

    fn snapshot(
        &self,
        stack: &[Frame],
        index: usize,
        texts: &[Arc<str>],
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
                let value = match self.read(home, texts)? {
                    RecoveredValue::Loan { .. } => recover_loan(stack, index, home)?,
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
    /// Where the program's output goes. Set before the first slice.
    pub fn set_output(&mut self, sink: OutputSink) {
        self.output = sink;
    }

    pub fn instructions_executed(&self) -> usize {
        self.spent
    }

    pub fn last_entry(&self) -> Option<EntryEvent> {
        self.last_entry
    }

    pub fn status(&self) -> Result<ExecutionStatus, NativeRunError> {
        match &self.state {
            State::Terminal(status) => Ok(*status),
            State::Broken(error) => Err(*error),
            State::Active(stack) => {
                let frame = stack.last().ok_or(NativeRunError::InvalidActivation)?;
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
        let State::Active(stack) = &mut self.state else {
            return self.status();
        };
        let mut available = budget.min(self.limits.fuel - self.spent);
        while available != 0 {
            match step(
                stack,
                &mut Runtime {
                    texts: &mut self.texts,
                    output: &mut self.output,
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
        let State::Active(stack) = self.state else {
            return Err(NativeRunError::InvalidActivation);
        };
        let root = stack.first().ok_or(NativeRunError::InvalidActivation)?;
        let snapshots = (0..stack.len())
            .map(|index| stack[index].snapshot(&stack, index, &self.texts))
            .collect::<Result<Vec<_>, _>>()?;
        let mut activation = root
            .entry
            .module()
            .restore_scalars(snapshots, self.limits, self.spent)
            .map_err(NativeRunError::Recovery)?;
        activation.set_output(self.output);
        Ok(activation)
    }
}

enum Step {
    Continue,
    Terminal(ExecutionStatus),
}

/// What native code reaches through bridges.
struct Runtime<'a> {
    texts: &'a mut Vec<Arc<str>>,
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
    let input_budget = u64::try_from(*available).map_err(|_| NativeRunError::WordWidth)?;
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
        slots: &mut frame.slots,
        links: pin.links(),
        result: &mut result,
    });
    *entry_event = Some(EntryEvent {
        origin: entry_site.origin,
        tier: pin.tier(),
    });
    let used = input_budget
        .checked_sub(context.budget)
        .filter(|used| *used != 0)
        .ok_or(NativeRunError::InvalidBudget)?;
    let used = usize::try_from(used).map_err(|_| NativeRunError::WordWidth)?;
    *available -= used;
    *spent += used;
    let pc = InstructionId(
        u32::try_from(context.next).map_err(|_| NativeRunError::InvalidPc(context.next))?,
    );
    let site = plan
        .site(pc)
        .ok_or(NativeRunError::InvalidPc(context.next))?;
    match outcome(raw)? {
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
            if let Some(error) = perform(bridge, frame, runtime, site.origin)? {
                return Ok(Step::Terminal(ExecutionStatus::Failed(error)));
            }
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
                    let words = frame.words(argument.caller)?.to_vec();
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
            let failure = site
                .failures
                .iter()
                .copied()
                .find(|failure| *failure as u64 == context.failure)
                .ok_or(NativeRunError::InvalidFailure(context.failure))?;
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
            return Ok(Step::Terminal(ExecutionStatus::Failed(error)));
        }
    }
    Ok(Step::Continue)
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

/// Run a bridge call for the frame. `Some` is a failure of the program.
fn perform(
    bridge: &BridgeCall,
    frame: &mut Frame,
    runtime: &mut Runtime<'_>,
    origin: Origin,
) -> Result<Option<RunError>, NativeRunError> {
    let intern = |runtime: &mut Runtime<'_>, text: Arc<str>| -> u64 {
        runtime.texts.push(text);
        (runtime.texts.len() - 1) as u64
    };
    match bridge {
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
                frame.text(*left, runtime.texts)?,
                frame.text(*right, runtime.texts)?
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
                .text(*left, runtime.texts)?
                .cmp(&frame.text(*right, runtime.texts)?);
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
            let length = frame.text(*source, runtime.texts)?.chars().count();
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
            let text = frame.text(*source, runtime.texts)?;
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
            let text = frame.text(*source, runtime.texts)?;
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
                frame.text(*text, runtime.texts)?,
                frame.text(*other, runtime.texts)?,
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
            let replaced = frame.text(*text, runtime.texts)?.replace(
                &*frame.text(*from, runtime.texts)?,
                &frame.text(*to, runtime.texts)?,
            );
            let word = intern(runtime, replaced.into());
            frame.put(*destination, word)?;
        }
        BridgeCall::ToText {
            destination,
            source,
        } => {
            let value = frame.read(*source, runtime.texts)?;
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
                .map(|&home| frame.read(home, runtime.texts))
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
) -> Result<RecoveredValue, NativeRunError> {
    use super::{RecoverySlot, prepare::layout_of};
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
    let base = holder.slots.as_ptr() as u64 + u64::from(owner_home.offset.0);
    let delta = pointer.checked_sub(base).ok_or(invalid)?;
    if delta % 8 != 0 {
        return Err(invalid);
    }
    let owner_layout = match owner_home.layout {
        Some(id) => plan.layout(id).ok_or(invalid)?.clone(),
        None => Layout::Scalar(owner_home.ty),
    };
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
    let target = layout_of(body, pointee).ok_or(invalid)?;
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
    texts: &[Arc<str>],
) -> Result<RecoveredValue, NativeRunError> {
    match layout {
        Layout::Text => {
            let word = *words.first().ok_or(NativeRunError::InvalidActivation)?;
            usize::try_from(word)
                .ok()
                .and_then(|index| texts.get(index))
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
                values.push(decode_layout(field, slice, texts)?);
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
                    Some(Box::new(decode_layout(layout, slice, texts)?))
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
