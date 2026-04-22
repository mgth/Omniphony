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

fn omniphony_speaker_to_scene(az_deg: f64, el_deg: f64, dist: f64) -> (f64, f64, f64) {
    spherical_to_cartesian(az_deg, el_deg, dist)
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
pub struct SpeakerPosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(rename = "azimuthDeg")]
    pub azimuth_deg: f64,
    #[serde(rename = "elevationDeg")]
    pub elevation_deg: f64,
    #[serde(rename = "distanceM")]
    pub distance_m: f64,
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
    #[serde(rename = "remove")]
    Remove { id: String },

    #[serde(rename = "config:speakers:count")]
    ConfigSpeakersCount { count: u32 },

    #[serde(rename = "config:speaker")]
    ConfigSpeaker {
        index: u32,
        name: String,
        #[serde(rename = "azimuthDeg")]
        azimuth_deg: f64,
        #[serde(rename = "elevationDeg")]
        elevation_deg: f64,
        #[serde(rename = "distanceM")]
        distance_m: f64,
        #[serde(rename = "coordMode")]
        coord_mode: String,
        x: f64,
        y: f64,
        z: f64,
        #[serde(rename = "delayMs")]
        delay_ms: f64,
        spatialize: u8,
        #[serde(rename = "freqLow")]
        freq_low: Option<f32>,
        #[serde(rename = "freqHigh")]
        freq_high: Option<f32>,
        position: SpeakerPosition,
    },

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

    #[serde(rename = "state:room_ratio")]
    StateRoomRatio {
        width: f64,
        length: f64,
        height: f64,
    },
    #[serde(rename = "state:room_ratio:rear")]
    StateRoomRatioRear { value: f64 },
    #[serde(rename = "state:room_ratio:lower")]
    StateRoomRatioLower { value: f64 },
    #[serde(rename = "state:room_ratio:center_blend")]
    StateRoomRatioCenterBlend { value: f64 },
    #[serde(rename = "state:layout:radius_m")]
    StateLayoutRadiusM { value: f64 },
    #[serde(rename = "state:spread:min")]
    StateSpreadMin { value: f64 },
    #[serde(rename = "state:spread:max")]
    StateSpreadMax { value: f64 },
    #[serde(rename = "state:spread:from_distance")]
    StateSpreadFromDistance { enabled: bool },
    #[serde(rename = "state:spread:distance_range")]
    StateSpreadDistanceRange { value: f64 },
    #[serde(rename = "state:spread:distance_curve")]
    StateSpreadDistanceCurve { value: f64 },
    #[serde(rename = "state:distance_model")]
    StateDistanceModel { value: String },
    #[serde(rename = "state:render_backend")]
    StateRenderBackend { value: String },
    #[serde(rename = "state:render_backend:effective")]
    StateRenderBackendEffective { value: String },
    #[serde(rename = "state:render_backend:state")]
    StateRenderBackendState { value: String },
    #[serde(rename = "state:render_evaluation_mode")]
    StateRenderEvaluationMode { value: String },
    #[serde(rename = "state:render_evaluation_mode:effective")]
    StateRenderEvaluationModeEffective { value: String },
    #[serde(rename = "state:debug:speaker_heatmap:meta")]
    StateDebugSpeakerHeatmapMeta { value: String },
    #[serde(rename = "state:debug:speaker_heatmap:slice_xy")]
    StateDebugSpeakerHeatmapSliceXy { value: String },
    #[serde(rename = "state:debug:speaker_heatmap:slice_xz")]
    StateDebugSpeakerHeatmapSliceXz { value: String },
    #[serde(rename = "state:debug:speaker_heatmap:slice_yz")]
    StateDebugSpeakerHeatmapSliceYz { value: String },
    #[serde(rename = "state:debug:speaker_heatmap:volume_chunk")]
    StateDebugSpeakerHeatmapVolumeChunk { value: String },
    #[serde(rename = "state:debug:speaker_heatmap:unavailable")]
    StateDebugSpeakerHeatmapUnavailable { value: String },
    #[serde(rename = "state:snapshot_complete")]
    StateSnapshotComplete,
    #[serde(rename = "state:loudness")]
    StateLoudness { enabled: bool },
    #[serde(rename = "state:loudness:source")]
    StateLoudnessSource { value: f64 },
    #[serde(rename = "state:loudness:gain")]
    StateLoudnessGain { value: f64 },
    #[serde(rename = "state:master:gain")]
    StateMasterGain { value: f64 },
    #[serde(rename = "state:latency")]
    StateLatency { value: f64 },
    #[serde(rename = "state:latency:instant")]
    StateLatencyInstant { value: f64 },
    #[serde(rename = "state:latency:control")]
    StateLatencyControl { value: f64 },
    #[serde(rename = "state:latency:target")]
    StateLatencyTarget { value: f64 },
    #[serde(rename = "state:latency:target_requested")]
    StateLatencyTargetRequested { value: f64 },
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
    #[serde(rename = "state:audio:sample_rate")]
    StateAudioSampleRate { value: u32 },
    #[serde(rename = "state:ramp_mode")]
    StateRampMode { value: String },
    #[serde(rename = "state:audio:output_device")]
    StateAudioOutputDevice { value: String },
    #[serde(rename = "state:audio:output_device:requested")]
    StateAudioOutputDeviceRequested { value: String },
    #[serde(rename = "state:audio:output_device:effective")]
    StateAudioOutputDeviceEffective { value: String },
    #[serde(rename = "state:audio:output_devices")]
    StateAudioOutputDevices { values: Vec<String> },
    #[serde(rename = "state:audio:sample_format")]
    StateAudioSampleFormat { value: String },
    #[serde(rename = "state:audio:error")]
    StateAudioError { value: String },
    #[serde(rename = "state:input:mode")]
    StateInputMode { value: String },
    #[serde(rename = "state:input:active_mode")]
    StateInputActiveMode { value: String },
    #[serde(rename = "state:input:apply_pending")]
    StateInputApplyPending { enabled: bool },
    #[serde(rename = "state:input:backend")]
    StateInputBackend { value: String },
    #[serde(rename = "state:input:channels")]
    StateInputChannels { value: u32 },
    #[serde(rename = "state:input:sample_rate")]
    StateInputSampleRate { value: u32 },
    #[serde(rename = "state:input:node")]
    StateInputNode { value: String },
    #[serde(rename = "state:input:description")]
    StateInputDescription { value: String },
    #[serde(rename = "state:input:stream_format")]
    StateInputStreamFormat { value: String },
    #[serde(rename = "state:input:error")]
    StateInputError { value: String },
    #[serde(rename = "state:render:bridge_path")]
    StateRenderBridgePath { value: String },
    #[serde(rename = "state:input:live:backend")]
    StateInputLiveBackend { value: String },
    #[serde(rename = "state:input:live:node")]
    StateInputLiveNode { value: String },
    #[serde(rename = "state:input:live:description")]
    StateInputLiveDescription { value: String },
    #[serde(rename = "state:input:live:layout")]
    StateInputLiveLayout { value: String },
    #[serde(rename = "state:input:live:clock_mode")]
    StateInputLiveClockMode { value: String },
    #[serde(rename = "state:input:live:channels")]
    StateInputLiveChannels { value: u32 },
    #[serde(rename = "state:input:live:sample_rate")]
    StateInputLiveSampleRate { value: u32 },
    #[serde(rename = "state:input:live:format")]
    StateInputLiveFormat { value: String },
    #[serde(rename = "state:input:live:map")]
    StateInputLiveMap { value: String },
    #[serde(rename = "state:input:live:lfe_mode")]
    StateInputLiveLfeMode { value: String },
    #[serde(rename = "state:input_pipe")]
    StateInputPipe { value: String },
    #[serde(rename = "state:osc:metering")]
    StateOscMetering { enabled: bool },
    #[serde(rename = "state:log_level")]
    StateLogLevel { value: String },
    #[serde(rename = "log")]
    Log { entry: LogEntry },
    #[serde(rename = "state:distance_diffuse:enabled")]
    StateDistanceDiffuseEnabled { enabled: bool },
    #[serde(rename = "state:distance_diffuse:threshold")]
    StateDistanceDiffuseThreshold { value: f64 },
    #[serde(rename = "state:distance_diffuse:curve")]
    StateDistanceDiffuseCurve { value: f64 },
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
    #[serde(rename = "state:adaptive_resampling:near_far_threshold_ms")]
    StateAdaptiveResamplingNearFarThresholdMs { value: f64 },
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

