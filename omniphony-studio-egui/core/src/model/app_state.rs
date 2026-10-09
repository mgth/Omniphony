#![allow(dead_code)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::layouts::Layout;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SourcePosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(rename = "coordMode", skip_serializing_if = "Option::is_none")]
    pub coord_mode: Option<String>,
    #[serde(rename = "azimuthDeg", skip_serializing_if = "Option::is_none")]
    pub azimuth_deg: Option<f64>,
    #[serde(rename = "elevationDeg", skip_serializing_if = "Option::is_none")]
    pub elevation_deg: Option<f64>,
    #[serde(rename = "distanceM", skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    #[serde(rename = "gainDb", skip_serializing_if = "Option::is_none")]
    pub gain_db: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(rename = "directSpeakerIndex", skip_serializing_if = "Option::is_none")]
    pub direct_speaker_index: Option<u32>,
    /// `true` for a fixed channel (pose from the channel plan), `false`/absent
    /// for a dynamic object. Explicit — never inferred from
    /// `directSpeakerIndex` (which stays as position info).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixed: Option<bool>,
    /// Canonical channel-label name for a fixed channel ("L", "TFL"…).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// "height", "phantom", or absent for an ordinary object. Explicit from
    /// the renderer; inferred from the name by the parser only for renderers
    /// that predate the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "sourceTag", skip_serializing_if = "Option::is_none")]
    pub source_tag: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Meter {
    #[serde(rename = "peakDbfs")]
    pub peak_dbfs: f64,
    #[serde(rename = "rmsDbfs")]
    pub rms_dbfs: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomRatio {
    pub width: f64,
    pub length: f64,
    pub height: f64,
    pub rear: f64,
    pub lower: f64,
    #[serde(rename = "centerBlend")]
    pub center_blend: f64,
    // Room scale (metres-per-unit = radius_m = Width/2), broadcast by the
    // renderer in the room domain so Studio restores the m/u reliably.
    #[serde(rename = "scaleM", default = "default_room_scale_m")]
    pub scale_m: f64,
}

fn default_room_scale_m() -> f64 {
    1.0
}

impl Default for RoomRatio {
    fn default() -> Self {
        Self {
            width: 1.0,
            length: 2.0,
            height: 1.0,
            rear: 1.0,
            lower: 0.5,
            center_blend: 0.5,
            scale_m: 1.0,
        }
    }
}

impl RoomRatio {
    /// The unit cube: no warp at all, every half-axis one unit, `scale_m`
    /// metres to the unit. What the direct binaural path renders in (the
    /// renderer's `RoomRatios::UNIT`, scaled by its `unit_scale_m`).
    pub fn unit(scale_m: f64) -> Self {
        Self {
            width: 1.0,
            length: 1.0,
            height: 1.0,
            rear: 1.0,
            lower: 1.0,
            center_blend: 0.0,
            scale_m,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SpreadState {
    pub min: Option<f64>,
    pub max: Option<f64>,
    #[serde(rename = "fromDistance")]
    pub from_distance: Option<bool>,
    #[serde(rename = "distanceRange")]
    pub distance_range: Option<f64>,
    #[serde(rename = "distanceCurve")]
    pub distance_curve: Option<f64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DistanceDiffuse {
    pub enabled: Option<bool>,
    pub threshold: Option<f64>,
    pub curve: Option<f64>,
    pub metric: Option<String>,
    /// Which axes the mirror image negates (the renderer's snapshot).
    #[serde(
        rename = "mirrorAxes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub mirror_axes: Option<MirrorAxes>,
}

/// `distanceDiffuse.mirrorAxes`. The renderer's default is X and Y: a
/// half-turn about the vertical, the classic behaviour.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorAxes {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

impl Default for MirrorAxes {
    fn default() -> Self {
        Self {
            x: true,
            y: true,
            z: false,
        }
    }
}

impl MirrorAxes {
    /// The renderer's `/distance_diffuse/mirror_axes` argument: the flipped
    /// axes spelled out, `none` for none. The set goes as one string because
    /// the flips compose into a single mirror rather than acting apart.
    pub fn to_arg(self) -> String {
        let axes: String = [(self.x, 'x'), (self.y, 'y'), (self.z, 'z')]
            .into_iter()
            .filter_map(|(on, axis)| on.then_some(axis))
            .collect();
        if axes.is_empty() {
            "none".to_owned()
        } else {
            axes
        }
    }

    /// `symmetryI18nKey`: what the flips compose into. One is a reflection in
    /// the plane normal to its axis, two a half-turn about the axis left
    /// alone, three an inversion through the origin.
    pub fn symmetry_key(self) -> &'static str {
        match (self.x, self.y, self.z) {
            (false, false, false) => "distance.symmetry.none",
            (true, false, false) => "distance.symmetry.planeX",
            (false, true, false) => "distance.symmetry.planeY",
            (false, false, true) => "distance.symmetry.planeZ",
            (false, true, true) => "distance.symmetry.axisX",
            (true, false, true) => "distance.symmetry.axisY",
            (true, true, false) => "distance.symmetry.axisZ",
            (true, true, true) => "distance.symmetry.origin",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DistanceModelState {
    pub value: Option<String>,
    pub metric: Option<String>,
}

/// The evaluation grid a bridge hints (`evaluationGridBridge` of
/// `/state/renderer`): what the grid is while it follows the bridge.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeGrid {
    pub mode: String,
    pub x_size: u32,
    pub y_size: u32,
    pub z_size: u32,
    pub z_neg_size: u32,
    pub allow_negative_z: bool,
    /// The hinting bridge's place among the loaded bridges of
    /// [`RenderBridges::bridges`].
    #[serde(default)]
    pub bridge_index: Option<usize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VbapCartesian {
    #[serde(rename = "xSize")]
    pub x_size: Option<u32>,
    #[serde(rename = "ySize")]
    pub y_size: Option<u32>,
    #[serde(rename = "zSize")]
    pub z_size: Option<u32>,
    #[serde(rename = "zNegSize")]
    pub z_neg_size: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VbapPolar {
    #[serde(rename = "azimuthResolution")]
    pub azimuth_resolution: Option<u32>,
    #[serde(rename = "elevationResolution")]
    pub elevation_resolution: Option<u32>,
    #[serde(rename = "distanceRes")]
    pub distance_res: Option<u32>,
    #[serde(rename = "distanceMax")]
    pub distance_max: Option<f64>,
    #[serde(rename = "positionInterpolation")]
    pub position_interpolation: Option<bool>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BackendCapabilitiesState {
    #[serde(rename = "supportsRealtime", alias = "supports_realtime")]
    pub supports_realtime: bool,
    #[serde(
        rename = "supportsPrecomputedPolar",
        alias = "supports_precomputed_polar"
    )]
    pub supports_precomputed_polar: bool,
    #[serde(
        rename = "supportsPrecomputedCartesian",
        alias = "supports_precomputed_cartesian"
    )]
    pub supports_precomputed_cartesian: bool,
    #[serde(
        rename = "supportsPositionInterpolation",
        alias = "supports_position_interpolation"
    )]
    pub supports_position_interpolation: bool,
    #[serde(rename = "supportsDistanceModel", alias = "supports_distance_model")]
    pub supports_distance_model: bool,
    #[serde(rename = "supportsSpread", alias = "supports_spread")]
    pub supports_spread: bool,
    #[serde(
        rename = "supportsSpreadFromDistance",
        alias = "supports_spread_from_distance"
    )]
    pub supports_spread_from_distance: bool,
    #[serde(
        rename = "supportsDistanceDiffuse",
        alias = "supports_distance_diffuse"
    )]
    pub supports_distance_diffuse: bool,
    #[serde(rename = "supportsTableExport", alias = "supports_table_export")]
    pub supports_table_export: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct HybridState {
    #[serde(rename = "externalBackend", alias = "external_backend")]
    pub external_backend: Option<String>,
    #[serde(rename = "internalBackend", alias = "internal_backend")]
    pub internal_backend: Option<String>,
    pub curve: Vec<[f64; 2]>,
    #[serde(rename = "curveSmoothing", alias = "curve_smoothing")]
    pub curve_smoothing: Option<f64>,
    pub metric: Option<String>,
}

