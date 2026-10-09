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

/// Sets the budget, once, before anything uses it (at startup); 0 means the default, half the
/// machine's logical CPUs. Says whether it took effect (a later call changes nothing).
pub fn set_threads(n: usize) -> bool {
    let n = if n == 0 { default_threads() } else { n };
    THREADS.set(n).is_ok()
}

/// How many threads the service computes on.
pub fn threads() -> usize {
    *THREADS.get_or_init(default_threads)
}

/// Half the logical CPUs, at least one.
fn default_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))
}

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
