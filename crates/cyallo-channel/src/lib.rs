//! Scheduler-aware MPMC channels. Implemented independently of Rust channel
//! receive futures so cancellation and selection share a commit protocol.
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
};

const NONE: usize = usize::MAX;
const DEFAULT: usize = usize::MAX - 1;
const TIMEOUT: usize = usize::MAX - 2;

struct Coordinator {
    gate: Mutex<()>,
    buffered: AtomicUsize,
    limit: usize,
    turn: AtomicU64,
    next: AtomicU64,
}
impl Coordinator {
    fn next_id(&self) -> u64 {
        self.next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("channel operation identity space exhausted")
    }
}
/// Channels from a Factory share selection coordination and a buffer quota.
#[derive(Clone)]
pub struct Factory {
    coordinator: Arc<Coordinator>,
}
impl Default for Factory {
    fn default() -> Self {
        Self::new(1_000_000)
    }
}
impl Factory {
    pub fn new(max_buffered_messages: usize) -> Self {
        Self {
            coordinator: Arc::new(Coordinator {
                gate: Mutex::new(()),
                buffered: AtomicUsize::new(0),
                limit: max_buffered_messages,
                turn: AtomicU64::new(0),
                next: AtomicU64::new(1),
            }),
        }
    }
    pub fn bounded<T: Send + 'static>(&self, capacity: usize) -> (Sender<T>, Receiver<T>) {
        self.make(Some(capacity))
    }
    pub fn unbounded<T: Send + 'static>(&self) -> (Sender<T>, Receiver<T>) {
        self.make(None)
    }
    pub fn buffered(&self) -> usize {
        self.coordinator.buffered.load(Ordering::Acquire)
    }
    fn make<T: Send + 'static>(&self, capacity: Option<usize>) -> (Sender<T>, Receiver<T>) {
        let inner = Arc::new(Inner {
            coordinator: self.coordinator.clone(),
            state: Mutex::new(State {
                capacity,
                closed: false,
                queue: VecDeque::new(),
                sends: VecDeque::new(),
                receives: VecDeque::new(),
            }),
            senders: AtomicUsize::new(1),
            receivers: AtomicUsize::new(1),
        });
        (
            Sender {
                inner: inner.clone(),
            },
            Receiver { inner },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFailure {
    Closed,
    Full,
    Limit,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError<T> {
    pub kind: SendFailure,
    pub value: T,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveError {
    Closed,
    Empty,
}
impl std::fmt::Display for SendFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "channel closed",
            Self::Full => "channel full",
            Self::Limit => "channel buffer quota exceeded",
        })
    }
}
impl<T> std::fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind.fmt(f)
    }
}
impl<T: std::fmt::Debug> std::error::Error for SendError<T> {}
impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Closed => "channel closed",
            Self::Empty => "channel empty",
        })
    }
}
impl std::error::Error for ReceiveError {}

struct State<T> {
    capacity: Option<usize>,
    closed: bool,
    queue: VecDeque<T>,
    sends: VecDeque<Arc<Op<T>>>,
    receives: VecDeque<Arc<Op<T>>>,
}
struct Inner<T> {
    coordinator: Arc<Coordinator>,
    state: Mutex<State<T>>,
    senders: AtomicUsize,
    receivers: AtomicUsize,
}
impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        self.coordinator
            .buffered
            .fetch_sub(self.state.get_mut().unwrap().queue.len(), Ordering::AcqRel);
    }
}
pub struct Sender<T> {
    inner: Arc<Inner<T>>,
}
pub struct Receiver<T> {
    inner: Arc<Inner<T>>,
}
impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        self.inner.senders.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.inner.receivers.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if self.inner.senders.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.close(false);
        }
    }
}
impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        if self.inner.receivers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.inner.close(true);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Send,
    Receive,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    Committed,
    Closed,
    Limit,
    Cancelled,
}
struct OpData<T> {
    status: Status,
    value: Option<T>,
    waker: Option<Waker>,
}
struct SelectionState {
    winner: usize,
    waker: Option<Waker>,
}
struct Op<T> {
    id: u64,
    kind: Kind,
    data: Mutex<OpData<T>>,
    selection: Option<(Arc<Mutex<SelectionState>>, usize)>,
}
impl<T> Op<T> {
    fn eligible(&self) -> bool {
        self.data.lock().unwrap().status == Status::Pending
            && self
                .selection
                .as_ref()
                .is_none_or(|(s, _)| s.lock().unwrap().winner == NONE)
    }
    fn claim(&self, wakes: &mut Vec<Waker>) {
        if let Some((s, arm)) = &self.selection {
            let mut s = s.lock().unwrap();
            s.winner = *arm;
            if let Some(w) = s.waker.take() {
                wakes.push(w);
            }
        }
    }
    fn mark(&self, status: Status, wakes: &mut Vec<Waker>) {
        let mut data = self.data.lock().unwrap();
        data.status = status;
        if let Some(w) = data.waker.take() {
            wakes.push(w);
        }
    }
    fn same_selection(&self, other: &Self) -> bool {
        matches!((&self.selection, &other.selection), (Some((a, _)), Some((b, _))) if Arc::ptr_eq(a, b))
    }
}
fn wake_all(wakes: Vec<Waker>) {
    for wake in wakes {
        wake.wake();
    }
}

