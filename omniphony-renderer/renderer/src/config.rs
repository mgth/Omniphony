use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Mapping;

pub(crate) mod unknown_values;

use unknown_values::{EnumKey, KeepsUnknownValues};

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Config {
    /// The [`CONFIG_SCHEMA_VERSION`] of the build that last saved the file;
    /// absent in a file saved before the key existed. A build meeting a
    /// higher one reads what it can and refuses to write the file
    /// ([`ConfigLoadStatus::NewerSchema`]). Every save writes this build's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<GlobalConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderConfig>,
    /// Name of the active configuration profile (see docs/config-profiles.md).
    /// `render:` always holds the active profile's authoritative content;
    /// this key names it. Absent means the implicit `"default"` profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
    /// Named configuration profiles: each entry is a complete render section
    /// (same schema as `render:`). The active entry is a mirror of `render:`,
    /// realigned by [`Config::save`]; the others are the switch targets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, RenderConfig>,
    /// Set only in a live-handoff sidecar written while the engine was
    /// running on the defaults it fell back to because `config.yaml` failed
    /// to parse. The state it carries is still those defaults, whatever the
    /// file holds by the time the next instance restores it, so that instance
    /// keeps `config_status = parse_error` (see [`boot_load_status`]) and
    /// refuses to Save until a reload. A Save starts from `config.yaml`, never
    /// from a sidecar, so this never reaches the persistent file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live_from_parse_error: bool,
    /// Captures any top-level key not modelled above so a load → mutate
    /// → save round-trip preserves it verbatim. Without this, every
    /// embedder of the engine that triggers `persist::save_live_config`
    /// (FFI in mpv-omniphony, future hosts, …) would silently strip
    /// CLI-only or host-specific keys from the user's config YAML.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// Name of the implicit profile a flat legacy config migrates into.
pub const DEFAULT_PROFILE: &str = "default";

/// The `schema_version` this build writes. Bump it when a build changes what
/// an existing key means, or moves or retires one, so that an older build,
/// which would read and save such a file on its own terms, refuses to write it
/// instead. Adding a key or an enum value needs no bump: an older build keeps
/// both through a save (`extra`, `unknown_values`).
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

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

/// `Deserialize` and `Serialize` are implemented below, around the derived
/// ones (`remote = "Self"`): an enum value this build does not know is kept
/// rather than failing the file (see `unknown_values`).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(remote = "Self")]
pub struct RenderConfig {
    #[serde(
        default,
        deserialize_with = "kept_enum::input_mode",
        skip_serializing_if = "Option::is_none"
    )]
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
    pub backend_params: crate::plugin::ParamBag,
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
    pub bed_conform: Option<bool>,
    /// How channel-based (non-object) content is rendered: `host` (let mpv/the
    /// sink handle it) or `spatial` (render through the parametrable virtual bed
    /// — the default). The legacy `direct`/`virtual` values load as `spatial`.
    /// Absent = `spatial`.
    #[serde(
        default,
        deserialize_with = "kept_enum::channel_render_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub channel_render_mode: Option<crate::live_params::ChannelRenderMode>,
    /// Legacy flat parameter map of "the active object generator", read for
    /// migration into `generator_params[object_generator_id]` and dropped on
    /// save (see [`crate::plugin::PluginParams::from_config`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_generator_params: Option<std::collections::HashMap<String, f32>>,
    /// Per-generator parameter values (see [`crate::plugin`]), keyed by
    /// generator id then param key, as each generator's schema declares
    /// them. Absent = every generator at its declared defaults.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub generator_params: crate::plugin::ParamBag,
    /// Legacy phantom enable flag, read for migration and dropped on save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phantom_enabled: Option<bool>,
    /// Legacy float-only parameter map of the phantom-extraction stage, read
    /// for migration into `phantom_extract_params` and dropped on save.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phantom_params: Option<std::collections::HashMap<String, f32>>,
    /// Parameter values of the phantom-extraction stage (param key → value),
    /// as its schema declares them. Absent = the stage's declared defaults.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub phantom_extract_params: crate::plugin::ParamMap,
    /// Legacy single virtual bed for channel-based content, read for
    /// migration only: it becomes `placement.generic` in manual mode with
    /// these entries, and is dropped on the next save. See `placement`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_bed: Option<crate::speaker_layout::SpeakerLayout>,
    /// Where fixed channels go, per source family: a mode (`sphere`, `room`,
    /// `manual`) and the family's entries (`spatialize`, `gain_db`, and the
    /// pose in manual mode), families inheriting from `generic`. Absent =
    /// every family at its built-in default (Auro-3D a sphere, the rest the
    /// room model, LFE direct). See `renderer::placement`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<crate::placement::PlacementConfig>,
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
    #[serde(
        default,
        deserialize_with = "kept_enum::size_to_spread_mode",
        skip_serializing_if = "Option::is_none"
    )]
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
    /// OSC meter cadence (Hz). Persisted so the renderer is the source of truth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meter_rate: Option<f32>,
    /// OSC diag-publication cadence (Hz). Persisted alongside `meter_rate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diag_rate: Option<f32>,
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
    /// The options declared in `options::declared`, one key each (see
    /// `DeclaredOptionsConfig`). Before `extra`, which takes what is left.
    #[serde(flatten)]
    pub options: crate::options::DeclaredOptionsConfig,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    /// This matters most for `render.*`: any field added by a future
    /// version of the CLI / a host that we haven't migrated into this
    /// struct yet survives a save from another embedder. An enum value this
    /// build does not know is kept here too, under its own key.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// `render.binaural` config section: selects and tunes the headphone output.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct BinauralConfig {
    /// Output path: `"speaker"` (default) or `"binaural"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_mode: Option<String>,
    /// Binaural stage input: `"direct"` (default, one HRTF per object) or
    /// `"cascaded"` (the speaker pipeline rendered on the app layout as a
    /// virtual room, then binauralised; convolution cost bound by the layout
    /// size — the embedded/low-power path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Headphone L/R linear gains for the binaural output (default unity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ear_gains: Option<[f32; 2]>,
    /// Headphone L/R mute flags for the binaural output (default false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ear_mutes: Option<[bool; 2]>,
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
    /// Path to a SOFA room-response file (`MultiSpeakerBRIR`, or a
    /// per-direction set with room-length responses), used when
    /// `hrir_source = "brir"`. Rendered through the virtual-speaker path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brir_sofa_path: Option<PathBuf>,
    /// Keep every measured head orientation of the BRIR resident (head
    /// tracking). Default: only when `head_tracking.osc_address` is set;
    /// otherwise the single orientation nearest straight ahead is loaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brir_head_tracking: Option<bool>,
    /// Longest BRIR kept, in seconds (default 2.0; 0 = whole responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brir_max_length_s: Option<f32>,
    /// Decibels below a response's total energy at which its tail is cut
    /// (default 60).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brir_tail_floor_db: Option<f32>,
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
    /// Diffuse-field equalisation of the HRIR set. Default false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diffuse_field_eq: Option<bool>,
    /// How finely a direction must change before its HRIR is rebuilt:
    /// `"exact"` (default, bit-exact) | `"fine"` | `"balanced"` | `"coarse"`.
    /// Anything but `exact` trades fidelity for speed — see
    /// [`crate::live_params::HrirUpdateLattice`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hrir_update_lattice: Option<String>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

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
    /// Scale on the delay-line lengths (0.5–2, 1 = nominal).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    /// Decay time below ~250 Hz as a ratio of `rt60_s` (0.25–4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rt60_low_ratio: Option<f32>,
    /// Decay time above ~4 kHz as a ratio of `rt60_s` (0.25–4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rt60_high_ratio: Option<f32>,
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
    /// High-frequency cutoff of the walls (Hz, 1000–20000; 20000 = none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_cutoff_hz: Option<f32>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

/// Head-tracking OSC input configuration. The orientation arrives on an
/// arbitrary OSC address (e.g. SensorsOSC `/android/rotationvector`), so both
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
    /// Sensor-to-head axis calibration quaternion `[w, x, y, z]` (see the
    /// three-pose calibration in `BINAURAL.md`), persisted like the
    /// reference. Absent until calibrated (identity = sensor axes are the
    /// head's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axes_quat: Option<[f32; 4]>,
    /// Exponential orientation smoothing in [0, 0.999]: 0 = instant, higher =
    /// smoother/laggier. Absent → the tracker default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub smoothing: Option<f32>,
    /// Flip the applied rotation, for sensors whose motion comes out mirrored.
    /// Absent → false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invert: Option<bool>,
    /// See `Config::extra` — preserve unknown keys through round-trips.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub enum InputModeConfig {
    #[serde(rename = "pipe_bridge", alias = "bridge")]
    Bridge,
    /// The PipeWire sink. `pipewire_bridge` was its name while a PCM-only
    /// sink held `pipewire` / `live`; all three still deserialize here, so a
    /// config saved under any of them keeps loading instead of falling back
    /// to defaults.
    #[serde(rename = "pipewire", alias = "pipewire_bridge", alias = "live")]
    Pipewire,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputBackendConfig {
    Pipewire,
}

