//! A JSON file for the UI's own preferences. The UI owns the types it stores;
//! the core owns the file I/O, so the UI tier never touches the disk.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Reads `path`, falling back to the default when the file is missing. A file
/// that does not parse is logged and replaced by the default: preferences are
/// a convenience, never a reason not to start.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    let Ok(data) = std::fs::read_to_string(path) else {
        return T::default();
    };
    match serde_json::from_str(&data) {
        Ok(value) => value,
        Err(e) => {
            log::warn!("[prefs] {}: {e}; using defaults", path.display());
            T::default()
        }
    }
}

/// Writes `value` to `path`, creating its directory first. A failure is
/// logged, not returned: there is nothing the caller could do about it.
pub fn save<T: Serialize>(path: &Path, value: &T) {
    if let Err(e) = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(|e| e.to_string())
        .and_then(|()| serde_json::to_string_pretty(value).map_err(|e| e.to_string()))
        .and_then(|data| std::fs::write(path, data).map_err(|e| e.to_string()))
    {
        log::warn!("[prefs] could not save {}: {e}", path.display());
    }
}
