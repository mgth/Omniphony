use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

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
    /// Destination for the `file` output backend: `-` (stdout) or a path to a
    /// regular file or named pipe (FIFO).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    /// Format for the `file` output backend: `raw_f32` or `caf`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file_format: Option<String>,
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
    /// Generic per-backend parameter values (see [`crate::backend_params`]),
    /// keyed by backend id then param key. Lets a contributor backend's params
    /// round-trip through config without a typed field here.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub backend_params: std::collections::HashMap<
        String,
        std::collections::HashMap<String, crate::backend_params::ParamValue>,
    >,
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
    /// Number of object-size intervals to precompute (0 = single table). Applies
    /// to both precomputed evaluation modes; see `EvaluationLiveParams`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation_object_size_intervals: Option<usize>,
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
    /// How channel-based (non-object) content is rendered: `host` (let mpv/the
    /// sink handle it) or `spatial` (render through the parametrable virtual bed
    /// — the default). The legacy `direct`/`virtual` values load as `spatial`.
    /// Absent = `spatial`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_render_mode: Option<crate::live_params::ChannelRenderMode>,
    /// Where the 4.x/5.x surround pair (`Ls`/`Rs`) is placed when rendered
    /// through the virtual bed: `side` (the default) or `back`. Only affects
    /// channel sources without dedicated back channels; 7.x ignores it.
    /// Absent = `side`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surround_placement: Option<crate::live_params::SurroundPlacement>,
    /// How output channels map to device ports: `by_index` (default — port N =
    /// layout speaker N, positionless) or `by_name` (positional: tag each channel
    /// with its speaker position so a position-aware host/sink routes by position).
    /// Absent = `by_index`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_channel_mapping: Option<crate::live_params::OutputChannelMapping>,
    /// Bed→height object generator (2D upmix) id for channel content
    /// (`none` / `copy_up` / `pad` / …). Absent / empty = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_generator_id: Option<String>,
    /// Live parameter overrides for the active object generator (param key →
    /// value), as declared by the generator's schema. Absent = each generator
    /// uses its declared defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_generator_params: Option<std::collections::HashMap<String, f32>>,
    /// Global renderer-synthesized-object master. Kept explicit once migrated so
    /// an off master can retain non-off child selections.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_objects_enabled: Option<bool>,
    /// Phantom extraction algorithm. Absent = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phantom_extract_mode: Option<crate::live_params::PhantomExtractMode>,
    /// Legacy phantom enable flag, read for migration and dropped on save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phantom_enabled: Option<bool>,
    /// Live parameter overrides for the phantom-extraction stage (`strength` /
    /// `passes` / `lift`). Absent = the stage's declared defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phantom_params: Option<std::collections::HashMap<String, f32>>,
    /// Parametrable virtual bed for channel-based (non-object) content. One
    /// entry per input-channel label (`L`, `R`, `C`, `LFE`, `Ls`, `Rs`, …):
    /// `spatialize:true` virtualizes the channel as an object at the entry's
    /// position; `spatialize:false` routes it direct to the matching output
    /// speaker (e.g. LFE → sub). Absent = built-in canonical poses (LFE direct,
    /// the rest virtualized). Reuses the speaker-layout schema so the Studio 3D
    /// editor can edit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_bed: Option<crate::speaker_layout::SpeakerLayout>,
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
    /// ADM axes negated to build the diffuse mirror image (`xy`, `y`, `xyz`,
    /// `none`, …). Absent means the default `xy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance_diffuse_mirror_axes: Option<String>,
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
    /// Independent binaural (headphone) output stage. Absent → classic speaker
    /// rendering. See [`crate::binaural`] and [`BinauralConfig`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binaural: Option<BinauralConfig>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    /// This matters most for `render.*`: any field added by a future
    /// version of the CLI / a host that we haven't migrated into this
    /// struct yet survives a save from another embedder.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// `render.binaural` config section: selects and tunes the headphone output.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct BinauralConfig {
    /// Output path: `"speaker"` (default) or `"binaural"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_mode: Option<String>,
    /// Metres represented by one ADM unit (isotropic distance scale). Default 1.0.
    /// Deliberately separate from `room_ratio`, which is anisotropic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_scale_m: Option<f32>,
    /// Effective head radius in metres for the ITD model (half the inter-ear
    /// distance). Default 0.0875 (KEMAR-ish).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_radius_m: Option<f32>,
    /// HRIR data set: `"synthetic"`, `"saf"`/`"kemar"` (embedded measured, default),
    /// or `"sofa"` (uses `hrtf_sofa_path`; needs the `sofa` build feature).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hrir_source: Option<String>,
    /// Path to a SOFA HRTF file, used when `hrir_source = "sofa"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hrtf_sofa_path: Option<PathBuf>,
    /// Head-tracking input wiring (SensorsOSC). Consumed from M2; stored now so
    /// the section round-trips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_tracking: Option<HeadTrackingConfig>,
    /// Shoebox early-reflection settings (externalization).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflections: Option<ReflectionsConfig>,
    /// Late-reverb (FDN) tail settings (distance / externalization).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverb: Option<ReverbConfig>,
    /// Distance low-pass on the direct path (air absorption). Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub air_absorption: Option<bool>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// Head-tracking OSC input configuration. The orientation arrives on an
