//! Persisted UI preferences. The web Studio keeps these in `localStorage`
//! (`spatialviz.*` keys); a native window has no such store, so they live in a
//! JSON file next to the OSC config, under the same per-environment config
//! directory (`OMNIPHONY_CONFIG_DIR`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::panels::object_test::ObjectTestPrefs;
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
}

fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("studio-egui-prefs.json")
}

pub fn load(config_dir: &Path) -> Prefs {
    let file = path(config_dir);
    let Ok(data) = std::fs::read_to_string(&file) else {
        return Prefs::default();
    };
    match serde_json::from_str(&data) {
        Ok(prefs) => prefs,
        Err(e) => {
            log::warn!("[prefs] {}: {e}; using defaults", file.display());
            Prefs::default()
        }
    }
}

pub fn save(config_dir: &Path, prefs: &Prefs) {
    if let Err(e) = std::fs::create_dir_all(config_dir)
        .map_err(|e| e.to_string())
        .and_then(|()| serde_json::to_string_pretty(prefs).map_err(|e| e.to_string()))
        .and_then(|data| std::fs::write(path(config_dir), data).map_err(|e| e.to_string()))
    {
        log::warn!("[prefs] could not save: {e}");
    }
}
