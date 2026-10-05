//! The Omniphony OSC control / state contract, as a single source of truth.
//!
//! The engine is driven and observed entirely over OSC: clients send
//! `/omniphony/control/…` messages and receive `/omniphony/state/…` updates.
//! Those address strings are the wire contract between the engine and every
//! client (Omniphony Studio, alternative front-ends, automation). They were
//! once bare string literals scattered across the dispatcher and the producers;
//! this module names them once so the Rust side references symbols instead of
//! magic strings, and so the surface is discoverable in one place.
//!
//! "Single source of truth" is enforced, not just asserted: a test walks the
//! OSC-facing sources and fails on any address spelled out rather than named
//! here. The reason it is worth a test is that the failure mode is invisible —
//! a mistyped literal compiles, matches nothing, and the control it belongs to
//! simply stops working, with no error anywhere.
//!
//! The human-readable companion — direction, argument types and semantics for
//! every address — lives in `docs/osc-control-contract.md`. Keep the two in
//! sync when adding or changing an address; a test fails when a catalogued
//! address is missing from that document's index.
//!
//! ## Address families with a dynamic or prefixed tail
//!
//! A few control addresses are not fixed strings:
//!
//! * **Per-object mute** — [`CONTROL_OBJECT_PREFIX`] + `"{id}/mute"`, e.g.
//!   `/omniphony/control/object/3/mute`.
//! * **Hybrid backend** — [`CONTROL_HYBRID_PREFIX`] + `{external_backend,
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
//!
//! ## Framing
//!
//! What both ends accept besides addresses is here too: how deep a datagram may
//! nest, and the check a listener runs before decoding one ([`nesting`]); and
//! the arguments each state address carries ([`shapes`]).

pub mod nesting;
pub mod shapes;

/// Revision of this contract. The engine advertises it as `contractRevision`
/// in `/state/capabilities`, and a client compares it with its own, so a
/// mismatch can be shown instead of discovered through a control that does
/// nothing.
///
/// Bump it with any change to the wire surface: an address added, removed or
/// renamed, or a change to the arguments an address carries. The address set
/// is fingerprinted by a test, so adding or removing one without a bump fails;
/// a change to arguments only is for the author to remember.
///
/// An engine that predates this advertises none, which a client reads as 0.
pub const CONTRACT_REVISION: u32 = 1;

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
// Optional correlation extension: get[backend,key,name-or-empty,request_id]
// and put[backend,key,name,content,request_id] carry an opaque string <=64 bytes.
// A capable renderer advertises fileRequestIds=true and appends the same id to
// content[backend,key,name,content,id] or error[backend,key,message,id]. Older
// clients omit it and receive the original payload shape. No cancellation or
// idempotent-write guarantee is implied by correlation.
pub const CONTROL_BACKEND_FILE_GET: &str = "/omniphony/control/backend/file/get";
pub const CONTROL_BACKEND_FILE_LIST: &str = "/omniphony/control/backend/file/list";
pub const CONTROL_BACKEND_FILE_PUT: &str = "/omniphony/control/backend/file/put";
pub const CONTROL_BACKEND_PARAM: &str = "/omniphony/control/backend/param";
pub const CONTROL_BINAURAL_AIR_ABSORPTION: &str = "/omniphony/control/binaural/air_absorption";
pub const CONTROL_BINAURAL_BRIR_HEAD_TRACKING: &str =
    "/omniphony/control/binaural/brir/head_tracking";
pub const CONTROL_BINAURAL_BRIR_MAX_LENGTH: &str = "/omniphony/control/binaural/brir/max_length";
pub const CONTROL_BINAURAL_BRIR_TAIL_FLOOR: &str = "/omniphony/control/binaural/brir/tail_floor";
pub const CONTROL_BINAURAL_DIFFUSE_FIELD_EQ: &str = "/omniphony/control/binaural/diffuse_field_eq";
pub const CONTROL_BINAURAL_EAR_GAIN: &str = "/omniphony/control/binaural/ear_gain";
pub const CONTROL_BINAURAL_EAR_MUTE: &str = "/omniphony/control/binaural/ear_mute";
pub const CONTROL_BINAURAL_HEAD_RADIUS: &str = "/omniphony/control/binaural/head_radius";
pub const CONTROL_BINAURAL_HRIR_SOURCE: &str = "/omniphony/control/binaural/hrir_source";
pub const CONTROL_BINAURAL_HRTF_UPLOAD_BEGIN: &str =
    "/omniphony/control/binaural/hrtf_upload/begin";
pub const CONTROL_BINAURAL_HRTF_UPLOAD_CHUNK: &str =
    "/omniphony/control/binaural/hrtf_upload/chunk";
pub const CONTROL_BINAURAL_HRTF_UPLOAD_END: &str = "/omniphony/control/binaural/hrtf_upload/end";
pub const CONTROL_BINAURAL_MODE: &str = "/omniphony/control/binaural_mode";
pub const CONTROL_BINAURAL_REFLECTIONS_ENABLED: &str =
    "/omniphony/control/binaural/reflections/enabled";
pub const CONTROL_BINAURAL_REFLECTIONS_LEVEL: &str =
    "/omniphony/control/binaural/reflections/level";
pub const CONTROL_BINAURAL_REFLECTIONS_ROOM_DEPTH: &str =
    "/omniphony/control/binaural/reflections/room_depth";
pub const CONTROL_BINAURAL_REFLECTIONS_ROOM_HEIGHT: &str =
    "/omniphony/control/binaural/reflections/room_height";
pub const CONTROL_BINAURAL_REFLECTIONS_ROOM_WIDTH: &str =
    "/omniphony/control/binaural/reflections/room_width";
pub const CONTROL_BINAURAL_REFLECTIONS_WALL_CUTOFF: &str =
    "/omniphony/control/binaural/reflections/wall_cutoff";