/// arbitrary OSC address (e.g. SensorsOSC `/android/rotationvector`), so both
/// `render.binaural.reverb`: late-reverb (FDN) tail of the binaural stage.
/// Models the (small, dry) listening room, not the scene's acoustics.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ReverbConfig {
    /// Master enable. Default false (dry headphone output unless opted in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Return level (0..1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<f32>,
    /// Broadband decay time (s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rt60_s: Option<f32>,
    /// Pre-delay (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predelay_ms: Option<f32>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// `render.binaural.reflections`: shoebox early reflections for the binaural
/// stage (six first-order images, listener at the room centre).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct ReflectionsConfig {
    /// Master enable. Default false (dry headphone output unless opted in).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Room width (x), metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_width_m: Option<f32>,
    /// Room depth (y), metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_depth_m: Option<f32>,
    /// Room height (z), metres.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_height_m: Option<f32>,
    /// Per-reflection wall gain (0..1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<f32>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// the address and the value format are configurable.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct HeadTrackingConfig {
    /// OSC address carrying the head orientation. Empty/None → tracking disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osc_address: Option<String>,
    /// Orientation value format: `"auto"` (default), `"quat"`, `"rotvec"`, `"euler"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Recenter "forward" reference quaternion `[w, x, y, z]`, persisted so the
    /// chosen centering survives an engine rebuild (mpv track change) or restart.
    /// Absent until the tracker has been recentered (identity = uncentered).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_quat: Option<[f32; 4]>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
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

    /// [`Config::load_or_default`] plus consume-once handling of the live
    /// handoff sidecar (see [`live_sidecar_path`]). Returns the config and
    /// whether it came from a sidecar — callers should then mark the live
    /// state dirty, since a restored overlay is by definition unsaved.
    ///
    /// The first call that finds a fresh sidecar parses it, deletes the file
    /// and caches the result for the rest of the process, so the several
    /// config loads of a single boot (and an in-process destroy→create cycle
    /// of the FFI host) all see the same state. Stale (older than
    /// [`LIVE_SIDECAR_TTL`]) or unparsable sidecars are deleted without being
    /// applied.
    pub fn load_or_default_with_live(path: &Path) -> (Self, bool) {
        Self::load_or_default_with_live_ttl(path, LIVE_SIDECAR_TTL)
    }

    fn load_or_default_with_live_ttl(path: &Path, ttl: Duration) -> (Self, bool) {
        // Disk first, cache second: a sidecar written after an earlier consume
        // (an instance that yielded the port to us, or an in-process
        // destroy→create cycle) must win over the older cached overlay.
        let sidecar = live_sidecar_path(path);
        if sidecar.exists() {
            let fresh = std::fs::metadata(&sidecar)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|mtime| mtime.elapsed().ok())
                .map(|age| age < ttl)
                // Unreadable or future mtime: assume fresh rather than drop
                // a handoff over filesystem clock noise.
                .unwrap_or(true);
            let parsed = if fresh {
                Self::load(&sidecar).ok()
            } else {
                None
            };
            // Consume-once: the file goes away whether applied, stale or corrupt.
            let _ = std::fs::remove_file(&sidecar);
            if let Some(cfg) = parsed {
                log::info!(
                    "restored live state from {} (unsaved until an explicit save)",
                    sidecar.display()
                );
                LIVE_OVERLAY
                    .lock()
                    .unwrap()
                    .insert(path.to_path_buf(), cfg.clone());
                return (cfg, true);
            }
        }
        if let Some(cfg) = LIVE_OVERLAY.lock().unwrap().get(path) {
            return (cfg.clone(), true);
        }
        (Self::load_or_default(path), false)
    }
}

