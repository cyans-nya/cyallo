use cyallo_channel::{CaseResult, Factory, ReceiveError, SendFailure, select};
use cyallo_scheduler::{Config, Outcome, Runtime};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

fn runtime(processors: usize) -> Runtime {
    Runtime::new(Config {
        processors,
        ..Default::default()
    })
    .unwrap()
}
fn poll<F: Future + Unpin>(future: &mut F) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
}

#[test]
fn factories_have_independent_quotas_and_cannot_mix_selection_arms() {
    let first = Factory::new(1);
    let second = Factory::new(1);
    let (a, receive_a) = first.bounded(1);
    let (b, receive_b) = second.bounded(1);
    a.try_send(1).unwrap();
    b.try_send(2).unwrap();
    assert_eq!((first.buffered(), second.buffered()), (1, 1));
    assert!(matches!(
        select(vec![receive_a.offer(), receive_b.offer()]),
        Err(cyallo_channel::SelectError::DifferentFactories)
    ));
    assert_eq!(receive_a.try_recv(), Ok(1));
    assert_eq!(receive_b.try_recv(), Ok(2));
}

#[test]
fn replacing_or_cancelling_a_waker_drops_it_outside_channel_locks() {
    struct Reenter(cyallo_channel::Sender<usize>);
    impl std::task::Wake for Reenter {
        fn wake(self: Arc<Self>) {
            self.0.close();
        }
    }
    impl Drop for Reenter {
        fn drop(&mut self) {
            self.0.is_closed();
        }
    }
    let factory = Factory::default();
    let (sender, receiver) = factory.bounded(0);
    let mut future = receiver.recv();
    let waker = Waker::from(Arc::new(Reenter(sender.clone())));
    assert!(
        Pin::new(&mut future)
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    drop(waker);
    assert!(poll(&mut future).is_pending());
    let waker = Waker::from(Arc::new(Reenter(sender)));
    assert!(
        Pin::new(&mut future)
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    drop(waker);
    drop(future);
}

#[test]
fn bounded_fifo_and_failed_sends_preserve_values() {
    let factory = Factory::default();
    let (s, r) = factory.bounded(2);
    s.try_send("one".to_string()).unwrap();
    s.try_send("two".to_string()).unwrap();
    let error = s.try_send("three".to_string()).unwrap_err();
    assert_eq!(
        (error.kind, error.value.as_str()),
        (SendFailure::Full, "three")
    );
    assert_eq!(r.try_recv().unwrap(), "one");
    assert_eq!(r.try_recv().unwrap(), "two");
    assert_eq!(r.try_recv(), Err(ReceiveError::Empty));
}

#[test]
fn rendezvous_parks_with_one_processor() {
    let factory = Factory::default();
    let runtime = runtime(1);
    let (s, r) = factory.bounded(0);
    let reader = runtime
        .spawn_future(async move { r.recv().await.unwrap() })
        .unwrap();
    let writer = runtime
        .spawn_future(async move {
            s.send(42).await.unwrap();
        })
        .unwrap();
    assert_eq!(reader.join(), Outcome::Completed(42));
    assert_eq!(writer.join(), Outcome::Completed(()));
}

#[test]
fn mpmc_delivers_every_message_exactly_once() {
    let factory = Factory::default();
    let runtime = runtime(4);
    let (s, r) = factory.bounded::<usize>(7);
    let received = Arc::new((0..4000).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>());
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let r = r.clone();
            let received = received.clone();
            runtime
                .spawn_future(async move {
                    while let Ok(value) = r.recv().await {
                        received[value].fetch_add(1, Ordering::SeqCst);
                    }
                })
                .unwrap()
        })
        .collect();
    drop(r);
    let writers: Vec<_> = (0..4)
        .map(|producer| {
            let s = s.clone();
            runtime
                .spawn_future(async move {
                    for i in 0..1000 {
                        s.send(producer * 1000 + i).await.unwrap();
                    }
                })
                .unwrap()
        })
        .collect();
    drop(s);
    for task in writers {
        assert_eq!(task.join(), Outcome::Completed(()));
    }
    for task in readers {
        assert_eq!(task.join(), Outcome::Completed(()));
    }
    assert!(received.iter().all(|n| n.load(Ordering::SeqCst) == 1));
}

#[test]
fn either_endpoint_closes_and_buffer_drains() {
    let factory = Factory::default();
    let (s, r) = factory.unbounded();
    s.try_send(1).unwrap();
    s.try_send(2).unwrap();
    r.close();
    r.close();
    s.close();
    assert_eq!(s.try_send(3).unwrap_err().value, 3);
    assert_eq!(r.try_recv(), Ok(1));
    assert_eq!(r.try_recv(), Ok(2));
    assert_eq!(r.try_recv(), Err(ReceiveError::Closed));
}