pub const CONTROL_BINAURAL_REVERB_ENABLED: &str = "/omniphony/control/binaural/reverb/enabled";
pub const CONTROL_BINAURAL_REVERB_LEVEL: &str = "/omniphony/control/binaural/reverb/level";
pub const CONTROL_BINAURAL_REVERB_PREDELAY: &str = "/omniphony/control/binaural/reverb/predelay";
pub const CONTROL_BINAURAL_REVERB_RT60: &str = "/omniphony/control/binaural/reverb/rt60";
pub const CONTROL_BINAURAL_REVERB_SIZE: &str = "/omniphony/control/binaural/reverb/size";
pub const CONTROL_BINAURAL_REVERB_RT60_LOW_RATIO: &str =
    "/omniphony/control/binaural/reverb/rt60_low_ratio";
pub const CONTROL_BINAURAL_REVERB_RT60_HIGH_RATIO: &str =
    "/omniphony/control/binaural/reverb/rt60_high_ratio";
pub const CONTROL_BINAURAL_UNIT_SCALE: &str = "/omniphony/control/binaural/unit_scale";
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
pub const CONTROL_HEAD_ORIENTATION: &str = "/omniphony/control/head/orientation";
pub const CONTROL_HEAD_QUAT: &str = "/omniphony/control/head/quat";
pub const CONTROL_HEAD_RECENTER: &str = "/omniphony/control/head/recenter";
/// `s`: `front` | `left` | `up` | `reset` — the three-pose sensor axis
/// calibration, one step per message (`front` also recenters).
pub const CONTROL_HEAD_CALIBRATE: &str = "/omniphony/control/head/calibrate";
pub const CONTROL_HEAD_TRACKING_ADDRESS: &str = "/omniphony/control/head/tracking/address";
pub const CONTROL_HEAD_TRACKING_FORMAT: &str = "/omniphony/control/head/tracking/format";
pub const CONTROL_HEAD_TRACKING_INVERT: &str = "/omniphony/control/head/tracking/invert";
pub const CONTROL_HEAD_TRACKING_SMOOTHING: &str = "/omniphony/control/head/tracking/smoothing";
pub const CONTROL_INPUT_APPLY: &str = "/omniphony/control/input/apply";
pub const CONTROL_INPUT_DRC_MODE: &str = "/omniphony/control/input/drc_mode";
pub const CONTROL_INPUT_DRC_WEIGHT: &str = "/omniphony/control/input/drc_weight";
pub const CONTROL_INPUT_LIVE_BACKEND: &str = "/omniphony/control/input/live/backend";
pub const CONTROL_INPUT_LIVE_CHANNELS: &str = "/omniphony/control/input/live/channels";
pub const CONTROL_INPUT_LIVE_CLOCK_MODE: &str = "/omniphony/control/input/live/clock_mode";
pub const CONTROL_INPUT_LIVE_DESCRIPTION: &str = "/omniphony/control/input/live/description";
pub const CONTROL_INPUT_LIVE_LAYOUT: &str = "/omniphony/control/input/live/layout";
pub const CONTROL_INPUT_LIVE_LAYOUT_IMPORT: &str = "/omniphony/control/input/live/layout_import";
pub const CONTROL_INPUT_LIVE_LFE_MODE: &str = "/omniphony/control/input/live/lfe_mode";
pub const CONTROL_INPUT_LIVE_MAP: &str = "/omniphony/control/input/live/map";
pub const CONTROL_INPUT_LIVE_NODE: &str = "/omniphony/control/input/live/node";
pub const CONTROL_INPUT_LIVE_SAMPLE_RATE: &str = "/omniphony/control/input/live/sample_rate";
pub const CONTROL_INPUT_MODE: &str = "/omniphony/control/input/mode";
pub const CONTROL_INPUT_REFRESH: &str = "/omniphony/control/input/refresh";
pub const CONTROL_LATENCY_TARGET: &str = "/omniphony/control/latency_target";
pub const CONTROL_LAYOUT_EXPORT: &str = "/omniphony/control/layout/export";
pub const CONTROL_LAYOUT_RADIUS_M: &str = "/omniphony/control/layout/radius_m";
pub const CONTROL_LOG_LEVEL: &str = "/omniphony/control/log_level";
pub const CONTROL_LOUDNESS: &str = "/omniphony/control/loudness";
pub const CONTROL_METERING: &str = "/omniphony/control/metering";
pub const CONTROL_METERING_RATE_HZ: &str = "/omniphony/control/metering/rate_hz";
pub const CONTROL_OUTPUT_MODE: &str = "/omniphony/control/output_mode";
pub const CONTROL_OVERLAY_ENABLED: &str = "/omniphony/control/overlay/enabled";
pub const CONTROL_OVERLAY_HEATMAP_BANDS: &str = "/omniphony/control/overlay/heatmap_bands";
pub const CONTROL_OVERLAY_HEATMAP_COLORMAP: &str = "/omniphony/control/overlay/heatmap_colormap";
pub const CONTROL_OVERLAY_HEATMAP_CUSTOM_STOPS: &str =
    "/omniphony/control/overlay/heatmap_custom_stops";
pub const CONTROL_OVERLAY_HEATMAP_ENABLED: &str = "/omniphony/control/overlay/heatmap_enabled";
pub const CONTROL_RENDER_EVALUATION_OBJECT_SIZE_INTERVALS: &str =
    "/omniphony/control/render_evaluation/object_size_intervals";
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
/// Crossover filter implementation for band-limited layouts: `lr4` (IIR, zero
/// latency) or `fir` (linear-phase, constant latency — the band sum is a pure
/// delay). Persisted to config.
pub const CONTROL_CROSSOVER_TYPE: &str = "/omniphony/control/crossover_type";
/// FIR crossover transition width as a fraction of the lowest cutoff
/// (float, clamped to [0.05, 2.0]): smaller = steeper bands, more
/// taps/latency. Persisted to config.
pub const CONTROL_CROSSOVER_FIR_TRANSITION_RATIO: &str =
    "/omniphony/control/crossover_fir_transition_ratio";
