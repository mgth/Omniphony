use abi_stable::library::RootModule;
use anyhow::{Context, Result, bail};
use bridge_api::{BridgeLibRef, FormatBridgeBox};
use std::path::{Path, PathBuf};

/// Loaded bridge library + live bridge instance.
///
/// Both fields must be kept alive together: `lib` holds the reference-count
/// that prevents the `.so` from being unloaded while `bridge` is in use.
pub struct LoadedBridge {
    /// Keeps the `.so` resident in memory.
    pub lib: BridgeLibRef,
    /// The live bridge instance (stateful, called per chunk).
    pub bridge: FormatBridgeBox,
}

impl LoadedBridge {
    /// Load a bridge plugin from `path` and create one instance with the given strict-mode flag.
    ///
    /// Format-specific options (e.g. presentation index) are applied afterwards via
    /// [`FormatBridgeBox::configure`] before the first [`FormatBridgeBox::push_packet`].
    pub fn load_with_params(path: &Path, strict: bool) -> Result<Self> {
        let lib = BridgeLibRef::load_from_file(path)
            .with_context(|| format!("Failed to load bridge plugin from {}", path.display()))?;
        let new_bridge = lib
            .new_bridge()
            .context("Bridge plugin is missing the `new_bridge` export")?;
        let bridge = new_bridge(strict);
        Ok(Self { lib, bridge })
    }
}

/// Resolve the path to the bridge plugin.
///
/// Search order:
/// 1. `--bridge-path` / config-provided explicit file path
/// 2. Any file matching `*_bridge.so` / `.dll` / `.dylib` next to the executable
pub fn resolve_bridge_path(explicit: Option<&Path>) -> Result<PathBuf> {
    // 1. Explicit path from CLI/config
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        bail!(
            "Bridge path '{}' does not exist or is not a file",
            path.display()
        );
    }

    // 2. Search next to the executable
    let exe = std::env::current_exe().context("Cannot determine executable path")?;
    let dir = exe.parent().context("Executable has no parent directory")?;
    let mut matches = find_bridge_candidates(dir)?;
    matches.sort();
    if let Some(path) = matches.into_iter().next() {
        return Ok(path);
    }

    bail!(
        "No bridge plugin found.\n\
         Searched in: {}\n\
         Expected one file matching: *_bridge.so / *_bridge.dll / *_bridge.dylib\n\
         Provide --bridge-path <FILE> or set render.bridge_path in config.",
        dir.display(),
    )
}

fn find_bridge_candidates(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read executable directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if is_bridge_filename(name) {
            out.push(path);
        }
    }
    Ok(out)
}

fn is_bridge_filename(name: &str) -> bool {
    name.ends_with("_bridge.so") || name.ends_with("_bridge.dll") || name.ends_with("_bridge.dylib")
}
