//! Spreading work over the machine's cores, for building a tenant's in-memory copies when it
//! opens: reading, parsing and tokenizing thousands of pages are independent of each other.

/// Fewer items than this are mapped on the calling thread: starting threads costs more.
const MIN_PARALLEL: usize = 256;

/// `f` over every item, in order, on up to one thread per core.
pub fn map<T: Send, U: Send>(items: Vec<T>, f: impl Fn(T) -> U + Sync) -> Vec<U> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    if items.len() < MIN_PARALLEL || threads < 2 {
        return items.into_iter().map(f).collect();
    }
    let per = items.len().div_ceil(threads);
    let mut chunks: Vec<Vec<T>> = Vec::with_capacity(threads);
    let mut items = items.into_iter();
    loop {
        let chunk: Vec<T> = items.by_ref().take(per).collect();
        if chunk.is_empty() {
            break;
        }
        chunks.push(chunk);
    }
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| s.spawn(move || chunk.into_iter().map(f).collect::<Vec<U>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a worker thread panicked"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_the_order() {
        let items: Vec<usize> = (0..10_000).collect();
        let out = super::map(items, |i| i * 2);
        assert!(out.iter().enumerate().all(|(i, v)| *v == i * 2));
        assert_eq!(super::map(vec![1, 2], |i| i + 1), vec![2, 3]);
    }
}