/// Decode on a thread of its own in the liborender engine (int 0/1), when
/// its host lets the option decide; the standalone renderer always does.
/// Persisted to config.
pub const CONTROL_DECODE_THREAD: &str = "/omniphony/control/decode_thread";
pub const CONTROL_UNKNOWN: &str = "/omniphony/control/unknown";
/// Legacy: the `generic` family's entries (see [`CONTROL_PLACEMENT_LAYOUT`]).
/// Argument is a YAML `SpeakerLayout`; an empty string clears them.
pub const CONTROL_VIRTUAL_BED: &str = "/omniphony/control/virtual_bed";
/// Placement mode of one source family's fixed channels
/// (`renderer::placement`): args `[family (string), mode (string)]`, family
/// one of `generic`, `dolby`, `dts`, `auro`, `pcm`; mode `sphere`, `room`,
/// `manual`, or `inherit` to clear the family's own choice (it then follows
/// `generic`, or its built-in default). Persisted to config on save;
/// re-plans the current stream.
pub const CONTROL_PLACEMENT_MODE: &str = "/omniphony/control/placement/mode";
/// One source family's own entries: args `[family (string), yaml (string)]`,
/// a YAML `SpeakerLayout` — one entry per channel label, `spatialize` =
/// virtual/direct and `gain_db` in every mode, the pose in manual mode. An
/// empty string clears them (the family then uses `generic`'s). Persisted to
/// config on save; re-plans the current stream.
pub const CONTROL_PLACEMENT_LAYOUT: &str = "/omniphony/control/placement/layout";
/// How finely an object must turn before its HRIR is rebuilt: `exact`
/// (default, bit-identical output), `fine`, `balanced` or `coarse`. Coarser
/// lattices skip more HRIR interpolation — and the crossfade that goes with it
/// — at a measurable cost in fidelity. Persisted to config.
pub const CONTROL_BINAURAL_HRIR_UPDATE_LATTICE: &str =
    "/omniphony/control/binaural/hrir_update_lattice";
/// Generic setter for any declared live option (`renderer::options`):
/// args `[key (string), value]` — `value` is as many arguments as the
/// option's kind takes (three numbers for `room_ratio`). The per-option
/// addresses listed above (`synthetic_objects`, `object_generator`,
/// `phantom_extract`, `surround_placement`, `output_channel_mapping`, the
/// `room_ratio*` family, …) are legacy aliases of this.
pub const CONTROL_OPTION: &str = "/omniphony/control/option";
/// Grouped setter: args `[key, value, key, value, …]`, each value as many
/// arguments as its option's kind takes. Every valid pair is applied at once:
/// one rebuild at most and one notification for the whole message, however
/// many keys it carries. An unknown key or a truncated value drops the whole
/// message; an invalid value drops only its pair.
pub const CONTROL_OPTIONS: &str = "/omniphony/control/options";
/// Apply a group of declared options: args `[group (string)]`. A `Staged`
/// group (the standalone renderer's live input) applies every value staged
/// since its last apply; a `Live` group has nothing waiting and is only
/// acknowledged. The per-domain apply addresses (`/control/input/apply`,
/// `/control/config/input/apply`, `/control/config/audio/apply`) are
/// aliases of this for their group.
pub const CONTROL_OPTIONS_APPLY: &str = "/omniphony/control/options/apply";
/// Named config profiles (docs/config-profiles.md). `switch`/`create`/`delete`
/// take `[name (string)]`; `rename` takes `[old (string), new (string)]`.
/// Every mutation saves the config and re-broadcasts [`STATE_PROFILES`].
pub const CONTROL_PROFILE_SWITCH: &str = "/omniphony/control/profile/switch";
pub const CONTROL_PROFILE_CREATE: &str = "/omniphony/control/profile/create";
pub const CONTROL_PROFILE_DELETE: &str = "/omniphony/control/profile/delete";
pub const CONTROL_PROFILE_RENAME: &str = "/omniphony/control/profile/rename";
/// What the object test's clip is, after a [`CONTROL_OBJECT_TEST_CLIP`] request.
///
/// Args: `[json: String]` — `{"name","path","seconds","sourceRate","channels",
/// "truncated"}` when one is loaded, `{"error":"…"}` when the file was refused,
/// and `{}` when it was cleared.
pub const STATE_OBJECT_TEST_CLIP: &str = "/omniphony/state/object_test/clip";

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
/// Restart the render pipeline, keeping the unsaved live state (it comes back
/// unsaved). For a change only a restart applies, such as a new bridge.
/// Honoured by a restartable (CLI) instance only.
pub const CONTROL_RESTART: &str = "/omniphony/control/restart";
/// Start or stop the per-speaker test signal (band-limited pink noise).
///
/// Args: `[speaker_idx: Int, level: Float, isolation: String]`. A negative
/// index stops any running test — the trigger policy (hold, fixed burst,
/// toggle) is the client's, so the renderer only ever hears "start this" or
/// "stop". Transient: never persisted, and cleared on a fresh start.
///
/// `level` is **peak** amplitude, clamped to `[0, 1]`: the renderer bounds the
/// injected signal to that peak, so `1.0` is full scale and no accepted level
/// can make the test clip by itself. It is not an RMS figure — pink noise
/// would peak a crest factor (~13 dB) above one. See
/// [`renderer::live_params::SpeakerTest`].
pub const CONTROL_SPEAKER_TEST: &str = "/omniphony/control/speaker_test";

