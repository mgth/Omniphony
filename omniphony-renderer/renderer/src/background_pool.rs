//! The thread pool background builds run their parallel loops on.
//!
//! Rayon's global pool has one thread per CPU. A build that fans out on it,
//! such as sampling a gain table over the grid, keeps every CPU busy, and the
//! render thread, which has no priority over the pool's threads, then waits
//! for a time slice: on a four-core machine a topology change held single
//! blocks for 2 to 10 ms, several block periods. This pool is one thread
//! short of the CPUs, so a build leaves a core to the render thread. With a
//! single CPU there is nothing to leave and the pool has one thread.

use std::sync::OnceLock;

/// The pool, built on first use and shared by every renderer in the process.
fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(cpus.saturating_sub(1).max(1))
            .thread_name(|i| format!("background-build-{i}"))
            .build()
            .expect("build the background thread pool")
    })
}

/// Run `build` with its rayon loops on the background pool. The calling
/// thread blocks until it returns.
///
/// A caller already on a rayon thread keeps its own pool: whoever installed
/// it chose it (the thread-count invariance tests of the table sampler do).
pub fn install<T: Send>(build: impl FnOnce() -> T + Send) -> T {
    if rayon::current_thread_index().is_some() {
        return build();
    }
    pool().install(build)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_leaves_a_cpu_to_the_render_thread() {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = install(rayon::current_num_threads);
        assert_eq!(threads, cpus.saturating_sub(1).max(1));
    }

    #[test]
    fn a_caller_on_its_own_pool_keeps_it() {
        let own = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .expect("thread pool");
        assert_eq!(own.install(|| install(rayon::current_num_threads)), 3);
    }
}
