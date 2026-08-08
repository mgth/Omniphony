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
            osc_rx_port: crate::runtime_env::default_osc_rx_port(),
            osc_metering_enabled: false,
            auto_start_renderer: true,
            keep_renderer_alive_on_quit: false,
            last_layout_import_dir: None,
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
    std::fs::create_dir_all(config_dir).map_err(|e| e.to_string())?;
    let data = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(config_path(config_dir), data).map_err(|e| e.to_string())
}
