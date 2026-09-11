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
