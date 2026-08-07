//! The Omniphony OSC control / state contract, as a single source of truth.
//!
//! The engine is driven and observed entirely over OSC: clients send
//! `/omniphony/control/…` messages and receive `/omniphony/state/…` updates.
//! Those address strings are the wire contract between the engine and every
//! client (Omniphony Studio, alternative front-ends, automation). Historically
//! they were bare string literals scattered across the dispatcher and the
//! producers; this module names them once so the Rust side references symbols
//! instead of magic strings, and so the surface is discoverable in one place.
//!
//! The human-readable companion — direction, argument types and semantics for
//! every address — lives in `docs/osc-control-contract.md`. Keep the two in
//! sync when adding or changing an address.
//!
//! ## Address families with a dynamic or prefixed tail
//!
//! A few control addresses are not fixed strings:
//!
//! * **Per-object mute** — [`CONTROL_OBJECT_PREFIX`] + `"{id}/mute"`, e.g.
//!   `/omniphony/control/object/3/mute`.
//! * **Hybrid backend** — `/omniphony/control/hybrid/{external_backend,
//!   internal_backend,metric,curve,curve_smoothing}`.
//! * **Distance diffuse** — `/omniphony/control/distance_diffuse/{enabled,
//!   threshold,curve,metric,mirror_axes}`. `mirror_axes` takes the axes to
//!   negate as a string (`xy` — the default half-turn about the vertical axis —
//!   `y`, `xyz`, `none`, …).
//! * **Render-evaluation tables** —
//!   `/omniphony/control/render_evaluation/cartesian/{x_size,y_size,z_size,
//!   z_neg_size}` and `/omniphony/control/render_evaluation/polar/{azimuth_
//!   resolution,elevation_resolution,distance_res,distance_max}`.
//!
//! See `docs/osc-control-contract.md` for the full list of those.

// ── Control: client → engine ────────────────────────────────────────────────

pub const CONTROL_ADAPTIVE_RESAMPLING: &str = "/omniphony/control/adaptive_resampling";
pub const CONTROL_ADAPTIVE_RESAMPLING_ENABLE_FAR_MODE: &str =
    "/omniphony/control/adaptive_resampling/enable_far_mode";
pub const CONTROL_ADAPTIVE_RESAMPLING_FAR_MODE_RETURN_FADE_IN_MS: &str =
    "/omniphony/control/adaptive_resampling/far_mode_return_fade_in_ms";
pub const CONTROL_ADAPTIVE_RESAMPLING_FORCE_SILENCE_IN_FAR_MODE: &str =
    "/omniphony/control/adaptive_resampling/force_silence_in_far_mode";
pub const CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_HIGH_IN_FAR_MODE: &str =
    "/omniphony/control/adaptive_resampling/hard_recover_high_in_far_mode";
pub const CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_IN_FAR_MODE: &str =
    "/omniphony/control/adaptive_resampling/hard_recover_in_far_mode";
pub const CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_LOW_IN_FAR_MODE: &str =
    "/omniphony/control/adaptive_resampling/hard_recover_low_in_far_mode";
pub const CONTROL_ADAPTIVE_RESAMPLING_HIGH_RECOVER_ENTRY_MARGIN_MS: &str =
    "/omniphony/control/adaptive_resampling/high_recover_entry_margin_ms";
pub const CONTROL_ADAPTIVE_RESAMPLING_INTEGRAL_DISCHARGE_RATIO: &str =
    "/omniphony/control/adaptive_resampling/integral_discharge_ratio";
pub const CONTROL_ADAPTIVE_RESAMPLING_KI: &str = "/omniphony/control/adaptive_resampling/ki";
pub const CONTROL_ADAPTIVE_RESAMPLING_KP_NEAR: &str =
    "/omniphony/control/adaptive_resampling/kp_near";
pub const CONTROL_ADAPTIVE_RESAMPLING_MAX_ADJUST: &str =
    "/omniphony/control/adaptive_resampling/max_adjust";
pub const CONTROL_ADAPTIVE_RESAMPLING_NEAR_FAR_THRESHOLD_MS: &str =
    "/omniphony/control/adaptive_resampling/near_far_threshold_ms";
pub const CONTROL_ADAPTIVE_RESAMPLING_PAUSE: &str = "/omniphony/control/adaptive_resampling/pause";
pub const CONTROL_ADAPTIVE_RESAMPLING_RESET_RATIO: &str =
    "/omniphony/control/adaptive_resampling/reset_ratio";
pub const CONTROL_ADAPTIVE_RESAMPLING_UPDATE_INTERVAL_CALLBACKS: &str =
    "/omniphony/control/adaptive_resampling/update_interval_callbacks";
