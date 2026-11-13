//! Compilation runs on a bounded worker pool, with a shared first-call wait budget.
use super::{
    CompileError, Executable, NativeBackend,
    cranelift_backend::{CraneliftUnit, compile_unit},
    winch::WinchUnit,
};
use crate::{
    ir::{Program, RoutineShape},
    runtime::{EngineError, Session, Status, native::ExitReason},
};
mod scheduler;
use scheduler::{Mailbox, Request, Slots, service};
use std::{
    cell::RefCell,
    sync::{Arc, OnceLock, Weak},
    thread::{self, ThreadId},
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Winch,
    CraneliftFast,
    CraneliftSpeed,
}
pub fn compiler_worker_count() -> usize {
    service().workers
}
#[derive(Debug, Clone, Copy)]
pub struct TierPolicy {
    pub eager: super::EagerPolicy,
    pub small_limit: usize,
    pub heavy_limit: usize,
    /// Executed instructions a routine must accumulate before fast compilation.
    pub warm_instructions: u64,
    /// Calls after which a routine requests baseline (Winch) code. Winch is cheap
    /// enough to install almost immediately; this keeps cold, one-shot routines
    /// interpreted while making repeated calls native within a few entries.
    pub winch_calls: u64,
    /// Work after which Winch is requested regardless of call count. Covers the
    /// entry body, which is entered once but can still run a long time.
    pub winch_instructions: u64,
    /// Executed instructions before a loop-shaped routine may use speed codegen.
    pub hot_instructions: u64,
    /// Speed codegen compile time grows sharply with body size; skip it beyond
    /// this bytecode length even when the routine is hot.
    pub speed_size_limit: usize,
    /// Legacy size-based Winch cutoff. Automatic tiering does not select Winch
    /// unless this is enabled.
    pub use_winch: bool,
    pub execution_quantum: usize,
    /// Total wait budget shared by all first-call admissions in this artifact.
    pub first_call_wait: Duration,
}
impl Default for TierPolicy {
    fn default() -> Self {
        Self {
            eager: Default::default(),
            small_limit: 64,
            heavy_limit: 512,
            warm_instructions: std::env::var("CAIE_WARM_INSTRUCTIONS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(5_000),
            winch_calls: std::env::var("CAIE_WINCH_CALLS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(8),
            winch_instructions: std::env::var("CAIE_WINCH_INSTRUCTIONS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2_000),
            // Speed codegen costs milliseconds. Only amortize it against a
            // genuinely hot, loop-shaped routine.
            hot_instructions: 8_000_000,
            speed_size_limit: 512,
            use_winch: true,
            execution_quantum: 1024,
            first_call_wait: Duration::from_millis(1),
        }
    }
}
impl TierPolicy {
    fn hinted(self, hint: cyallo_ast::HeatHint) -> Self {
        let scale = |n: u64| match hint {
            cyallo_ast::HeatHint::Auto => n,
            cyallo_ast::HeatHint::Hot => {
                if n == 0 {
                    0
                } else {
                    (n / 2).max(1)
                }
            }
            cyallo_ast::HeatHint::Cold => n.saturating_mul(4),
        };
        Self {
            warm_instructions: scale(self.warm_instructions),
            hot_instructions: scale(self.hot_instructions),
            winch_calls: scale(self.winch_calls),
            winch_instructions: scale(self.winch_instructions),
            ..self
        }
    }
    /// Size-based request used by explicit precompilation and legacy callers.
    pub fn select(&self, size: usize, work: u64) -> Tier {
        if size >= self.heavy_limit || work >= self.hot_instructions {
            Tier::CraneliftSpeed
        } else if size <= self.small_limit {
            Tier::Winch
        } else {
            Tier::CraneliftFast
        }
    }
    /// Work-based request used by automatic tiering. `None` keeps the routine
    /// interpreted. Winch comes first: it compiles in well under a millisecond,
    /// so it is requested as soon as the routine either has been called
    /// `winch_calls` times or has run `winch_instructions` instructions. Both
    /// apply to the entry body, which is entered once but can run for a long
    /// time. Fast has a separate work threshold, and speed is reserved for hot
    /// loop-only bodies. These are admission heuristics; installation is async
    /// and compile cost need not be recovered by every short invocation.
    pub fn promote(&self, shape: RoutineShape, size: usize, work: u64, calls: u64) -> Option<Tier> {
        if shape.loops
            && !shape.calls
            && work >= self.hot_instructions
            && size <= self.speed_size_limit
        {
            return Some(Tier::CraneliftSpeed);
        }
        if work >= self.warm_instructions {
            return Some(Tier::CraneliftFast);
        }
        if self.use_winch && (calls >= self.winch_calls || work >= self.winch_instructions) {
            return Some(Tier::Winch);
        }
        None
    }
}
#[derive(Debug, Clone)]
pub struct TierInfo {
    pub function: usize,
    pub tier: Option<Tier>,
    pub requested: Option<Tier>,
    pub compiling: bool,
    pub compilation_time: Option<Duration>,
    pub compiler_thread: Option<ThreadId>,
    pub fallback_reason: Option<CompileError>,
    pub error: Option<CompileError>,
}
#[derive(Debug, Clone)]
pub enum TierEventKind {
    Requested {
        tier: Tier,
        executed: Option<u64>,
    },
    CompileStarted {
        tier: Tier,
        worker: ThreadId,
        queue_time: Duration,
    },
    CompileFinished {
        tier: Tier,
        duration: Duration,
        fallback: Option<CompileError>,
    },
    Superseded {
        previous: Tier,
        tier: Tier,
    },
    FirstCall {
        waited: Duration,
        compiled: bool,
    },
    Discarded {
        tier: Tier,
        installed: Tier,
    },
    Installed {
        previous: Option<Tier>,
        tier: Tier,
    },
    Failed {
        error: CompileError,
    },
}
#[derive(Debug, Clone)]
pub struct TierEvent {
    pub elapsed: Duration,
    pub function: usize,
    pub kind: TierEventKind,
}
#[derive(Default)]
pub struct TieredBackend {
    pub policy: TierPolicy,
}
pub struct TieredArtifact {
    program: Arc<Program>,
    policy: TierPolicy,
    state: RefCell<State>,
    _alive: Arc<()>,
}
struct State {
    eager_jobs: Vec<bool>,
    eager_words: usize,
    eager_cost: Duration,
    first_wait_spent: Duration,
    admitted: Vec<bool>,
    units: Vec<Option<Unit>>,
    lower: Vec<Option<Unit>>,
    transition_limits: Box<[u64]>,
    speed_eligible: Box<[bool]>,
    osr_targets: Box<[u8]>,
    hot_threshold: u64,
    warm_threshold: u64,
    heat: Box<[cyallo_ast::HeatHint]>,
    // Fixed slots are stable for the artifact lifetime. Only the execution
    // thread installs entries, between runs, while it owns all executable units.
    entries: Box<[usize]>,
    info: Vec<TierInfo>,
    mailbox: Arc<Mailbox>,
    seen: u64,
    requests: Vec<Arc<Request>>,
    origin: Option<Instant>,
    events: Vec<TierEvent>,
}
enum Unit {
    Winch(Box<WinchUnit>),
    Cranelift(Box<CraneliftUnit>),
}
enum WorkerMessage {
    Event {
        function: usize,
        at: Instant,
        kind: TierEventKind,
    },
    Finished(Finished),
}
struct Finished {
    function: usize,
    actual: Tier,
    result: Result<Unit, CompileError>,
    elapsed: Duration,
    thread: ThreadId,
    fallback: Option<CompileError>,
    completed: Instant,
}
impl State {
    fn receive(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Finished(finished) => self.install(finished),
            WorkerMessage::Event { function, at, kind } => {
                if let TierEventKind::CompileStarted { worker, .. } = &kind {
                    self.info[function].compiler_thread = Some(*worker);
                }
                if let Some(origin) = self.origin {
                    self.events.push(TierEvent {
                        elapsed: at.duration_since(origin),
                        function,
                        kind,
                    });
                }
            }
        }
    }
    fn install(&mut self, finished: Finished) {
        let previous = self.info[finished.function].tier;
        if let Some(origin) = self.origin {
            self.events.push(TierEvent {
                elapsed: finished.completed.duration_since(origin),
                function: finished.function,
                kind: TierEventKind::CompileFinished {
                    tier: finished.actual,
                    duration: finished.elapsed,
                    fallback: finished.fallback.clone(),
                },
            });
            self.events.push(TierEvent {
                elapsed: origin.elapsed(),
                function: finished.function,
                kind: match &finished.result {
                    Ok(_) if previous.is_some_and(|tier| tier > finished.actual) => {
                        TierEventKind::Discarded {
                            tier: finished.actual,
                            installed: previous.unwrap(),
                        }
                    }
                    Ok(_) => TierEventKind::Installed {
                        previous,
                        tier: finished.actual,
                    },
                    Err(error) => TierEventKind::Failed {
                        error: error.clone(),
                    },
                },
            });
        }
        let info = &mut self.info[finished.function];
        let (desired, queued, running) = self.requests[finished.function].status();
        info.compiling = queued.is_some() || running.is_some();
        info.requested = desired;
        info.compilation_time = Some(finished.elapsed);
        info.compiler_thread = Some(finished.thread);
        info.fallback_reason = finished.fallback;
        match finished.result {
            Ok(unit) if info.tier.is_none_or(|previous| finished.actual >= previous) => {
                self.entries[finished.function] = match &unit {
                    Unit::Cranelift(unit) => unit.entry_address(),
                    Unit::Winch(unit) => unit.entry_address(),
                };
                if finished.actual == Tier::CraneliftSpeed && previous != Some(Tier::CraneliftSpeed)
                {
                    self.lower[finished.function] = self.units[finished.function].take();
                }
                self.units[finished.function] = Some(unit);
                self.osr_targets[finished.function] =
                    u8::from(finished.actual == Tier::CraneliftSpeed);
                info.tier = Some(finished.actual);
                info.error = None;
            }
            Ok(_) => {}
            Err(error) => info.error = Some(error),
        }
    }
    fn poll(&mut self) {
        loop {
            for message in self.mailbox.drain(&mut self.seen) {
                self.receive(message);
            }
            for (fi, (info, request)) in self.info.iter_mut().zip(&self.requests).enumerate() {
                let (desired, queued, running) = request.status();
                info.requested = desired;
                info.compiling = queued.is_some() || running.is_some();
                // A native routine must notify the driver at its next *unrequested*
                // transition, including Winch -> fast. Desired tiers remain durable
                // after a failed job, so pending/failed requests cannot spin on the
                // same threshold. Installed tiers also cover a Winch -> fast fallback.
                let known = desired.max(info.tier);
                let scale = |n: u64| match self.heat[fi] {
                    cyallo_ast::HeatHint::Auto => n,
                    cyallo_ast::HeatHint::Hot => {
                        if n == 0 {
                            0
                        } else {
                            (n / 2).max(1)
                        }
                    }
                    cyallo_ast::HeatHint::Cold => n.saturating_mul(4),
                };
                let speed = if self.speed_eligible[fi] {
                    scale(self.hot_threshold)
                } else {
                    u64::MAX
                };
                self.transition_limits[fi] = match known {
                    None | Some(Tier::Winch) => scale(self.warm_threshold).min(speed),
                    Some(Tier::CraneliftFast) => speed,
                    Some(Tier::CraneliftSpeed) => u64::MAX,
                };
            }
            if self
                .mailbox
                .epoch
                .load(std::sync::atomic::Ordering::Acquire)
                == self.seen
            {
                break;
            }
        }
    }
}
impl NativeBackend for TieredBackend {
    type Artifact = TieredArtifact;
    fn compile(&self, program: Arc<Program>) -> Result<Self::Artifact, CompileError> {
        self.compile_observed(program, None)
    }
}
impl TieredBackend {
    /// Keep a timestamped history without printing on the execution path.
    pub fn compile_with_reporting(
        &self,
        program: Arc<Program>,
        origin: Instant,
    ) -> Result<TieredArtifact, CompileError> {
        self.compile_observed(program, Some(origin))
    }
    fn compile_observed(
        &self,
        program: Arc<Program>,
        origin: Option<Instant>,
    ) -> Result<TieredArtifact, CompileError> {
        if self.policy.execution_quantum == 0
            || self.policy.small_limit >= self.policy.heavy_limit
            || self.policy.eager.min_words >= self.policy.eager.max_words
            || self.policy.eager.forced_max_words == 0
        {
            return Err(CompileError::InvalidTierPolicy);
        }
        let mailbox = Mailbox::new();
        let alive = Arc::new(());
        let slots = Slots::new(program.codegen_jobs().workers());
        let requests = (0..program.functions.len())
            .map(|function| {
                Request::new(
                    program.clone(),
                    function,
                    mailbox.clone(),
                    Arc::downgrade(&alive),
                    origin.is_some(),
                    slots.clone(),
                )
            })
            .collect();
        let count = program.functions.len();
        let speed_eligible = program
            .functions
            .iter()
            .map(|function| function.shape.loops && !function.shape.calls)
            .collect();
        let artifact = TieredArtifact {
            program: program.clone(),
            policy: self.policy,
            _alive: alive,
            state: RefCell::new(State {
                eager_jobs: vec![false; count],
                eager_words: 0,
                eager_cost: Duration::ZERO,
                first_wait_spent: Duration::ZERO,
                heat: program
                    .functions
                    .iter()
                    .map(|f| f.attributes.heat)
                    .collect(),
                admitted: vec![false; count],
                units: (0..count).map(|_| None).collect(),
                lower: (0..count).map(|_| None).collect(),
                transition_limits: vec![u64::MAX; count].into_boxed_slice(),
                speed_eligible,
                osr_targets: vec![0; count].into_boxed_slice(),
                hot_threshold: self.policy.hot_instructions,
                warm_threshold: self.policy.warm_instructions,
                entries: vec![0; count].into_boxed_slice(),
                info: (0..count)
                    .map(|function| TierInfo {
                        function,
                        tier: None,
                        requested: None,
                        compiling: false,
                        compilation_time: None,
                        compiler_thread: None,
                        fallback_reason: None,
                        error: None,
                    })
                    .collect(),
                mailbox,
                seen: 0,
                requests,
                origin,
                events: vec![],
            }),
        };
        for fi in 1..artifact.program.functions.len() {
            if matches!(
                artifact.program.functions[fi].attributes.compilation,
                cyallo_ast::CompileHint::Eager | cyallo_ast::CompileHint::AlwaysEager
            ) {
                let _ = artifact.schedule_eager(fi, true, 1);
            }
        }
        artifact.schedule_upcoming(0);
        Ok(artifact)
    }
}
impl TieredArtifact {
    pub fn hint_call_soon(&self, function: usize) -> Result<super::EagerDecision, CompileError> {
        if function >= self.program.functions.len() {
            return Err(CompileError::UnknownFunction(function));
        }
        self.schedule_eager(function, true, 128)
    }
    fn schedule_upcoming(&self, root: usize) {
        if !self.policy.eager.enabled {
            return;
        }
        let mut queue = std::collections::VecDeque::from([(root, 0)]);
        let mut seen = std::collections::HashSet::new();
        while let Some((fi, depth)) = queue.pop_front() {
            if !seen.insert(fi) || seen.len() > 32 {
                continue;
            }
            let f = &self.program.functions[fi];
            let repeats = if f.shape.loops { 128 } else { 1 };
            if fi == 0 && f.shape.loops {
                let _ = self.schedule_eager(fi, false, 128);
            }
            if depth < 2 {
                for &to in f.direct_calls.iter().take(8) {
                    let _ = self.schedule_eager(to, false, repeats);
                    queue.push_back((to, depth + 1));
                }
            }
        }
    }
    fn schedule_eager(
        &self,
        function: usize,
        explicit: bool,
        repeats: u64,
    ) -> Result<super::EagerDecision, CompileError> {
        use cyallo_ast::{CompileHint, HeatHint};
        let routine = &self.program.functions[function];
        let options = self.policy.eager;
        let words = if routine.is_analyzed() {
            routine
                .checked()
                .map_or(routine.estimated_words, |f| f.code.len())
        } else {
            routine.estimated_words
        };
        let forced = routine.attributes.compilation == CompileHint::AlwaysEager;
        let decline = |reason| {
            self.program.notice(
                if forced || routine.attributes.compilation == CompileHint::Eager {
                    "unfulfilled"
                } else {
                    "eager"
                },
                function,
                reason,
            );
            super::EagerDecision::Deferred(reason)
        };
        if let Some(reason) = options.size_reason(words, routine.attributes.compilation) {
            return Ok(decline(reason));
        }
        let tier = if self.policy.use_winch {
            Tier::Winch
        } else {
            Tier::CraneliftFast
        };
        let cost = service().predict(tier, words);
        let saved = Duration::from_nanos(
            (words as u64)
                .saturating_mul(if routine.shape.loops { 128 } else { repeats })
                .saturating_mul(60),
        );
        let priority = explicit
            || routine.attributes.heat == HeatHint::Hot
            || matches!(
                routine.attributes.compilation,
                CompileHint::Eager | CompileHint::AlwaysEager
            );
        if !priority && (routine.attributes.heat == HeatHint::Cold || saved < cost) {
            return Ok(decline("predicted savings do not repay compilation"));
        }
        let mut state = self.state.borrow_mut();
        state.poll();
        if state.info[function].requested.is_some() || state.info[function].tier.is_some() {
            let request = state.requests[function].clone();
            drop(state);
            if explicit {
                service().prioritize(&request);
            }
            return Ok(super::EagerDecision::AlreadyRequested);
        }
        let outstanding = state
            .eager_jobs
            .iter()
            .enumerate()
            .filter(|(fi, job)| **job && state.info[*fi].compiling)
            .count();
        if outstanding >= options.max_routines {
            return Ok(decline("eager outstanding-job cap"));
        }
        if !forced
            && (state.eager_words.saturating_add(words) > options.max_total_words
                || state.eager_cost.saturating_add(cost) > options.predicted_budget)
        {
            return Ok(decline("speculative work or predicted-cost budget"));
        }
        state.eager_jobs[function] = true;
        state.eager_words = state.eager_words.saturating_add(words);
        state.eager_cost = state.eager_cost.saturating_add(cost);
        state.requests[function].eager_range(Some((
            options.min_words,
            if forced {
                options.forced_max_words
            } else {
                options.max_words
            },
            forced,
        )));
        drop(state);
        self.program.notice(
            "eager",
            function,
            format!(
                "queued {tier:?}: {} estimated words in [{}, {}), call-soon={}, predicted {:?}",
                routine.estimated_words, options.min_words, options.max_words, explicit, cost
            ),
        );
        self.request(function, tier, None);
        if priority {
            let request = self.state.borrow().requests[function].clone();
            service().prioritize(&request);
        }
        Ok(super::EagerDecision::Queued(tier))
    }
    pub fn precompile(&self, function: usize, tier: Tier) -> Result<(), CompileError> {
        let routine = self
            .program
            .functions
            .get(function)
            .ok_or(CompileError::UnknownFunction(function))?;
        if routine.attributes.compilation == cyallo_ast::CompileHint::AlwaysLazy {
            return Err(CompileError::AlwaysLazy);
        }
        self.state.borrow().requests[function].eager_range(None);
        self.request(function, tier, None);
        Ok(())
    }
    /// Queues `function` (a valid routine index) for `tier`.
    fn request(&self, function: usize, tier: Tier, executed: Option<u64>) {
        if self.program.functions[function].attributes.compilation
            == cyallo_ast::CompileHint::AlwaysLazy
        {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.poll();
        if state
            .info
            .get(function)
            .is_some_and(|info| info.tier.is_some_and(|installed| installed >= tier))
        {
            return;
        }
        let request = state.requests[function].clone();
        drop(state);
        service().request(
            &request,
            tier,
            executed,
            executed.is_some_and(|count| count >= self.policy.hot_instructions),
        );
    }

    pub fn tiers(&self) -> Vec<TierInfo> {
        let mut state = self.state.borrow_mut();
        state.poll();
        state.info.clone()
    }
    pub fn events(&self) -> Vec<TierEvent> {
        let mut state = self.state.borrow_mut();
        state.poll();
        let mut events = state.events.clone();
        events.sort_by_key(|event| event.elapsed);
        events
    }
    /// Poll once so final tier states and events describe the same instant.
    pub fn reporting_snapshot(&self) -> (Vec<TierInfo>, Vec<TierEvent>) {
        let mut state = self.state.borrow_mut();
        state.poll();
        let mut events = state.events.clone();
        events.sort_by_key(|event| event.elapsed);
        (state.info.clone(), events)
    }
    pub fn policy(&self) -> TierPolicy {
        self.policy
    }
    fn admit_first_call(&self, function: usize, session: &mut Session) {
        if function == 0
            || self.program.functions[function].attributes.compilation
                == cyallo_ast::CompileHint::AlwaysLazy
        {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            if state.admitted[function] {
                return;
            }
            state.admitted[function] = true;
        }
        let _ = self.schedule_eager(function, false, 128);
        self.schedule_upcoming(function);
        if self.policy.first_call_wait.is_zero() {
            return;
        }
        let mut state = self.state.borrow_mut();
        state.poll();
        if state.units[function].is_some() {
            return;
        }
        if !state.info[function].compiling {
            return;
        }
        let start = Instant::now();
        let allowance = self
            .policy
            .first_call_wait
            .saturating_sub(state.first_wait_spent);
        while state.units[function].is_none() && state.info[function].compiling {
            let Some(remaining) = allowance.checked_sub(start.elapsed()) else {
                break;
            };
            if state.mailbox.receiver.recv_timeout(remaining).is_err() {
                break;
            }
            state.poll();
        }
        let waited = start.elapsed();
        state.first_wait_spent = state.first_wait_spent.saturating_add(waited);
        if let Some(report) = session.execution_report_mut() {
            report.first_call_wait += waited;
            report.first_call_admissions += 1;
        }
        let compiled = state.units[function].is_some();
        if let Some(origin) = state.origin {
            state.events.push(TierEvent {
                elapsed: origin.elapsed(),
                function,
                kind: TierEventKind::FirstCall { waited, compiled },
            });
        }
    }
    /// Explicit synchronization for tests, prewarming and benchmarks. run() never calls this.
    pub fn wait_for_compilation(&self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        let mut state = self.state.borrow_mut();
        state.poll();
        while state.info.iter().any(|i| i.compiling) {
            let Some(remaining) = until.checked_duration_since(Instant::now()) else {
                return false;
            };
            if state.mailbox.receiver.recv_timeout(remaining).is_err() {
                return false;
            }
            state.poll();
        }
        true
    }
}
impl Executable for TieredArtifact {
    fn run(&self, session: &mut Session, budget: usize) -> Result<(), EngineError> {
        if !Arc::ptr_eq(&self.program, &session.program) {
            self.program.notice(
                "invalidation",
                0,
                "stale Program artifact rejected before execution; no frame reconstruction",
            );
            return Err(EngineError::ForeignProgram);
        }
        let start = session.instruction_count();
        while session.instruction_count() - start < budget as u64 {
            if matches!(session.status(), Status::Finished | Status::Failed(_))
                || (*session.status() == Status::WaitingForInput
                    && session.context.input.is_empty())
            {
                break;
            }
            let Some(function) = session.active_function() else {
                break;
            };
            let work = session.function_instruction_count(function);
            let size = self.program.bytecode.functions[function].instructions.len();
            let shape = self.program.functions[function].shape;
            if shape.loops && !shape.calls && size > self.policy.speed_size_limit {
                // Too large to amortize speed codegen; stop polling for a hot
                // upgrade that the policy would reject anyway.
                self.state.borrow_mut().speed_eligible[function] = false;
            }
            let calls = session.function_call_count(function);
            if let Some(desired) = self
                .policy
                .hinted(self.program.functions[function].attributes.heat)
                .promote(shape, size, work, calls)
            {
                self.request(function, desired, Some(work));
            }
            self.admit_first_call(function, session);
            let remaining = (budget as u64 - (session.instruction_count() - start)) as usize;
            let before = session.instruction_count();
            {
                let mut state = self.state.borrow_mut();
                state.poll();
                session.bind_native_entries(state.entries.as_ptr());
                session.bind_native_control(
                    &state.mailbox.epoch,
                    state.seen,
                    state.transition_limits.as_ptr(),
                    state.osr_targets.as_ptr(),
                    self.policy.execution_quantum as u64,
                );
                let optimized = state.info[function].tier == Some(Tier::CraneliftSpeed);
                let pc = session.frames.last().unwrap().pc;
                let resume_lower = optimized
                    && !self.program.functions[function].resume.accepts(pc)
                    && !matches!(&state.units[function],Some(Unit::Cranelift(unit)) if unit.owns_speed_frame(session));
                let selected = if resume_lower {
                    &state.lower[function]
                } else {
                    &state.units[function]
                };
                let result = match selected {
                    Some(Unit::Winch(unit)) => unit.run(session, remaining),
                    Some(Unit::Cranelift(unit)) => unit.run(session, remaining),
                    None => {
                        // Stop at a call so the new active routine is admitted
                        // immediately, and at an optimized entry when bridging.
                        let depth = session.frames.len();
                        for _ in 0..remaining.min(self.policy.execution_quantum) {
                            if !session.step()
                                || session.frames.len() != depth
                                || session.active_function() != Some(function)
                            {
                                break;
                            }
                            if resume_lower
                                && self.program.functions[function]
                                    .resume
                                    .accepts(session.frames.last().unwrap().pc)
                            {
                                break;
                            }
                        }
                        session.pause_reporting();
                        Ok(())
                    }
                };
                session.bind_native_entries(std::ptr::null());
                session.bind_native_control(
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                );
                result?;
            }
            if session.instruction_count() == before
                && !matches!(
                    session.native_exit_reason(),
                    Some(ExitReason::CodeReady | ExitReason::Osr | ExitReason::Hot)
                )
            {
                return Err(EngineError::NoProgress);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heat_scales_call_and_work_thresholds_and_keeps_zero() {
        let policy = TierPolicy {
            winch_calls: 7,
            winch_instructions: 10,
            warm_instructions: 0,
            hot_instructions: u64::MAX,
            ..Default::default()
        };
        let hot = policy.hinted(cyallo_ast::HeatHint::Hot);
        assert_eq!(
            (
                hot.winch_calls,
                hot.winch_instructions,
                hot.warm_instructions
            ),
            (3, 5, 0)
        );
        let cold = policy.hinted(cyallo_ast::HeatHint::Cold);
        assert_eq!(
            (
                cold.winch_calls,
                cold.winch_instructions,
                cold.warm_instructions,
                cold.hot_instructions
            ),
            (28, 40, 0, u64::MAX)
        );
    }
}
