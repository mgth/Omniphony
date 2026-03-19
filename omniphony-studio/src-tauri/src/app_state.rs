use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::layouts::Layout;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(rename = "directSpeakerIndex", skip_serializing_if = "Option::is_none")]
    pub direct_speaker_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
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
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct VbapMode {
    pub selection: Option<String>,
    #[serde(rename = "effectiveMode")]
    pub effective_mode: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct OutputDeviceOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct LiveSpeakerConfig {
    pub name: String,
    pub delay_ms: f64,
    pub spatialize: u8,
    pub coord_mode: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub azimuth_deg: f64,
    pub elevation_deg: f64,
    pub distance_m: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AppState {
    pub sources: HashMap<String, SourcePosition>,
    #[serde(rename = "sourceLevels")]
    pub source_levels: HashMap<String, Meter>,
    #[serde(rename = "speakerLevels")]
    pub speaker_levels: HashMap<String, Meter>,
    #[serde(rename = "objectSpeakerGains")]
    pub object_speaker_gains: HashMap<String, Vec<f64>>,
    #[serde(rename = "objectGains")]
    pub object_gains: HashMap<String, f64>,
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
    #[serde(rename = "distanceDiffuse")]
    pub distance_diffuse: DistanceDiffuse,
    #[serde(rename = "vbapCartesian")]
    pub vbap_cartesian: VbapCartesian,
    #[serde(rename = "vbapPolar")]
    pub vbap_polar: VbapPolar,
    #[serde(rename = "vbapMode")]
    pub vbap_mode: VbapMode,
    #[serde(rename = "vbapAllowNegativeZ")]
    pub vbap_allow_negative_z: Option<bool>,
    #[serde(rename = "adaptiveResampling")]
    pub adaptive_resampling: Option<u8>,
    #[serde(rename = "adaptiveResamplingKpNear")]
    pub adaptive_resampling_kp_near: Option<f64>,
    #[serde(rename = "adaptiveResamplingKpFar")]
    pub adaptive_resampling_kp_far: Option<f64>,
    #[serde(rename = "adaptiveResamplingKi")]
    pub adaptive_resampling_ki: Option<f64>,
    #[serde(rename = "adaptiveResamplingMaxAdjust")]
    pub adaptive_resampling_max_adjust: Option<f64>,
    #[serde(rename = "adaptiveResamplingMaxAdjustFar")]
    pub adaptive_resampling_max_adjust_far: Option<f64>,
    #[serde(rename = "adaptiveResamplingNearFarThresholdMs")]
    pub adaptive_resampling_near_far_threshold_ms: Option<i64>,
    #[serde(rename = "adaptiveResamplingHardCorrectionThresholdMs")]
    pub adaptive_resampling_hard_correction_threshold_ms: Option<i64>,
    #[serde(rename = "adaptiveResamplingMeasurementSmoothingAlpha")]
    pub adaptive_resampling_measurement_smoothing_alpha: Option<f64>,
    #[serde(rename = "adaptiveResamplingBand")]
    pub adaptive_resampling_band: Option<String>,
    #[serde(rename = "vbapRecomputing")]
    pub vbap_recomputing: Option<bool>,
    #[serde(rename = "configSaved")]
    pub config_saved: Option<u8>,
    #[serde(rename = "latencyMs")]
    pub latency_ms: Option<i64>,
    #[serde(rename = "latencyInstantMs")]
    pub latency_instant_ms: Option<i64>,
    #[serde(rename = "latencyControlMs")]
    pub latency_control_ms: Option<i64>,
    #[serde(rename = "latencyTargetMs")]
    pub latency_target_ms: Option<i64>,
    #[serde(rename = "resampleRatio")]
    pub resample_ratio: Option<f64>,
    #[serde(rename = "audioSampleRate")]
    pub audio_sample_rate: Option<u32>,
    #[serde(rename = "rampMode")]
    pub ramp_mode: Option<String>,
    #[serde(rename = "audioOutputDevice")]
    pub audio_output_device: Option<String>,
    #[serde(rename = "audioOutputDevices")]
    pub audio_output_devices: Vec<OutputDeviceOption>,
    #[serde(rename = "audioSampleFormat")]
    pub audio_sample_format: Option<String>,
    #[serde(rename = "orenderInputPipe")]
    pub orender_input_pipe: Option<String>,
    #[serde(rename = "oscStatus")]
    pub osc_status: Option<String>,
    #[serde(rename = "oscMeteringEnabled")]
    pub osc_metering_enabled: Option<u8>,
    #[serde(rename = "logLevel")]
    pub log_level: Option<String>,
    #[serde(rename = "lastSpatialSamplePos")]
    pub last_spatial_sample_pos: Option<i64>,
    #[serde(skip)]
    pub current_content_generation: Option<u64>,
    #[serde(skip)]
    pub live_speaker_count: Option<u32>,
    #[serde(skip)]
    pub live_speakers: BTreeMap<u32, LiveSpeakerConfig>,
    #[serde(rename = "currentCoordinateFormat")]
    pub current_coordinate_format: u8,
    pub layouts: Vec<Layout>,
    #[serde(rename = "selectedLayoutKey")]
    pub selected_layout_key: Option<String>,
}

impl AppState {
    pub fn new(layouts: Vec<Layout>) -> Self {
        let selected_layout_key = layouts.first().map(|l| l.key.clone());
        Self {
            layouts,
            selected_layout_key,
            room_ratio: RoomRatio {
                width: 1.0,
                length: 2.0,
                height: 1.0,
                rear: 1.0,
                lower: 0.5,
                center_blend: 0.5,
            },
            ..Default::default()
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            sources: HashMap::new(),
            source_levels: HashMap::new(),
            speaker_levels: HashMap::new(),
            object_speaker_gains: HashMap::new(),
            object_gains: HashMap::new(),
            speaker_gains: HashMap::new(),
            object_mutes: HashMap::new(),
            speaker_mutes: HashMap::new(),
            room_ratio: RoomRatio::default(),
            spread: SpreadState::default(),
            loudness: None,
            loudness_source: None,
            loudness_gain: None,
            master_gain: None,
            distance_diffuse: DistanceDiffuse::default(),
            vbap_cartesian: VbapCartesian::default(),
            vbap_polar: VbapPolar::default(),
            vbap_mode: VbapMode::default(),
            vbap_allow_negative_z: None,
            adaptive_resampling: Some(0),
            adaptive_resampling_kp_near: Some(0.00001),
            adaptive_resampling_kp_far: Some(0.00002),
            adaptive_resampling_ki: Some(0.0000005),
            adaptive_resampling_max_adjust: Some(0.01),
            adaptive_resampling_max_adjust_far: Some(0.02),
            adaptive_resampling_near_far_threshold_ms: Some(120),
            adaptive_resampling_hard_correction_threshold_ms: Some(0),
            adaptive_resampling_measurement_smoothing_alpha: Some(0.15),
            adaptive_resampling_band: None,
            vbap_recomputing: None,
            config_saved: None,
            latency_ms: None,
            latency_instant_ms: None,
            latency_control_ms: None,
            latency_target_ms: None,
            resample_ratio: None,
            audio_sample_rate: None,
            ramp_mode: Some("sample".to_string()),
            audio_output_device: None,
            audio_output_devices: Vec::new(),
            audio_sample_format: None,
            orender_input_pipe: None,
            osc_status: Some("initializing".to_string()),
            osc_metering_enabled: Some(0),
            log_level: Some("info".to_string()),
            last_spatial_sample_pos: None,
            current_content_generation: None,
            live_speaker_count: None,
            live_speakers: BTreeMap::new(),
            current_coordinate_format: 0,
            layouts: Vec::new(),
            selected_layout_key: None,
        }
    }
}
