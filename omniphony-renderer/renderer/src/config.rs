use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Mapping;

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<GlobalConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderConfig>,
    /// Captures any top-level key not modelled above so a load → mutate
    /// → save round-trip preserves it verbatim. Without this, every
    /// embedder of the engine that triggers `persist::save_live_config`
    /// (FFI in mpv-omniphony, future hosts, …) would silently strip
    /// CLI-only or host-specific keys from the user's config YAML.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct GlobalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loglevel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_format: Option<String>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RenderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<InputModeConfig>,
    /// Named pipe / file orender reads its bitstream from in continuous mode.
    /// Shared source of truth with the mpv lua routing script.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_pipe: Option<PathBuf>,
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
    pub render_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_evaluation_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render_evaluation_position_interpolation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_cartesian_x_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_cartesian_y_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_cartesian_z_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_cartesian_z_neg_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_allow_negative_z: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vbap_distance_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_gain: Option<f32>,
    /// Room geometry, stored in metres. Width is the reference (the room scale,
    /// a.k.a. radius_m, is Width/2). On load these are normalised into the
    /// renderer-facing `room_ratio*` + `current_layout.radius_m` so the rest of
    /// the pipeline is unchanged; `room_ratio*` below is legacy (read for
    /// migration, dropped on the next save).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_width_m: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_front_m: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_rear_m: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_height_m: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_lower_m: Option<f32>,
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
    pub auto_gain_ceiling_db: Option<f32>,
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
    pub size_to_spread_mode: Option<crate::render_backend::SizeToSpreadMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_adaptive_resampling: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_enable_far_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_force_silence_in_far_mode: Option<bool>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "adaptive_resampling_hard_recover_in_far_mode"
    )]
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "adaptive_resampling_near_far_threshold_ms"
    )]
    pub adaptive_resampling_high_recover_entry_margin_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_low_recover_settle_stable_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_low_recover_entry_margin_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_low_recover_exit_margin_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_low_recover_settle_margin_ms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_low_recover_refill_delta_alpha: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_control_smoothing_cutoff_hz: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_control_smoothing_order: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_use_pre_bridge_clock: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_use_output_pacing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_resampling_disable_backpressure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drc_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drc_weight: Option<f32>,
    /// OSC meter cadence (Hz). Persisted so the renderer is the source of truth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meter_rate: Option<f32>,
    /// OSC diag-publication cadence (Hz). Persisted alongside `meter_rate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diag_rate: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ramp_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse_threshold: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse_curve: Option<f32>,
    /// Distance metric (spherical / chebyshev) for the distance model stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_model_metric: Option<String>,
    /// Distance metric (spherical / chebyshev) for the distance diffuse stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse_metric: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_distance_distance_floor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_distance_min_active_speakers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_distance_max_active_speakers: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_distance_position_error_floor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_distance_position_error_nearest_scale: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_distance_position_error_span_scale: Option<f32>,
    /// Hybrid backend: id of the backend mixed in at ratio = 1 (cube surface).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hybrid_external_backend: Option<String>,
    /// Hybrid backend: id of the backend mixed in at ratio = 0 (centre).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hybrid_internal_backend: Option<String>,
    /// Hybrid backend: editable blend curve as `(distance, ratio)` control points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hybrid_curve: Option<Vec<[f32; 2]>>,
    /// Hybrid backend: blend curve smoothing in `[0, 1]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hybrid_curve_smoothing: Option<f32>,
    /// Hybrid backend: blend distance metric (spherical / chebyshev).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hybrid_metric: Option<String>,
    /// Barycenter backend: localization sharpness (`live_params` default 0.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub barycenter_localize: Option<f32>,
    /// Scriptable backend: path to the Lua script file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_backend_path: Option<String>,
    /// Scriptable backend: numeric parameters exposed to the script as a Lua
    /// table (`{ key = number }`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub script_backend_params: Option<Mapping>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    /// This matters most for `render.*`: any field added by a future
    /// version of the CLI / a host that we haven't migrated into this
    /// struct yet survives a save from another embedder.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// Convert a YAML mapping of `{ key: number }` into the numeric params list the
/// scriptable backend consumes. Non-numeric values and non-string keys are
/// dropped.
pub fn script_params_from_mapping(map: &Mapping) -> Vec<(String, f64)> {
    map.iter()
        .filter_map(|(k, v)| {
            let key = k.as_str()?.to_string();
            let value = v.as_f64().or_else(|| v.as_i64().map(|i| i as f64))?;
            Some((key, value))
        })
        .collect()
}

