//! Instance-owned timers. Dropping or stopping the service releases its sleepers;
//! a Clock is a weak capability and never keeps the service alive.
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, Weak},
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

struct State {
    stopped: bool,
    next: u64,
    timers: BTreeMap<(Instant, u64), Weak<Mutex<Alarm>>>,
}
struct Inner {
    state: Mutex<State>,
    changed: Condvar,
}
struct Alarm {
    ready: bool,
    waker: Option<Waker>,
}
pub struct TimerService {
    inner: Arc<Inner>,
    thread: Option<thread::JoinHandle<()>>,
}
impl TimerService {
    pub fn new() -> Self {
        Self::try_new().expect("create timer thread")
    }
    pub fn try_new() -> std::io::Result<Self> {
        let inner = Arc::new(Inner {
            state: Mutex::new(State {
                stopped: false,
                next: 0,
                timers: BTreeMap::new(),
            }),
            changed: Condvar::new(),
        });
        let shared = inner.clone();
        let thread = thread::Builder::new()
            .name("cyallo-timer".into())
            .spawn(move || {
                let mut s = shared.state.lock().unwrap();
                while !s.stopped {
                    let Some((&key, _)) = s.timers.first_key_value() else {
                        s = shared.changed.wait(s).unwrap();
                        continue;
                    };
                    if let Some(delay) = key.0.checked_duration_since(Instant::now()) {
                        s = shared.changed.wait_timeout(s, delay).unwrap().0;
                        continue;
                    }
                    let alarm = s.timers.remove(&key).unwrap().upgrade();
                    drop(s);
                    if let Some(alarm) = alarm {
                        let wake = {
                            let mut a = alarm.lock().unwrap();
                            a.ready = true;
                            a.waker.take()
                        };
                        if let Some(w) = wake {
                            w.wake();
                        }
                    }
                    s = shared.state.lock().unwrap();
                }
            })?;
        Ok(Self {
            inner,
            thread: Some(thread),
        })
    }
    pub fn clock(&self) -> Clock {
        Clock {
            inner: Arc::downgrade(&self.inner),
        }
    }
    pub fn stop(&self) {
        let alarms = {
            let mut s = self.inner.state.lock().unwrap();
            s.stopped = true;
            self.inner.changed.notify_all();
            std::mem::take(&mut s.timers)
        };
        for alarm in alarms.into_values().filter_map(|a| a.upgrade()) {
            let wake = {
                let mut alarm = alarm.lock().unwrap();
                alarm.ready = true;
                alarm.waker.take()
            };
            if let Some(wake) = wake {
                wake.wake();
            }
        }
    }
}
impl Default for TimerService {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for TimerService {
    fn drop(&mut self) {
        self.stop();
        if let Some(t) = self.thread.take()
            && t.thread().id() != thread::current().id()
        {
            let _ = t.join();
        }
    }
}
/// Explicit time capability. A Clock does not keep its Runtime alive.
#[derive(Clone)]
pub struct Clock {
    inner: Weak<Inner>,
}
impl Clock {
    pub fn sleep(&self, duration: Duration) -> Sleep {
        self.until(Instant::now() + duration)
    }
    pub fn until(&self, deadline: Instant) -> Sleep {
        Sleep {
            clock: self.inner.clone(),
            deadline,
            alarm: Arc::new(Mutex::new(Alarm {
                ready: false,
                waker: None,
            })),
            key: None,
        }
    }
}
pub struct Sleep {
    clock: Weak<Inner>,
    deadline: Instant,
    alarm: Arc<Mutex<Alarm>>,
    key: Option<(Instant, u64)>,
}
impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if Instant::now() >= self.deadline {
            return Poll::Ready(());
        }
        let Some(clock) = self.clock.upgrade() else {
            return Poll::Ready(());
        };
        let next = cx.waker().clone();
        let previous = {
            let mut alarm = self.alarm.lock().unwrap();
            if alarm.ready {
                return Poll::Ready(());
            }
            alarm.waker.replace(next)
        };
        drop(previous);
        if self.key.is_none() {
            let mut s = clock.state.lock().unwrap();
            if s.stopped {
                return Poll::Ready(());
            }
            let key = (self.deadline, s.next);
            s.next = s
                .next
                .checked_add(1)
                .expect("timer identity space exhausted");
            s.timers.insert(key, Arc::downgrade(&self.alarm));
            self.key = Some(key);
            clock.changed.notify_one();
        }
        Poll::Pending
    }
}
impl Drop for Sleep {
    fn drop(&mut self) {
        if let (Some(clock), Some(key)) = (self.clock.upgrade(), self.key) {
            clock.state.lock().unwrap().timers.remove(&key);
            clock.changed.notify_one();
        }
    }
}