impl RenderBackendState {
    /// Bring a freshly-received backend state into a shape the frontend can
    /// render without checking it first.
    ///
    /// The renderer is the authority on which backends exist, but the values
    /// still travel over a protocol that can carry a stale config: an id from
    /// a backend that is no longer registered, a curve point outside the unit
    /// square, a metric nothing implements. The frontend used to test all of
    /// this on arrival, which put the rules for "valid state" on the far side
    /// of the boundary from the state itself.
    pub fn sanitize(&mut self) {
        fn lowered(value: &Option<String>) -> Option<String> {
            value
                .as_ref()
                .map(|v| v.trim().to_ascii_lowercase())
                .filter(|v| !v.is_empty())
        }

        self.selection = lowered(&self.selection);
        self.effective = lowered(&self.effective);
        self.allowed_evaluation_modes = self
            .allowed_evaluation_modes
            .iter()
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect();

        // A hybrid's inner models may be any registered backend except another
        // hybrid. When the engine has not published its list, any non-hybrid id
        // is accepted rather than none — an empty list means "unknown", not
        // "nothing is valid".
        let known: Vec<String> = self
            .available_backends
            .as_array()
            .map(|backends| {
                backends
                    .iter()
                    .filter_map(|backend| backend.get("id").and_then(|v| v.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let valid_inner = |id: &Option<String>| -> Option<String> {
            let id = lowered(id)?;
            if id == "hybrid" {
                return None;
            }
            if known.is_empty() || known.iter().any(|k| *k == id) {
                Some(id)
            } else {
                None
            }
        };
        self.hybrid.external_backend = valid_inner(&self.hybrid.external_backend);
        self.hybrid.internal_backend = valid_inner(&self.hybrid.internal_backend);

        // The curve is a blend weight against a normalised distance: a point
        // outside the unit square has no meaning, and a non-finite one would
        // poison every sample drawn from it.
        self.hybrid
            .curve
            .retain(|point| point[0].is_finite() && point[1].is_finite());
        for point in &mut self.hybrid.curve {
            point[0] = point[0].clamp(0.0, 1.0);
            point[1] = point[1].clamp(0.0, 1.0);
        }
        self.hybrid.curve_smoothing = self
            .hybrid
            .curve_smoothing
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 1.0));
        self.hybrid.metric = lowered(&self.hybrid.metric)
            .filter(|metric| matches!(metric.as_str(), "spherical" | "chebyshev"));
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RenderBackendState {
    pub selection: Option<String>,
    pub effective: Option<String>,
    #[serde(rename = "effectiveLabel", alias = "effective_label")]
    pub effective_label: Option<String>,
    pub capabilities: Option<BackendCapabilitiesState>,
    #[serde(rename = "allowedEvaluationModes", alias = "allowed_evaluation_modes")]
    pub allowed_evaluation_modes: Vec<String>,
    #[serde(rename = "frozenRoomRatio", alias = "frozen_room_ratio")]
    pub frozen_room_ratio: bool,
    #[serde(rename = "frozenSpeakers", alias = "frozen_speakers")]
    pub frozen_speakers: bool,
    #[serde(
        rename = "restoreBackendAvailable",
        alias = "restore_backend_available"
    )]
    pub restore_backend_available: bool,
    pub hybrid: HybridState,
    /// Selectable backends with their declared param schema: `[{ id, label,
    /// params: [...] }]`. Passed through verbatim so the UI can populate the
    /// backend dropdown and generate per-backend controls.
    #[serde(
        rename = "availableBackends",
        alias = "available_backends",
        default,
        skip_serializing_if = "serde_json::Value::is_null"
    )]
    pub available_backends: serde_json::Value,
    /// Host-set param values for every backend (`{ backendId: { key: value } }`),
    /// used to seed generated controls for a backend that is not the active
    /// selection (e.g. a hybrid inner-backend tab).
    #[serde(
        rename = "backendParamValuesById",
        alias = "backend_param_values_by_id",
        default,
        skip_serializing_if = "serde_json::Value::is_null"
    )]
    pub backend_param_values_by_id: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RenderEvaluationModeState {
    pub selection: Option<String>,
    pub effective: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct OutputDeviceOption {
    pub value: String,
    pub label: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RuntimeLatencyState {
    #[serde(rename = "latencyMs")]
    pub latency_ms: Option<i64>,
    #[serde(rename = "latencyInstantMs")]
    pub latency_instant_ms: Option<i64>,
    #[serde(rename = "latencyControlMs")]
    pub latency_control_ms: Option<i64>,
    #[serde(rename = "latencySmoothedMs")]
    pub latency_smoothed_ms: Option<f64>,
    #[serde(rename = "latencyDownstreamMs")]
    pub latency_downstream_ms: Option<i64>,
    #[serde(rename = "latencyTargetMs")]
    pub latency_target_ms: Option<i64>,
    #[serde(rename = "latencyRequestedMs")]
    pub latency_requested_ms: Option<i64>,
    /// Ring-buffer occupancy converted to ms — first component of control_available.
    #[serde(rename = "latencyAvailInputMs")]
    pub latency_avail_input_ms: Option<f64>,
    /// Output-FIFO content (input-domain) as ms — second component of control_available.
    #[serde(rename = "latencyOutputFifoMs")]
    pub latency_output_fifo_ms: Option<f64>,
    /// Resampler pending input as ms — third component of control_available.
    #[serde(rename = "latencyResamplerPendingMs")]
    pub latency_resampler_pending_ms: Option<f64>,
    /// Diagnostic-metric schema (list of registered metrics with labels/groups
    /// /units). Sent once on registry change; lets the Studio plot offer a
    /// dynamic multi-select of what to trace.
    #[serde(rename = "diagSchema")]
    pub diag_schema: Option<serde_json::Value>,
    /// Current values of every registered diag metric, keyed by name. Sent
    /// every meter-bundle tick; the Studio plot polls this map.
    #[serde(rename = "diagValues")]
    pub diag_values: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct RuntimeAudioState {
    #[serde(rename = "audioSampleRate")]
    pub audio_sample_rate: Option<u32>,
    #[serde(rename = "rampMode")]
    pub ramp_mode: Option<String>,
    #[serde(rename = "audioOutputDevice")]
    pub audio_output_device: Option<String>,
    #[serde(rename = "audioOutputDeviceEffective")]
    pub audio_output_device_effective: Option<String>,
    #[serde(rename = "audioOutputDevices")]
    pub audio_output_devices: Vec<OutputDeviceOption>,
    #[serde(rename = "audioOutputBackend")]
    pub audio_output_backend: Option<String>,
    #[serde(rename = "audioOutputFile")]
    pub audio_output_file: Option<String>,
    #[serde(rename = "audioOutputFileFormat")]
    pub audio_output_file_format: Option<String>,
    #[serde(rename = "audioSampleFormat")]
    pub audio_sample_format: Option<String>,
    #[serde(rename = "audioError")]
    pub audio_error: Option<String>,
    /// The host the engine's output stream plays through, as the engine
    /// names it: `ASIO`, `WASAPI (fallback: no ASIO driver)`, `CoreAudio`.
    /// `None` when no stream is open or the backend names no host.
    #[serde(rename = "audioOutputHost")]
    pub audio_output_host: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct LiveInputState {
    pub backend: Option<String>,
    pub node: Option<String>,
    pub description: Option<String>,
    pub layout: Option<String>,
    #[serde(rename = "clockMode")]
    pub clock_mode: Option<String>,
    pub channels: Option<u32>,
    #[serde(rename = "sampleRate")]
    pub sample_rate: Option<u32>,
    pub map: Option<String>,
    #[serde(rename = "lfeMode")]
    pub lfe_mode: Option<String>,
}

/// The fixed-channel-source companions that are NOT scalar registry options, mirrored
/// verbatim from the renderer's `/omniphony/state/renderer` domain (camelCase
/// keys). The scalar options themselves (surround placement, synthesized-object
/// master, generator ids/modes, output mapping) ride the generic
/// `options` passthrough on [`AppState`] instead — no typed mirror per option
/// (registry RFC phase 2). Grouped in one struct flattened into both the
/// domain parser and [`AppState`].
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LiveOptionsState {
    /// Stored param values of every object generator (`{ id: { key:
    /// value } }`), as `renderBackendState.backendParamValuesById` holds the
    /// backends'.
    pub object_generator_param_values_by_id: Option<serde_json::Value>,
    pub object_generator_layout_has_height: Option<bool>,
    /// Facts about the crossover bank the renderer actually built (engine,
    /// bands, cutoffs, FIR taps, latency). Passthrough JSON; annotates the
    /// crossover control.
    pub crossover: Option<serde_json::Value>,
    /// Stored param values of the phantom stage (`{ key: value }`).
    pub phantom_param_values: Option<serde_json::Value>,
    pub fixed_channel_catalog: Option<serde_json::Value>,
    pub fixed_channel_processing: Option<serde_json::Value>,
    pub output_channel_mapping_unroutable: Option<Vec<String>>,
    /// `None` serializes as an explicit `"virtualBed": null` (no skip): the UI
    /// distinguishes "renderer reports no saved bed" (null → it materialises
    /// the canonical bed once) from a configured bed object. Legacy mirror of
    /// the generic family's entries; `placement` is the real thing.
    pub virtual_bed: Option<serde_json::Value>,
    /// Per-family placement of fixed channels (`renderer::placement`), the
    /// renderer's `placement` block passed through: one object per family of
    /// its table, keyed by name, with its `label`, `defaultMode`, own `mode`
    /// and `layout` (null when inherited), `effectiveMode`, `modeSource`
    /// (absent from renderers older than the headphones default) and
    /// `layoutSource`. Read through `host::channels::family_placement`.
    pub placement: Option<serde_json::Value>,
    /// The families to offer, by name, in the renderer's order: the generic
    /// family, the loaded bridge's, the renderer's PCM input. Studio knows
    /// no family by name; these are what it shows (`host::channels::families`).
    pub placement_families: Option<Vec<String>>,
}

/// A tag the stream's bridge puts on some of its channels (`channelTags` on
/// `/state/input`): the dialogue a format codes apart from the rest.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelTag {
    /// `dialogue`, or a kind this Studio does not know yet.
    pub kind: String,
    /// BCP 47, empty when the stream states none.
    #[serde(default)]
    pub language: String,
    /// The stream's name for the channels, empty when it states none.
    #[serde(default)]
    pub label: String,
    /// Indices into the stream's channels.
    #[serde(default)]
    pub channels: Vec<u32>,
}

impl ChannelTag {
    pub const DIALOGUE: &'static str = "dialogue";
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppState {
    pub sources: HashMap<String, SourcePosition>,
    /// Binaural stage state forwarded verbatim from the renderer (output mode,
    /// HRIR source, head pose, tracking). Passthrough JSON so new sub-fields reach
    /// the UI without a typed mirror here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binaural: Option<serde_json::Value>,
    /// The loudspeakers of the BRIR set a headphone render pans onto in place
    /// of the editable layout (`binaural.brir.layout`), while it does. Set
    /// from the renderer state; read-only, the measurement fixes them.
    #[serde(skip)]
    pub brir_speakers: Option<Vec<super::layouts::Speaker>>,
    /// Declared live options (`options` block of `/state/renderer`, canonical
    /// snake_case keys straight from the renderer's registry). Passthrough
    /// JSON: a registry row needs no typed mirror here (registry RFC phase 1).
    /// The typed [`LiveOptionsState`] fields above remain the UI's consumers
    /// until the `data-option` binder (phase 2) reads this instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
    #[serde(rename = "sourceLevels")]
    pub source_levels: HashMap<String, Meter>,
    #[serde(rename = "speakerLevels")]
    pub speaker_levels: HashMap<String, Meter>,
    #[serde(rename = "masterLevel")]
    pub master_level: Option<Meter>,
    #[serde(rename = "objectSpeakerGains")]
    pub object_speaker_gains: HashMap<String, Vec<f64>>,
    #[serde(rename = "speakerGains")]
    pub speaker_gains: HashMap<String, f64>,
    #[serde(rename = "objectMutes")]
    pub object_mutes: HashMap<String, u8>,
    #[serde(rename = "speakerMutes")]
    pub speaker_mutes: HashMap<String, u8>,
    #[serde(rename = "roomRatio")]
    pub room_ratio: RoomRatio,
    pub spread: SpreadState,
    #[serde(rename = "loudness")]
    pub loudness: Option<u8>,
    #[serde(rename = "loudnessSource")]
    pub loudness_source: Option<f64>,
    #[serde(rename = "loudnessGain")]
    pub loudness_gain: Option<f64>,
    #[serde(rename = "masterGain")]
    pub master_gain: Option<f64>,
    #[serde(rename = "autoGain")]
    pub auto_gain: Option<bool>,
    #[serde(rename = "autoGainCeilingDb")]
    pub auto_gain_ceiling_db: Option<f64>,
    #[serde(rename = "distanceDiffuse")]
    pub distance_diffuse: DistanceDiffuse,
    #[serde(rename = "distanceModel")]
    pub distance_model: DistanceModelState,
    #[serde(rename = "vbapCartesian")]
    pub vbap_cartesian: VbapCartesian,
    #[serde(rename = "vbapPolar")]
    pub vbap_polar: VbapPolar,
    #[serde(rename = "renderBackendState")]
    pub render_backend_state: RenderBackendState,
    #[serde(rename = "renderEvaluationModeState")]
    pub render_evaluation_mode_state: RenderEvaluationModeState,
    /// Number of object-size intervals precomputed (0 = single table).
    #[serde(rename = "objectSizeIntervals")]
    pub object_size_intervals: u32,
    #[serde(rename = "vbapAllowNegativeZ")]
    pub vbap_allow_negative_z: Option<bool>,
    /// Where the evaluation grid comes from: `bridge` (the active bridge's
    /// hint) or `custom`. `None` from a renderer that predates the setting.
    #[serde(rename = "evaluationGrid")]
    pub evaluation_grid: Option<String>,
    /// The grid the active bridge hints, once the renderer knows it.
    #[serde(rename = "evaluationGridBridge")]
    pub evaluation_grid_bridge: Option<BridgeGrid>,
    #[serde(rename = "adaptiveResampling")]
    pub adaptive_resampling: Option<u8>,
    #[serde(rename = "adaptiveResamplingEnableFarMode")]
    pub adaptive_resampling_enable_far_mode: Option<u8>,
    #[serde(rename = "adaptiveResamplingForceSilenceInFarMode")]
    pub adaptive_resampling_force_silence_in_far_mode: Option<u8>,
    #[serde(rename = "adaptiveResamplingHardRecoverHighInFarMode")]
    pub adaptive_resampling_hard_recover_high_in_far_mode: Option<u8>,
    #[serde(rename = "adaptiveResamplingHardRecoverLowInFarMode")]
    pub adaptive_resampling_hard_recover_low_in_far_mode: Option<u8>,
    #[serde(rename = "adaptiveResamplingFarModeReturnFadeInMs")]
    pub adaptive_resampling_far_mode_return_fade_in_ms: Option<i64>,
    #[serde(rename = "adaptiveResamplingKpNear")]
    pub adaptive_resampling_kp_near: Option<f64>,
    #[serde(rename = "adaptiveResamplingKi")]
    pub adaptive_resampling_ki: Option<f64>,
    #[serde(rename = "adaptiveResamplingIntegralDischargeRatio")]
    pub adaptive_resampling_integral_discharge_ratio: Option<f64>,
    #[serde(rename = "adaptiveResamplingMaxAdjust")]
    pub adaptive_resampling_max_adjust: Option<f64>,
    #[serde(rename = "adaptiveResamplingUpdateIntervalCallbacks")]
    pub adaptive_resampling_update_interval_callbacks: Option<i64>,
    #[serde(rename = "adaptiveResamplingHighRecoverEntryMarginMs")]
    pub adaptive_resampling_high_recover_entry_margin_ms: Option<i64>,
    #[serde(rename = "adaptiveResamplingLowRecoverSettleStableMs")]
    pub adaptive_resampling_low_recover_settle_stable_ms: Option<f64>,
    #[serde(rename = "adaptiveResamplingLowRecoverEntryMarginMs")]
    pub adaptive_resampling_low_recover_entry_margin_ms: Option<f64>,
    #[serde(rename = "adaptiveResamplingLowRecoverExitMarginMs")]
    pub adaptive_resampling_low_recover_exit_margin_ms: Option<f64>,
    #[serde(rename = "adaptiveResamplingLowRecoverSettleMarginMs")]
    pub adaptive_resampling_low_recover_settle_margin_ms: Option<f64>,
    #[serde(rename = "adaptiveResamplingLowRecoverRefillDeltaAlpha")]
    pub adaptive_resampling_low_recover_refill_delta_alpha: Option<f64>,
    #[serde(rename = "adaptiveResamplingControlSmoothingCutoffHz")]
    pub adaptive_resampling_control_smoothing_cutoff_hz: Option<f64>,
    #[serde(rename = "adaptiveResamplingControlSmoothingOrder")]
    pub adaptive_resampling_control_smoothing_order: Option<u32>,
    #[serde(rename = "adaptiveResamplingBand")]
    pub adaptive_resampling_band: Option<String>,
    #[serde(rename = "adaptiveResamplingState")]
    pub adaptive_resampling_state: Option<String>,
    #[serde(rename = "adaptiveResamplingPaused")]
    pub adaptive_resampling_paused: Option<u8>,
    #[serde(rename = "adaptiveResamplingUsePreBridgeClock")]
    pub adaptive_resampling_use_pre_bridge_clock: Option<u8>,
    #[serde(rename = "adaptiveResamplingUseOutputPacing")]
    pub adaptive_resampling_use_output_pacing: Option<u8>,
    #[serde(rename = "adaptiveResamplingDisableBackpressure")]
    pub adaptive_resampling_disable_backpressure: Option<u8>,
    #[serde(rename = "vbapRecomputing")]
    pub vbap_recomputing: Option<bool>,
    #[serde(rename = "recomputeError", skip_serializing_if = "Option::is_none")]
    pub recompute_error: Option<String>,
    #[serde(rename = "saveError", skip_serializing_if = "Option::is_none")]
    pub save_error: Option<String>,
    #[serde(rename = "configSaved")]
    pub config_saved: Option<u8>,
    #[serde(flatten)]
    pub latency: RuntimeLatencyState,
    #[serde(rename = "decodeTimeMs")]
    pub decode_time_ms: Option<f64>,
    #[serde(rename = "renderTimeMs")]
    pub render_time_ms: Option<f64>,
    #[serde(rename = "crossoverTimeMs")]
    pub crossover_time_ms: Option<f64>,
    #[serde(rename = "writeTimeMs")]
    pub write_time_ms: Option<f64>,
    #[serde(rename = "frameDurationMs")]
    pub frame_duration_ms: Option<f64>,
    #[serde(rename = "resampleRatio")]
    pub resample_ratio: Option<f64>,
    #[serde(flatten)]
    pub audio: RuntimeAudioState,
    #[serde(flatten)]
    pub live_options: LiveOptionsState,
    #[serde(rename = "inputMode")]
    pub input_mode: Option<String>,
    #[serde(rename = "inputActiveMode")]
    pub input_active_mode: Option<String>,
    #[serde(rename = "inputApplyPending")]
    pub input_apply_pending: Option<u8>,
    #[serde(rename = "drcMode")]
    pub drc_mode: Option<String>,
    #[serde(rename = "drcWeight")]
    pub drc_weight: Option<f32>,
    #[serde(rename = "meterRateHz")]
    pub meter_rate_hz: Option<f32>,
    #[serde(rename = "diagRateHz")]
    pub diag_rate_hz: Option<f32>,
    #[serde(rename = "supportedDrcModes")]
    pub supported_drc_modes: Vec<String>,
    /// What the current stream tags among its channels.
    #[serde(rename = "channelTags")]
    pub channel_tags: Vec<ChannelTag>,
    #[serde(rename = "inputBackend")]
    pub input_backend: Option<String>,
    #[serde(rename = "inputChannels")]
    pub input_channels: Option<u32>,
    #[serde(rename = "inputSampleRate")]
    pub input_sample_rate: Option<u32>,
    #[serde(rename = "inputNode")]
    pub input_node: Option<String>,
    #[serde(rename = "inputDescription")]
    pub input_description: Option<String>,
    #[serde(rename = "inputStreamFormat")]
    pub input_stream_format: Option<String>,
    #[serde(rename = "inputError")]
    pub input_error: Option<String>,
    #[serde(rename = "renderBridgePath")]
    pub render_bridge_path: Option<String>,
    #[serde(rename = "renderConfigPath")]
    pub render_config_path: Option<String>,
    #[serde(rename = "renderConfigStatus")]
    pub render_config_status: Option<String>,
    #[serde(rename = "renderVersion")]
    pub render_version: Option<String>,
    #[serde(rename = "renderExecutable")]
    pub render_executable: Option<String>,
    #[serde(rename = "renderAbi")]
    pub render_abi: Option<String>,
    /// The `bridge_api` version the renderer loads bridges of.
    #[serde(rename = "renderBridgeApi")]
    pub render_bridge_api: Option<String>,
    /// Named config profiles (`/omniphony/state/profiles`): the active profile
    /// name and the full name list, mirrored verbatim from the renderer.
    #[serde(rename = "activeProfile")]
    pub active_profile: Option<String>,
    #[serde(rename = "profileNames")]
    pub profile_names: Vec<String>,
    #[serde(rename = "renderBridgeError")]
    pub render_bridge_error: Option<String>,
    /// The decoder bridges (`/omniphony/state/render/bridges`): the paths
    /// asked for, then each bridge loaded or failed.
    #[serde(rename = "renderBridges")]
    pub render_bridges: Option<RenderBridges>,
    /// The bridge list was edited from this Studio since the last restart it
    /// sent: the only sign of a pending change once the list is emptied.
    #[serde(skip)]
    pub render_bridges_edited: bool,
    #[serde(rename = "liveInput")]
    pub live_input: LiveInputState,
    #[serde(rename = "orenderInputPipe")]
    pub orender_input_pipe: Option<String>,
    #[serde(rename = "producerCapabilities")]
    pub producer_capabilities: Option<serde_json::Value>,
    #[serde(rename = "producerSession")]
    pub producer_session: Option<serde_json::Value>,
    /// Instance epoch from the last `/omniphony/heartbeat/ack`. A change means a
    /// different renderer instance now answers on the RX port (a CLI⇄mpv swap)
    /// → force a re-handshake. Internal detection state, never sent to the UI,
    /// and cleared by `reset_runtime_state` so it re-latches on each connection.
    #[serde(skip)]
    pub producer_epoch: Option<i32>,
    #[serde(rename = "oscMeteringEnabled")]
    pub osc_metering_enabled: Option<u8>,
    #[serde(rename = "logLevel")]
    pub log_level: Option<String>,
    #[serde(rename = "lastSpatialSamplePos")]
    pub last_spatial_sample_pos: Option<i64>,
    #[serde(skip)]
    pub current_content_generation: Option<u64>,
    #[serde(skip)]
    pub object_band_gains: HashMap<String, Vec<Vec<f64>>>,
    #[serde(rename = "currentCoordinateFormat")]
    pub current_coordinate_format: u8,
    pub layouts: Vec<Layout>,
    #[serde(rename = "selectedLayoutKey")]
    pub selected_layout_key: Option<String>,
    #[serde(rename = "oscSnapshotReady")]
    pub osc_snapshot_ready: bool,
    /// Hash of the last `state/layout` JSON applied, so we can dedup repeated
    /// broadcasts that carry identical content (the renderer re-broadcasts the
    /// full layout on apply and again post-recompute).
    #[serde(skip)]
    pub last_layout_state_hash: Option<u64>,
    /// Hash of the last `state:snapshot_ready` payload actually emitted to the
    /// webview. The domain appliers merge partial payloads and cannot cheaply
    /// tell "already at these values" from "changed" (the renderer even
    /// alternates two `/state/input` documents in one bundle), so the dedup
    /// happens here, on the one thing that matters: the bytes the webview
    /// would receive. An identical snapshot is not re-emitted — each emit
    /// triggers a full `applyInitState` UI rebuild on the other side.
    #[serde(skip)]
    pub last_snapshot_emit_hash: Option<u64>,
    /// Same guard for `overlay:state`: the payload is small but each emit makes
    /// the frontend re-adopt the display prefs and re-render the switches.
    #[serde(skip)]
    pub last_overlay_emit_hash: Option<u64>,
}

/// Why the connected renderer will not write its configuration file, as its
/// `render/config_status` says.
/// The engine and this Studio speak different revisions of the OSC contract
/// (osc-contract `CONTRACT_REVISION`): controls one side does not know are
/// refused, and state it does not know is dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContractMismatch {
    pub engine: u32,
    pub studio: u32,
}

impl ContractMismatch {
    pub fn engine_is_older(self) -> bool {
        self.engine < self.studio
    }
}

/// What is wrong with the connected renderer's decoder bridge, as its
/// `render/bridge_error` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeProblemKind {
    /// No bridge was asked for and auto-discovery found none
    /// (osc-contract `BRIDGE_ERROR_NONE_FOUND`). The renderer runs without a
    /// decoder: PCM and channel input still work. A degraded but normal state,
    /// the usual one for Studio's standby renderer while the player, which has
    /// its own bridge, plays films.
    NoDecoder,
    /// A bridge was asked for (`render.bridge_path`, `--bridge-path`) or
    /// found, and could not be loaded: a wrong path, a mismatched release, a
    /// file that is no bridge. Something to fix.
    LoadFailed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeProblem {
    pub kind: BridgeProblemKind,
    /// The engine's own report, trimmed: what it searched, what it refused.
    pub report: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigRefusal {
    /// `parse_error`: the file failed to parse, and the renderer runs on its
    /// built-in defaults.
    ParseError,
    /// `newer_schema`: a newer build wrote the file, and the renderer runs on
    /// what it understands of it.
    NewerSchema,
}

impl AppState {
    /// Whether the speakers shown cannot be edited: the backend froze them,
    /// or they are a BRIR set's own loudspeakers.
    /// The path the render takes to the output, from the binaural document
    /// and the loudspeakers in use (`model::binaural::RenderPath`).
    pub fn render_path(&self) -> super::binaural::RenderPath {
        super::binaural::RenderPath::of(self.binaural.as_ref(), self.brir_speakers.is_some())
    }

    /// The frame the scene draws in and the editors convert through: the
    /// live room on every path through the speaker stage, the unit room on
    /// the direct binaural path, which reads a direction straight off a
    /// position (`RenderPath::warps_with_room`). One resolution for the
    /// forward projection, the gizmos' inverse, the channel editor's polar
    /// conversions and the heatmap volumes, so a drag lands where the
    /// pointer is and a volume sits on its sources whatever the path.
    pub fn display_room(&self) -> RoomRatio {
        if self.render_path().warps_with_room() {
            self.room_ratio.clone()
        } else {
            RoomRatio::unit(self.binaural_unit_scale_m())
        }
    }

    /// Metres to one unit of the direct binaural path's cube
    /// (`binaural.unitScaleM`, the renderer's distance scale).
    pub fn binaural_unit_scale_m(&self) -> f64 {
        self.binaural
            .as_ref()
            .and_then(|b| b.get("unitScaleM"))
            .and_then(serde_json::Value::as_f64)
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(1.0)
    }

    pub fn speakers_read_only(&self) -> bool {
        self.render_backend_state.frozen_speakers || self.brir_speakers.is_some()
    }

    /// The renderer's decoder bridge problem, if it reported one. An engine
    /// predating the "none found" marker reports every problem as a failure.
    pub fn bridge_problem(&self) -> Option<BridgeProblem> {
        let report = self.render_bridge_error.as_deref()?.trim();
        if report.is_empty() {
            return None;
        }
        let kind = if report.contains(crate::osc_contract::BRIDGE_ERROR_NONE_FOUND) {
            BridgeProblemKind::NoDecoder
        } else {
            BridgeProblemKind::LoadFailed
        };
        Some(BridgeProblem {
            kind,
            report: report.to_owned(),
        })
    }

    /// Why the renderer will not write its configuration file, if it said so.
    /// A Reload that reads the file publishes the status again, which lifts
    /// it.
    pub fn config_refusal(&self) -> Option<ConfigRefusal> {
        match self.render_config_status.as_deref()? {
            "parse_error" => Some(ConfigRefusal::ParseError),
            "newer_schema" => Some(ConfigRefusal::NewerSchema),
            _ => None,
        }
    }

    /// The engine's OSC contract revision against this build's, when they
    /// differ. An engine that advertises none predates revisions and counts
    /// as 0. `None` until the capabilities arrive.
    pub fn contract_mismatch(&self) -> Option<ContractMismatch> {
        let caps = self.producer_capabilities.as_ref()?;
        let engine = caps
            .get("contractRevision")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |revision| u32::try_from(revision).unwrap_or(u32::MAX));
        let studio = crate::osc_contract::CONTRACT_REVISION;
        (engine != studio).then_some(ContractMismatch { engine, studio })
    }

    /// The current stream's dialogue tag, when it codes its dialogue apart
    /// (the dialogue level only means something then).
    pub fn dialogue_tag(&self) -> Option<&ChannelTag> {
        self.channel_tags
            .iter()
            .find(|tag| tag.kind == ChannelTag::DIALOGUE)
    }

    pub fn new(layouts: Vec<Layout>) -> Self {
        Self {
            layouts,
            selected_layout_key: None,
            room_ratio: RoomRatio {
                width: 1.0,
                length: 2.0,
                height: 1.0,
                rear: 1.0,
                lower: 0.5,
                center_blend: 0.5,
                scale_m: 1.0,
            },
            ..Default::default()
        }
    }

    pub fn reset_runtime_state(&mut self) {
        let layouts = std::mem::take(&mut self.layouts);
        let selected_layout_key = self.selected_layout_key.clone();
        let osc_metering_enabled = self.osc_metering_enabled;
        let log_level = self.log_level.clone();

        *self = Self::new(layouts);
        self.selected_layout_key = selected_layout_key;
        self.osc_metering_enabled = osc_metering_enabled;
        self.log_level = log_level;
    }

    pub fn set_latency_value(&mut self, value: f64) -> i64 {
        let rounded = value.round() as i64;
        self.latency.latency_ms = Some(rounded);
        rounded
    }

    pub fn set_latency_instant_value(&mut self, value: f64) -> i64 {
        let rounded = value.round() as i64;
        self.latency.latency_instant_ms = Some(rounded);
        rounded
    }

    pub fn set_latency_control_value(&mut self, value: f64) -> i64 {
        let rounded = value.round() as i64;
        self.latency.latency_control_ms = Some(rounded);
        rounded
    }

    pub fn set_latency_smoothed_value(&mut self, value: f64) -> f64 {
        self.latency.latency_smoothed_ms = Some(value);
        value
    }

    pub fn set_latency_downstream_value(&mut self, value: f64) -> i64 {
        let rounded = value.round() as i64;
        self.latency.latency_downstream_ms = Some(rounded);
        rounded
    }

    pub fn set_latency_target_value(&mut self, value: f64) -> i64 {
        let rounded = value.round() as i64;
        self.latency.latency_target_ms = Some(rounded);
        rounded
    }

    pub fn set_latency_requested_value(&mut self, value: f64) -> i64 {
        let rounded = value.round() as i64;
        self.latency.latency_requested_ms = Some(rounded);
        rounded
    }

    pub fn set_latency_avail_input_value(&mut self, value: f64) -> f64 {
        self.latency.latency_avail_input_ms = Some(value);
        value
    }

    pub fn set_latency_output_fifo_value(&mut self, value: f64) -> f64 {
        self.latency.latency_output_fifo_ms = Some(value);
        value
    }

    pub fn set_latency_resampler_pending_value(&mut self, value: f64) -> f64 {
        self.latency.latency_resampler_pending_ms = Some(value);
        value
    }

    pub fn set_audio_sample_rate_value(&mut self, value: u32) -> Option<u32> {
        self.audio.audio_sample_rate = if value == 0 { None } else { Some(value) };
        self.audio.audio_sample_rate
    }

    pub fn set_audio_requested_output_device(&mut self, value: &str) -> Option<String> {
        self.audio.audio_output_device = if value.trim().is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        self.audio.audio_output_device.clone()
    }

    pub fn set_audio_effective_output_device(&mut self, value: &str) -> Option<String> {
        self.audio.audio_output_device_effective = if value.trim().is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        self.audio.audio_output_device_effective.clone()
    }

    pub fn set_audio_output_devices(&mut self, devices: Vec<OutputDeviceOption>) {
        self.audio.audio_output_devices = devices;
    }

    pub fn set_audio_output_backend(&mut self, value: Option<String>) {
        self.audio.audio_output_backend = value.filter(|v| !v.trim().is_empty());
    }

    pub fn set_audio_output_file(&mut self, value: Option<String>) {
        self.audio.audio_output_file = value.filter(|v| !v.trim().is_empty());
    }

    pub fn set_audio_output_file_format(&mut self, value: Option<String>) {
        self.audio.audio_output_file_format = value.filter(|v| !v.trim().is_empty());
    }

    pub fn set_audio_sample_format(&mut self, value: String) {
        self.audio.audio_sample_format = Some(value);
    }

    pub fn set_audio_output_host(&mut self, value: &str) {
        self.audio.audio_output_host = if value.trim().is_empty() {
            None
        } else {
            Some(value.to_string())
        };
    }

    pub fn set_audio_error(&mut self, value: &str) -> Option<String> {
        self.audio.audio_error = if value.trim().is_empty() {
            None
        } else {
            Some(value.to_string())
        };
        self.audio.audio_error.clone()
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            sources: HashMap::new(),
            source_levels: HashMap::new(),
            speaker_levels: HashMap::new(),
            master_level: None,
            object_speaker_gains: HashMap::new(),
            speaker_gains: HashMap::new(),
            object_mutes: HashMap::new(),
            speaker_mutes: HashMap::new(),
            room_ratio: RoomRatio::default(),
            spread: SpreadState::default(),
            loudness: None,
            loudness_source: None,
            loudness_gain: None,
            master_gain: None,
            auto_gain: None,
            auto_gain_ceiling_db: None,
            distance_diffuse: DistanceDiffuse::default(),
            distance_model: DistanceModelState::default(),
            vbap_cartesian: VbapCartesian::default(),
            vbap_polar: VbapPolar::default(),
            render_backend_state: RenderBackendState::default(),
            render_evaluation_mode_state: RenderEvaluationModeState::default(),
            object_size_intervals: 0,
            binaural: None,
            brir_speakers: None,
            options: None,
            vbap_allow_negative_z: None,
            evaluation_grid: None,
            evaluation_grid_bridge: None,
            adaptive_resampling: Some(0),
            adaptive_resampling_enable_far_mode: Some(1),
            adaptive_resampling_force_silence_in_far_mode: Some(1),
            adaptive_resampling_hard_recover_high_in_far_mode: Some(1),
            adaptive_resampling_hard_recover_low_in_far_mode: Some(0),
            adaptive_resampling_far_mode_return_fade_in_ms: Some(500),
            adaptive_resampling_kp_near: Some(1.0),
            adaptive_resampling_ki: Some(1.0),
            adaptive_resampling_integral_discharge_ratio: Some(0.25),
            adaptive_resampling_max_adjust: Some(0.01),
            adaptive_resampling_update_interval_callbacks: Some(1),
            adaptive_resampling_high_recover_entry_margin_ms: Some(1000),
            adaptive_resampling_low_recover_settle_stable_ms: Some(200.0),
            adaptive_resampling_low_recover_entry_margin_ms: Some(18.0),
            adaptive_resampling_low_recover_exit_margin_ms: Some(6.0),
            adaptive_resampling_low_recover_settle_margin_ms: Some(6.0),
            adaptive_resampling_low_recover_refill_delta_alpha: Some(0.5),
            adaptive_resampling_control_smoothing_cutoff_hz: Some(0.5),
            adaptive_resampling_control_smoothing_order: Some(1),
            adaptive_resampling_band: None,
            adaptive_resampling_state: None,
            adaptive_resampling_paused: Some(0),
            adaptive_resampling_use_pre_bridge_clock: Some(0),
            adaptive_resampling_use_output_pacing: Some(0),
            adaptive_resampling_disable_backpressure: Some(0),
            vbap_recomputing: None,
            recompute_error: None,
            save_error: None,
            config_saved: None,
            latency: RuntimeLatencyState::default(),
            decode_time_ms: None,
            render_time_ms: None,
            crossover_time_ms: None,
            write_time_ms: None,
            frame_duration_ms: None,
            resample_ratio: None,
            audio: RuntimeAudioState {
                ramp_mode: Some("sample".to_string()),
                ..RuntimeAudioState::default()
            },
            live_options: LiveOptionsState::default(),
            input_mode: Some("pipe_bridge".to_string()),
            input_active_mode: Some("pipe_bridge".to_string()),
            input_apply_pending: Some(0),
            drc_mode: None,
            drc_weight: Some(1.0),
            meter_rate_hz: None,
            diag_rate_hz: None,
            supported_drc_modes: Vec::new(),
            channel_tags: Vec::new(),
            input_backend: None,
            input_channels: None,
            input_sample_rate: None,
            input_node: None,
            input_description: None,
            input_stream_format: None,
            input_error: None,
            render_bridge_path: None,
            render_config_path: None,
            render_config_status: None,
            render_version: None,
            render_executable: None,
            render_abi: None,
            render_bridge_api: None,
            active_profile: None,
            profile_names: Vec::new(),
            render_bridge_error: None,
            render_bridges: None,
            render_bridges_edited: false,
            live_input: LiveInputState::default(),
            orender_input_pipe: None,
            producer_capabilities: None,
            producer_session: None,
            producer_epoch: None,
            osc_metering_enabled: Some(0),
            log_level: Some("info".to_string()),
            last_spatial_sample_pos: None,
            current_content_generation: None,
            object_band_gains: HashMap::new(),
            current_coordinate_format: 0,
            layouts: Vec::new(),
            selected_layout_key: None,
            osc_snapshot_ready: false,
            last_layout_state_hash: None,
            last_snapshot_emit_hash: None,
            last_overlay_emit_hash: None,
        }
    }
}

#[cfg(test)]
mod validation_tests {
    use super::RenderBackendState;

    fn with_backends(ids: &[&str]) -> RenderBackendState {
        RenderBackendState {
            available_backends: serde_json::json!(
                ids.iter()
                    .map(|id| serde_json::json!({ "id": id }))
                    .collect::<Vec<_>>()
            ),
            ..Default::default()
        }
    }

    #[test]
    fn ids_are_trimmed_and_lowercased() {
        let mut state = RenderBackendState {
            selection: Some("  VBAP  ".to_string()),
            effective: Some("Hybrid".to_string()),
            ..Default::default()
        };
        state.sanitize();
        assert_eq!(state.selection.as_deref(), Some("vbap"));
        assert_eq!(state.effective.as_deref(), Some("hybrid"));
    }

    #[test]
    fn a_blank_id_becomes_none_rather_than_an_empty_string() {
        let mut state = RenderBackendState {
            selection: Some("   ".to_string()),
            ..Default::default()
        };
        state.sanitize();
        assert_eq!(state.selection, None);
    }

    /// The acceptance case: an id from a backend that is no longer registered.
    #[test]
    fn an_unknown_inner_backend_is_dropped() {
        let mut state = with_backends(&["vbap", "cavern"]);
        state.hybrid.external_backend = Some("gone".to_string());
        state.hybrid.internal_backend = Some("vbap".to_string());
        state.sanitize();
        assert_eq!(state.hybrid.external_backend, None);
        assert_eq!(state.hybrid.internal_backend.as_deref(), Some("vbap"));
    }

    /// A hybrid cannot nest inside itself, whatever the registry says.
    #[test]
    fn a_hybrid_cannot_be_its_own_inner_backend() {
        let mut state = with_backends(&["vbap", "hybrid"]);
        state.hybrid.external_backend = Some("hybrid".to_string());
        state.sanitize();
        assert_eq!(state.hybrid.external_backend, None);
    }

    /// An empty registry means "not published yet", not "nothing is valid" —
    /// rejecting everything there would blank a working hybrid config.
    #[test]
    fn an_unpublished_registry_accepts_any_non_hybrid_id() {
        let mut state = RenderBackendState::default();
        state.hybrid.external_backend = Some("vbap".to_string());
        state.hybrid.internal_backend = Some("hybrid".to_string());
        state.sanitize();
        assert_eq!(state.hybrid.external_backend.as_deref(), Some("vbap"));
        assert_eq!(state.hybrid.internal_backend, None);
    }

    /// The other acceptance case: a curve point outside the unit square.
    #[test]
    fn curve_points_are_clamped_into_the_unit_square() {
        let mut state = RenderBackendState::default();
        state.hybrid.curve = vec![[-0.5, 2.0], [0.5, 0.5], [3.0, -1.0]];
        state.sanitize();
        assert_eq!(state.hybrid.curve, vec![[0.0, 1.0], [0.5, 0.5], [1.0, 0.0]]);
    }

    /// A non-finite point cannot be clamped into anything meaningful, so it is
    /// removed — leaving it would poison every sample drawn from the curve.
    #[test]
    fn non_finite_curve_points_are_removed_not_clamped() {
        let mut state = RenderBackendState::default();
        state.hybrid.curve = vec![
            [0.0, 0.0],
            [f64::NAN, 0.5],
            [1.0, f64::INFINITY],
            [1.0, 1.0],
        ];
        state.sanitize();
        assert_eq!(state.hybrid.curve, vec![[0.0, 0.0], [1.0, 1.0]]);
    }

    #[test]
    fn smoothing_is_clamped_and_non_finite_is_dropped() {
        let mut state = RenderBackendState::default();
        state.hybrid.curve_smoothing = Some(5.0);
        state.sanitize();
        assert_eq!(state.hybrid.curve_smoothing, Some(1.0));

        state.hybrid.curve_smoothing = Some(f64::NAN);
        state.sanitize();
        assert_eq!(state.hybrid.curve_smoothing, None);
    }

    #[test]
    fn only_implemented_metrics_survive() {
        let mut state = RenderBackendState::default();
        state.hybrid.metric = Some("Chebyshev".to_string());
        state.sanitize();
        assert_eq!(state.hybrid.metric.as_deref(), Some("chebyshev"));

        state.hybrid.metric = Some("manhattan".to_string());
        state.sanitize();
        assert_eq!(state.hybrid.metric, None);
    }

    #[test]
    fn evaluation_modes_are_normalised_and_blanks_dropped() {
        let mut state = RenderBackendState {
            allowed_evaluation_modes: vec![
                " Realtime ".to_string(),
                String::new(),
                "PRECOMPUTED_POLAR".to_string(),
            ],
            ..Default::default()
        };
        state.sanitize();
        assert_eq!(
            state.allowed_evaluation_modes,
            vec!["realtime".to_string(), "precomputed_polar".to_string()]
        );
    }

    /// Sanitizing an already-clean state must not change it.
    #[test]
    fn sanitize_is_idempotent() {
        let mut state = with_backends(&["vbap"]);
        state.selection = Some("vbap".to_string());
        state.hybrid.external_backend = Some("vbap".to_string());
        state.hybrid.curve = vec![[0.0, 0.0], [1.0, 1.0]];
        state.hybrid.curve_smoothing = Some(0.5);
        state.hybrid.metric = Some("spherical".to_string());
        state.sanitize();
        let once = state.clone();
        state.sanitize();
        assert_eq!(format!("{once:?}"), format!("{state:?}"));
    }
}

#[cfg(test)]
mod mirror_axes_tests {
    use super::{DistanceDiffuse, MirrorAxes};

    #[test]
    fn the_flips_name_their_symmetry_and_spell_the_renderer_argument() {
        let axes = |x, y, z| MirrorAxes { x, y, z };
        assert_eq!(
            MirrorAxes::default().symmetry_key(),
            "distance.symmetry.axisZ"
        );
        assert_eq!(MirrorAxes::default().to_arg(), "xy");
        assert_eq!(axes(false, false, false).to_arg(), "none");
        assert_eq!(
            axes(false, false, false).symmetry_key(),
            "distance.symmetry.none"
        );
        assert_eq!(
            axes(false, true, false).symmetry_key(),
            "distance.symmetry.planeY"
        );
        assert_eq!(axes(true, true, true).to_arg(), "xyz");
        assert_eq!(
            axes(true, true, true).symmetry_key(),
            "distance.symmetry.origin"
        );
    }

    #[test]
    fn the_snapshot_carries_the_mirror_axes() {
        let parsed: DistanceDiffuse = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "mirrorAxes": { "x": false, "y": true, "z": true }
        }))
        .unwrap();
        assert_eq!(
            parsed.mirror_axes,
            Some(MirrorAxes {
                x: false,
                y: true,
                z: true
            })
        );
    }
}