/// Start, move or stop the object test signal: pink noise placed at a position
/// in the room and panned there by the active render backend.
///
/// Args: `[on: Int, x: Float, y: Float, z: Float, level: Float, size: Float,
/// isolation: String]`. `on = 0` stops; everything after it is then ignored.
/// Position is ADM Cartesian, each axis clamped to `[-1, 1]`: x left/right,
/// y back/front, z floor/ceiling. `size` is an isotropic extent in `[0, 1]`,
/// `0` being a point source. `level` is peak amplitude with the same meaning as
/// on [`CONTROL_SPEAKER_TEST`].
///
/// **Re-sending with a new position is how the object moves, and must be
/// cheap.** The renderer keeps position out of the signal's identity and ramps
/// the gains instead, so a stream of these messages — one per pointer move
/// while dragging — slides the source continuously without ever restarting the
/// noise. Changing `level` or `isolation` does restart it, deliberately.
///
/// What is heard is what the renderer would do with a real object there: the
/// gains come from whichever backend is live, so the out-of-hull mode, distance
/// model and spread settings all apply. In `output_mode: binaural` it renders
/// through the HRIR path as an object rather than falling silent.
///
/// Transient like the speaker test: never persisted, cleared on a fresh start,
/// and subject to the same safety cap.
pub const CONTROL_OBJECT_TEST: &str = "/omniphony/control/object_test";

/// Set the orbit applied to the object test's placed position.
///
/// Args: `[axis: String, radius: Float, period_s: Float, azimuth: Float,
/// elevation: Float]`. `axis` is `x`, `y`, `z` or `free`; the two angles are
/// read only for `free` and describe the axis direction in the usual ADM
/// convention. `radius` is in ADM units and `0` stops the orbit, so there is no
/// separate on/off. `period_s` is seconds per revolution.
///
/// A radius rather than a diameter because that is what the room's geometry is
/// stated in: √2 from the centre reaches a vertical edge, √3 a corner.
///
/// **Separate from [`CONTROL_OBJECT_TEST`] on purpose.** That message is
/// re-sent on every pointer move while dragging; folding the orbit into it
/// would mean re-stating these five arguments hundreds of times a second, or
/// losing the orbit the first time a client forgot them.
///
/// The renderer advances the phase itself, on the block clock. A client
/// stepping the angle over OSC would hand the smoothness of the motion — the
/// very thing this test exists to judge — to its own UI thread's worst moment.
///
/// The circle is clamped per axis to the room, which changes its shape rather
/// than its motion: it becomes a D, running along the wall for part of the turn
/// instead of arcing through it. The alternative silently shrinks the radius
/// that was asked for. Transient like the rest of the test.
pub const CONTROL_OBJECT_TEST_ROTATION: &str = "/omniphony/control/object_test/rotation";

/// Choose (or clear) the WAV file [`ObjectTestSignal::Clip`] plays.
///
/// Args: `[path: String]`. An empty path clears the clip. The file is read,
/// downmixed to mono, resampled to the render rate and peak-normalised **once,
/// here on the control thread** — the render path then only walks an array, so
/// choosing a file cannot cause a dropout.
///
/// Separate from [`CONTROL_OBJECT_TEST`] for the same reason the orbit is: that
/// message is re-sent on every pointer move while dragging, and a path is a
/// long argument to restate hundreds of times a second.
///
/// The result comes back on [`STATE_OBJECT_TEST_CLIP`], including the failure
/// case — a file that cannot be read exactly is refused rather than guessed at,
/// since the point of the feature is to hear a *known* signal.
///
/// [`ObjectTestSignal::Clip`]: renderer::live_params::ObjectTestSignal::Clip
pub const CONTROL_OBJECT_TEST_CLIP: &str = "/omniphony/control/object_test/clip";

/// Arm/disarm the test idle feed.
///
/// Args: `[on: Int]` (non-zero arms). While armed, the decode loop fabricates
/// silence input frames whenever no real input is flowing, so the output chain
/// is already warm when a test starts and the noise is heard immediately
/// instead of after the writer/latency-controller settling time. Serves the
/// speaker test and the object test alike — the address keeps its original name
/// for compatibility, but the feed is not specific to either.
/// The arm expires after a keepalive window; clients re-send it periodically
/// while their test pane is open, so a dead client cannot leave the feed
/// running forever. Transient: never persisted, cleared on a fresh start.
pub const CONTROL_SPEAKER_TEST_IDLE_FEED: &str = "/omniphony/control/speaker_test/idle_feed";
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
/// Ask for the live-state snapshot again: args `[reply_port (int)]`, the port
/// optional as on [`REGISTER`]. Sent by a client whose [`STATE_GENERATION`]
/// fell behind the engine's. Unlike a re-registration it resends nothing else
/// (no log backlog, no metering state), and leaves the registration as it is.
pub const CONTROL_STATE_REFRESH: &str = "/omniphony/control/state/refresh";
pub const CONTROL_YIELD_PORT: &str = "/omniphony/control/yield_port";
/// Sent to a standing-by instance (on the dynamic resume port it advertised in
/// reply to a yield) to ask it to re-acquire the OSC port + audio and resume.
pub const CONTROL_RESUME: &str = "/omniphony/control/resume";

/// Prefix for the per-object mute address. Append `"{id}/mute"`.
pub const CONTROL_OBJECT_PREFIX: &str = "/omniphony/control/object/";

// Families the engine matches by prefix and then by tail, rather than as whole
// addresses. Naming the prefix is what both sides can share: the engine strips
// it, a client appends to it, and neither spells the half the other relies on.

/// Append `"enabled"`, `"threshold"`, `"curve"`, `"metric"` or `"mirror_axes"`.
pub const CONTROL_DISTANCE_DIFFUSE_PREFIX: &str = "/omniphony/control/distance_diffuse/";
/// Append `"external_backend"`, `"internal_backend"`, `"metric"`, `"curve"` or
/// `"curve_smoothing"`.
pub const CONTROL_HYBRID_PREFIX: &str = "/omniphony/control/hybrid/";
/// Append `"x_size"`, `"y_size"`, `"z_size"` or `"z_neg_size"`.
pub const CONTROL_RENDER_EVALUATION_CARTESIAN_PREFIX: &str =
    "/omniphony/control/render_evaluation/cartesian/";
/// Append `"azimuth_resolution"`, `"elevation_resolution"`, `"distance_res"` or
/// `"distance_max"`.
pub const CONTROL_RENDER_EVALUATION_POLAR_PREFIX: &str =
    "/omniphony/control/render_evaluation/polar/";

