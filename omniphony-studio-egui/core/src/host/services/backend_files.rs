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
    pub request_id: String,
    pub requires_id: bool,
}
pub fn begin(state: &SharedState, backend: &str, key: &str) -> String {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let request_id = format!(
        "{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut live = state.inner.lock().unwrap();
    live.backend_file_content = None;
    live.backend_file_error = None;
    let requires_id = live
        .app
        .producer_capabilities
        .as_ref()
        .and_then(|v| v.get("fileRequestIds"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    live.backend_file_pending = Some(Pending {
        backend: backend.into(),
        key: key.into(),
        due: Instant::now() + TIMEOUT,
        request_id: request_id.clone(),
        requires_id,
    });
    drop(live);
    (state.waker)();
    request_id
}
pub fn cancel(state: &SharedState) {
    let mut live = state.inner.lock().unwrap();
    live.backend_file_pending = None;
    live.backend_file_content = None;
    live.backend_file_error = None;
}
/// Tagged replies must match the current request. Legacy untagged replies are
/// accepted only for a producer that did not advertise correlation support.
pub fn finish(live: &mut Live, backend: &str, key: &str, request_id: Option<&str>) -> bool {
    let matches = live.backend_file_pending.as_ref().is_some_and(|pending| {
        pending.backend == backend
            && pending.key == key
            && match request_id {
                Some(id) => id == pending.request_id,
                None => !pending.requires_id,
            }
    });
    if matches {
        live.backend_file_pending = None;
    }
    matches
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
        finish(&mut state.inner.lock().unwrap(), "script", "file", None);
        assert!(!tick(&state, due + TIMEOUT).changed);
        assert!(state.read().backend_file_error.is_none());
    }
    #[test]
    fn delayed_or_untagged_replies_cannot_complete_a_new_tagged_request() {
        use crate::osc::{
            dispatch::{Change, apply_event},
            parser::OscEvent,
        };
        let state = crate::host::commands::tests::state();
        state.inner.lock().unwrap().app.producer_capabilities =
            Some(serde_json::json!({"fileRequestIds": true}));
        let old = begin(&state, "script", "file");
        cancel(&state);
        let current = begin(&state, "script", "file");
        assert_ne!(old, current);
        let content = |id| OscEvent::StateBackendFileContent {
            backend: "script".into(),
            key: "file".into(),
            name: "file.lua".into(),
            content: "new".into(),
            request_id: id,
        };
        let mut live = state.inner.lock().unwrap();
        assert_eq!(
            apply_event(&mut live, content(Some(old.clone()))),
            Change::None
        );
        assert_eq!(apply_event(&mut live, content(None)), Change::None);
        assert_eq!(
            apply_event(
                &mut live,
                OscEvent::StateBackendFileError {
                    backend: "script".into(),
                    key: "file".into(),
                    message: "late".into(),
                    request_id: Some(old)
                }
            ),
            Change::None
        );
        assert!(live.backend_file_error.is_none());
        assert_eq!(
            live.backend_file_pending.as_ref().unwrap().request_id,
            current
        );
        assert_eq!(
            apply_event(&mut live, content(Some(current.clone()))),
            Change::Snapshot
        );
        assert!(live.backend_file_pending.is_none());
        live.backend_file_content = None;
        assert_eq!(apply_event(&mut live, content(Some(current))), Change::None);
        assert!(live.backend_file_content.is_none());
    }
    #[test]
    fn legacy_renderer_keeps_untagged_reply_compatibility() {
        let state = crate::host::commands::tests::state();
        begin(&state, "script", "file");
        assert!(finish(
            &mut state.inner.lock().unwrap(),
            "script",
            "file",
            None
        ));
    }
}
