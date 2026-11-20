//! A session over the verified interpreter, with the surface of the older
//! [`crate::runtime::Session`] for what the verified engine can answer: build
//! from a program, run for a budget, read the status and the output, feed
//! input. Engine internals the older session reported (inline-cache and
//! collector statistics, tiering reports) have no counterpart.
pub mod repl;
pub use crate::runtime::{Context, Status};
use crate::{
    Diagnostic, Span,
    backend::verified::{CodeStore, Dispatch, ExecutionMode, NativeRunError},
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

fn lower(program: Arc<Program>) -> Result<VerifiedModule, Diagnostic> {
    VerifiedModule::lower_shared(program, 0)
        .map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))
}

/// Start a verified batch run, preferring published native code when available.
/// The context supplies the frame limit, GC stress setting and random seed.
/// The caller can configure input on the dispatch before the first slice.
pub fn start_jit(program: Arc<Program>, context: &Context) -> Result<Dispatch, Diagnostic> {
    let module = lower(program)?;
    let limits = ExecutionLimits {
        fuel: TOTAL_FUEL,
        frames: context.max_frames,
    };
    let started = match CodeStore::new(module.clone()) {
        Ok(mut store) => {
            store.compile_available();
            store.start(&[], limits, ExecutionMode::PreferNative)
        }
        Err(_) => module
            .start(&[], limits)
            .map(|activation| Dispatch::Interpreter(Box::new(activation)))
            .map_err(NativeRunError::Guest),
    };
    let mut dispatch =
        started.map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))?;
    match &mut dispatch {
        Dispatch::Native(activation) => {
            activation.set_gc_stress(context.gc.stress);
            activation.set_random_seed(context.random_seed);
        }
        Dispatch::Interpreter(activation) => {
            activation.set_gc_stress(context.gc.stress);
            activation.set_random_seed(context.random_seed);
        }
    }
    Ok(dispatch)
}

pub struct Session {
    /// Input queued for the program, and the output it has written so far.
    pub context: Context,
    program: Arc<Program>,
    module: VerifiedModule,
    activation: Activation,
    status: Status,
    written: Arc<Mutex<String>>,
    queued: Arc<Mutex<VecDeque<String>>>,
    /// Which of the statements that begin at the next instruction a debugger
    /// is stopped at: statements without code share an instruction with the
    /// next one that has code, and each is a stop of its own.
    cursor: usize,
}

impl Session {
    pub fn new(program: Arc<Program>) -> Result<Self, Diagnostic> {
        Self::with_context(program, Context::default())
    }

    pub fn with_context(program: Arc<Program>, context: Context) -> Result<Self, Diagnostic> {
        let module = lower(program.clone())?;
        Self::from_module(program, module, context)
    }