// ── Control error codes ─────────────────────────────────────────────────────
//
// The `code` argument of [`STATE_CONTROL_ERROR`]: stable strings a client can
// match on, where the message beside them is only for a person.

/// No handler knows the address: not in this contract, or not in the one the
/// engine was built with.
pub const CONTROL_ERROR_UNKNOWN_ADDRESS: &str = "unknown_address";
/// The handler for the address refused its arguments: a wrong type, a missing
/// one, a value out of range or not one it accepts.
pub const CONTROL_ERROR_INVALID_ARGUMENTS: &str = "invalid_arguments";
/// The address is in the contract, but nothing on this engine applied it: its
/// handler refused the arguments without saying which, or this host does not
/// implement it (an audio-output control sent to an engine embedded in mpv).
pub const CONTROL_ERROR_NOT_APPLIED: &str = "not_applied";
/// The datagram is not OSC the engine can decode, or nests deeper than
/// [`nesting::MAX_NESTING`]. Its address is unknown, so the reply's is empty.
pub const CONTROL_ERROR_UNDECODABLE: &str = "undecodable";

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
/// A control the engine did not apply, sent back to its sender only: args
/// `[address (string), code (string), message (string)]`. `code` is one of the
/// `CONTROL_ERROR_*` values; `message` is for a person. `address` is empty when
/// the datagram could not be decoded at all.
///
/// Silence still does not mean success: a handler that takes the message and
/// then finds nothing to change sends nothing either.
pub const STATE_CONTROL_ERROR: &str = "/omniphony/state/control_error";
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
/// Where the control-plane state a client holds stands: args
/// `[generation (int), full (int), part (int), parts (int)]`.
///
/// The engine counts every state publication that is not telemetry (the
/// snapshot, and the values controls and the engine publish) and sends the
/// count with the state it versions. With `full = 0` it follows an update, as
/// part 0 of 1, and a client that holds generation `g` expects `g + 1`: any
/// other value means one went missing. With `full = 1` it opens each datagram
/// of a snapshot, with that datagram's index and the snapshot's datagram count:
/// a client that has every part of it holds that generation whatever it held
/// before, and one that misses a part does not. The [`HEARTBEAT_ACK`] carries
/// the current count too, so the last update of a burst is not lost unnoticed
/// either. A client that falls behind sends [`CONTROL_STATE_REFRESH`].
///
/// The count is taken with the state, under one lock in the engine, so a later
/// count never carries an older state. It wraps; compare for equality only.
pub const STATE_GENERATION: &str = "/omniphony/state/generation";
pub const STATE_HEAD_POSE: &str = "/omniphony/state/head_pose";
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
pub const STATE_LATENCY_TARGET: &str = "/omniphony/state/latency_target";
pub const STATE_LAYOUT: &str = "/omniphony/state/layout";
pub const STATE_LOG_LEVEL: &str = "/omniphony/state/log_level";
pub const STATE_LOUDNESS: &str = "/omniphony/state/loudness";
pub const STATE_MONITORING: &str = "/omniphony/state/monitoring";
pub const STATE_OBJECT_GENERATORS: &str = "/omniphony/state/object_generators";
pub const STATE_OBJECT_TEST_POSITION: &str = "/omniphony/state/object_test/position";
/// Schema of the declared live options (`renderer::options` registry rows),
/// as a JSON string. Same pattern as `/state/object_generators` / `/state/phantom`.
pub const STATE_OPTIONS_SCHEMA: &str = "/omniphony/state/options_schema";
/// The options a host declares (the standalone renderer's audio output and
/// live input), as JSON: `{"options": {key: requested value}, "applied":
/// {key: value in force}, "pending": {group: bool}}`. Sent with every
/// live-state bundle by a host that declares any; the core options stay in
/// the `/state/renderer` `options` block.
pub const STATE_HOST_OPTIONS: &str = "/omniphony/state/host_options";
pub const STATE_PHANTOM: &str = "/omniphony/state/phantom";
/// Named config profiles view as JSON: `{"active": "...", "names": ["..."]}`.
/// Broadcast in the state snapshot and after every profile mutation.
pub const STATE_PROFILES: &str = "/omniphony/state/profiles";
pub const STATE_OSC_DIAG: &str = "/omniphony/state/osc/diag";
pub const STATE_OSC_METERING: &str = "/omniphony/state/osc/metering";
pub const STATE_REALTIME_MASTER_GAIN: &str = "/omniphony/state/realtime/master_gain";
pub const STATE_REALTIME_SPEAKER_GAIN: &str = "/omniphony/state/realtime/speaker_gain";
pub const STATE_RENDER_ABI: &str = "/omniphony/state/render/abi";
/// The `bridge_api` version this engine was built against (`"0.5.0"`): a
/// decoder bridge loads only if it was built against the same minor.
pub const STATE_RENDER_BRIDGE_API: &str = "/omniphony/state/render/bridge_api";
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
pub const STATE_RENDER_EVALUATION_OBJECT_SIZE_INTERVALS: &str =
    "/omniphony/state/render_evaluation/object_size_intervals";
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
pub const STATE_RENDER_EXECUTABLE: &str = "/omniphony/state/render/executable";
pub const STATE_RESAMPLE_RATIO: &str = "/omniphony/state/resample_ratio";
pub const STATE_SHUTDOWN: &str = "/omniphony/state/shutdown";
pub const STATE_SNAPSHOT_COMPLETE: &str = "/omniphony/state/snapshot_complete";
pub const STATE_SPEAKERS: &str = "/omniphony/state/speakers";
pub const STATE_SPEAKERS_RECOMPUTE_ERROR: &str = "/omniphony/state/speakers/recompute_error";
pub const STATE_SPEAKERS_RECOMPUTING: &str = "/omniphony/state/speakers/recomputing";
pub const STATE_VBAP_ALLOW_NEGATIVE_Z: &str = "/omniphony/state/vbap/allow_negative_z";
pub const STATE_WRITE_TIME_MS: &str = "/omniphony/state/write_time_ms";

