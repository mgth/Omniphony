//! Owned threads with cooperative cancellation and a joined shutdown.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
pub struct StopToken(Arc<AtomicBool>);
impl StopToken {
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    /// Interruptible wait: shutdown unparks the worker even on a long deadline.
    pub fn wait(&self, duration: Option<Duration>) {
        if self.cancelled() {
            return;
        }
        match duration {
            Some(duration) => thread::park_timeout(duration),
            None => thread::park(),
        }
    }
}

pub struct Worker {
    stop: StopToken,
    thread: Option<JoinHandle<()>>,
}
impl Worker {
    pub fn spawn(
        name: &str,
        work: impl FnOnce(StopToken) + Send + 'static,
    ) -> std::io::Result<Self> {
        let stop = StopToken(Arc::new(AtomicBool::new(false)));
        let token = stop.clone();
        let thread = thread::Builder::new()
            .name(name.into())
            .spawn(move || work(token))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
    pub fn request_stop(&self) {
        self.stop.0.store(true, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
    pub fn shutdown(&mut self) {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                log::error!("runtime worker panicked during shutdown");
            }
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_wakes_a_parked_worker_and_is_idempotent() {
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let mut worker = Worker::spawn("test-worker", move |stop| {
            while !stop.cancelled() {
                stop.wait(None);
            }
            finished.store(true, Ordering::Release);
        })
        .unwrap();
        worker.shutdown();
        worker.shutdown();
        assert!(done.load(Ordering::Acquire));
    }
}