/// Wire value of the retired ASIO live-input backend. It was accepted here
/// while a Windows capture path was planned, but that path was never
/// implemented and the value has been removed.
const RETIRED_ASIO_INPUT_BACKEND: &str = "asio";

/// `live_input.backend`, tolerant of the retired `asio` value: a config saved
/// with it still loads, with the key dropped (the platform default applies)
/// and a warning, instead of the whole file failing to parse. Any other
/// unknown value is still an error, as before.
fn deserialize_live_input_backend<'de, D>(
    deserializer: D,
) -> Result<Option<InputBackendConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(value) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    match value.as_str() {
        "pipewire" => Ok(Some(InputBackendConfig::Pipewire)),
        RETIRED_ASIO_INPUT_BACKEND => {
            log::warn!(
                "config: live_input.backend 'asio' is no longer supported (the ASIO live-input \
                 backend was never implemented); ignoring it and using the platform default"
            );
            Ok(None)
        }
        other => Err(serde::de::Error::unknown_variant(other, &["pipewire"])),
    }
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
    #[serde(
        default,
        deserialize_with = "kept_enum::backend",
        skip_serializing_if = "Option::is_none"
    )]
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
    #[serde(
        default,
        deserialize_with = "kept_enum::clock_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub clock_mode: Option<InputClockModeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(
        default,
        deserialize_with = "kept_enum::map",
        skip_serializing_if = "Option::is_none"
    )]
    pub map: Option<InputMapModeConfig>,
    #[serde(
        default,
        deserialize_with = "kept_enum::lfe_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub lfe_mode: Option<InputLfeModeConfig>,
    /// See `Config::extra` — preserve unknown keys through round-trips, and
    /// the enum values this build does not know (see `unknown_values`).
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

impl RenderConfig {
    /// The input an absent `input_mode` stands for: the bridge pipe.
    pub fn input_mode_or_default(&self) -> InputModeConfig {
        self.input_mode.clone().unwrap_or(InputModeConfig::Bridge)
    }
}

impl LiveInputConfig {
    /// The map an absent `map` stands for.
    pub const DEFAULT_MAP: InputMapModeConfig = InputMapModeConfig::SevenOneFixed;
    /// The LFE handling an absent `lfe_mode` stands for.
    pub const DEFAULT_LFE_MODE: InputLfeModeConfig = InputLfeModeConfig::Direct;

    /// The clock an absent `clock_mode` stands for: upstream for the PipeWire
    /// sink, the DAC otherwise.
    pub fn default_clock_mode(input_mode: &InputModeConfig) -> InputClockModeConfig {
        match input_mode {
            InputModeConfig::Pipewire => InputClockModeConfig::Upstream,
            InputModeConfig::Bridge => InputClockModeConfig::Dac,
        }
    }

    pub fn clock_mode_or_default(&self, input_mode: &InputModeConfig) -> InputClockModeConfig {
        self.clock_mode
            .clone()
            .unwrap_or_else(|| Self::default_clock_mode(input_mode))
    }

    pub fn map_or_default(&self) -> InputMapModeConfig {
        self.map.clone().unwrap_or(Self::DEFAULT_MAP)
    }

    pub fn lfe_mode_or_default(&self) -> InputLfeModeConfig {
        self.lfe_mode.clone().unwrap_or(Self::DEFAULT_LFE_MODE)
    }
}

/// The `deserialize_with` readers of the enum-typed fields: a value this
/// build does not know falls back to the default and is kept for the next
/// save (see [`unknown_values`]).
mod kept_enum {
    use super::*;

    macro_rules! reader {
        ($section:literal, $parent:expr, $name:ident: $ty:ty) => {
            reader!($section, $parent, $name: $ty, |value| Option::<$ty>::deserialize(value));
        };
        ($section:literal, $parent:expr, $name:ident: $ty:ty, $read:expr) => {
            pub(super) fn $name<'de, D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> Result<Option<$ty>, D::Error> {
                unknown_values::keep_unknown(deserializer, $parent, stringify!($name), $section, $read)
            }
        };
    }

    reader!("render.", None, input_mode: InputModeConfig);
    reader!("render.", None, channel_render_mode: crate::live_params::ChannelRenderMode);
    reader!("render.", None, size_to_spread_mode: crate::render_backend::SizeToSpreadMode);
    // The retired `asio` is read (and dropped) by the field's own reader, so
    // it is not kept.
    reader!(
        "render.live_input.",
        Some("live_input"),
        backend: InputBackendConfig,
        |value| deserialize_live_input_backend(value)
    );
    reader!("render.live_input.", Some("live_input"), clock_mode: InputClockModeConfig);
    reader!("render.live_input.", Some("live_input"), map: InputMapModeConfig);
    reader!("render.live_input.", Some("live_input"), lfe_mode: InputLfeModeConfig);
}

/// [`EnumKey`] for a field of `render` whose absent value is `default`.
macro_rules! render_enum_key {
    ($field:ident : $ty:ty = $default:expr) => {
        EnumKey {
            parent: None,
            key: stringify!($field),
            chosen: |render| render.$field.as_ref().is_some_and(|v| *v != $default),
            clear: |render| render.$field = None,
        }
    };
}

/// [`EnumKey`] for a field of `render.live_input`; `default` is what an
/// absent value stands for, given the render section.
macro_rules! live_input_enum_key {
    ($field:ident : $ty:ty, default = $default:expr) => {
        EnumKey {
            parent: Some("live_input"),
            key: stringify!($field),
            chosen: |render: &RenderConfig| {
                let default: fn(&RenderConfig) -> Option<$ty> = $default;
                render.live_input.as_ref().is_some_and(|live_input| {
                    live_input.$field.is_some() && live_input.$field != default(render)
                })
            },
            clear: |render| {
                if let Some(live_input) = render.live_input.as_mut() {
                    live_input.$field = None;
                }
            },
        }
    };
}

impl KeepsUnknownValues for RenderConfig {
    const ENUM_KEYS: &'static [EnumKey<Self>] = &[
        render_enum_key!(input_mode: InputModeConfig = InputModeConfig::Bridge),
        render_enum_key!(
            channel_render_mode: crate::live_params::ChannelRenderMode =
                crate::config_fields::channel_render_mode::DEFAULT
        ),
        render_enum_key!(
            size_to_spread_mode: crate::render_backend::SizeToSpreadMode =
                crate::render_backend::SizeToSpreadMode::default()
        ),
        live_input_enum_key!(
            backend: InputBackendConfig,
            // Absent is the platform default, which no value spells.
            default = |_| None
        ),
        live_input_enum_key!(
            clock_mode: InputClockModeConfig,
            default = |render| Some(LiveInputConfig::default_clock_mode(
                &render.input_mode_or_default()
            ))
        ),
        live_input_enum_key!(
            map: InputMapModeConfig,
            default = |_| Some(LiveInputConfig::DEFAULT_MAP)
        ),
        live_input_enum_key!(
            lfe_mode: InputLfeModeConfig,
            default = |_| Some(LiveInputConfig::DEFAULT_LFE_MODE)
        ),
    ];

    fn enum_keys() -> impl Iterator<Item = &'static EnumKey<Self>> {
        Self::ENUM_KEYS
            .iter()
            .chain(crate::options::DECLARED_ENUM_KEYS.iter().flatten())
    }

    fn extra(&self, parent: Option<&str>) -> Option<&Mapping> {
        match parent {
            None => Some(&self.extra),
            Some(_) => self.live_input.as_ref().map(|live_input| &live_input.extra),
        }
    }

    fn extra_mut(&mut self, parent: Option<&str>) -> Option<&mut Mapping> {
        match parent {
            None => Some(&mut self.extra),
            Some(_) => self
                .live_input
                .as_mut()
                .map(|live_input| &mut live_input.extra),
        }
    }
}

impl<'de> Deserialize<'de> for RenderConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        unknown_values::deserialize(deserializer, RenderConfig::deserialize)
    }
}

impl Serialize for RenderConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        unknown_values::serialize(self, serializer, |render, serializer| {
            RenderConfig::serialize(render, serializer)
        })
    }
}

