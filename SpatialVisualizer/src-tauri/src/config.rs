use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OscConfig {
    pub host: String,
    pub osc_port: u16,
    pub osc_rx_port: u16,
    #[serde(default)]
    pub osc_metering_enabled: bool,
}

impl Default for OscConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            osc_port: 0,
            osc_rx_port: 9000,
            osc_metering_enabled: false,
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