pub const CONTROL_AUDIO_OUTPUT_BACKEND: &str = "/omniphony/control/audio/output_backend";
pub const CONTROL_AUDIO_OUTPUT_DEVICE: &str = "/omniphony/control/audio/output_device";
pub const CONTROL_AUDIO_OUTPUT_DEVICES_REFRESH: &str =
    "/omniphony/control/audio/output_devices/refresh";
pub const CONTROL_AUDIO_OUTPUT_FILE: &str = "/omniphony/control/audio/output_file";
pub const CONTROL_AUDIO_OUTPUT_FILE_FORMAT: &str = "/omniphony/control/audio/output_file_format";
pub const CONTROL_AUDIO_SAMPLE_RATE: &str = "/omniphony/control/audio/sample_rate";
pub const CONTROL_AUTO_GAIN: &str = "/omniphony/control/auto_gain";
pub const CONTROL_AUTO_GAIN_CEILING: &str = "/omniphony/control/auto_gain_ceiling";
// Editable backend file (e.g. the scriptable backend's `.lua`). Content is owned
// by the renderer, so the editor reads/writes it over OSC rather than via a
// cross-host path. `get [backend_id, key]` replies STATE_BACKEND_FILE_CONTENT;
// `list [backend_id]` replies STATE_BACKEND_FILE_LIST; `put [backend_id, key,
// name, content]` writes the renderer-managed store and rebuilds. The whole file
// rides in a single message (small text); an absolute handle is only honoured for
// a loopback caller.
pub const CONTROL_BACKEND_FILE_GET: &str = "/omniphony/control/backend/file/get";
pub const CONTROL_BACKEND_FILE_LIST: &str = "/omniphony/control/backend/file/list";
pub const CONTROL_BACKEND_FILE_PUT: &str = "/omniphony/control/backend/file/put";
pub const CONTROL_BACKEND_PARAM: &str = "/omniphony/control/backend/param";
pub const CONTROL_CONFIG_AUDIO: &str = "/omniphony/control/config/audio";
pub const CONTROL_CONFIG_AUDIO_APPLY: &str = "/omniphony/control/config/audio/apply";
pub const CONTROL_CONFIG_INPUT: &str = "/omniphony/control/config/input";
pub const CONTROL_CONFIG_INPUT_APPLY: &str = "/omniphony/control/config/input/apply";
pub const CONTROL_CONFIG_LAYOUT: &str = "/omniphony/control/config/layout";
pub const CONTROL_CONFIG_LAYOUT_APPLY: &str = "/omniphony/control/config/layout/apply";
pub const CONTROL_CONFIG_SPEAKERS: &str = "/omniphony/control/config/speakers";
pub const CONTROL_DEBUG_SPEAKER_GAINTABLE_NACK: &str =
    "/omniphony/control/debug/speaker_gaintable/nack";
pub const CONTROL_DEBUG_SPEAKER_GAINTABLE_SUBSCRIBE: &str =
    "/omniphony/control/debug/speaker_gaintable/subscribe";
pub const CONTROL_DEBUG_SPEAKER_GAINTABLE_UNSUBSCRIBE: &str =
    "/omniphony/control/debug/speaker_gaintable/unsubscribe";
