//! The service's compute budget: one pool of threads that every piece of parallel work shares
//! (parsing and tokenizing a tenant's pages when it opens, comparing a query with a large
//! tenant's vectors, a meaning-based search), so however many requests run at once, the
//! service's computing stays within `[server] threads` and never takes the whole machine. The
//! embedding model and the async runtime are sized to the same number.

use std::sync::OnceLock;

use rayon::prelude::*;

/// Fewer items than this are mapped on the calling thread: handing them out costs more.
const MIN_PARALLEL: usize = 256;

static THREADS: OnceLock<usize> = OnceLock::new();
static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();

/// Sets the budget, once, before anything uses it (at startup); 0 means the default, 4 threads
/// (fewer on a machine with fewer logical CPUs). Says whether it took effect (a later call
/// changes nothing).
pub fn set_threads(n: usize) -> bool {
    let n = if n == 0 { default_threads() } else { n };
    THREADS.set(n).is_ok()
}

/// How many threads the service computes on.
pub fn threads() -> usize {
    *THREADS.get_or_init(default_threads)
}

/// 4 threads, or the logical CPUs when there are fewer: measured, 4 threads come within 10% of
/// a 12-thread laptop's speed on the largest tenants and embed a query fastest (design, "A
/// compute budget").
fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(DEFAULT_THREADS))
}

/// Threads the service computes on unless configured.
const DEFAULT_THREADS: usize = 4;

fn pool() -> &'static rayon::ThreadPool {
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads())
            .thread_name(|i| format!("compute-{i}"))
            .build()
            .expect("the compute pool starts")
    })
}

/// Runs `f` on the compute pool, waiting for it; parallel work inside it shares the pool.
pub fn run<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    pool().install(f)
}

/// Runs `a` and `b` on the compute pool, at the same time when a thread of it is free (`a` on
/// the thread that takes the call, `b` on another), and waits for both.
pub fn join<A: Send, B: Send>(
    a: impl FnOnce() -> A + Send,
    b: impl FnOnce() -> B + Send,
) -> (A, B) {
    if threads() < 2 {
        return (a(), b());
    }
    run(|| rayon::join(a, b))
}

/// `f` over every item, in order, on the compute pool.
pub fn map<T: Send, U: Send>(items: Vec<T>, f: impl Fn(T) -> U + Sync + Send) -> Vec<U> {
    if items.len() < MIN_PARALLEL || threads() < 2 {
        return items.into_iter().map(f).collect();
    }
    run(|| items.into_par_iter().map(f).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_the_order() {
        let items: Vec<usize> = (0..10_000).collect();
        let out = super::map(items, |i| i * 2);
        assert!(out.iter().enumerate().all(|(i, v)| *v == i * 2));
        assert_eq!(super::map(vec![1, 2], |i| i + 1), vec![2, 3]);
        assert!(super::threads() >= 1);
    }
}
