//! Scoped, bounded compilation jobs with ordered publication and no nested pools.
use std::{
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};
thread_local! {static ACTIVE:Cell<bool>=const {Cell::new(false)};}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Jobs(pub usize);
impl Jobs {
    pub fn for_each_mut<T: Send>(
        self,
        items: &mut [T],
        minimum: usize,
        f: impl Fn(usize, &mut T) + Sync,
    ) {
        let workers = self.workers().min(items.len());
        if workers <= 1 || items.len() < minimum || ACTIVE.get() {
            for (i, t) in items.iter_mut().enumerate() {
                f(i, t)
            }
            return;
        }
        let width = items.len().div_ceil(workers);
        std::thread::scope(|scope| {
            for (chunk, items) in items.chunks_mut(width).enumerate() {
                let f = &f;
                scope.spawn(move || {
                    let old = ACTIVE.replace(true);
                    for (i, t) in items.iter_mut().enumerate() {
                        f(chunk * width + i, t)
                    }
                    ACTIVE.set(old);
                });
            }
        });
    }
    /// Zero selects the available cores, capped at 32 to bound compiler memory.
    pub fn workers(self) -> usize {
        if self.0 == 0 {
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .min(32)
        } else {
            self.0.clamp(1, 128)
        }
    }
    /// Every result is published in input order, including errors. A nested stage
    /// executes in its existing worker, rather than multiplying thread pools.
    pub fn map<'a, T: Sync, R: Send>(
        self,
        items: &'a [T],
        minimum: usize,
        f: impl Fn(usize, &'a T) -> R + Sync,
    ) -> Vec<R> {
        let workers = self.workers().min(items.len());
        if workers <= 1 || items.len() < minimum || ACTIVE.get() {
            return items.iter().enumerate().map(|(i, t)| f(i, t)).collect();
        }
        struct Active(bool);
        impl Active {
            fn enter() -> Self {
                Self(ACTIVE.replace(true))
            }
        }
        impl Drop for Active {
            fn drop(&mut self) {
                ACTIVE.set(self.0)
            }
        }
        let next = AtomicUsize::new(0);
        let work = || {
            let _active = Active::enter();
            let mut out = vec![];
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                out.push((i, f(i, &items[i])))
            }
            out
        };
        std::thread::scope(|scope| {
            let mut threads = vec![];
            for _ in 1..workers {
                threads.push(scope.spawn(work))
            }
            let mut output = work();
            for thread in threads {
                output.extend(
                    thread
                        .join()
                        .unwrap_or_else(|p| std::panic::resume_unwind(p)),
                )
            }
            output.sort_unstable_by_key(|(i, _)| *i);
            output.into_iter().map(|(_, r)| r).collect()
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_jobs_overlap_and_publish_in_order() {
        let barrier = std::sync::Barrier::new(4);
        let values = Jobs(4).map(&[0, 1, 2, 3], 1, |i, _| {
            barrier.wait();
            (i, std::thread::current().id())
        });
        assert_eq!(
            values.iter().map(|v| v.0).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        let ids = values
            .iter()
            .map(|v| v.1)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 4);
    }
    #[test]
    fn nested_stages_reuse_their_worker() {
        Jobs(4).map(&[0, 1, 2, 3], 1, |_, _| {
            let parent = std::thread::current().id();
            Jobs(8).map(&[0, 1, 2, 3], 1, |_, _| {
                assert_eq!(std::thread::current().id(), parent)
            });
        });
    }
}
