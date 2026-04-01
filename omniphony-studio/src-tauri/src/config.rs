use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OscConfig {
    pub host: String,
    pub osc_port: u16,
    pub osc_rx_port: u16,
    #[serde(default)]
    pub osc_metering_enabled: bool,
    #[serde(default)]
    pub bridge_path: Option<String>,
    #[serde(default)]
    pub orender_path: Option<String>,
    #[serde(default)]
    pub input_pipe: Option<String>,
    #[serde(default)]
    pub audio_output_device: Option<String>,
    #[serde(default)]
    pub audio_sample_rate: Option<u32>,
    #[serde(default)]
    pub ramp_mode: Option<String>,
}

impl Default for OscConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            osc_port: 0,
            osc_rx_port: 9000,
            osc_metering_enabled: false,
            bridge_path: None,
            orender_path: None,
            input_pipe: None,
            audio_output_device: None,
            audio_sample_rate: None,
            ramp_mode: None,
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
