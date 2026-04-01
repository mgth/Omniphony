use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<GlobalConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderConfig>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct GlobalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loglevel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RenderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<InputModeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_input: Option<LiveInputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_vbap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_layout: Option<PathBuf>,
    /// Embedded current speaker layout (preferred over `speaker_layout` path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_layout: Option<crate::speaker_layout::SpeakerLayout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_table: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_azimuth_resolution: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_elevation_resolution: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_spread: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_distance_res: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_distance_max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_position_interpolation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_table_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_cart_x_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_cart_y_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_cart_z_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_cart_z_neg_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_allow_negative_z: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_distance_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_gain: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_ratio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_ratio_rear: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_ratio_lower: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_ratio_center_blend: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc_metering: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc_rx_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc_port: Option<u16>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "sink",
        alias = "asio_device_name"
    )]
    pub output_device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_target: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuous: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_loudness: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_gain: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bed_conform: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_from_distance: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_distance_range: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spread_distance_curve: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_spread_min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_spread_max: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_adaptive_resampling: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_enable_far_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_force_silence_in_far_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", alias = "adaptive_resampling_hard_recover_in_far_mode")]
    pub adaptive_resampling_hard_recover_high_in_far_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_hard_recover_low_in_far_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_far_mode_return_fade_in_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_kp_near: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_ki: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_integral_discharge_ratio: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_max_adjust: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_update_interval_callbacks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_near_far_threshold_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ramp_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse_threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse_curve: Option<f32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputModeConfig {
    Bridge,
    Live,
    PipewireBridge,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputBackendConfig {
    Pipewire,
    Asio,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InputMapModeConfig {
    SevenOneFixed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputLfeModeConfig {
    Object,
    Direct,
    Drop,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct LiveInputConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<InputBackendConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<InputMapModeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lfe_mode: Option<InputLfeModeConfig>,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = serde_yaml_ng::from_str(&content)?;
        Ok(config)
    }

    /// Load config from path, returning default if the file is absent.
    /// Prints a warning to stderr (not the log) if the file exists but fails to parse,
    /// because this may be called before the logger is initialized.
    pub fn load_or_default(path: &Path) -> Self {
        if !path.exists() {
            return Self::default();
        }
        match Self::load(path) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!(
                    "warning: failed to parse config file {}: {}",
                    path.display(),
                    e
                );
                Self::default()
            }
        }
    }

    /// Serialize this config to YAML and write it to `path`.
    /// Parent directories are created automatically.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let yaml = serde_yaml_ng::to_string(self)?;
        std::fs::write(path, yaml)?;
        Ok(())
    }
}

/// Returns the platform default config path without external dependencies.
///
/// - Linux:   `$XDG_CONFIG_HOME/omniphony/config.yaml`  (fallback: `~/.config/omniphony/config.yaml`)
/// - Windows: `%APPDATA%\omniphony\config.yaml`
pub fn default_config_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        return std::env::var("APPDATA")
            .ok()
            .map(|p| PathBuf::from(p).join("omniphony").join("config.yaml"));
    }

    // Unix / Linux
    #[cfg(not(windows))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })?;
        Some(base.join("omniphony").join("config.yaml"))
    }
}
