//! Durable requests and coalesced notifications; channel capacity never owns work.
use super::*;
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering},
    },
};
struct CostHistory {
    buckets: [VecDeque<Duration>; 24],
}
impl Default for CostHistory {
    fn default() -> Self {
        Self {
            buckets: std::array::from_fn(|_| VecDeque::new()),
        }
    }
}
impl CostHistory {
    fn bucket(tier: Tier, words: usize) -> usize {
        let size = (usize::BITS - words.max(1).leading_zeros()) as usize;
        tier as usize * 8 + size.saturating_sub(3).min(7)
    }
    fn predict(&self, tier: Tier, words: usize) -> Duration {
        let bucket = &self.buckets[Self::bucket(tier, words)];
        if bucket.is_empty() {
            return Duration::from_micros(match tier {
                Tier::Winch => 50,
                Tier::CraneliftFast => 500,
                Tier::CraneliftSpeed => 1500,
            });
        }
        let mut values: Vec<_> = bucket.iter().copied().collect();
        values.sort();
        values[values.len() * 3 / 4]
    }
    fn record(&mut self, tier: Tier, words: usize, cost: Duration) {
        let bucket = &mut self.buckets[Self::bucket(tier, words)];
        if bucket.len() == 16 {
            bucket.pop_front();
        }
        bucket.push_back(cost);
    }
}

