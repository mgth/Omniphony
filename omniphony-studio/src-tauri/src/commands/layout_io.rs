//! Layout selection, import/export and the native file-dialog pickers (layouts,
//! evaluation artifacts, bridge & orender executables).
//!
//! Unlike most command modules these touch the shared [`AppState`] layout list
//! and the local filesystem rather than only forwarding over OSC.
//!
//! [`AppState`]: crate::app_state::AppState

use crate::config::{load_config, save_config};
use crate::layouts::{self, Layout};
use crate::SharedState;
use rfd::FileDialog;
use std::path::Path;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
pub fn select_layout(state: State<SharedState>, key: String) -> bool {
    let mut s = state.inner.lock().unwrap();
    let exists = s.layouts.iter().any(|l| l.key == key);
    if exists {
        s.selected_layout_key = Some(key);
    }
    exists
}

#[tauri::command]
pub fn import_layout_from_path(
    state: State<SharedState>,
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
fn import_start_dir(app: &AppHandle, state: &State<SharedState>) -> Option<std::path::PathBuf> {
    if let Some(dir) = load_config(&state.config_dir).last_layout_import_dir {
        let p = std::path::PathBuf::from(dir);
        if p.is_dir() {
            return Some(p);
        }
    }
    app.path()
        .resource_dir()
        .ok()
        .map(|d| d.join("layouts"))
        .filter(|p| p.is_dir())
}

#[tauri::command]
pub fn pick_import_layout_path(app: AppHandle, state: State<SharedState>) -> Option<String> {
    let mut dialog = FileDialog::new().add_filter("Layout", &["json", "yaml", "yml"]);
    if let Some(dir) = import_start_dir(&app, &state) {
        dialog = dialog.set_directory(dir);
    }
    let picked = dialog.pick_file()?;

    // Remember where the user imported from for next time.
    if let Some(parent) = picked.parent() {
        let mut cfg = load_config(&state.config_dir);
        cfg.last_layout_import_dir = Some(parent.to_string_lossy().to_string());
        let _ = save_config(&state.config_dir, &cfg);
    }

    Some(picked.to_string_lossy().to_string())
}

/// Picker for the dedicated "Presets" button: always opens in the bundled
/// presets dir. Unlike the generic import picker it ignores — and doesn't
/// update — the remembered import dir, since the presets live at a fixed
/// location the user shouldn't have to navigate back to.
#[tauri::command]
pub fn pick_preset_layout_path(app: AppHandle) -> Option<String> {
    let mut dialog = FileDialog::new().add_filter("Layout", &["json", "yaml", "yml"]);
    if let Some(dir) = app
        .path()
        .resource_dir()
        .ok()
        .map(|d| d.join("layouts"))
        .filter(|p| p.is_dir())
    {
        dialog = dialog.set_directory(dir);
    }
    dialog
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn pick_export_layout_path(suggested_name: Option<String>) -> Option<String> {
    let file_name = suggested_name
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
        .unwrap_or_else(|| "layout.yaml".to_string());

    FileDialog::new()
        .add_filter("Layout YAML", &["yaml", "yml"])
        .add_filter("Layout JSON", &["json"])
        .set_file_name(&file_name)
        .save_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn pick_import_evaluation_artifact_path() -> Option<String> {
    FileDialog::new()
        .add_filter("Omniphony evaluator", &["oevl"])
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn pick_export_evaluation_artifact_path(suggested_name: Option<String>) -> Option<String> {
    let file_name = suggested_name
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
        .unwrap_or_else(|| "evaluation.oevl".to_string());

    FileDialog::new()
        .add_filter("Omniphony evaluator", &["oevl"])
        .set_file_name(&file_name)
        .save_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn pick_bridge_path() -> Option<String> {
    FileDialog::new()
        .set_title("Select bridge library")
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
pub fn pick_orender_path() -> Option<String> {
    FileDialog::new()
        .set_title("Select orender executable")
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

/// Native picker for an editable backend file (e.g. a script `.lua`), restricted
/// to `extensions` when non-empty. The returned path is in *this* machine's
/// namespace, so the UI only offers Browse when the renderer is local
/// (see `renderer_is_local`).
#[tauri::command]
pub fn pick_backend_file_path(extensions: Vec<String>) -> Option<String> {
    let mut dialog = FileDialog::new().set_title("Select backend file");
    if !extensions.is_empty() {
        let exts: Vec<&str> = extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter("Backend file", &exts);
    }
    dialog
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
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
#[tauri::command]
pub fn default_layout_export_name(layout: Layout) -> String {
    layouts::sanitize_export_name(&layouts::default_export_name(&layout.speakers))
}