impl<T> Inner<T> {
    // All commitment, including paired rendezvous selections, is serialized by
    // the factory gate. No guest destructors/wakers run under either lock.
    fn progress(&self, state: &mut State<T>, wakes: &mut Vec<Waker>) {
        state.sends.retain(|op| op.eligible());
        state.receives.retain(|op| op.eligible());
        loop {
            if !state.queue.is_empty()
                && let Some(receive) = state.receives.pop_front()
            {
                if !receive.eligible() {
                    continue;
                }
                receive.claim(wakes);
                receive.data.lock().unwrap().value = state.queue.pop_front();
                self.coordinator.buffered.fetch_sub(1, Ordering::AcqRel);
                receive.mark(Status::Committed, wakes);
                continue;
            }
            if state.closed {
                while let Some(send) = state.sends.pop_front() {
                    if send.eligible() {
                        send.claim(wakes);
                        send.mark(Status::Closed, wakes);
                    }
                }
                if state.queue.is_empty() {
                    while let Some(receive) = state.receives.pop_front() {
                        if receive.eligible() {
                            receive.claim(wakes);
                            receive.mark(Status::Closed, wakes);
                        }
                    }
                }
                break;
            }
            let pair = state.receives.iter().enumerate().find_map(|(ri, r)| {
                r.eligible()
                    .then(|| {
                        state
                            .sends
                            .iter()
                            .position(|s| s.eligible() && !s.same_selection(r))
                            .map(|si| (si, ri))
                    })
                    .flatten()
            });
            if let Some((si, ri)) = pair {
                let send = state.sends.remove(si).unwrap();
                let receive = state.receives.remove(ri).unwrap();
                send.claim(wakes);
                receive.claim(wakes);
                receive.data.lock().unwrap().value = send.data.lock().unwrap().value.take();
                send.mark(Status::Committed, wakes);
                receive.mark(Status::Committed, wakes);
                continue;
            }
            if state.capacity.is_none_or(|cap| state.queue.len() < cap)
                && let Some(send) = state.sends.pop_front()
            {
                if !send.eligible() {
                    continue;
                }
                send.claim(wakes);
                if self.coordinator.buffered.load(Ordering::Relaxed) >= self.coordinator.limit {
                    send.mark(Status::Limit, wakes);
                } else {
                    self.coordinator.buffered.fetch_add(1, Ordering::AcqRel);
                    state
                        .queue
                        .push_back(send.data.lock().unwrap().value.take().unwrap());
                    send.mark(Status::Committed, wakes);
                }
                continue;
            }
            break;
        }
    }
    fn close(&self, discard: bool) {
        let mut wakes = vec![];
        let discarded = {
            let _gate = self.coordinator.gate.lock().unwrap();
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            let discarded = if discard {
                std::mem::take(&mut state.queue)
            } else {
                VecDeque::new()
            };
            self.coordinator
                .buffered
                .fetch_sub(discarded.len(), Ordering::AcqRel);
            self.progress(&mut state, &mut wakes);
            discarded
        };
        drop(discarded);
        wake_all(wakes);
    }
}

