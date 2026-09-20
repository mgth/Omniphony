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
    /// Missing versions are the original native format (version 1). Reject
    /// future schemas rather than loading defaults and overwriting their data.
    #[serde(default, deserialize_with = "read_schema_version")]
    pub schema_version: SchemaVersion,
    /// `spatialviz.side_panels`.
    pub side_panels: OverlayLayout,
    /// `spatialviz.locale` (`auto`, or a bundled locale key).
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

/// Kept private internally so newly saved documents always use the supported version.
#[derive(Clone, Debug)]
pub struct SchemaVersion;
impl Default for SchemaVersion {
    fn default() -> Self {
        Self
    }
}
impl Serialize for SchemaVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(1)
    }
}
fn read_schema_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<SchemaVersion, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version == 1 {
        Ok(SchemaVersion)
    } else {
        Err(serde::de::Error::custom(format!(
            "unsupported preference schema {version}; this Studio supports version 1"
        )))
    }
}

fn path(config_dir: &Path) -> PathBuf {
    config_dir.join("studio-egui-prefs.json")
}

pub fn load(config_dir: &Path, legacy_dir: &Path) -> (Prefs, Option<String>, bool) {
    let migration_error = if crate::host::runtime_env::config_dir().is_none() {
        json_store::migrate::<Prefs>(&path(legacy_dir), &path(config_dir)).err()
    } else {
        None
    };
    let (prefs, load_error) = json_store::load(&path(config_dir));
    let writable = load_error.is_none() && migration_error.is_none();
    (prefs, load_error.or(migration_error), writable)
}

pub fn writer(
    config_dir: &Path,
    wake: crate::osc::Waker,
    error: Option<String>,
    writable: bool,
) -> std::io::Result<json_store::Writer<Prefs>> {
    if !writable {
        return Ok(json_store::Writer::read_only(
            error.unwrap_or_else(|| "Preferences are read-only".into()),
        ));
    }
    json_store::Writer::new(
        path(config_dir),
        std::time::Duration::from_millis(600),
        wake,
        error,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_unversioned_preferences_upgrade_without_losing_their_values() {
        let prefs: Prefs = serde_json::from_str(r#"{"locale":"fr"}"#).unwrap();
        let saved = serde_json::to_value(&prefs).unwrap();
        assert_eq!(saved["schema_version"], 1);
        assert_eq!(saved["locale"], "fr");
        let reloaded: Prefs = serde_json::from_value(saved).unwrap();
        assert_eq!(reloaded.locale.as_deref(), Some("fr"));
    }
    #[test]
    fn unsupported_or_invalid_versions_are_not_silently_downgraded() {
        for schema in [
            serde_json::json!(0),
            serde_json::json!(2),
            serde_json::json!("1"),
            serde_json::Value::Null,
        ] {
            let error =
                serde_json::from_value::<Prefs>(serde_json::json!({"schema_version": schema}))
                    .unwrap_err();
            assert!(!error.to_string().is_empty());
        }
    }
}