impl RenderConfig {
    /// When the room geometry is stored in metres (`room_*_m`), derive the
    /// renderer-facing ratios + the layout radius from them so the rest of the
    /// pipeline keeps consuming `room_ratio` + `current_layout.radius_m`
    /// unchanged. Width is the reference: `radius = Width/2` (so width ratio is
    /// always 1). A no-op when the metre fields are absent (legacy config).
    pub fn normalize_room_meters(&mut self) {
        let Some(derived) = self.room_ratios_from_meters() else {
            return;
        };
        self.room_ratio = Some(derived.ratio);
        self.room_ratio_rear = Some(derived.rear);
        self.room_ratio_lower = Some(derived.lower);
        if let Some(layout) = self.current_layout.as_mut() {
            layout.radius_m = derived.radius;
        }
    }

    /// The ratio keys and layout radius the metre fields stand for, without
    /// writing them (see [`Self::normalize_room_meters`]). `None` when the
    /// room is not stored in metres.
    pub fn room_ratios_from_meters(&self) -> Option<RoomFromMeters> {
        let width_m = self.room_width_m?;
        let radius = (width_m / 2.0).max(0.01);
        let front = self.room_front_m.unwrap_or(2.0 * radius).max(0.0);
        let rear = self.room_rear_m.unwrap_or(radius).max(0.0);
        let height = self.room_height_m.unwrap_or(radius).max(0.0);
        let lower = self.room_lower_m.unwrap_or(0.5 * radius).max(0.0);
        Some(RoomFromMeters {
            ratio: format!("1.0,{:.6},{:.6}", front / radius, height / radius),
            rear: (rear / radius).max(0.01),
            lower: (lower / radius).max(0.01),
            radius,
        })
    }
}

/// The renderer-facing room derived from the metre fields
/// ([`RenderConfig::room_ratios_from_meters`]).
#[derive(Debug, Clone, PartialEq)]
pub struct RoomFromMeters {
    /// The `room_ratio` string (`"1.0,length,height"`, six decimals — the
    /// width is the reference).
    pub ratio: String,
    pub rear: f32,
    pub lower: f32,
    /// The layout radius: half the width.
    pub radius: f32,
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
    /// File parsed, but a newer build wrote it (a `schema_version` above
    /// [`CONFIG_SCHEMA_VERSION`]): the renderer runs on what this build
    /// understands of it, and refuses to write it.
    NewerSchema,
}

