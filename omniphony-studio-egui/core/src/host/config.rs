use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OscConfig {
    pub host: String,
    pub osc_port: u16,
    pub osc_rx_port: u16,
    #[serde(default)]
    pub osc_metering_enabled: bool,
    /// Auto-launch a local standby renderer (with --osc-yield) when nothing
    /// answers on a loopback target. See the watchdog in osc_listener.
    #[serde(default = "default_true")]
    pub auto_start_renderer: bool,
    /// Leave the Studio-launched standby renderer running when Studio quits.
    #[serde(default)]
    pub keep_renderer_alive_on_quit: bool,
    /// Directory the layout-import picker last opened in. Server-only (never
    /// sent by the JS config form); seeded with the bundled layouts dir on the
    /// first import so users can find the presets, then tracks their last pick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_layout_import_dir: Option<String>,
    /// Drive the PI auto-tune from the backend port instead of the frontend
    /// state machine. Off until a full run has been done on real hardware.
    /// Server-only, like `last_layout_import_dir`: the JS config form does not
    /// carry it, so `save_osc_config` has to preserve it explicitly.
    #[serde(default)]
    pub rust_auto_tune: bool,
}

fn default_true() -> bool {
    true
}

impl Default for OscConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            osc_port: 0,
            // An environment that carved out its own runtime namespace pins the
            // port, so this Studio only ever meets the renderer belonging to it.
            osc_rx_port: super::runtime_env::default_osc_rx_port(),
            osc_metering_enabled: false,
            auto_start_renderer: true,
            keep_renderer_alive_on_quit: false,
            last_layout_import_dir: None,
            rust_auto_tune: false,
        }
    }
}

fn config_path(config_dir: &PathBuf) -> PathBuf {
    config_dir.join("osc_config.json")
}

pub fn load_config(config_dir: &PathBuf) -> OscConfig {
    let path = config_path(config_dir);
    let Ok(data) = std::fs::read_to_string(&path) else {
        return OscConfig::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_config(config_dir: &PathBuf, cfg: &OscConfig) -> Result<(), String> {
    super::json_store::save(&config_path(config_dir), cfg)
}

/// Carry a checkout's native connection settings into the stable namespace.
/// An explicit runtime namespace is intentionally isolated from legacy files.
pub fn migrate_legacy(
    config_dir: &std::path::Path,
    layouts_dir: &std::path::Path,
) -> Result<(), String> {
    if super::runtime_env::config_dir().is_some() {
        return Ok(());
    }
    super::json_store::migrate::<OscConfig>(
        &layouts_dir.join(".studio-egui/osc_config.json"),
        &config_dir.join("osc_config.json"),
    )
}

struct RuntimeState {
    config: OscConfig,
    closing: bool,
}

/// One in-memory connection configuration per host. Reads and typed patches
/// never perform disk I/O; a coalescing writer persists complete snapshots.
/// External file edits are picked up at the next host start.
pub struct RuntimeConfig {
    value: std::sync::Mutex<RuntimeState>,
    writer: std::sync::Mutex<Option<super::json_store::Writer<OscConfig>>>,
    initial_error: Option<String>,
}
impl RuntimeConfig {
    pub fn new(directory: &PathBuf, wake: crate::osc::Waker) -> Self {
        let path = config_path(directory);
        let (value, mut error) = super::json_store::load::<OscConfig>(&path);
        // A corrupt/unreadable file must not be overwritten with defaults.
        let writer = if error.is_none() {
            match super::json_store::Writer::new(
                path,
                std::time::Duration::from_millis(200),
                wake,
                None,
            ) {
                Ok(writer) => Some(writer),
                Err(failure) => {
                    error = Some(failure.to_string());
                    None
                }
            }
        } else {
            None
        };
        Self {
            value: std::sync::Mutex::new(RuntimeState {
                config: value,
                closing: false,
            }),
            writer: std::sync::Mutex::new(writer),
            initial_error: error,
        }
    }
    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self {
            value: std::sync::Mutex::new(RuntimeState {
                config: OscConfig::default(),
                closing: false,
            }),
            writer: std::sync::Mutex::new(None),
            initial_error: None,
        }
    }
    pub fn snapshot(&self) -> OscConfig {
        self.value.lock().unwrap().config.clone()
    }
    pub fn update(&self, patch: impl FnOnce(&mut OscConfig)) -> Result<(), String> {
        let mut value = self.value.lock().unwrap();
        if value.closing {
            return Err("Connection configuration is closing".into());
        }
        patch(&mut value.config);
        // Keep the snapshot submission ordered with the patch. Otherwise two
        // concurrent commands could submit older state after a newer patch.
        if let Some(writer) = &*self.writer.lock().unwrap() {
            writer.submit(value.config.clone());
        }
        self.initial_error.clone().map_or(Ok(()), Err)
    }
    pub fn error(&self) -> Option<String> {
        self.initial_error.clone().or_else(|| {
            self.writer
                .lock()
                .unwrap()
                .as_ref()
                .and_then(super::json_store::Writer::error)
        })
    }
    pub fn shutdown(&self) -> Result<(), String> {
        self.value.lock().unwrap().closing = true;
        if let Some(writer) = &mut *self.writer.lock().unwrap() {
            writer.shutdown()?;
        }
        self.initial_error.clone().map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn concurrent_patches_preserve_other_fields_and_flush_the_latest_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let config = Arc::new(RuntimeConfig::new(&path, Arc::new(|| {})));
        let first = config.clone();
        let worker = std::thread::spawn(move || {
            for index in 0..100 {
                first
                    .update(|cfg| cfg.host = format!("host-{index}"))
                    .unwrap();
            }
        });
        for index in 0..100 {
            config
                .update(|cfg| {
                    cfg.last_layout_import_dir = Some(format!("directory-{index}"));
                    cfg.keep_renderer_alive_on_quit = true;
                })
                .unwrap();
        }
        worker.join().unwrap();
        config.shutdown().unwrap();
        let saved = load_config(&path);
        assert_eq!(saved.host, "host-99");
        assert_eq!(
            saved.last_layout_import_dir.as_deref(),
            Some("directory-99")
        );
        assert!(saved.keep_renderer_alive_on_quit);
        assert!(
            config
                .update(|_| panic!("must not run after close"))
                .is_err()
        );
        config.shutdown().unwrap();
    }
    #[test]
    fn malformed_config_stays_read_only_while_session_changes_are_available() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_path_buf();
        let file = config_path(&path);
        std::fs::write(&file, "invalid document").unwrap();
        let config = RuntimeConfig::new(&path, Arc::new(|| {}));
        assert!(config.error().is_some());
        assert!(
            config
                .update(|cfg| cfg.keep_renderer_alive_on_quit = true)
                .is_err()
        );
        assert!(config.snapshot().keep_renderer_alive_on_quit);
        assert!(config.shutdown().is_err());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "invalid document");
    }
}