// ── Session handshake and high-rate streams ─────────────────────────────────
//
// Everything under `/omniphony/` that is neither a control nor a state address:
// the client/engine handshake, the log relay, and the per-frame streams whose
// rate makes them their own family (objects, meters, timestamps).

pub const BED_CONFIG: &str = "/omniphony/bed/config";
pub const HEARTBEAT: &str = "/omniphony/heartbeat";
/// Reply to a registered client's [`HEARTBEAT`]: args `[instance_epoch (int),
/// state_generation (int)]`. The epoch is random per engine instance, so a
/// change means another engine answers on the port. The generation is the
/// current [`STATE_GENERATION`] count; an engine older than contract revision
/// 1 sends the epoch alone.
pub const HEARTBEAT_ACK: &str = "/omniphony/heartbeat/ack";
pub const HEARTBEAT_UNKNOWN: &str = "/omniphony/heartbeat/unknown";
pub const LOG: &str = "/omniphony/log";
pub const METER_DRC_GAIN: &str = "/omniphony/meter/drc_gain";
pub const METER_MASTER: &str = "/omniphony/meter/master";
pub const REGISTER: &str = "/omniphony/register";
pub const SPATIAL_FRAME: &str = "/omniphony/spatial/frame";
/// Suffix for the per-object lifecycle message: `/omniphony/object/{id}/remove`.
///
/// An object's slot going away used to be signalled only by zeroing its
/// position, size and meta — a client had to infer "gone" from "silent at the
/// origin", or from the frame's object count, which is what left ghosts behind
/// after a seek. This says it.
///
/// The zeroed triple is still sent for clients that predate this.
pub const OBJECT_REMOVE_SUFFIX: &str = "remove";
/// The per-object stream family, `/omniphony/object/{id}/…`. A prefix, matched
/// with `starts_with`, so not catalogued.
pub const OBJECT_STREAM_PREFIX: &str = "/omniphony/object/";
/// The meter family, `/omniphony/meter/…` (objects, speakers, ears, master,
/// DRC gain). A prefix, like [`OBJECT_STREAM_PREFIX`].
pub const METER_PREFIX: &str = "/omniphony/meter/";
/// `h pos`: the stream messages that follow — object frames, timestamps, meter
/// bundles — describe the block of audio starting at sample `pos`. Sent only
/// while the engine also publishes [`PLAYOUT_HEARD`], and only ahead of the
/// first such message of a new block, so it costs nothing when nobody waits.
pub const PLAYOUT_BLOCK: &str = "/omniphony/playout/block";
/// `h pos i rate`: the listener is hearing sample `pos` of the same timeline as
/// [`PLAYOUT_BLOCK`], which advances by `rate` per second while it plays. With
/// both, a client can show each block when it is heard instead of when it was
/// rendered, which is up to the whole output buffer (seconds, behind a host
/// such as Kodi) earlier.
pub const PLAYOUT_HEARD: &str = "/omniphony/playout/heard";
pub const TIMESTAMP: &str = "/omniphony/timestamp";
pub const YIELD_RESUME_PORT: &str = "/omniphony/yield/resume_port";

// ── Address catalogues (handy for clients / tests) ──────────────────────────

/// Whether `address` is a control this contract defines: catalogued (the test
/// sentinel [`CONTROL_UNKNOWN`] aside), or under one of the families matched by
/// prefix. A linear search, for the error path.
pub fn is_known_control(address: &str) -> bool {
    const FAMILIES: &[&str] = &[
        CONTROL_OBJECT_PREFIX,
        CONTROL_DISTANCE_DIFFUSE_PREFIX,
        CONTROL_HYBRID_PREFIX,
        CONTROL_RENDER_EVALUATION_CARTESIAN_PREFIX,
        CONTROL_RENDER_EVALUATION_POLAR_PREFIX,
    ];
    (address != CONTROL_UNKNOWN && ALL_CONTROL.contains(&address))
        || FAMILIES.iter().any(|family| address.starts_with(family))
}