// ── Live-state handoff sidecar ──────────────────────────────────────────────
//
// On graceful shutdown an instance writes its full live state as a complete
// config file next to the persistent one. The next instance consumes it once
// at boot and treats it as UNSAVED live state: the persistent config is never
// touched, and an explicit save supersedes the overlay. This is how unsaved
// tweaks survive the standby-renderer ↔ mpv-embedded-renderer handoff.

/// Freshness window for the live sidecar: anything older is a leftover from an
/// unrelated session and is deleted without being applied.
pub const LIVE_SIDECAR_TTL: Duration = Duration::from_secs(600);

/// Sidecar path for `config_path`: `config.yaml` → `config.live.yaml`.
pub fn live_sidecar_path(config_path: &Path) -> PathBuf {
    let stem = config_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("config");
    config_path.with_file_name(format!("{stem}.live.yaml"))
}

/// Consumed sidecars for this process, keyed by the config path they overlay.
static LIVE_OVERLAY: Mutex<BTreeMap<PathBuf, Config>> = Mutex::new(BTreeMap::new());

/// Whether a consumed live overlay is active for `config_path` (cache only,
/// no disk access). Hosts use this to mark the restored state as unsaved.
pub fn live_overlay_active(config_path: &Path) -> bool {
    LIVE_OVERLAY.lock().unwrap().contains_key(config_path)
}

/// Forget any consumed sidecar. Called after writing a *new* sidecar (so a
/// later boot-in-the-same-process re-reads it), after an explicit save (a
/// deliberate save supersedes the overlay) and on reload_config (whose
/// contract is "discard live state").
pub fn clear_live_overlay_cache() {
    LIVE_OVERLAY.lock().unwrap().clear();
}

/// Returns the platform default config path without external dependencies.
///
/// - Linux:   `$XDG_CONFIG_HOME/omniphony/config.yaml`  (fallback: `~/.config/omniphony/config.yaml`)
/// - Windows: `%ProgramData%\omniphony\config.yaml`  (fallback: `C:\ProgramData\omniphony\config.yaml`)
///
/// On Windows the config is machine-wide so that the user-mode renderer and a
/// LocalSystem service resolve the *same* file — `%ProgramData%` is account
/// independent, unlike `%APPDATA%` which differs between the logged-in user and
/// the service's system profile.
pub fn default_config_path() -> Option<PathBuf> {
    // An environment that carved out its own runtime namespace pins the
    // directory, so several checkouts of the tree do not fight over one
    // config.yaml — nor over its live sidecar, which is consumed on read and
    // rewritten on exit and therefore propagates whatever the last process to
    // quit believed.
    if let Some(dir) = crate::runtime_env::config_dir() {
        return Some(dir.join("config.yaml"));
    }

    #[cfg(windows)]
    {
        let dir = windows_program_data_dir().join("omniphony");
        return Some(dir.join("config.yaml"));
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

/// Machine-wide `%ProgramData%` directory (same for every account), with the
/// canonical `C:\ProgramData` fallback when the env var is somehow unset.
#[cfg(windows)]
fn windows_program_data_dir() -> PathBuf {
    std::env::var("ProgramData")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// One-shot migration of the pre-machine-wide config: older builds stored it in
/// the per-user `%APPDATA%\omniphony\`. If the new `%ProgramData%` location has
/// no `config.yaml` yet but the legacy one exists, copy it (and its live
/// sidecar) over so upgrades keep the user's settings. Best-effort and guarded
/// by a process `Once`; never panics. Runs in whatever account the renderer
/// starts as — for a user-context launch this seeds ProgramData from the user's
/// config; the service (SYSTEM) at worst migrates its own old system-profile
/// config, which is harmless.
#[cfg(windows)]
pub fn migrate_legacy_windows_config() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let Some(appdata) = std::env::var("APPDATA").ok().filter(|p| !p.is_empty()) else {
            return;
        };
        let legacy_dir = PathBuf::from(appdata).join("omniphony");
        let legacy = legacy_dir.join("config.yaml");
        if !legacy.is_file() {
            return;
        }
        let dest_dir = windows_program_data_dir().join("omniphony");
        let dest = dest_dir.join("config.yaml");
        if dest.exists() {
            return; // already migrated / machine config present
        }
        if std::fs::create_dir_all(&dest_dir).is_err() {
            return;
        }
        if std::fs::copy(&legacy, &dest).is_err() {
            return;
        }
        // Carry the unsaved-live sidecar across too, if present.
        let legacy_live = live_sidecar_path(&legacy);
        if legacy_live.is_file() {
            let _ = std::fs::copy(&legacy_live, live_sidecar_path(&dest));
        }
    });
}

