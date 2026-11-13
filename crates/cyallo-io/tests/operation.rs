use cyallo_io::{Cancelled, OperationRegistry};
use std::{
    future::Future,
    pin::pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

struct Count(AtomicUsize);
impl Wake for Count {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn dropping_last_completer_cancels_and_wakes_the_waiter() {
    let registry = OperationRegistry::default();
    let (op, completion) = registry.operation::<()>();
    let other = completion.clone();
    let mut op = pin!(op);
    let wakes = Arc::new(Count(AtomicUsize::new(0)));
    let waker = Waker::from(wakes.clone());
    let mut cx = Context::from_waker(&waker);
    assert!(op.as_mut().poll(&mut cx).is_pending());
    drop(completion);
    assert_eq!(wakes.0.load(Ordering::SeqCst), 0);
    drop(other);
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    assert_eq!(op.as_mut().poll(&mut cx), Poll::Ready(Err(Cancelled)));
}

#[test]
fn replacing_a_cancel_hook_drops_its_capture_outside_the_lock() {
    struct Reenter(cyallo_io::Completer<()>);
    impl Drop for Reenter {
        fn drop(&mut self) {
            self.0.is_cancelled();
        }
    }
    let registry = OperationRegistry::default();
    let (op, completion) = registry.operation();
    let capture = Reenter(completion.clone());
    completion.on_cancel(move || drop(capture));
    completion.on_cancel(|| {});
    assert!(op.cancel());
    assert_eq!(completion.complete(()), Err(()));
}

#[test]
fn separate_registries_cannot_complete_each_others_operations() {
    let first = OperationRegistry::default();
    let second = OperationRegistry::default();
    let (a, complete_a) = first.operation();
    let (b, complete_b) = second.operation();
    assert_eq!(a.id(), b.id());
    complete_a.complete("a").unwrap();
    complete_b.complete("b").unwrap();
    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut cx = Context::from_waker(Waker::noop());
    assert_eq!(a.as_mut().poll(&mut cx), Poll::Ready(Ok("a")));
    assert_eq!(b.as_mut().poll(&mut cx), Poll::Ready(Ok("b")));
}