#[cfg(test)]
mod contract_mismatch_tests {
    use super::{AppState, ContractMismatch};
    use crate::osc_contract::CONTRACT_REVISION;
    use serde_json::json;

    fn with_caps(caps: Option<serde_json::Value>) -> AppState {
        let mut app = AppState::new(Vec::new());
        app.producer_capabilities = caps;
        app
    }

    #[test]
    fn the_same_revision_is_no_mismatch() {
        let app = with_caps(Some(json!({ "contractRevision": CONTRACT_REVISION })));
        assert_eq!(app.contract_mismatch(), None);
    }

    #[test]
    fn an_engine_that_advertises_none_is_revision_zero() {
        let mismatch = with_caps(Some(json!({ "variant": "embedded" })))
            .contract_mismatch()
            .expect("an engine from before revisions differs");
        assert_eq!(
            mismatch,
            ContractMismatch {
                engine: 0,
                studio: CONTRACT_REVISION
            }
        );
        assert!(mismatch.engine_is_older());
    }

    #[test]
    fn a_newer_engine_is_reported_newer() {
        let app = with_caps(Some(json!({ "contractRevision": CONTRACT_REVISION + 1 })));
        assert!(!app.contract_mismatch().unwrap().engine_is_older());
    }

    #[test]
    fn nothing_is_said_before_the_capabilities_arrive() {
        assert_eq!(with_caps(None).contract_mismatch(), None);
    }
}