pub const CONTROL_DIAG_ENABLED: &str = "/omniphony/control/diag/enabled";
pub const CONTROL_DIAG_RATE_HZ: &str = "/omniphony/control/diag/rate_hz";
pub const CONTROL_DISTANCE_MODEL: &str = "/omniphony/control/distance_model";
pub const CONTROL_DISTANCE_MODEL_METRIC: &str = "/omniphony/control/distance_model_metric";
pub const CONTROL_GAIN: &str = "/omniphony/control/gain";
pub const CONTROL_INPUT_APPLY: &str = "/omniphony/control/input/apply";
pub const CONTROL_INPUT_DRC_MODE: &str = "/omniphony/control/input/drc_mode";
pub const CONTROL_INPUT_DRC_WEIGHT: &str = "/omniphony/control/input/drc_weight";
pub const CONTROL_INPUT_LIVE_BACKEND: &str = "/omniphony/control/input/live/backend";
pub const CONTROL_INPUT_LIVE_CHANNELS: &str = "/omniphony/control/input/live/channels";
pub const CONTROL_INPUT_LIVE_CLOCK_MODE: &str = "/omniphony/control/input/live/clock_mode";
pub const CONTROL_INPUT_LIVE_DESCRIPTION: &str = "/omniphony/control/input/live/description";
pub const CONTROL_INPUT_LIVE_FORMAT: &str = "/omniphony/control/input/live/format";
pub const CONTROL_INPUT_LIVE_LAYOUT: &str = "/omniphony/control/input/live/layout";
pub const CONTROL_INPUT_LIVE_LAYOUT_IMPORT: &str = "/omniphony/control/input/live/layout_import";
pub const CONTROL_INPUT_LIVE_LFE_MODE: &str = "/omniphony/control/input/live/lfe_mode";
pub const CONTROL_INPUT_LIVE_MAP: &str = "/omniphony/control/input/live/map";
pub const CONTROL_INPUT_LIVE_NODE: &str = "/omniphony/control/input/live/node";
pub const CONTROL_INPUT_LIVE_SAMPLE_RATE: &str = "/omniphony/control/input/live/sample_rate";
pub const CONTROL_INPUT_MODE: &str = "/omniphony/control/input/mode";
pub const CONTROL_INPUT_REFRESH: &str = "/omniphony/control/input/refresh";
pub const CONTROL_LAYOUT_EXPORT: &str = "/omniphony/control/layout/export";
pub const CONTROL_LAYOUT_RADIUS_M: &str = "/omniphony/control/layout/radius_m";
pub const CONTROL_LOG_LEVEL: &str = "/omniphony/control/log_level";
pub const CONTROL_LOUDNESS: &str = "/omniphony/control/loudness";
pub const CONTROL_METERING: &str = "/omniphony/control/metering";
pub const CONTROL_METERING_RATE_HZ: &str = "/omniphony/control/metering/rate_hz";
pub const CONTROL_OVERLAY_ENABLED: &str = "/omniphony/control/overlay/enabled";
pub const CONTROL_OVERLAY_HEATMAP_BANDS: &str = "/omniphony/control/overlay/heatmap_bands";
pub const CONTROL_OVERLAY_HEATMAP_COLORMAP: &str = "/omniphony/control/overlay/heatmap_colormap";
pub const CONTROL_OVERLAY_HEATMAP_CUSTOM_STOPS: &str =
    "/omniphony/control/overlay/heatmap_custom_stops";
pub const CONTROL_OVERLAY_HEATMAP_ENABLED: &str = "/omniphony/control/overlay/heatmap_enabled";
/// Enables/disables every renderer-synthesized-object stage without clearing
/// the configured phantom/height selections.
pub const CONTROL_SYNTHETIC_OBJECTS: &str = "/omniphony/control/synthetic_objects";
/// Selects the bed→height object generator (2D upmix) for channel content.
/// Argument is the generator id string (`none` / `copy_up` / …).
pub const CONTROL_OBJECT_GENERATOR: &str = "/omniphony/control/object_generator";
/// Sets a live object-generator parameter. Args: key (string), value (float).
/// Keys: `strength`, `hpf_hz`, `gain_db` (PAD).
pub const CONTROL_OBJECT_GENERATOR_PARAM: &str = "/omniphony/control/object_generator/param";
/// Selects phantom extraction: `off`, `broadband`, or `spectral`. The old int
/// 0/1 spelling remains accepted as off/broadband on this legacy address.
pub const CONTROL_PHANTOM_EXTRACT: &str = "/omniphony/control/phantom_extract";
/// Sets a live phantom-extraction parameter. Args: key (string), value (float).
/// Keys: `strength`, `passes`, `lift`.
pub const CONTROL_PHANTOM_EXTRACT_PARAM: &str = "/omniphony/control/phantom_extract/param";
/// Where the 4.x/5.x surround pair is placed: `side` or `back`. Only affects
/// channel sources without dedicated back channels. Persisted to config.
pub const CONTROL_SURROUND_PLACEMENT: &str = "/omniphony/control/surround_placement";
/// How output channels map to device ports: `by_index` (positionless — port N =
/// layout speaker N) or `by_name` (positional). Persisted to config.
pub const CONTROL_OUTPUT_CHANNEL_MAPPING: &str = "/omniphony/control/output_channel_mapping";
/// Set the parametrable virtual bed for channel content. Argument is a YAML
/// `SpeakerLayout` (one entry per channel label, `spatialize` = virtual/direct);
/// an empty string resets to the built-in canonical poses (LFE direct).
pub const CONTROL_VIRTUAL_BED: &str = "/omniphony/control/virtual_bed";
/// Generic setter for any declared live option (`renderer::options`):
/// args `[key (string), value]`. The per-option addresses listed above
/// (`synthetic_objects`, `object_generator`, `phantom_extract`,
/// `surround_placement`, `output_channel_mapping`) are legacy aliases of this.
pub const CONTROL_OPTION: &str = "/omniphony/control/option";
/// Overlay display preferences as JSON, republished whenever they change —
/// including when an mpv keybind flips one through the FFI toggles. The overlay
/// is a process-global singleton with two writers, so a client must read this
/// rather than trust its own mirror.
pub const STATE_OVERLAY: &str = "/omniphony/state/overlay";
pub const CONTROL_OVERLAY_LABELS: &str = "/omniphony/control/overlay/labels";
pub const CONTROL_OVERLAY_OBJECTS: &str = "/omniphony/control/overlay/objects";
pub const CONTROL_OVERLAY_TAG: &str = "/omniphony/control/overlay/tag";
pub const CONTROL_OVERLAY_TRAILS: &str = "/omniphony/control/overlay/trails";
pub const CONTROL_QUIT: &str = "/omniphony/control/quit";
pub const CONTROL_RAMP_MODE: &str = "/omniphony/control/ramp_mode";
pub const CONTROL_REALTIME_MASTER_GAIN: &str = "/omniphony/control/realtime/master_gain";
pub const CONTROL_REALTIME_SPEAKER_GAIN: &str = "/omniphony/control/realtime/speaker_gain";
pub const CONTROL_RELOAD_CONFIG: &str = "/omniphony/control/reload_config";
pub const CONTROL_RENDER_BACKEND: &str = "/omniphony/control/render_backend";
pub const CONTROL_RENDER_BACKEND_RESTORE: &str = "/omniphony/control/render_backend/restore";
pub const CONTROL_RENDER_BRIDGE_PATH: &str = "/omniphony/control/render/bridge_path";
pub const CONTROL_RENDER_EVALUATION_MODE: &str = "/omniphony/control/render_evaluation_mode";
pub const CONTROL_RENDER_EVALUATION_MODE_FROM_FILE: &str =
    "/omniphony/control/render_evaluation_mode/from_file";
