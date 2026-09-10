//! Applies parsed `OscEvent`s to the live model.
//!
//! This is the state-mutation half of the Tauri host's `handle_event`
//! (`src-tauri/src/osc_listener.rs`), without the webview emits: the native UI
//! reads the model directly, so there is no payload to build and no
//! camelCase re-modelling layer. Events the host only forwards to the UI
//! (head pose, object sizes, overlay, decoded gain tables, schemas) are kept
//! in [`Live`] next to the reused [`AppState`].

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::model::app_state::{AppState, Meter};
use crate::model::layouts::Speaker;
use crate::osc::apply::{
    GainTable, apply_audio_domain_state, apply_input_domain_state, apply_layout_domain_state,
    apply_loudness_domain_state, apply_monitoring_domain_state, apply_profiles_domain_state,
    apply_renderer_domain_state, apply_speakers_domain_state, gaintable_on_chunk,
    gaintable_on_meta,
};
use crate::osc::parser::OscEvent;

#[derive(Debug, Clone, Copy)]
pub struct ObjectTestPosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
}

/// What an event changed, so the UI thread can decide whether to repaint and
/// whether derived caches (speaker geometry, room box) must be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    None,
    /// Per-frame data: positions, meters, gains.
    Scene,
    /// Snapshot-level data: layout, speakers, room geometry, renderer state.
    Snapshot,
}

/// One recorded trail sample (`trails.js` point record).
#[derive(Clone, Debug)]
pub struct TrailPoint {
    /// Normalised ADM position.
    pub adm: [f64; 3],
    pub t: Instant,
    pub rms_dbfs: Option<f64>,
    pub gain_db: Option<i32>,
    pub direct_speaker_index: Option<u32>,
}

/// Per-object trail ring (`sourceTrails`): at most 240 points, one every
/// 70 ms at most; the TTL is applied when drawing.
#[derive(Default, Debug)]
pub struct Trail {
    pub points: VecDeque<TrailPoint>,
    pub last_point_at: Option<Instant>,
}

pub const TRAIL_MIN_POINT_INTERVAL: Duration = Duration::from_millis(70);
pub const TRAIL_MAX_POINTS: usize = 240;

/// The whole live model: the host's `AppState` plus the UI-only mirrors.
pub struct Live {
    pub app: AppState,
    /// When each object's / speaker's meter last arrived, for the display
    /// decay (`decayMeters`).
    pub source_level_seen: HashMap<String, Instant>,
    pub speaker_level_seen: HashMap<String, Instant>,
    pub trails: HashMap<String, Trail>,
    /// Head-tracking quaternion `[w, x, y, z]` from `/omniphony/state/head_pose`.
    pub head_pose: Option<[f32; 4]>,
    /// Object extents `[w, d, h]` from `/omniphony/object/<id>/size`.
    pub object_sizes: HashMap<String, [f32; 3]>,
    /// Per-object band RMS levels, alongside `app.source_levels`.
    pub object_band_rms: HashMap<String, Vec<f64>>,
    /// Decoded speaker gain tables keyed by speaker index; `-1` is the
    /// all-speaker energy field.
    pub gain_tables: HashMap<i64, GainTable>,
    pub gaintable_unavailable: Option<serde_json::Value>,
    pub overlay: Option<serde_json::Value>,
    pub object_test_position: Option<ObjectTestPosition>,
    pub options_schema: Option<serde_json::Value>,
    pub object_generators_schema: Option<serde_json::Value>,
    pub phantom_schema: Option<serde_json::Value>,
    pub drc_gain: Option<f64>,
    pub ear_levels: HashMap<String, Meter>,
    /// Last clip report: speaker index and when it arrived.
    pub clip: Option<(i32, Instant)>,
    pub last_frame_reset: Option<Instant>,
    /// Bumped on every `Change::Snapshot`; the UI compares it to rebuild caches.
    pub snapshot_epoch: u64,
}