impl ConfigLoadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigLoadStatus::Loaded => "loaded",
            ConfigLoadStatus::Missing => "missing",
            ConfigLoadStatus::ParseError => "parse_error",
            ConfigLoadStatus::NewerSchema => "newer_schema",
        }
    }

    /// The status of a file that parsed into `config`.
    fn of_loaded(config: &Config) -> Self {
        if config.is_from_newer_build() {
            ConfigLoadStatus::NewerSchema
        } else {
            ConfigLoadStatus::Loaded
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

    /// Read `path` once: `Ok(None)` when there is no file, an error when it is
    /// present but fails to read or parse. The one place that tells a missing
    /// file from a broken one; every loader below goes through it, so the
    /// published `config_status` and the write refusal cannot disagree.
    fn load_if_present(path: &Path) -> anyhow::Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        Self::load(path).map(Some)
    }

    /// Load config from path, returning default if the file is absent.
    /// Prints a warning to stderr (not the log) if the file exists but fails to parse,
    /// because this may be called before the logger is initialized.
    pub fn load_or_default(path: &Path) -> Self {
        Self::load_or_default_with_status(path).0
    }

    /// [`Config::load_or_default`], plus which of the three outcomes it was,
    /// from a single read of the file.
    pub fn load_or_default_with_status(path: &Path) -> (Self, ConfigLoadStatus) {
        match Self::load_if_present(path) {
            Ok(Some(cfg)) => {
                let status = ConfigLoadStatus::of_loaded(&cfg);
                (cfg, status)
            }
            Ok(None) => (Self::default(), ConfigLoadStatus::Missing),
            Err(e) => {
                eprintln!(
                    "warning: failed to parse config file {}: {}",
                    path.display(),
                    e
                );
                (Self::default(), ConfigLoadStatus::ParseError)
            }
        }
    }

    /// Load the file a write is about to amend. A missing file starts from
    /// [`Config::for_new_file`]: the defaults [`Config::load_or_default`]
    /// reads, with the OSC state the engine had without a file; a file that
    /// is present but fails to parse is an error instead, so the write that
    /// follows cannot replace the user's layout, profiles and unknown keys
    /// with defaults. A file a newer build wrote is an error too: this build
    /// would save it on its own terms (see [`CONFIG_SCHEMA_VERSION`]). Every
    /// writer of the persistent config starts here.
    pub fn load_for_update(path: &Path) -> anyhow::Result<Self> {
        let config = Self::load_if_present(path)
            .map(|config| config.unwrap_or_else(Self::for_new_file))
            .map_err(|e| {
                anyhow::anyhow!(
                    "{} failed to parse, so it was left untouched; fix or remove it first ({e})",
                    path.display()
                )
            })?;
        if config.is_from_newer_build() {
            anyhow::bail!(
                "{} was written by a newer Omniphony (config schema {}, this build knows up to \
                 {CONFIG_SCHEMA_VERSION}), so it was left untouched; save it from that version",
                path.display(),
                config.schema_version.unwrap_or_default()
            );
        }
        Ok(config)
    }

    /// What a write starts from when no config file exists yet: the
    /// defaults, plus `render.osc: true`. Without a file, the engine embedded
    /// in a player runs with OSC on (`orender_engine::osc_settings`), but a
    /// file without the key means off. Recording the state the engine was in
    /// keeps whatever creates the file (Studio's first Save, a view write, a
    /// profile operation) from switching OSC off for the next start.
    fn for_new_file() -> Self {
        Self {
            render: Some(RenderConfig {
                osc: Some(true),
                ..RenderConfig::default()
            }),
            ..Self::default()
        }
    }

    /// Whether a build newer than this one saved the file this was read from.
    pub fn is_from_newer_build(&self) -> bool {
        self.schema_version
            .is_some_and(|version| version > CONFIG_SCHEMA_VERSION)
    }

    /// Diagnose what `load_or_default` would actually do for `path`, without
    /// keeping the result. `load_or_default` silently swallows both a missing
    /// file and a parse error into `Config::default()` (no current_layout → the
    /// default speaker preset + room), which is exactly how a host can end up on
    /// the wrong geometry while looking like it "has" a config path. Studio
    /// surfaces this in About so the silent fallback becomes visible.
    pub fn load_status(path: &Path) -> ConfigLoadStatus {
        match Self::load_if_present(path) {
            Ok(Some(cfg)) => ConfigLoadStatus::of_loaded(&cfg),
            Ok(None) => ConfigLoadStatus::Missing,
            Err(_) => ConfigLoadStatus::ParseError,
        }
    }

    /// Serialize this config to YAML and write it to `path`, keeping the file
    /// it replaces as `<name>.bak` ([`backup_path`]). Parent directories are
    /// created automatically. The write is atomic (see `replace_file`): a
    /// crash or a full disk leaves the previous file, never half of the new one.
    ///
    /// Saving realigns the profile mirror first (see [`Config::sync_active_profile`]):
    /// the written file always has `profiles[active] == render`, and a flat
    /// legacy file is migrated into the implicit `"default"` profile on its
    /// first save by a profiles-aware binary.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        replace_file(path, self.to_yaml()?.as_bytes(), true, || Ok(()))
    }

    /// [`Config::save`] without the `.bak` and without syncing the directory:
    /// still atomic, but cheap enough for the writes that are not a user's
    /// deliberate save. The transient live-handoff sidecar is consumed once and
    /// must not leave a backup behind; a targeted view write (meter rate,
    /// head-tracker calibration) runs on the OSC thread and must not rotate
    /// the `.bak` away from the file as it was before the last Save.
    pub fn save_without_backup(&self, path: &Path) -> anyhow::Result<()> {
        replace_file(path, self.to_yaml()?.as_bytes(), false, || Ok(()))
    }

    fn to_yaml(&self) -> anyhow::Result<String> {
        let mut out = self.clone();
        out.schema_version = Some(CONFIG_SCHEMA_VERSION);
        out.sync_active_profile();
        Ok(serde_yaml_ng::to_string(&out)?)
    }

    /// Name of the active profile (`"default"` when the file predates profiles).
    pub fn active_profile_name(&self) -> &str {
        self.active_profile.as_deref().unwrap_or(DEFAULT_PROFILE)
    }

    /// All profile names, active first-class: the active name is present even
    /// before the mirror entry exists (flat legacy file).
    pub fn profile_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.profiles.keys().cloned().collect();
        let active = self.active_profile_name();
        if !names.iter().any(|n| n == active) {
            names.push(active.to_string());
            names.sort();
        }
        names
    }

    /// The client-visible profiles view (active name + name list), built here
    /// so the embedded engine, the CLI bootstrap and the OSC profile ops all
    /// derive it from the same accessors.
    pub fn profiles_info(&self) -> crate::live_params::ProfilesInfo {
        crate::live_params::ProfilesInfo {
            active: self.active_profile_name().to_string(),
            names: self.profile_names(),
        }
    }

    /// Mirror the active profile: `render:` is authoritative for the active
    /// profile's content, so keep `profiles[active]` aligned with it. On a
    /// flat legacy file this materialises the implicit `"default"` profile.
    /// No-op without a render section (nothing to profile yet).
    pub fn sync_active_profile(&mut self) {
        let Some(render) = self.render.as_ref() else {
            return;
        };
        let name = self.active_profile_name().to_string();
        self.profiles.insert(name.clone(), render.clone());
        self.active_profile = Some(name);
    }

    /// Switch the active profile to `name`: mirror the current `render:` into
    /// the outgoing profile, install the target profile's content as the new
    /// `render:` — carrying over the machine-level input plumbing
    /// (`input_mode`, `input_pipe`, `live_input`, `bridge_path`), which
    /// belongs to the machine, not to a listening setup — and point
    /// `active_profile` at it. Fails without touching anything if the target
    /// does not exist. The caller re-seeds the live state and rebuilds the
    /// topology from the new `render:` (see docs/config-profiles.md).
    pub fn switch_profile(&mut self, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("profile name is empty");
        }
        if name == self.active_profile_name() {
            return Ok(());
        }
        if !self.profiles.contains_key(name) {
            anyhow::bail!("unknown profile '{name}'");
        }
        self.sync_active_profile();
        let Some(mut incoming) = self.profiles.get(name).cloned() else {
            // Unreachable behind the contains_key check above, but never fall
            // back to a default render section: that would silently replace
            // the user's setup instead of erroring.
            anyhow::bail!("unknown profile '{name}'");
        };
        if let Some(outgoing) = self.render.as_ref() {
            incoming.input_mode = outgoing.input_mode.clone();
            // An input mode only a newer build knows is kept in `extra`
            // (`live_input` carries its own along).
            match outgoing.extra.get("input_mode") {
                Some(kept) => incoming.extra.insert("input_mode".into(), kept.clone()),
                None => incoming.extra.shift_remove("input_mode"),
            };
            incoming.input_pipe = outgoing.input_pipe.clone();
            incoming.live_input = outgoing.live_input.clone();
            incoming.bridge_path = outgoing.bridge_path.clone();
        }
        incoming.normalize_room_meters();
        self.render = Some(incoming);
        self.active_profile = Some(name.to_string());
        self.sync_active_profile();
        Ok(())
    }

    /// Create profile `name` as a copy of the current `render:` (no switch).
    /// Fails if the name is empty or already taken.
    pub fn create_profile(&mut self, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("profile name is empty");
        }
        if name == self.active_profile_name() || self.profiles.contains_key(name) {
            anyhow::bail!("profile '{name}' already exists");
        }
        self.sync_active_profile();
        let content = self.render.clone().unwrap_or_default();
        self.profiles.insert(name.to_string(), content);
        Ok(())
    }

    /// Delete profile `name`. The active profile cannot be deleted (switch
    /// away first) — that keeps `render:`, `active_profile` and the mirror
    /// trivially consistent.
    pub fn delete_profile(&mut self, name: &str) -> anyhow::Result<()> {
        let name = name.trim();
        if name == self.active_profile_name() {
            anyhow::bail!("cannot delete the active profile '{name}'");
        }
        if self.profiles.remove(name).is_none() {
            anyhow::bail!("unknown profile '{name}'");
        }
        Ok(())
    }

    /// Rename profile `old` to `new`; follows `active_profile` when renaming
    /// the active one. Fails on an unknown source or a colliding target.
    pub fn rename_profile(&mut self, old: &str, new: &str) -> anyhow::Result<()> {
        let (old, new) = (old.trim(), new.trim());
        if new.is_empty() {
            anyhow::bail!("profile name is empty");
        }
        if old == new {
            return Ok(());
        }
        if new == self.active_profile_name() || self.profiles.contains_key(new) {
            anyhow::bail!("profile '{new}' already exists");
        }
        self.sync_active_profile();
        let Some(content) = self.profiles.remove(old) else {
            anyhow::bail!("unknown profile '{old}'");
        };
        self.profiles.insert(new.to_string(), content);
        if self.active_profile_name() == old {
            self.active_profile = Some(new.to_string());
        }
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

/// Backup path for `path`: `config.yaml` → `config.yaml.bak`.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Replace `path` with `contents` atomically: write a temp file in the same
/// directory, sync it, copy the current file to [`backup_path`] when
/// `deliberate` is set, then rename the temp file over `path` (and, when
/// `deliberate`, sync the directory so the rename itself survives a crash). A failure at any step
/// leaves the current file as it was. A symlinked `path` is written through,
/// so the link survives, and the `.bak` sits next to the link, where the
/// docs and the user look for it. `before_rename` is the test seam for a failure
/// between the write and the rename.
///
/// The rename needs a writable directory and replaces the file's inode, so
/// where that would refuse a write the old in-place `fs::write` allowed, or
/// change what the file is, the file is rewritten in place instead (not
/// atomic, as before this existed): the directory is not writable (a config
/// under `/etc` whose file alone is writable), the file has other hard links,
/// or it belongs to another owner or group (a save run as root on a user's
/// file). ACLs and extended attributes are not carried over by the rename.
fn replace_file(
    path: &Path,
    contents: &[u8],
    deliberate: bool,
    before_rename: impl FnOnce() -> std::io::Result<()>,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    // One save at a time in this process. Two saves of one file both copy
    // it to the same `.bak` and rename over the same target: on Windows the
    // second is refused (a sharing violation), elsewhere the `.bak` can come
    // out torn. Saves are rare and never on the audio path.
    static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _saving = SAVING.lock().unwrap_or_else(|e| e.into_inner());

    let target = resolve_symlinks(path)?;
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir)?;
    let name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", target.display()))?
        .to_string_lossy();
    let current = std::fs::metadata(&target).ok();
    // The rename only needs the directory, so it would replace a file the
    // old in-place `fs::write` was refused (`chmod a-w`, an ACL): ask for
    // write access to the file itself first. Opening without truncating
    // changes nothing.
    if current.is_some() {
        std::fs::OpenOptions::new().write(true).open(&target)?;
    }

    let (tmp, mut file) = match create_temp_file(&dir, &name) {
        Ok(created) => created,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && current.is_some() => {
            return write_in_place(&target, &backup_path(path), contents, deliberate);
        }
        Err(e) => return Err(e.into()),
    };
    if let Some(meta) = &current {
        if !rename_keeps_identity(meta, &file.metadata()?) {
            drop(file);
            let _ = std::fs::remove_file(&tmp);
            return write_in_place(&target, &backup_path(path), contents, deliberate);
        }
    }

    let result = (|| -> anyhow::Result<()> {
        file.write_all(contents)?;
        file.sync_all()?;
        if let Some(meta) = &current {
            file.set_permissions(meta.permissions())?;
        }
        drop(file);
        before_rename()?;
        if deliberate && current.is_some() {
            std::fs::copy(&target, backup_path(path))?;
        }
        std::fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    // Make the rename itself durable. Best-effort: not every platform can
    // open a directory. A light write skips it: the temp file was synced, so
    // a crash leaves the old file or the new one, never a torn one.
    #[cfg(unix)]
    if deliberate {
        if let Ok(dir) = std::fs::File::open(&dir) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// Follow `path` through any chain of symlinks to the file a write lands on.
/// Unlike `canonicalize`, the final file need not exist: a link to a config
/// not created yet is written through, creating it, as `fs::write` did. A
/// relative link resolves against the directory of the link itself.
fn resolve_symlinks(path: &Path) -> std::io::Result<PathBuf> {
    // The usual kernel limit (ELOOP).
    const MAX_LINKS: usize = 40;
    let mut target = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&target)?;
                target = match target.parent() {
                    Some(parent) => parent.join(link),
                    None => link,
                };
            }
            _ => return Ok(target),
        }
    }
    Err(std::io::Error::other(format!(
        "{}: too many levels of symbolic links",
        path.display()
    )))
}

/// Create a fresh temp file next to `name` in `dir`. The pid keeps two
/// processes (the Studio engine and a player's liborender) apart and the
/// counter two saves in one process; `create_new` makes sure no save ever
/// opens, and truncates, another's temp file.
fn create_temp_file(dir: &Path, name: &str) -> std::io::Result<(PathBuf, std::fs::File)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            // A leftover of a crashed process that had the same pid.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Whether renaming a file created by this process over `current` leaves it
/// the same file to everyone else: the same owner and group, and no other
/// hard link left pointing at the old contents.
#[cfg(unix)]
fn rename_keeps_identity(current: &std::fs::Metadata, created: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    current.nlink() == 1 && current.uid() == created.uid() && current.gid() == created.gid()
}

