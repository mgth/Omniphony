//! Deploy the bundled engine library (liborender) to the per-user location
//! external consumers search at runtime.
//!
//! The Studio bundle carries the engine library built from the same commit as
//! the `orender` sidecar (see `scripts/prepare-sidecar.mjs`). On startup we
//! copy it to `<local data dir>/omniphony/lib/` — the exact path the forked
//! mpv's runtime loader probes after `--ad-orender-library`/`$ORENDER_LIBRARY`
//! and before its own bundled fallback — so updating Studio updates the engine
//! every consumer uses, with no mpv rebuild.
//!
//! Tauri's `local_data_dir()` resolves to `~/.local/share` (Linux),
//! `~/Library/Application Support` (macOS) and `%LOCALAPPDATA%` (Windows),
//! matching the loader's search list on all three.
//!
//! Deployment is best-effort and must never block startup: the copy only
//! happens when the bytes differ (upgrade), goes through a temp file + rename
//! so a consumer never sees a half-written library, and failures (e.g. the
//! DLL locked by a running mpv on Windows) are logged and retried on the next
//! launch.

use std::fs;
use std::path::Path;

use tauri::Manager;

pub(crate) fn deploy(app: &tauri::App) {
    let Ok(resource_dir) = app.path().resource_dir() else {
        log::warn!("engine deploy: no resource dir; skipping");
        return;
    };
    let Ok(local_data) = app.path().local_data_dir() else {
        log::warn!("engine deploy: no local data dir; skipping");
        return;
    };

    let src_dir = resource_dir.join("engine");
    if !src_dir.is_dir() {
        // Dev runs (`tauri dev`) have no bundled resources — normal.
        log::info!("engine deploy: no bundled engine at {src_dir:?}; skipping");
        return;
    }
    let dest_dir = local_data.join("omniphony").join("lib");

    let entries = match fs::read_dir(&src_dir) {
        Ok(e) => e,
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
    if let Ok(dest_bytes) = fs::read(dest) {
        if dest_bytes == src_bytes {
            log::info!("engine deploy: {dest:?} up to date");
            return Ok(());
        }
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
