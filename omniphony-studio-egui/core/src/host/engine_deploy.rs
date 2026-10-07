//! Deploy the engine library (liborender) a shipped Studio carries to the
//! per-user location external consumers search at runtime.
//!
//! A release archive or installer of the native Studio carries the engine
//! library built from the same commit as its `orender`, under `engine/` in
//! its resource directory (see [`crate::host::bundle`]). On startup it is
//! copied to `<local data>/omniphony/lib/`, the path the forked mpv's loader
//! probes after `--ad-orender-library` / `$ORENDER_LIBRARY` and before its own
//! bundled fallback (`common/orender_dl.c` in mpv-omniphony), so updating
//! Studio updates the engine every consumer uses, with no mpv rebuild:
//!
//! - Linux: `$XDG_DATA_HOME/omniphony/lib/` (default `~/.local/share`)
//! - macOS: `~/Library/Application Support/omniphony/lib/`
//! - Windows: `%LOCALAPPDATA%\omniphony\lib\`
//!
//! Ported from the Tauri host's `engine_deploy.rs` (#677). A system package
//! (AUR) ships no `engine/` directory: its engine library is the `orender`
//! package's, found by the loader on the system path, and nothing is copied.
//!
//! Deployment is best-effort and must never block startup: the copy only
//! happens when the bytes differ (upgrade), goes through a temp file + rename
//! so a consumer never sees a half-written library, and failures (e.g. the
//! DLL locked by a running mpv on Windows) are logged and retried on the next
//! launch.

use std::fs;
use std::path::{Path, PathBuf};

/// Copy every file of `<resource_dir>/engine/` to the per-user library
/// directory. Nothing to do for a checkout build (no resource directory) or a
/// system package (no `engine/`).
pub fn deploy(resource_dir: Option<&Path>) {
    let Some(src_dir) = resource_dir.map(|dir| dir.join("engine")) else {
        log::debug!("engine deploy: no resource directory; skipping");
        return;
    };
    if !src_dir.is_dir() {
        log::debug!("engine deploy: no bundled engine at {src_dir:?}; skipping");
        return;
    }
    let Some(dest_dir) = user_lib_dir() else {
        log::warn!("engine deploy: no local data directory; skipping");
        return;
    };
    deploy_dir(&src_dir, &dest_dir);
}

/// `<local data>/omniphony/lib`, resolved the way mpv's loader resolves it.
fn user_lib_dir() -> Option<PathBuf> {
    local_data_dir().map(|dir| dir.join("omniphony").join("lib"))
}

fn local_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home.map(|home| home.join("Library/Application Support"))
    } else {
        // The loader takes any non-empty XDG_DATA_HOME.
        std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".local/share")))
    }
}

fn deploy_dir(src_dir: &Path, dest_dir: &Path) {
    let entries = match fs::read_dir(src_dir) {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!("engine deploy: cannot read {src_dir:?}: {e}");
            return;
        }
    };
    for entry in entries.flatten() {
        let src = entry.path();
        if !src.is_file() {
            continue;
        }
        let Some(name) = src.file_name() else {
            continue;
        };
        if let Err(e) = deploy_one(&src, &dest_dir.join(name)) {
            log::warn!("engine deploy: {src:?} -> {dest_dir:?}: {e}");
        }
    }
}

fn deploy_one(src: &Path, dest: &Path) -> std::io::Result<()> {
    let src_bytes = fs::read(src)?;
    // Copy only when the content actually changed, so an unchanged engine
    // never touches the file a consumer may have open.
    if fs::read(dest).is_ok_and(|dest_bytes| dest_bytes == src_bytes) {
        log::info!("engine deploy: {dest:?} up to date");
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // Temp file + rename: atomic on the same filesystem, so a consumer either
    // sees the old library or the new one, never a torn write.
    let tmp = dest.with_extension("tmp-deploy");
    fs::write(&tmp, &src_bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755));
    }
    match fs::rename(&tmp, dest) {
        Ok(()) => {
            log::info!("engine deploy: installed {dest:?}");
            Ok(())
        }
        Err(e) => {
            // Windows: rename-over fails while the DLL is loaded by a running
            // mpv. Leave things as they were; the next launch retries.
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "studio-engine-deploy-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_the_engine_and_creates_the_directory() {
        let root = tmp("copy");
        let src = root.join("engine");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("liborender.so.1"), b"v1").unwrap();
        let dest = root.join("share/omniphony/lib");

        deploy_dir(&src, &dest);

        assert_eq!(fs::read(dest.join("liborender.so.1")).unwrap(), b"v1");
        assert!(!dest.join("liborender.tmp-deploy").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn replaces_an_older_engine_and_leaves_an_identical_one_alone() {
        let root = tmp("upgrade");
        let src = root.join("engine");
        let dest = root.join("lib");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();
        fs::write(src.join("orender.dll"), b"new").unwrap();
        fs::write(dest.join("orender.dll"), b"old").unwrap();

        deploy_dir(&src, &dest);
        assert_eq!(fs::read(dest.join("orender.dll")).unwrap(), b"new");

        // Up to date: the file is not rewritten (its mtime stays put).
        let before = fs::metadata(dest.join("orender.dll"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        deploy_dir(&src, &dest);
        let after = fs::metadata(dest.join("orender.dll"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_build_without_a_bundled_engine_deploys_nothing() {
        let root = tmp("none");
        // A resource directory without `engine/` (a system package) and no
        // resource directory at all (a checkout build) both return quietly.
        deploy(Some(&root));
        deploy(None);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
