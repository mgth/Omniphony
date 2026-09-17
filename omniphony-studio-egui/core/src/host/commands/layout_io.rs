//! Layout selection and import/export, and what the file dialogs need from the
//! core: where they start, what they remember, the names they propose. The
//! dialogs themselves are UI (`ui::file_dialogs`).
//!
//! Unlike most command modules these touch the shared [`AppState`] layout list
//! and the local filesystem rather than only forwarding over OSC.
//!
//! [`AppState`]: crate::model::app_state::AppState

use super::HostPaths;

use super::SharedState;
use crate::host::config::{load_config, save_config};
use crate::model::layouts::{self, Layout};
use std::path::Path;

pub fn select_layout(state: &SharedState, key: String) -> bool {
    let mut s = state.inner.lock().unwrap();
    let exists = s.layouts.iter().any(|l| l.key == key);
    if exists {
        s.selected_layout_key = Some(key);
    }
    exists
}

pub fn import_layout_from_path(
    state: &SharedState,
    path: String,
) -> Result<serde_json::Value, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty layout path".to_string());
    }
    let mut layout = layouts::load_layout_file(Path::new(trimmed))
        .ok_or_else(|| "failed to parse layout file".to_string())?;

    let mut s = state.inner.lock().unwrap();
    let base_key = layout.key.clone();
    let mut suffix = 1usize;
    while s.layouts.iter().any(|l| l.key == layout.key) {
        layout.key = format!("{base_key}-{}", suffix);
        suffix += 1;
    }
    s.selected_layout_key = Some(layout.key.clone());
    s.layouts.push(layout);
    s.layouts
        .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(serde_json::json!({
        "layouts": s.layouts,
        "selectedLayoutKey": s.selected_layout_key
    }))
}

/// Identity captured when the user starts choosing a layout. A late file read
/// must not replace a different renderer session or profile.
#[derive(Debug)]
pub struct SessionToken {
    eligible: bool,
    request: u64,
    epoch: u64,
    context_generation: u64,
    profile: Option<String>,
}
impl SessionToken {
    pub fn new(state: &SharedState) -> Self {
        let request = *state.connection_request.lock().unwrap();
        let live = state.inner.lock().unwrap();
        Self {
            eligible: live.pending_connection_request.is_none()
                && live.queued_connection_request.is_none(),
            request,
            epoch: state
                .stats
                .connection_epoch
                .load(std::sync::atomic::Ordering::Relaxed),
            profile: live.app.active_profile.clone(),
            context_generation: live.layout_context_generation,
        }
    }

    /// Whether a delayed UI choice still belongs to this renderer/profile.
    /// Commands revalidate on the application thread before queuing their work.
    pub fn is_current(&self, state: &SharedState) -> bool {
        let request = state.connection_request.lock().unwrap();
        let live = state.inner.lock().unwrap();
        self.eligible
            && live.pending_connection_request.is_none()
            && live.queued_connection_request.is_none()
            && *request == self.request
            && state
                .stats
                .connection_epoch
                .load(std::sync::atomic::Ordering::Relaxed)
                == self.epoch
            && live.layout_context_generation == self.context_generation
            && live.app.active_profile == self.profile
    }

    /// Keep validation and queuing a local-file action in one connection
    /// critical section. In particular a DNS worker cannot enqueue Reconnect
    /// between a successful validation and the caller's control message.
    pub fn with_local_target<R>(&self, state: &SharedState, work: impl FnOnce() -> R) -> Option<R> {
        let request = state.connection_request.lock().unwrap();
        {
            let live = state.inner.lock().unwrap();
            if !self.eligible
                || live.pending_connection_request.is_some()
                || live.queued_connection_request.is_some()
                || *request != self.request
                || state
                    .stats
                    .connection_epoch
                    .load(std::sync::atomic::Ordering::Relaxed)
                    != self.epoch
                || live.layout_context_generation != self.context_generation
                || live.app.active_profile != self.profile
            {
                return None;
            }
        }
        if !state
            .stats
            .target
            .lock()
            .unwrap()
            .is_some_and(|target| target.ip().is_loopback())
        {
            return None;
        }
        Some(work())
    }