/// Inverse of [`script_params_from_mapping`], for saving. Returns `None` for an
/// empty list so the config field is omitted entirely.
pub fn script_params_to_mapping(params: &[(String, f64)]) -> Option<Mapping> {
    if params.is_empty() {
        return None;
    }
    let mut map = Mapping::new();
    for (key, value) in params {
        map.insert(
            serde_yaml_ng::Value::from(key.as_str()),
            serde_yaml_ng::Value::from(*value),
        );
    }
    Some(map)
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum InputModeConfig {
    #[serde(rename = "pipe_bridge", alias = "bridge")]
    Bridge,
    #[serde(rename = "pipewire", alias = "live")]
    Live,
    #[serde(rename = "pipewire_bridge")]
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputClockModeConfig {
    Dac,
    Pipewire,
    Upstream,
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
    /// Embedded input speaker layout (preferred over `layout` path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_layout: Option<crate::speaker_layout::SpeakerLayout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_mode: Option<InputClockModeConfig>,
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

impl RenderConfig {
    /// When the room geometry is stored in metres (`room_*_m`), derive the
    /// renderer-facing ratios + the layout radius from them so the rest of the
    /// pipeline keeps consuming `room_ratio` + `current_layout.radius_m`
    /// unchanged. Width is the reference: `radius = Width/2` (so width ratio is
    /// always 1). A no-op when the metre fields are absent (legacy config).
    pub fn normalize_room_meters(&mut self) {
        let Some(width_m) = self.room_width_m else {
            return;
        };
        let radius = (width_m / 2.0).max(0.01);
        let front = self.room_front_m.unwrap_or(2.0 * radius).max(0.0);
        let rear = self.room_rear_m.unwrap_or(radius).max(0.0);
        let height = self.room_height_m.unwrap_or(radius).max(0.0);
        let lower = self.room_lower_m.unwrap_or(0.5 * radius).max(0.0);
        self.room_ratio = Some(format!("1.0,{:.6},{:.6}", front / radius, height / radius));
        self.room_ratio_rear = Some((rear / radius).max(0.01));
        self.room_ratio_lower = Some((lower / radius).max(0.01));
        if let Some(layout) = self.current_layout.as_mut() {
            layout.radius_m = radius;
        }
    }
}

/// Outcome of resolving a config file, for diagnostics (see [`Config::load_status`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigLoadStatus {
    /// File present and parsed — the renderer is running on it.
    Loaded,
    /// No file at the resolved path → renderer fell back to built-in defaults.
    Missing,
    /// File present but failed to parse → renderer fell back to built-in
    /// defaults (the classic symptom of a stale host whose schema diverged).
    ParseError,
}

impl ConfigLoadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigLoadStatus::Loaded => "loaded",
            ConfigLoadStatus::Missing => "missing",
            ConfigLoadStatus::ParseError => "parse_error",
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut config: Self = serde_yaml_ng::from_str(&content)?;
        if let Some(render) = config.render.as_mut() {
            render.normalize_room_meters();
        }
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

    /// Diagnose what `load_or_default` would actually do for `path`, without
    /// keeping the result. `load_or_default` silently swallows both a missing
    /// file and a parse error into `Config::default()` (no current_layout → the
    /// default speaker preset + room), which is exactly how a host can end up on
    /// the wrong geometry while looking like it "has" a config path. Studio
    /// surfaces this in About so the silent fallback becomes visible.
    pub fn load_status(path: &Path) -> ConfigLoadStatus {
        if !path.exists() {
            return ConfigLoadStatus::Missing;
        }
        match Self::load(path) {
            Ok(_) => ConfigLoadStatus::Loaded,
            Err(_) => ConfigLoadStatus::ParseError,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_metres_normalize_to_ratios_and_radius() {
        let mut rc = RenderConfig {
            room_width_m: Some(4.0),
            room_front_m: Some(4.0),
            room_rear_m: Some(2.0),
            room_height_m: Some(2.0),
            room_lower_m: Some(1.0),
            current_layout: Some(crate::speaker_layout::SpeakerLayout {
                radius_m: 1.0,
                speakers: vec![],
            }),
            ..Default::default()
        };
        rc.normalize_room_meters();
        // radius = Width / 2 = 2, so width ratio = 1 and the others = m / radius.
        assert_eq!(rc.current_layout.as_ref().unwrap().radius_m, 2.0);
        assert_eq!(rc.room_ratio.as_deref(), Some("1.0,2.000000,1.000000"));
        assert_eq!(rc.room_ratio_rear, Some(1.0));
        assert_eq!(rc.room_ratio_lower, Some(0.5));
    }

    #[test]
    fn room_legacy_without_metres_is_noop() {
        let mut rc = RenderConfig {
            room_ratio: Some("1.0,2.0,1.0".to_string()),
            room_ratio_rear: Some(1.0),
            ..Default::default()
        };
        rc.normalize_room_meters();
        assert_eq!(rc.room_ratio.as_deref(), Some("1.0,2.0,1.0"));
        assert_eq!(rc.room_ratio_rear, Some(1.0));
    }

    #[test]
    fn unknown_fields_survive_round_trip_at_top_level() {
        let yaml = "\
cli_only_marker: keep-me
render:
  bridge_path: /tmp/x.so
  some_future_key:
    nested: value
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("parse");
        // Known field still typed.
        assert_eq!(
            cfg.render.as_ref().unwrap().bridge_path,
            Some(PathBuf::from("/tmp/x.so"))
        );
        // Unknown top-level + nested-unknown are captured.
        assert!(cfg.extra.contains_key("cli_only_marker"));
        assert!(
            cfg.render
                .as_ref()
                .unwrap()
                .extra
                .contains_key("some_future_key")
        );

        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            out.contains("cli_only_marker: keep-me"),
            "top-level unknown key dropped:\n{out}"
        );
        assert!(
            out.contains("some_future_key"),
            "nested unknown key dropped:\n{out}"
        );
        assert!(
            out.contains("bridge_path: /tmp/x.so"),
            "typed field missing:\n{out}"
        );
    }

    #[test]
    fn save_round_trip_preserves_unknown_fields() {
        let yaml = "\
render:
  bridge_path: /tmp/x.so
  cli_specific_thing: 42
";
        let mut cfg: Config = serde_yaml_ng::from_str(yaml).expect("parse");
        // Mutate a known field, as `persist::save_live_config` would.
        cfg.render.as_mut().unwrap().bridge_path = Some(PathBuf::from("/tmp/y.so"));
        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(out.contains("bridge_path: /tmp/y.so"), "{out}");
        assert!(
            out.contains("cli_specific_thing: 42"),
            "unknown field erased on save:\n{out}"
        );
    }
}
