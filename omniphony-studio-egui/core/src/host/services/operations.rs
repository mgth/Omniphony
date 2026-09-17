//! Bounded user-triggered host work. Drawing can submit one operation and read
//! its result; DNS, files and service-manager processes run on the worker.

use crate::host::commands::{SharedState, app, orender};
use std::sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
};

pub enum Action {
    Refresh,
    Connect { host: String, port: u16 },
    Launch,
    Stop,
    InstallService,
    RestartService,
    UninstallService,
    RestartPipewire,
}

struct Completion {
    result: Result<(), String>,
    status: Option<Result<orender::OrenderServiceStatus, String>>,
}

#[derive(Default)]
pub struct Operations {
    pending: Option<Receiver<Completion>>,
    pub status: Option<Result<orender::OrenderServiceStatus, String>>,
    pub error: Option<String>,
}

impl Operations {
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn request(&mut self, state: &Arc<SharedState>, action: Action) -> bool {
        if self.pending() {
            return false;
        }
        self.error = None;
        let host = state.clone();
        self.pending = Some(super::jobs::run(state, move || {
            let refresh = !matches!(&action, Action::Connect { .. });
            let result = execute(&host, action);
            if let Err(error) = &result {
                app::push_log(&host, "error", "host", error.clone());
            }
            Completion {
                result,
                status: refresh.then(orender::get_orender_service_status),
            }
        }));
        true
    }

    pub fn poll(&mut self) {
        let Some(pending) = &self.pending else {
            return;
        };
        match pending.try_recv() {
            Ok(done) => {
                self.pending = None;
                self.error = done.result.err();
                if let Some(status) = done.status {
                    self.status = Some(status);
                }
            }
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.error = Some("Host worker disconnected".into());
            }
            Err(TryRecvError::Empty) => {}
        }
    }
}

fn execute(state: &SharedState, action: Action) -> Result<(), String> {
    match action {
        Action::Refresh => Ok(()),
        Action::Connect { host, port } => {
            app::connect_to(state, &host, port)?;
            let metering = state.read().app.osc_metering_enabled.unwrap_or(0) != 0;
            state.config.update(|config| {
                config.host = host.trim().to_owned();
                config.osc_rx_port = port;
                config.osc_metering_enabled = metering;
            })
        }
        Action::Stop => {
            orender::stop_orender(state);
            Ok(())
        }
        Action::RestartService => orender::restart_orender_service(),
        Action::UninstallService => orender::uninstall_orender_service(),
        Action::RestartPipewire => orender::restart_pipewire_services(),
        action @ (Action::Launch | Action::InstallService) => {
            let config = state.config.snapshot();
            let paths = &state.paths;
            let result = match action {
                Action::Launch => orender::launch_orender(
                    paths,
                    state,
                    config.host,
                    config.osc_rx_port,
                    config.osc_port,
                    config.osc_metering_enabled,
                    None,
                    None,
                ),
                _ => orender::install_orender_service(
                    paths,
                    state,
                    config.host,
                    config.osc_rx_port,
                    config.osc_port,
                    config.osc_metering_enabled,
                    None,
                    None,
                ),
            }?;
            app::push_log(state, "info", "host", result.to_string());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_work_never_blocks_polling_or_accepts_a_duplicate() {
        let state = Arc::new(crate::host::commands::tests::state());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut operations = Operations {
            pending: Some(rx),
            ..Default::default()
        };
        operations.poll();
        assert!(operations.pending());
        assert!(!operations.request(&state, Action::Refresh));
        tx.send(Completion {
            result: Err("failed".into()),
            status: None,
        })
        .unwrap();
        operations.poll();
        assert!(!operations.pending());
        assert_eq!(operations.error.as_deref(), Some("failed"));
    }
}