    /// Runs on the application thread after the worker returns. Filesystem
    /// access is already finished; only validation/model/ordered commands remain.
    pub fn apply(self, state: &SharedState, mut layout: Layout) -> Result<(), String> {
        let request = state.connection_request.lock().unwrap();
        let mut live = state.inner.lock().unwrap();
        if !self.eligible
            || live.pending_connection_request.is_some()
            || live.queued_connection_request.is_some()
            || *request != self.request
            || state
                .stats
                .connection_epoch
                .load(std::sync::atomic::Ordering::Relaxed)
                != self.epoch
            || live.layout_context_generation != self.context_generation
            || live.app.active_profile != self.profile
        {
            return Err("Layout import discarded: renderer session or profile changed".into());
        }
        if live.app.render_backend_state.frozen_speakers {
            return Err("Layout import refused: speakers are frozen".into());
        }
        let payload = replace_layout_payload(&layout);
        let base = layout.key.clone();
        let mut suffix = 1usize;
        while live.app.layouts.iter().any(|item| item.key == layout.key) {
            layout.key = format!("{base}-{suffix}");
            suffix += 1;
        }
        live.app.selected_layout_key = Some(layout.key.clone());
        live.app.layouts.push(layout);
        live.app
            .layouts
            .sort_by_cached_key(|item| item.name.to_lowercase());
        drop(live);
        // Keep the connection-intent lock until both commands are queued, so
        // a concurrent reconnect cannot split the replacement and its apply.
        super::speakers::apply_layout_document(state, payload);
        Ok(())
    }
}

pub fn prepare_import(
    state: &std::sync::Arc<SharedState>,
    presets: bool,
) -> std::sync::mpsc::Receiver<Option<std::path::PathBuf>> {
    let host = state.clone();
    crate::host::services::jobs::run(state, move || {
        if presets {
            presets_dir(&host.paths)
        } else {
            import_start_dir(&host.paths, &host)
        }
    })
}

pub fn read_import(
    state: &std::sync::Arc<SharedState>,
    path: std::path::PathBuf,
    remember: bool,
) -> std::sync::mpsc::Receiver<Result<Layout, String>> {
    let host = state.clone();
    crate::host::services::jobs::run(state, move || {
        let layout = layouts::load_layout_file(&path)
            .ok_or_else(|| "failed to parse layout file".to_string())?;
        if remember && let Some(parent) = path.parent() {
            remember_import_dir(&host, parent);
        }
        Ok(layout)
    })
}

pub fn write_export(
    state: &std::sync::Arc<SharedState>,
    path: std::path::PathBuf,
    mut layout: Layout,
) -> std::sync::mpsc::Receiver<Result<(), String>> {
    crate::host::services::jobs::run(state, move || {
        for speaker in &mut layout.speakers {
            layouts::normalize_for_export(speaker);
        }
        layouts::save_layout_file(&path, &layout)
    })
}

pub fn selected_layout(state: &SharedState) -> Option<Layout> {
    let live = state.inner.lock().unwrap();
    live.app
        .layouts
        .iter()
        .find(|layout| Some(&layout.key) == live.app.selected_layout_key.as_ref())
        .cloned()
}

/// Directory the import picker should open in: the user's last import dir if
/// known, otherwise the bundled layouts dir (so a first-time user lands right
/// on the shipped presets).
pub fn import_start_dir(app: &HostPaths, state: &SharedState) -> Option<std::path::PathBuf> {
    if let Some(dir) = load_config(&state.config_dir).last_layout_import_dir {
        let p = std::path::PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    presets_dir(app)
}

/// The bundled presets, where the "Presets" picker always opens.
pub fn presets_dir(app: &HostPaths) -> Option<std::path::PathBuf> {
    app.resource_dir()
        .ok()
        .map(|d| d.join("layouts"))
        .filter(|p| p.is_dir())
}

/// Remember where the user imported from, for the next import.
pub fn remember_import_dir(state: &SharedState, dir: &Path) {
    let mut cfg = load_config(&state.config_dir);
    cfg.last_layout_import_dir = Some(dir.to_string_lossy().to_string());
    let _ = save_config(&state.config_dir, &cfg);
}

/// The file name a layout export dialog proposes: the suggestion with a
/// layout extension, `.yaml` when it has none, or `layout.yaml`.
pub fn layout_export_file_name(suggested_name: Option<String>) -> String {
    suggested_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let lowered = s.to_ascii_lowercase();
            if lowered.ends_with(".yaml") || lowered.ends_with(".yml") || lowered.ends_with(".json")
            {
                s.to_string()
            } else {
                format!("{s}.yaml")
            }
        })
        .unwrap_or_else(|| "layout.yaml".to_string())
}