pub const CONTROL_RENDER_EVALUATION_POSITION_INTERPOLATION: &str =
    "/omniphony/control/render_evaluation/position_interpolation";
pub const CONTROL_RENDER_INPUT_PIPE: &str = "/omniphony/control/render/input_pipe";
pub const CONTROL_ROOM_RATIO: &str = "/omniphony/control/room_ratio";
pub const CONTROL_ROOM_RATIO_CENTER_BLEND: &str = "/omniphony/control/room_ratio_center_blend";
pub const CONTROL_ROOM_RATIO_LOWER: &str = "/omniphony/control/room_ratio_lower";
pub const CONTROL_ROOM_RATIO_REAR: &str = "/omniphony/control/room_ratio_rear";
pub const CONTROL_SAVE_CONFIG: &str = "/omniphony/control/save_config";
pub const CONTROL_SPREAD_DISTANCE_CURVE: &str = "/omniphony/control/spread/distance_curve";
pub const CONTROL_SPREAD_DISTANCE_RANGE: &str = "/omniphony/control/spread/distance_range";
pub const CONTROL_SPREAD_FROM_DISTANCE: &str = "/omniphony/control/spread/from_distance";
pub const CONTROL_SPREAD_MAX: &str = "/omniphony/control/spread/max";
pub const CONTROL_SPREAD_MIN: &str = "/omniphony/control/spread/min";
pub const CONTROL_SPREAD_SIZE_TO_SPREAD_MODE: &str =
    "/omniphony/control/spread/size_to_spread_mode";
pub const CONTROL_YIELD_PORT: &str = "/omniphony/control/yield_port";
/// Sent to a standing-by instance (on the dynamic resume port it advertised in
/// reply to a yield) to ask it to re-acquire the OSC port + audio and resume.
pub const CONTROL_RESUME: &str = "/omniphony/control/resume";

/// Prefix for the per-object mute address. Append `"{id}/mute"`.
pub const CONTROL_OBJECT_PREFIX: &str = "/omniphony/control/object/";

// ── State: engine → clients ─────────────────────────────────────────────────

