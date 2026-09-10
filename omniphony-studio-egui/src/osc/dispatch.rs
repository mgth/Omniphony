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

use crate::host::peak_hold::PeakHolds;
use crate::host::timing_stats::{TimeWindow, WindowStats};
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
#[allow(dead_code)] // read by the object-test editor, a later panel
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
    /// Origin of the millisecond clock the rolling windows are keyed on.
    pub started: Instant,
    /// Instant latency over the last four seconds, so the meter can show the
    /// Per-stage timing rings, in the order decode, crossover, render, write.
    /// The host used to compute these and broadcast them as `latency:stats`;
    /// here the panel reads them where they are made.
    pub stage_windows: [TimeWindow; 4],
    /// spread the host used to compute for the web (`LATENCY_RAW_WINDOW_MS`).
    pub latency_window: TimeWindow,
    /// When the last spatial frame arrived. The channel editor's at-rest
    /// markers stand down while a stream owns the scene.
    pub last_spatial_frame_at: Option<Instant>,
    /// Peak-hold cursors of every meter, keyed as the host keys them
    /// (`master`, `spk:<id>`, `src:<id>`, `ear:<id>`).
    pub peaks: PeakHolds,
    /// Held peak in dBFS per meter key, the value the host would have put in
    /// `peakHoldDbfs`.
    pub peak_hold_db: HashMap<String, f64>,
    /// Log ring shown by the log overlay (`src/log.js`, 120 entries).
    pub log: VecDeque<LogLine>,
    /// Set while a config save is in flight (`app.saveRequested`).
    pub save_requested: bool,
    /// Decoded `state:object_test:clip` document.
    pub object_test_clip: Option<serde_json::Value>,
    /// Script files declared by each backend, and the last one fetched.
    pub backend_files: HashMap<String, Vec<String>>,
    pub backend_file_content: Option<BackendFile>,
}

/// One rendered log line. `src/log.js` keeps the newest 120 and prefixes the
/// target in brackets unless the message already carries one.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub at: Instant,
}

impl LogLine {
    /// `buildRenderedMessage`: `[target] message`, unless the message already
    /// starts with a bracket or there is no target.
    pub fn rendered(&self) -> String {
        if self.message.starts_with('[') || self.target.is_empty() {
            self.message.clone()
        } else {
            format!("[{}] {}", self.target, self.message)
        }
    }
}

/// The levels `log.js` renders. Anything else it receives becomes `Info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => LogLevel::Error,
            "warn" => LogLevel::Warn,
            "debug" => LogLevel::Debug,
            "trace" => LogLevel::Trace,
            _ => LogLevel::Info,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }
}

/// A backend script file fetched for the editor.
#[derive(Clone, Debug)]
#[allow(dead_code)] // read by the script editor, a later panel
pub struct BackendFile {
    pub backend: String,
    pub key: String,
    pub name: String,
    pub content: String,
}

/// The host's `LATENCY_RAW_WINDOW_MS`.
const LATENCY_RAW_WINDOW_MS: u64 = 4000;
/// The host's `RENDER_TIME_WINDOW_MS`: how far back a stage's worst case is
/// remembered.
pub const RENDER_TIME_WINDOW_MS: u64 = 5000;
/// The host's `RENDER_TIME_AVERAGE_WINDOW_MS`: what the readouts average over.
pub const RENDER_TIME_AVERAGE_WINDOW_MS: u64 = 1000;

/// The four stages of the renderer's frame, in the order they are drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Decode,
    Crossover,
    Render,
    Write,
}

impl Stage {
    fn index(self) -> usize {
        match self {
            Stage::Decode => 0,
            Stage::Crossover => 1,
            Stage::Render => 2,
            Stage::Write => 3,
        }
    }
}

/// `LOG_ENTRY_LIMIT` of `src/log.js`.
const LOG_ENTRY_LIMIT: usize = 120;

impl std::ops::Deref for Live {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        &self.app
    }
}