/// The file name an evaluation-artifact export dialog proposes.
pub fn evaluation_export_file_name(suggested_name: Option<String>) -> String {
    suggested_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let lowered = s.to_ascii_lowercase();
            if lowered.ends_with(".oevl") {
                s.to_string()
            } else {
                format!("{s}.oevl")
            }
        })
        .unwrap_or_else(|| "evaluation.oevl".to_string())
}

pub fn export_layout_to_path(path: String, layout: Layout) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty export path".to_string());
    }

    // Normalize here rather than trusting the caller. The frontend used to
    // clamp and derive the missing coordinate representation on its way in,
    // which put the rules for a valid stored speaker in two places — and left
    // export following different ones from import.
    let mut layout = layout;
    for speaker in &mut layout.speakers {
        layouts::normalize_for_export(speaker);
    }

    layouts::save_layout_file(Path::new(trimmed), &layout)
}

/// Default file name offered for a layout export, e.g. `7.1.4`.
///
/// Both the convention and the file-name sanitizing live in `layouts.rs`, next
/// to the writer that consumes the result.
pub fn default_layout_export_name(layout: Layout) -> String {
    layouts::sanitize_export_name(&layouts::default_export_name(&layout.speakers))
}

/// `buildReplaceLayoutPayload`: the whole layout as one patch, with the
/// frontend's clamps — which are the ones that matter, since the web never uses
/// the host's single-field helpers.
fn replace_layout_payload(layout: &Layout) -> serde_json::Value {
    let speakers: Vec<serde_json::Value> = layout
        .speakers
        .iter()
        .enumerate()
        .map(|(index, speaker)| {
            let name = if speaker.id.is_empty() {
                format!("spk-{index}")
            } else {
                speaker.id.clone()
            };
            serde_json::json!({
                "name": name,
                "coordMode": speaker.coord_mode,
                "x": speaker.x.clamp(-1.0, 1.0),
                "y": speaker.y.clamp(-1.0, 1.0),
                "z": speaker.z.clamp(-1.0, 1.0),
                "azimuth": finite_or(speaker.azimuth_deg, 0.0),
                "elevation": finite_or(speaker.elevation_deg, 0.0),
                "distance": number_or(speaker.distance_m, 1.0).max(0.01),
                "spatialize": speaker.spatialize != 0,
                "delayMs": speaker.delay_ms.max(0.0),
                // A band edge of zero means "no edge", which is null on the
                // wire and not a 0 Hz corner.
                "freqLow": positive(speaker.freq_low),
                "freqHigh": positive(speaker.freq_high),
            })
        })
        .collect();
    serde_json::json!({ "replaceLayout": {
        "radiusM": number_or(layout.radius_m, 1.0).max(0.01),
        "speakers": speakers,
    }})
}

/// The web's `Number.isFinite(v) ? v : fallback`: an honest zero survives.
fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

/// The web's `Number(v) || fallback`: a zero is not a radius or a distance, so
/// it falls back rather than collapsing the room.
fn number_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value != 0.0 {
        value
    } else {
        fallback
    }
}