    /// A session that can be continued with more code: every variable of the
    /// entry routine is kept outside its frame.
    pub fn persistent(program: Arc<Program>, context: Context) -> Result<Self, Diagnostic> {
        let module = VerifiedModule::lower_for_session(program.clone(), 0)
            .map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))?;
        Self::from_module(program, module, context)
    }

    fn from_module(
        program: Arc<Program>,
        module: VerifiedModule,
        context: Context,
    ) -> Result<Self, Diagnostic> {
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
            cursor: 0,
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
        let activation = module
            .start(&[], limits)
            .map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))?;
        Ok(Self::attach(activation, context, written, queued))
    }

    /// Give an activation the context's collection, seed, output and input.
    fn attach(
        mut activation: Activation,
        context: &Context,
        written: &Arc<Mutex<String>>,
        queued: &Arc<Mutex<VecDeque<String>>>,
    ) -> Activation {
        activation.set_gc_stress(context.gc.stress);
        activation.set_random_seed(context.random_seed);
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
        activation
    }

    /// Replace the code with a longer program and run its new statements, those
    /// that start at or after `committed` in the source, with the variables
    /// and everything they refer to kept. The session must be
    /// [persistent](Self::persistent).
    pub fn continue_with(
        &mut self,
        program: Arc<Program>,
        committed: u32,
    ) -> Result<(), Diagnostic> {
        let refuse = |message: String| Diagnostic::new("E204", message, Span::default());
        let next = VerifiedModule::lower_for_session(program.clone(), 0)
            .map_err(|error| Diagnostic::new("R900", error.to_string(), Span::default()))?;
        self.module
            .is_extended_by_with(&next, true)
            .map_err(|reason| {
                refuse(format!(
                    "this entry would change code that already ran: {reason}"
                ))
            })?;
        let at = next
            .entry_statement_from(committed)
            .unwrap_or_else(|| next.entry_end());
        let limits = ExecutionLimits {
            fuel: TOTAL_FUEL,
            frames: self.context.max_frames,
        };
        let counted = self.activation.instructions_executed();
        let idle = next
            .start(&[], limits)
            .map_err(|error| refuse(error.to_string()))?;
        let memory = std::mem::replace(&mut self.activation, idle).into_memory();
        let activation = next
            .continue_at(memory, at, limits, counted)
            .map_err(|error| refuse(error.to_string()))?;
        self.activation = Self::attach(activation, &self.context, &self.written, &self.queued);
        self.module = next;
        self.program = program;
        self.status = Status::Paused;
        self.cursor = 0;
        Ok(())
    }

    /// Abandon the running computation, keeping what it already wrote to
    /// variables, so another entry may be submitted.
    pub fn interrupt(&mut self) {
        let span = self.current_span().unwrap_or_default();
        self.status = Status::Failed(RuntimeFailure::new(
            RuntimeError::Limit(ResourceLimit::Instructions),
            span,
        ));
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
            (_, ExecutionStatus::Paused(position)) => Some(
                self.activation
                    .statement_boundaries()
                    .nth(self.cursor)
                    .map_or(position.span, |statement| statement.span),
            ),
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
        self.cursor = 0;
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
            ExecutionStatus::Failed(error) => Status::Failed(error.into()),
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

    /// Whether the next instruction begins a source statement.
    pub fn at_statement(&self) -> bool {
        self.activation
            .statement_boundaries()
            .nth(self.cursor)
            .is_some()
    }

    /// The names of the routines on the stack, outermost first.
    pub fn stack(&self) -> Vec<String> {
        self.activation
            .call_stack()
            .into_iter()
            .map(|function| {
                self.module
                    .routine_of(function)
                    .and_then(|routine| self.program.functions().get(routine))
                    .map_or_else(|| "?".to_owned(), |routine| routine.name.to_string())
            })
            .collect()
    }

    /// The source variables of the innermost frame, with their values.
    pub fn variables(&self) -> Vec<(String, String)> {
        self.activation.frame_variables()
    }

    /// The module-level variables, which outlive the entry routine's frame.
    pub fn globals(&self) -> Vec<(String, String)> {
        self.activation.module_variables()
    }

    /// Run one instruction. `true` while the program can go on.
    pub fn step(&mut self) -> bool {
        // Passing a statement that has no code of its own runs nothing.
        if self
            .activation
            .statement_boundaries()
            .nth(self.cursor + 1)
            .is_some()
        {
            self.cursor += 1;
            return true;
        }
        matches!(self.run(1), Status::Paused)
    }

    /// Start again from the entry routine; queued input and output are kept.
    pub fn restart(&mut self) -> Result<(), Diagnostic> {
        self.activation = Self::start(&self.module, &self.context, &self.written, &self.queued)?;
        self.context.output.clear();
        self.status = Status::Paused;
        self.cursor = 0;
        Ok(())
    }
}

impl From<RunError> for RuntimeFailure {
    fn from(error: RunError) -> Self {
        Self::new(translate(error), error.span().unwrap_or_default())
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
        RunError::RandomBound { .. } => RuntimeError::Arithmetic(ArithmeticError::RandomBound),
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
