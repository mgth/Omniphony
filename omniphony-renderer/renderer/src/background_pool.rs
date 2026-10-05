//! Where background builds run, and at which priority.
//!
//! Rayon's global pool has one thread per CPU. A build that fans out on it,
//! such as sampling a gain table over the grid, keeps every CPU busy, and the
//! render thread, which has no priority over the pool's threads, then waits
//! for a time slice: on a four-core machine a topology change held single
//! blocks for 2 to 10 ms, several block periods.
//!
//! Two things keep a build off the render thread's time:
//!
//! - **Priority.** The threads that only ever build in the background (the
//!   speaker stage's band worker, the HRIR and BRIR workers) call
//!   [`enter_background`], and the pool their parallel loops run on is made
//!   of such threads. On Linux they are in the scheduler's idle class
//!   (`SCHED_IDLE`): they only get a CPU no other thread wants, and give it
//!   up the moment one does, without finishing their time slice. A build then
//!   costs the render thread nothing; on a machine kept busy by something
//!   else, the build waits instead, and the previous set keeps rendering.
//! - **One CPU spare.** The pool is one thread short of the CPUs (one thread
//!   on a single CPU), which is the only protection where the idle class is
//!   not applied (Windows, macOS).
//!
//! A thread that waits for the build it asks for keeps its own priority and
//! the global pool: the render thread's synchronous builds (the first band
//! set, offline renders), engine start-up. Making it wait on idle threads
//! would only stretch the wait.
//!
//! No thread is ever moved back out of the idle class: without privilege
//! Linux refuses it (see [`run_in_background`]). A thread that serves other
//! work hands its background build to a short-lived thread instead.

use std::cell::Cell;
use std::sync::OnceLock;

thread_local! {
    /// Whether this thread runs at background priority.
    static BACKGROUND: Cell<bool> = const { Cell::new(false) };
}

/// The pool, built on first use and shared by every renderer in the process.
fn pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        rayon::ThreadPoolBuilder::new()
            .num_threads(cpus.saturating_sub(1).max(1))
            .thread_name(|i| format!("background-build-{i}"))
            .start_handler(|_| enter_background())
            .build()
            .expect("build the background thread pool")
    })
}

/// Put the calling thread at background priority for the rest of its life.
/// For threads that do nothing but background work.
pub fn enter_background() {
    set_idle_priority();
    BACKGROUND.with(|b| b.set(true));
}

/// Run `work` at background priority on a thread of its own, and wait for
/// it. For a thread that serves other requests too (the OSC control thread
/// building the gain table the Studio displays): the caller keeps its
/// priority throughout.
///
/// A thread is never moved back out of the idle class. Without privilege,
/// Linux refuses that (`EPERM`) unless `RLIMIT_NICE` allows the nice value,
/// and its default of 0 does not: a caller lowered in place would stay at
/// idle priority for good. The short-lived thread is lowered instead, and
/// ends with the work.
pub fn run_in_background<T: Send>(work: impl FnOnce() -> T + Send) -> T {
    if BACKGROUND.with(Cell::get) {
        return work();
    }
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("background-build".into())
            .spawn_scoped(scope, || {
                enter_background();
                work()
            })
            .expect("spawn a background build thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Run `build` with its rayon loops on the pool that matches the calling
/// thread: the background pool from a background thread, the global pool
/// otherwise (see the module doc). The calling thread blocks until it
/// returns.
///
/// A caller already on a rayon thread keeps its own pool: whoever installed
/// it chose it (the thread-count invariance tests of the table sampler do).
pub fn install<T: Send>(build: impl FnOnce() -> T + Send) -> T {
    if rayon::current_thread_index().is_some() || !BACKGROUND.with(Cell::get) {
        return build();
    }
    pool().install(build)
}

/// Move the calling thread into the idle scheduling class (Linux applies it
/// to the calling thread alone). One way only: see [`run_in_background`].
#[cfg(target_os = "linux")]
fn set_idle_priority() {
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: `param` is a valid sched_param; pid 0 is the calling thread.
    if unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) } != 0 {
        log::debug!(
            "background thread priority not changed: {}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn set_idle_priority() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_leaves_a_cpu_to_the_render_thread() {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = std::thread::spawn(|| {
            enter_background();
            install(rayon::current_num_threads)
        })
        .join()
        .unwrap();
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

    #[test]
    fn a_waiting_caller_keeps_the_global_pool() {
        let global = std::thread::spawn(|| install(rayon::current_num_threads))
            .join()
            .unwrap();
        assert_eq!(global, rayon::current_num_threads());
    }

    #[cfg(target_os = "linux")]
    fn policy() -> i32 {
        // SAFETY: pid 0 is the calling thread.
        unsafe { libc::sched_getscheduler(0) }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn background_threads_are_in_the_idle_class() {
        let (worker, pool_thread) = std::thread::spawn(|| {
            enter_background();
            (policy(), install(policy))
        })
        .join()
        .unwrap();
        assert_eq!(worker, libc::SCHED_IDLE);
        assert_eq!(pool_thread, libc::SCHED_IDLE);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn run_in_background_leaves_the_caller_in_its_class() {
        let (during, after) = std::thread::spawn(|| {
            let during = run_in_background(policy);
            (during, policy())
        })
        .join()
        .unwrap();
        assert_eq!(during, libc::SCHED_IDLE);
        assert_eq!(after, libc::SCHED_OTHER);
    }

    #[test]
    fn run_in_background_uses_the_background_pool() {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let threads = run_in_background(|| install(rayon::current_num_threads));
        assert_eq!(threads, cpus.saturating_sub(1).max(1));
    }
}