/// No-op on non-Windows: the Linux/macOS config path never moved.
#[cfg(not(windows))]
pub fn migrate_legacy_windows_config() {}

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
    fn head_tracking_reference_quat_round_trips_and_omits_when_absent() {
        // Present: serializes and parses back equal.
        let ht = HeadTrackingConfig {
            osc_address: Some("/android/rotationvector".to_string()),
            reference_quat: Some([0.5, 0.5, 0.5, 0.5]),
            ..Default::default()
        };
        let yaml = serde_yaml_ng::to_string(&ht).unwrap();
        assert!(yaml.contains("reference_quat"), "field not written: {yaml}");
        let back: HeadTrackingConfig = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.reference_quat, Some([0.5, 0.5, 0.5, 0.5]));

        // Absent: the key is skipped entirely.
        let bare = HeadTrackingConfig::default();
        let yaml = serde_yaml_ng::to_string(&bare).unwrap();
        assert!(
            !yaml.contains("reference_quat"),
            "None should omit the key: {yaml}"
        );
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

    #[test]
    fn backend_params_round_trip() {
        use crate::backend_params::ParamValue;

        let yaml = "\
render:
  render_backend: example
  backend_params:
    example:
      sharpness: 3.5
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("parse");
        let render = cfg.render.as_ref().unwrap();
        assert_eq!(
            render.backend_params["example"]["sharpness"],
            ParamValue::Float(3.5)
        );

        // Round-trips back out.
        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        let reparsed: Config = serde_yaml_ng::from_str(&out).expect("reparse");
        assert_eq!(
            reparsed.render.unwrap().backend_params["example"]["sharpness"],
            ParamValue::Float(3.5)
        );
    }

    #[test]
    fn empty_backend_params_are_not_serialised() {
        let cfg = Config {
            render: Some(RenderConfig::default()),
            ..Default::default()
        };
        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            !out.contains("backend_params"),
            "empty map should be skipped:\n{out}"
        );
    }

    // ── Live-handoff sidecar ────────────────────────────────────────────────
    // Each test uses its own config path; the overlay cache is per-path so
    // parallel tests don't interact.

    fn sidecar_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omniphony-sidecar-test-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_config_with_bridge(path: &Path, bridge: &str) {
        let cfg = Config {
            render: Some(RenderConfig {
                bridge_path: Some(PathBuf::from(bridge)),
                ..Default::default()
            }),
            ..Default::default()
        };
        cfg.save(path).unwrap();
    }

    fn bridge_of(cfg: &Config) -> Option<String> {
        cfg.render
            .as_ref()
            .and_then(|r| r.bridge_path.as_ref())
            .map(|p| p.display().to_string())
    }

    #[test]
    fn fresh_sidecar_is_consumed_once_and_cached() {
        let dir = sidecar_test_dir("fresh");
        let config = dir.join("config.yaml");
        write_config_with_bridge(&config, "/tmp/base.so");
        let sidecar = live_sidecar_path(&config);
        assert_eq!(sidecar, dir.join("config.live.yaml"));
        write_config_with_bridge(&sidecar, "/tmp/live.so");

        let (cfg, restored) = Config::load_or_default_with_live(&config);
        assert!(restored);
        assert_eq!(bridge_of(&cfg).as_deref(), Some("/tmp/live.so"));
        assert!(!sidecar.exists(), "sidecar must be consumed (deleted)");
        assert!(live_overlay_active(&config));

        // Subsequent loads in the same process see the same overlay.
        let (cfg2, restored2) = Config::load_or_default_with_live(&config);
        assert!(restored2);
        assert_eq!(bridge_of(&cfg2).as_deref(), Some("/tmp/live.so"));

        // The persistent config was never touched.
        let base = Config::load(&config).unwrap();
        assert_eq!(bridge_of(&base).as_deref(), Some("/tmp/base.so"));
    }

    #[test]
    fn stale_sidecar_is_deleted_without_being_applied() {
        let dir = sidecar_test_dir("stale");
        let config = dir.join("config.yaml");
        write_config_with_bridge(&config, "/tmp/base.so");
        let sidecar = live_sidecar_path(&config);
        write_config_with_bridge(&sidecar, "/tmp/live.so");

        // TTL zero ⇒ any on-disk sidecar counts as stale.
        let (cfg, restored) =
            Config::load_or_default_with_live_ttl(&config, Duration::from_secs(0));
        assert!(!restored);
        assert_eq!(bridge_of(&cfg).as_deref(), Some("/tmp/base.so"));
        assert!(!sidecar.exists(), "stale sidecar must still be deleted");
        assert!(!live_overlay_active(&config));
    }

    #[test]
    fn corrupt_sidecar_is_deleted_and_base_config_used() {
        let dir = sidecar_test_dir("corrupt");
        let config = dir.join("config.yaml");
        write_config_with_bridge(&config, "/tmp/base.so");
        let sidecar = live_sidecar_path(&config);
        std::fs::write(&sidecar, "{ this is : [ not yaml").unwrap();

        let (cfg, restored) = Config::load_or_default_with_live(&config);
        assert!(!restored);
        assert_eq!(bridge_of(&cfg).as_deref(), Some("/tmp/base.so"));
        assert!(!sidecar.exists(), "corrupt sidecar must be deleted");
    }

    #[test]
    fn rewritten_sidecar_wins_over_cached_overlay() {
        let dir = sidecar_test_dir("rewrite");
        let config = dir.join("config.yaml");
        write_config_with_bridge(&config, "/tmp/base.so");
        let sidecar = live_sidecar_path(&config);

        write_config_with_bridge(&sidecar, "/tmp/live-a.so");
        let (cfg_a, _) = Config::load_or_default_with_live(&config);
        assert_eq!(bridge_of(&cfg_a).as_deref(), Some("/tmp/live-a.so"));

        // A second handoff in the same process (FFI destroy→create cycle)
        // re-writes the sidecar; disk must win over the cached overlay.
        write_config_with_bridge(&sidecar, "/tmp/live-b.so");
        let (cfg_b, restored) = Config::load_or_default_with_live(&config);
        assert!(restored);
        assert_eq!(bridge_of(&cfg_b).as_deref(), Some("/tmp/live-b.so"));
        assert!(!sidecar.exists());
    }
}

#[cfg(test)]
mod config_dir_override_tests {
    /// The env is process-global, so this test owns it for its duration.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_pinned_runtime_namespace_moves_the_config_out_of_the_shared_location() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("OMNIPHONY_CONFIG_DIR").ok();

        unsafe { std::env::set_var("OMNIPHONY_CONFIG_DIR", "/tmp/omniphony-wf-probe") };
        let pinned = super::default_config_path();
        unsafe { std::env::remove_var("OMNIPHONY_CONFIG_DIR") };
        let shared = super::default_config_path();

        match previous {
            Some(v) => unsafe { std::env::set_var("OMNIPHONY_CONFIG_DIR", v) },
            None => unsafe { std::env::remove_var("OMNIPHONY_CONFIG_DIR") },
        }

        assert_eq!(
            pinned,
            Some(std::path::PathBuf::from(
                "/tmp/omniphony-wf-probe/config.yaml"
            ))
        );
        assert_ne!(
            pinned, shared,
            "the override must not resolve to the shared path"
        );
    }
}