pub(super) struct Mailbox {
    messages: Mutex<VecDeque<WorkerMessage>>,
    wake: kanal::Sender<()>,
    pub receiver: kanal::Receiver<()>,
    pub epoch: AtomicU64,
}
impl Mailbox {
    pub fn new() -> Arc<Self> {
        let (wake, receiver) = kanal::bounded(1);
        Arc::new(Self {
            messages: Mutex::new(VecDeque::new()),
            wake,
            receiver,
            epoch: AtomicU64::new(0),
        })
    }
    fn push(&self, message: WorkerMessage) {
        self.messages.lock().unwrap().push_back(message);
        self.notify();
    }
    fn notify(&self) {
        self.epoch.fetch_add(1, AtomicOrdering::Release);
        let _ = self.wake.try_send(());
    }
    pub fn drain(&self, seen: &mut u64) -> VecDeque<WorkerMessage> {
        let epoch = self.epoch.load(AtomicOrdering::Acquire);
        if epoch == *seen {
            return VecDeque::new();
        }
        while matches!(self.receiver.try_recv(), Ok(Some(()))) {}
        let messages = std::mem::take(&mut *self.messages.lock().unwrap());
        *seen = epoch;
        messages
    }
}
#[derive(Default)]
struct RequestState {
    eager_range: Option<(usize, usize, bool)>,
    desired: Option<Tier>,
    queued: bool,
    running: Option<Tier>,
    generation: u64,
    at: Option<Instant>,
    priority: u8,
}
pub(super) struct Slots {
    active: AtomicUsize,
    limit: usize,
}
impl Slots {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            limit: limit.max(1),
        })
    }
    fn acquire(&self) -> bool {
        self.active
            .try_update(AtomicOrdering::AcqRel, AtomicOrdering::Relaxed, |n| {
                (n < self.limit).then_some(n + 1)
            })
            .is_ok()
    }
}
pub(super) struct Request {
    slots: Arc<Slots>,
    program: Arc<Program>,
    function: usize,
    mailbox: Arc<Mailbox>,
    alive: Weak<()>,
    reporting: bool,
    state: Mutex<RequestState>,
}
impl Request {
    pub fn eager_range(&self, range: Option<(usize, usize, bool)>) {
        self.state.lock().unwrap().eager_range = range;
    }
    pub fn new(
        program: Arc<Program>,
        function: usize,
        mailbox: Arc<Mailbox>,
        alive: Weak<()>,
        reporting: bool,
        slots: Arc<Slots>,
    ) -> Arc<Self> {
        Arc::new(Self {
            slots,
            program,
            function,
            mailbox,
            alive,
            reporting,
            state: Mutex::default(),
        })
    }
    pub fn status(&self) -> (Option<Tier>, Option<Tier>, Option<Tier>) {
        let state = self.state.lock().unwrap();
        (
            state.desired,
            state.queued.then_some(state.desired).flatten(),
            state.running,
        )
    }
    fn event(&self, at: Instant, kind: TierEventKind) {
        if self.reporting {
            self.mailbox.push(WorkerMessage::Event {
                function: self.function,
                at,
                kind,
            });
        }
    }
}
#[derive(Clone)]
struct Pending {
    request: Arc<Request>,
    generation: u64,
    priority: u8,
    sequence: u64,
}
impl PartialEq for Pending {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Pending {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}
#[derive(Default)]
struct Queue {
    heap: BinaryHeap<Pending>,
    sequence: u64,
    started: u64,
}
impl Queue {
    fn push(&mut self, request: Arc<Request>, generation: u64, priority: u8) {
        self.sequence += 1;
        self.heap.push(Pending {
            request,
            generation,
            priority,
            sequence: self.sequence,
        });
    }
    fn pop(&mut self) -> Option<Pending> {
        // Every sixteenth selection gives the oldest request a turn.
        if self.started % 16 == 15 {
            let oldest = self.heap.iter().min_by_key(|job| job.sequence)?.clone();
            self.heap.retain(|job| job.sequence != oldest.sequence);
            Some(oldest)
        } else {
            self.heap.pop()
        }
    }
}
pub(super) struct Service {
    history: Mutex<CostHistory>,
    queue: Mutex<Queue>,
    wake: kanal::Sender<()>,
    pub workers: usize,
}
fn worker_count(logical: usize) -> usize {
    logical.saturating_sub(1).clamp(1, 3)
}
impl Service {
    pub fn predict(&self, tier: Tier, words: usize) -> Duration {
        self.history.lock().unwrap().predict(tier, words)
    }
    pub fn prioritize(&self, request: &Arc<Request>) {
        let mut state = request.state.lock().unwrap();
        if state.queued && state.priority < 2 {
            state.priority = 2;
            state.generation += 1;
            self.queue
                .lock()
                .unwrap()
                .push(request.clone(), state.generation, 2);
            let _ = self.wake.try_send(());
        }
    }
    pub fn request(&self, request: &Arc<Request>, tier: Tier, executed: Option<u64>, hot: bool) {
        let at = Instant::now();
        let mut state = request.state.lock().unwrap();
        if executed.is_some() {
            state.eager_range = None;
        }
        if state.desired.is_some_and(|previous| previous >= tier) {
            return;
        }
        let previous = state.desired;
        let superseded = state.queued.then_some(previous).flatten();
        state.desired = Some(tier);
        state.generation += 1;
        state.at = Some(at);
        state.priority = if hot || state.running.is_some() {
            2
        } else {
            u8::from(executed.is_some_and(|count| count > 0))
        };
        if state.running.is_none() {
            state.queued = true;
            self.queue
                .lock()
                .unwrap()
                .push(request.clone(), state.generation, state.priority);
        }
        drop(state);
        request.event(at, TierEventKind::Requested { tier, executed });
        if let Some(previous) = superseded {
            request.event(at, TierEventKind::Superseded { previous, tier });
        }
        let _ = self.wake.try_send(());
    }
    fn take(&self) -> Option<(Arc<Request>, Tier, Instant)> {
        let attempts = self.queue.lock().unwrap().heap.len();
        let mut blocked = Vec::new();
        for _ in 0..attempts {
            let Some(pending) = self.queue.lock().unwrap().pop() else {
                break;
            };
            let pending_again = pending.clone();
            let request = pending.request;
            if request.alive.upgrade().is_none() {
                continue;
            }
            let mut state = request.state.lock().unwrap();
            if !state.queued || state.generation != pending.generation {
                continue;
            }
            if !request.slots.acquire() {
                blocked.push(pending_again);
                continue;
            }
            let tier = state.desired.unwrap();
            let queued = state.at.unwrap();
            state.queued = false;
            state.running = Some(tier);
            drop(state);
            let mut queue = self.queue.lock().unwrap();
            queue.heap.extend(blocked);
            queue.started += 1;
            if !queue.heap.is_empty() {
                let _ = self.wake.try_send(());
            }
            return Some((request, tier, queued));
        }
        self.queue.lock().unwrap().heap.extend(blocked);
        None
    }
    fn finish(&self, request: &Arc<Request>, tier: Tier, message: Option<Finished>) {
        let mut state = request.state.lock().unwrap();
        self.finish_locked(request, &mut state, tier, message);
    }
    fn reject_eager(
        &self,
        request: &Arc<Request>,
        tier: Tier,
        range: (usize, usize, bool),
    ) -> bool {
        let mut state = request.state.lock().unwrap();
        // An observed-work or explicit precompile request may have superseded
        // the speculative size gate while this worker was checking the body.
        if state.eager_range != Some(range) {
            return false;
        }
        if state.desired == Some(tier) {
            state.desired = None;
        }
        state.eager_range = None;
        self.finish_locked(request, &mut state, tier, None);
        true
    }
    fn finish_locked(
        &self,
        request: &Arc<Request>,
        state: &mut RequestState,
        tier: Tier,
        message: Option<Finished>,
    ) {
        state.running = None;
        request.slots.active.fetch_sub(1, AtomicOrdering::Release);
        if state.desired.is_some_and(|desired| desired > tier) && request.alive.upgrade().is_some()
        {
            state.queued = true;
            self.queue
                .lock()
                .unwrap()
                .push(request.clone(), state.generation, state.priority);
            let _ = self.wake.try_send(());
        }
        if let Some(message) = message {
            request.mailbox.push(WorkerMessage::Finished(message));
        } else {
            request.mailbox.notify();
        }
        let _ = self.wake.try_send(());
    }
}
pub(super) fn service() -> &'static Arc<Service> {
    static SERVICE: OnceLock<Arc<Service>> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (wake, receiver) = kanal::bounded(1);
        let service = Arc::new(Service {
            history: Mutex::default(),
            queue: Mutex::default(),
            wake,
            workers: worker_count(thread::available_parallelism().map_or(1, usize::from)),
        });
        for worker in 0..service.workers {
            let compiler = service.clone();
            let receiver = receiver.clone();
            thread::Builder::new()
                .name(format!("cyallo-codegen-{}", worker + 1))
                .spawn(move || {
                    loop {
                        let Some((request, tier, queued)) = compiler.take() else {
                            if receiver.recv().is_err() {
                                break;
                            }
                            continue;
                        };
                        let started = Instant::now();
                        let range = request.state.lock().unwrap().eager_range;
                        if let Some(range @ (small, medium, forced)) = range
                            && let Ok(body) = request.program.functions[request.function].checked()
                        {
                            let size = body.code.len();
                            let outside = if forced { size > medium } else { size < small || size >= medium };
                            if outside && compiler.reject_eager(&request, tier, range) {
                                let kind = if matches!(body.attributes.compilation,
                                    cyallo_ast::CompileHint::Eager | cyallo_ast::CompileHint::AlwaysEager)
                                { "unfulfilled" } else { "eager" };
                                request.program.notice(kind, request.function, format!(
                                    "actual lowered size {size} is outside eager eligibility; retained lazy tiering"));
                                continue;
                            }
                        }
                        request.event(
                            started,
                            TierEventKind::CompileStarted {
                                tier,
                                worker: thread::current().id(),
                                queue_time: started.duration_since(queued),
                            },
                        );
                        let mut actual = tier;
                        let mut fallback = None;
                        let result = match tier {
                            Tier::Winch => {
                                match WinchUnit::compile(request.program.clone(), request.function)
                                {
                                    Ok(unit) => Ok(Unit::Winch(Box::new(unit))),
                                    Err(error) => {
                                        fallback = Some(error);
                                        actual = Tier::CraneliftFast;
                                        compile_unit(
                                            request.program.clone(),
                                            request.function,
                                            false,
                                        )
                                        .map(|unit| Unit::Cranelift(Box::new(unit)))
                                    }
                                }
                            }
                            Tier::CraneliftFast | Tier::CraneliftSpeed => compile_unit(
                                request.program.clone(),
                                request.function,
                                tier == Tier::CraneliftSpeed,
                            )
                            .map(|unit| Unit::Cranelift(Box::new(unit))),
                        };
                        let completed = Instant::now();
                        if result.is_ok() && !request.program.inspection().is_some_and(|o|o.options.emit_ir || o.options.emit_bytecode || o.options.emit_clif || o.options.full_trace) {
                            let words=request.program.functions[request.function].code.len();
                            compiler.history.lock().unwrap().record(actual,words,completed.duration_since(started));
                        }
                        compiler.finish(
                            &request,
                            tier,
                            Some(Finished {
                                function: request.function,
                                actual,
                                result,
                                elapsed: completed.duration_since(started),
                                thread: thread::current().id(),
                                fallback,
                                completed,
                            }),
                        );
                    }
                })
                .expect("create dedicated compiler thread");
        }
        service
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Service, Arc<Request>, Arc<()>) {
        let (wake, _receiver) = kanal::bounded(1);
        let service = Service {
            history: Mutex::default(),
            queue: Mutex::default(),
            wake,
            workers: 1,
        };
        let owner = Arc::new(());
        let request = Request::new(
            Arc::new(crate::compile("std::io::print(1)\n").unwrap()),
            0,
            Mailbox::new(),
            Arc::downgrade(&owner),
            true,
            Slots::new(1),
        );
        (service, request, owner)
    }
    #[test]
    fn queued_upgrade_supersedes_and_notifications_do_not_own_requests() {
        let (service, request, _owner) = setup();
        service.request(&request, Tier::Winch, Some(0), false);
        service.request(&request, Tier::CraneliftSpeed, Some(10000), true);
        let (_, tier, _) = service.take().unwrap();
        assert_eq!(tier, Tier::CraneliftSpeed);
        assert!(service.take().is_none());
    }
    #[test]
    fn final_size_rejection_cannot_cancel_an_observed_request() {
        let (service, request, _owner) = setup();
        let range = (16, 128, false);
        request.eager_range(Some(range));
        service.request(&request, Tier::Winch, None, false);
        service.take().unwrap();
        service.request(&request, Tier::Winch, Some(2000), false);
        assert!(!service.reject_eager(&request, Tier::Winch, range));
        assert_eq!(
            request.status(),
            (Some(Tier::Winch), None, Some(Tier::Winch))
        );
        service.finish(&request, Tier::Winch, None);
    }
    #[test]
    fn rejected_eager_work_can_be_requested_again_without_lost_wakeups() {
        let (service, request, _owner) = setup();
        let range = (16, 128, false);
        request.eager_range(Some(range));
        service.request(&request, Tier::Winch, None, false);
        service.take().unwrap();
        assert!(service.reject_eager(&request, Tier::Winch, range));
        assert_eq!(request.status(), (None, None, None));
        service.request(&request, Tier::Winch, Some(2000), false);
        assert_eq!(service.take().unwrap().1, Tier::Winch);
    }
    #[test]
    fn running_upgrade_survives_and_queues_without_driver_poll() {
        let (service, request, _owner) = setup();
        service.request(&request, Tier::Winch, Some(0), false);
        let (_, tier, _) = service.take().unwrap();
        service.request(&request, Tier::CraneliftSpeed, Some(10000), true);
        assert_eq!(
            request.status(),
            (Some(Tier::CraneliftSpeed), None, Some(Tier::Winch))
        );
        service.finish(&request, tier, None);
        assert_eq!(service.take().unwrap().1, Tier::CraneliftSpeed);
    }
    #[test]
    fn abandoned_requests_are_skipped() {
        let (service, request, owner) = setup();
        service.request(&request, Tier::Winch, Some(0), false);
        drop(owner);
        assert!(service.take().is_none());
    }
    #[test]
    fn failed_upgrade_is_reported_without_rearming_its_transition() {
        let (service, request, _owner) = setup();
        service.request(&request, Tier::CraneliftFast, Some(5000), false);
        let (_, tier, _) = service.take().unwrap();
        service.finish(
            &request,
            tier,
            Some(Finished {
                function: 0,
                actual: tier,
                result: Err(CompileError::UnsupportedTarget),
                elapsed: Duration::ZERO,
                thread: thread::current().id(),
                fallback: None,
                completed: Instant::now(),
            }),
        );
        let artifact = TieredBackend::default()
            .compile(request.program.clone())
            .unwrap();
        let mut state = artifact.state.borrow_mut();
        state.mailbox = request.mailbox.clone();
        state.requests[0] = request.clone();
        state.poll();
        assert!(matches!(
            state.info[0].error,
            Some(CompileError::UnsupportedTarget)
        ));
        assert!(!state.info[0].compiling);
        assert_eq!(state.transition_limits[0], u64::MAX);
        service.request(&request, Tier::CraneliftFast, Some(5001), false);
        assert!(service.take().is_none());
    }
    #[test]
    fn workers_reserve_one_logical_cpu_and_cap_at_three() {
        for (logical, expected) in [(0, 1), (1, 1), (2, 1), (3, 2), (4, 3), (8, 3), (64, 3)] {
            assert_eq!(worker_count(logical), expected);
        }
    }
    #[test]
    fn per_artifact_slots_limit_parallelism_without_blocking_other_work() {
        let (service, a, owner) = setup();
        let b = Request::new(
            a.program.clone(),
            0,
            Mailbox::new(),
            Arc::downgrade(&owner),
            false,
            a.slots.clone(),
        );
        service.request(&a, Tier::Winch, None, false);
        service.request(&b, Tier::Winch, None, false);
        let (running, tier, _) = service.take().unwrap();
        assert!(service.take().is_none());
        let c = Request::new(
            a.program.clone(),
            0,
            Mailbox::new(),
            Arc::downgrade(&owner),
            false,
            Slots::new(1),
        );
        service.request(&c, Tier::Winch, None, false);
        let (other, _, _) = service.take().unwrap();
        assert!(Arc::ptr_eq(&other, &c));
        service.finish(&running, tier, None);
        assert!(Arc::ptr_eq(&service.take().unwrap().0, &b));
    }
    #[test]
    fn cost_history_is_bounded_and_separates_tier_and_size_buckets() {
        let mut history = CostHistory::default();
        for n in 1..=20 {
            history.record(Tier::Winch, 32, Duration::from_micros(n));
        }
        assert_eq!(
            history.buckets[CostHistory::bucket(Tier::Winch, 32)].len(),
            16
        );
        assert_eq!(history.predict(Tier::Winch, 32), Duration::from_micros(17));
        assert_eq!(
            history.predict(Tier::CraneliftFast, 32),
            Duration::from_micros(500)
        );
        assert_eq!(
            history.predict(Tier::Winch, 1024),
            Duration::from_micros(50)
        );
    }
    #[test]
    fn coalesced_mailbox_keeps_all_completions_and_old_requests_get_a_turn() {
        let (service, request, _owner) = setup();
        for _ in 0..100 {
            request.mailbox.push(WorkerMessage::Event {
                function: 0,
                at: Instant::now(),
                kind: TierEventKind::Requested {
                    tier: Tier::Winch,
                    executed: None,
                },
            });
        }
        assert_eq!(request.mailbox.drain(&mut 0).len(), 100);
        let mut q = service.queue.lock().unwrap();
        q.push(request.clone(), 0, 0);
        for _ in 0..20 {
            q.push(request.clone(), 0, 2);
        }
        for _ in 0..15 {
            assert_eq!(q.pop().unwrap().priority, 2);
            q.started += 1;
        }
        assert_eq!(q.pop().unwrap().priority, 0);
    }
}
