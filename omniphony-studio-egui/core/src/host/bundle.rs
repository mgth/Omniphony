//! Where a shipped Studio finds the files it ships with.
//!
//! The release archive unpacks to one directory: the executable, the
//! renderer, `layouts/` and `assets/` side by side. A system package puts the
//! executable in `bin/` and the rest under `share/omniphony-studio-egui/`. A
//! build run from its checkout has neither, and the callers fall back to the
//! checkout's own copies.

use std::path::{Path, PathBuf};

/// The directory a system package installs the Studio's files under, next to
/// `bin/`.
const SHARE_DIR: &str = "omniphony-studio-egui";

/// The directory holding the shipped `layouts/`, if this executable ships
/// with one: its own directory (the archive), then `../share/<name>` (a
/// package). `None` for a checkout build, whose `target/` holds no layouts.
pub fn resource_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    resource_dir_for(exe.parent()?)
}

fn resource_dir_for(exe_dir: &Path) -> Option<PathBuf> {
    let share = exe_dir
        .parent()
        .map(|prefix| prefix.join("share").join(SHARE_DIR));
    std::iter::once(exe_dir.to_path_buf())
        .chain(share)
        .find(|dir| dir.join("layouts").is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("studio-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_archive_layout_is_the_executables_own_directory() {
        let root = tmp("archive");
        std::fs::create_dir_all(root.join("layouts")).unwrap();
        assert_eq!(resource_dir_for(&root), Some(root.clone()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_package_keeps_its_files_under_share() {
        let root = tmp("package");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let share = root.join("share").join(SHARE_DIR);
        std::fs::create_dir_all(share.join("layouts")).unwrap();
        assert_eq!(resource_dir_for(&bin), Some(share));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_checkout_build_ships_nothing() {
        let root = tmp("checkout");
        let target = root.join("target").join("release");
        std::fs::create_dir_all(&target).unwrap();
        assert_eq!(resource_dir_for(&target), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
