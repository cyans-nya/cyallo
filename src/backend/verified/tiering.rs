//! Tier-up. Native code starts in Winch, the one-to-one baseline. The opt-in
//! interpreter-first mode enters Winch at a taken loop backedge. A run is
//! sliced; each pause attributes the slice to the function that was running
//! (sampling: no counters in generated code). A function whose executed
//! instructions reach its threshold is compiled with Cranelift on a worker
//! thread, and the image is installed at the next pause. Both tiers come from
//! the same plan, so frames and recovery maps are identical and a swap is just
//! re-entering the new image at the paused instruction.
//!
//! The threshold is the point where the saving of a Cranelift instruction over
//! a Winch instruction (about 0.5 ns, measured in `docs/native-tier-samples.md`)
//! repays the compile cost, which grows with the function's size:
//! `N(s) = base + per_site * s + per_site_squared * s^2` instructions for a
//! function of `s` plan sites. It counts instructions, not time, because the
//! compile cost and the saving scale together with the machine's speed.

use super::{
    CodeStore, CraneliftProfile, Dispatch, NativeRunError, NativeTier, PreparedFunction,
    UnlinkedImage,
};
use crate::bytecode::verified::{ExecutionStatus, FunctionId, Instruction, InstructionId};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierPolicy {
    /// Instructions per slice; the sampling interval.
    pub slice: usize,
    pub base: u64,
    pub per_site: u64,
    pub per_site_squared: u64,
    /// Percent of the threshold at which the compile is requested. The compile
    /// runs on another thread, so starting early costs CPU, not latency.
    pub request_percent: u64,
    pub enabled: bool,
    /// Run in the interpreter until native code can take over a live loop.
    pub interpreter_first: bool,
}

impl Default for TierPolicy {
    fn default() -> Self {
        Self {
            slice: 100_000,
            base: 500_000,
            per_site: 32_000,
            per_site_squared: 10,
            request_percent: 25,
            enabled: true,
            interpreter_first: false,
        }
    }
}

impl TierPolicy {
    /// Read the tiering overrides for this run.
    pub fn from_env() -> Self {
        let enabled = std::env::var("CYALLO_TIERING").map_or(true, |value| value != "off");
        let request_percent = std::env::var("CYALLO_TIER_REQUEST_PERCENT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(Self::default().request_percent);
        Self {
            enabled,
            request_percent,
            interpreter_first: std::env::var("CYALLO_JIT_START")
                .is_ok_and(|value| value == "interpreter"),
            ..Self::default()
        }
    }

    /// Promote at the first sample; for tests that must observe a swap.
    pub fn eager(slice: usize) -> Self {
        Self {
            slice,
            base: 0,
            per_site: 0,
            per_site_squared: 0,
            request_percent: 100,
            enabled: true,
            interpreter_first: false,
        }
    }

    /// Executed instructions in a function of `sites` plan sites after which
    /// it is worth compiling with Cranelift.
    pub fn threshold(&self, sites: usize) -> u64 {
        let sites = sites as u64;
        self.base
            .saturating_add(self.per_site.saturating_mul(sites))
            .saturating_add(
                self.per_site_squared
                    .saturating_mul(sites.saturating_mul(sites)),
            )
    }

    /// Executed instructions at which the compile is requested.
    pub fn request_at(&self, sites: usize) -> u64 {
        let threshold = u128::from(self.threshold(sites));
        u64::try_from(threshold * u128::from(self.request_percent) / 100).unwrap_or(u64::MAX)
    }
}

struct Job {
    function: FunctionId,
    plan: Arc<PreparedFunction>,
    tier: NativeTier,
}

struct Done {
    function: FunctionId,
    image: Option<UnlinkedImage>,
}

/// One thread that emits native images. Emitting reads a plan and produces
/// an image; publishing stays on the owning thread because pins are not `Send`.
struct Worker {
    jobs: Option<Sender<Job>>,
    done: Receiver<Done>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn() -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (finished, done) = mpsc::channel::<Done>();
        let handle = thread::Builder::new()
            .name("cyallo-tier-up".into())
            .spawn(move || {
                for Job {
                    function,
                    plan,
                    tier,
                } in queue
                {
                    let image = tier.emit(plan).ok();
                    if finished.send(Done { function, image }).is_err() {
                        break;
                    }
                }
            })
            .ok();
        Self {
            jobs: Some(jobs),
            done,
            handle,
        }
    }
}

impl Drop for Worker {
    /// The run is over: close the queue and let the thread finish on its own.
    /// Waiting for it would delay the end of the program by a compile.
    fn drop(&mut self) {
        self.jobs = None;
        self.handle.take();
    }
}

/// A native run that promotes hot functions from Winch to Cranelift.
pub struct Tiered {
    dispatch: Dispatch,
    store: Option<CodeStore>,
    policy: TierPolicy,
    executed: HashMap<FunctionId, u64>,
    requested: HashMap<FunctionId, NativeTier>,
    worker: Option<Worker>,
    /// Compile jobs sent and not yet received back.
    outstanding: usize,
    promoted: Vec<FunctionId>,
}

