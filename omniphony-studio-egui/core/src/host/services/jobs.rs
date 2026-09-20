//! Bounded host-owned work, outside the drawing and service-clock threads.
use crate::host::commands::SharedState;
use std::sync::{
    Arc, Mutex,
    mpsc::{Receiver, channel},
};
use std::thread::{self, JoinHandle};

const MAX_JOBS: usize = 8;

#[derive(Default)]
struct State {
    closing: bool,
    workers: Vec<JoinHandle<()>>,
}

/// At most eight active jobs, with no unbounded waiting queue. Shutdown refuses
/// new work and joins work already accepted. It does not forcibly interrupt a
/// filesystem call, DNS lookup or an interactive OS authorization prompt.
#[derive(Default)]
pub struct Jobs(Mutex<State>);
impl Jobs {
    fn start(&self, work: impl FnOnce() + Send + 'static) -> Result<(), String> {
        let mut state = self.0.lock().unwrap();
        if state.closing {
            return Err("Studio is closing".into());
        }
        let mut index = 0;
        while index < state.workers.len() {
            if state.workers[index].is_finished() {
                join(state.workers.swap_remove(index));
            } else {
                index += 1;
            }
        }
        if state.workers.len() == MAX_JOBS {
            return Err("Too many background operations; wait for an operation to finish".into());
        }
        let worker = thread::Builder::new()
            .name("studio-job".into())
            .spawn(work)
            .map_err(|error| error.to_string())?;
        state.workers.push(worker);
        Ok(())
    }

    pub fn shutdown(&self) {
        let workers = {
            let mut state = self.0.lock().unwrap();
            state.closing = true;
            std::mem::take(&mut state.workers)
        };
        // Never hold the registry lock while joining: an accepted job may
        // attempt another operation, which must be rejected without deadlock.
        for worker in workers {
            join(worker);
        }
    }
}
impl Drop for Jobs {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn join(worker: JoinHandle<()>) {
    // A construction failure can drop the last host Arc from inside a job.
    // That worker is already returning; joining itself would deadlock.
    if worker.thread().id() != thread::current().id() && worker.join().is_err() {
        log::error!("Studio background operation panicked");
    }
}

pub fn try_run<T, F>(state: &Arc<SharedState>, work: F) -> Result<Receiver<T>, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = channel();
    let wake = state.waker.clone();
    state.jobs.start(move || {
        let _ = tx.send(work());
        wake();
    })?;
    Ok(rx)
}

/// Existing panel callers receive a disconnected channel if submission fails,
/// which uses their normal error path instead of leaving them pending forever.
pub fn run<T, F>(state: &Arc<SharedState>, work: F) -> Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match try_run(state, work) {
        Ok(receiver) => receiver,
        Err(error) => {
            crate::host::commands::app::push_log(state, "error", "jobs", error);
            let (_sender, receiver) = channel();
            receiver
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn capacity_is_bounded_and_shutdown_joins_every_accepted_job() {
        let jobs = Jobs::default();
        let finished = Arc::new(AtomicUsize::new(0));
        let mut releases = Vec::new();
        for _ in 0..MAX_JOBS {
            let (tx, rx) = channel();
            let done = finished.clone();
            jobs.start(move || {
                rx.recv().unwrap();
                done.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
            releases.push(tx);
        }
        assert!(jobs.start(|| panic!("must not run")).is_err());
        for release in releases {
            release.send(()).unwrap();
        }
        jobs.shutdown();
        assert_eq!(finished.load(Ordering::SeqCst), MAX_JOBS);
        assert!(jobs.start(|| panic!("must not run after close")).is_err());
        jobs.shutdown();
    }

    #[test]
    fn completed_slots_are_reclaimed_and_panics_do_not_poison_the_registry() {
        let jobs = Jobs::default();
        for _ in 0..MAX_JOBS * 2 {
            jobs.start(|| {}).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !jobs
                .0
                .lock()
                .unwrap()
                .workers
                .iter()
                .all(JoinHandle::is_finished)
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "worker did not finish"
                );
                thread::yield_now();
            }
        }
        jobs.start(|| panic!("injected worker failure")).unwrap();
        jobs.shutdown();
    }

    #[test]
    fn last_host_reference_can_be_released_inside_its_worker() {
        let state = Arc::new(crate::host::commands::tests::state());
        let host = state.clone();
        let (release, wait) = channel();
        let (done, finished) = channel();
        let _answer = run(&state, move || {
            wait.recv().unwrap();
            drop(host);
            done.send(()).unwrap();
        });
        drop(state);
        release.send(()).unwrap();
        finished
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
    }

    #[test]
    fn rejected_submission_disconnects_the_result_without_running_the_work() {
        let state = Arc::new(crate::host::commands::tests::state());
        state.jobs.shutdown();
        let receiver = run(&state, || panic!("must not run"));
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }
}
