//! A session over the verified interpreter, with the surface of the older
//! [`crate::runtime::Session`] for what the verified engine can answer: build
//! from a program, run for a budget, read the status and the output, feed
//! input. Engine internals the older session reported (inline-cache and
//! collector statistics, tiering reports) have no counterpart.
pub use crate::runtime::{Context, Status};
use crate::{
    Diagnostic, Span,
    bytecode::verified::{
        Activation, ExecutionLimits, ExecutionStatus, GeneratorFault, InputSource, OutputSink,
        RunError, VerifiedModule,
    },
    ir::Program,
    runtime::{
        ArithmeticError, GeneratorError, InputError, ResourceLimit, RuntimeError, RuntimeFailure,
        VerifiedFailure,
    },
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

/// Instructions one session may run in total.
const TOTAL_FUEL: usize = usize::MAX;

pub struct Session {
    /// Input queued for the program, and the output it has written so far.
    pub context: Context,
    program: Arc<Program>,
    module: VerifiedModule,
    activation: Activation,
    status: Status,
    written: Arc<Mutex<String>>,
    queued: Arc<Mutex<VecDeque<String>>>,
}

impl Session {
    pub fn new(program: Arc<Program>) -> Result<Self, Diagnostic> {
        Self::with_context(program, Context::default())
    }

    pub fn with_context(program: Arc<Program>, context: Context) -> Result<Self, Diagnostic> {
        let module = VerifiedModule::lower_shared(program.clone(), 0)
            .map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))?;
        let written = Arc::new(Mutex::new(String::new()));
        let queued = Arc::new(Mutex::new(VecDeque::new()));
        let activation = Self::start(&module, &context, &written, &queued)?;
        Ok(Self {
            context,
            program,
            module,
            activation,
            status: Status::Paused,
            written,
            queued,
        })
    }

    fn start(
        module: &VerifiedModule,
        context: &Context,
        written: &Arc<Mutex<String>>,
        queued: &Arc<Mutex<VecDeque<String>>>,
    ) -> Result<Activation, Diagnostic> {
        let limits = ExecutionLimits {
            fuel: TOTAL_FUEL,
            frames: context.max_frames,
        };
        let mut activation = module
            .start(&[], limits)
            .map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))?;
        activation.set_gc_stress(context.gc.stress);
        let written = written.clone();
        activation.set_output(OutputSink::new(move |line| {
            let mut text = written.lock().unwrap();
            text.push_str(line);
            text.push('\n');
        }));
        let queued = queued.clone();
        activation.set_input(InputSource::waiting(move || {
            queued.lock().unwrap().pop_front()
        }));
        Ok(activation)
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn program(&self) -> &Arc<Program> {
        &self.program
    }

    /// Frames on the running stack.
    pub fn depth(&self) -> usize {
        self.activation.depth()
    }

    /// Instructions executed so far.
    pub fn instruction_count(&self) -> u64 {
        self.activation.instructions_executed() as u64
    }

    /// The source span of the next instruction, or of the failure.
    pub fn current_span(&self) -> Option<Span> {
        match (&self.status, self.activation.status()) {
            (Status::Failed(failure), _) => Some(failure.span),
            (_, ExecutionStatus::Paused(position)) => Some(position.span),
            _ => None,
        }
    }

    /// Run at most `budget` instructions. Input queued in the context is handed
    /// to the program, and what it wrote is appended to the context's output.
    pub fn run(&mut self, budget: usize) -> &Status {
        if matches!(self.status, Status::Finished | Status::Failed(_)) {
            return &self.status;
        }
        {
            let mut queue = self.queued.lock().unwrap();
            while let Some(line) = self.context.input.pop_front() {
                queue.push_back(line);
            }
        }
        let outcome = self.activation.resume(budget);
        self.context
            .output
            .push_str(&std::mem::take(&mut *self.written.lock().unwrap()));
        self.status = match outcome {
            ExecutionStatus::Paused(_) if self.activation.awaiting_input() => {
                Status::WaitingForInput
            }
            ExecutionStatus::Paused(_) => Status::Paused,
            ExecutionStatus::Returned(_) => Status::Finished,
            ExecutionStatus::Failed(error) => Status::Failed(RuntimeFailure::new(
                translate(error),
                error.span().unwrap_or_default(),
            )),
        };
        &self.status
    }

    /// No more input will come: a program that waits for a line fails with the
    /// end of input instead.
    pub fn end_input(&mut self) {
        self.activation.end_input();
        if self.status == Status::WaitingForInput {
            self.status = Status::Paused;
        }
    }

    /// Run one instruction. `true` while the program can go on.
    pub fn step(&mut self) -> bool {
        matches!(self.run(1), Status::Paused)
    }

    /// Start again from the entry routine; queued input and output are kept.
    pub fn restart(&mut self) -> Result<(), Diagnostic> {
        self.activation = Self::start(&self.module, &self.context, &self.written, &self.queued)?;
        self.status = Status::Paused;
        Ok(())
    }
}

/// The failure a verified error is reported as: the older error where there is
/// one, otherwise the verified message under a code of its family.
fn translate(error: RunError) -> RuntimeError {
    match error {
        RunError::IntegerOverflow { .. } => {
            RuntimeError::Arithmetic(ArithmeticError::IntegerOverflow)
        }
        RunError::DivisionByZero { .. } => {
            RuntimeError::Arithmetic(ArithmeticError::DivisionByZero)
        }
        RunError::ZeroStep { .. } => RuntimeError::Arithmetic(ArithmeticError::ZeroStep),
        RunError::InputExhausted { .. } => RuntimeError::Input(InputError::EndOfInput),
        RunError::FrameLimit => RuntimeError::Limit(ResourceLimit::CallDepth),
        RunError::Generator { fault, .. } => RuntimeError::Generator(match fault {
            GeneratorFault::Finished => GeneratorError::Finished,
            GeneratorFault::AlreadyRunning => GeneratorError::AlreadyRunning,
            GeneratorFault::NotStarted => GeneratorError::NotStarted,
            GeneratorFault::AlreadyStarted => GeneratorError::AlreadyStarted,
            GeneratorFault::LimitExceeded => GeneratorError::LimitExceeded,
        }),
        other => RuntimeError::Verified(VerifiedFailure {
            code: match other {
                RunError::InputInvalid { .. } => "R140",
                RunError::IndexOutOfBounds { .. } | RunError::Path { .. } => "R110",
                RunError::Text { .. } => "R130",
                RunError::Deadlock { .. } | RunError::NotTransferable { .. } => "R230",
                RunError::Service { .. } => "R180",
                RunError::AllocationRejected { .. } => "R200",
                RunError::HeapExhausted { .. }
                | RunError::TaskLimit { .. }
                | RunError::ChannelLimit { .. } => "R210",
                RunError::Object { .. } => "R240",
                _ => "R900",
            },
            message: other.to_string().into(),
        }),
    }
}
