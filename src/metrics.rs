//! Opt-in execution observation. Durations are wall time, not CPU time.
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ExecutionEngine {
    Interpreter = 0,
    Winch = 1,
    Cranelift = 2,
    CraneliftSpeed = 3,
}
impl ExecutionEngine {
    pub const ALL: [Self; 4] = [
        Self::Interpreter,
        Self::Winch,
        Self::Cranelift,
        Self::CraneliftSpeed,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Interpreter => "interpreter",
            Self::Winch => "winch",
            Self::Cranelift => "cranelift",
            Self::CraneliftSpeed => "cranelift_speed",
        }
    }
    pub(crate) fn from_native(value: u64) -> Self {
        match value {
            1 => Self::Winch,
            2 => Self::Cranelift,
            3 => Self::CraneliftSpeed,
            _ => unreachable!("validated native engine"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FunctionMetrics {
    pub instructions: [u64; 4],
    pub execution_time: [Duration; 4],
    pub invocations: u64,
    pub osr_transitions: u64,
}
#[derive(Debug, Clone)]
pub struct ExecutionEvent {
    pub code_version: u64,
    pub elapsed: Duration,
    pub function: usize,
    pub invocation: u64,
    pub pc: usize,
    pub depth: usize,
    pub from: Option<ExecutionEngine>,
    pub to: ExecutionEngine,
    /// A new highest tier after this invocation has executed work. Resuming an
    /// already visited tier is not another promotion.
    pub osr: bool,
}
#[derive(Debug, Clone)]
pub struct ExecutionReport {
    /// GC pause total when this report began (Session setup is excluded).
    pub gc_pause_at_start: Duration,
    pub deferred_bodies: u64,
    pub body_analysis_time: Duration,
    /// Native yields (return,budget,notification,OSR,hot,error,continuation).
    pub native_exits: [u64; 7],
    pub boundaries: RuntimeBoundaries,
    pub native_prepare_time: Duration,
    pub native_materialize_time: Duration,
    pub first_call_admissions: u64,
    pub first_call_wait: Duration,
    pub functions: Vec<FunctionMetrics>,
    pub events: Vec<ExecutionEvent>,
    pub dropped_events: u64,
    pub peak_frames: usize,
    pub peak_cells: usize,
    origin: Instant,
    next_invocation: u64,
    active: Option<(Instant, usize, ExecutionEngine)>,
}
#[derive(Debug)]
pub(crate) struct FrameMetrics {
    pub highest: Option<ExecutionEngine>,
    pub invocation: u64,
    pub engine: Option<ExecutionEngine>,
    pub instructions: u64,
}
impl ExecutionReport {
    pub(crate) fn new(origin: Instant, functions: usize, frames: usize, cells: usize) -> Self {
        Self {
            gc_pause_at_start: Duration::ZERO,
            deferred_bodies: 0,
            body_analysis_time: Duration::ZERO,
            native_exits: [0; 7],
            boundaries: RuntimeBoundaries::default(),
            native_prepare_time: Duration::ZERO,
            native_materialize_time: Duration::ZERO,
            first_call_admissions: 0,
            first_call_wait: Duration::ZERO,
            functions: vec![FunctionMetrics::default(); functions],
            events: vec![],
            dropped_events: 0,
            peak_frames: frames,
            peak_cells: cells,
            origin,
            next_invocation: 0,
            active: None,
        }
    }
    pub(crate) fn frame(&mut self, function: usize) -> FrameMetrics {
        self.next_invocation += 1;
        self.functions[function].invocations += 1;
        FrameMetrics {
            highest: None,
            invocation: self.next_invocation,
            engine: None,
            instructions: 0,
        }
    }
    pub(crate) fn enter(
        &mut self,
        frame: &mut FrameMetrics,
        function: usize,
        pc: usize,
        depth: usize,
        engine: ExecutionEngine,
        code_version: u64,
    ) {
        if self
            .active
            .is_none_or(|(_, f, e)| f != function || e != engine)
        {
            self.pause();
            self.active = Some((Instant::now(), function, engine));
        }
        if frame.engine == Some(engine) {
            return;
        }
        let osr = frame.instructions != 0 && frame.highest.is_some_and(|from| engine > from);
        if osr {
            self.functions[function].osr_transitions += 1;
        }
        if self.events.len() < 4096 {
            self.events.push(ExecutionEvent {
                code_version,
                elapsed: self.origin.elapsed(),
                function,
                invocation: frame.invocation,
                pc,
                depth,
                from: frame.engine,
                to: engine,
                osr,
            });
        } else {
            self.dropped_events += 1;
        }
        frame.engine = Some(engine);
        frame.highest = Some(
            frame
                .highest
                .map_or(engine, |previous| previous.max(engine)),
        );
    }
    pub(crate) fn pause(&mut self) {
        if let Some((start, function, engine)) = self.active.take() {
            self.functions[function].execution_time[engine as usize] += start.elapsed();
        }
    }
    pub fn instruction_count(&self) -> u64 {
        self.functions.iter().flat_map(|f| f.instructions).sum()
    }
}
#[derive(Debug, Clone, Default)]
pub struct RuntimeBoundaries {
    pub step: u64,
    /// Native return synchronization attempts, including already-clean returns.
    pub flush: u64,
    pub flush_skipped: u64,
    pub state: u64,
    pub get: u64,
    pub put: u64,
}
