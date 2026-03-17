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

fn gsrd_speaker_to_scene(az_deg: f64, el_deg: f64, dist: f64) -> (f64, f64, f64) {
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(rename = "directSpeakerIndex", skip_serializing_if = "Option::is_none")]
    pub direct_speaker_index: Option<u32>,
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
        #[serde(rename = "delayMs")]
        delay_ms: f64,
        spatialize: u8,
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

    #[serde(rename = "meter:speaker")]
    MeterSpeaker {
        id: String,
        #[serde(rename = "peakDbfs")]
        peak_dbfs: f64,
        #[serde(rename = "rmsDbfs")]
        rms_dbfs: f64,
    },

    #[serde(rename = "state:object:gain")]
    StateObjectGain { id: String, gain: f64 },
    #[serde(rename = "state:speaker:gain")]
    StateSpeakerGain { id: String, gain: f64 },
    #[serde(rename = "state:speaker:delay")]
    StateSpeakerDelay { id: String, delay_ms: f64 },
    #[serde(rename = "state:object:mute")]
    StateObjectMute { id: String, muted: bool },
    #[serde(rename = "state:speaker:mute")]
    StateSpeakerMute { id: String, muted: bool },
    #[serde(rename = "state:speaker:spatialize")]
    StateSpeakerSpatialize { id: String, spatialize: bool },
    #[serde(rename = "state:speaker:name")]
    StateSpeakerName { id: String, name: String },

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
    #[serde(rename = "state:resample_ratio")]
    StateResampleRatio { value: f64 },
    #[serde(rename = "state:audio:sample_rate")]
    StateAudioSampleRate { value: u32 },
    #[serde(rename = "state:ramp_mode")]
    StateRampMode { value: String },
    #[serde(rename = "state:audio:output_device")]
    StateAudioOutputDevice { value: String },
    #[serde(rename = "state:audio:output_devices")]
    StateAudioOutputDevices { values: Vec<String> },
    #[serde(rename = "state:audio:sample_format")]
    StateAudioSampleFormat { value: String },
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
    #[serde(rename = "state:vbap:cart:x_size")]
    StateVbapCartXSize { value: u32 },
    #[serde(rename = "state:vbap:cart:y_size")]
    StateVbapCartYSize { value: u32 },
    #[serde(rename = "state:vbap:cart:z_size")]
    StateVbapCartZSize { value: u32 },
    #[serde(rename = "state:vbap:cart:z_neg_size")]
    StateVbapCartZNegSize { value: u32 },
    #[serde(rename = "state:vbap:table_mode")]
    StateVbapTableMode { value: String },
    #[serde(rename = "state:vbap:effective_mode")]
    StateVbapEffectiveMode { value: String },
    #[serde(rename = "state:vbap:polar:azimuth_resolution")]
    StateVbapPolarAzimuthResolution { value: u32 },
    #[serde(rename = "state:vbap:polar:elevation_resolution")]
    StateVbapPolarElevationResolution { value: u32 },
    #[serde(rename = "state:vbap:polar:distance_res")]
    StateVbapPolarDistanceRes { value: u32 },
    #[serde(rename = "state:vbap:polar:distance_max")]
    StateVbapPolarDistanceMax { value: f64 },
    #[serde(rename = "state:vbap:allow_negative_z")]
    StateVbapAllowNegativeZ { enabled: bool },
    #[serde(rename = "state:speakers:recomputing")]
    StateSpeakersRecomputing { enabled: bool },
    #[serde(rename = "state:adaptive_resampling")]
    StateAdaptiveResampling { enabled: bool },
    #[serde(rename = "state:adaptive_resampling:kp_near")]
    StateAdaptiveResamplingKpNear { value: f64 },
    #[serde(rename = "state:adaptive_resampling:kp_far")]
    StateAdaptiveResamplingKpFar { value: f64 },
    #[serde(rename = "state:adaptive_resampling:ki")]
    StateAdaptiveResamplingKi { value: f64 },
    #[serde(rename = "state:adaptive_resampling:max_adjust")]
    StateAdaptiveResamplingMaxAdjust { value: f64 },
    #[serde(rename = "state:adaptive_resampling:max_adjust_far")]
    StateAdaptiveResamplingMaxAdjustFar { value: f64 },
    #[serde(rename = "state:adaptive_resampling:near_far_threshold_ms")]
    StateAdaptiveResamplingNearFarThresholdMs { value: f64 },
    #[serde(rename = "state:adaptive_resampling:hard_correction_threshold_ms")]
    StateAdaptiveResamplingHardCorrectionThresholdMs { value: f64 },
    #[serde(rename = "state:adaptive_resampling:measurement_smoothing_alpha")]
    StateAdaptiveResamplingMeasurementSmoothingAlpha { value: f64 },
    #[serde(rename = "state:adaptive_resampling:band")]
    StateAdaptiveResamplingBand { value: String },
    #[serde(rename = "state:config:saved")]
    StateConfigSaved { saved: bool },
}