impl std::ops::DerefMut for Live {
    fn deref_mut(&mut self) -> &mut AppState {
        &mut self.app
    }
}

impl Live {
    /// One declared live option, falling back to the published schema's
    /// default when the renderer has not sent a snapshot yet
    /// (`getLiveOption`).
    pub fn option(&self, key: &str) -> Option<serde_json::Value> {
        if let Some(value) = self
            .app
            .options
            .as_ref()
            .and_then(|o| o.get(key))
            .filter(|v| !v.is_null())
        {
            return Some(value.clone());
        }
        self.options_schema
            .as_ref()
            .and_then(|s| s.as_array())
            .and_then(|specs| {
                specs
                    .iter()
                    .find(|spec| spec.get("key").and_then(|k| k.as_str()) == Some(key))
            })
            .and_then(|spec| spec.get("default").cloned())
    }

    pub fn option_str(&self, key: &str) -> Option<String> {
        self.option(key)
            .and_then(|v| v.as_str().map(|s| s.to_owned()))
    }

    pub fn option_f64(&self, key: &str) -> Option<f64> {
        self.option(key).and_then(|v| v.as_f64())
    }

    #[allow(dead_code)] // used by the bool options of panels not ported yet
    pub fn option_bool(&self, key: &str) -> Option<bool> {
        self.option(key).and_then(|v| v.as_bool())
    }

    /// Optimistic write into the option store, so a control does not lag the
    /// click while the renderer echoes the canonical value.
    pub fn set_option(&mut self, key: &str, value: serde_json::Value) {
        let options = self
            .app
            .options
            .get_or_insert_with(|| serde_json::Value::Object(Default::default()));
        if let Some(map) = options.as_object_mut() {
            map.insert(key.to_owned(), value);
        }
    }

    /// Feed one meter to its peak-hold cursor, the way the host does before
    /// emitting `peakHoldDbfs`.
    fn hold(&mut self, key: String, peak_dbfs: f64) {
        let held = self.peaks.update(&key, peak_dbfs, Instant::now());
        self.peak_hold_db.insert(key, held);
    }

    /// Held peak of one meter key, if it has ever had a sample.
    pub fn peak_hold(&self, key: &str) -> Option<f64> {
        self.peak_hold_db.get(key).copied()
    }

    /// Record one instant-latency sample in the rolling window.
    pub fn record_latency(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        let now_ms = self.started.elapsed().as_millis() as u64;
        self.latency_window.record(now_ms, value);
    }

    /// Spread of the instant latency over the window, if it has samples.
    /// Feed one stage sample. Non-finite values are dropped rather than
    /// poisoning the window's mean.
    pub fn record_stage(&mut self, stage: Stage, value: f64) {
        if !value.is_finite() {
            return;
        }
        let now_ms = self.started.elapsed().as_millis() as u64;
        self.stage_windows[stage.index()].record(now_ms, value);
    }

    /// A stage's one-second mean and five-second worst case.
    pub fn stage_stats(&self, stage: Stage) -> (Option<WindowStats>, Option<WindowStats>) {
        let now_ms = self.started.elapsed().as_millis() as u64;
        let window = &self.stage_windows[stage.index()];
        (
            window.stats(now_ms, RENDER_TIME_AVERAGE_WINDOW_MS),
            window.stats(now_ms, RENDER_TIME_WINDOW_MS),
        )
    }

    pub fn latency_stats(&self) -> Option<WindowStats> {
        let now_ms = self.started.elapsed().as_millis() as u64;
        self.latency_window.stats(now_ms, LATENCY_RAW_WINDOW_MS)
    }

