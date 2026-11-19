//! Fuel-bounded native execution and transfer to the verified interpreter.

use super::{
    CodeStore, EntryPin, Failure, NativeHome, NativeTier, Operation, Origin, RecoverySlot,
    ScalarOutcome,
    abi::{Edge, ScalarContext, scalar_word},
    platform::Invocation,
};
use crate::bytecode::verified::{
    Activation, ExecutionLimits, ExecutionPosition, ExecutionStatus, InstructionId, Register,
    RunError, Scalar, ScalarFrameSnapshot, ScalarRecoveryError, ScalarType, StatementArrival,
};
use std::fmt;

const ENTRY: InstructionId = InstructionId(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    PreferNative,
    InterpreterOnly,
}

#[derive(Debug)]
pub enum Dispatch {
    Native(NativeActivation),
    Interpreter(Activation),
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
            .map(Dispatch::Interpreter)
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
        if home.ty != value.ty() {
            return Err(NativeRunError::InvalidHome(home.register));
        }
        let index = self.slot(home)?;
        self.slots[index] = scalar_word(value);
        Ok(())
    }

    fn load(&self, home: NativeHome) -> Result<Scalar, NativeRunError> {
        decode(home.ty, self.slots[self.slot(home)?])
    }

    fn snapshot(&self) -> Result<ScalarFrameSnapshot, NativeRunError> {
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
                values.push((home.register, self.load(home)?));
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
        let snapshots = stack
            .iter()
            .map(Frame::snapshot)
            .collect::<Result<Vec<_>, _>>()?;
        root.entry
            .module()
            .restore_scalars(snapshots, self.limits, self.spent)
            .map_err(NativeRunError::Recovery)
    }
}

enum Step {
    Continue,
    Terminal(ExecutionStatus),
}

fn step(
    stack: &mut Vec<Frame>,
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
                callee.store(argument.callee, frame.load(argument.caller)?)?;
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
            let destination = frame.destination;
            stack.pop();
            let Some(caller) = stack.last_mut() else {
                return Ok(Step::Terminal(ExecutionStatus::Returned(value)));
            };
            let Position::Waiting { call, successor } = caller.position else {
                return Err(NativeRunError::InvalidActivation);
            };
            if let Some(home) = destination {
                caller.store(
                    home,
                    value.ok_or(NativeRunError::InvalidHome(home.register))?,
                )?;
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

fn decode(ty: ScalarType, word: u64) -> Result<Scalar, NativeRunError> {
    match (ty, word) {
        (ScalarType::I64, word) => Ok(Scalar::I64(i64::from_ne_bytes(word.to_ne_bytes()))),
        (ScalarType::F64, word) => Ok(Scalar::F64(word)),
        (ScalarType::Bool, 0) => Ok(Scalar::Bool(false)),
        (ScalarType::Bool, 1) => Ok(Scalar::Bool(true)),
        (ScalarType::Bool, word) => Err(NativeRunError::InvalidBoolean(word)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRunError {
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
