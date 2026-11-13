//! Blocking host work runs on bounded OS Ts without a P. These callbacks own
//! their arguments and must never borrow guest execution/heap storage.
mod error;
pub use error::{HostError, StartError};
use cyallo_io::{Operation, OperationRegistry};
use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Condvar, Mutex},
    thread,
};
type Job = Box<dyn FnOnce() + Send>;
struct State {
    stopped: bool,
    jobs: VecDeque<Job>,
}
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}
pub struct BlockingPool {
    shared: Arc<Shared>,
    operations: Arc<OperationRegistry>,
    max_pending: usize,
}
impl BlockingPool {
    pub fn new(threads: usize, operations: Arc<OperationRegistry>) -> Result<Self, StartError> {
        Self::with_limit(threads, 1024, operations)
    }
    pub fn with_limit(
        threads: usize,
        max_pending: usize,
        operations: Arc<OperationRegistry>,
    ) -> Result<Self, StartError> {
        if threads == 0 || max_pending == 0 {
            return Err(StartError::ZeroLimit);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                stopped: false,
                jobs: VecDeque::new(),
            }),
            ready: Condvar::new(),
        });
        let pool = Self {
            shared,
            operations,
            max_pending,
        };
        for index in 0..threads {
            let shared = pool.shared.clone();
            thread::Builder::new()
                .name(format!("cyallo-t-host-{index}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let mut s = shared.state.lock().unwrap();
                            loop {
                                if s.stopped {
                                    return;
                                }
                                if let Some(job) = s.jobs.pop_front() {
                                    break job;
                                }
                                s = shared.ready.wait(s).unwrap();
                            }
                        };
                        // The job boundary contains its own adapter panic handler.
                        job();
                    }
                })
                .map_err(StartError::Thread)?;
        }
        Ok(pool)
    }
    pub fn submit<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Operation<Result<T, HostError>> {
        let (operation, completion) = self.operations.operation();
        let mut s = self.shared.state.lock().unwrap();
        if s.stopped || s.jobs.len() >= self.max_pending {
            let error = if s.stopped {
                HostError::Stopped
            } else {
                HostError::QueueFull
            };
            drop(s);
            let _ = completion.complete(Err(error));
            return operation;
        }
        let job = Box::new(move || {
            if completion.is_cancelled() {
                return;
            }
            let result = catch_unwind(AssertUnwindSafe(work)).map_err(|error| HostError::Panicked(error.into()));
            let _ = completion.complete(result);
        });
        s.jobs.push_back(job);
        self.shared.ready.notify_one();
        operation
    }
    /// Reject new work and cancel queued jobs. Already running callbacks retain
    /// their owned buffers and finish without holding an execution processor.
    pub fn stop(&self) {
        let jobs = {
            let mut s = self.shared.state.lock().unwrap();
            s.stopped = true;
            self.shared.ready.notify_all();
            std::mem::take(&mut s.jobs)
        };
        drop(jobs);
    }
}
impl Drop for BlockingPool {
    fn drop(&mut self) {
        // Running callbacks retain their owned state until they return. Never
        // join an uninterruptible OS call during abrupt runtime shutdown.
        self.stop();
    }
}