// ── sub-parsers ─────────────────────────────────────────────────────────────

fn parse_gsrd_config(parts: &[&str], args: &[f64], raw_args: &[OscType]) -> Option<OscEvent> {
    if !parts.contains(&"gsrd") || !parts.contains(&"config") {
        return None;
    }

    if parts.len() == 3 && parts[2] == "speakers" {
        let count = args.first().copied().and_then(to_number)? as u32;
        return Some(OscEvent::ConfigSpeakersCount { count });
    }

    if parts.len() == 4 && parts[2] == "speaker" {
        let index = parts[3].parse::<u32>().ok()?;
        // raw_args: name, az, el, dist, spatialize
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
        let (px, py, pz) = gsrd_speaker_to_scene(az, el, dist);
        let delay_ms = args
            .get(5)
            .copied()
            .and_then(to_number)
            .unwrap_or(0.0)
            .max(0.0);

        return Some(OscEvent::ConfigSpeaker {
            index,
            name,
            azimuth_deg: az,
            elevation_deg: el,
            distance_m: dist,
            delay_ms,
            spatialize,
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

fn parse_gsrd_object_xyz(
    parts: &[&str],
    args: &[f64],
    raw_args: &[OscType],
    coordinate_format: CoordinateFormat,
) -> Option<OscEvent> {
    if !parts.contains(&"gsrd") || !parts.contains(&"object") || !parts.contains(&"xyz") {
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

    let (mx, my, mz) = match coordinate_format {
        // gsrd xyz: x=right, y=front, z=up → scene: x=front, y=up, z=right
        CoordinateFormat::Cartesian => (y, z, x),
        // In polar mode payload is [azimuth_deg, elevation_deg, distance]
        CoordinateFormat::Polar => spherical_to_cartesian(x, y, z),
    };
    Some(OscEvent::Update {
        id,
        position: Position {
            x: mx,
            y: my,
            z: mz,
            generation,
            direct_speaker_index,
        },
        name,
    })
}

fn parse_gsrd_spatial_frame(parts: &[&str], args: &[f64]) -> Option<OscEvent> {
    if parts.len() != 3 || parts[0] != "gsrd" || parts[1] != "spatial" || parts[2] != "frame" {
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

fn parse_gsrd_log(parts: &[&str], raw_args: &[OscType]) -> Option<OscEvent> {
    if parts.len() != 2 || parts[0] != "gsrd" || parts[1] != "log" {
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

fn parse_gsrd_state(parts: &[&str], args: &[f64], raw_args: &[OscType]) -> Option<OscEvent> {
    if parts.len() < 3 || parts[0] != "gsrd" || parts[1] != "state" {
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
        (5, "vbap") if parts[3] == "cart" => {
            let value = to_number(args[0])?.max(0.0) as u32;
            match parts[4] {
                "x_size" => Some(OscEvent::StateVbapCartXSize { value }),
                "y_size" => Some(OscEvent::StateVbapCartYSize { value }),
                "z_size" => Some(OscEvent::StateVbapCartZSize { value }),
                "z_neg_size" => Some(OscEvent::StateVbapCartZNegSize { value }),
                _ => None,
            }
        }
        (4, "vbap") if parts[3] == "table_mode" => {
            let value = raw_args.first().and_then(unwrap_string)?;
            Some(OscEvent::StateVbapTableMode { value })
        }
        (4, "vbap") if parts[3] == "effective_mode" => {
            let value = raw_args.first().and_then(unwrap_string)?;
            Some(OscEvent::StateVbapEffectiveMode { value })
        }
        (5, "vbap") if parts[3] == "polar" => {
            match parts[4] {
                "azimuth_resolution" => {
                    let value = to_number(args[0])?.max(0.0) as u32;
                    Some(OscEvent::StateVbapPolarAzimuthResolution { value })
                }
                "elevation_resolution" => {
                    let value = to_number(args[0])?.max(0.0) as u32;
                    Some(OscEvent::StateVbapPolarElevationResolution { value })
                }
                "distance_res" => Some(OscEvent::StateVbapPolarDistanceRes {
                    value: to_number(args[0])?.max(0.0) as u32,
                }),
                "distance_max" => Some(OscEvent::StateVbapPolarDistanceMax {
                    value: to_number(args[0])?.max(0.0),
                }),
                _ => None,
            }
        }
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
            "kp_near" => Some(OscEvent::StateAdaptiveResamplingKpNear {
                value: to_number(args[0])?,
            }),
            "kp_far" => Some(OscEvent::StateAdaptiveResamplingKpFar {
                value: to_number(args[0])?,
            }),
            "ki" => Some(OscEvent::StateAdaptiveResamplingKi {
                value: to_number(args[0])?,
            }),
            "max_adjust" => Some(OscEvent::StateAdaptiveResamplingMaxAdjust {
                value: to_number(args[0])?,
            }),
            "max_adjust_far" => Some(OscEvent::StateAdaptiveResamplingMaxAdjustFar {
                value: to_number(args[0])?,
            }),
            "near_far_threshold_ms" => {
                Some(OscEvent::StateAdaptiveResamplingNearFarThresholdMs {
                    value: to_number(args[0])?,
                })
            }
            "hard_correction_threshold_ms" => {
                Some(OscEvent::StateAdaptiveResamplingHardCorrectionThresholdMs {
                    value: to_number(args[0])?,
                })
            }
            "measurement_smoothing_alpha" => {
                Some(OscEvent::StateAdaptiveResamplingMeasurementSmoothingAlpha {
                    value: to_number(args[0])?,
                })
            }
            "band" => Some(OscEvent::StateAdaptiveResamplingBand {
                value: unwrap_string(raw_args.first()?)?,
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
            _ => None,
        },
        (4, "osc") if parts[3] == "metering" => Some(OscEvent::StateOscMetering {
            enabled: to_number(args[0])? != 0.0,
        }),
        (5, kind) if kind == "object" || kind == "speaker" => match parts[4] {
            "gain" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let gain = clamp(to_number(args[0])?, 0.0, 2.0);
                if kind == "speaker" {
                    Some(OscEvent::StateSpeakerGain { id, gain })
                } else {
                    Some(OscEvent::StateObjectGain { id, gain })
                }
            }
            "delay" if kind == "speaker" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let delay_ms = clamp(to_number(args[0])?, 0.0, 10_000.0);
                Some(OscEvent::StateSpeakerDelay { id, delay_ms })
            }
            "mute" => {
                let id = parts[3].parse::<u32>().ok()?.to_string();
                let muted = to_number(args[0])? != 0.0;
                if kind == "speaker" {
                    Some(OscEvent::StateSpeakerMute { id, muted })
                } else {
                    Some(OscEvent::StateObjectMute { id, muted })
                }
            }
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
            _ => None,
        },
        _ => None,
    }
}

fn parse_meter(parts: &[&str], args: &[f64]) -> Option<OscEvent> {
    let meter_idx = parts.iter().position(|&p| p == "meter")?;
    let after = &parts[meter_idx..];

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
    if let Some(ev) = parse_gsrd_config(&parts, &args, raw_args) {
        return Some(ev);
    }

    // gsrd object xyz
    if let Some(ev) = parse_gsrd_object_xyz(&parts, &args, raw_args, coordinate_format) {
        return Some(ev);
    }

    // gsrd spatial frame
    if let Some(ev) = parse_gsrd_spatial_frame(&parts, &args) {
        return Some(ev);
    }

    if let Some(ev) = parse_gsrd_log(&parts, raw_args) {
        return Some(ev);
    }

    // gsrd state
    if let Some(ev) = parse_gsrd_state(&parts, &args, raw_args) {
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
            generation: None,
            direct_speaker_index: None,
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
