//! One asynchronous mpv config operation at a time, independent of panel draws.
use crate::host::commands::{SharedState, app, mpv_config};
use mpv_config::MpvOrenderStatus;
use std::sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
};
type Result = std::result::Result<MpvOrenderStatus, String>;

#[derive(Default)]
pub struct MpvConfig {
    pending: Option<Receiver<Result>>,
    stale: bool,
    pub status: Option<Result>,
}
impl MpvConfig {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The user can edit mpv.conf while the panel is closed. An in-flight read
    /// must not repopulate that invalidated cache when the panel opens again.
    pub fn invalidate(&mut self) {
        self.status = None;
        self.stale = true;
    }
    pub fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("mpv configuration worker disconnected".into()),
        };
        self.pending = None;
        if !self.stale {
            self.status = Some(result);
        }
    }
    pub fn refresh(&mut self, state: &Arc<SharedState>) {
        self.poll();
        if self.status.is_none() {
            self.request(state, None);
        }
    }
    pub fn set_enabled(&mut self, state: &Arc<SharedState>, enabled: bool) -> bool {
        self.request(state, Some(enabled))
    }
    fn request(&mut self, state: &Arc<SharedState>, enabled: Option<bool>) -> bool {
        if self.pending() {
            return false;
        }
        self.stale = false;
        let host = state.clone();
        self.pending = Some(super::jobs::run(state, move || {
            let result = match enabled {
                Some(enabled) => mpv_config::mpv_orender_set(enabled),
                None => mpv_config::mpv_orender_status(),
            };
            if let Some(enabled) = enabled {
                let (level, message) = match &result {
                    Ok(status) => (
                        "info",
                        crate::i18n::tf(
                            if enabled {
                                "log.mpvOrenderEnabled"
                            } else {
                                "log.mpvOrenderDisabled"
                            },
                            &[("path", &status.path)],
                        ),
                    ),
                    Err(error) => (
                        "error",
                        crate::i18n::tf("log.mpvOrenderFailed", &[("error", error)]),
                    ),
                };
                app::push_log(&host, level, "mpv", message);
            }
            result
        }));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_work_does_not_block_poll_or_accept_a_second_operation() {
        let state = Arc::new(crate::host::commands::tests::state());
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut config = MpvConfig {
            pending: Some(receiver),
            ..Default::default()
        };
        config.refresh(&state);
        assert!(config.pending());
        assert!(!config.set_enabled(&state, true));
        sender.send(Err("read failed".into())).unwrap();
        config.poll();
        assert!(!config.pending());
        assert_eq!(
            config.status.as_ref().unwrap().as_ref().unwrap_err(),
            "read failed"
        );
    }
    #[test]
    fn closing_during_a_read_never_adopts_the_old_result() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut config = MpvConfig {
            pending: Some(receiver),
            ..Default::default()
        };
        config.invalidate();
        sender.send(Err("stale read".into())).unwrap();
        config.poll();
        assert!(!config.pending());
        assert!(config.status.is_none());
    }
}
