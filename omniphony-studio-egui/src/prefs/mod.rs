//! Persisted UI preferences. The web Studio keeps these in `localStorage`
//! (`spatialviz.*` keys); a native window has no such store, so they live in a
//! JSON file next to the OSC config, under the same per-environment config
//! directory (`OMNIPHONY_CONFIG_DIR`).
//!
//! What is stored is the UI's business, so the types live here; reading and
//! writing the file is the core's (`host::json_store`).

pub mod display;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::host::json_store;
use crate::panels::diag_plot::DiagPlotPrefs;
use crate::panels::object_test::ObjectTestPrefs;
use crate::panels::updates::UpdatePrefs;
use crate::ui::layout::OverlayLayout;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// `spatialviz.side_panels`.
    pub side_panels: OverlayLayout,
    /// `spatialviz.locale` (`auto`, or a locale key). Only `en` is bundled so
    /// far; the value is kept so the choice survives the cutover.
    pub locale: Option<String>,
    /// The `objectTest.*` keys of the injection editor.
    pub object_test: ObjectTestPrefs,
    /// The `diagPlot.*` keys of the diagnostics plot.
    pub diag_plot: DiagPlotPrefs,
    /// The `omniphony.updateCheck.*` keys of the release check.
    pub updates: UpdatePrefs,
    /// The Display panel: the web's effective-render and trail prefs.
    pub display: display::DisplayPrefs,
}

fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("studio-egui-prefs.json")
}

pub fn load(config_dir: &Path, legacy_dir: &Path) -> (Prefs, Option<String>) {
    let migration_error = if crate::host::runtime_env::config_dir().is_none() {
        json_store::migrate::<Prefs>(&path(legacy_dir), &path(config_dir)).err()
    } else {
        None
    };
    let (prefs, load_error) = json_store::load(&path(config_dir));
    (prefs, load_error.or(migration_error))
}

pub fn writer(
    config_dir: &Path,
    wake: crate::osc::Waker,
    error: Option<String>,
) -> std::io::Result<json_store::Writer<Prefs>> {
    json_store::Writer::new(
        path(config_dir),
        std::time::Duration::from_millis(600),
        wake,
        error,
    )
}
