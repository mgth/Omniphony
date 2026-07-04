use rosc::OscType;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinateFormat {
    Cartesian = 0,
    Polar = 1,
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn unwrap_arg(arg: &OscType) -> f64 {
    match arg {
        OscType::Float(v) => *v as f64,
        OscType::Double(v) => *v,
        OscType::Int(v) => *v as f64,
        OscType::Long(v) => *v as f64,
        _ => f64::NAN,
    }
}

fn unwrap_string(arg: &OscType) -> Option<String> {
    match arg {
        OscType::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn unwrap_blob(arg: &OscType) -> Option<Vec<u8>> {
    match arg {
        OscType::Blob(b) => Some(b.clone()),
        _ => None,
    }
}

fn to_number(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

fn clamp(v: f64, min: f64, max: f64) -> f64 {
    v.max(min).min(max)
}

fn spherical_to_cartesian(az_deg: f64, el_deg: f64, dist: f64) -> (f64, f64, f64) {
    let az = az_deg.to_radians();
    let el = el_deg.to_radians();
    let x = dist * el.cos() * az.cos();
    let y = dist * el.sin();
    let z = dist * el.cos() * az.sin();
    (x, y, z)
}

fn find_id_in_address(parts: &[&str]) -> Option<String> {
    let anchors = ["source", "sources", "object", "obj", "track", "channel"];
    let reserved: std::collections::HashSet<&str> = [
        "position",
        "pos",
        "xyz",
        "aed",
        "spherical",
        "polar",
        "angles",
        "remove",
        "delete",
        "off",
    ]
    .iter()
    .copied()
    .collect();

    for i in 0..parts.len().saturating_sub(1) {
        if anchors.contains(&parts[i]) {
            let candidate = parts[i + 1];
            if !reserved.contains(candidate) {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

// ── return types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(rename = "coordMode")]
    pub coord_mode: String,
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
    #[serde(rename = "sourceTag", skip_serializing_if = "Option::is_none")]
    pub source_tag: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct LogEntry {
    pub seq: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OscEvent {
    #[serde(rename = "spatial:frame")]
    SpatialFrame {
        #[serde(rename = "samplePos")]
        sample_pos: i64,
        generation: u64,
        #[serde(rename = "objectCount")]
        object_count: u32,
        #[serde(rename = "coordinateFormat")]
        coordinate_format: u8,
    },

    #[serde(rename = "update")]
    Update {
        id: String,
        position: Position,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    #[serde(rename = "update:size")]
    UpdateSize {
        id: String,
        size: [f32; 3],
        #[serde(skip_serializing_if = "Option::is_none")]
        generation: Option<u64>,
    },
    #[serde(rename = "remove")]
    Remove { id: String },

    #[serde(rename = "meter:object")]
    MeterObject {
        id: String,
        #[serde(rename = "peakDbfs")]
        peak_dbfs: f64,
        #[serde(rename = "rmsDbfs")]
        rms_dbfs: f64,
    },

    #[serde(rename = "meter:object:gains")]
    MeterObjectGains { id: String, gains: Vec<f64> },

    #[serde(rename = "meter:object:band_gains")]
    MeterObjectBandGains {
        id: String,
        band: usize,
        gains: Vec<f64>,
    },

    #[serde(rename = "meter:speaker")]
    MeterSpeaker {
        id: String,
        #[serde(rename = "peakDbfs")]
        peak_dbfs: f64,
        #[serde(rename = "rmsDbfs")]
        rms_dbfs: f64,
    },

    #[serde(rename = "meter:master")]
    MeterMaster {
        #[serde(rename = "peakDbfs")]
        peak_dbfs: f64,
        #[serde(rename = "rmsDbfs")]
        rms_dbfs: f64,
    },

    #[serde(rename = "meter:drc_gain")]
    MeterDrcGain { value: f64 },

    #[serde(rename = "state:speaker:gain")]
    StateSpeakerGain { id: String, gain: f64 },
    #[serde(rename = "state:speaker:delay")]
    StateSpeakerDelay { id: String, delay_ms: f64 },
    #[serde(rename = "state:object:mute")]
    StateObjectMute { id: String, muted: bool },
    #[serde(rename = "state:object:source_tag")]
    StateObjectSourceTag { id: String, source_tag: String },
    #[serde(rename = "state:speaker:mute")]
    StateSpeakerMute { id: String, muted: bool },
    #[serde(rename = "state:speaker:spatialize")]
    StateSpeakerSpatialize { id: String, spatialize: bool },
    #[serde(rename = "state:speaker:name")]
    StateSpeakerName { id: String, name: String },
    #[serde(rename = "state:speaker:freq_low")]
    StateSpeakerFreqLow { id: String, freq_low: Option<f32> },
    #[serde(rename = "state:speaker:freq_high")]
    StateSpeakerFreqHigh { id: String, freq_high: Option<f32> },

    #[serde(rename = "state:capabilities")]
    StateCapabilities { value: String },
    #[serde(rename = "state:renderer")]
    StateRenderer { value: String },
    #[serde(rename = "state:head_pose")]
    StateHeadPose { w: f32, x: f32, y: f32, z: f32 },
    #[serde(rename = "state:clip")]
    StateClip { speaker: i32 },
    #[serde(rename = "state:audio")]
    StateAudio { value: String },
    #[serde(rename = "state:layout")]
    StateLayout { value: String },
    #[serde(rename = "state:speakers")]
    StateSpeakers { value: String },
    #[serde(rename = "state:input")]
    StateInput { value: String },
    #[serde(rename = "state:loudness")]
    StateLoudness { value: String },
    #[serde(rename = "state:monitoring")]
    StateMonitoring { value: String },
    #[serde(rename = "state:session")]
    StateSession { value: String },
    #[serde(rename = "state:debug:speaker_gaintable:meta")]
    StateDebugSpeakerGaintableMeta { value: String },
    #[serde(rename = "state:debug:speaker_gaintable:chunk")]
    StateDebugSpeakerGaintableChunk { bytes: Vec<u8> },
    #[serde(rename = "state:debug:speaker_gaintable:unavailable")]
    StateDebugSpeakerGaintableUnavailable { value: String },
    #[serde(rename = "state:debug:speaker_gaintable:uptodate")]
    StateDebugSpeakerGaintableUptodate { version: i32 },
    #[serde(rename = "state:snapshot_complete")]
    StateSnapshotComplete,
    #[serde(rename = "state:realtime:master_gain")]
    StateRealtimeMasterGain { value: f64, seq: i32 },
    #[serde(rename = "state:realtime:speaker_gain")]
    StateRealtimeSpeakerGain { id: String, value: f64, seq: i32 },
    #[serde(rename = "state:latency")]
    StateLatency { value: f64 },
    #[serde(rename = "state:latency:instant")]
    StateLatencyInstant { value: f64 },
    #[serde(rename = "state:latency:control")]
    StateLatencyControl { value: f64 },
    #[serde(rename = "state:latency:smoothed")]
    StateLatencySmoothed { value: f64 },
    #[serde(rename = "state:latency:downstream")]
    StateLatencyDownstream { value: f64 },
    #[serde(rename = "state:latency:target")]
    StateLatencyTarget { value: f64 },
    #[serde(rename = "state:latency:target_requested")]
    StateLatencyTargetRequested { value: f64 },
    #[serde(rename = "state:latency:avail_input")]
    StateLatencyAvailInput { value: f64 },
    #[serde(rename = "state:latency:output_fifo")]
    StateLatencyOutputFifo { value: f64 },
    #[serde(rename = "state:latency:resampler_pending")]
    StateLatencyResamplerPending { value: f64 },
    #[serde(rename = "state:diag:schema")]
    StateDiagSchema { value: String },
    #[serde(rename = "state:diag:values")]
    StateDiagValues { value: String },
    #[serde(rename = "state:object_generators")]
    StateObjectGenerators { value: String },
    #[serde(rename = "state:phantom")]
    StatePhantom { value: String },
    #[serde(rename = "state:options_schema")]
    StateOptionsSchema { value: String },
    #[serde(rename = "state:decode_time_ms")]
    StateDecodeTimeMs { value: f64 },
    #[serde(rename = "state:render_time_ms")]
    StateRenderTimeMs { value: f64 },
    #[serde(rename = "state:crossover_time_ms")]
    StateCrossoverTimeMs { value: f64 },
    #[serde(rename = "state:write_time_ms")]
    StateWriteTimeMs { value: f64 },
    #[serde(rename = "state:frame_duration_ms")]
    StateFrameDurationMs { value: f64 },
    #[serde(rename = "state:resample_ratio")]
    StateResampleRatio { value: f64 },
    #[serde(rename = "state:render:bridge_path")]
    StateRenderBridgePath { value: String },
    #[serde(rename = "state:render:config_path")]
    StateRenderConfigPath { value: String },
    #[serde(rename = "state:render:config_status")]
    StateRenderConfigStatus { value: String },
    #[serde(rename = "state:render:version")]
    StateRenderVersion { value: String },
    #[serde(rename = "state:render:abi")]
    StateRenderAbi { value: String },
    #[serde(rename = "state:render:bridge_error")]
    StateRenderBridgeError { value: String },
    #[serde(rename = "state:input_pipe")]
    StateInputPipe { value: String },
    #[serde(rename = "state:osc:metering")]
    StateOscMetering { enabled: bool },
    #[serde(rename = "state:log_level")]
    StateLogLevel { value: String },
    #[serde(rename = "log")]
    Log { entry: LogEntry },
    #[serde(rename = "state:render_evaluation:cartesian:x_size")]
    StateRenderEvaluationCartesianXSize { value: u32 },
    #[serde(rename = "state:render_evaluation:cartesian:y_size")]
    StateRenderEvaluationCartesianYSize { value: u32 },
    #[serde(rename = "state:render_evaluation:cartesian:z_size")]
    StateRenderEvaluationCartesianZSize { value: u32 },
    #[serde(rename = "state:render_evaluation:cartesian:z_neg_size")]
    StateRenderEvaluationCartesianZNegSize { value: u32 },
    #[serde(rename = "state:render_evaluation:polar:azimuth_resolution")]
    StateRenderEvaluationPolarAzimuthResolution { value: u32 },
    #[serde(rename = "state:render_evaluation:polar:elevation_resolution")]
    StateRenderEvaluationPolarElevationResolution { value: u32 },
    #[serde(rename = "state:render_evaluation:polar:distance_res")]
    StateRenderEvaluationPolarDistanceRes { value: u32 },
    #[serde(rename = "state:render_evaluation:polar:distance_max")]
    StateRenderEvaluationPolarDistanceMax { value: f64 },
    #[serde(rename = "state:render_evaluation:position_interpolation")]
    StateRenderEvaluationPositionInterpolation { enabled: bool },
    #[serde(rename = "state:vbap:allow_negative_z")]
    StateVbapAllowNegativeZ { enabled: bool },
    #[serde(rename = "state:speakers:recomputing")]
    StateSpeakersRecomputing { enabled: bool },
    #[serde(rename = "state:speakers:recompute_error")]
    StateSpeakersRecomputeError { message: String },
    #[serde(rename = "state:backend:file:content")]
    StateBackendFileContent {
        backend: String,
        key: String,
        name: String,
        content: String,
    },
    #[serde(rename = "state:backend:file:list")]
    StateBackendFileList { backend: String, json: String },
    #[serde(rename = "state:backend:file:error")]
    StateBackendFileError {
        backend: String,
        key: String,
        message: String,
    },
    #[serde(rename = "state:config:save_error")]
    StateConfigSaveError { message: String },
    #[serde(rename = "state:adaptive_resampling")]
    StateAdaptiveResampling { enabled: bool },
    #[serde(rename = "state:adaptive_resampling:enable_far_mode")]
    StateAdaptiveResamplingEnableFarMode { enabled: bool },
    #[serde(rename = "state:adaptive_resampling:force_silence_in_far_mode")]
    StateAdaptiveResamplingForceSilenceInFarMode { enabled: bool },
    #[serde(rename = "state:adaptive_resampling:hard_recover_high_in_far_mode")]
    StateAdaptiveResamplingHardRecoverHighInFarMode { enabled: bool },
    #[serde(rename = "state:adaptive_resampling:hard_recover_low_in_far_mode")]
    StateAdaptiveResamplingHardRecoverLowInFarMode { enabled: bool },
    #[serde(rename = "state:adaptive_resampling:far_mode_return_fade_in_ms")]
    StateAdaptiveResamplingFarModeReturnFadeInMs { value: f64 },
    #[serde(rename = "state:adaptive_resampling:kp_near")]
    StateAdaptiveResamplingKpNear { value: f64 },
    #[serde(rename = "state:adaptive_resampling:ki")]
    StateAdaptiveResamplingKi { value: f64 },
    #[serde(rename = "state:adaptive_resampling:integral_discharge_ratio")]
    StateAdaptiveResamplingIntegralDischargeRatio { value: f64 },
    #[serde(rename = "state:adaptive_resampling:max_adjust")]
    StateAdaptiveResamplingMaxAdjust { value: f64 },
    #[serde(rename = "state:adaptive_resampling:update_interval_callbacks")]
    StateAdaptiveResamplingUpdateIntervalCallbacks { value: f64 },
    #[serde(rename = "state:adaptive_resampling:high_recover_entry_margin_ms")]
    StateAdaptiveResamplingHighRecoverEntryMarginMs { value: f64 },
    #[serde(rename = "state:adaptive_resampling:band")]
    StateAdaptiveResamplingBand { value: String },
    #[serde(rename = "state:adaptive_resampling:state")]
    StateAdaptiveResamplingState { value: String },
    #[serde(rename = "state:adaptive_resampling:pause")]
    StateAdaptiveResamplingPaused { enabled: bool },
    #[serde(rename = "state:config:saved")]
    StateConfigSaved { saved: bool },
}

// ── sub-parsers ─────────────────────────────────────────────────────────────

fn parse_omniphony_object_position(
    parts: &[&str],
    args: &[f64],
    raw_args: &[OscType],
    coordinate_format: CoordinateFormat,
) -> Option<OscEvent> {
    if !parts.contains(&"omniphony") || !parts.contains(&"object") {
        return None;
    }
    let explicit_cartesian = parts.contains(&"xyz");
    let explicit_polar =
        parts.contains(&"aed") || parts.contains(&"spherical") || parts.contains(&"polar");
    if !explicit_cartesian && !explicit_polar {
        return None;
    }

    let id = find_id_in_address(parts)?;
    let x = to_number(args[0])?;
    let y = to_number(args[1])?;
    let z = to_number(args[2])?;

    let direct_speaker_index = args
        .get(3)
        .copied()
        .and_then(to_number)
        .map(|v| v as i64)
        .filter(|&v| v >= 0)
        .map(|v| v as u32);
    let gain_db = args.get(4).copied().and_then(to_number).map(|v| v as i32);

    // Layout after `divergence` removal:
    //   [pos0, pos1, pos2, speaker_idx, gain, priority, ramp, gen, name]  (9 args)
    // Previous payloads carried `divergence` at index 6, shifting the trailing
    // fields by one slot. Probe both layouts for backward compatibility.
    let generation = match raw_args.get(7) {
        Some(OscType::Long(v)) if *v >= 0 => Some(*v as u64),
        Some(OscType::Int(v)) if *v >= 0 => Some(*v as u64),
        _ => match raw_args.get(8) {
            Some(OscType::Long(v)) if *v >= 0 => Some(*v as u64),
            Some(OscType::Int(v)) if *v >= 0 => Some(*v as u64),
            _ => None,
        },
    };

    let name_idx = if raw_args.len() >= 10 {
        9
    } else if raw_args.len() >= 9 {
        8
    } else {
        7
    };
    let name = raw_args
        .get(name_idx)
        .and_then(|a| unwrap_string(a))
        .filter(|s| !s.trim().is_empty());

    let payload_format = if explicit_cartesian {
        CoordinateFormat::Cartesian
    } else if explicit_polar {
        CoordinateFormat::Polar
    } else {
        coordinate_format
    };

    Some(OscEvent::Update {
        id,
        position: Position {
            x: if payload_format == CoordinateFormat::Cartesian {
                x
            } else {
                0.0
            },
            y: if payload_format == CoordinateFormat::Cartesian {
                y
            } else {
                0.0
            },
            z: if payload_format == CoordinateFormat::Cartesian {
                z
            } else {
                0.0
            },
            coord_mode: if payload_format == CoordinateFormat::Cartesian {
                "cartesian".to_string()
            } else {
                "polar".to_string()
            },
            azimuth_deg: if payload_format == CoordinateFormat::Polar {
                Some(x)
            } else {
                None
            },
            elevation_deg: if payload_format == CoordinateFormat::Polar {
                Some(y)
            } else {
                None
            },
            distance_m: if payload_format == CoordinateFormat::Polar {
                Some(z.max(0.0))
            } else {
                None
            },
            gain_db,
            generation,
            direct_speaker_index,
            source_tag: None,
        },
        name,
    })
}

fn parse_omniphony_object_size(
    parts: &[&str],
    args: &[f64],
    raw_args: &[OscType],
) -> Option<OscEvent> {
    if !parts.contains(&"omniphony") || !parts.contains(&"object") || !parts.contains(&"size") {
        return None;
    }
    let id = find_id_in_address(parts)?;
    let w = to_number(args.get(0).copied()?)?.clamp(0.0, 1.0) as f32;
    let d = to_number(args.get(1).copied()?)?.clamp(0.0, 1.0) as f32;
    let h = to_number(args.get(2).copied()?)?.clamp(0.0, 1.0) as f32;
    let generation = match raw_args.get(3) {
        Some(OscType::Long(v)) if *v >= 0 => Some(*v as u64),
        Some(OscType::Int(v)) if *v >= 0 => Some(*v as u64),
        _ => None,
    };
    Some(OscEvent::UpdateSize {
        id,
        size: [w, d, h],
        generation,
    })
}

fn parse_omniphony_spatial_frame(parts: &[&str], args: &[f64]) -> Option<OscEvent> {
    if parts.len() != 3 || parts[0] != "omniphony" || parts[1] != "spatial" || parts[2] != "frame" {
        return None;
    }
    let sample_pos = to_number(args[0])? as i64;
    let (generation, count_index, format_index) = if args.len() >= 4 {
        (to_number(args[1])? as u64, 2usize, 3usize)
    } else {
        (0u64, 1usize, 2usize)
    };
    let object_count_raw = to_number(args[count_index])?;
    let object_count = object_count_raw.max(0.0) as u32;
    let coordinate_format = match args
        .get(format_index)
        .copied()
        .and_then(to_number)
        .unwrap_or(0.0) as i64
    {
        1 => 1u8,
        _ => 0u8,
    };
    Some(OscEvent::SpatialFrame {
        sample_pos,
        generation,
        object_count,
        coordinate_format,
    })
}

fn parse_omniphony_log(parts: &[&str], raw_args: &[OscType]) -> Option<OscEvent> {
    if parts.len() != 2 || parts[0] != "omniphony" || parts[1] != "log" {
        return None;
    }
    let seq = match raw_args.first()? {
        OscType::Long(v) if *v >= 0 => *v as u64,
        OscType::Int(v) if *v >= 0 => *v as u64,
        _ => return None,
    };
    let level = raw_args.get(1).and_then(unwrap_string)?;
    let target = raw_args.get(2).and_then(unwrap_string)?;
    let message = raw_args.get(3).and_then(unwrap_string)?;
    Some(OscEvent::Log {
        entry: LogEntry {
            seq,
            level,
            target,
            message,
        },
    })
}

fn parse_omniphony_state(parts: &[&str], args: &[f64], raw_args: &[OscType]) -> Option<OscEvent> {
    if parts.len() < 3 || parts[0] != "omniphony" || parts[1] != "state" {
        return None;
    }

    match (parts.len(), parts[2]) {
        (3, "latency") => Some(OscEvent::StateLatency {
            value: to_number(args[0])?,
        }),
        (3, "latency_instant") => Some(OscEvent::StateLatencyInstant {
            value: to_number(args[0])?,
        }),
        (3, "latency_control") => Some(OscEvent::StateLatencyControl {
            value: to_number(args[0])?,
        }),
        (3, "latency_smoothed") => Some(OscEvent::StateLatencySmoothed {
            value: to_number(args[0])?,
        }),
        (3, "latency_downstream") => Some(OscEvent::StateLatencyDownstream {
            value: to_number(args[0])?,
        }),
        (3, "latency_target") => Some(OscEvent::StateLatencyTarget {
            value: to_number(args[0])?,
        }),
        (3, "latency_target_requested") => Some(OscEvent::StateLatencyTargetRequested {
            value: to_number(args[0])?,
        }),
        (3, "latency_avail_input") => Some(OscEvent::StateLatencyAvailInput {
            value: to_number(args[0])?,
        }),
        (3, "latency_output_fifo") => Some(OscEvent::StateLatencyOutputFifo {
            value: to_number(args[0])?,
        }),
        (3, "latency_resampler_pending") => Some(OscEvent::StateLatencyResamplerPending {
            value: to_number(args[0])?,
        }),
        (3, "diag_schema") => Some(OscEvent::StateDiagSchema {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "object_generators") => Some(OscEvent::StateObjectGenerators {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "phantom") => Some(OscEvent::StatePhantom {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "options_schema") => Some(OscEvent::StateOptionsSchema {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "diag_values") => Some(OscEvent::StateDiagValues {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "decode_time_ms") => Some(OscEvent::StateDecodeTimeMs {
            value: to_number(args[0])?,
        }),
        (3, "render_time_ms") => Some(OscEvent::StateRenderTimeMs {
            value: to_number(args[0])?,
        }),
        (3, "crossover_time_ms") => Some(OscEvent::StateCrossoverTimeMs {
            value: to_number(args[0])?,
        }),
        (3, "write_time_ms") => Some(OscEvent::StateWriteTimeMs {
            value: to_number(args[0])?,
        }),
        (3, "frame_duration_ms") => Some(OscEvent::StateFrameDurationMs {
            value: to_number(args[0])?,
        }),
        (3, "resample_ratio") => Some(OscEvent::StateResampleRatio {
            value: to_number(args[0])?,
        }),
        (3, "log_level") => Some(OscEvent::StateLogLevel {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "capabilities") => Some(OscEvent::StateCapabilities {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "renderer") => Some(OscEvent::StateRenderer {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        // Lightweight ~30 Hz head-pose channel (w, x, y, z) for the 3D head;
        // the full renderer state stays at 10 Hz.
        (3, "head_pose") if args.len() >= 4 => Some(OscEvent::StateHeadPose {
            w: to_number(args[0])? as f32,
            x: to_number(args[1])? as f32,
            y: to_number(args[2])? as f32,
            z: to_number(args[3])? as f32,
        }),
        (3, "clip") => Some(OscEvent::StateClip {
            speaker: match raw_args.first() {
                Some(OscType::Int(v)) => *v,
                _ => -1,
            },
        }),
        (3, "audio") => Some(OscEvent::StateAudio {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "layout") => Some(OscEvent::StateLayout {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "speakers") => Some(OscEvent::StateSpeakers {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "input") => Some(OscEvent::StateInput {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "session") => Some(OscEvent::StateSession {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "snapshot_complete") => Some(OscEvent::StateSnapshotComplete),
        (3, "loudness") => Some(OscEvent::StateLoudness {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "monitoring") => Some(OscEvent::StateMonitoring {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (4, "realtime") => match parts[3] {
            "master_gain" => Some(OscEvent::StateRealtimeMasterGain {
                value: to_number(args.first().copied()?)?,
                seq: to_number(args.get(1).copied()?)? as i32,
            }),
            "speaker_gain" => Some(OscEvent::StateRealtimeSpeakerGain {
                id: to_number(args.first().copied()?)?.round().to_string(),
                value: to_number(args.get(1).copied()?)?,
                seq: to_number(args.get(2).copied()?)? as i32,
            }),
            _ => None,
        },
        (5, "debug") if parts[3] == "speaker_gaintable" => match parts[4] {
            "meta" => Some(OscEvent::StateDebugSpeakerGaintableMeta {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "unavailable" => Some(OscEvent::StateDebugSpeakerGaintableUnavailable {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "chunk" => Some(OscEvent::StateDebugSpeakerGaintableChunk {
                bytes: raw_args.first().and_then(unwrap_blob)?,
            }),
            "uptodate" => Some(OscEvent::StateDebugSpeakerGaintableUptodate {
                version: to_number(args.first().copied()?)? as i32,
            }),
            _ => None,
        },
        (5, "render_evaluation") if parts[3] == "cartesian" => {
            let value = to_number(args[0])?.max(0.0) as u32;
            match parts[4] {
                "x_size" => Some(OscEvent::StateRenderEvaluationCartesianXSize { value }),
                "y_size" => Some(OscEvent::StateRenderEvaluationCartesianYSize { value }),
                "z_size" => Some(OscEvent::StateRenderEvaluationCartesianZSize { value }),
                "z_neg_size" => Some(OscEvent::StateRenderEvaluationCartesianZNegSize { value }),
                _ => None,
            }
        }
        (4, "render_evaluation") if parts[3] == "position_interpolation" => {
            Some(OscEvent::StateRenderEvaluationPositionInterpolation {
                enabled: to_number(args[0])? != 0.0,
            })
        }
        (5, "render_evaluation") if parts[3] == "polar" => match parts[4] {
            "azimuth_resolution" => {
                let value = to_number(args[0])?.max(0.0) as u32;
                Some(OscEvent::StateRenderEvaluationPolarAzimuthResolution { value })
            }
            "elevation_resolution" => {
                let value = to_number(args[0])?.max(0.0) as u32;
                Some(OscEvent::StateRenderEvaluationPolarElevationResolution { value })
            }
            "distance_res" => Some(OscEvent::StateRenderEvaluationPolarDistanceRes {
                value: to_number(args[0])?.max(0.0) as u32,
            }),
            "distance_max" => Some(OscEvent::StateRenderEvaluationPolarDistanceMax {
                value: to_number(args[0])?.max(0.0),
            }),
            _ => None,
        },
        (4, "vbap") if parts[3] == "allow_negative_z" => Some(OscEvent::StateVbapAllowNegativeZ {
            enabled: to_number(args[0])? != 0.0,
        }),
        (4, "speakers") if parts[3] == "recomputing" => Some(OscEvent::StateSpeakersRecomputing {
            enabled: to_number(args[0])? != 0.0,
        }),
        (4, "speakers") if parts[3] == "recompute_error" => {
            Some(OscEvent::StateSpeakersRecomputeError {
                message: raw_args.first().and_then(unwrap_string).unwrap_or_default(),
            })
        }
        (4, "config") if parts[3] == "save_error" => Some(OscEvent::StateConfigSaveError {
            message: raw_args.first().and_then(unwrap_string).unwrap_or_default(),
        }),
        (5, "backend") if parts[3] == "file" => match parts[4] {
            // [backend_id, key, name, content]
            "content" => Some(OscEvent::StateBackendFileContent {
                backend: raw_args.first().and_then(unwrap_string)?,
                key: raw_args.get(1).and_then(unwrap_string)?,
                name: raw_args.get(2).and_then(unwrap_string).unwrap_or_default(),
                content: raw_args.get(3).and_then(unwrap_string).unwrap_or_default(),
            }),
            // [backend_id, json_names]
            "list" => Some(OscEvent::StateBackendFileList {
                backend: raw_args.first().and_then(unwrap_string)?,
                json: raw_args.get(1).and_then(unwrap_string).unwrap_or_default(),
            }),
            // [backend_id, key, message]
            "error" => Some(OscEvent::StateBackendFileError {
                backend: raw_args.first().and_then(unwrap_string)?,
                key: raw_args.get(1).and_then(unwrap_string)?,
                message: raw_args.get(2).and_then(unwrap_string).unwrap_or_default(),
            }),
            _ => None,
        },
        (3, "adaptive_resampling") => Some(OscEvent::StateAdaptiveResampling {
            enabled: to_number(args[0])? != 0.0,
        }),
        (4, "adaptive_resampling") => match parts[3] {
            "enable_far_mode" => Some(OscEvent::StateAdaptiveResamplingEnableFarMode {
                enabled: to_number(args[0])? != 0.0,
            }),
            "force_silence_in_far_mode" => {
                Some(OscEvent::StateAdaptiveResamplingForceSilenceInFarMode {
                    enabled: to_number(args[0])? != 0.0,
                })
            }
            "hard_recover_in_far_mode" | "hard_recover_high_in_far_mode" => {
                Some(OscEvent::StateAdaptiveResamplingHardRecoverHighInFarMode {
                    enabled: to_number(args[0])? != 0.0,
                })
            }
            "hard_recover_low_in_far_mode" => {
                Some(OscEvent::StateAdaptiveResamplingHardRecoverLowInFarMode {
                    enabled: to_number(args[0])? != 0.0,
                })
            }
            "far_mode_return_fade_in_ms" => {
                Some(OscEvent::StateAdaptiveResamplingFarModeReturnFadeInMs {
                    value: to_number(args[0])?,
                })
            }
            "kp_near" => Some(OscEvent::StateAdaptiveResamplingKpNear {
                value: to_number(args[0])?,
            }),
            "ki" => Some(OscEvent::StateAdaptiveResamplingKi {
                value: to_number(args[0])?,
            }),
            "integral_discharge_ratio" => {
                Some(OscEvent::StateAdaptiveResamplingIntegralDischargeRatio {
                    value: to_number(args[0])?,
                })
            }
            "max_adjust" => Some(OscEvent::StateAdaptiveResamplingMaxAdjust {
                value: to_number(args[0])?,
            }),
            "update_interval_callbacks" => {
                Some(OscEvent::StateAdaptiveResamplingUpdateIntervalCallbacks {
                    value: to_number(args[0])?,
                })
            }
            "high_recover_entry_margin_ms" => {
                Some(OscEvent::StateAdaptiveResamplingHighRecoverEntryMarginMs {
                    value: to_number(args[0])?,
                })
            }
            "band" => Some(OscEvent::StateAdaptiveResamplingBand {
                value: unwrap_string(raw_args.first()?)?,
            }),
            "state" => Some(OscEvent::StateAdaptiveResamplingState {
                value: unwrap_string(raw_args.first()?)?,
            }),
            "pause" => Some(OscEvent::StateAdaptiveResamplingPaused {
                enabled: to_number(args[0])? != 0.0,
            }),
            _ => None,
        },
        (4, "config") if parts[3] == "saved" => Some(OscEvent::StateConfigSaved {
            saved: to_number(args[0])? != 0.0,
        }),
        (4, "render") if parts[3] == "bridge_path" => Some(OscEvent::StateRenderBridgePath {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (4, "render") if parts[3] == "config_path" => Some(OscEvent::StateRenderConfigPath {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (4, "render") if parts[3] == "config_status" => Some(OscEvent::StateRenderConfigStatus {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (4, "render") if parts[3] == "version" => Some(OscEvent::StateRenderVersion {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        // C-ABI version of the liborender shim hosting the engine
        // ("major.minor"); empty when the engine is linked as a Rust crate.
        (4, "render") if parts[3] == "abi" => Some(OscEvent::StateRenderAbi {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (4, "render") if parts[3] == "bridge_error" => Some(OscEvent::StateRenderBridgeError {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "input_pipe") => {
            let value = raw_args.first().and_then(unwrap_string)?;
            Some(OscEvent::StateInputPipe { value })
        }
        (4, "osc") if parts[3] == "metering" => Some(OscEvent::StateOscMetering {
            enabled: to_number(args[0])? != 0.0,
        }),
        (5, kind) if kind == "object" || kind == "speaker" => match parts[4] {
            "gain" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let gain = clamp(to_number(args[0])?, 0.0, 2.0);
                Some(OscEvent::StateSpeakerGain { id, gain })
            }
            "delay" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let delay_ms = clamp(to_number(args[0])?, 0.0, 10_000.0);
                Some(OscEvent::StateSpeakerDelay { id, delay_ms })
            }
            "mute" => {
                let id = if kind == "speaker" {
                    parts[3].parse::<u32>().ok()?.to_string()
                } else {
                    parts[3].to_string()
                };
                let muted = to_number(args[0])? != 0.0;
                if kind == "speaker" {
                    Some(OscEvent::StateSpeakerMute { id, muted })
                } else {
                    Some(OscEvent::StateObjectMute { id, muted })
                }
            }
            "source_tag" if kind == "object" => Some(OscEvent::StateObjectSourceTag {
                id: parts[3].to_string(),
                source_tag: raw_args.first().and_then(unwrap_string)?,
            }),
            "spatialize" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let spatialize = to_number(args[0])? != 0.0;
                Some(OscEvent::StateSpeakerSpatialize { id, spatialize })
            }
            "name" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let name = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateSpeakerName { id, name })
            }
            "freq_low" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let freq_low =
                    to_number(args[0]).and_then(|v| if v > 0.0 { Some(v as f32) } else { None });
                Some(OscEvent::StateSpeakerFreqLow { id, freq_low })
            }
            "freq_high" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let freq_high =
                    to_number(args[0]).and_then(|v| if v > 0.0 { Some(v as f32) } else { None });
                Some(OscEvent::StateSpeakerFreqHigh { id, freq_high })
            }
            _ => None,
        },
        _ => None,
    }
}

fn parse_meter(parts: &[&str], args: &[f64]) -> Option<OscEvent> {
    let meter_idx = parts.iter().position(|&p| p == "meter")?;
    let after = &parts[meter_idx..];

    // band gains: meter / object / {id} / band / {b} / gains
    if after.len() >= 6 && after[1] == "object" && after[3] == "band" && after[5] == "gains" {
        let id = after[2].to_string();
        let band: usize = after[4].parse().ok()?;
        let gains: Vec<f64> = args.iter().map(|&v| clamp(v, 0.0, 1.0)).collect();
        return Some(OscEvent::MeterObjectBandGains { id, band, gains });
    }

    // gains sub-message: meter / object / {id} / gains
    if after.len() >= 4 && after[1] == "object" && after[3] == "gains" {
        let id = after[2].to_string();
        let gains: Vec<f64> = args.iter().map(|&v| clamp(v, 0.0, 1.0)).collect();
        return Some(OscEvent::MeterObjectGains { id, gains });
    }

    if after.len() >= 3 {
        let kind = after[1];
        let id = after[2].to_string();
        // Peak ceiling is left high (+24 dBFS) so true over-0 dBFS peaks
        // (clipping) reach the UI instead of being flattened to 0; the RMS that
        // drives the bar stays bounded for a clean fill.
        let peak = clamp(to_number(args[0]).unwrap_or(-100.0), -100.0, 24.0);
        let rms = clamp(to_number(args[1]).unwrap_or(-100.0), -100.0, 0.0);
        match kind {
            "object" => {
                return Some(OscEvent::MeterObject {
                    id,
                    peak_dbfs: peak,
                    rms_dbfs: rms,
                })
            }
            "speaker" => {
                return Some(OscEvent::MeterSpeaker {
                    id,
                    peak_dbfs: peak,
                    rms_dbfs: rms,
                })
            }
            _ => {}
        }
    }

    if after.len() == 2 && after[1] == "master" {
        let peak = clamp(
            args.get(0).copied().and_then(to_number).unwrap_or(-100.0),
            -100.0,
            24.0,
        );
        let rms = clamp(
            args.get(1).copied().and_then(to_number).unwrap_or(-100.0),
            -100.0,
            0.0,
        );
        return Some(OscEvent::MeterMaster {
            peak_dbfs: peak,
            rms_dbfs: rms,
        });
    }

    if after.len() == 2 && after[1] == "drc_gain" {
        return Some(OscEvent::MeterDrcGain {
            value: args.get(0).copied().unwrap_or(1.0),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{parse_osc_message, CoordinateFormat, OscEvent};
    use rosc::OscType;

    #[test]
    fn parses_state_speaker_freq_high() {
        let parsed = parse_osc_message(
            "/omniphony/state/speaker/3/freq_high",
            &[OscType::Float(12000.0)],
            CoordinateFormat::Cartesian,
        );
        assert!(matches!(
            parsed,
            Some(OscEvent::StateSpeakerFreqHigh {
                id,
                freq_high: Some(value)
            }) if id == "3" && (value - 12000.0).abs() < f32::EPSILON
        ));
    }
}

// ── public entry point ───────────────────────────────────────────────────────

pub fn parse_osc_message(
    address: &str,
    raw_args: &[OscType],
    coordinate_format: CoordinateFormat,
) -> Option<OscEvent> {
    let parts_owned: Vec<String> = address
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect();
    let parts: Vec<&str> = parts_owned.iter().map(|s| s.as_str()).collect();

    let args: Vec<f64> = raw_args.iter().map(|a| unwrap_arg(a)).collect();

    // omniphony object position (xyz or aed)
    if let Some(ev) = parse_omniphony_object_position(&parts, &args, raw_args, coordinate_format) {
        return Some(ev);
    }

    // omniphony object size
    if let Some(ev) = parse_omniphony_object_size(&parts, &args, raw_args) {
        return Some(ev);
    }

    // omniphony spatial frame
    if let Some(ev) = parse_omniphony_spatial_frame(&parts, &args) {
        return Some(ev);
    }

    if let Some(ev) = parse_omniphony_log(&parts, raw_args) {
        return Some(ev);
    }

    // omniphony state
    if let Some(ev) = parse_omniphony_state(&parts, &args, raw_args) {
        return Some(ev);
    }

    // meters
    if parts.contains(&"meter") {
        return parse_meter(&parts, &args);
    }

    // remove
    if parts
        .iter()
        .any(|&p| p == "remove" || p == "delete" || p == "off")
    {
        let id_from_arg = if !args.is_empty() {
            Some(args[0].to_string())
        } else {
            None
        };
        let id = id_from_arg.or_else(|| find_id_in_address(&parts))?;
        return Some(OscEvent::Remove { id });
    }

    // generic position (cartesian / spherical)
    let id = {
        let from_addr = find_id_in_address(&parts);
        if from_addr.is_none() && args.len() >= 4 {
            Some(args[0].to_string())
        } else {
            from_addr
        }
    }?;

    let numeric_args: Vec<f64> = if find_id_in_address(&parts).is_none() && raw_args.len() >= 4 {
        args[1..]
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .collect()
    } else {
        args.iter().copied().filter(|v| v.is_finite()).collect()
    };

    if numeric_args.len() < 3 {
        return None;
    }

    let has_spherical = parts
        .iter()
        .any(|&p| matches!(p, "aed" | "spherical" | "polar" | "angles"));

    let (x, y, z) = if has_spherical {
        let (px, py, pz) =
            spherical_to_cartesian(numeric_args[0], numeric_args[1], numeric_args[2]);
        (px, py, pz)
    } else {
        (numeric_args[0], numeric_args[1], numeric_args[2])
    };

    Some(OscEvent::Update {
        id,
        position: Position {
            x,
            y,
            z,
            coord_mode: if has_spherical {
                "polar".to_string()
            } else {
                "cartesian".to_string()
            },
            azimuth_deg: if has_spherical {
                Some(numeric_args[0])
            } else {
                None
            },
            elevation_deg: if has_spherical {
                Some(numeric_args[1])
            } else {
                None
            },
            distance_m: if has_spherical {
                Some(numeric_args[2].max(0.0))
            } else {
                None
            },
            gain_db: None,
            generation: None,
            direct_speaker_index: None,
            source_tag: None,
        },
        name: None,
    })
}

pub fn is_heartbeat_address(address: &str) -> HeartbeatResponse {
    let lower = address.to_lowercase();
    if lower == "/omniphony/heartbeat/ack" {
        HeartbeatResponse::Ack
    } else if lower == "/omniphony/heartbeat/unknown" {
        HeartbeatResponse::Unknown
    } else {
        HeartbeatResponse::None
    }
}

pub enum HeartbeatResponse {
    Ack,
    Unknown,
    None,
}