impl Tiered {
    pub fn new(store: CodeStore, dispatch: Dispatch, policy: TierPolicy) -> Self {
        Self::build(Some(store), dispatch, policy)
    }

    /// A run with nothing to promote (the interpreter alone).
    pub fn without_store(dispatch: Dispatch) -> Self {
        Self::build(None, dispatch, TierPolicy::default())
    }

    fn build(store: Option<CodeStore>, dispatch: Dispatch, policy: TierPolicy) -> Self {
        Self {
            dispatch,
            store,
            policy,
            executed: HashMap::new(),
            requested: HashMap::new(),
            worker: None,
            outstanding: 0,
            promoted: Vec::new(),
        }
    }

    /// The running dispatch, for input and output configuration.
    pub fn dispatch(&mut self) -> &mut Dispatch {
        &mut self.dispatch
    }

    /// Functions whose Cranelift image was installed, in order.
    pub fn promoted(&self) -> &[FunctionId] {
        &self.promoted
    }

    /// The tier of the function's latest published entry.
    pub fn tier_of(&self, function: FunctionId) -> Option<NativeTier> {
        self.store.as_ref()?.entry(function).map(|pin| pin.tier())
    }

    /// Run for up to `budget` instructions, in sampling slices. Returns at the
    /// first status that is not a plain budget pause (finished, failed, or
    /// waiting for input) or when the budget is spent.
    pub fn resume(&mut self, budget: usize) -> Result<ExecutionStatus, NativeRunError> {
        if budget == 0 || !self.policy.enabled || self.store.is_none() {
            return self.dispatch.resume(budget);
        }
        let mut remaining = budget;
        loop {
            let slice = remaining.min(self.policy.slice.max(1));
            let before = self.dispatch.instructions_executed();
            let status = self.dispatch.resume(slice)?;
            let used = self.dispatch.instructions_executed().saturating_sub(before);
            remaining = remaining.saturating_sub(used.max(1));
            self.install_finished();
            let ExecutionStatus::Paused(position) = status else {
                return Ok(status);
            };
            if self.dispatch.awaiting_input() {
                return Ok(status);
            }
            self.sample(position.function, used as u64);
            if remaining == 0 {
                return Ok(status);
            }
        }
    }

    /// Attribute `used` instructions to `function`; request its promotion
    /// when it crosses the threshold.
    fn sample(&mut self, function: FunctionId, used: u64) {
        let total = self.executed.entry(function).or_insert(0);
        *total = total.saturating_add(used);
        let total = *total;
        let waiting_for_osr = self.dispatch.waiting_for_osr();
        let Some(store) = &mut self.store else {
            return;
        };
        let tier = match store.entry(function) {
            Some(current) if !waiting_for_osr && current.tier() == NativeTier::Winch => {
                NativeTier::Cranelift(CraneliftProfile::LowLatency)
            }
            None if waiting_for_osr => NativeTier::Winch,
            _ => return,
        };
        if self.requested.get(&function) == Some(&tier) {
            return;
        }
        if waiting_for_osr && !has_backedge(store, function) {
            return;
        }
        let Ok(plan) = store.prepare(function) else {
            return;
        };
        // The experimental interpreter-first mode reuses the existing
        // conservative request threshold until Winch's startup cost is tuned.
        if total < self.policy.request_at(plan.sites().len()) {
            return;
        }
        self.requested.insert(function, tier);
        let worker = self.worker.get_or_insert_with(Worker::spawn);
        if let Some(jobs) = &worker.jobs
            && jobs
                .send(Job {
                    function,
                    plan,
                    tier,
                })
                .is_ok()
        {
            self.outstanding += 1;
        }
    }

    /// Publish the images the worker finished and install them in the run.
    fn install_finished(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        let finished: Vec<Done> = worker.done.try_iter().collect();
        for done in finished {
            self.finish(done);
        }
    }

    fn finish(&mut self, Done { function, image }: Done) {
        self.outstanding = self.outstanding.saturating_sub(1);
        let Some(image) = image else {
            return;
        };
        let Some(store) = &mut self.store else {
            return;
        };
        let Ok(pin) = store.publish(image) else {
            return;
        };
        if pin.tier() == NativeTier::Winch {
            self.executed.remove(&function);
        }
        self.dispatch.install(function, pin);
        self.promoted.push(function);
    }

    /// Wait for outstanding compiles and install them; for tests.
    pub fn settle(&mut self) {
        while self.outstanding > 0 {
            let Some(worker) = &self.worker else {
                return;
            };
            match worker.done.recv() {
                Ok(done) => self.finish(done),
                Err(_) => return,
            }
        }
    }
}

fn has_backedge(store: &CodeStore, function: FunctionId) -> bool {
    let Some(body) = store.module().function(function) else {
        return false;
    };
    body.instructions()
        .iter()
        .enumerate()
        .any(|(pc, instruction)| {
            let back = |to: InstructionId| to.0 as usize <= pc;
            let is_backedge = match *instruction {
                Instruction::Jump(to) => back(to),
                Instruction::Branch(_, yes, no) => back(yes) || back(no),
                _ => false,
            };
            is_backedge && body.is_reachable(InstructionId(pc as u32))
        })
}
