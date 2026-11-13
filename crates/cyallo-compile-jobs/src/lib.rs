//! Scoped, bounded compilation jobs with ordered publication and no nested pools.
//!
//! Nesting is explicit: every job receives the [`Jobs`] it may use for nested
//! stages. Inside a pool worker that is [`Jobs::SEQUENTIAL`], so a nested stage
//! runs on its existing worker instead of multiplying thread pools.
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Jobs(pub usize);
impl Jobs {
    /// Runs every stage on the calling thread.
    pub const SEQUENTIAL: Self = Self(1);
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
    /// Whether `len` items with this `minimum` stay on the calling thread.
    fn sequential(self, len: usize, minimum: usize) -> Option<usize> {
        let workers = self.workers().min(len);
        (workers > 1 && len >= minimum).then_some(workers)
    }
    pub fn for_each_mut<T: Send>(
        self,
        items: &mut [T],
        minimum: usize,
        f: impl Fn(Jobs, usize, &mut T) + Sync,
    ) {
        let Some(workers) = self.sequential(items.len(), minimum) else {
            for (i, t) in items.iter_mut().enumerate() {
                f(self, i, t)
            }
            return;
        };
        let width = items.len().div_ceil(workers);
        std::thread::scope(|scope| {
            for (chunk, items) in items.chunks_mut(width).enumerate() {
                let f = &f;
                scope.spawn(move || {
                    for (i, t) in items.iter_mut().enumerate() {
                        f(Jobs::SEQUENTIAL, chunk * width + i, t)
                    }
                });
            }
        });
    }
    /// Every result is published in input order, including errors.
    pub fn map<'a, T: Sync, R: Send>(
        self,
        items: &'a [T],
        minimum: usize,
        f: impl Fn(Jobs, usize, &'a T) -> R + Sync,
    ) -> Vec<R> {
        let Some(workers) = self.sequential(items.len(), minimum) else {
            return items
                .iter()
                .enumerate()
                .map(|(i, t)| f(self, i, t))
                .collect();
        };
        let next = AtomicUsize::new(0);
        let work = || {
            let mut out = vec![];
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= items.len() {
                    break;
                }
                out.push((i, f(Jobs::SEQUENTIAL, i, &items[i])))
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
        let values = Jobs(4).map(&[0, 1, 2, 3], 1, |_, i, _| {
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
        Jobs(4).map(&[0, 1, 2, 3], 1, |nested, _, _| {
            assert_eq!(nested, Jobs::SEQUENTIAL);
            let parent = std::thread::current().id();
            nested.map(&[0, 1, 2, 3], 1, |_, _, _| {
                assert_eq!(std::thread::current().id(), parent)
            });
        });
    }
    #[test]
    fn sequential_callers_pass_their_own_jobs_through() {
        Jobs(4).map(&[0], 4, |nested, _, _| assert_eq!(nested, Jobs(4)));
    }
}
