use cyallo_scheduler::{Config, Context, Coroutine, Outcome, Runtime};
#[derive(Debug, PartialEq, Eq)]
enum TestError {
    GuestFailure,
}
impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GuestFailure => f.write_str("guest failure"),
        }
    }
}
impl std::error::Error for TestError {}
use std::{
    pin::Pin,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    task::Poll,
    time::Duration,
};

fn runtime(processors: usize) -> Runtime {
    Runtime::new(Config {
        processors,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn multiple_processors_really_execute_in_parallel() {
    let runtime = runtime(2);
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let barrier = barrier.clone();
            runtime
                .spawn_future(async move {
                    barrier.wait();
                    i
                })
                .unwrap()
        })
        .collect();
    for (i, h) in handles.into_iter().enumerate() {
        assert_eq!(h.join(), Outcome::Completed(i));
    }
    assert_eq!(runtime.stats().peak_running, 2);
}

struct Slices {
    left: usize,
}
impl Coroutine for Slices {
    type Output = (u64, usize);
    fn resume(&mut self, cx: &mut Context) -> Poll<Self::Output> {
        assert!(cx.consume(cx.fuel()));
        self.left -= 1;
        if self.left == 0 {
            Poll::Ready((cx.id().0, cx.processor()))
        } else {
            cx.yield_now();
            Poll::Pending
        }
    }
}
#[test]
fn wake_while_running_is_not_lost() {
    let r = runtime(2);
    let tasks: Vec<_> = (0..64)
        .map(|_| r.spawn(Slices { left: 100 }).unwrap())
        .collect();
    for task in tasks {
        assert!(matches!(task.join(), Outcome::Completed(_)));
    }
    r.wait();
    assert_eq!(r.stats().coroutines, 0);
}

#[test]
fn pending_io_does_not_reserve_a_processor() {
    let r = runtime(1);
    let (input, complete) = r.operation();
    let reader = r.spawn_future(async move { input.await.unwrap() }).unwrap();
    let writer = r
        .spawn_future(async move {
            complete.complete(42).unwrap();
        })
        .unwrap();
    assert_eq!(writer.join(), Outcome::Completed(()));
    assert_eq!(reader.join(), Outcome::Completed(42));
}

#[test]
fn early_completion_and_cancellation_are_single_commit() {
    let r = runtime(1);
    let (op, complete) = r.operation();
    complete.complete(7).unwrap();
    assert_eq!(complete.complete(8), Err(8));
    assert!(!op.cancel());
    assert_eq!(
        r.spawn_future(op).unwrap().join(),
        Outcome::Completed(Ok(7))
    );
    let (op, complete) = r.operation();
    assert!(op.cancel());
    assert_eq!(complete.complete(9), Err(9));
    assert_eq!(
        r.spawn_future(op).unwrap().join(),
        Outcome::Completed(Err(cyallo_scheduler::Cancelled))
    );
}

#[test]
fn blocking_host_work_runs_without_a_processor() {
    let r = runtime(1);
    let barrier = Arc::new(Barrier::new(2));
    let host_barrier = barrier.clone();
    let op = r.blocking(move || {
        host_barrier.wait();
        99
    });
    let guest = r
        .spawn_future(async move {
            barrier.wait();
        })
        .unwrap();
    assert_eq!(guest.join(), Outcome::Completed(()));
    assert_eq!(
        r.spawn_future(op).unwrap().join(),
        Outcome::Completed(Ok(Ok(99)))
    );
    assert_eq!(r.stats().peak_running, 1);
}

#[test]
fn timer_parks_and_cancel_wakes_a_waiting_task() {
    let r = runtime(1);
    let sleep = r.clock().sleep(Duration::from_secs(60));
    let task = r.spawn_future(sleep).unwrap();
    task.cancel();
    assert_eq!(task.join(), Outcome::Cancelled);
    let sleep = r.clock().sleep(Duration::from_millis(2));
    assert_eq!(
        r.spawn_future(sleep).unwrap().join(),
        Outcome::Completed(())
    );
}

struct Cleanup {
    operation: cyallo_scheduler::Operation<()>,
    drops: Arc<AtomicUsize>,
}
impl Coroutine for Cleanup {
    type Output = ();
    fn resume(&mut self, _: &mut Context) -> Poll<()> {
        Poll::Pending
    }
    fn cancel(&mut self, cx: &mut Context) -> Poll<()> {
        cx.poll(Pin::new(&mut self.operation)).map(|_| ())
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn cleanup_can_park_and_runs_once() {
    let r = runtime(1);
    let (operation, complete) = r.operation();
    let drops = Arc::new(AtomicUsize::new(0));
    let task = r
        .spawn(Cleanup {
            operation,
            drops: drops.clone(),
        })
        .unwrap();
    task.cancel();
    let sender = r
        .spawn_future(async move {
            complete.complete(()).unwrap();
        })
        .unwrap();
    assert_eq!(sender.join(), Outcome::Completed(()));
    assert_eq!(task.join(), Outcome::Cancelled);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

#[test]
fn failures_do_not_stop_independent_coroutines() {
    let r = runtime(2);
    let failed = r.spawn_future(async { panic!("source failure") }).unwrap();
    let healthy = r.spawn_future(async { 42 }).unwrap();
    assert!(matches!(
        failed.join(),
        Outcome::Failed(cyallo_scheduler::Failure::Panic(_))
    ));
    assert_eq!(healthy.join(), Outcome::Completed(42));
    assert_eq!(r.failures().len(), 1);
}

#[test]
fn group_failure_cancels_parked_sibling() {
    let r = runtime(2);
    let group = r.group();
    let (op, _complete) = r.operation::<()>();
    let sibling = group.spawn_future(op).unwrap();
    let failed = group
        .spawn_future(async { panic!("group failure") })
        .unwrap();
    assert_eq!(group.wait().len(), 1);
    assert!(matches!(failed.join(), Outcome::Failed(_)));
    assert_eq!(sibling.join(), Outcome::Cancelled);
}

#[test]
fn future_join_parks_instead_of_blocking_t() {
    let r = runtime(1);
    let child = r.spawn(Slices { left: 20 }).unwrap();
    let parent = r.spawn_future(child).unwrap();
    assert!(matches!(
        parent.join(),
        Outcome::Completed(Outcome::Completed(_))
    ));
}

#[test]
fn stopping_runtime_completes_parked_handles_and_rejects_spawns() {
    let r = runtime(2);
    let (op, _completion) = r.operation::<()>();
    let task = r.spawn_future(op).unwrap();
    r.stop();
    assert_eq!(task.join(), Outcome::Stopped);
    assert!(matches!(
        r.spawn_future(async {}),
        Err(cyallo_scheduler::SpawnError::Stopped)
    ));
}

#[test]
fn operation_and_runtime_identity_are_instance_owned() {
    let a = runtime(1);
    let b = runtime(1);
    let (op_a, complete_a) = a.operation();
    let (op_b, complete_b) = b.operation();
    assert_eq!(op_a.id(), op_b.id()); // IDs are scoped to their registry.
    complete_a.complete("a").unwrap();
    complete_b.complete("b").unwrap();
    assert_eq!(
        a.spawn_future(op_a).unwrap().join(),
        Outcome::Completed(Ok("a"))
    );
    assert_eq!(
        b.spawn_future(op_b).unwrap().join(),
        Outcome::Completed(Ok("b"))
    );
}

#[test]
fn group_wait_parks_on_one_processor_and_result_errors_are_values() {
    let r = runtime(1);
    let group = r.group();
    let child = group.spawn(Slices { left: 20 }).unwrap();
    let waiter = r.spawn_future(group.wait_async()).unwrap();
    assert_eq!(waiter.join(), Outcome::Completed(vec![]));
    assert!(matches!(child.join(), Outcome::Completed(_)));
    assert!(matches!(
        group.spawn(Slices { left: 1 }),
        Err(cyallo_scheduler::SpawnError::GroupClosed)
    ));
    let normal_error = r
        .spawn_future(async { Err::<(), _>("ordinary error") })
        .unwrap();
    assert_eq!(
        normal_error.join(),
        Outcome::Completed(Err("ordinary error"))
    );
    assert!(r.failures().is_empty());
}

#[test]
fn a_trap_cancels_siblings_before_waiting_for_cleanup() {
    struct Trap {
        cleanup: cyallo_scheduler::Operation<()>,
    }
    impl Coroutine for Trap {
        type Output = ();
        fn resume(&mut self, cx: &mut Context) -> Poll<()> {
            cx.fail(TestError::GuestFailure);
            Poll::Pending
        }
        fn cancel(&mut self, cx: &mut Context) -> Poll<()> {
            cx.poll(Pin::new(&mut self.cleanup)).map(|_| ())
        }
    }
    struct Sibling(Option<cyallo_scheduler::Completer<()>>);
    impl Coroutine for Sibling {
        type Output = ();
        fn resume(&mut self, _: &mut Context) -> Poll<()> {
            Poll::Pending
        }
        fn cancel(&mut self, _: &mut Context) -> Poll<()> {
            self.0.take().unwrap().complete(()).unwrap();
            Poll::Ready(())
        }
    }
    let r = runtime(1);
    let group = r.group();
    let (cleanup, complete) = r.operation();
    let sibling = group.spawn(Sibling(Some(complete))).unwrap();
    let failed = group.spawn(Trap { cleanup }).unwrap();
    assert!(
        matches!(failed.join(), Outcome::Failed(cyallo_scheduler::Failure::Guest(error)) if error.downcast_ref::<TestError>() == Some(&TestError::GuestFailure))
    );
    assert_eq!(sibling.join(), Outcome::Cancelled);
    assert_eq!(group.wait().len(), 1);
}

#[test]
fn graceful_cancel_rejects_new_submissions() {
    let r = runtime(1);
    let spawner = r.spawner();
    r.cancel_and_wait();
    assert!(matches!(
        spawner.spawn(Slices { left: 1 }),
        Err(cyallo_scheduler::SpawnError::Stopped)
    ));
}

#[test]
fn host_execution_uses_the_same_processor_budget() {
    let r = runtime(1);
    let permit = r.enter().unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let observed = ran.clone();
    let child = r
        .spawn_future(async move {
            observed.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert_eq!(r.stats().running, 1);
    drop(permit);
    assert_eq!(child.join(), Outcome::Completed(()));
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert_eq!(r.stats().peak_running, 1);
}

#[test]
fn a_busy_coroutine_hands_its_processor_to_a_host_waiter() {
    struct Busy {
        started: Option<std::sync::mpsc::Sender<()>>,
        finish: Arc<AtomicUsize>,
    }
    impl Coroutine for Busy {
        type Output = ();
        fn resume(&mut self, cx: &mut Context) -> Poll<()> {
            if let Some(started) = self.started.take() {
                started.send(()).unwrap();
            }
            if self.finish.load(Ordering::SeqCst) != 0 {
                return Poll::Ready(());
            }
            cx.yield_now();
            Poll::Pending
        }
    }
    let r = runtime(1);
    let (started, receive) = std::sync::mpsc::channel();
    let finish = Arc::new(AtomicUsize::new(0));
    let child = r
        .spawn(Busy {
            started: Some(started),
            finish: finish.clone(),
        })
        .unwrap();
    receive.recv_timeout(Duration::from_secs(1)).unwrap();
    let permit = r.enter().unwrap();
    finish.store(1, Ordering::SeqCst);
    drop(permit);
    assert_eq!(child.join(), Outcome::Completed(()));
    assert_eq!(r.stats().peak_running, 1);
}