pub const STATE_ADAPTIVE_RESAMPLING_BAND: &str = "/omniphony/state/adaptive_resampling/band";
pub const STATE_ADAPTIVE_RESAMPLING_STATE: &str = "/omniphony/state/adaptive_resampling/state";
pub const STATE_AUDIO: &str = "/omniphony/state/audio";
pub const STATE_BACKEND_FILE_CONTENT: &str = "/omniphony/state/backend/file/content";
pub const STATE_BACKEND_FILE_ERROR: &str = "/omniphony/state/backend/file/error";
pub const STATE_BACKEND_FILE_LIST: &str = "/omniphony/state/backend/file/list";
pub const STATE_CAPABILITIES: &str = "/omniphony/state/capabilities";
pub const STATE_CLIP: &str = "/omniphony/state/clip";
pub const STATE_CONFIG_SAVED: &str = "/omniphony/state/config/saved";
pub const STATE_CONFIG_SAVE_ERROR: &str = "/omniphony/state/config/save_error";
pub const STATE_CROSSOVER_TIME_MS: &str = "/omniphony/state/crossover_time_ms";
pub const STATE_DEBUG_SPEAKER_GAINTABLE_CHUNK: &str =
    "/omniphony/state/debug/speaker_gaintable/chunk";
pub const STATE_DEBUG_SPEAKER_GAINTABLE_META: &str =
    "/omniphony/state/debug/speaker_gaintable/meta";
pub const STATE_DEBUG_SPEAKER_GAINTABLE_UNAVAILABLE: &str =
    "/omniphony/state/debug/speaker_gaintable/unavailable";
pub const STATE_DEBUG_SPEAKER_GAINTABLE_UPTODATE: &str =
    "/omniphony/state/debug/speaker_gaintable/uptodate";
pub const STATE_DECODE_TIME_MS: &str = "/omniphony/state/decode_time_ms";
pub const STATE_DIAG_SCHEMA: &str = "/omniphony/state/diag_schema";
pub const STATE_DIAG_VALUES: &str = "/omniphony/state/diag_values";
pub const STATE_FRAME_DURATION_MS: &str = "/omniphony/state/frame_duration_ms";
pub const STATE_INPUT: &str = "/omniphony/state/input";
pub const STATE_INPUT_PIPE: &str = "/omniphony/state/input_pipe";
pub const STATE_LATENCY: &str = "/omniphony/state/latency";
pub const STATE_LATENCY_AVAIL_INPUT: &str = "/omniphony/state/latency_avail_input";
pub const STATE_LATENCY_CONTROL: &str = "/omniphony/state/latency_control";
pub const STATE_LATENCY_DOWNSTREAM: &str = "/omniphony/state/latency_downstream";
pub const STATE_LATENCY_INSTANT: &str = "/omniphony/state/latency_instant";
pub const STATE_LATENCY_OUTPUT_FIFO: &str = "/omniphony/state/latency_output_fifo";
pub const STATE_LATENCY_RESAMPLER_PENDING: &str = "/omniphony/state/latency_resampler_pending";
pub const STATE_LATENCY_SMOOTHED: &str = "/omniphony/state/latency_smoothed";
pub const STATE_LAYOUT: &str = "/omniphony/state/layout";
pub const STATE_LOG_LEVEL: &str = "/omniphony/state/log_level";
pub const STATE_LOUDNESS: &str = "/omniphony/state/loudness";
pub const STATE_MONITORING: &str = "/omniphony/state/monitoring";
/// Schema of the declared live options (`renderer::options` registry rows),
/// as a JSON string. Same pattern as `/state/object_generators` / `/state/phantom`.
pub const STATE_OPTIONS_SCHEMA: &str = "/omniphony/state/options_schema";
pub const STATE_OSC_DIAG: &str = "/omniphony/state/osc/diag";
pub const STATE_OSC_METERING: &str = "/omniphony/state/osc/metering";
pub const STATE_REALTIME_MASTER_GAIN: &str = "/omniphony/state/realtime/master_gain";
pub const STATE_REALTIME_SPEAKER_GAIN: &str = "/omniphony/state/realtime/speaker_gain";
pub const STATE_RENDER_ABI: &str = "/omniphony/state/render/abi";
pub const STATE_RENDER_BRIDGE_ERROR: &str = "/omniphony/state/render/bridge_error";
pub const STATE_RENDER_BRIDGE_PATH: &str = "/omniphony/state/render/bridge_path";
pub const STATE_RENDER_CONFIG_PATH: &str = "/omniphony/state/render/config_path";
pub const STATE_RENDER_CONFIG_STATUS: &str = "/omniphony/state/render/config_status";
pub const STATE_RENDERER: &str = "/omniphony/state/renderer";
pub const STATE_RENDER_EVALUATION_CARTESIAN_X_SIZE: &str =
    "/omniphony/state/render_evaluation/cartesian/x_size";
pub const STATE_RENDER_EVALUATION_CARTESIAN_Y_SIZE: &str =
    "/omniphony/state/render_evaluation/cartesian/y_size";
