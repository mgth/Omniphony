//! Native file dialogs. A dialog is UI; where it opens, what it remembers and
//! the names it proposes are the core's (`host::commands::layout_io`), and so
//! is whatever is done with the path it returns.

#![allow(dead_code)] // the host's full set of pickers, ported ahead of their panels

use rfd::FileDialog;

use crate::host::commands::layout_io;

fn path_string(path: std::path::PathBuf) -> String {
    path.to_string_lossy().to_string()
}

pub fn pick_import_evaluation_artifact_path() -> Option<String> {
    FileDialog::new()
        .add_filter("Omniphony evaluator", &["oevl"])
        .pick_file()
        .map(path_string)
}

pub fn pick_export_evaluation_artifact_path(suggested_name: Option<String>) -> Option<String> {
    FileDialog::new()
        .add_filter("Omniphony evaluator", &["oevl"])
        .set_file_name(layout_io::evaluation_export_file_name(suggested_name))
        .save_file()
        .map(path_string)
}

pub fn pick_bridge_path() -> Option<String> {
    FileDialog::new()
        .set_title("Select bridge library")
        .pick_file()
        .map(path_string)
}

pub fn pick_orender_path() -> Option<String> {
    FileDialog::new()
        .set_title("Select orender executable")
        .pick_file()
        .map(path_string)
}

/// Native picker for an editable backend file (e.g. a script `.lua`), restricted
/// to `extensions` when non-empty. The returned path is in *this* machine's
/// namespace, so the UI only offers Browse when the renderer is local
/// (see `renderer_is_local`).
pub fn pick_backend_file_path(extensions: Vec<String>) -> Option<String> {
    let mut dialog = FileDialog::new().set_title("Select backend file");
    if !extensions.is_empty() {
        let exts: Vec<&str> = extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter("Backend file", &exts);
    }
    dialog.pick_file().map(path_string)
}
