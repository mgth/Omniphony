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
pub struct ExperimentalDistanceOptionsSnapshot {
    pub distance_floor: f32,
    pub min_active_speakers: usize,
    pub max_active_speakers: usize,
    pub position_error_floor: f32,
    pub position_error_nearest_scale: f32,
    pub position_error_span_scale: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct BarycenterOptionsSnapshot {
    pub localize: f32,
}

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
    pub capabilities: renderer::render_backend::BackendCapabilities,
    pub allowed_evaluation_modes: Vec<String>,
    pub frozen_room_ratio: bool,
    pub frozen_speakers: bool,
    pub restore_backend_available: bool,
    pub barycenter: BarycenterOptionsSnapshot,
    pub experimental_distance: ExperimentalDistanceOptionsSnapshot,
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
) -> RenderBackendStateSnapshot {
    let backend = &active_topology.backend;
    let capabilities = backend.capabilities();
    RenderBackendStateSnapshot {
        selection: live.backend_id().to_string(),
        effective: backend.backend_id().to_string(),
        effective_label: backend.backend_label().to_string(),
        capabilities,
        allowed_evaluation_modes: allowed_evaluation_modes(backend, capabilities),
        frozen_room_ratio: false,
        frozen_speakers: false,
        restore_backend_available: false,
        barycenter: BarycenterOptionsSnapshot {
            localize: live.barycenter.localize,
        },
        experimental_distance: ExperimentalDistanceOptionsSnapshot {
            distance_floor: live.experimental_distance.distance_floor,
            min_active_speakers: live.experimental_distance.min_active_speakers,
            max_active_speakers: live.experimental_distance.max_active_speakers,
            position_error_floor: live.experimental_distance.position_error_floor,
            position_error_nearest_scale: live.experimental_distance.position_error_nearest_scale,
            position_error_span_scale: live.experimental_distance.position_error_span_scale,
        },
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
) -> String {
    serde_json::to_string(&build_render_backend_state_snapshot(live, active_topology))
        .unwrap_or_else(|_| "{}".to_string())
}

pub fn build_renderer_state_json(live: &LiveParams, active_topology: &RenderTopology) -> String {
    let effective_backend = active_topology.backend.kind().as_str();
    let effective_evaluation_mode = active_topology.backend.evaluation_mode().as_str();
    let render_backend_state_json = build_render_backend_state_json(live, active_topology);
    json!({
        "renderBackend": live.backend_id(),
        "renderBackendEffective": effective_backend,
        "renderEvaluationMode": live.requested_evaluation_mode().as_str(),
        "renderEvaluationModeEffective": effective_evaluation_mode,
        "masterGain": live.master_gain,
        "rampMode": live.ramp_mode.as_str(),
        "distanceModel": live.distance_model.to_string(),
        "distanceModelMetric": live.distance_model_metric.to_string(),
        "roomRatio": {
            "width": live.room_ratio[0],
            "length": live.room_ratio[1],
            "height": live.room_ratio[2],
            "rear": live.room_ratio_rear,
            "lower": live.room_ratio_lower,
            "centerBlend": live.room_ratio_center_blend
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
            "metric": live.distance_diffuse_metric.to_string()
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
            .unwrap_or_else(|_| json!({}))
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
        "realtime": ["master_gain", "speaker_gain", "object_gain"],
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
    let live = control.live.read().unwrap();
    let active_topology = control.active_topology();
    let editable_layout = control.editable_layout();
    let layout_json = serde_json::to_string(&editable_layout).unwrap_or_else(|_| "{}".to_string());
    let speakers_state_json = build_speakers_state_json(&live, &editable_layout);
    let loudness_gain: f32 = match (live.use_loudness, live.dialogue_level) {
        (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
        _ => 1.0,
    };
    let renderer_state_json = build_renderer_state_json(&live, &active_topology);

    let mut messages = vec![
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/capabilities".to_string(),
            args: vec![OscType::String(build_renderer_capabilities_json(
                has_audio,
                has_input,
            ))],
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
        if obj.gain != 1.0 {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/object/{}/gain", idx),
                args: vec![OscType::Float(obj.gain)],
            }));
        }
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