impl Live {
    pub fn new(app: AppState) -> Self {
        Self {
            app,
            source_level_seen: HashMap::new(),
            speaker_level_seen: HashMap::new(),
            trails: HashMap::new(),
            head_pose: None,
            object_sizes: HashMap::new(),
            object_band_rms: HashMap::new(),
            gain_tables: HashMap::new(),
            gaintable_unavailable: None,
            overlay: None,
            object_test_position: None,
            options_schema: None,
            object_generators_schema: None,
            phantom_schema: None,
            drc_gain: None,
            ear_levels: HashMap::new(),
            clip: None,
            last_frame_reset: None,
            snapshot_epoch: 0,
        }
    }

    /// The speakers of the layout the renderer currently runs.
    pub fn selected_speakers(&self) -> &[Speaker] {
        let key = self.app.selected_layout_key.as_deref();
        self.app
            .layouts
            .iter()
            .find(|l| Some(l.key.as_str()) == key)
            .map(|l| l.speakers.as_slice())
            .unwrap_or(&[])
    }

    fn with_selected_speaker(&mut self, id: &str, f: impl FnOnce(&mut Speaker)) {
        let Ok(index) = id.parse::<usize>() else {
            return;
        };
        let Some(key) = self.app.selected_layout_key.clone() else {
            return;
        };
        if let Some(layout) = self.app.layouts.iter_mut().find(|l| l.key == key)
            && let Some(spk) = layout.speakers.get_mut(index)
        {
            f(spk);
        }
    }

    fn remove_source(&mut self, id: &str) {
        self.app.sources.remove(id);
        self.app.source_levels.remove(id);
        self.app.object_speaker_gains.remove(id);
        self.app.object_band_gains.remove(id);
        self.object_sizes.remove(id);
        self.object_band_rms.remove(id);
        self.source_level_seen.remove(id);
        self.trails.remove(id);
    }