fn parse_omniphony_config(parts: &[&str], args: &[f64], raw_args: &[OscType]) -> Option<OscEvent> {
    if !parts.contains(&"omniphony") || !parts.contains(&"config") {
        return None;
    }

    if parts.len() == 3 && parts[2] == "speakers" {
        let count = args.first().copied().and_then(to_number)? as u32;
        return Some(OscEvent::ConfigSpeakersCount { count });
    }

    if parts.len() == 4 && parts[2] == "speaker" {
        let index = parts[3].parse::<u32>().ok()?;
        // raw_args: name, az, el, dist, spatialize, delay, coord_mode, x, y, z, freq_low, freq_high
        let name = raw_args
            .first()
            .and_then(unwrap_string)
            .unwrap_or_else(|| format!("spk-{index}"));
        let az = args.get(1).copied().and_then(to_number)?;
        let el = args.get(2).copied().and_then(to_number)?;
        let dist = args.get(3).copied().and_then(to_number)?;
        let spatialize_raw = args.get(4).copied().and_then(to_number);
        let spatialize = match spatialize_raw {
            None => 1u8,
            Some(v) => {
                if v != 0.0 {
                    1
                } else {
                    0
                }
            }
        };
        let (px, py, pz) = omniphony_speaker_to_scene(az, el, dist);
        let delay_ms = args
            .get(5)
            .copied()
            .and_then(to_number)
            .unwrap_or(0.0)
            .max(0.0);
        let coord_mode = match raw_args.get(6) {
            Some(rosc::OscType::String(value)) if value.eq_ignore_ascii_case("cartesian") => {
                "cartesian".to_string()
            }
            _ => "polar".to_string(),
        };
        let x = args
            .get(7)
            .copied()
            .and_then(to_number)
            .unwrap_or(px)
            .clamp(-1.0, 1.0);
        let y = args
            .get(8)
            .copied()
            .and_then(to_number)
            .unwrap_or(py)
            .clamp(-1.0, 1.0);
        let z = args
            .get(9)
            .copied()
            .and_then(to_number)
            .unwrap_or(pz)
            .clamp(-1.0, 1.0);
        let freq_low = args.get(10).copied().and_then(to_number).and_then(|value| {
            if value > 0.0 {
                Some(value as f32)
            } else {
                None
            }
        });
        let freq_high = args.get(11).copied().and_then(to_number).and_then(|value| {
            if value > 0.0 {
                Some(value as f32)
            } else {
                None
            }
        });

        return Some(OscEvent::ConfigSpeaker {
            index,
            name,
            azimuth_deg: az,
            elevation_deg: el,
            distance_m: dist,
            coord_mode,
            x,
            y,
            z,
            delay_ms,
            spatialize,
            freq_low,
            freq_high,
            position: SpeakerPosition {
                x: px,
                y: py,
                z: pz,
                azimuth_deg: az,
                elevation_deg: el,
                distance_m: dist,
            },
        });
    }

    None
}

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

    let generation = match raw_args.get(8) {
        Some(OscType::Long(v)) if *v >= 0 => Some(*v as u64),
        Some(OscType::Int(v)) if *v >= 0 => Some(*v as u64),
        _ => None,
    };

    // name at arg index 9 for the generation payload, 8 for the extended payload, or 7 for the legacy one.
    let name = raw_args
        .get(if raw_args.len() >= 10 {
            9
        } else if raw_args.len() >= 9 {
            8
        } else {
            7
        })
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
        (3, "latency_target") => Some(OscEvent::StateLatencyTarget {
            value: to_number(args[0])?,
        }),
        (3, "latency_target_requested") => Some(OscEvent::StateLatencyTargetRequested {
            value: to_number(args[0])?,
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
        (3, "ramp_mode") => Some(OscEvent::StateRampMode {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "gain") => Some(OscEvent::StateMasterGain {
            value: to_number(args[0])?,
        }),
        (3, "distance_model") => Some(OscEvent::StateDistanceModel {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "render_backend") => Some(OscEvent::StateRenderBackend {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "render_evaluation_mode") => Some(OscEvent::StateRenderEvaluationMode {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (3, "snapshot_complete") => Some(OscEvent::StateSnapshotComplete),
        (3, "loudness") => Some(OscEvent::StateLoudness {
            enabled: to_number(args[0])? != 0.0,
        }),
        (3, "room_ratio") => {
            let w = to_number(args[0])?;
            let l = to_number(args[1])?;
            let h = to_number(args[2])?;
            Some(OscEvent::StateRoomRatio {
                width: w,
                length: l,
                height: h,
            })
        }
        (3, "room_ratio_rear") => Some(OscEvent::StateRoomRatioRear {
            value: to_number(args[0])?,
        }),
        (3, "room_ratio_lower") => Some(OscEvent::StateRoomRatioLower {
            value: to_number(args[0])?,
        }),
        (3, "room_ratio_center_blend") => Some(OscEvent::StateRoomRatioCenterBlend {
            value: to_number(args[0])?,
        }),
        (4, "layout") if parts[3] == "radius_m" => Some(OscEvent::StateLayoutRadiusM {
            value: to_number(args[0])?,
        }),
        (4, "loudness") => {
            let value = to_number(args[0])?;
            match parts[3] {
                "source" => Some(OscEvent::StateLoudnessSource { value }),
                "gain" => Some(OscEvent::StateLoudnessGain { value }),
                _ => None,
            }
        }
        (4, "render_backend") if parts[3] == "effective" => {
            Some(OscEvent::StateRenderBackendEffective {
                value: raw_args.first().and_then(unwrap_string)?,
            })
        }
        (4, "render_backend") if parts[3] == "state" => Some(OscEvent::StateRenderBackendState {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (4, "render_evaluation_mode") if parts[3] == "effective" => {
            Some(OscEvent::StateRenderEvaluationModeEffective {
                value: raw_args.first().and_then(unwrap_string)?,
            })
        }
        (5, "debug") if parts[3] == "speaker_heatmap" => {
            let value = raw_args.first().and_then(unwrap_string)?;
            match parts[4] {
                "meta" => Some(OscEvent::StateDebugSpeakerHeatmapMeta { value }),
                "slice_xy" => Some(OscEvent::StateDebugSpeakerHeatmapSliceXy { value }),
                "slice_xz" => Some(OscEvent::StateDebugSpeakerHeatmapSliceXz { value }),
                "slice_yz" => Some(OscEvent::StateDebugSpeakerHeatmapSliceYz { value }),
                "volume_chunk" => Some(OscEvent::StateDebugSpeakerHeatmapVolumeChunk { value }),
                "unavailable" => Some(OscEvent::StateDebugSpeakerHeatmapUnavailable { value }),
                _ => None,
            }
        }
        (4, "spread") => {
            let value = to_number(args[0])?;
            match parts[3] {
                "min" => Some(OscEvent::StateSpreadMin { value }),
                "max" => Some(OscEvent::StateSpreadMax { value }),
                "from_distance" => Some(OscEvent::StateSpreadFromDistance {
                    enabled: value != 0.0,
                }),
                "distance_range" => Some(OscEvent::StateSpreadDistanceRange { value }),
                "distance_curve" => Some(OscEvent::StateSpreadDistanceCurve { value }),
                _ => None,
            }
        }
        (4, "distance_diffuse") => match parts[3] {
            "enabled" => Some(OscEvent::StateDistanceDiffuseEnabled {
                enabled: to_number(args[0])? != 0.0,
            }),
            "threshold" => Some(OscEvent::StateDistanceDiffuseThreshold {
                value: to_number(args[0])?,
            }),
            "curve" => Some(OscEvent::StateDistanceDiffuseCurve {
                value: to_number(args[0])?,
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
            "near_far_threshold_ms" => Some(OscEvent::StateAdaptiveResamplingNearFarThresholdMs {
                value: to_number(args[0])?,
            }),
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
        (4, "audio") => match parts[3] {
            "sample_rate" => Some(OscEvent::StateAudioSampleRate {
                value: to_number(args[0])?.max(0.0) as u32,
            }),
            "output_device" => {
                let value = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateAudioOutputDevice { value })
            }
            "output_devices" => Some(OscEvent::StateAudioOutputDevices {
                values: raw_args.iter().filter_map(unwrap_string).collect(),
            }),
            "sample_format" => {
                let value = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateAudioSampleFormat { value })
            }
            "error" => {
                let value = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateAudioError { value })
            }
            _ => None,
        },
        (5, "audio") if parts[3] == "output_device" => match parts[4] {
            "requested" => {
                let value = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateAudioOutputDeviceRequested { value })
            }
            "effective" => {
                let value = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateAudioOutputDeviceEffective { value })
            }
            _ => None,
        },
        (4, "input") => match parts[3] {
            "mode" => {
                let value = raw_args.first().and_then(unwrap_string)?;
                Some(OscEvent::StateInputMode { value })
            }
            "active_mode" => Some(OscEvent::StateInputActiveMode {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "apply_pending" => Some(OscEvent::StateInputApplyPending {
                enabled: to_number(args[0])? != 0.0,
            }),
            "backend" => Some(OscEvent::StateInputBackend {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "channels" => Some(OscEvent::StateInputChannels {
                value: to_number(args[0])?.max(0.0) as u32,
            }),
            "sample_rate" => Some(OscEvent::StateInputSampleRate {
                value: to_number(args[0])?.max(0.0) as u32,
            }),
            "node" => Some(OscEvent::StateInputNode {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "description" => Some(OscEvent::StateInputDescription {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "stream_format" => Some(OscEvent::StateInputStreamFormat {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "error" => Some(OscEvent::StateInputError {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            _ => None,
        },
        (4, "render") if parts[3] == "bridge_path" => Some(OscEvent::StateRenderBridgePath {
            value: raw_args.first().and_then(unwrap_string)?,
        }),
        (5, "input") if parts[3] == "live" => match parts[4] {
            "backend" => Some(OscEvent::StateInputLiveBackend {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "node" => Some(OscEvent::StateInputLiveNode {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "description" => Some(OscEvent::StateInputLiveDescription {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "layout" => Some(OscEvent::StateInputLiveLayout {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "clock_mode" => Some(OscEvent::StateInputLiveClockMode {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "channels" => Some(OscEvent::StateInputLiveChannels {
                value: to_number(args[0])?.max(0.0) as u32,
            }),
            "sample_rate" => Some(OscEvent::StateInputLiveSampleRate {
                value: to_number(args[0])?.max(0.0) as u32,
            }),
            "format" => Some(OscEvent::StateInputLiveFormat {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "map" => Some(OscEvent::StateInputLiveMap {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            "lfe_mode" => Some(OscEvent::StateInputLiveLfeMode {
                value: raw_args.first().and_then(unwrap_string)?,
            }),
            _ => None,
        },
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
        let peak = clamp(to_number(args[0]).unwrap_or(-100.0), -100.0, 0.0);
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

    None
}

#[cfg(test)]
mod tests {
    use super::{parse_osc_message, CoordinateFormat, OscEvent};
    use rosc::OscType;

    #[test]
    fn parses_config_speaker_freq_range() {
        let parsed = parse_osc_message(
            "/omniphony/config/speaker/2",
            &[
                OscType::String("L".to_string()),
                OscType::Float(30.0),
                OscType::Float(10.0),
                OscType::Float(1.5),
                OscType::Int(1),
                OscType::Float(0.0),
                OscType::String("polar".to_string()),
                OscType::Float(0.0),
                OscType::Float(0.0),
                OscType::Float(1.0),
                OscType::Float(80.0),
                OscType::Float(16000.0),
            ],
            CoordinateFormat::Cartesian,
        );
        match parsed {
            Some(OscEvent::ConfigSpeaker {
                index,
                freq_low,
                freq_high,
                ..
            }) => {
                assert_eq!(index, 2);
                assert_eq!(freq_low, Some(80.0));
                assert_eq!(freq_high, Some(16000.0));
            }
            other => panic!("unexpected parse result: {:?}", other),
        }
    }

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

    // config
    if let Some(ev) = parse_omniphony_config(&parts, &args, raw_args) {
        return Some(ev);
    }

    // omniphony object position (xyz or aed)
    if let Some(ev) = parse_omniphony_object_position(&parts, &args, raw_args, coordinate_format) {
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
