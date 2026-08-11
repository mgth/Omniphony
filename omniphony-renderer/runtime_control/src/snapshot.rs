use std::sync::Arc;

use renderer::live_params::{LiveParams, RenderTopology, RendererControl};
use rosc::{OscMessage, OscPacket, OscType};
use serde::Serialize;
use serde_json::json;
// audio output/input + their OSC dispatch are owned by the host_audio crate
// (registered via runtime_control::HostControlHandler); this audio-free core
// no longer references audio_output/audio_input so liborender cross-compiles
// without cpal/pipewire/asio.

#[derive(Debug, Clone, Serialize)]
pub struct HybridOptionsSnapshot {
    pub external_backend: String,
    pub internal_backend: String,
    pub curve: Vec<[f32; 2]>,
    pub curve_smoothing: f32,
    pub metric: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderBackendStateSnapshot {
    pub selection: String,
    pub effective: String,
    pub effective_label: String,
    /// Every selectable backend (built-in + host-registered) with its param
    /// schema, for the UI list and control generation.
    pub available_backends: Vec<renderer::backend_registry::BackendListing>,
    /// Host-set param values for *every* backend, keyed by backend id then param
    /// key. The UI reads each backend's values from here, including an inner
    /// backend (e.g. the hybrid barycenter tab) that is not the active selection.
    pub backend_param_values_by_id: std::collections::HashMap<
        String,
        std::collections::HashMap<String, renderer::backend_params::ParamValue>,
    >,
    pub capabilities: renderer::render_backend::BackendCapabilities,
    pub allowed_evaluation_modes: Vec<String>,
    pub frozen_room_ratio: bool,
    pub frozen_speakers: bool,
    pub restore_backend_available: bool,
    pub hybrid: HybridOptionsSnapshot,
}

fn allowed_evaluation_modes(
    backend: &renderer::render_backend::PreparedRenderEngine,
    capabilities: renderer::render_backend::BackendCapabilities,
) -> Vec<String> {
    let _ = backend;
    let mut modes = vec!["auto".to_string()];
    if capabilities.supports_realtime {
        modes.push("realtime".to_string());
    }
    if capabilities.supports_precomputed_polar {
        modes.push("precomputed_polar".to_string());
    }
    if capabilities.supports_precomputed_cartesian {
        modes.push("precomputed_cartesian".to_string());
    }
    modes
}

pub fn build_render_backend_state_snapshot(
    live: &LiveParams,
    active_topology: &RenderTopology,
    available_backends: Vec<renderer::backend_registry::BackendListing>,
    backend_param_values_by_id: std::collections::HashMap<
        String,
        std::collections::HashMap<String, renderer::backend_params::ParamValue>,
    >,
) -> RenderBackendStateSnapshot {
    let backend = &active_topology.backend;
    let capabilities = backend.capabilities();

    RenderBackendStateSnapshot {
        selection: live.backend_id().to_string(),
        effective: backend.backend_id().to_string(),
        effective_label: backend.backend_label().to_string(),
        available_backends,
        backend_param_values_by_id,
        capabilities,
        allowed_evaluation_modes: allowed_evaluation_modes(backend, capabilities),
        frozen_room_ratio: false,
        frozen_speakers: false,
        restore_backend_available: false,
        hybrid: HybridOptionsSnapshot {
            external_backend: live.hybrid.external_backend_id.clone(),
            internal_backend: live.hybrid.internal_backend_id.clone(),
            curve: live.hybrid.curve.clone(),
            curve_smoothing: live.hybrid.curve_smoothing,
            metric: live.hybrid.metric.to_string(),
        },
    }
}

pub fn build_render_backend_state_json(
    live: &LiveParams,
    active_topology: &RenderTopology,
    available_backends: Vec<renderer::backend_registry::BackendListing>,
    backend_param_values_by_id: std::collections::HashMap<
        String,
        std::collections::HashMap<String, renderer::backend_params::ParamValue>,
    >,
) -> String {
    serde_json::to_string(&build_render_backend_state_snapshot(
        live,
        active_topology,
        available_backends,
        backend_param_values_by_id,
    ))
    .unwrap_or_else(|_| "{}".to_string())
}

pub fn build_renderer_state_json(
    live: &LiveParams,
    active_topology: &RenderTopology,
    room_scale_m: f32,
    available_backends: Vec<renderer::backend_registry::BackendListing>,
    backend_param_values_by_id: std::collections::HashMap<
        String,
        std::collections::HashMap<String, renderer::backend_params::ParamValue>,
    >,
    // Speaker names that can't be routed by position in by_name mode (computed by
    // the engine, which owns the name→label classifier). Shown as a warning.
    unroutable_speaker_names: &[String],
    fixed_channel_catalog_json: &str,
    fixed_channel_processing_json: &str,
) -> String {
    let effective_backend = active_topology.backend.backend_id();
    let effective_evaluation_mode = active_topology.backend.evaluation_mode().as_str();
    let render_backend_state_json = build_render_backend_state_json(
        live,
        active_topology,
        available_backends,
        backend_param_values_by_id,
    );
    let fixed_channel_catalog =
        serde_json::from_str::<serde_json::Value>(fixed_channel_catalog_json)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
    let fixed_channel_processing =
        serde_json::from_str::<serde_json::Value>(fixed_channel_processing_json)
            .unwrap_or_else(|_| serde_json::json!({ "stream": "idle" }));
    json!({
        "renderBackend": live.backend_id(),
        "renderBackendEffective": effective_backend,
        "renderEvaluationMode": live.requested_evaluation_mode().as_str(),
        "renderEvaluationModeEffective": effective_evaluation_mode,
        "objectSizeIntervals": live.evaluation.object_size_intervals,
        "masterGain": live.master_gain,
        "autoGain": live.auto_gain,
        "autoGainCeilingDb": live.auto_gain_ceiling_db,
        "rampMode": live.ramp_mode.as_str(),
        "channelRenderMode": live.channel_render_mode.as_str(),
        "syntheticObjectsEnabled": live.synthetic_objects_enabled,
        // Active fixed-bed→height object generator id; empty = off.
        "objectGeneratorId": live.object_generator_id.as_str(),
        // Live param overrides for the active generator (key → value), for the
        // Studio sliders. The schema itself is published separately by the engine
        // on `/omniphony/state/object_generators`.
        "objectGeneratorParams":
            serde_json::to_value(&live.object_generator_params).unwrap_or(serde_json::Value::Null),
        // Whether the active output layout has a top speaker. Generators are a
        // strict no-op without one; Studio still leaves them editable for
        // offline configuration and reports the applicability reason.
        "objectGeneratorLayoutHasHeight": active_topology
            .speaker_layout
            .speakers
            .iter()
            .any(|s| s.spatialize && s.z > 1.0e-3),
        // Canonical three-position mode plus the old derived boolean spelling
        // for clients that have not migrated yet.
        "phantomExtractMode": live.phantom_extract_mode.as_str(),
        "phantomEnabled": live.synthetic_objects_enabled
            && live.phantom_extract_mode != renderer::live_params::PhantomExtractMode::Off,
        "phantomParams":
            serde_json::to_value(&live.phantom_params).unwrap_or(serde_json::Value::Null),
        "surroundPlacement": live.surround_placement.as_str(),
        "outputChannelMapping": live.output_channel_mapping.as_str(),
        "outputChannelMappingUnroutable": unroutable_speaker_names,
        "fixedChannelCatalog": fixed_channel_catalog,
        "fixedChannelProcessing": fixed_channel_processing,
        // Declared live options, emitted generically from the registry under
        // their canonical (snake_case) keys. The flat camelCase keys above are
        // the legacy spellings, kept while clients migrate to this block.
        "options": renderer::options::options_json(live),
        // Parametrable virtual bed for channel content (null = built-in
        // canonical poses, LFE direct). Reuses the speaker-layout schema so the
        // Studio 3D editor can target it.
        "virtualBed": live.virtual_bed.as_ref()
            .map(|bed| serde_json::to_value(bed).unwrap_or(serde_json::Value::Null)),
        "distanceModel": live.distance_model.to_string(),
        "distanceModelMetric": live.distance_model_metric.to_string(),
        "roomRatio": {
            "width": live.room_ratio[0],
            "length": live.room_ratio[1],
            "height": live.room_ratio[2],
            "rear": live.room_ratio_rear,
            "lower": live.room_ratio_lower,
            "centerBlend": live.room_ratio_center_blend,
            // The room scale (metres-per-unit = radius_m = Width/2), broadcast
            // here in the reliable room domain so Studio restores it directly
            // instead of via the fragile layout-radius path. Live (editable)
            // value, so an in-session edit isn't reverted by a stale topology.
            "scaleM": room_scale_m
        },
        "spread": {
            "min": live.spread_min,
            "max": live.spread_max,
            "fromDistance": live.spread_from_distance,
            "distanceRange": live.spread_distance_range,
            "distanceCurve": live.spread_distance_curve,
            "sizeToSpreadMode": live.size_to_spread_mode.as_str()
        },
        "distanceDiffuse": {
            "enabled": live.use_distance_diffuse,
            "threshold": live.distance_diffuse_threshold,
            "curve": live.distance_diffuse_curve,
            "metric": live.distance_diffuse_metric.to_string(),
            "mirrorAxes": {
                "x": live.distance_diffuse_mirror_axes.x,
                "y": live.distance_diffuse_mirror_axes.y,
                "z": live.distance_diffuse_mirror_axes.z
            }
        },
        "vbapCartesian": {
            "xSize": live.evaluation.cartesian.x_size,
            "ySize": live.evaluation.cartesian.y_size,
            "zSize": live.evaluation.cartesian.z_size,
            "zNegSize": live.evaluation.cartesian.z_neg_size
        },
        "vbapPolar": {
            "azimuthResolution": live.evaluation.polar.azimuth_values,
            "elevationResolution": live.evaluation.polar.elevation_values,
            "distanceRes": live.evaluation.polar.distance_res,
            "distanceMax": live.evaluation.polar.distance_max,
            "positionInterpolation": live.evaluation.position_interpolation
        },
        "renderBackendState": serde_json::from_str::<serde_json::Value>(&render_backend_state_json)
            .unwrap_or_else(|_| json!({})),
        "binaural": {
            "outputMode": live.binaural.output_mode.as_str(),
            "mode": live.binaural.mode.as_str(),
            "ears": live.binaural.ears.iter().map(|e| json!({
                "gain": e.gain,
                "muted": e.muted,
            })).collect::<Vec<_>>(),
            "unitScaleM": live.binaural.unit_scale_m,
            "headRadiusM": live.binaural.head_radius_m,
            "reflections": {
                "enabled": live.binaural.reflections.enabled,
                "roomM": live.binaural.reflections.room_size_m,
                "level": live.binaural.reflections.level,
            },
            "reverb": {
                "enabled": live.binaural.reverb.enabled,
                "level": live.binaural.reverb.level,
                "rt60S": live.binaural.reverb.rt60_s,
                "predelayMs": live.binaural.reverb.predelay_ms,
            },
            "airAbsorption": live.binaural.air_absorption,
            "hrirSource": live.binaural.hrir_source.as_str(),
            "hrtfSofaPath": match &live.binaural.hrir_source {
                renderer::binaural::HrirSource::Sofa(p) => p.as_str(),
                _ => "",
            },
            "headPose": {
                "w": live.binaural.head_pose.w,
                "x": live.binaural.head_pose.x,
                "y": live.binaural.head_pose.y,
                "z": live.binaural.head_pose.z
            },
            "tracking": {
                "address": live.binaural.tracking.address,
                "format": live.binaural.tracking.format.as_str(),
                "smoothing": live.binaural.tracking.smoothing,
                "invert": live.binaural.tracking.invert
            }
        }
    })
    .to_string()
}

/// Build the capabilities handshake describing what this renderer instance can
/// actually do, so studio can label the connection and hide inapplicable panels.
///
/// Capabilities are derived from the host's actual control surfaces:
/// - `has_audio` (an [`AudioControl`] is attached) → output device + adaptive
///   resampling + latency-target controls apply (the standalone CLI/service).
/// - `has_input` (an [`InputControl`] is attached) → input-source controls apply.
///
/// The embedded host (`liborender` inside mpv) owns no audio output or input
/// stage — mpv does — so it attaches neither and advertises the reduced set,
/// tagged `variant: "embedded"` / `host: "mpv"` for the connection label.
fn build_renderer_capabilities_json(has_audio: bool, has_input: bool) -> String {
    let mut domains = vec!["renderer", "layout", "speakers", "loudness"];
    let mut control_config = vec!["layout", "speakers"];
    if has_audio {
        domains.push("audio");
        // The output device, adaptive resampler and latency-target servo all
        // live in the audio-output stage.
        control_config.push("audio");
        control_config.push("adaptive_resampling");
    }
    if has_input {
        domains.push("input");
        control_config.push("input");
    }
    json!({
        "producer": "renderer",
        "variant": if has_audio { "standalone" } else { "embedded" },
        "host": if has_audio { "cli" } else { "mpv" },
        "domains": domains,
        "realtime": ["master_gain", "speaker_gain"],
        "spatial": true,
        "metering": true,
        "controlConfig": control_config
    })
    .to_string()
}

#[cfg(test)]
mod capability_tests {
    use super::build_renderer_capabilities_json;

