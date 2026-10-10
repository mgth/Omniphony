//! Where a shipped Studio finds the files it ships with.
//!
//! The release archive and the Windows installers unpack to one directory:
//! the executable, the renderer, `layouts/`, `assets/` and `engine/` side by
//! side. The deb and the AppImage (cargo-packager, `Cargo.toml`) put the
//! executables in `bin/` and the rest under `lib/omniphony-studio-egui/`, the
//! AUR package under `share/omniphony-studio-egui/`, and the macOS .app in
//! `Contents/Resources/` next to `Contents/MacOS/`. A build run from its
//! checkout has none of these, and the callers fall back to the checkout's own
//! copies.

use std::path::{Path, PathBuf};

/// The directory a package installs the Studio's files under, in `lib/` or
/// `share/` next to `bin/`.
const PACKAGE_DIR: &str = "omniphony-studio-egui";

/// The directory holding the shipped `layouts/`, if this executable ships
/// with one: its own directory (archive, Windows installer), then
/// `../lib/<name>` (deb, AppImage), `../share/<name>` (AUR) and
/// `../Resources` (macOS .app). `None` for a checkout build, whose `target/`
/// holds no layouts.
pub fn resource_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    resource_dir_for(exe.parent()?)
}

fn resource_dir_for(exe_dir: &Path) -> Option<PathBuf> {
    let installed = exe_dir.parent().into_iter().flat_map(|prefix| {
        [
            prefix.join("lib").join(PACKAGE_DIR),
            prefix.join("share").join(PACKAGE_DIR),
            prefix.join("Resources"),
        ]
    });
    std::iter::once(exe_dir.to_path_buf())
        .chain(installed)
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
        let share = root.join("share").join(PACKAGE_DIR);
        std::fs::create_dir_all(share.join("layouts")).unwrap();
        assert_eq!(resource_dir_for(&bin), Some(share));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_deb_or_appimage_keeps_its_files_under_lib() {
        let root = tmp("deb");
        let bin = root.join("usr").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let lib = root.join("usr").join("lib").join(PACKAGE_DIR);
        std::fs::create_dir_all(lib.join("layouts")).unwrap();
        assert_eq!(resource_dir_for(&bin), Some(lib));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_macos_app_keeps_its_files_in_resources() {
        let root = tmp("app");
        let contents = root.join("Omniphony Studio (native).app").join("Contents");
        let macos = contents.join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let resources = contents.join("Resources");
        std::fs::create_dir_all(resources.join("layouts")).unwrap();
        assert_eq!(resource_dir_for(&macos), Some(resources));
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