pub const ALL_CONTROL: &[&str] = &[
    CONTROL_ADAPTIVE_RESAMPLING,
    CONTROL_BINAURAL_HRIR_UPDATE_LATTICE,
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
    CONTROL_OPTIONS,
    CONTROL_OPTIONS_APPLY,
    CONTROL_PROFILE_SWITCH,
    CONTROL_PROFILE_CREATE,
    CONTROL_PROFILE_DELETE,
    CONTROL_PROFILE_RENAME,
    CONTROL_PHANTOM_EXTRACT,
    CONTROL_PHANTOM_EXTRACT_PARAM,
    CONTROL_SURROUND_PLACEMENT,
    CONTROL_OUTPUT_CHANNEL_MAPPING,
    CONTROL_CROSSOVER_TYPE,
    CONTROL_CROSSOVER_FIR_TRANSITION_RATIO,
    CONTROL_DECODE_THREAD,
    CONTROL_VIRTUAL_BED,
    CONTROL_PLACEMENT_MODE,
    CONTROL_PLACEMENT_LAYOUT,
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
    CONTROL_RESTART,
    CONTROL_SPEAKER_TEST,
    CONTROL_SPEAKER_TEST_IDLE_FEED,
    CONTROL_RENDER_BACKEND,
    CONTROL_RENDER_BACKEND_RESTORE,
    CONTROL_RENDER_BRIDGE_PATH,
    CONTROL_OBJECT_TEST,
    CONTROL_OBJECT_TEST_CLIP,
    CONTROL_OBJECT_TEST_ROTATION,
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
    CONTROL_STATE_REFRESH,
    CONTROL_YIELD_PORT,
    CONTROL_BINAURAL_AIR_ABSORPTION,
    CONTROL_BINAURAL_BRIR_HEAD_TRACKING,
    CONTROL_BINAURAL_BRIR_MAX_LENGTH,
    CONTROL_BINAURAL_BRIR_TAIL_FLOOR,
    CONTROL_BINAURAL_DIFFUSE_FIELD_EQ,
    CONTROL_BINAURAL_EAR_GAIN,
    CONTROL_BINAURAL_EAR_MUTE,
    CONTROL_BINAURAL_HEAD_RADIUS,
    CONTROL_BINAURAL_HRIR_SOURCE,
    CONTROL_BINAURAL_HRTF_UPLOAD_BEGIN,
    CONTROL_BINAURAL_HRTF_UPLOAD_CHUNK,
    CONTROL_BINAURAL_HRTF_UPLOAD_END,
    CONTROL_BINAURAL_MODE,
    CONTROL_BINAURAL_REFLECTIONS_ENABLED,
    CONTROL_BINAURAL_REFLECTIONS_LEVEL,
    CONTROL_BINAURAL_REFLECTIONS_ROOM_DEPTH,
    CONTROL_BINAURAL_REFLECTIONS_ROOM_HEIGHT,
    CONTROL_BINAURAL_REFLECTIONS_ROOM_WIDTH,
    CONTROL_BINAURAL_REFLECTIONS_WALL_CUTOFF,
    CONTROL_BINAURAL_REVERB_ENABLED,
    CONTROL_BINAURAL_REVERB_LEVEL,
    CONTROL_BINAURAL_REVERB_PREDELAY,
    CONTROL_BINAURAL_REVERB_RT60,
    CONTROL_BINAURAL_REVERB_RT60_HIGH_RATIO,
    CONTROL_BINAURAL_REVERB_RT60_LOW_RATIO,
    CONTROL_BINAURAL_REVERB_SIZE,
    CONTROL_BINAURAL_UNIT_SCALE,
    CONTROL_HEAD_ORIENTATION,
    CONTROL_HEAD_QUAT,
    CONTROL_HEAD_RECENTER,
    CONTROL_HEAD_CALIBRATE,
    CONTROL_HEAD_TRACKING_ADDRESS,
    CONTROL_HEAD_TRACKING_FORMAT,
    CONTROL_HEAD_TRACKING_INVERT,
    CONTROL_HEAD_TRACKING_SMOOTHING,
    CONTROL_LATENCY_TARGET,
    CONTROL_OUTPUT_MODE,
    CONTROL_RENDER_EVALUATION_OBJECT_SIZE_INTERVALS,
    CONTROL_UNKNOWN,
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
    STATE_CONTROL_ERROR,
    STATE_OBJECT_TEST_CLIP,
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
    STATE_GENERATION,
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
    STATE_HOST_OPTIONS,
    STATE_PROFILES,
    STATE_OSC_DIAG,
    STATE_OSC_METERING,
    STATE_REALTIME_MASTER_GAIN,
    STATE_REALTIME_SPEAKER_GAIN,
    STATE_RENDER_ABI,
    STATE_RENDER_BRIDGE_API,
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
    STATE_RENDER_EXECUTABLE,
    STATE_RESAMPLE_RATIO,
    STATE_SHUTDOWN,
    STATE_SNAPSHOT_COMPLETE,
    STATE_SPEAKERS,
    STATE_SPEAKERS_RECOMPUTE_ERROR,
    STATE_SPEAKERS_RECOMPUTING,
    STATE_VBAP_ALLOW_NEGATIVE_Z,
    STATE_WRITE_TIME_MS,
    STATE_HEAD_POSE,
    STATE_LATENCY_TARGET,
    STATE_OBJECT_GENERATORS,
    STATE_OBJECT_TEST_POSITION,
    STATE_PHANTOM,
    STATE_RENDER_EVALUATION_OBJECT_SIZE_INTERVALS,
];

