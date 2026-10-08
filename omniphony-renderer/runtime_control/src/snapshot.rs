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
    pub available_backends: Vec<renderer::plugin::PluginListing>,
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
    available_backends: Vec<renderer::plugin::PluginListing>,
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
    available_backends: Vec<renderer::plugin::PluginListing>,
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
    available_backends: Vec<renderer::plugin::PluginListing>,
    // Every plugin's stored parameter values (backends, object generators,
    // the phantom stage).
    plugin_params: renderer::plugin::PluginParams,
    // Speaker names that can't be routed by position in by_name mode (computed by
    // the engine, which owns the name→label classifier). Shown as a warning.
    unroutable_speaker_names: &[String],
    fixed_channel_catalog_json: &str,
    fixed_channel_processing_json: &str,
    crossover_info: Option<renderer::live_params::CrossoverInfo>,
    hrir_status: &renderer::binaural::HrirStatus,
    brir_status: &renderer::binaural::BrirStatus,
    // Why the resident BRIR set's loudspeakers are not the layout a
    // headphone render pans onto (`RendererControl::brir_layout`). Read
    // by the caller before it takes the live params' lock.
    brir_layout_error: Option<String>,
) -> String {
    let effective_backend = active_topology.backend.backend_id();
    let effective_evaluation_mode = active_topology.backend.evaluation_mode().as_str();
    // The spread addresses write the "vbap" param bag, not the live fields, so
    // resolve the block the way the backend build does (bag value, live
    // fallback) — the live fields alone go stale after the first edit.
    use renderer::plugin::{PHANTOM_EXTRACT_ID, PluginKind};
    let spread = renderer::backend_registry::resolve_vbap_spread_params(
        live,
        plugin_params.bag(PluginKind::Backend).get("vbap"),
    );
    let render_backend_state_json = build_render_backend_state_json(
        live,
        active_topology,
        available_backends,
        plugin_params.bag(PluginKind::Backend).clone(),
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
        // Where the grid comes from (`bridge` or `custom`), and the grid the
        // active bridge hints (null until known): Studio shows it read-only
        // while the grid follows the bridge.
        "evaluationGrid": live.evaluation.source.as_str(),
        "evaluationGridBridge": live.evaluation.bridge_hint.map(|grid| {
            let mut json = grid.to_json();
            // Its place among the loaded bridges of `render/bridges`.
            json["bridgeIndex"] = live.evaluation.bridge_index.into();
            json
        }),
        "objectSizeIntervals": live.evaluation.object_size_intervals,
        "masterGain": live.master_gain,
        "autoGain": live.options.auto_gain,
        "autoGainCeilingDb": live.options.auto_gain_ceiling_db,
        "rampMode": live.options.ramp_mode.as_str(),
        "channelRenderMode": live.channel_render_mode.as_str(),
        "syntheticObjectsEnabled": live.options.synthetic_objects_enabled,
        // Active fixed-bed→height object generator id; empty = off.
        "objectGeneratorId": live.options.object_generator_id.as_str(),
        // Stored param values of every generator (`{ id: { key: value } }`),
        // as `renderBackendState.backendParamValuesById` carries the
        // backends'. The listings are published separately by the engine on
        // `/omniphony/state/object_generators`.
        "objectGeneratorParamValuesById":
            serde_json::to_value(plugin_params.bag(PluginKind::ObjectGenerator))
                .unwrap_or(serde_json::Value::Null),
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
        "phantomExtractMode": live.options.phantom_extract_mode.as_str(),
        "phantomEnabled": live.options.synthetic_objects_enabled
            && live.options.phantom_extract_mode != renderer::live_params::PhantomExtractMode::Off,
        // Stored param values of the phantom stage (`{ key: value }`); its
        // listing is published on `/omniphony/state/phantom`.
        "phantomParamValues": serde_json::to_value(
            plugin_params
                .plugin(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID)
                .cloned()
                .unwrap_or_default(),
        )
        .unwrap_or(serde_json::Value::Null),
        "surroundPlacement": live.options.surround_placement.as_str(),
        "outputChannelMapping": live.options.output_channel_mapping.as_str(),
        "outputChannelMappingUnroutable": unroutable_speaker_names,
        "fixedChannelCatalog": fixed_channel_catalog,
        "fixedChannelProcessing": fixed_channel_processing,
        // Facts about the crossover bank the speaker stage actually built
        // (engine, bands, cutoffs, FIR taps, latency). Studio annotates the
        // crossover control with them. Null until the first render built one.
        "crossover": crossover_info.map(|info| json!({
            "engine": info.engine.as_str(),
            "bands": info.bands,
            "cutoffsHz": info.cutoffs_hz,
            "taps": info.taps,
            "latencyMs": info.latency_samples as f64 * 1000.0
                / info.sample_rate.max(1) as f64,
        })),
        // Declared live options, emitted generically from the registry under
        // their canonical (snake_case) keys. The flat camelCase keys above are
        // the legacy spellings, kept while clients migrate to this block.
        "options": renderer::options::options_json(live),
        // Per-family placement of fixed channels (`renderer::placement`):
        // each family's own settings and what they resolve to, keyed by
        // name; `placementFamilies` lists the families a client offers, in
        // order (the renderer's own and the loaded bridge's).
        "placement": placement_json(&live.placement, live.binaural.output_mode),
        "placementFamilies": placement_families_json(&live.placement),
        // Legacy mirror of the generic family's own entries (null = none),
        // for clients that predate `placement`.
        "virtualBed": live.placement.family(renderer::placement::SourceFamily::GENERIC)
            .layout.as_ref()
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
            "min": spread.spread_min,
            "max": spread.spread_max,
            "fromDistance": spread.spread_from_distance,
            "distanceRange": spread.spread_distance_range,
            "distanceCurve": spread.spread_distance_curve,
            "sizeToSpreadMode": spread.size_to_spread_mode.as_str()
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
            // What actually renders: a room response (`brir` source) only
            // knows its loudspeakers, so it forces the virtual-speaker path
            // whatever `mode` says.
            "modeEffective": if matches!(
                live.binaural.hrir_source,
                renderer::binaural::HrirSource::Brir(_)
            ) {
                "cascaded"
            } else {
                live.binaural.mode.as_str()
            },
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
                "wallCutoffHz": live.binaural.reflections.wall_cutoff_hz,
            },
            "reverb": {
                "enabled": live.binaural.reverb.enabled,
                "level": live.binaural.reverb.level,
                "rt60S": live.binaural.reverb.rt60_s,
                "predelayMs": live.binaural.reverb.predelay_ms,
                "size": live.binaural.reverb.size,
                "rt60LowRatio": live.binaural.reverb.rt60_low_ratio,
                "rt60HighRatio": live.binaural.reverb.rt60_high_ratio,
            },
            "airAbsorption": live.binaural.air_absorption,
            "diffuseFieldEq": live.binaural.diffuse_field_eq,
            "hrirSource": live.binaural.hrir_source.as_str(),
            // The parametric sources' settings, which travel inside the
            // source string: echoed so a client shows what is rendered (and
            // saved) rather than its own defaults.
            "hrirParams": hrir_params_json(&live.binaural.hrir_source),
            "hrtfSofaPath": match &live.binaural.hrir_source {
                renderer::binaural::HrirSource::Sofa(p) => p.as_str(),
                _ => "",
            },
            // What is actually convolved: differs from `hrirSource` when the
            // SOFA file failed to load and the build fell back to KEMAR, in
            // which case `hrirError` says why.
            "hrirEffective": hrir_status.effective.as_str(),
            "hrirError": hrir_status.error,
            "brirSofaPath": match &live.binaural.hrir_source {
                renderer::binaural::HrirSource::Brir(p) => p.as_str(),
                _ => "",
            },
            // A room response (`brir` source): its load options, and what
            // the renderer holds of it — or why it holds nothing, in which
            // case the virtual room is binauralised by the HRTF stage.
            "brir": {
                "headTracking": match live.binaural.brir.head_tracking {
                    None => json!("auto"),
                    Some(b) => json!(b),
                },
                "maxLengthS": live.binaural.brir.max_length_s,
                "tailFloorDb": live.binaural.brir.tail_floor_db,
                "path": brir_status.path,
                "loaded": brir_status.loaded.as_ref().map(|s| json!({
                    "conventions": s.conventions,
                    "emitters": s.emitters,
                    "orientations": s.orientations,
                    "maxTaps": s.max_taps,
                    "sampleRate": s.sample_rate,
                    "bytes": s.bytes,
                })),
                "error": brir_status.error,
                // The set's own loudspeakers, once the topology renders on
                // them (`RenderTopology::brir_layout`): what the listener is
                // panned onto in place of the editable layout. Read-only —
                // the measurement fixes them.
                "layout": active_topology.brir_layout.then(|| {
                    serde_json::to_value(&active_topology.speaker_layout)
                        .unwrap_or(serde_json::Value::Null)
                }),
                "layoutError": brir_layout_error,
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
                "invert": live.binaural.tracking.invert,
                // Three-pose axis calibration: 0 idle, 1 = turn left next,
                // 2 = look up next; and whether axes are calibrated at all.
                "calibrationStep": live.binaural.tracking.calibration.step(),
                "axesCalibrated": live.binaural.tracking.axes
                    != renderer::binaural::HeadPose::identity()
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
/// The parametric HRIR sources' settings (`hrirParams`), `null` for the
/// others.
fn hrir_params_json(source: &renderer::binaural::HrirSource) -> serde_json::Value {
    match source {
        renderer::binaural::HrirSource::Pinna {
            preset,
            d_scale_pct,
            depth_pct,
        } => json!({
            "preset": preset.as_str(),
            "dScalePct": d_scale_pct,
            "depthPct": depth_pct,
        }),
        renderer::binaural::HrirSource::Prtf {
            freq_scale_pct,
            depth_pct,
        } => json!({
            "freqScalePct": freq_scale_pct,
            "depthPct": depth_pct,
        }),
        _ => serde_json::Value::Null,
    }
}

/// The options schema a host publishes: the core options it offers (a host
/// with audio I/O leaves out the embedded engine's), then its own.
fn options_schema_json(has_audio: bool, host: Option<&dyn crate::HostControlHandler>) -> String {
    let mut entries = renderer::options::schema_entries(has_audio);
    if let Some(host) = host {
        entries.extend(host.options_schema());
    }
    serde_json::Value::Array(entries).to_string()
}

/// `/state/host_options`: the host's options, requested and applied, and its
/// `Staged` groups' pending flags. `None` for a host that declares none.
fn host_options_json(host: &dyn crate::HostControlHandler) -> Option<String> {
    let options = host.options_json();
    if options.is_empty() {
        return None;
    }
    let pending: serde_json::Map<String, serde_json::Value> = host
        .option_groups_pending()
        .into_iter()
        .map(|(group, pending)| (group.to_string(), pending.into()))
        .collect();
    Some(
        json!({
            "options": options,
            "applied": host.options_applied_json(),
            "pending": pending,
        })
        .to_string(),
    )
}

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
        "fileRequestIds": true,
        "controlConfig": control_config,
        // What a client compares its own contract with (osc-contract).
        "contractRevision": crate::osc_contract::CONTRACT_REVISION
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
    fn both_variants_advertise_the_contract_revision() {
        for has_host in [true, false] {
            let v = parse(&build_renderer_capabilities_json(has_host, has_host));
            assert_eq!(
                v["contractRevision"],
                crate::osc_contract::CONTRACT_REVISION
            );
        }
    }

    #[test]
    fn standalone_advertises_full_set() {
        let v = parse(&build_renderer_capabilities_json(true, true));
        assert_eq!(v["variant"], "standalone");
        assert_eq!(v["host"], "cli");
        assert_eq!(v["fileRequestIds"], true);
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
        assert_eq!(v["fileRequestIds"], true);
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
                "gain": live_state.map_or_else(
                    || renderer::live_params::speaker_gain_linear(speaker.gain_db),
                    |state| state.gain
                ),
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
/// JSON payload of [`crate::osc_contract::STATE_PROFILES`]:
/// `{"active": "...", "names": ["..."]}`.
/// `/omniphony/state/render/bridges`: the bridges asked for, then what the
/// host loaded and what failed.
pub fn bridges_state_json(control: &RendererControl) -> String {
    json!({
        "requested": control
            .bridge_paths()
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "bridges": control.bridges_status(),
    })
    .to_string()
}

pub fn profiles_state_json(control: &RendererControl) -> String {
    let info = control.profiles_info();
    json!({
        "active": info.active,
        "names": info.names,
    })
    .to_string()
}

pub fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    has_audio: bool,
    has_input: bool,
) -> Vec<OscPacket> {
    build_live_state_bundle_with_host(control, has_audio, has_input, None)
}

/// [`build_live_state_bundle`] with the registered host handler, whose
/// declared options join the published schema and go out in
/// `/state/host_options`.
pub fn build_live_state_bundle_with_host(
    control: &Arc<RendererControl>,
    has_audio: bool,
    has_input: bool,
    host: Option<&dyn crate::HostControlHandler>,
) -> Vec<OscPacket> {
    // Before the live lock: it reads the live params itself.
    let brir_layout_error = control.brir_layout().err();
    let live = control.live.read();
    let active_topology = control.active_topology();
    let editable_layout = control.editable_layout();
    let layout_json = serde_json::to_string(&editable_layout).unwrap_or_else(|_| "{}".to_string());
    let speakers_state_json = build_speakers_state_json(&live, &editable_layout);
    let loudness_gain: f32 = match (live.options.use_loudness, live.dialogue_level) {
        (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
        _ => 1.0,
    };
    let renderer_state_json = build_renderer_state_json(
        &live,
        &active_topology,
        editable_layout.radius_m,
        control.available_backends(),
        control.plugin_params(),
        // The name→label classifier lives in orender_engine (bridge_api types),
        // which runtime_control can't reach; the engine's recompute broadcast
        // (fired on topology build and every layout edit) carries the real list.
        &[],
        &control.fixed_channel_catalog(),
        &control.fixed_channel_processing(),
        control.crossover_info(),
        &control.binaural_hrir_status(),
        &control.binaural_brir_status(),
        brir_layout_error,
    );

    let mut messages = vec![
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_CAPABILITIES.to_string(),
            args: vec![OscType::String(build_renderer_capabilities_json(
                has_audio, has_input,
            ))],
        }),
        OscPacket::Message(OscMessage {
            // Schema of the declared live options (registry rows: key, kind,
            // default, flags, i18n keys) — same pattern as the generator /
            // phantom param schemas, so clients can build controls from it.
            addr: crate::osc_contract::STATE_OPTIONS_SCHEMA.to_string(),
            args: vec![OscType::String(options_schema_json(has_audio, host))],
        }),
        OscPacket::Message(OscMessage {
            // Named config profiles (docs/config-profiles.md): active + list,
            // so clients render the picker without reading the config file.
            addr: crate::osc_contract::STATE_PROFILES.to_string(),
            args: vec![OscType::String(profiles_state_json(control))],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDERER.to_string(),
            args: vec![OscType::String(renderer_state_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_LAYOUT.to_string(),
            args: vec![OscType::String(layout_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_SPEAKERS.to_string(),
            args: vec![OscType::String(speakers_state_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_LOUDNESS.to_string(),
            args: vec![OscType::String(
                json!({
                    "enabled": live.options.use_loudness,
                    "source": live.dialogue_level,
                    "gain": loudness_gain
                })
                .to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            // Monitoring cadences — renderer is the source of truth so studio
            // syncs its UI from here instead of pushing its own localStorage.
            addr: crate::osc_contract::STATE_MONITORING.to_string(),
            args: vec![OscType::String(
                json!({
                    "meterRateHz": control.meter_rate_hz(),
                    "diagRateHz": control.diag_rate_hz(),
                })
                .to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_CARTESIAN_X_SIZE.to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.x_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_CARTESIAN_Y_SIZE.to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.y_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_CARTESIAN_Z_SIZE.to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.z_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_CARTESIAN_Z_NEG_SIZE.to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.z_neg_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_POSITION_INTERPOLATION.to_string(),
            args: vec![OscType::Int(if live.evaluation.position_interpolation {
                1
            } else {
                0
            })],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_OBJECT_SIZE_INTERVALS.to_string(),
            args: vec![OscType::Int(live.evaluation.object_size_intervals as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_LOG_LEVEL.to_string(),
            args: vec![OscType::String(
                live_log::current_runtime_level_name().to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_POLAR_AZIMUTH_RESOLUTION.to_string(),
            args: vec![OscType::Int(live.evaluation.polar.azimuth_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_POLAR_ELEVATION_RESOLUTION
                .to_string(),
            args: vec![OscType::Int(live.evaluation.polar.elevation_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_POLAR_DISTANCE_RES.to_string(),
            args: vec![OscType::Int(live.evaluation.polar.distance_res.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_EVALUATION_POLAR_DISTANCE_MAX.to_string(),
            args: vec![OscType::Float(live.evaluation.polar.distance_max.max(0.01))],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_VBAP_ALLOW_NEGATIVE_Z.to_string(),
            // The live value, which the gain models are built with; the
            // engine's default is off.
            args: vec![OscType::Int(i32::from(live.evaluation.allow_negative_z))],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_CONFIG_SAVED.to_string(),
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
            addr: crate::osc_contract::STATE_INPUT_PIPE.to_string(),
            args: vec![OscType::String(control.input_path().unwrap_or_default())],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_BRIDGE_PATH.to_string(),
            args: vec![OscType::String(
                control
                    .first_bridge_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_RENDER_BRIDGES.to_string(),
            args: vec![OscType::String(bridges_state_json(control))],
        }),
        OscPacket::Message(OscMessage {
            // The config file this renderer instance actually loaded. Empty
            // means it booted on built-in defaults (no config). Studio shows
            // this in About so a CLI-vs-host config mismatch (e.g. mpv falling
            // back to defaults while the CLI used ~/.config/omniphony) is
            // immediately visible.
            addr: crate::osc_contract::STATE_RENDER_CONFIG_PATH.to_string(),
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
            addr: crate::osc_contract::STATE_RENDER_CONFIG_STATUS.to_string(),
            args: vec![OscType::String(control.config_status().unwrap_or_default())],
        }),
        OscPacket::Message(OscMessage {
            // Build fingerprint of THIS renderer (git-describe + build time,
            // stamped into runtime_control which both the CLI binary and the
            // mpv-embedded liborender link). Studio shows it in About so a
            // liborender-vs-orender version skew is visible at a glance.
            addr: crate::osc_contract::STATE_RENDER_VERSION.to_string(),
            args: vec![OscType::String(crate::build_fingerprint())],
        }),
        OscPacket::Message(OscMessage {
            // Path of the process serving this engine. Two checkouts of the same
            // commit share a fingerprint, so only the path tells a client whether
            // the renderer answering on this port is the one it started or one
            // left behind by another environment.
            addr: crate::osc_contract::STATE_RENDER_EXECUTABLE.to_string(),
            args: vec![OscType::String(crate::executable_path())],
        }),
        OscPacket::Message(OscMessage {
            // C-ABI version ("major.minor") of the liborender shim hosting this
            // engine, or "" when the engine is linked directly as a Rust crate
            // (the CLI — no C ABI involved). Studio shows it in About next to
            // the build fingerprint.
            addr: crate::osc_contract::STATE_RENDER_ABI.to_string(),
            args: vec![OscType::String(
                control
                    .host_abi()
                    .map(|(major, minor)| format!("{major}.{minor}"))
                    .unwrap_or_default(),
            )],
        }),
        OscPacket::Message(OscMessage {
            // The bridge_api this engine loads bridges of (same minor only).
            // Studio shows it in About next to the ABI, so "installed and no
            // sound" can be matched against the bridge's own version (#676).
            addr: crate::osc_contract::STATE_RENDER_BRIDGE_API.to_string(),
            args: vec![OscType::String(bridge_api::VERSION.to_string())],
        }),
        OscPacket::Message(OscMessage {
            // Non-empty when this renderer came up in the degraded "no decoder"
            // state because the bridge couldn't be resolved/loaded. The embedded
            // (mpv) host returns NULL from orender_create in that case (so mpv
            // falls back to its native decoder), but a process-global degraded
            // reporter still serves OSC so Studio can show a red banner with this
            // message. Empty in normal operation.
            addr: crate::osc_contract::STATE_RENDER_BRIDGE_ERROR.to_string(),
            args: vec![OscType::String(control.bridge_error().unwrap_or_default())],
        }),
    ];
    if let Some(host_options) = host.and_then(host_options_json) {
        messages.push(OscPacket::Message(OscMessage {
            addr: crate::osc_contract::STATE_HOST_OPTIONS.to_string(),
            args: vec![OscType::String(host_options)],
        }));
    }

    // DRC is a decode-stage control owned by the core (lives in liborender).
    // Always publish the DRC fields on /state/input. When a host_audio
    // HostControlHandler is attached, its extend_snapshot() emits a separate
    // /state/input message carrying the live-input device fields; studio's
    // Tauri InputDomainState parser merges partial payloads, so two
    // /state/input messages in one bundle compose cleanly.
    messages.push(OscPacket::Message(OscMessage {
        addr: crate::osc_contract::STATE_INPUT.to_string(),
        args: vec![OscType::String(
            json!({
                "drcMode": live.options.drc_mode,
                "drcWeight": live.options.drc_weight,
                "supportedDrcModes": control.bridge_supported_drc_modes(),
                // What the stream tags among its channels (the dialogue a
                // format codes apart): `[{kind, language, label, channels}]`.
                "channelTags": serde_json::from_str::<serde_json::Value>(&control.channel_tags())
                    .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
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

/// The `placement` block of the renderer snapshot: per family of the table,
/// its `label`, whether it is `declared` (by the renderer or the loaded
/// bridge, else known only from the config), its `defaultMode` (the mode on
/// speakers when neither it nor the generic family sets one; headphones
/// default to sphere), its own `mode`/`layout` (null
/// when unset, i.e. inherited) and the effective result on the current
/// output — `effectiveMode`, `modeSource` saying why (`own`, `generic`,
/// `headphones` or `family`), and `layoutSource` saying whose entries apply
/// (`own`, `generic` or `none`).
fn placement_json(
    state: &renderer::placement::PlacementState,
    output: renderer::live_params::OutputMode,
) -> serde_json::Value {
    use renderer::placement::SourceFamily;
    let generic_has_layout = state.family(SourceFamily::GENERIC).layout.is_some();
    let mut families = serde_json::Map::new();
    for (family, info) in state.families() {
        let own = state.family(family);
        let (effective_mode, mode_source) = state.resolve_mode(family, output);
        let layout_source = if own.layout.is_some() {
            "own"
        } else if generic_has_layout {
            "generic"
        } else {
            "none"
        };
        families.insert(
            info.name.clone(),
            json!({
                "label": info.label,
                "declared": info.declared,
                "defaultMode": info.default_mode.as_str(),
                "mode": own.mode.map(|m| m.as_str()),
                "layout": own.layout.as_ref()
                    .map(|bed| serde_json::to_value(bed).unwrap_or(serde_json::Value::Null)),
                "effectiveMode": effective_mode.as_str(),
                "modeSource": mode_source.as_str(),
                "layoutSource": layout_source,
            }),
        );
    }
    serde_json::Value::Object(families)
}

/// The families a client offers, by name, in order: the generic family, the
/// loaded bridge's in its catalogue order, then the renderer's PCM input.
/// A family known only from the config is left out — no stream can have it.
fn placement_families_json(state: &renderer::placement::PlacementState) -> serde_json::Value {
    use renderer::placement::SourceFamily;
    let name = |family| state.info(family).name.as_str();
    let bridge = state
        .families()
        .filter(|(family, info)| {
            info.declared && *family != SourceFamily::GENERIC && *family != SourceFamily::PCM
        })
        .map(|(_, info)| info.name.as_str());
    let names: Vec<&str> = std::iter::once(name(SourceFamily::GENERIC))
        .chain(bridge)
        .chain(std::iter::once(name(SourceFamily::PCM)))
        .collect();
    json!(names)
}

#[cfg(test)]
mod hrir_params_tests {
    use super::hrir_params_json;
    use renderer::binaural::{HrirSource, PinnaPreset};

    /// The parametric settings are echoed, so Studio shows what is rendered
    /// and saved instead of its own defaults.
    #[test]
    fn parametric_sources_echo_their_settings() {
        let pinna = hrir_params_json(&HrirSource::Pinna {
            preset: PinnaPreset::Rd,
            d_scale_pct: 110,
            depth_pct: 60,
        });
        assert_eq!(pinna["preset"], "rd");
        assert_eq!(pinna["dScalePct"], 110);
        assert_eq!(pinna["depthPct"], 60);
        let prtf = hrir_params_json(&HrirSource::Prtf {
            freq_scale_pct: 95,
            depth_pct: 40,
        });
        assert_eq!(prtf["freqScalePct"], 95);
        assert_eq!(prtf["depthPct"], 40);
        assert!(hrir_params_json(&HrirSource::SafKemar).is_null());
    }
}