pub const STATE_RENDER_EVALUATION_CARTESIAN_Z_NEG_SIZE: &str =
    "/omniphony/state/render_evaluation/cartesian/z_neg_size";
pub const STATE_RENDER_EVALUATION_CARTESIAN_Z_SIZE: &str =
    "/omniphony/state/render_evaluation/cartesian/z_size";
pub const STATE_RENDER_EVALUATION_POLAR_AZIMUTH_RESOLUTION: &str =
    "/omniphony/state/render_evaluation/polar/azimuth_resolution";
pub const STATE_RENDER_EVALUATION_POLAR_DISTANCE_MAX: &str =
    "/omniphony/state/render_evaluation/polar/distance_max";
pub const STATE_RENDER_EVALUATION_POLAR_DISTANCE_RES: &str =
    "/omniphony/state/render_evaluation/polar/distance_res";
pub const STATE_RENDER_EVALUATION_POLAR_ELEVATION_RESOLUTION: &str =
    "/omniphony/state/render_evaluation/polar/elevation_resolution";
pub const STATE_RENDER_EVALUATION_POSITION_INTERPOLATION: &str =
    "/omniphony/state/render_evaluation/position_interpolation";
pub const STATE_RENDER_TIME_MS: &str = "/omniphony/state/render_time_ms";
pub const STATE_RENDER_VERSION: &str = "/omniphony/state/render/version";
pub const STATE_RESAMPLE_RATIO: &str = "/omniphony/state/resample_ratio";
pub const STATE_SHUTDOWN: &str = "/omniphony/state/shutdown";
pub const STATE_SNAPSHOT_COMPLETE: &str = "/omniphony/state/snapshot_complete";
pub const STATE_SPEAKERS: &str = "/omniphony/state/speakers";
pub const STATE_SPEAKERS_RECOMPUTE_ERROR: &str = "/omniphony/state/speakers/recompute_error";
pub const STATE_SPEAKERS_RECOMPUTING: &str = "/omniphony/state/speakers/recomputing";
pub const STATE_VBAP_ALLOW_NEGATIVE_Z: &str = "/omniphony/state/vbap/allow_negative_z";
pub const STATE_WRITE_TIME_MS: &str = "/omniphony/state/write_time_ms";

// ── Address catalogues (handy for clients / tests) ──────────────────────────