fn positive(value: Option<f32>) -> Option<f32> {
    value.filter(|v| v.is_finite() && *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::layouts::Speaker;

    fn speaker(id: &str) -> Speaker {
        Speaker {
            id: id.to_owned(),
            x: 2.0,
            y: -3.0,
            z: 0.5,
            azimuth_deg: 30.0,
            elevation_deg: 0.0,
            distance_m: 0.0,
            coord_mode: "cartesian".to_owned(),
            spatialize: 0,
            delay_ms: -5.0,
            freq_low: Some(0.0),
            freq_high: Some(120.0),
        }
    }

    fn fixture_layout() -> Layout {
        Layout {
            key: "test".into(),
            name: "Test".into(),
            speakers: vec![speaker("L")],
            radius_m: 1.0,
        }
    }
    #[test]
    fn delayed_import_revalidates_session_profile_and_freeze() {
        for change in 0..6 {
            let state = super::super::tests::state();
            let request = SessionToken::new(&state);
            match change {
                0 => {
                    super::super::app::begin_connection(&state);
                }
                1 => {
                    state
                        .stats
                        .connection_epoch
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                2 => {
                    state.inner.lock().unwrap().app.active_profile = Some("another".into());
                }
                3 => {
                    state
                        .inner
                        .lock()
                        .unwrap()
                        .app
                        .render_backend_state
                        .frozen_speakers = true;
                }
                4 => {
                    state.inner.lock().unwrap().layout_context_generation += 1;
                }
                _ => {
                    super::super::profiles::control_profile_switch(&state, "another".into());
                }
            }
            assert!(!request.is_current(&state) || change == 3);
            assert!(request.apply(&state, fixture_layout()).is_err());
            assert!(state.read().app.layouts.is_empty());
        }
    }
    #[test]
    fn a_file_choice_kept_through_confirmation_is_revalidated_at_send_time() {
        let state = super::super::tests::state();
        *state.stats.target.lock().unwrap() = Some("127.0.0.1:9000".parse().unwrap());
        let token = SessionToken::new(&state);
        assert_eq!(token.with_local_target(&state, || 42), Some(42));
        super::super::app::begin_connection(&state);
        assert_eq!(
            token.with_local_target(&state, || panic!("stale file GET must not be sent")),
            None::<()>
        );
        let token = SessionToken::new(&state);
        *state.stats.target.lock().unwrap() = Some("192.0.2.1:9000".parse().unwrap());
        assert_eq!(
            token.with_local_target(&state, || panic!("local path must not be sent remotely")),
            None::<()>
        );
    }
    #[test]
    fn ordinary_snapshot_changes_do_not_invalidate_a_native_file_choice() {
        let state = super::super::tests::state();
        let token = SessionToken::new(&state);
        state.inner.lock().unwrap().snapshot_epoch += 1;
        assert!(token.is_current(&state));
    }
    #[test]
    fn accepted_import_preserves_existing_layout_and_marks_recompute() {
        let state = super::super::tests::state();
        for _ in 0..2 {
            SessionToken::new(&state)
                .apply(&state, fixture_layout())
                .unwrap();
        }
        let live = state.read();
        assert_eq!(live.app.layouts.len(), 2);
        assert_eq!(live.app.selected_layout_key.as_deref(), Some("test-1"));
        assert_eq!(live.app.vbap_recomputing, Some(true));
    }
    #[test]
    fn file_jobs_round_trip_without_mutating_the_model() {
        let state = std::sync::Arc::new(super::super::tests::state());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let wait = std::time::Duration::from_secs(10);
        write_export(&state, path.clone(), fixture_layout())
            .recv_timeout(wait)
            .unwrap()
            .unwrap();
        let imported = read_import(&state, path, false)
            .recv_timeout(wait)
            .unwrap()
            .unwrap();
        assert_eq!(imported.speakers.len(), 1);
        assert!(state.read().app.layouts.is_empty());
        assert!(
            read_import(&state, dir.path().join("missing.json"), false)
                .recv_timeout(wait)
                .unwrap()
                .is_err()
        );
        state.shutdown_jobs();
    }

    #[test]
    fn the_replace_patch_carries_the_frontends_own_clamps() {
        let layout = Layout {
            key: "k".to_owned(),
            name: "n".to_owned(),
            speakers: vec![
                speaker("L"),
                Speaker {
                    id: String::new(),
                    ..speaker("")
                },
            ],
            radius_m: 0.0,
        };
        let payload = replace_layout_payload(&layout);
        let patch = &payload["replaceLayout"];
        // A zero radius is not a room: it falls back rather than collapsing.
        assert_eq!(patch["radiusM"], 1.0);
        let first = &patch["speakers"][0];
        assert_eq!(first["x"], 1.0, "out-of-cube coordinates are clamped");
        assert_eq!(first["y"], -1.0);
        // A zero distance is not a distance: it falls back to one metre
        // rather than collapsing the speaker onto the listener.
        assert_eq!(first["distance"], 1.0);
        assert_eq!(first["delayMs"], 0.0, "a negative delay is not a delay");
        assert_eq!(first["spatialize"], false);
        // An empty band edge is null, not a 0 Hz corner.
        assert!(first["freqLow"].is_null());
        assert_eq!(first["freqHigh"], 120.0);
        // An unnamed speaker gets its index, so the renderer can address it.
        assert_eq!(patch["speakers"][1]["name"], "spk-1");
    }
}
