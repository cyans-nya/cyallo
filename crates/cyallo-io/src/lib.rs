//! Single-commit host operations. Registration precedes submission, so an early
//! completion is retained even when the coroutine has not parked yet.
mod panic;
pub use panic::PanicPayload;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll, Waker},
};

/// Allocated and owned by a runtime or an embedding host, never a singleton.
pub struct OperationRegistry {
    next: AtomicU64,
}
impl Default for OperationRegistry {
    fn default() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("operation cancelled")
    }
}
impl std::error::Error for Cancelled {}
struct State<T> {
    value: Option<T>,
    committed: bool,
    cancelled: bool,
    waker: Option<Waker>,
    cancel: Option<Box<dyn FnOnce() + Send>>,
    completers: usize,
}
struct Inner<T> {
    id: OperationId,
    state: Mutex<State<T>>,
}
pub struct Operation<T> {
    inner: Arc<Inner<T>>,
}
pub struct Completer<T> {
    inner: Arc<Inner<T>>,
}
impl<T> Clone for Completer<T> {
    fn clone(&self) -> Self {
        self.inner.state.lock().unwrap().completers += 1;
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl OperationRegistry {
    pub fn operation<T>(&self) -> (Operation<T>, Completer<T>) {
        let id = self
            .next
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .expect("operation identity space exhausted");
        let inner = Arc::new(Inner {
            id: OperationId(id),
            state: Mutex::new(State {
                value: None,
                committed: false,
                cancelled: false,
                waker: None,
                cancel: None,
                completers: 1,
            }),
        });
        (
            Operation {
                inner: inner.clone(),
            },
            Completer { inner },
        )
    }
}
impl<T> Operation<T> {
    pub fn id(&self) -> OperationId {
        self.inner.id
    }
    /// Cancellation cannot roll back an already committed completion.
    pub fn cancel(&self) -> bool {
        self.inner.cancel()
    }
}
impl<T> Inner<T> {
    fn cancel(&self) -> bool {
        let (wake, cancel) = {
            let mut s = self.state.lock().unwrap();
            if s.committed || s.cancelled {
                return false;
            }
            s.cancelled = true;
            (s.waker.take(), s.cancel.take())
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        if let Some(cancel) = cancel {
            cancel();
        }
        true
    }
}
impl<T> Completer<T> {
    pub fn id(&self) -> OperationId {
        self.inner.id
    }
    pub fn is_cancelled(&self) -> bool {
        self.inner.state.lock().unwrap().cancelled
    }
    pub fn on_cancel(&self, cancel: impl FnOnce() + Send + 'static) {
        let mut cancel = Some(Box::new(cancel) as Box<dyn FnOnce() + Send>);
        let previous = {
            let mut s = self.inner.state.lock().unwrap();
            if s.committed {
                return;
            }
            if !s.cancelled {
                std::mem::replace(&mut s.cancel, cancel.take())
            } else {
                None
            }
        };
        drop(previous);
        if let Some(cancel) = cancel {
            cancel();
        }
    }
    /// Rejected or duplicate completions return their payload to the adapter.
    pub fn complete(&self, value: T) -> Result<(), T> {
        let (wake, cancel) = {
            let mut s = self.inner.state.lock().unwrap();
            if s.cancelled || s.committed {
                return Err(value);
            }
            s.committed = true;
            s.value = Some(value);
            (s.waker.take(), s.cancel.take())
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        drop(cancel);
        Ok(())
    }
}
impl<T> Future for Operation<T> {
    type Output = Result<T, Cancelled>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let next = cx.waker().clone();
        let mut s = self.inner.state.lock().unwrap();
        if s.cancelled {
            return Poll::Ready(Err(Cancelled));
        }
        if let Some(value) = s.value.take() {
            return Poll::Ready(Ok(value));
        }
        assert!(!s.committed, "operation polled after completion");
        let previous = s.waker.replace(next);
        drop(s);
        drop(previous);
        Poll::Pending
    }
}
impl<T> Drop for Completer<T> {
    fn drop(&mut self) {
        let abandoned = {
            let mut s = self.inner.state.lock().unwrap();
            s.completers -= 1;
            s.completers == 0 && !s.committed && !s.cancelled
        };
        if abandoned {
            self.inner.cancel();
        }
    }
}
impl<T> Drop for Operation<T> {
    fn drop(&mut self) {
        self.cancel();
    }
}
