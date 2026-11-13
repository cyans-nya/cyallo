use cyallo_host::BlockingPool;
use cyallo_io::{Cancelled, OperationRegistry};
use std::{
    future::Future,
    pin::pin,
    sync::{Arc, mpsc},
    task::{Context, Poll, Waker},
    time::Duration,
};

#[test]
fn queue_is_bounded_and_shutdown_cancels_queued_work() {
    let pool = BlockingPool::with_limit(1, 1, Arc::new(OperationRegistry::default())).unwrap();
    let (started, ready) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let running = pool.submit(move || {
        started.send(()).unwrap();
        released.recv().unwrap();
    });
    ready.recv_timeout(Duration::from_secs(1)).unwrap();
    let queued = pool.submit(|| 42);
    let rejected = pool.submit(|| 43);
    let mut rejected = pin!(rejected);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        matches!(rejected.as_mut().poll(&mut cx), Poll::Ready(Ok(Err(cyallo_host::HostError::QueueFull))))
    );
    pool.stop();
    let mut queued = pin!(queued);
    assert_eq!(queued.as_mut().poll(&mut cx), Poll::Ready(Err(Cancelled)));
    release.send(()).unwrap();
    drop(running);
}
