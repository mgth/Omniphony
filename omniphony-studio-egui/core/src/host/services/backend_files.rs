//! Deadlines for renderer-owned file requests; independent of visible frames.
use super::Tick;
use crate::{
    host::commands::SharedState,
    osc::dispatch::{BackendFileError, BackendFileFailure, Live},
};
use std::time::{Duration, Instant};
pub const TIMEOUT: Duration = Duration::from_secs(15);
pub struct Pending {
    pub backend: String,
    pub key: String,
    pub due: Instant,
}
pub fn begin(state: &SharedState, backend: &str, key: &str) {
    let mut live = state.inner.lock().unwrap();
    live.backend_file_content = None;
    live.backend_file_error = None;
    live.backend_file_pending = Some(Pending {
        backend: backend.into(),
        key: key.into(),
        due: Instant::now() + TIMEOUT,
    });
    drop(live);
    (state.waker)();
}
pub fn cancel(state: &SharedState) {
    let mut live = state.inner.lock().unwrap();
    live.backend_file_pending = None;
    live.backend_file_content = None;
    live.backend_file_error = None;
}
pub fn finish(live: &mut Live, backend: &str, key: &str) {
    if live
        .backend_file_pending
        .as_ref()
        .is_some_and(|p| p.backend == backend && p.key == key)
    {
        live.backend_file_pending = None;
    }
}
pub fn interrupted(pending: Pending) -> BackendFileError {
    BackendFileError {
        backend: pending.backend,
        key: pending.key,
        failure: BackendFileFailure::ConnectionChanged,
    }
}
pub fn tick(state: &SharedState, now: Instant) -> Tick {
    let mut live = state.inner.lock().unwrap();
    let Some(pending) = live.backend_file_pending.as_ref() else {
        return Tick::idle();
    };
    if now < pending.due {
        return Tick {
            changed: false,
            next: Some(pending.due),
        };
    }
    let pending = live.backend_file_pending.take().unwrap();
    live.backend_file_error = Some(BackendFileError {
        backend: pending.backend,
        key: pending.key,
        failure: BackendFileFailure::TimedOut,
    });
    Tick {
        changed: true,
        next: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_fires_without_frames_and_completed_requests_do_not_expire() {
        let state = crate::host::commands::tests::state();
        begin(&state, "script", "file");
        let due = state.read().backend_file_pending.as_ref().unwrap().due;
        assert!(!tick(&state, due - Duration::from_millis(1)).changed);
        assert!(tick(&state, due).changed);
        assert!(state.read().backend_file_error.is_some());
        assert!(!tick(&state, due + TIMEOUT).changed);
        begin(&state, "script", "file");
        finish(&mut state.inner.lock().unwrap(), "script", "file");
        assert!(!tick(&state, due + TIMEOUT).changed);
        assert!(state.read().backend_file_error.is_none());
    }
}