pub const ALL_CONTROL: &[&str] = &[
    CONTROL_ADAPTIVE_RESAMPLING,
    CONTROL_ADAPTIVE_RESAMPLING_ENABLE_FAR_MODE,
    CONTROL_ADAPTIVE_RESAMPLING_FAR_MODE_RETURN_FADE_IN_MS,
    CONTROL_ADAPTIVE_RESAMPLING_FORCE_SILENCE_IN_FAR_MODE,
    CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_HIGH_IN_FAR_MODE,
    CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_IN_FAR_MODE,
    CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_LOW_IN_FAR_MODE,
    CONTROL_ADAPTIVE_RESAMPLING_HIGH_RECOVER_ENTRY_MARGIN_MS,
    CONTROL_ADAPTIVE_RESAMPLING_INTEGRAL_DISCHARGE_RATIO,
    CONTROL_ADAPTIVE_RESAMPLING_KI,
    CONTROL_ADAPTIVE_RESAMPLING_KP_NEAR,
    CONTROL_ADAPTIVE_RESAMPLING_MAX_ADJUST,
    CONTROL_ADAPTIVE_RESAMPLING_NEAR_FAR_THRESHOLD_MS,
    CONTROL_ADAPTIVE_RESAMPLING_PAUSE,
    CONTROL_ADAPTIVE_RESAMPLING_RESET_RATIO,
    CONTROL_ADAPTIVE_RESAMPLING_UPDATE_INTERVAL_CALLBACKS,
    CONTROL_AUDIO_OUTPUT_BACKEND,
    CONTROL_AUDIO_OUTPUT_DEVICE,
    CONTROL_AUDIO_OUTPUT_DEVICES_REFRESH,
    CONTROL_AUDIO_OUTPUT_FILE,
    CONTROL_AUDIO_OUTPUT_FILE_FORMAT,
    CONTROL_AUDIO_SAMPLE_RATE,
    CONTROL_AUTO_GAIN,
    CONTROL_AUTO_GAIN_CEILING,
    CONTROL_BACKEND_FILE_GET,
    CONTROL_BACKEND_FILE_LIST,
    CONTROL_BACKEND_FILE_PUT,
    CONTROL_BACKEND_PARAM,
    CONTROL_SYNTHETIC_OBJECTS,
    CONTROL_OBJECT_GENERATOR,
    CONTROL_OBJECT_GENERATOR_PARAM,
    CONTROL_OPTION,
    CONTROL_PHANTOM_EXTRACT,
    CONTROL_PHANTOM_EXTRACT_PARAM,
    CONTROL_SURROUND_PLACEMENT,
    CONTROL_OUTPUT_CHANNEL_MAPPING,
    CONTROL_VIRTUAL_BED,
    CONTROL_CONFIG_AUDIO,
    CONTROL_CONFIG_AUDIO_APPLY,
    CONTROL_CONFIG_INPUT,
    CONTROL_CONFIG_INPUT_APPLY,
    CONTROL_CONFIG_LAYOUT,
    CONTROL_CONFIG_LAYOUT_APPLY,
    CONTROL_CONFIG_SPEAKERS,
    CONTROL_DEBUG_SPEAKER_GAINTABLE_NACK,
    CONTROL_DEBUG_SPEAKER_GAINTABLE_SUBSCRIBE,
    CONTROL_DEBUG_SPEAKER_GAINTABLE_UNSUBSCRIBE,
    CONTROL_DIAG_ENABLED,
    CONTROL_DIAG_RATE_HZ,
    CONTROL_DISTANCE_MODEL,
    CONTROL_DISTANCE_MODEL_METRIC,
    CONTROL_GAIN,
    CONTROL_INPUT_APPLY,
    CONTROL_INPUT_DRC_MODE,
    CONTROL_INPUT_DRC_WEIGHT,
    CONTROL_INPUT_LIVE_BACKEND,
    CONTROL_INPUT_LIVE_CHANNELS,
    CONTROL_INPUT_LIVE_CLOCK_MODE,
    CONTROL_INPUT_LIVE_DESCRIPTION,
    CONTROL_INPUT_LIVE_FORMAT,
    CONTROL_INPUT_LIVE_LAYOUT,
    CONTROL_INPUT_LIVE_LAYOUT_IMPORT,
    CONTROL_INPUT_LIVE_LFE_MODE,
    CONTROL_INPUT_LIVE_MAP,
    CONTROL_INPUT_LIVE_NODE,
    CONTROL_INPUT_LIVE_SAMPLE_RATE,
    CONTROL_INPUT_MODE,
    CONTROL_INPUT_REFRESH,
    CONTROL_LAYOUT_EXPORT,
    CONTROL_LAYOUT_RADIUS_M,
    CONTROL_LOG_LEVEL,
    CONTROL_LOUDNESS,
    CONTROL_METERING,
    CONTROL_METERING_RATE_HZ,
    CONTROL_OVERLAY_ENABLED,
    CONTROL_OVERLAY_HEATMAP_BANDS,
    CONTROL_OVERLAY_HEATMAP_COLORMAP,
    CONTROL_OVERLAY_HEATMAP_CUSTOM_STOPS,
    CONTROL_OVERLAY_HEATMAP_ENABLED,
    CONTROL_OVERLAY_LABELS,
    CONTROL_OVERLAY_OBJECTS,
    CONTROL_OVERLAY_TAG,
    CONTROL_OVERLAY_TRAILS,
    CONTROL_QUIT,
    CONTROL_RAMP_MODE,
    CONTROL_REALTIME_MASTER_GAIN,
    CONTROL_REALTIME_SPEAKER_GAIN,
    CONTROL_RELOAD_CONFIG,
    CONTROL_RENDER_BACKEND,
    CONTROL_RENDER_BACKEND_RESTORE,
    CONTROL_RENDER_BRIDGE_PATH,
    CONTROL_RENDER_EVALUATION_MODE,
    CONTROL_RENDER_EVALUATION_MODE_FROM_FILE,
    CONTROL_RENDER_EVALUATION_POSITION_INTERPOLATION,
    CONTROL_RENDER_INPUT_PIPE,
    CONTROL_RESUME,
    CONTROL_ROOM_RATIO,
    CONTROL_ROOM_RATIO_CENTER_BLEND,
    CONTROL_ROOM_RATIO_LOWER,
    CONTROL_ROOM_RATIO_REAR,
    CONTROL_SAVE_CONFIG,
    CONTROL_SPREAD_DISTANCE_CURVE,
    CONTROL_SPREAD_DISTANCE_RANGE,
    CONTROL_SPREAD_FROM_DISTANCE,
    CONTROL_SPREAD_MAX,
    CONTROL_SPREAD_MIN,
    CONTROL_SPREAD_SIZE_TO_SPREAD_MODE,
    CONTROL_YIELD_PORT,
];