/// A non-executing operation offer. Creating an offer does not communicate.
pub struct Offer<T> {
    inner: Arc<Inner<T>>,
    op: Arc<Op<T>>,
    registered: bool,
    _sender: Option<Sender<T>>,
    _receiver: Option<Receiver<T>>,
}
impl<T> Offer<T> {
    fn register(&mut self, wakes: &mut Vec<Waker>) {
        if self.registered {
            return;
        }
        self.registered = true;
        let mut state = self.inner.state.lock().unwrap();
        match self.op.kind {
            Kind::Send => state.sends.push_back(self.op.clone()),
            Kind::Receive => state.receives.push_back(self.op.clone()),
        }
        self.inner.progress(&mut state, wakes);
    }
    fn cancel_locked(&mut self, retired: &mut Vec<Waker>) -> Option<T> {
        let mut state = self.inner.state.lock().unwrap();
        state.sends.retain(|op| op.id != self.op.id);
        state.receives.retain(|op| op.id != self.op.id);
        let mut data = self.op.data.lock().unwrap();
        if data.status == Status::Pending {
            data.status = Status::Cancelled;
        }
        retired.extend(data.waker.take());
        data.value.take()
    }
    fn result(&self) -> Option<CaseResult<T>> {
        let mut data = self.op.data.lock().unwrap();
        match (self.op.kind, data.status) {
            (_, Status::Pending | Status::Cancelled) => None,
            (Kind::Send, Status::Committed) => Some(CaseResult::Send(Ok(()))),
            (Kind::Receive, Status::Committed) => Some(CaseResult::Receive(Ok(data
                .value
                .take()
                .expect("receive result consumed once")))),
            (Kind::Send, status) => Some(CaseResult::Send(Err(SendError {
                kind: if status == Status::Limit {
                    SendFailure::Limit
                } else {
                    SendFailure::Closed
                },
                value: data.value.take().expect("failed send retains its value"),
            }))),
            (Kind::Receive, _) => Some(CaseResult::Receive(Err(ReceiveError::Closed))),
        }
    }
}
impl<T> Drop for Offer<T> {
    fn drop(&mut self) {
        let mut retired = vec![];
        let value = {
            let coordinator = self.inner.coordinator.clone();
            let _gate = coordinator.gate.lock().unwrap();
            self.cancel_locked(&mut retired)
        };
        drop(value);
    }
}
impl<T> Sender<T> {
    pub fn offer(&self, value: T) -> Offer<T> {
        Offer {
            inner: self.inner.clone(),
            op: Arc::new(Op {
                id: self.inner.coordinator.next_id(),
                kind: Kind::Send,
                data: Mutex::new(OpData {
                    status: Status::Pending,
                    value: Some(value),
                    waker: None,
                }),
                selection: None,
            }),
            registered: false,
            _sender: Some(self.clone()),
            _receiver: None,
        }
    }
    pub fn send(&self, value: T) -> SendFuture<T> {
        SendFuture {
            offer: self.offer(value),
            done: false,
        }
    }
    pub fn close(&self) {
        self.inner.close(false);
    }
    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }
    pub fn len(&self) -> usize {
        self.inner.state.lock().unwrap().queue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn try_send(&self, value: T) -> Result<(), SendError<T>> {
        let mut offer = self.offer(value);
        let coordinator = self.inner.coordinator.clone();
        let mut wakes = vec![];
        let mut retired = vec![];
        let result = {
            let _gate = coordinator.gate.lock().unwrap();
            offer.register(&mut wakes);
            match offer.result() {
                Some(CaseResult::Send(result)) => result,
                None => Err(SendError {
                    kind: SendFailure::Full,
                    value: offer.cancel_locked(&mut retired).unwrap(),
                }),
                _ => unreachable!(),
            }
        };
        wake_all(wakes);
        result
    }
}
impl<T> Receiver<T> {
    pub fn offer(&self) -> Offer<T> {
        Offer {
            inner: self.inner.clone(),
            op: Arc::new(Op {
                id: self.inner.coordinator.next_id(),
                kind: Kind::Receive,
                data: Mutex::new(OpData {
                    status: Status::Pending,
                    value: None,
                    waker: None,
                }),
                selection: None,
            }),
            registered: false,
            _sender: None,
            _receiver: Some(self.clone()),
        }
    }
    pub fn recv(&self) -> ReceiveFuture<T> {
        ReceiveFuture {
            offer: self.offer(),
            done: false,
        }
    }
    pub fn close(&self) {
        self.inner.close(false);
    }
    pub fn is_closed(&self) -> bool {
        self.inner.state.lock().unwrap().closed
    }
    pub fn len(&self) -> usize {
        self.inner.state.lock().unwrap().queue.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn try_recv(&self) -> Result<T, ReceiveError> {
        let mut offer = self.offer();
        let coordinator = self.inner.coordinator.clone();
        let mut wakes = vec![];
        let mut retired = vec![];
        let result = {
            let _gate = coordinator.gate.lock().unwrap();
            offer.register(&mut wakes);
            match offer.result() {
                Some(CaseResult::Receive(result)) => result,
                None => {
                    offer.cancel_locked(&mut retired);
                    Err(ReceiveError::Empty)
                }
                _ => unreachable!(),
            }
        };
        wake_all(wakes);
        result
    }
}
pub struct SendFuture<T> {
    offer: Offer<T>,
    done: bool,
}
pub struct ReceiveFuture<T> {
    offer: Offer<T>,
    done: bool,
}
impl<T> Unpin for SendFuture<T> {}
impl<T> Unpin for ReceiveFuture<T> {}
impl<T> Future for SendFuture<T> {
    type Output = Result<(), SendError<T>>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.done, "send polled after completion");
        let next = cx.waker().clone();
        let coordinator = self.offer.inner.coordinator.clone();
        let mut wakes = vec![];
        let mut retired = vec![];
        let result = {
            let _gate = coordinator.gate.lock().unwrap();
            retired.extend(self.offer.op.data.lock().unwrap().waker.replace(next));
            self.offer.register(&mut wakes);
            self.offer.result()
        };
        wake_all(wakes);
        if let Some(CaseResult::Send(result)) = result {
            self.done = true;
            Poll::Ready(result)
        } else {
            Poll::Pending
        }
    }
}
impl<T> Future for ReceiveFuture<T> {
    type Output = Result<T, ReceiveError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.done, "receive polled after completion");
        let next = cx.waker().clone();
        let coordinator = self.offer.inner.coordinator.clone();
        let mut wakes = vec![];
        let mut retired = vec![];
        let result = {
            let _gate = coordinator.gate.lock().unwrap();
            retired.extend(self.offer.op.data.lock().unwrap().waker.replace(next));
            self.offer.register(&mut wakes);
            self.offer.result()
        };
        wake_all(wakes);
        if let Some(CaseResult::Receive(result)) = result {
            self.done = true;
            Poll::Ready(result)
        } else {
            Poll::Pending
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CaseResult<T> {
    Send(Result<(), SendError<T>>),
    Receive(Result<T, ReceiveError>),
    Default,
    TimedOut,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Selected<T> {
    pub index: Option<usize>,
    pub result: CaseResult<T>,
    /// Values from losing send offers, returned without cloning or dropping.
    pub unsent: Vec<(usize, T)>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectError {
    Empty,
    DifferentFactories,
}
impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "selection requires at least one offer",
            Self::DifferentFactories => "selection offers belong to different channel factories",
        })
    }
}
impl std::error::Error for SelectError {}
pub struct Select<T> {
    offers: Vec<Offer<T>>,
    state: Arc<Mutex<SelectionState>>,
    coordinator: Arc<Coordinator>,
    started: bool,
    done: bool,
    default: bool,
}
impl<T> Unpin for Select<T> {}
pub fn select<T>(mut offers: Vec<Offer<T>>) -> Result<Select<T>, SelectError> {
    let Some(first) = offers.first() else {
        return Err(SelectError::Empty);
    };
    let coordinator = first.inner.coordinator.clone();
    if offers
        .iter()
        .any(|o| !Arc::ptr_eq(&o.inner.coordinator, &coordinator))
    {
        return Err(SelectError::DifferentFactories);
    }
    let state = Arc::new(Mutex::new(SelectionState {
        winner: NONE,
        waker: None,
    }));
    for (arm, offer) in offers.iter_mut().enumerate() {
        Arc::get_mut(&mut offer.op)
            .expect("offer not yet registered")
            .selection = Some((state.clone(), arm));
    }
    Ok(Select {
        offers,
        state,
        coordinator,
        started: false,
        done: false,
        default: false,
    })
}
impl<T> Select<T> {
    pub fn with_default(mut self) -> Self {
        self.default = true;
        self
    }
    /// The timeout future comes from an explicit clock capability.
    pub fn with_timeout<F: Future<Output = ()>>(self, timeout: F) -> SelectTimeout<T, F> {
        SelectTimeout {
            select: self,
            timeout: Box::pin(timeout),
        }
    }
    fn finish_locked(&mut self, winner: usize, retired: &mut Vec<Waker>) -> Selected<T> {
        let result = if winner == DEFAULT {
            CaseResult::Default
        } else if winner == TIMEOUT {
            CaseResult::TimedOut
        } else {
            self.offers[winner]
                .result()
                .expect("winning operation committed")
        };
        let mut unsent = vec![];
        for (index, offer) in self.offers.iter_mut().enumerate() {
            if index != winner
                && let Some(value) = offer.cancel_locked(retired)
            {
                unsent.push((index, value));
            }
        }
        self.done = true;
        Selected {
            index: (winner < self.offers.len()).then_some(winner),
            result,
            unsent,
        }
    }
}
impl<T> Future for Select<T> {
    type Output = Selected<T>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.done, "selection polled after completion");
        let next = cx.waker().clone();
        let coordinator = self.coordinator.clone();
        let mut wakes = vec![];
        let mut retired = vec![];
        let result = {
            let _gate = coordinator.gate.lock().unwrap();
            retired.extend(self.state.lock().unwrap().waker.replace(next));
            if !self.started {
                self.started = true;
                let start =
                    coordinator.turn.fetch_add(1, Ordering::Relaxed) as usize % self.offers.len();
                for offset in 0..self.offers.len() {
                    if self.state.lock().unwrap().winner != NONE {
                        break;
                    }
                    let arm = (start + offset) % self.offers.len();
                    self.offers[arm].register(&mut wakes);
                }
                if self.default && self.state.lock().unwrap().winner == NONE {
                    self.state.lock().unwrap().winner = DEFAULT;
                }
            }
            let winner = self.state.lock().unwrap().winner;
            if winner != NONE {
                Poll::Ready(self.finish_locked(winner, &mut retired))
            } else {
                Poll::Pending
            }
        };
        wake_all(wakes);
        result
    }
}
pub struct SelectTimeout<T, F> {
    select: Select<T>,
    timeout: Pin<Box<F>>,
}
impl<T> Drop for Select<T> {
    fn drop(&mut self) {
        let coordinator = self.coordinator.clone();
        let mut retired = vec![];
        let values = {
            let _gate = coordinator.gate.lock().unwrap();
            let mut state = self.state.lock().unwrap();
            if state.winner == NONE {
                state.winner = DEFAULT;
            }
            retired.extend(state.waker.take());
            drop(state);
            self.offers
                .iter_mut()
                .filter_map(|offer| offer.cancel_locked(&mut retired))
                .collect::<Vec<_>>()
        };
        drop(values);
    }
}
impl<T, F> Unpin for SelectTimeout<T, F> {}
impl<T, F: Future<Output = ()>> Future for SelectTimeout<T, F> {
    type Output = Selected<T>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(result) = Pin::new(&mut self.select).poll(cx) {
            return Poll::Ready(result);
        }
        if self.timeout.as_mut().poll(cx).is_pending() {
            return Poll::Pending;
        }
        let coordinator = self.select.coordinator.clone();
        let mut retired = vec![];
        let _gate = coordinator.gate.lock().unwrap();
        let winner = {
            let mut s = self.select.state.lock().unwrap();
            if s.winner == NONE {
                s.winner = TIMEOUT;
            }
            s.winner
        };
        let result = self.select.finish_locked(winner, &mut retired);
        drop(_gate);
        Poll::Ready(result)
    }
}