#[cfg(not(unix))]
fn rename_keeps_identity(_current: &std::fs::Metadata, _created: &std::fs::Metadata) -> bool {
    true
}

/// The non-atomic fallback of [`replace_file`]: truncate and rewrite `target`
/// itself, which keeps its inode, owner, links and attributes. The `.bak` is
/// best-effort here: a directory that refused a temp file may refuse it too,
/// and that must not block the write the old `fs::write` allowed.
fn write_in_place(
    target: &Path,
    backup: &Path,
    contents: &[u8],
    deliberate: bool,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    if deliberate {
        if let Err(e) = std::fs::copy(target, backup) {
            log::warn!("no backup of {} kept: {e}", target.display());
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(target)?;
    file.write_all(contents)?;
    file.sync_all()?;
    Ok(())
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

/// The `config_status` a host booting on `config_path` publishes: the file's
/// own [`Config::load_status`], unless the live state it restored from a
/// handoff sidecar was the parse-error fallback of the previous instance
/// ([`Config::live_from_parse_error`]). Call it after the sidecar was consumed
/// ([`Config::load_or_default_with_live`]).
pub fn boot_load_status(config_path: &Path) -> ConfigLoadStatus {
    let overlay = LIVE_OVERLAY.lock().unwrap().get(config_path).cloned();
    match overlay {
        Some(overlay) => live_load_status(config_path, &overlay, true),
        None => Config::load_status(config_path),
    }
}

/// The `config_status` for a live state `loaded` from
/// [`Config::load_or_default_with_live`] (`restored` as it returned): a
/// restored parse-error fallback stays `parse_error`, anything else gets the
/// file's own status. For a host adopting the state after boot (a standby
/// resume), which has the result in hand.
pub fn live_load_status(config_path: &Path, loaded: &Config, restored: bool) -> ConfigLoadStatus {
    if restored && loaded.live_from_parse_error {
        ConfigLoadStatus::ParseError
    } else {
        Config::load_status(config_path)
    }
}

/// Forget the sidecar consumed for `config_path`. Called after writing a *new*
/// sidecar (so a later boot-in-the-same-process re-reads it), after an
/// explicit save (a deliberate save supersedes the overlay) and on
/// reload_config (whose contract is "discard live state"). Overlays cached for
/// other config paths are left alone: they are not superseded by a write to
/// this one (and tests running in parallel on their own paths must not clobber
/// each other's).
pub fn clear_live_overlay_cache(config_path: &Path) {
    LIVE_OVERLAY.lock().unwrap().remove(config_path);
}

/// Apply a targeted config write to the live overlay for `config_path` — the
/// consumed one this process caches and a sidecar still waiting on disk — so
/// the pending unsaved state carries the written value instead of the stale
/// one it was taken with. Everything else in the overlay is left alone.
pub fn amend_live_overlay(config_path: &Path, amend: impl Fn(&mut Config)) {
    if let Some(cfg) = LIVE_OVERLAY.lock().unwrap().get_mut(config_path) {
        amend(cfg);
    }
    let sidecar = live_sidecar_path(config_path);
    if !sidecar.exists() {
        return;
    }
    match Config::load(&sidecar) {
        Ok(mut cfg) => {
            amend(&mut cfg);
            if let Err(e) = cfg.save_without_backup(&sidecar) {
                log::warn!("failed to amend {}: {e}", sidecar.display());
            }
        }
        // Unparsable: the next load deletes it without applying it anyway.
        Err(e) => log::warn!("not amending unreadable {}: {e}", sidecar.display()),
    }
}

/// Discard the live-handoff sidecar for `config_path`: remove the file AND
/// clear the consumed-overlay cache. The two must happen together — clearing
/// only one re-applies a superseded overlay on the next engine rebuild.
/// Called after any deliberate persistent write (explicit save, targeted
/// option persist, profile operation).
pub fn discard_live_sidecar(config_path: &Path) {
    let _ = std::fs::remove_file(live_sidecar_path(config_path));
    clear_live_overlay_cache(config_path);
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
    fn head_tracking_smoothing_and_invert_round_trip_and_omit_when_absent() {
        let ht = HeadTrackingConfig {
            smoothing: Some(0.6),
            invert: Some(true),
            ..Default::default()
        };
        let yaml = serde_yaml_ng::to_string(&ht).unwrap();
        let back: HeadTrackingConfig = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(back.smoothing, Some(0.6));
        assert_eq!(back.invert, Some(true));
        assert!(
            back.extra.is_empty(),
            "known keys leaked into extra: {yaml}"
        );

        let yaml = serde_yaml_ng::to_string(&HeadTrackingConfig::default()).unwrap();
        assert!(
            !yaml.contains("smoothing") && !yaml.contains("invert"),
            "None should omit the keys: {yaml}"
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
    fn retired_asio_live_input_backend_still_loads_as_the_default() {
        let yaml = "\
render:
  input_mode: pipewire
  live_input:
    backend: asio
    node: omniphony-in
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("a legacy asio backend must parse");
        let render = cfg.render.as_ref().unwrap();
        let live_input = render.live_input.as_ref().unwrap();
        assert_eq!(live_input.backend, None, "asio falls back to the default");
        // The rest of the section is untouched.
        assert_eq!(live_input.node.as_deref(), Some("omniphony-in"));
        assert_eq!(render.input_mode, Some(InputModeConfig::Pipewire));
        // The retired value is dropped, not carried back to disk as unknown.
        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(
            !out.contains("asio"),
            "retired backend re-serialized:\n{out}"
        );
    }

    #[test]
    fn live_input_backend_round_trips_and_rejects_unknown_values() {
        let cfg: Config =
            serde_yaml_ng::from_str("render:\n  live_input:\n    backend: pipewire\n")
                .expect("parse");
        let live_input = cfg.render.as_ref().unwrap().live_input.as_ref().unwrap();
        assert_eq!(live_input.backend, Some(InputBackendConfig::Pipewire));
        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(out.contains("backend: pipewire"), "{out}");

        let cfg: Config =
            serde_yaml_ng::from_str("render:\n  live_input:\n    node: x\n").expect("parse");
        let live_input = cfg.render.as_ref().unwrap().live_input.as_ref().unwrap();
        assert_eq!(live_input.backend, None, "an absent key stays absent");

        // An unknown one is a value a newer build knows: kept, not an error.
        let cfg: Config =
            serde_yaml_ng::from_str("render:\n  live_input:\n    backend: coreaudio\n")
                .expect("an unknown backend no longer fails the file");
        let live_input = cfg.render.as_ref().unwrap().live_input.as_ref().unwrap();
        assert_eq!(live_input.backend, None);
        assert_eq!(live_input.extra.get("backend").unwrap(), "coreaudio");
    }

    /// A value in every enum-typed key that no build knows, between keys
    /// this one does.
    pub(super) const VALUES_FROM_A_NEWER_BUILD: &str = "\
render:
  input_mode: carrier_pigeon
  bridge_path: /opt/bridge.so
  channel_render_mode: hologram
  surround_placement: ceiling
  output_channel_mapping: by_mood
  crossover_type: brickwall
  phantom_extract_mode: neural
  ramp_mode: warp
  size_to_spread_mode: volume
  master_gain: -3.5
  live_input:
    backend: jack
    node: omniphony-in
    clock_mode: ptp
    map: nine-one-fixed
    lfe_mode: bass_shaker
    channels: 8
  placement:
    generic:
      mode: hemisphere
";

    /// The value at `path` in a YAML document, as a string.
    pub(super) fn yaml_at(yaml: &str, path: &[&str]) -> Option<String> {
        let mut value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap();
        for key in path {
            value = value.get(key)?.clone();
        }
        Some(
            serde_yaml_ng::to_string(&value)
                .unwrap()
                .trim_end()
                .to_owned(),
        )
    }

    /// Every enum-typed key with its value in [`VALUES_FROM_A_NEWER_BUILD`].
    pub(super) const UNKNOWN_VALUES: &[(&[&str], &str)] = &[
        (&["input_mode"], "carrier_pigeon"),
        (&["channel_render_mode"], "hologram"),
        (&["surround_placement"], "ceiling"),
        (&["output_channel_mapping"], "by_mood"),
        (&["crossover_type"], "brickwall"),
        (&["phantom_extract_mode"], "neural"),
        (&["ramp_mode"], "warp"),
        (&["size_to_spread_mode"], "volume"),
        (&["live_input", "backend"], "jack"),
        (&["live_input", "clock_mode"], "ptp"),
        (&["live_input", "map"], "nine-one-fixed"),
        (&["live_input", "lfe_mode"], "bass_shaker"),
        (&["placement", "generic", "mode"], "hemisphere"),
    ];

    #[test]
    fn an_unknown_enum_value_falls_back_without_failing_the_file() {
        let cfg: Config = serde_yaml_ng::from_str(VALUES_FROM_A_NEWER_BUILD)
            .expect("values a newer build wrote must not fail the file");
        let render = cfg.render.as_ref().unwrap();
        // Every enum field is at its default...
        assert_eq!(render.input_mode, None);
        assert_eq!(render.channel_render_mode, None);
        assert_eq!(render.options.surround_placement, None);
        assert_eq!(render.options.output_channel_mapping, None);
        assert_eq!(render.options.crossover_type, None);
        assert_eq!(render.options.phantom_extract_mode, None);
        assert_eq!(render.size_to_spread_mode, None);
        let live_input = render.live_input.as_ref().unwrap();
        assert_eq!(live_input.backend, None);
        assert_eq!(live_input.clock_mode, None);
        assert_eq!(live_input.map, None);
        assert_eq!(live_input.lfe_mode, None);
        let generic = render.placement.as_ref().unwrap().get("generic").unwrap();
        assert_eq!(generic.mode, None);
        // ...every other key is read as usual...
        assert_eq!(render.bridge_path, Some(PathBuf::from("/opt/bridge.so")));
        assert_eq!(render.master_gain, Some(-3.5));
        assert_eq!(live_input.node.as_deref(), Some("omniphony-in"));
        assert_eq!(live_input.channels, Some(8));
        // ...and the values are kept under their own keys.
        assert_eq!(render.extra.get("crossover_type").unwrap(), "brickwall");
        assert_eq!(live_input.extra.get("clock_mode").unwrap(), "ptp");
        assert_eq!(generic.extra.get("mode").unwrap(), "hemisphere");
    }

    /// Only the enum fields read through a `Value`: the rest of the section
    /// keeps the YAML deserializer's own conversions, such as a plain number
    /// read into a string field.
    #[test]
    fn keeping_unknown_values_leaves_the_yaml_conversions_alone() {
        let yaml = "\
render:
  output_device: 0
  room_ratio: 1.50
  crossover_type: brickwall
  current_layout:
    speakers:
      - name: 1
        azimuth: 30
  live_input:
    node: 42
    clock_mode: ptp
profiles:
  other:
    osc_host: 127
";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("parse");
        let render = cfg.render.as_ref().unwrap();
        assert_eq!(render.output_device.as_deref(), Some("0"));
        assert_eq!(
            render.room_ratio.as_deref(),
            Some("1.50"),
            "the text as written"
        );
        let layout = render.current_layout.as_ref().unwrap();
        assert_eq!(layout.speakers[0].name, "1");
        let live_input = render.live_input.as_ref().unwrap();
        assert_eq!(live_input.node.as_deref(), Some("42"));
        assert_eq!(cfg.profiles["other"].osc_host.as_deref(), Some("127"));
        // And the unknown values next to them are still kept.
        assert_eq!(render.extra.get("crossover_type").unwrap(), "brickwall");
        assert_eq!(live_input.extra.get("clock_mode").unwrap(), "ptp");
    }

    #[test]
    fn a_known_enum_value_is_still_read() {
        let cfg: Config = serde_yaml_ng::from_str(
            "render:\n  crossover_type: fir\n  input_mode: live\n  live_input:\n    clock_mode: \
             pipewire\n",
        )
        .unwrap();
        let render = cfg.render.as_ref().unwrap();
        assert_eq!(
            render.options.crossover_type,
            Some(crate::live_params::CrossoverType::Fir)
        );
        assert_eq!(
            render.input_mode,
            Some(InputModeConfig::Pipewire),
            "an alias"
        );
        assert_eq!(
            render.live_input.as_ref().unwrap().clock_mode,
            Some(InputClockModeConfig::Pipewire)
        );
        assert!(render.extra.is_empty(), "nothing kept: {:?}", render.extra);
    }

    #[test]
    fn a_kept_enum_value_is_written_back_unchanged() {
        let cfg: Config = serde_yaml_ng::from_str(VALUES_FROM_A_NEWER_BUILD).unwrap();
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        for (path, value) in UNKNOWN_VALUES {
            let path = [&["render"], *path].concat();
            assert_eq!(
                yaml_at(&out, &path).as_deref(),
                Some(*value),
                "{path:?}:\n{out}"
            );
        }
        // Once each: the field it shadows is not written next to it.
        assert_eq!(out.matches("crossover_type").count(), 1, "{out}");
    }

    #[test]
    fn a_choice_of_this_build_replaces_a_kept_value_and_its_default_does_not() {
        use crate::live_params::CrossoverType;
        let mut cfg: Config = serde_yaml_ng::from_str(VALUES_FROM_A_NEWER_BUILD).unwrap();
        let render = cfg.render.as_mut().unwrap();
        // What a save writes for a setting nobody touched: the value the
        // unknown one fell back to. This build reads the kept value back the
        // same way, so the newer build's survives.
        render.options.crossover_type = Some(CrossoverType::Lr4);
        let live_input = render.live_input.as_mut().unwrap();
        live_input.clock_mode = Some(InputClockModeConfig::Dac);
        live_input.lfe_mode = Some(InputLfeModeConfig::Direct);
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(
            yaml_at(&out, &["render", "crossover_type"]).unwrap(),
            "brickwall"
        );
        assert_eq!(
            yaml_at(&out, &["render", "live_input", "clock_mode"]).unwrap(),
            "ptp"
        );
        assert_eq!(
            yaml_at(&out, &["render", "live_input", "lfe_mode"]).unwrap(),
            "bass_shaker"
        );

        // A value of its own, set in this build: it wins, once.
        let render = cfg.render.as_mut().unwrap();
        render.options.crossover_type = Some(CrossoverType::Fir);
        let live_input = render.live_input.as_mut().unwrap();
        live_input.clock_mode = Some(InputClockModeConfig::Upstream);
        live_input.backend = Some(InputBackendConfig::Pipewire);
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(yaml_at(&out, &["render", "crossover_type"]).unwrap(), "fir");
        assert_eq!(out.matches("crossover_type").count(), 1, "{out}");
        assert_eq!(
            yaml_at(&out, &["render", "live_input", "clock_mode"]).unwrap(),
            "upstream"
        );
        assert_eq!(
            yaml_at(&out, &["render", "live_input", "backend"]).unwrap(),
            "pipewire"
        );
        assert_eq!(out.matches("backend").count(), 1, "{out}");
    }

    #[test]
    fn the_clock_an_absent_key_stands_for_follows_the_input_mode() {
        let cfg: Config = serde_yaml_ng::from_str(
            "render:\n  input_mode: pipewire\n  live_input:\n    clock_mode: ptp\n",
        )
        .unwrap();
        let mut cfg = cfg;
        let live_input = cfg.render.as_mut().unwrap().live_input.as_mut().unwrap();
        // For the PipeWire sink, an absent clock is upstream: the DAC is a
        // choice of this build's, and replaces the kept value.
        live_input.clock_mode = Some(InputClockModeConfig::Dac);
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(
            yaml_at(&out, &["render", "live_input", "clock_mode"]).unwrap(),
            "dac"
        );
        let live_input = cfg.render.as_mut().unwrap().live_input.as_mut().unwrap();
        live_input.clock_mode = Some(InputClockModeConfig::Upstream);
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(
            yaml_at(&out, &["render", "live_input", "clock_mode"]).unwrap(),
            "ptp"
        );
    }

    #[test]
    fn a_profile_switch_carries_a_kept_input_mode() {
        let mut cfg: Config = serde_yaml_ng::from_str(
            "render:\n  input_mode: carrier_pigeon\nprofiles:\n  other:\n    master_gain: -1\n",
        )
        .unwrap();
        cfg.switch_profile("other").unwrap();
        let out = serde_yaml_ng::to_string(&cfg).unwrap();
        assert_eq!(
            yaml_at(&out, &["render", "input_mode"]).unwrap(),
            "carrier_pigeon"
        );
        assert_eq!(yaml_at(&out, &["render", "master_gain"]).unwrap(), "-1.0");
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

    /// Discarding one config's live state leaves the overlay consumed for
    /// another config path in place.
    #[test]
    fn discarding_one_overlay_keeps_the_others() {
        let dir = sidecar_test_dir("discard-scope");
        let kept = dir.join("kept.yaml");
        let discarded = dir.join("discarded.yaml");
        for path in [&kept, &discarded] {
            write_config_with_bridge(path, "/tmp/base.so");
            write_config_with_bridge(&live_sidecar_path(path), "/tmp/live.so");
            assert!(Config::load_or_default_with_live(path).1);
        }

        discard_live_sidecar(&discarded);
        assert!(!live_overlay_active(&discarded));
        assert!(live_overlay_active(&kept));
        let (cfg, restored) = Config::load_or_default_with_live(&kept);
        assert!(restored);
        assert_eq!(bridge_of(&cfg).as_deref(), Some("/tmp/live.so"));
        discard_live_sidecar(&kept);
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    fn render_with_backend(backend: &str) -> RenderConfig {
        RenderConfig {
            render_backend: Some(backend.to_string()),
            ..Default::default()
        }
    }

    fn profile_test_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omniphony-profile-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_flat_legacy_file_migrates_into_a_default_profile_on_save() {
        let dir = profile_test_dir("migrate");
        let path = dir.join("config.yaml");
        std::fs::write(&path, "render:\n  render_backend: barycenter\n").unwrap();

        let cfg = Config::load_or_default(&path);
        assert_eq!(cfg.active_profile_name(), DEFAULT_PROFILE);
        assert_eq!(cfg.profile_names(), vec![DEFAULT_PROFILE.to_string()]);

        cfg.save(&path).unwrap();
        let reloaded = Config::load_or_default(&path);
        assert_eq!(reloaded.active_profile.as_deref(), Some(DEFAULT_PROFILE));
        assert_eq!(
            reloaded.profiles[DEFAULT_PROFILE].render_backend.as_deref(),
            Some("barycenter")
        );
    }

    #[test]
    fn switch_mirrors_the_outgoing_profile_and_installs_the_target() {
        let mut cfg = Config {
            render: Some(RenderConfig {
                render_backend: Some("vbap".into()),
                input_pipe: Some(PathBuf::from("/tmp/pipe")),
                bridge_path: Some(PathBuf::from("/tmp/bridge.so")),
                ..Default::default()
            }),
            ..Default::default()
        };
        cfg.create_profile("headphones").unwrap();
        cfg.profiles.get_mut("headphones").unwrap().render_backend = Some("barycenter".into());
        // Divergent input plumbing in the stored profile must NOT win: the
        // machine's input side carries over from the outgoing render section.
        cfg.profiles.get_mut("headphones").unwrap().input_pipe =
            Some(PathBuf::from("/tmp/stale-pipe"));

        cfg.switch_profile("headphones").unwrap();

        assert_eq!(cfg.active_profile.as_deref(), Some("headphones"));
        let render = cfg.render.as_ref().unwrap();
        assert_eq!(render.render_backend.as_deref(), Some("barycenter"));
        assert_eq!(render.input_pipe.as_deref(), Some(Path::new("/tmp/pipe")));
        assert_eq!(
            render.bridge_path.as_deref(),
            Some(Path::new("/tmp/bridge.so"))
        );
        // The outgoing profile captured the previous render content.
        assert_eq!(
            cfg.profiles[DEFAULT_PROFILE].render_backend.as_deref(),
            Some("vbap")
        );
        // The mirror invariant holds for the new active profile too.
        assert_eq!(
            cfg.profiles["headphones"].render_backend.as_deref(),
            Some("barycenter")
        );
    }

    #[test]
    fn switch_to_unknown_or_active_profile_is_safe() {
        let mut cfg = Config {
            render: Some(render_with_backend("vbap")),
            ..Default::default()
        };
        assert!(cfg.switch_profile("nope").is_err());
        assert_eq!(cfg.active_profile_name(), DEFAULT_PROFILE);
        // Switching to the already-active name is a no-op, not an error.
        cfg.switch_profile(DEFAULT_PROFILE).unwrap();
        assert_eq!(
            cfg.render.as_ref().unwrap().render_backend.as_deref(),
            Some("vbap")
        );
    }

    #[test]
    fn create_delete_rename_enforce_name_rules() {
        let mut cfg = Config {
            render: Some(render_with_backend("vbap")),
            ..Default::default()
        };
        assert!(cfg.create_profile("  ").is_err());
        assert!(cfg.create_profile(DEFAULT_PROFILE).is_err());
        cfg.create_profile("desk").unwrap();
        assert!(cfg.create_profile("desk").is_err());

        assert!(cfg.delete_profile(DEFAULT_PROFILE).is_err());
        assert!(cfg.delete_profile("nope").is_err());

        assert!(cfg.rename_profile("desk", DEFAULT_PROFILE).is_err());
        cfg.rename_profile("desk", "couch").unwrap();
        assert!(cfg.profiles.contains_key("couch"));
        assert!(!cfg.profiles.contains_key("desk"));

        // Renaming the active profile follows `active_profile`.
        cfg.rename_profile(DEFAULT_PROFILE, "speakers").unwrap();
        assert_eq!(cfg.active_profile.as_deref(), Some("speakers"));

        cfg.delete_profile("couch").unwrap();
        assert_eq!(cfg.profile_names(), vec!["speakers".to_string()]);
    }

    #[test]
    fn unknown_keys_inside_a_profile_survive_the_round_trip() {
        let yaml = "render:\n  render_backend: vbap\nactive_profile: speakers\nprofiles:\n  speakers:\n    render_backend: vbap\n  headphones:\n    some_future_key: keep-me\n";
        let cfg: Config = serde_yaml_ng::from_str(yaml).expect("parse");
        assert_eq!(cfg.active_profile.as_deref(), Some("speakers"));
        assert!(
            cfg.profiles["headphones"]
                .extra
                .contains_key("some_future_key")
        );
        let out = serde_yaml_ng::to_string(&cfg).expect("serialize");
        assert!(out.contains("some_future_key: keep-me"));
    }
}

#[cfg(test)]
mod config_dir_override_tests {
    use crate::runtime_env::with_var;

    #[test]
    fn a_pinned_runtime_namespace_moves_the_config_out_of_the_shared_location() {
        let pinned = with_var(
            "OMNIPHONY_CONFIG_DIR",
            Some("/tmp/omniphony-wf-probe"),
            super::default_config_path,
        );
        let shared = with_var("OMNIPHONY_CONFIG_DIR", None, super::default_config_path);

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

#[cfg(test)]
mod save_tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("orender-config-save-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn with_layout(name: &str) -> Config {
        let mut config = Config::default();
        config
            .render
            .get_or_insert_with(Default::default)
            .output_file = Some(name.to_string());
        config
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_save_carries_the_schema_version() {
        let dir = dir("schema");
        let path = dir.join("config.yaml");
        with_layout("x").save(&path).unwrap();
        let yaml = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            super::tests::yaml_at(&yaml, &["schema_version"]),
            Some(CONFIG_SCHEMA_VERSION.to_string())
        );
        assert_eq!(Config::load_status(&path), ConfigLoadStatus::Loaded);
        // So does a save of a file from before the key.
        std::fs::write(&path, "render:\n  output_file: old\n").unwrap();
        let config = Config::load_for_update(&path).unwrap();
        assert_eq!(config.schema_version, None);
        config.save(&path).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().schema_version,
            Some(CONFIG_SCHEMA_VERSION)
        );
    }

    #[test]
    fn a_file_from_a_newer_schema_loads_but_is_refused_for_update() {
        let dir = dir("newer");
        let path = dir.join("config.yaml");
        let newer = format!(
            "schema_version: {}\nrender:\n  output_file: kept\n  a_key_from_the_future: 1\n",
            CONFIG_SCHEMA_VERSION + 1
        );
        std::fs::write(&path, &newer).unwrap();
        // Read as far as this build understands it...
        let (config, status) = Config::load_or_default_with_status(&path);
        assert_eq!(status, ConfigLoadStatus::NewerSchema);
        assert_eq!(status.as_str(), "newer_schema");
        assert_eq!(Config::load_status(&path), ConfigLoadStatus::NewerSchema);
        let render = config.render.as_ref().unwrap();
        assert_eq!(render.output_file.as_deref(), Some("kept"));
        assert!(config.is_from_newer_build());
        // ...but never written: every writer starts at `load_for_update`.
        let err = Config::load_for_update(&path).unwrap_err().to_string();
        assert!(err.contains("newer Omniphony"), "{err}");
        assert!(err.contains("left untouched"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
        // The version this build writes is not "newer".
        std::fs::write(
            &path,
            format!("schema_version: {CONFIG_SCHEMA_VERSION}\nrender: {{}}\n"),
        )
        .unwrap();
        assert_eq!(Config::load_status(&path), ConfigLoadStatus::Loaded);
        assert!(Config::load_for_update(&path).is_ok());
    }

    #[test]
    fn a_load_then_save_writes_unknown_enum_values_back_unchanged() {
        let dir = dir("unknown-enum");
        let path = dir.join("config.yaml");
        std::fs::write(&path, super::tests::VALUES_FROM_A_NEWER_BUILD).unwrap();
        let (config, status) = Config::load_or_default_with_status(&path);
        assert_eq!(status, ConfigLoadStatus::Loaded, "the file loads");
        Config::load_for_update(&path).unwrap().save(&path).unwrap();
        let yaml = std::fs::read_to_string(&path).unwrap();
        // The active profile's mirror carries them too.
        for section in [&["render"][..], &["profiles", DEFAULT_PROFILE]] {
            for (key, value) in super::tests::UNKNOWN_VALUES {
                let at = [section, *key].concat();
                assert_eq!(
                    super::tests::yaml_at(&yaml, &at).as_deref(),
                    Some(*value),
                    "{at:?}:\n{yaml}"
                );
            }
        }
        // And the keys this build knows are as they were.
        let saved = Config::load(&path).unwrap();
        let (before, after) = (config.render.unwrap(), saved.render.unwrap());
        assert_eq!(after.bridge_path, before.bridge_path);
        assert_eq!(after.master_gain, before.master_gain);
        let live_input = after.live_input.unwrap();
        assert_eq!(live_input.node.as_deref(), Some("omniphony-in"));
        assert_eq!(live_input.channels, Some(8));
    }

    #[test]
    fn a_file_that_fails_to_parse_is_refused_for_update() {
        let dir = dir("refuse");
        let path = dir.join("config.yaml");
        std::fs::write(&path, "render: [ not yaml").unwrap();
        let err = Config::load_for_update(&path).unwrap_err().to_string();
        assert!(err.contains("left untouched"), "{err}");
        // A missing file is a fresh start, not an error.
        assert!(Config::load_for_update(&dir.join("absent.yaml")).is_ok());
    }

    /// The file a write creates records OSC on, the state the embedded engine
    /// runs in without a file; a file that exists keeps what it says,
    /// including saying nothing.
    #[test]
    fn a_write_that_creates_the_file_records_osc_on() {
        let dir = dir("new-file-osc");
        let path = dir.join("config.yaml");
        let fresh = Config::load_for_update(&path).unwrap();
        assert_eq!(fresh.render.as_ref().and_then(|r| r.osc), Some(true));
        fresh.save(&path).unwrap();
        let saved = Config::load(&path).unwrap();
        assert_eq!(saved.render.as_ref().and_then(|r| r.osc), Some(true));

        for existing in ["render:\n  output_file: kept\n", "render:\n  osc: false\n"] {
            std::fs::write(&path, existing).unwrap();
            let config = Config::load_for_update(&path).unwrap();
            let osc = config.render.as_ref().and_then(|r| r.osc);
            assert_ne!(osc, Some(true), "{existing}");
        }
        // Reading a missing file is still the plain defaults.
        assert!(
            Config::load_or_default(&dir.join("absent.yaml"))
                .render
                .is_none()
        );
    }

    #[test]
    fn save_keeps_the_previous_file_as_bak() {
        let dir = dir("bak");
        let path = dir.join("config.yaml");
        with_layout("first").save(&path).unwrap();
        assert!(!backup_path(&path).exists(), "nothing to back up yet");
        let first = std::fs::read(&path).unwrap();
        with_layout("second").save(&path).unwrap();
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), first);
        assert_eq!(entries(&dir), ["config.yaml", "config.yaml.bak"]);
    }

    #[test]
    fn save_without_backup_leaves_no_bak() {
        let dir = dir("no-bak");
        let path = dir.join("config.live.yaml");
        with_layout("first").save_without_backup(&path).unwrap();
        with_layout("second").save_without_backup(&path).unwrap();
        assert_eq!(entries(&dir), ["config.live.yaml"]);
    }

    #[test]
    fn a_failure_before_the_rename_leaves_the_old_file_intact() {
        let dir = dir("inject");
        let path = dir.join("config.yaml");
        with_layout("old").save(&path).unwrap();
        let old = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(backup_path(&path));

        let yaml = with_layout("new").to_yaml().unwrap();
        let result = replace_file(&path, yaml.as_bytes(), true, || {
            Err(std::io::Error::other("injected"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), old);
        assert_eq!(entries(&dir), ["config.yaml"], "temp file cleaned up");
    }

    /// Saves of one file from several threads of one process: none fails,
    /// and no temp file is left behind.
    #[test]
    fn concurrent_saves_in_one_process_do_not_share_a_temp_file() {
        let dir = dir("concurrent");
        let path = dir.join("config.yaml");
        with_layout("start").save(&path).unwrap();
        std::thread::scope(|scope| {
            for t in 0..4 {
                let path = &path;
                scope.spawn(move || {
                    for i in 0..25 {
                        with_layout(&format!("t{t}-{i}")).save(path).unwrap();
                    }
                });
            }
        });
        assert!(Config::load(&path).is_ok());
        assert_eq!(entries(&dir), ["config.yaml", "config.yaml.bak"]);
        // Both files are, byte for byte, one of the configs saved: a torn
        // write can still parse, so loading alone would not show one. The
        // `.bak` is the file as the last save found it, so it is a different
        // one of them.
        let saved: Vec<Vec<u8>> = std::iter::once("start".to_string())
            .chain((0..4).flat_map(|t| (0..25).map(move |i| format!("t{t}-{i}"))))
            .map(|name| with_layout(&name).to_yaml().unwrap().into_bytes())
            .collect();
        let current = std::fs::read(&path).unwrap();
        let backup = std::fs::read(backup_path(&path)).unwrap();
        assert!(
            saved.contains(&current),
            "config.yaml is not a saved config"
        );
        assert!(
            saved.contains(&backup),
            "config.yaml.bak is not a saved config"
        );
        assert_ne!(backup, current);
    }

    /// A config whose directory is not writable (only the file is) is still
    /// saved, in place, as `fs::write` used to.
    #[cfg(unix)]
    #[test]
    fn save_rewrites_in_place_when_the_directory_is_not_writable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = dir("ro-dir");
        let path = dir.join("config.yaml");
        with_layout("older").save(&path).unwrap();
        // The `.bak` already exists, so it can be rewritten in place too.
        with_layout("old").save(&path).unwrap();
        let old = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root ignores the directory mode; there is nothing to check then.
        let enforced = std::fs::File::create(dir.join("probe")).is_err();
        let result = with_layout("new").save(&path);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !enforced {
            return;
        }
        result.unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.render.unwrap().output_file.as_deref(), Some("new"));
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), old);
        assert_eq!(entries(&dir), ["config.yaml", "config.yaml.bak"]);
    }

    /// A read-only config stays read-only: the save is refused, as the old
    /// `fs::write` was, although the directory would allow the rename.
    #[cfg(unix)]
    #[test]
    fn save_refuses_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = dir("ro-file");
        let path = dir.join("config.yaml");
        with_layout("old").save(&path).unwrap();
        let old = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        // Root ignores the file mode; there is nothing to check then.
        if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
            return;
        }
        assert!(with_layout("new").save(&path).is_err());
        assert!(with_layout("new").save_without_backup(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), old);
        assert_eq!(entries(&dir), ["config.yaml"]);
    }

    /// A link to a config not created yet is written through, creating it.
    #[cfg(unix)]
    #[test]
    fn save_creates_the_missing_target_of_a_symlink() {
        let dir = dir("dangling");
        let link = dir.join("config.yaml");
        std::os::unix::fs::symlink("absent.yaml", &link).unwrap();
        assert!(Config::load_for_update(&link).is_ok(), "a missing config");
        with_layout("new").save(&link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let back = Config::load(&dir.join("absent.yaml")).unwrap();
        assert_eq!(back.render.unwrap().output_file.as_deref(), Some("new"));
    }

    /// A hard-linked config stays one file under both names.
    #[cfg(unix)]
    #[test]
    fn save_keeps_hard_links() {
        let dir = dir("hardlink");
        let path = dir.join("config.yaml");
        let other = dir.join("other.yaml");
        with_layout("old").save(&path).unwrap();
        std::fs::hard_link(&path, &other).unwrap();
        with_layout("new").save(&path).unwrap();
        let back = Config::load(&other).unwrap();
        assert_eq!(back.render.unwrap().output_file.as_deref(), Some("new"));
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_through_a_symlink() {
        let dir = dir("symlink");
        let real = dir.join("real.yaml");
        let link = dir.join("config.yaml");
        with_layout("old").save_without_backup(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        with_layout("new").save(&link).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let back = Config::load(&real).unwrap();
        assert_eq!(back.render.unwrap().output_file.as_deref(), Some("new"));
        assert_eq!(
            entries(&dir),
            ["config.yaml", "config.yaml.bak", "real.yaml"],
            "the .bak sits next to the link"
        );
    }
}