    fn parse(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("valid capabilities JSON")
    }

    #[test]
    fn standalone_advertises_full_set() {
        let v = parse(&build_renderer_capabilities_json(true, true));
        assert_eq!(v["variant"], "standalone");
        assert_eq!(v["host"], "cli");
        let domains = v["domains"].as_array().unwrap();
        assert!(domains.iter().any(|d| d == "audio"));
        assert!(domains.iter().any(|d| d == "input"));
        let cc = v["controlConfig"].as_array().unwrap();
        assert!(cc.iter().any(|c| c == "audio"));
        assert!(cc.iter().any(|c| c == "adaptive_resampling"));
        assert!(cc.iter().any(|c| c == "input"));
    }

    #[test]
    fn embedded_drops_audio_and_input() {
        let v = parse(&build_renderer_capabilities_json(false, false));
        assert_eq!(v["variant"], "embedded");
        assert_eq!(v["host"], "mpv");
        let domains = v["domains"].as_array().unwrap();
        assert!(!domains.iter().any(|d| d == "audio"));
        assert!(!domains.iter().any(|d| d == "input"));
        // Spatial domains stay.
        assert!(domains.iter().any(|d| d == "renderer"));
        assert!(domains.iter().any(|d| d == "speakers"));
        let cc = v["controlConfig"].as_array().unwrap();
        assert!(!cc.iter().any(|c| c == "audio"));
        assert!(!cc.iter().any(|c| c == "adaptive_resampling"));
        assert!(!cc.iter().any(|c| c == "input"));
        assert!(cc.iter().any(|c| c == "speakers"));
    }
}

pub fn build_speakers_state_json(
    live: &LiveParams,
    layout: &renderer::speaker_layout::SpeakerLayout,
) -> String {
    let speakers = layout
        .speakers
        .iter()
        .enumerate()
        .map(|(idx, speaker)| {
            let live_state = live.speakers.get(&idx);
            json!({
                "id": idx,
                "gain": live_state.map(|state| state.gain).unwrap_or(1.0),
                "delayMs": live_state
                    .map(|state| state.delay_ms)
                    .unwrap_or(speaker.delay_ms)
                    .max(0.0),
                "muted": live_state.map(|state| state.muted).unwrap_or(false)
            })
        })
        .collect::<Vec<_>>();
    json!({ "speakers": speakers }).to_string()
}

/// Build the core OSC messages for a live-state snapshot.
///
/// Returns `Vec<OscPacket>` rather than encoded bytes so the engine wrapper
/// can append `HostControlHandler::extend_snapshot()` + the
/// `/state/snapshot_complete` marker and bundle/encode the whole thing. The
/// core never references host-owned audio/input state directly; capabilities
/// are passed in (`has_audio`/`has_input`) by the engine wrapper, derived from
/// whether a `HostControlHandler` is attached.
pub fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    has_audio: bool,
    has_input: bool,
) -> Vec<OscPacket> {
    let live = control.live.read();
    let active_topology = control.active_topology();
    let editable_layout = control.editable_layout();
    let layout_json = serde_json::to_string(&editable_layout).unwrap_or_else(|_| "{}".to_string());
    let speakers_state_json = build_speakers_state_json(&live, &editable_layout);
    let loudness_gain: f32 = match (live.use_loudness, live.dialogue_level) {
        (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
        _ => 1.0,
    };
    let renderer_state_json = build_renderer_state_json(
        &live,
        &active_topology,
        editable_layout.radius_m,
        control.available_backends(),
        control.all_backend_params(),
        // The name→label classifier lives in orender_engine (bridge_api types),
        // which runtime_control can't reach; the engine's recompute broadcast
        // (fired on topology build and every layout edit) carries the real list.
        &[],
        &control.fixed_channel_catalog(),
        &control.fixed_channel_processing(),
    );

    let mut messages = vec![
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/capabilities".to_string(),
            args: vec![OscType::String(build_renderer_capabilities_json(
                has_audio, has_input,
            ))],
        }),
        OscPacket::Message(OscMessage {
            // Schema of the declared live options (registry rows: key, kind,
            // default, flags, i18n keys) — same pattern as the generator /
            // phantom param schemas, so clients can build controls from it.
            addr: crate::osc_contract::STATE_OPTIONS_SCHEMA.to_string(),
            args: vec![OscType::String(renderer::options::schema_json())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/renderer".to_string(),
            args: vec![OscType::String(renderer_state_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/layout".to_string(),
            args: vec![OscType::String(layout_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/speakers".to_string(),
            args: vec![OscType::String(speakers_state_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/loudness".to_string(),
            args: vec![OscType::String(
                json!({
                    "enabled": live.use_loudness,
                    "source": live.dialogue_level,
                    "gain": loudness_gain
                })
                .to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            // Monitoring cadences — renderer is the source of truth so studio
            // syncs its UI from here instead of pushing its own localStorage.
            addr: "/omniphony/state/monitoring".to_string(),
            args: vec![OscType::String(
                json!({
                    "meterRateHz": control.meter_rate_hz(),
                    "diagRateHz": control.diag_rate_hz(),
                })
                .to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/x_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.x_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/y_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.y_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/z_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.z_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/z_neg_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.z_neg_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/position_interpolation".to_string(),
            args: vec![OscType::Int(if live.evaluation.position_interpolation {
                1
            } else {
                0
            })],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/object_size_intervals".to_string(),
            args: vec![OscType::Int(live.evaluation.object_size_intervals as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/log_level".to_string(),
            args: vec![OscType::String(
                sys::live_log::current_runtime_level_name().to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/azimuth_resolution".to_string(),
            args: vec![OscType::Int(live.evaluation.polar.azimuth_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/elevation_resolution".to_string(),
            args: vec![OscType::Int(live.evaluation.polar.elevation_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/distance_res".to_string(),
            args: vec![OscType::Int(live.evaluation.polar.distance_res.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/distance_max".to_string(),
            args: vec![OscType::Float(live.evaluation.polar.distance_max.max(0.01))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/allow_negative_z".to_string(),
            args: vec![OscType::Int(
                if control
                    .backend_rebuild_params()
                    .map(|p| p.allow_negative_z)
                    .unwrap_or(true)
                {
                    1
                } else {
                    0
                },
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/config/saved".to_string(),
            args: vec![OscType::Int(
                if control
                    .config_dirty
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    0
                } else {
                    1
                },
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/input_pipe".to_string(),
            args: vec![OscType::String(control.input_path().unwrap_or_default())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render/bridge_path".to_string(),
            args: vec![OscType::String(
                control
                    .bridge_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )],
        }),
        OscPacket::Message(OscMessage {
            // The config file this renderer instance actually loaded. Empty
            // means it booted on built-in defaults (no config). Studio shows
            // this in About so a CLI-vs-host config mismatch (e.g. mpv falling
            // back to defaults while the CLI used ~/.config/omniphony) is
            // immediately visible.
            addr: "/omniphony/state/render/config_path".to_string(),
            args: vec![OscType::String(
                control
                    .config_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )],
        }),
        OscPacket::Message(OscMessage {
            // Did that config actually load? "loaded" / "missing" /
            // "parse_error", or "" when no config path was given (defaults by
            // design). A non-"loaded" value means the renderer is on built-in
            // defaults despite config_path looking valid — Studio flags it red.
            addr: "/omniphony/state/render/config_status".to_string(),
            args: vec![OscType::String(control.config_status().unwrap_or_default())],
        }),
        OscPacket::Message(OscMessage {
            // Build fingerprint of THIS renderer (git-describe + build time,
            // stamped into runtime_control which both the CLI binary and the
            // mpv-embedded liborender link). Studio shows it in About so a
            // liborender-vs-orender version skew is visible at a glance.
            addr: "/omniphony/state/render/version".to_string(),
            args: vec![OscType::String(crate::build_fingerprint())],
        }),
        OscPacket::Message(OscMessage {
            // Path of the process serving this engine. Two checkouts of the same
            // commit share a fingerprint, so only the path tells a client whether
            // the renderer answering on this port is the one it started or one
            // left behind by another environment.
            addr: "/omniphony/state/render/executable".to_string(),
            args: vec![OscType::String(crate::executable_path())],
        }),
        OscPacket::Message(OscMessage {
            // C-ABI version ("major.minor") of the liborender shim hosting this
            // engine, or "" when the engine is linked directly as a Rust crate
            // (the CLI — no C ABI involved). Studio shows it in About next to
            // the build fingerprint.
            addr: "/omniphony/state/render/abi".to_string(),
            args: vec![OscType::String(
                control
                    .host_abi()
                    .map(|(major, minor)| format!("{major}.{minor}"))
                    .unwrap_or_default(),
            )],
        }),
        OscPacket::Message(OscMessage {
            // Non-empty when this renderer came up in the degraded "no decoder"
            // state because the bridge couldn't be resolved/loaded. The embedded
            // (mpv) host returns NULL from orender_create in that case (so mpv
            // falls back to its native decoder), but a process-global degraded
            // reporter still serves OSC so Studio can show a red banner with this
            // message. Empty in normal operation.
            addr: "/omniphony/state/render/bridge_error".to_string(),
            args: vec![OscType::String(control.bridge_error().unwrap_or_default())],
        }),
    ];

    // DRC is a decode-stage control owned by the core (lives in liborender).
    // Always publish the DRC fields on /state/input. When a host_audio
    // HostControlHandler is attached, its extend_snapshot() emits a separate
    // /state/input message carrying the live-input device fields; studio's
    // Tauri InputDomainState parser merges partial payloads, so two
    // /state/input messages in one bundle compose cleanly.
    messages.push(OscPacket::Message(OscMessage {
        addr: "/omniphony/state/input".to_string(),
        args: vec![OscType::String(
            json!({
                "drcMode": live.drc_mode,
                "drcWeight": live.drc_weight,
                "supportedDrcModes": control.bridge_supported_drc_modes(),
            })
            .to_string(),
        )],
    }));

    let mut all_messages = messages;

    for (&idx, obj) in &live.objects {
        if obj.muted {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/object/{}/mute", idx),
                args: vec![OscType::Int(1)],
            }));
        }
    }

    // The engine wrapper appends `HostControlHandler::extend_snapshot()` and
    // the `/state/snapshot_complete` marker, then bundles + encodes.
    all_messages
}
