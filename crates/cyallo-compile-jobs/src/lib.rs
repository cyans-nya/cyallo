//! Bounded compilation jobs on persistent workers, with ordered publication and no nested pools.
//!
//! Nesting is explicit: every job receives the [`Jobs`] it may use for nested
//! stages. Inside a pool worker that is [`Jobs::SEQUENTIAL`], so a nested stage
//! runs on its existing worker instead of multiplying thread pools.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// The worker threads for a configured width (not for a call's item count, so a
/// process holds one pool per `Jobs` setting, not one per batch size), created on first use and kept for the life
/// of the process. Compilations are short and frequent; creating and joining threads
/// for every parallel stage cost more than the stage. A pool holds no program state:
/// it is capacity only, so independent compilations (and runtimes) never share
/// anything through it but idle threads.
fn pool(width: usize) -> &'static rayon_core::ThreadPool {
    static POOLS: OnceLock<Mutex<BTreeMap<usize, &'static rayon_core::ThreadPool>>> =
        OnceLock::new();
    let mut pools = POOLS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pools.entry(width).or_insert_with(|| {
        Box::leak(Box::new(
            rayon_core::ThreadPoolBuilder::new()
                .num_threads(width)
                .thread_name(|index| format!("cyallo-compile-{index}"))
                .build()
                .expect("compile worker pool"),
        ))
    })
}
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
        pool(self.workers()).scope(|scope| {
            for (chunk, items) in items.chunks_mut(width).enumerate() {
                let f = &f;
                scope.spawn(move |_| {
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
        let published = Mutex::new(Vec::with_capacity(items.len()));
        pool(self.workers()).scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|_| {
                    let mut out = vec![];
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= items.len() {
                            break;
                        }
                        out.push((i, f(Jobs::SEQUENTIAL, i, &items[i])))
                    }
                    published
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .extend(out);
                });
            }
        });
        let mut output = published
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        output.sort_unstable_by_key(|(i, _)| *i);
        output.into_iter().map(|(_, r)| r).collect()
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
#[cfg(test)]
mod pool_tests {
    use super::*;
    use std::collections::HashSet;
    use std::thread::ThreadId;
    fn threads_used(jobs: Jobs, len: usize) -> HashSet<ThreadId> {
        jobs.map(&vec![0u8; len], 1, |_, _, _| std::thread::current().id())
            .into_iter()
            .collect()
    }
    #[test]
    fn workers_persist_across_stages() {
        let first = threads_used(Jobs(3), 12);
        let mut union = first.clone();
        for _ in 0..20 {
            union.extend(threads_used(Jobs(3), 12));
        }
        // Twenty more stages never need more than the pool's three threads.
        assert!(union.len() <= 3, "{} distinct threads", union.len());
    }
    #[test]
    fn batch_size_does_not_create_pools() {
        let widths = (2..9).flat_map(|len| threads_used(Jobs(4), len)).collect::<HashSet<_>>();
        assert!(widths.len() <= 4, "{} distinct threads", widths.len());
    }
    #[test]
    fn a_panic_in_a_job_reaches_the_caller_and_the_pool_survives() {
        let result = std::panic::catch_unwind(|| {
            Jobs(2).map(&[0, 1, 2, 3], 1, |_, i, _| {
                if i == 2 {
                    panic!("job failed")
                }
                i
            })
        });
        assert!(result.is_err());
        assert_eq!(Jobs(2).map(&[1, 2, 3, 4], 1, |_, _, v| *v * 2), vec![2, 4, 6, 8]);
    }
    #[test]
    fn mutable_items_and_borrowed_data_work() {
        let offset = 100;
        let mut values: Vec<usize> = (0..50).collect();
        Jobs(4).for_each_mut(&mut values, 1, |_, i, v| *v = i + offset);
        assert_eq!(values, (100..150).collect::<Vec<_>>());
    }
    #[test]
    fn concurrent_callers_share_the_pool_without_blocking_each_other() {
        let handles: Vec<_> = (0..6)
            .map(|n| std::thread::spawn(move || Jobs(4).map(&(0..40).collect::<Vec<_>>(), 1, move |_, _, v| v + n)))
            .collect();
        for (n, handle) in handles.into_iter().enumerate() {
            assert_eq!(handle.join().unwrap(), (0..40).map(|v| v + n).collect::<Vec<_>>());
        }
    }
}