pub const ALL_STATE: &[&str] = &[
    STATE_ADAPTIVE_RESAMPLING_BAND,
    STATE_ADAPTIVE_RESAMPLING_STATE,
    STATE_AUDIO,
    STATE_BACKEND_FILE_CONTENT,
    STATE_BACKEND_FILE_ERROR,
    STATE_BACKEND_FILE_LIST,
    STATE_CAPABILITIES,
    STATE_CLIP,
    STATE_OVERLAY,
    STATE_CONFIG_SAVED,
    STATE_CONFIG_SAVE_ERROR,
    STATE_CROSSOVER_TIME_MS,
    STATE_DEBUG_SPEAKER_GAINTABLE_CHUNK,
    STATE_DEBUG_SPEAKER_GAINTABLE_META,
    STATE_DEBUG_SPEAKER_GAINTABLE_UNAVAILABLE,
    STATE_DEBUG_SPEAKER_GAINTABLE_UPTODATE,
    STATE_DECODE_TIME_MS,
    STATE_DIAG_SCHEMA,
    STATE_DIAG_VALUES,
    STATE_FRAME_DURATION_MS,
    STATE_INPUT,
    STATE_INPUT_PIPE,
    STATE_LATENCY,
    STATE_LATENCY_AVAIL_INPUT,
    STATE_LATENCY_CONTROL,
    STATE_LATENCY_DOWNSTREAM,
    STATE_LATENCY_INSTANT,
    STATE_LATENCY_OUTPUT_FIFO,
    STATE_LATENCY_RESAMPLER_PENDING,
    STATE_LATENCY_SMOOTHED,
    STATE_LAYOUT,
    STATE_LOG_LEVEL,
    STATE_LOUDNESS,
    STATE_MONITORING,
    STATE_OPTIONS_SCHEMA,
    STATE_OSC_DIAG,
    STATE_OSC_METERING,
    STATE_REALTIME_MASTER_GAIN,
    STATE_REALTIME_SPEAKER_GAIN,
    STATE_RENDER_ABI,
    STATE_RENDER_BRIDGE_ERROR,
    STATE_RENDER_BRIDGE_PATH,
    STATE_RENDER_CONFIG_PATH,
    STATE_RENDER_CONFIG_STATUS,
    STATE_RENDERER,
    STATE_RENDER_EVALUATION_CARTESIAN_X_SIZE,
    STATE_RENDER_EVALUATION_CARTESIAN_Y_SIZE,
    STATE_RENDER_EVALUATION_CARTESIAN_Z_NEG_SIZE,
    STATE_RENDER_EVALUATION_CARTESIAN_Z_SIZE,
    STATE_RENDER_EVALUATION_POLAR_AZIMUTH_RESOLUTION,
    STATE_RENDER_EVALUATION_POLAR_DISTANCE_MAX,
    STATE_RENDER_EVALUATION_POLAR_DISTANCE_RES,
    STATE_RENDER_EVALUATION_POLAR_ELEVATION_RESOLUTION,
    STATE_RENDER_EVALUATION_POSITION_INTERPOLATION,
    STATE_RENDER_TIME_MS,
    STATE_RENDER_VERSION,
    STATE_RESAMPLE_RATIO,
    STATE_SHUTDOWN,
    STATE_SNAPSHOT_COMPLETE,
    STATE_SPEAKERS,
    STATE_SPEAKERS_RECOMPUTE_ERROR,
    STATE_SPEAKERS_RECOMPUTING,
    STATE_VBAP_ALLOW_NEGATIVE_Z,
    STATE_WRITE_TIME_MS,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn control_consts_are_control_addresses() {
        for &a in ALL_CONTROL {
            assert!(
                a.starts_with("/omniphony/control/"),
                "not a control address: {a}"
            );
        }
    }

    #[test]
    fn state_consts_are_state_addresses() {
        for &a in ALL_STATE {
            assert!(
                a.starts_with("/omniphony/state/"),
                "not a state address: {a}"
            );
        }
    }

    #[test]
    fn no_duplicate_addresses() {
        // A duplicated value would mean two names collide on one wire address,
        // or a typo merged two addresses — both are contract bugs.
        let mut seen = HashSet::new();
        for &a in ALL_CONTROL.iter().chain(ALL_STATE) {
            assert!(seen.insert(a), "duplicate OSC address in contract: {a}");
        }
    }

    #[test]
    fn object_prefix_composes() {
        assert_eq!(
            format!("{}{}/mute", CONTROL_OBJECT_PREFIX, 3),
            "/omniphony/control/object/3/mute"
        );
    }
}