    /// `recordTrailPoint` + `shouldAppendTrailPoint`.
    fn record_trail_point(&mut self, id: &str, now: Instant) {
        let Some(src) = self.app.sources.get(id) else {
            return;
        };
        let point = TrailPoint {
            adm: [src.x, src.y, src.z],
            t: now,
            rms_dbfs: self.app.source_levels.get(id).map(|m| m.rms_dbfs),
            gain_db: src.gain_db,
            direct_speaker_index: src.direct_speaker_index,
        };
        let trail = self.trails.entry(id.to_owned()).or_default();
        if trail
            .last_point_at
            .is_some_and(|t| now.duration_since(t) < TRAIL_MIN_POINT_INTERVAL)
        {
            return;
        }
        trail.last_point_at = Some(now);
        trail.points.push_back(point);
        while trail.points.len() > TRAIL_MAX_POINTS {
            trail.points.pop_front();
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Apply one event and report what changed.
pub fn apply_event(live: &mut Live, ev: OscEvent) -> Change {
    let change = apply_event_inner(live, ev);
    if change == Change::Snapshot {
        live.snapshot_epoch += 1;
    }
    change
}

fn apply_event_inner(live: &mut Live, ev: OscEvent) -> Change {
    match ev {
        OscEvent::SpatialFrame {
            sample_pos,
            generation,
            object_count,
            coordinate_format,
        } => {
            let s = &mut live.app;
            let generation_changed = s
                .current_content_generation
                .is_some_and(|prev| prev != generation);
            let is_reset = generation_changed
                || s.last_spatial_sample_pos
                    .is_some_and(|prev| sample_pos < prev);
            s.last_spatial_sample_pos = Some(sample_pos);
            s.current_content_generation = Some(generation);
            s.current_coordinate_format = coordinate_format;
            // Compatibility with renderers that predate the explicit removal
            // message: infer vanished slots from the frame's object count.
            let stale_ids: Vec<String> = if is_reset {
                s.sources.keys().cloned().collect()
            } else {
                s.sources
                    .keys()
                    .filter(|id| id.parse::<u32>().is_ok_and(|idx| idx >= object_count))
                    .cloned()
                    .collect()
            };
            for id in &stale_ids {
                live.app.sources.remove(id);
                live.app.source_levels.remove(id);
                live.app.object_speaker_gains.remove(id);
                live.app.object_band_gains.remove(id);
                live.object_sizes.remove(id);
                live.object_band_rms.remove(id);
                live.source_level_seen.remove(id);
                live.trails.remove(id);
                // A reset keeps each slot's mute (the renderer does too); a
                // genuine shrink drops it.
                if !is_reset {
                    live.app.object_mutes.remove(id);
                }
            }
            if is_reset {
                live.last_frame_reset = Some(Instant::now());
                for trail in live.trails.values_mut() {
                    trail.points.clear();
                }
            }
            if stale_ids.is_empty() {
                Change::None
            } else {
                Change::Scene
            }
        }
        OscEvent::Update { id, position, name } => {
            let id_for_trail = id.clone();
            let s = &mut live.app;
            let current_generation = s.current_content_generation;
            let entry = s.sources.entry(id).or_default();
            entry.x = position.x;
            entry.y = position.y;
            entry.z = position.z;
            entry.coord_mode = Some(position.coord_mode);
            entry.azimuth_deg = position.azimuth_deg;
            entry.elevation_deg = position.elevation_deg;
            entry.distance_m = position.distance_m;
            entry.gain_db = position.gain_db;
            entry.generation = position.generation.or(current_generation);
            entry.direct_speaker_index = position.direct_speaker_index;
            if let Some(tag) = position.source_tag {
                entry.source_tag = Some(tag);
            }
            if let Some(n) = name {
                entry.name = Some(n);
            }
            let id = id_for_trail;
            live.record_trail_point(&id, Instant::now());
            Change::Scene
        }
        OscEvent::UpdateMeta {
            id,
            fixed,
            label,
            generation,
            kind,
        } => {
            let s = &mut live.app;
            let current_generation = s.current_content_generation;
            let entry = s.sources.entry(id).or_default();
            entry.fixed = Some(fixed);
            entry.label = label;
            entry.kind = kind;
            if entry.generation.is_none() {
                entry.generation = generation.or(current_generation);
            }
            Change::Scene
        }
        OscEvent::UpdateSize { id, size, .. } => {
            live.object_sizes.insert(id, size);
            Change::Scene
        }
        OscEvent::Remove { id } => {
            live.remove_source(&id);
            live.app.object_mutes.remove(&id);
            Change::Scene
        }
        OscEvent::MeterObject {
            id,
            peak_dbfs,
            rms_dbfs,
            band_rms_dbfs,
        } => {
            live.app.source_levels.insert(
                id.clone(),
                Meter {
                    peak_dbfs,
                    rms_dbfs,
                },
            );
            // Sent even when empty: "no crossover" must clear a stale band level.
            live.source_level_seen.insert(id.clone(), Instant::now());
            live.object_band_rms.insert(id, band_rms_dbfs);
            Change::Scene
        }
        OscEvent::MeterObjectGains { id, gains } => {
            live.app.object_speaker_gains.insert(id, gains);
            Change::Scene
        }
        OscEvent::MeterObjectBandGains { id, band, gains } => {
            let entry = live.app.object_band_gains.entry(id).or_default();
            if entry.len() <= band {
                entry.resize(band + 1, Vec::new());
            }
            entry[band] = gains;
            Change::Scene
        }
        OscEvent::MeterSpeaker {
            id,
            peak_dbfs,
            rms_dbfs,
        } => {
            live.speaker_level_seen.insert(id.clone(), Instant::now());
            live.app.speaker_levels.insert(
                id,
                Meter {
                    peak_dbfs,
                    rms_dbfs,
                },
            );
            Change::Scene
        }
        OscEvent::MeterEar {
            id,
            peak_dbfs,
            rms_dbfs,
        } => {
            live.ear_levels.insert(
                id,
                Meter {
                    peak_dbfs,
                    rms_dbfs,
                },
            );
            Change::Scene
        }
        OscEvent::MeterMaster {
            peak_dbfs,
            rms_dbfs,
        } => {
            live.app.master_level = Some(Meter {
                peak_dbfs,
                rms_dbfs,
            });
            Change::Scene
        }
        OscEvent::MeterDrcGain { value } => {
            live.drc_gain = Some(value);
            Change::Scene
        }
        OscEvent::StateSpeakerGain { id, gain } => {
            live.app.speaker_gains.insert(id, gain);
            Change::Scene
        }
        OscEvent::StateRealtimeSpeakerGain { id, value, .. } => {
            live.app.speaker_gains.insert(id, value);
            Change::Scene
        }
        OscEvent::StateRealtimeMasterGain { value, .. } => {
            live.app.master_gain = Some(value);
            Change::Scene
        }
        OscEvent::StateSpeakerDelay { id, delay_ms } => {
            live.with_selected_speaker(&id, |spk| spk.delay_ms = delay_ms.max(0.0));
            Change::Snapshot
        }
        OscEvent::StateSpeakerSpatialize { id, spatialize } => {
            live.with_selected_speaker(&id, |spk| spk.spatialize = u8::from(spatialize));
            Change::Snapshot
        }
        OscEvent::StateSpeakerName { id, name } => {
            live.with_selected_speaker(&id, |spk| spk.id = name);
            Change::Snapshot
        }
        OscEvent::StateSpeakerFreqLow { id, freq_low } => {
            live.with_selected_speaker(&id, |spk| spk.freq_low = freq_low);
            Change::Snapshot
        }
        OscEvent::StateSpeakerFreqHigh { id, freq_high } => {
            live.with_selected_speaker(&id, |spk| spk.freq_high = freq_high);
            Change::Snapshot
        }
        OscEvent::StateObjectMute { id, muted } => {
            if muted {
                live.app.object_mutes.insert(id, 1);
            } else {
                live.app.object_mutes.remove(&id);
            }
            Change::Scene
        }
        OscEvent::StateObjectSourceTag { id, source_tag } => {
            live.app.sources.entry(id).or_default().source_tag = Some(source_tag);
            Change::Scene
        }
        OscEvent::StateSpeakerMute { id, muted } => {
            if muted {
                live.app.speaker_mutes.insert(id, 1);
            } else {
                live.app.speaker_mutes.remove(&id);
            }
            Change::Scene
        }
        OscEvent::StateOscMetering { enabled } => {
            live.app.osc_metering_enabled = Some(u8::from(enabled));
            Change::None
        }
        OscEvent::StateCapabilities { value } => {
            live.app.producer_capabilities = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StateSession { value } => {
            live.app.producer_session = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StateClip { speaker } => {
            live.clip = Some((speaker, Instant::now()));
            Change::Scene
        }
        OscEvent::StateOverlay { json } => {
            live.overlay = serde_json::from_str(&json).ok();
            Change::None
        }
        OscEvent::StateHeadPose { w, x, y, z } => {
            live.head_pose = Some([w, x, y, z]);
            Change::Scene
        }
        OscEvent::StateRenderer { value } => {
            snapshot_if(apply_renderer_domain_state(&mut live.app, &value))
        }
        OscEvent::StateAudio { value } => {
            snapshot_if(apply_audio_domain_state(&mut live.app, &value))
        }
        OscEvent::StateLayout { value } => {
            snapshot_if(apply_layout_domain_state(&mut live.app, &value))
        }
        OscEvent::StateSpeakers { value } => {
            snapshot_if(apply_speakers_domain_state(&mut live.app, &value))
        }
        OscEvent::StateInput { value } => {
            snapshot_if(apply_input_domain_state(&mut live.app, &value))
        }
        OscEvent::StateLoudness { value } => {
            snapshot_if(apply_loudness_domain_state(&mut live.app, &value))
        }
        OscEvent::StateMonitoring { value } => {
            snapshot_if(apply_monitoring_domain_state(&mut live.app, &value))
        }
        OscEvent::StateProfiles { value } => {
            snapshot_if(apply_profiles_domain_state(&mut live.app, &value))
        }
        OscEvent::StateDebugSpeakerGaintableMeta { value } => {
            gaintable_on_meta(&value);
            Change::None
        }
        OscEvent::StateDebugSpeakerGaintableChunk { bytes } => match gaintable_on_chunk(&bytes) {
            Some(table) => {
                live.gaintable_unavailable = None;
                live.gain_tables.insert(table.speaker_index(), table);
                Change::Scene
            }
            None => Change::None,
        },
        OscEvent::StateDebugSpeakerGaintableUnavailable { value } => {
            live.gaintable_unavailable = serde_json::from_str(&value).ok();
            Change::Scene
        }
        OscEvent::StateSnapshotComplete => {
            live.app.osc_snapshot_ready = true;
            Change::Snapshot
        }
        OscEvent::StateLatency { value } => {
            live.app.set_latency_value(value);
            Change::None
        }
        OscEvent::StateLatencyInstant { value } => {
            live.app.set_latency_instant_value(value);
            Change::None
        }
        OscEvent::StateLatencyControl { value } => {
            live.app.set_latency_control_value(value);
            Change::None
        }
        OscEvent::StateLatencySmoothed { value } => {
            live.app.set_latency_smoothed_value(value);
            Change::None
        }
        OscEvent::StateLatencyDownstream { value } => {
            live.app.set_latency_downstream_value(value);
            Change::None
        }
        OscEvent::StateLatencyTarget { value } => {
            live.app.set_latency_target_value(value);
            Change::None
        }
        OscEvent::StateLatencyTargetRequested { value } => {
            live.app.set_latency_requested_value(value);
            Change::None
        }
        OscEvent::StateLatencyAvailInput { value } => {
            live.app.set_latency_avail_input_value(value);
            Change::None
        }
        OscEvent::StateLatencyOutputFifo { value } => {
            live.app.set_latency_output_fifo_value(value);
            Change::None
        }
        OscEvent::StateLatencyResamplerPending { value } => {
            live.app.set_latency_resampler_pending_value(value);
            Change::None
        }
        OscEvent::StateDiagSchema { value } => {
            live.app.latency.diag_schema = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StateDiagValues { value } => {
            live.app.latency.diag_values = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StateObjectGenerators { value } => {
            live.object_generators_schema = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StatePhantom { value } => {
            live.phantom_schema = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StateOptionsSchema { value } => {
            live.options_schema = serde_json::from_str(&value).ok();
            Change::None
        }
        OscEvent::StateObjectTestPosition {
            x,
            y,
            z,
            peak_dbfs,
            rms_dbfs,
        } => {
            live.object_test_position = Some(ObjectTestPosition {
                x,
                y,
                z,
                peak_dbfs,
                rms_dbfs,
            });
            Change::Scene
        }
        OscEvent::StateDecodeTimeMs { value } => {
            live.app.decode_time_ms = Some(value);
            Change::None
        }
        OscEvent::StateRenderTimeMs { value } => {
            live.app.render_time_ms = Some(value);
            Change::None
        }
        OscEvent::StateCrossoverTimeMs { value } => {
            live.app.crossover_time_ms = Some(value);
            Change::None
        }
        OscEvent::StateWriteTimeMs { value } => {
            live.app.write_time_ms = Some(value);
            Change::None
        }
        OscEvent::StateFrameDurationMs { value } => {
            live.app.frame_duration_ms = Some(value);
            Change::None
        }
        OscEvent::StateResampleRatio { value } => {
            live.app.resample_ratio = Some(value);
            Change::None
        }
        OscEvent::StateRenderBridgePath { value } => {
            live.app.render_bridge_path = non_empty(value);
            Change::None
        }
        OscEvent::StateRenderConfigPath { value } => {
            live.app.render_config_path = non_empty(value);
            Change::None
        }
        OscEvent::StateRenderConfigStatus { value } => {
            live.app.render_config_status = non_empty(value);
            Change::None
        }
        OscEvent::StateRenderVersion { value } => {
            live.app.render_version = non_empty(value);
            Change::None
        }
        OscEvent::StateRenderExecutable { value } => {
            live.app.render_executable = non_empty(value);
            Change::None
        }
        OscEvent::StateRenderAbi { value } => {
            live.app.render_abi = non_empty(value);
            Change::None
        }
        OscEvent::StateRenderBridgeError { value } => {
            live.app.render_bridge_error = non_empty(value);
            Change::None
        }
        // Log lines, recompute status and anything else the viewport does not
        // draw are ignored here; the panels phase picks them up.
        _ => Change::None,
    }
}

fn snapshot_if(changed: bool) -> Change {
    if changed {
        Change::Snapshot
    } else {
        Change::None
    }
}