#[cfg(test)]
mod bridge_problem_tests {
    use super::{AppState, BridgeProblemKind};
    use crate::osc_contract::BRIDGE_ERROR_NONE_FOUND;

    fn with_error(error: Option<&str>) -> AppState {
        let mut app = AppState::new(Vec::new());
        app.render_bridge_error = error.map(str::to_owned);
        app
    }

    /// What the CLI publishes when nothing was asked for and nothing found:
    /// its own context in front of the engine's marker.
    #[test]
    fn nothing_found_is_no_decoder_not_a_failure() {
        let error = format!(
            "format bridge unavailable: {BRIDGE_ERROR_NONE_FOUND}: none requested \
             (no explicit path, no render.bridge_path) and none in the \
             auto-discovery directories: No bridge plugin found."
        );
        let problem = with_error(Some(&error)).bridge_problem().unwrap();
        assert_eq!(problem.kind, BridgeProblemKind::NoDecoder);
        assert_eq!(problem.report, error);
    }

    #[test]
    fn a_requested_bridge_that_does_not_load_is_a_failure() {
        for error in [
            "format bridge unavailable: render.bridge_path '/x/libh_bridge.so' \
             (from config) does not exist or is not a file.",
            "Failed to load bridge plugin from /x/libh_bridge.so: bridge_api 0.4.0",
            // An engine from before the marker: its "none found" text included.
            "no decoder bridge requested (no explicit path, no render.bridge_path) \
             and none found by auto-discovery",
        ] {
            let problem = with_error(Some(error)).bridge_problem().unwrap();
            assert_eq!(problem.kind, BridgeProblemKind::LoadFailed, "{error}");
        }
    }

    #[test]
    fn no_error_or_a_blank_one_is_no_problem() {
        assert_eq!(with_error(None).bridge_problem(), None);
        assert_eq!(with_error(Some("  \n")).bridge_problem(), None);
    }

    /// The published report is shown trimmed, as the banner did before.
    #[test]
    fn the_report_is_trimmed() {
        let error = format!("  {BRIDGE_ERROR_NONE_FOUND}: none requested\n");
        let problem = with_error(Some(&error)).bridge_problem().unwrap();
        assert_eq!(
            problem.report,
            format!("{BRIDGE_ERROR_NONE_FOUND}: none requested")
        );
    }
}

/// The renderer's decoder bridges (`/omniphony/state/render/bridges`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderBridges {
    /// The paths asked for, in load order; empty for auto-discovery.
    #[serde(default)]
    pub requested: Vec<String>,
    /// Each bridge loaded, then each one that failed.
    #[serde(default)]
    pub bridges: Vec<RenderBridge>,
}

/// One decoder bridge as the renderer reports it: loaded, with the source
/// families it declares, or failed, with why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderBridge {
    pub path: String,
    #[serde(default)]
    pub families: Vec<String>,
    #[serde(default)]
    pub error: Option<String>,
}