#[test]
fn quota_failure_returns_payload_and_releases_reservations() {
    let factory = Factory::new(1);
    let (s, r) = factory.unbounded();
    s.try_send(10).unwrap();
    assert_eq!(s.try_send(20).unwrap_err().kind, SendFailure::Limit);
    assert_eq!(factory.buffered(), 1);
    assert_eq!(r.try_recv(), Ok(10));
    assert_eq!(factory.buffered(), 0);
    s.try_send(30).unwrap();
    drop(r);
    assert_eq!(factory.buffered(), 0);
    assert_eq!(s.try_send(40).unwrap_err().value, 40);
}

#[test]
fn select_commits_only_one_ready_receive() {
    let factory = Factory::default();
    let runtime = runtime(1);
    let (a, ar) = factory.bounded(1);
    let (b, br) = factory.bounded(1);
    a.try_send(1).unwrap();
    b.try_send(2).unwrap();
    let selection = runtime
        .spawn_future(select(vec![ar.offer(), br.offer()]).unwrap())
        .unwrap()
        .join();
    let Outcome::Completed(selection) = selection else {
        panic!("selection failed")
    };
    assert!(matches!(selection.result, CaseResult::Receive(Ok(1 | 2))));
    let remaining = [ar.try_recv(), br.try_recv()]
        .into_iter()
        .filter(Result::is_ok)
        .count();
    assert_eq!(remaining, 1);
}

#[test]
fn losing_sends_return_owned_payloads() {
    let factory = Factory::default();
    let (s, r) = factory.bounded(0);
    let mut selection = select(vec![s.offer(String::from("retained")), r.offer()])
        .unwrap()
        .with_default();
    let Poll::Ready(selected) = poll(&mut selection) else {
        panic!("default did not complete")
    };
    // A select cannot rendezvous with itself.
    assert_eq!(selected.result, CaseResult::Default);
    assert_eq!(selected.unsent, vec![(0, "retained".to_string())]);
}

#[test]
fn cancelling_selection_does_not_leave_a_receive_behind() {
    let factory = Factory::default();
    let (s, r) = factory.bounded(1);
    let mut selection = select(vec![r.offer()]).unwrap();
    assert!(poll(&mut selection).is_pending());
    drop(selection);
    s.try_send(42).unwrap();
    assert_eq!(r.try_recv(), Ok(42));
}

#[test]
fn two_selections_can_rendezvous_without_double_commit() {
    let factory = Factory::default();
    let runtime = runtime(2);
    let (s, r) = factory.bounded(0);
    let send = runtime
        .spawn_future(select(vec![s.offer(42)]).unwrap())
        .unwrap();
    let receive = runtime
        .spawn_future(select(vec![r.offer()]).unwrap())
        .unwrap();
    assert!(matches!(send.join(), Outcome::Completed(v) if v.result == CaseResult::Send(Ok(()))));
    assert!(
        matches!(receive.join(), Outcome::Completed(v) if v.result == CaseResult::Receive(Ok(42)))
    );
}

#[test]
fn timeout_removes_all_waiters() {
    let factory = Factory::default();
    let runtime = runtime(1);
    let (s, r) = factory.bounded(1);
    let choice = select(vec![r.offer()])
        .unwrap()
        .with_timeout(runtime.clock().sleep(Duration::from_millis(1)));
    assert!(
        matches!(runtime.spawn_future(choice).unwrap().join(), Outcome::Completed(v) if v.result == CaseResult::TimedOut)
    );
    s.try_send(8).unwrap();
    assert_eq!(r.try_recv(), Ok(8));
}

#[test]
fn close_wakes_pending_operations() {
    let factory = Factory::default();
    let runtime = runtime(2);
    let (s, r) = factory.bounded::<i64>(0);
    let recv = r.clone();
    let task = runtime
        .spawn_future(async move { recv.recv().await })
        .unwrap();
    s.close();
    assert_eq!(task.join(), Outcome::Completed(Err(ReceiveError::Closed)));
}

#[test]
fn concurrently_ready_losing_arm_never_consumes_a_message() {
    let factory = Factory::default();
    for _ in 0..100 {
        let (a, ar) = factory.bounded(1);
        let (b, br) = factory.bounded(1);
        let mut selection = select(vec![ar.offer(), br.offer()]).unwrap();
        assert!(poll(&mut selection).is_pending());
        let barrier = Arc::new(Barrier::new(3));
        let first = barrier.clone();
        let second = barrier.clone();
        let left = std::thread::spawn(move || {
            first.wait();
            a.try_send(7)
        });
        let right = std::thread::spawn(move || {
            second.wait();
            b.try_send(8)
        });
        barrier.wait();
        assert!(left.join().unwrap().is_ok());
        assert!(right.join().unwrap().is_ok());
        let Poll::Ready(completed) = poll(&mut selection) else {
            panic!("selection was not readied")
        };
        assert!(matches!(completed.result, CaseResult::Receive(Ok(7 | 8))));
        drop(selection);
        assert_eq!(
            [ar.try_recv(), br.try_recv()]
                .iter()
                .filter(|v| v.is_ok())
                .count(),
            1
        );
    }
}