/// Session handshake and high-rate stream addresses: everything under
/// `/omniphony/` that is neither `control/` nor `state/`.
///
/// Catalogued for the same reason as the other two — so the guard tests below
/// see the whole wire surface, not just the part that happens to be named after
/// a direction.
pub const ALL_SESSION: &[&str] = &[
    BED_CONFIG,
    HEARTBEAT,
    HEARTBEAT_ACK,
    HEARTBEAT_UNKNOWN,
    LOG,
    METER_DRC_GAIN,
    METER_MASTER,
    PLAYOUT_BLOCK,
    PLAYOUT_HEARD,
    REGISTER,
    SPATIAL_FRAME,
    TIMESTAMP,
    YIELD_RESUME_PORT,
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
    fn session_consts_are_neither_control_nor_state() {
        for &a in ALL_SESSION {
            assert!(
                a.starts_with("/omniphony/"),
                "not an omniphony address: {a}"
            );
            assert!(
                !a.starts_with("/omniphony/control/") && !a.starts_with("/omniphony/state/"),
                "belongs in ALL_CONTROL or ALL_STATE, not here: {a}"
            );
        }
    }

    #[test]
    fn no_duplicate_addresses() {
        // A duplicated value would mean two names collide on one wire address,
        // or a typo merged two addresses — both are contract bugs.
        let mut seen = HashSet::new();
        for &a in ALL_CONTROL.iter().chain(ALL_STATE).chain(ALL_SESSION) {
            assert!(seen.insert(a), "duplicate OSC address in contract: {a}");
        }
    }

    /// The OSC-facing sources must name addresses, not spell them.
    ///
    /// This module claims to be the single source of truth, but nothing used to
    /// enforce it: a bare literal compiles just as well as a constant, and a
    /// typo in one produces an address that silently never matches — no error,
    /// no log, just a control that does nothing. So the claim is checked.
    ///
    /// Two forms are still spelled out, because they are not addresses: family
    /// prefixes matched with `starts_with`, and `format!` templates with a
    /// dynamic tail (`/omniphony/meter/object/{}`). Both would need a different
    /// shape in the contract than a `&str` constant; until they get one, they
    /// are listed here rather than silently tolerated.
    #[test]
    fn osc_sources_reference_the_contract_instead_of_spelling_addresses() {
        use std::path::Path;

        // Relative to this crate's root, so the guard follows the files. Both
        // ends of the wire are listed: an address only works if the two agree,
        // and a typo on the client side fails exactly as silently.
        const SOURCES: &[&str] = &[
            "../omniphony-renderer/runtime_control/src/osc.rs",
            "../omniphony-renderer/runtime_control/src/command.rs",
            "../omniphony-renderer/runtime_control/src/live_control.rs",
            "../omniphony-renderer/host_audio/src/lib.rs",
            "../omniphony-renderer/orender_engine/src/osc.rs",
            "../omniphony-renderer/orender_engine/src/osc/dispatch.rs",
            "../omniphony-renderer/orender_engine/src/osc/transport.rs",
            "../omniphony-renderer/orender_engine/src/osc/export.rs",
            "../omniphony-renderer/orender_engine/src/osc/recompute.rs",
            "../omniphony-renderer/orender_engine/src/osc/state_emit.rs",
            "../omniphony-renderer/orender_engine/src/osc/metadata_emit.rs",
            "../omniphony-renderer/orender_engine/src/osc/profiles.rs",
            // The core's handlers and snapshot producer, and the options
            // registry's legacy aliases: whole directories, so a module added
            // there is covered without anyone remembering to list it.
            "../omniphony-renderer/runtime_control/src/",
            "../omniphony-renderer/renderer/src/",
            // The client's send path. A directory, so a command module added
            // tomorrow is covered without anyone remembering to list it.
            "../omniphony-studio-egui/core/src/host/commands/",
        ];

        /// Every `.rs` under a directory, or the file itself.
        fn sources(path: &Path) -> Vec<std::path::PathBuf> {
            if path.is_file() {
                return vec![path.to_owned()];
            }
            let mut out = Vec::new();
            let entries = std::fs::read_dir(path)
                .unwrap_or_else(|e| panic!("guard is stale: cannot read {}: {e}", path.display()));
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    out.extend(sources(&p));
                } else if p.extension().is_some_and(|e| e == "rs") {
                    out.push(p);
                }
            }
            out.sort();
            out
        }

        let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("crate root");
        let root = root.as_path();
        // A file can be reached through its own entry and its directory's:
        // scan each once.
        let files: std::collections::BTreeSet<std::path::PathBuf> = SOURCES
            .iter()
            .flat_map(|rel| sources(&root.join(rel)))
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
            .collect();
        let mut offenders = Vec::new();
        for path in files {
            // Shown relative to the repository root.
            let rel = path
                .strip_prefix(root.parent().unwrap_or(root))
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("guard is stale: cannot read {}: {e}", path.display()));
            for (n, line) in src.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let mut rest = line;
                while let Some(i) = rest.find("\"/omniphony/") {
                    let after = &rest[i + 1..];
                    let Some(end) = after.find('"') else { break };
                    let addr = &after[..end];
                    // A prefix is matched with `starts_with`; a template is
                    // filled in by `format!`. Neither is a whole address.
                    if !addr.ends_with('/') && !addr.contains('{') {
                        offenders.push(format!("{}:{}: {addr}", rel, n + 1));
                    }
                    rest = &after[end..];
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "OSC addresses spelled out instead of referencing this module \
             (add a constant here and use it):\n  {}",
            offenders.join("\n  ")
        );
    }

    /// `docs/osc-control-contract.md` indexes every catalogued address, so an
    /// address added here without documentation fails instead of drifting.
    #[test]
    fn every_catalogued_address_is_indexed_in_the_contract_doc() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/osc-control-contract.md"
        );
        let doc = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("guard is stale: cannot read {path}: {e}"));
        let missing: Vec<&str> = ALL_CONTROL
            .iter()
            .chain(ALL_STATE)
            .chain(ALL_SESSION)
            .copied()
            .filter(|a| !doc.contains(&format!("`{a}`")))
            .collect();
        assert!(
            missing.is_empty(),
            "addresses missing from the docs/osc-control-contract.md index:\n  {}",
            missing.join("\n  ")
        );
    }

    /// The address set [`CONTRACT_REVISION`] was last bumped for, as
    /// `(revision, fingerprint)`. Change both together, and only together with
    /// a bump: a new fingerprint under the old revision tells clients nothing
    /// changed when it did.
    const PINNED_ADDRESS_SET: (u32, u64) = (1, 0x6ef3_1994_e50f_d48d);

    /// FNV-1a over the sorted catalogue, so the fingerprint follows the set
    /// and not the order the lists happen to be written in.
    fn address_set_fingerprint() -> u64 {
        let mut addresses: Vec<&str> = ALL_CONTROL
            .iter()
            .chain(ALL_STATE)
            .chain(ALL_SESSION)
            .copied()
            .collect();
        addresses.sort_unstable();
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for address in addresses {
            for byte in address.bytes().chain(std::iter::once(b'\n')) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn the_contract_revision_moves_with_the_address_set() {
        let fingerprint = address_set_fingerprint();
        assert!(
            PINNED_ADDRESS_SET == (CONTRACT_REVISION, fingerprint),
            "the catalogued address set or CONTRACT_REVISION changed: bump \
             CONTRACT_REVISION for any change to the wire surface, then pin \
             PINNED_ADDRESS_SET = ({}, {fingerprint:#018x})",
            CONTRACT_REVISION.max(PINNED_ADDRESS_SET.0 + 1),
        );
    }

    #[test]
    fn known_controls_are_the_catalogue_and_the_families() {
        assert!(is_known_control(CONTROL_GAIN));
        assert!(is_known_control("/omniphony/control/object/3/mute"));
        assert!(is_known_control("/omniphony/control/hybrid/metric"));
        assert!(!is_known_control(CONTROL_UNKNOWN));
        assert!(!is_known_control("/omniphony/control/gainn"));
    }

    #[test]
    fn object_prefix_composes() {
        assert_eq!(
            format!("{}{}/mute", CONTROL_OBJECT_PREFIX, 3),
            "/omniphony/control/object/3/mute"
        );
    }
}