    /// `pushLog`: drop empty messages, coerce the level, keep the newest 120.
    pub fn push_log(&mut self, level: &str, target: &str, message: impl Into<String>) {
        let message: String = message.into();
        let message = message.trim().to_owned();
        if message.is_empty() {
            return;
        }
        self.log.push_back(LogLine {
            level: LogLevel::parse(level),
            target: target.trim().to_owned(),
            message,
            at: Instant::now(),
        });
        while self.log.len() > LOG_ENTRY_LIMIT {
            self.log.pop_front();
        }
    }

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
            last_spatial_frame_at: None,
            stage_windows: std::array::from_fn(|_| TimeWindow::new(RENDER_TIME_WINDOW_MS)),
            latency_window: TimeWindow::new(LATENCY_RAW_WINDOW_MS),
            peaks: PeakHolds::new(),
            peak_hold_db: HashMap::new(),
            log: VecDeque::new(),
            save_requested: false,
            object_test_clip: None,
            backend_files: HashMap::new(),
            backend_file_content: None,
            options_schema: None,
            object_generators_schema: None,
            phantom_schema: None,
            drc_gain: None,
            ear_levels: HashMap::new(),
            clip: None,
            last_frame_reset: None,
            snapshot_epoch: 0,
            started: Instant::now(),
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
            live.last_spatial_frame_at = Some(Instant::now());
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
            live.peaks.forget(&format!("src:{id}"));
            live.peak_hold_db.remove(&format!("src:{id}"));
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
            live.hold(format!("src:{id}"), peak_dbfs);
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
            live.hold(format!("spk:{id}"), peak_dbfs);
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
            live.hold(format!("ear:{id}"), peak_dbfs);
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
            live.hold("master".to_owned(), peak_dbfs);
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
            live.record_latency(value);
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
            live.record_stage(Stage::Decode, value);
            Change::None
        }
        OscEvent::StateRenderTimeMs { value } => {
            live.app.render_time_ms = Some(value);
            live.record_stage(Stage::Render, value);
            Change::None
        }
        OscEvent::StateCrossoverTimeMs { value } => {
            live.app.crossover_time_ms = Some(value);
            live.record_stage(Stage::Crossover, value);
            Change::None
        }
        OscEvent::StateWriteTimeMs { value } => {
            live.app.write_time_ms = Some(value);
            live.record_stage(Stage::Write, value);
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

        // ── panels: renderer evaluation grid ──────────────────────────────
        // `0` means "not set" for every size but `z_neg_size`, where the
        // renderer's zero is a real value (`tauri-bridge.js`).
        OscEvent::StateRenderEvaluationCartesianXSize { value } => {
            live.app.vbap_cartesian.x_size = positive(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationCartesianYSize { value } => {
            live.app.vbap_cartesian.y_size = positive(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationCartesianZSize { value } => {
            live.app.vbap_cartesian.z_size = positive(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationCartesianZNegSize { value } => {
            live.app.vbap_cartesian.z_neg_size = Some(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationPolarAzimuthResolution { value } => {
            live.app.vbap_polar.azimuth_resolution = positive(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationPolarElevationResolution { value } => {
            live.app.vbap_polar.elevation_resolution = positive(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationPolarDistanceRes { value } => {
            live.app.vbap_polar.distance_res = positive(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationPolarDistanceMax { value } => {
            live.app.vbap_polar.distance_max = (value > 0.0).then_some(value);
            Change::Snapshot
        }
        OscEvent::StateRenderEvaluationPositionInterpolation { enabled } => {
            live.app.vbap_polar.position_interpolation = Some(enabled);
            Change::Snapshot
        }
        OscEvent::StateVbapAllowNegativeZ { enabled } => {
            live.app.vbap_allow_negative_z = Some(enabled);
            Change::Snapshot
        }
        OscEvent::StateSpeakersRecomputing { enabled } => {
            live.app.vbap_recomputing = Some(enabled);
            if enabled {
                live.app.recompute_error = None;
            }
            Change::Snapshot
        }
        OscEvent::StateSpeakersRecomputeError { message } => {
            live.app.recompute_error = non_empty(message);
            if live.app.recompute_error.is_some() {
                live.app.vbap_recomputing = Some(false);
            }
            Change::Snapshot
        }

        // ── panels: configuration save feedback ──────────────────────────
        OscEvent::StateConfigSaved { saved } => {
            live.app.config_saved = Some(u8::from(saved));
            live.app.save_error = None;
            live.save_requested = false;
            Change::Snapshot
        }
        OscEvent::StateConfigSaveError { message } => {
            let message = non_empty(message);
            if let Some(text) = &message {
                live.push_log("error", "config", text.clone());
            }
            live.app.save_error = message;
            live.save_requested = false;
            Change::Snapshot
        }

        // ── panels: object test, log ─────────────────────────────────────
        OscEvent::StateObjectTestClip { value } => {
            live.object_test_clip = serde_json::from_str(&value).ok();
            Change::Snapshot
        }
        OscEvent::StateLogLevel { value } => {
            live.app.log_level = non_empty(value);
            Change::Snapshot
        }
        OscEvent::Log { entry } => {
            live.push_log(&entry.level, &entry.target, entry.message);
            Change::Snapshot
        }

        // ── panels: adaptive resampling ──────────────────────────────────
        OscEvent::StateAdaptiveResampling { enabled } => {
            live.app.adaptive_resampling = Some(u8::from(enabled));
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingEnableFarMode { enabled } => {
            live.app.adaptive_resampling_enable_far_mode = Some(u8::from(enabled));
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingForceSilenceInFarMode { enabled } => {
            live.app.adaptive_resampling_force_silence_in_far_mode = Some(u8::from(enabled));
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingHardRecoverHighInFarMode { enabled } => {
            live.app.adaptive_resampling_hard_recover_high_in_far_mode = Some(u8::from(enabled));
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingHardRecoverLowInFarMode { enabled } => {
            live.app.adaptive_resampling_hard_recover_low_in_far_mode = Some(u8::from(enabled));
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingFarModeReturnFadeInMs { value } => {
            live.app.adaptive_resampling_far_mode_return_fade_in_ms = Some(value.round() as i64);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingKpNear { value } => {
            live.app.adaptive_resampling_kp_near = Some(value);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingKi { value } => {
            live.app.adaptive_resampling_ki = Some(value);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingIntegralDischargeRatio { value } => {
            live.app.adaptive_resampling_integral_discharge_ratio = Some(value);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingMaxAdjust { value } => {
            live.app.adaptive_resampling_max_adjust = Some(value);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingUpdateIntervalCallbacks { value } => {
            live.app.adaptive_resampling_update_interval_callbacks = Some(value.round() as i64);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingHighRecoverEntryMarginMs { value } => {
            live.app.adaptive_resampling_high_recover_entry_margin_ms = Some(value.round() as i64);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingBand { value } => {
            live.app.adaptive_resampling_band = non_empty(value);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingState { value } => {
            live.app.adaptive_resampling_state = non_empty(value);
            Change::Snapshot
        }
        OscEvent::StateAdaptiveResamplingPaused { enabled } => {
            live.app.adaptive_resampling_paused = Some(u8::from(enabled));
            Change::Snapshot
        }

        // ── panels: backend script files (editor phase) ──────────────────
        OscEvent::StateBackendFileList { backend, json } => {
            let names: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
            live.backend_files.insert(backend, names);
            Change::Snapshot
        }
        OscEvent::StateBackendFileContent {
            backend,
            key,
            name,
            content,
        } => {
            live.backend_file_content = Some(BackendFile {
                backend,
                key,
                name,
                content,
            });
            Change::Snapshot
        }
        OscEvent::StateBackendFileError {
            backend,
            key,
            message,
        } => {
            live.push_log("error", "backend", format!("{backend}/{key}: {message}"));
            Change::Snapshot
        }

        // Events the viewport does not draw and no panel reads yet.
        _ => Change::None,
    }
}

/// `0` means "unset" for the evaluation-grid sizes (`tauri-bridge.js`).
fn positive(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}

fn snapshot_if(changed: bool) -> Change {
    if changed {
        Change::Snapshot
    } else {
        Change::None
    }
}

#[cfg(test)]
mod panel_event_tests {
    use super::*;
    use crate::osc::parser::LogEntry;

    fn live() -> Live {
        Live::new(AppState::new(Vec::new()))
    }

    #[test]
    fn evaluation_sizes_treat_zero_as_unset_except_the_negative_z_one() {
        let mut l = live();
        apply_event(
            &mut l,
            OscEvent::StateRenderEvaluationCartesianXSize { value: 0 },
        );
        apply_event(
            &mut l,
            OscEvent::StateRenderEvaluationCartesianYSize { value: 9 },
        );
        apply_event(
            &mut l,
            OscEvent::StateRenderEvaluationCartesianZNegSize { value: 0 },
        );
        assert_eq!(l.app.vbap_cartesian.x_size, None);
        assert_eq!(l.app.vbap_cartesian.y_size, Some(9));
        assert_eq!(l.app.vbap_cartesian.z_neg_size, Some(0));
    }

    #[test]
    fn a_recompute_error_stops_the_recomputing_flag_and_the_reverse() {
        let mut l = live();
        apply_event(&mut l, OscEvent::StateSpeakersRecomputing { enabled: true });
        assert_eq!(l.app.vbap_recomputing, Some(true));
        apply_event(
            &mut l,
            OscEvent::StateSpeakersRecomputeError {
                message: "no hull".to_owned(),
            },
        );
        assert_eq!(l.app.recompute_error.as_deref(), Some("no hull"));
        assert_eq!(l.app.vbap_recomputing, Some(false));
        apply_event(&mut l, OscEvent::StateSpeakersRecomputing { enabled: true });
        assert_eq!(l.app.recompute_error, None);
    }

    #[test]
    fn a_save_error_is_logged_and_clears_the_pending_save() {
        let mut l = live();
        l.save_requested = true;
        apply_event(
            &mut l,
            OscEvent::StateConfigSaveError {
                message: "read-only".to_owned(),
            },
        );
        assert!(!l.save_requested);
        assert_eq!(l.app.save_error.as_deref(), Some("read-only"));
        assert_eq!(l.log.len(), 1);
        assert_eq!(l.log[0].level, LogLevel::Error);
    }

    #[test]
    fn the_log_ring_keeps_the_newest_entries_and_renders_the_target() {
        let mut l = live();
        for i in 0..LOG_ENTRY_LIMIT + 5 {
            apply_event(
                &mut l,
                OscEvent::Log {
                    entry: LogEntry {
                        seq: i as u64,
                        level: "warn".to_owned(),
                        target: "render".to_owned(),
                        message: format!("line {i}"),
                    },
                },
            );
        }
        assert_eq!(l.log.len(), LOG_ENTRY_LIMIT);
        assert_eq!(l.log.back().unwrap().rendered(), "[render] line 124");
        assert_eq!(l.log.front().unwrap().message, "line 5");
    }

    #[test]
    fn an_empty_log_message_is_dropped_and_a_bracketed_one_is_kept_verbatim() {
        let mut l = live();
        l.push_log("info", "render", "   ");
        assert!(l.log.is_empty());
        l.push_log("nonsense", "render", "[osc] already tagged");
        assert_eq!(l.log[0].level, LogLevel::Info);
        assert_eq!(l.log[0].rendered(), "[osc] already tagged");
    }

    #[test]
    fn adaptive_resampling_values_round_to_the_stored_integer_type() {
        let mut l = live();
        apply_event(
            &mut l,
            OscEvent::StateAdaptiveResamplingUpdateIntervalCallbacks { value: 12.6 },
        );
        assert_eq!(
            l.app.adaptive_resampling_update_interval_callbacks,
            Some(13)
        );
    }
}
