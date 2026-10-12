//! Host-side resolution and storage for [`ParamKind::File`] backend params.
//!
//! [`ParamKind::File`]: crate::backend_params::ParamKind::File
//!
//! A File param value is a *handle*: an absolute path on the renderer host, or a
//! bare name in the renderer's managed store at
//! `<config_dir>/backend-files/<backend_id>/`. The renderer resolves the handle
//! to an absolute path before a backend factory ever sees it, so backends just
//! read a real path and stay unaware of the store (see [`resolve_file_params`]).
//!
//! The same resolution backs the editor's content I/O (get/put over OSC), with
//! absolute handles gated to local callers via `allow_absolute` so a remote peer
//! can only reach the managed store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::backend_params::ParamValue;

/// Directory under the config dir holding every backend's managed files.
const STORE_DIRNAME: &str = "backend-files";

/// The managed store directory for `backend_id`, or `None` if the renderer has no
/// known config dir to root it in.
pub fn store_dir(config_dir: Option<&Path>, backend_id: &str) -> Option<PathBuf> {
    let name = sanitize_name(backend_id)?;
    Some(config_dir?.join(STORE_DIRNAME).join(name))
}

/// Reduce a handle to a safe bare filename (its last path component), rejecting
/// empty / `.` / `..`. Using the basename inherently prevents path traversal for
/// store-relative handles.
pub fn sanitize_name(handle: &str) -> Option<String> {
    let name = Path::new(handle.trim()).file_name()?.to_str()?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(name.to_string())
}

/// Resolve a File handle to an absolute path on the renderer.
///
/// - An **absolute** handle is kept literally when `allow_absolute` (the renderer
///   reading its own filesystem — local or shared FS); otherwise it is reduced to
///   its basename in the managed store, the only safe interpretation for a remote
///   caller.
/// - A **bare name** resolves into `<config_dir>/backend-files/<backend_id>/`.
///
/// Returns `None` for an empty handle, an unsafe name, or a store-relative handle
/// with no known config dir.
pub fn resolve(
    config_dir: Option<&Path>,
    backend_id: &str,
    handle: &str,
    allow_absolute: bool,
) -> Option<PathBuf> {
    let h = handle.trim();
    if h.is_empty() {
        return None;
    }
    if allow_absolute && Path::new(h).is_absolute() {
        return Some(PathBuf::from(h));
    }
    let name = sanitize_name(h)?;
    Some(store_dir(config_dir, backend_id)?.join(name))
}

/// List the managed store entries (file names) for `backend_id`, sorted. Empty if
/// the store dir does not exist or cannot be read.
pub fn list(config_dir: Option<&Path>, backend_id: &str) -> Vec<String> {
    let Some(dir) = store_dir(config_dir, backend_id) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Produce a copy of `params_by_backend` with every File-kind handle resolved to
/// an absolute renderer path (absolute kept literally; bare names into the store),
/// so backend factories read a real path. `is_file_key(backend_id, key)` reports
/// whether a key is a File param per the backend's static schema. Non-File values
/// and unresolvable handles are passed through unchanged.
pub fn resolve_file_params(
    params_by_backend: &HashMap<String, HashMap<String, ParamValue>>,
    config_dir: Option<&Path>,
    is_file_key: impl Fn(&str, &str) -> bool,
) -> HashMap<String, HashMap<String, ParamValue>> {
    params_by_backend
        .iter()
        .map(|(backend_id, params)| {
            let resolved = params
                .iter()
                .map(|(key, value)| {
                    if is_file_key(backend_id, key) {
                        if let ParamValue::Text(handle) = value {
                            // The renderer reads its own FS here, so absolute
                            // handles are allowed (local / shared-FS case).
                            if let Some(path) = resolve(config_dir, backend_id, handle, true) {
                                return (
                                    key.clone(),
                                    ParamValue::Text(path.to_string_lossy().into_owned()),
                                );
                            }
                        }
                    }
                    (key.clone(), value.clone())
                })
                .collect();
            (backend_id.clone(), resolved)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unix` as an absolute path on this platform: on Windows a path needs a
    /// drive to be absolute, and `/home/me` is not one.
    fn abs(unix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{unix}"))
        } else {
            PathBuf::from(unix)
        }
    }

    fn cfg() -> PathBuf {
        abs("/etc/omniphony")
    }

    #[test]
    fn absolute_handle_is_literal_when_allowed() {
        let handle = abs("/home/me/foo.lua");
        let p = resolve(Some(&cfg()), "script", handle.to_str().unwrap(), true).unwrap();
        assert_eq!(p, handle);
    }

    #[test]
    fn absolute_handle_falls_back_to_store_basename_for_remote() {
        // A remote peer may not address an absolute path: it is reduced to its
        // basename inside the managed store.
        let p = resolve(Some(&cfg()), "script", "/home/me/foo.lua", false).unwrap();
        assert_eq!(p, abs("/etc/omniphony/backend-files/script/foo.lua"));
    }

    #[test]
    fn bare_name_resolves_into_the_store() {
        let p = resolve(Some(&cfg()), "script", "panner.lua", true).unwrap();
        assert_eq!(p, abs("/etc/omniphony/backend-files/script/panner.lua"));
    }

    #[test]
    fn traversal_and_empty_are_rejected() {
        assert_eq!(sanitize_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_name(".."), None);
        assert_eq!(sanitize_name("   "), None);
        // A store-relative handle that sanitises to a basename can never escape
        // the store dir.
        let p = resolve(Some(&cfg()), "script", "../secret.lua", false).unwrap();
        assert_eq!(p, abs("/etc/omniphony/backend-files/script/secret.lua"));
        assert!(resolve(Some(&cfg()), "script", "   ", true).is_none());
    }

    #[test]
    fn store_relative_needs_a_config_dir() {
        assert!(resolve(None, "script", "panner.lua", true).is_none());
        // ...but an absolute handle still resolves without one.
        let handle = abs("/abs/panner.lua");
        assert!(resolve(None, "script", handle.to_str().unwrap(), true).is_some());
    }

    #[test]
    fn resolve_file_params_only_substitutes_file_keys() {
        let mut params = HashMap::new();
        let mut script = HashMap::new();
        script.insert("path".to_string(), ParamValue::Text("panner.lua".into()));
        script.insert("falloff".to_string(), ParamValue::Float(0.1));
        params.insert("script".to_string(), script);

        let out = resolve_file_params(&params, Some(&cfg()), |backend, key| {
            backend == "script" && key == "path"
        });
        let script = &out["script"];
        let expected = cfg()
            .join("backend-files")
            .join("script")
            .join("panner.lua");
        assert_eq!(script["path"].as_str(), expected.to_str());
        // Non-file params are untouched.
        assert_eq!(script["falloff"].as_f32(), Some(0.1));
    }
}
