use cyallo_clock::TimerService;
use std::{
    future::Future,
    pin::pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    time::Duration,
};

struct Notify(std::sync::mpsc::Sender<()>);
impl Wake for Notify {
    fn wake(self: Arc<Self>) {
        self.0.send(()).unwrap();
    }
}

#[test]
fn stopping_a_clock_wakes_registered_sleepers() {
    let service = TimerService::new();
    let mut sleep = pin!(service.clock().sleep(Duration::from_secs(3600)));
    let (tx, rx) = std::sync::mpsc::channel();
    let waker = Waker::from(Arc::new(Notify(tx)));
    let mut cx = Context::from_waker(&waker);
    assert!(sleep.as_mut().poll(&mut cx).is_pending());
    service.stop();
    rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(sleep.as_mut().poll(&mut cx), Poll::Ready(()));
}

#[test]
fn clocks_are_independent_and_do_not_keep_services_alive() {
    let first = TimerService::new();
    let second = TimerService::new();
    let clock = first.clock();
    drop(first);
    let mut first_sleep = pin!(clock.sleep(Duration::from_secs(3600)));
    let mut second_sleep = pin!(second.clock().sleep(Duration::from_secs(3600)));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(first_sleep.as_mut().poll(&mut cx).is_ready());
    assert!(second_sleep.as_mut().poll(&mut cx).is_pending());
}
