use crate::decode_step::LogLevelSync;
use abi_stable::library::{LibHeader, RootModule, lib_header_from_path};
use abi_stable::sabi_types::VersionNumber;
use abi_stable::std_types::RStr;
use anyhow::{Context, Result, bail};
use bridge_api::{
    BridgeHostLogSink, BridgeLibRef, FormatBridgeBox, RLogLevel, RVbapCartesianDefaults,
    RVbapTableMode,
};
use omniphony_osc_contract as osc_contract;
use renderer::live_params::RendererControl;
use renderer::placement::PlacementMode;
use std::ffi::OsString;
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
    /// The log level `bridge` was opened with, for the host that drives it to
    /// keep in line with its own ([`open_bridge`]).
    pub log_level: LogLevelSync,
}

impl LoadedBridge {
    /// Load a bridge plugin from `path` and create one instance.
    ///
    /// Format-specific options (e.g. presentation index) are applied afterwards via
    /// [`FormatBridgeBox::configure`] before the first [`FormatBridgeBox::push_packet`].
    pub fn load_with_params(path: &Path) -> Result<Self> {
        let lib = load_bridge_library(path)?;
        let (bridge, log_level) = open_bridge(&lib);
        Ok(Self {
            lib,
            bridge,
            log_level,
        })
    }

    /// [`load_with_params`](Self::load_with_params), then ask the bridge for
    /// `presentation`: an error when it refuses (see [`configure_presentation`]).
    pub fn load_for_presentation(path: &Path, presentation: &str) -> Result<Self> {
        let mut loaded = Self::load_with_params(path)?;
        configure_presentation(&mut loaded.bridge, presentation)?;
        Ok(loaded)
    }

    /// Set a bridge configuration option. Must be called before the first packet.
    pub fn configure(&mut self, key: &str, value: &str) -> bool {
        self.bridge.configure(key.into(), value.into())
    }

    /// Default Cartesian VBAP grid dimensions suggested by the bridge.
    pub fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
        self.bridge.vbap_cartesian_defaults()
    }

    /// Preferred VBAP table mode suggested by the bridge.
    pub fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
        self.bridge.preferred_vbap_table_mode()
    }
}

/// Open the bridge plugin at `path` and return its root module.
///
/// Not `BridgeLibRef::load_from_file`: abi_stable's `RootModule::load_from`
/// keeps the first root module it loads in a process-wide static and returns
/// it for every later path, so a second, different bridge (a config reloaded
/// with another `bridge_path`, another engine in the same player) would
/// silently be the first one again. Initialising the root module from the
/// library's own header runs the same version and layout checks without that
/// cache: each library keeps its root module in its header, so opening one
/// file twice still yields one module, and two files yield two.
pub fn load_bridge_library(path: &Path) -> Result<BridgeLibRef> {
    let header = lib_header_from_path(path)
        .with_context(|| format!("Failed to load bridge plugin from {}", path.display()))?;
    check_bridge_api_version(path, header)?;
    header
        .init_root_module::<BridgeLibRef>()
        .and_then(RootModule::initialization)
        .with_context(|| format!("Failed to load bridge plugin from {}", path.display()))
}

/// Refuse a plugin built against another `bridge_api` minor than this host,
/// with a message naming both versions (`BRIDGE_API.md`, "Versioning").
///
/// abi_stable compares layouts before versions, and its layout check refuses
/// any type whose package minor differs from the host's: left to itself, a
/// plugin of another minor fails with a layout error ("too many fields",
/// "package version") that says nothing a user can act on. So the version
/// the plugin declares in its header is read first.
fn check_bridge_api_version(path: &Path, header: &LibHeader) -> Result<()> {
    let host = host_bridge_api_version();
    let bridge = header.version_strings().parsed().with_context(|| {
        format!(
            "Bridge plugin {} has no valid bridge_api version (this host loads bridge_api {}.{}.x)",
            path.display(),
            host.major,
            host.minor
        )
    })?;
    if let Err(reason) = bridge_api_compatible(host, bridge) {
        bail!(
            "Bridge plugin {} cannot be loaded: {reason}",
            path.display()
        );
    }
    Ok(())
}

/// The `bridge_api` version this host was built against.
pub fn host_bridge_api_version() -> VersionNumber {
    BridgeLibRef::VERSION_STRINGS
        .parsed()
        .expect("bridge_api's own version parses")
}

/// The rule of `BRIDGE_API.md` ("Versioning"): a bridge loads in a host built
/// against the same `bridge_api` minor (the patch does not matter, a patch
/// release never changes the ABI). Older and newer minors are both refused:
/// abi_stable refuses them anyway, this only says why.
fn bridge_api_compatible(host: VersionNumber, bridge: VersionNumber) -> Result<(), String> {
    if host.major == bridge.major && host.minor == bridge.minor {
        return Ok(());
    }
    let (series, advice) = if (bridge.major, bridge.minor) < (host.major, host.minor) {
        (
            "older",
            "install the bridge released with this version of Omniphony, or rebuild the bridge against it",
        )
    } else {
        (
            "newer",
            "update the host (Omniphony, or the player's liborender) to the release that bridge was built for, \
             or use a bridge built against this host",
        )
    };
    Err(format!(
        "it was built against bridge_api {bridge}, {series} than the bridge_api {}.{}.x this host \
         loads; a bridge loads only in a host built against the same bridge_api minor version, \
         so {advice}",
        host.major, host.minor
    ))
}

/// One more bridge instance from an already-loaded plugin, its logs routed to
/// the host's and filtered at the host's level: how every host opens one, from
/// a path ([`LoadedBridge`]) or from the plugin a session already holds (the
/// PipeWire sink's own bridge). Later level changes reach it through the
/// [`LogLevelSync`] returned with it, which the host keeps with the bridge.
pub fn open_bridge(lib: &BridgeLibRef) -> (FormatBridgeBox, LogLevelSync) {
    install_bridge_host_log_sink(lib);
    let new_bridge = lib.new_bridge();
    // strict mode removed from the host; bridges ignore the flag. The ABI
    // parameter is kept for compatibility and always passed as `false`.
    let mut bridge = new_bridge(false);
    let log_level = LogLevelSync::open(live_log::current_runtime_level(), &mut bridge);
    (bridge, log_level)
}

/// Ask `bridge` to format and forward only the diagnostics at `level` or
/// below, so the ones the host would drop cost it nothing. `false` from a
/// bridge that predates the `log_level` key: it keeps its own level
/// (`HARLETTY_LOG`, info by default), which is no fault worth a warning.
pub fn configure_log_level(bridge: &mut FormatBridgeBox, level: log::LevelFilter) -> bool {
    let name = live_log::level_name(level);
    let accepted = bridge.configure("log_level".into(), name.into());
    if !accepted {
        log::debug!("bridge does not take log_level {name}; it keeps its own level");
    }
    accepted
}

/// Ask `bridge` for `presentation` (before its first packet); an error naming
/// the value when the bridge refuses it. Whether that is fatal is the host's
/// call: the CLI stops, a player keeps the bridge's default.
pub fn configure_presentation(bridge: &mut FormatBridgeBox, presentation: &str) -> Result<()> {
    if !bridge.configure("presentation".into(), presentation.into()) {
        bail!("Bridge rejected presentation value '{presentation}'");
    }
    Ok(())
}

/// Put the plugin's source families (`BridgeLib::source_families`) in the
/// renderer's family table, so they can be configured — and the config's
/// settings for them apply — before a stream of theirs plays. Called once
/// per loaded plugin, after the renderer is built (seeding the config keeps
/// the table, so the order does not matter).
pub fn declare_source_families(lib: &BridgeLibRef, control: &RendererControl) {
    let Some(source_families) = lib.source_families() else {
        return;
    };
    let families = source_families();
    let mut live = control.live.write();
    for family in families.iter() {
        let mode =
            PlacementMode::parse(family.default_mode.as_str()).unwrap_or(PlacementMode::Room);
        live.placement
            .declare(family.name.as_str(), family.label.as_str(), mode);
    }
    log::info!(
        "Bridge source families: {}",
        families
            .iter()
            .map(|family| family.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
}

pub fn install_bridge_host_log_sink(lib: &BridgeLibRef) {
    let Some(set_host_log_sink) = lib.set_host_log_sink() else {
        return;
    };
    set_host_log_sink(forward_bridge_log_to_host as BridgeHostLogSink as usize);
}

extern "C" fn forward_bridge_log_to_host(level: RLogLevel, target: RStr<'_>, message: RStr<'_>) {
    let level = match level {
        RLogLevel::Error => log::Level::Error,
        RLogLevel::Warn => log::Level::Warn,
        RLogLevel::Info => log::Level::Info,
        RLogLevel::Debug => log::Level::Debug,
        RLogLevel::Trace => log::Level::Trace,
    };
    live_log::emit_external_record(level, target.as_str(), message.as_str());
}

/// Resolve the path to the bridge plugin.
///
/// Search order:
/// 1. `--bridge-path` / config-provided explicit file path
/// 2. Any file matching `*_bridge.so` / `.dll` / `.dylib` in the
///    auto-discovery directories (see [`auto_discovery_dirs`]), the host
///    executable's directory first
///
/// The exe-relative fallback applies to *any* host: the `orender` CLI, but
/// also library hosts like mpv loading `liborender.dll`/`.so`. On Windows in
/// particular the typical install pattern (extract a release zip into a single
/// folder) lands `mpv.exe`, `orender.dll` and the bridge `.dll` side by side,
/// so `current_exe()` -> mpv.exe's parent dir is exactly where the bridge
/// sits. On systems where the host binary is in a system path that won't
/// contain bridge plugins (e.g. `/usr/bin/mpv` on Linux), the fallback simply
/// finds nothing and the caller gets the regular missing-bridge error.
pub fn resolve_bridge_path(explicit: Option<&Path>) -> Result<PathBuf> {
    resolve_bridge(explicit, None)
}

/// Unified, strict bridge-path resolution shared by the CLI (`resolve_bridge_path`)
/// and the FFI/mpv host (`Engine::from_paths`), so both behave identically.
///
/// Order of precedence — an *explicitly requested* path (CLI `--bridge-path`,
/// FFI param, or `render.bridge_path` in the config) is taken **as a strict
/// instruction**: it must point at an existing file (the *named* one — we never
/// substitute a different bridge), else error. We do not fall through to the
/// auto-discovery glob when a path was requested. Only when no path is requested
/// at all do we auto-discover a `*_bridge.{so,dll,dylib}` next to the host
/// executable (the "drop the bundle in one folder" install).
///
/// A requested path that is **relative** is resolved CWD-independently — against
/// the process working dir first, then the host executable's directory (see
/// `resolve_requested`) — so a bare `harletty_bridge.dll` next to `mpv.exe`
/// works regardless of which folder the host was launched from. This is the
/// common real-world footgun: a relative `render.bridge_path` / `--bridge-path`
/// that previously only resolved against the CWD.
///
/// 1. `explicit` set → must resolve to a file, else error.
/// 2. else `config` (`render.bridge_path`) set → must resolve to a file, else error.
/// 3. else → [`find_bridge_next_to_exe`]: the exact file in
///    `$ORENDER_BRIDGE_FILE` if it names one, else a scan of the host
///    executable's directory, then `$ORENDER_BRIDGE_DIR`, then the per-user
///    engine directory, then the system plugin directory
///    ([`auto_discovery_dirs`]).
///    Finding none there is not a failure of the engine: the error then
///    contains [`osc_contract::BRIDGE_ERROR_NONE_FOUND`], which a client reads
///    as "running without a decoder" rather than "a bridge failed to load".
pub fn resolve_bridge(explicit: Option<&Path>, config: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if let Some(found) = resolve_requested(path) {
            return Ok(found);
        }
        bail!(
            "bridge path '{}' does not exist or is not a file{}. \
             Give an absolute path to the decoder bridge, or drop a \
             *_bridge.{{so,dll,dylib}} next to the host binary and remove the \
             explicit path.",
            path.display(),
            searched_locations_hint(path),
        );
    }
    if let Some(path) = config {
        if let Some(found) = resolve_requested(path) {
            return Ok(found);
        }
        bail!(
            "render.bridge_path '{}' (from config) does not exist or is not a file{}. \
             Fix it to an existing file (an absolute path is safest), or remove it \
             and drop a *_bridge.{{so,dll,dylib}} next to the host binary.",
            path.display(),
            searched_locations_hint(path),
        );
    }
    discover(&|key| std::env::var_os(key), &auto_discovery_dirs())
}

/// Auto-discovery ([`auto_discover`]) over `dirs`, its failure worded as
/// "nothing found" with the contract's
/// [`osc_contract::BRIDGE_ERROR_NONE_FOUND`] marker in front.
fn discover(env: &dyn Fn(&str) -> Option<OsString>, dirs: &[PathBuf]) -> Result<PathBuf> {
    auto_discover(env, dirs).with_context(|| {
        format!(
            "{}: none requested (no explicit path, no render.bridge_path) and none \
             in the auto-discovery directories",
            osc_contract::BRIDGE_ERROR_NONE_FOUND
        )
    })
}

/// Resolve a *requested* bridge path (CLI `--bridge-path`, FFI param, or
/// `render.bridge_path`). An absolute path is taken verbatim. A **relative**
/// path is resolved deterministically and CWD-independently: tried against the
/// process working dir first (back-compat) then the host executable's directory,
/// so a bare filename like `harletty_bridge.dll` dropped next to `mpv.exe` /
/// `orender` is found no matter which folder the host was launched from. Returns
/// the first candidate that is an existing file, else `None`.
fn resolve_requested(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    requested_search_bases()
        .into_iter()
        .map(|base| base.join(path))
        .find(|cand| cand.is_file())
}

/// Base directories a *relative* requested bridge path is resolved against, in
/// priority order: the process CWD, then the host executable's directory.
fn requested_search_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            bases.push(dir.to_path_buf());
        }
    }
    bases
}

/// For error messages: list the absolute candidates a *relative* path was tried
/// at (empty for absolute paths, which are self-explanatory).
fn searched_locations_hint(path: &Path) -> String {
    if path.is_absolute() {
        return String::new();
    }
    let tried: Vec<String> = requested_search_bases()
        .iter()
        .map(|base| base.join(path).display().to_string())
        .collect();
    if tried.is_empty() {
        String::new()
    } else {
        format!(" (searched: {})", tried.join(", "))
    }
}

/// System-wide plugin directory, searched last. Distro packages install the
/// bridge to a fixed libdir that is nowhere near the host binary (`/usr/bin/mpv`
/// vs `/usr/lib/orender/`), so without this a packaged install finds nothing and
/// every user has to set `render.bridge_path` by hand. Packagers whose libdir
/// differs (lib64, multiarch) override it at build time:
///
/// ```sh
/// ORENDER_BRIDGE_DIR=/usr/lib64/orender cargo build --release
/// ```
const SYSTEM_BRIDGE_DIR: Option<&str> = option_env!("ORENDER_BRIDGE_DIR");

#[cfg(unix)]
const SYSTEM_BRIDGE_DIR_DEFAULT: Option<&str> = Some("/usr/lib/orender");
#[cfg(not(unix))]
const SYSTEM_BRIDGE_DIR_DEFAULT: Option<&str> = None;

/// Directories auto-discovery scans, in priority order:
///
/// 1. next to the host executable — the "drop the bundle in one folder" install,
///    and the dev/portable layout. Kept first so a build tree always wins over
///    anything installed elsewhere. For mpv-omniphony this is the player's own
///    folder, where its install pages put the bridge.
/// 2. `$ORENDER_BRIDGE_DIR` at runtime — lets a test or an unpackaged install
///    point somewhere else without touching the config.
/// 3. the per-user engine directory ([`user_engine_dir`]): where Studio
///    deploys the engine library and mpv-omniphony's loader looks for it
///    first, so the one place every host on the machine shares. A bridge put
///    there serves the player and Studio's standby renderer alike.
/// 4. the system plugin directory (see [`SYSTEM_BRIDGE_DIR`]): where the
///    distribution packages (the AUR's `harletty-bridge`) install it.
///
/// This mirrors the candidate chain the liborender loader already uses on the
/// host side. Resolved once when an engine starts, never on the audio path.
fn auto_discovery_dirs() -> Vec<PathBuf> {
    discovery_dirs(current_exe_dir(), &|key| std::env::var_os(key))
}

fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// The runtime variable naming one exact bridge file for auto-discovery.
pub const BRIDGE_FILE_ENV: &str = "ORENDER_BRIDGE_FILE";

/// Auto-discovery: the file `$ORENDER_BRIDGE_FILE` names, when it is one,
/// then the first `*_bridge.*` of `dirs` ([`auto_discovery_dirs`] outside tests).
///
/// The variable is how a host that knows *which* bridge it wants hands it
/// over without making it a requested path: Studio passes the bridge
/// mpv-omniphony is configured with (`ad-orender-bridge-path` in
/// `mpv.conf`) to the renderer it spawns. A folder would not do: the scan
/// takes the first bridge of a folder in name order, which need not be the
/// named one when the folder holds several. It sits at auto-discovery's
/// level, below `--bridge-path` and `render.bridge_path`, and is checked
/// before the folders because it names one file. A value naming no file is
/// logged and skipped, as a missing folder is.
fn auto_discover(env: &dyn Fn(&str) -> Option<OsString>, dirs: &[PathBuf]) -> Result<PathBuf> {
    if let Some(file) = env(BRIDGE_FILE_ENV).filter(|value| !value.is_empty()) {
        let file = PathBuf::from(file);
        if file.is_file() {
            return Ok(file);
        }
        log::warn!(
            "${BRIDGE_FILE_ENV} '{}' is not a file; searching the auto-discovery folders",
            file.display()
        );
    }
    find_bridge_in_dirs(dirs)
}

/// [`auto_discovery_dirs`] with the executable's directory and the
/// environment given, so the order is testable without touching either.
/// A directory listed twice is kept at its first, higher-priority place.
fn discovery_dirs(
    exe_dir: Option<PathBuf>,
    env: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::with_capacity(4);
    dirs.extend(exe_dir);
    if let Some(dir) = env("ORENDER_BRIDGE_DIR").filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    dirs.extend(user_engine_dir(env));
    if let Some(dir) = SYSTEM_BRIDGE_DIR.or(SYSTEM_BRIDGE_DIR_DEFAULT) {
        dirs.push(PathBuf::from(dir));
    }
    let mut unique: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    for dir in dirs {
        if !unique.contains(&dir) {
            unique.push(dir);
        }
    }
    unique
}

/// `<local data>/omniphony/lib`, resolved exactly as mpv-omniphony's loader
/// (`common/orender_dl.c`) and Studio's engine deploy resolve it:
///
/// - Linux and other Unix: `$XDG_DATA_HOME/omniphony/lib` (any non-empty
///   value), else `~/.local/share/omniphony/lib`
/// - macOS: `~/Library/Application Support/omniphony/lib`
/// - Windows: `%LOCALAPPDATA%\omniphony\lib`
///
/// `None` when the variable it rests on is unset.
fn user_engine_dir(env: &dyn Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |key: &str| {
        env(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let data = if cfg!(target_os = "windows") {
        set("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        set("HOME").map(|home| home.join("Library").join("Application Support"))
    } else {
        set("XDG_DATA_HOME").or_else(|| set("HOME").map(|home| home.join(".local").join("share")))
    }?;
    Some(data.join("omniphony").join("lib"))
}

/// Look for a bridge the way auto-discovery does ([`auto_discover`]): the
/// file `$ORENDER_BRIDGE_FILE` names, else a `*_bridge.{so,dll,dylib}` in the
/// auto-discovery directories.
/// [`resolve_bridge`] (the CLI's and [`crate::engine::Engine::from_paths`]'s
/// resolution) falls back to it only when no path was requested at all: a
/// requested path that does not resolve to a file is an error, never a cue
/// to load some other bridge.
pub fn find_bridge_next_to_exe() -> Result<PathBuf> {
    auto_discover(&|key| std::env::var_os(key), &auto_discovery_dirs())
}

/// First `*_bridge.*` found scanning `dirs` in order. Split out from
/// [`find_bridge_next_to_exe`] so the priority rules are testable without
/// touching the process environment or the test binary's own directory.
fn find_bridge_in_dirs(dirs: &[PathBuf]) -> Result<PathBuf> {
    for dir in dirs {
        // A missing or unreadable directory is not an error here: the list is
        // speculative by nature (the system dir is absent on a portable install,
        // and vice versa). Only an empty *search* is worth reporting.
        let Ok(mut matches) = find_bridge_candidates(dir) else {
            continue;
        };
        matches.sort();
        if let Some(found) = matches.into_iter().next() {
            return Ok(found);
        }
    }
    let searched = dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join("\n  ");
    bail!(
        "No bridge plugin found.\n\
         Searched in:\n  {searched}\n\
         Expected one file matching: *_bridge.so / *_bridge.dll / *_bridge.dylib"
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    // Unique temp file per call so parallel tests don't collide.
    fn tmp_bridge(stem: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "orender_{}_{}_{}_bridge.so",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
            stem
        ));
        fs::write(&p, b"x").unwrap();
        p
    }

    fn version(major: u32, minor: u32, patch: u32) -> VersionNumber {
        VersionNumber {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn a_bridge_of_the_same_minor_loads_whatever_its_patch() {
        let host = version(0, 5, 2);
        assert!(bridge_api_compatible(host, version(0, 5, 0)).is_ok());
        assert!(bridge_api_compatible(host, version(0, 5, 7)).is_ok());
    }

    #[test]
    fn an_older_minor_is_refused_naming_both_versions() {
        let err = bridge_api_compatible(version(0, 5, 0), version(0, 4, 0)).unwrap_err();
        assert!(err.contains("bridge_api 0.4.0"), "{err}");
        assert!(err.contains("0.5.x"), "{err}");
        assert!(err.contains("older"), "{err}");
        assert!(err.contains("rebuild the bridge"), "{err}");
    }

    #[test]
    fn a_newer_minor_or_another_major_is_refused() {
        let err = bridge_api_compatible(version(0, 5, 0), version(0, 6, 0)).unwrap_err();
        assert!(
            err.contains("bridge_api 0.6.0") && err.contains("newer"),
            "{err}"
        );
        assert!(bridge_api_compatible(version(1, 0, 0), version(0, 5, 0)).is_err());
        assert!(bridge_api_compatible(version(0, 5, 0), version(1, 5, 0)).is_err());
    }

    /// A file that is not an abi_stable plugin fails on its header, before
    /// anything of it is called, and the error names the file.
    #[test]
    fn a_file_that_is_no_plugin_is_refused_with_its_path() {
        let f = tmp_bridge("notaplugin");
        let err = LoadedBridge::load_with_params(&f).err().expect("refused");
        fs::remove_file(&f).ok();
        assert!(
            format!("{err:#}").contains(&f.display().to_string()),
            "{err:#}"
        );
    }

    #[test]
    fn explicit_existing_is_used() {
        let f = tmp_bridge("expl");
        assert_eq!(resolve_bridge(Some(&f), None).unwrap(), f);
        fs::remove_file(&f).ok();
    }

    #[test]
    fn explicit_missing_errors() {
        let missing = Path::new("definitely_nonexistent_bridge.so");
        assert!(resolve_bridge(Some(missing), None).is_err());
    }

    #[test]
    fn config_used_when_no_explicit() {
        let f = tmp_bridge("cfg");
        assert_eq!(resolve_bridge(None, Some(&f)).unwrap(), f);
        fs::remove_file(&f).ok();
    }

    #[test]
    fn config_missing_errors() {
        let missing = Path::new("nonexistent_cfg_bridge.so");
        assert!(resolve_bridge(None, Some(missing)).is_err());
    }

    #[test]
    fn explicit_wins_over_config() {
        let expl = tmp_bridge("prec_expl");
        let cfg = tmp_bridge("prec_cfg");
        assert_eq!(resolve_bridge(Some(&expl), Some(&cfg)).unwrap(), expl);
        fs::remove_file(&expl).ok();
        fs::remove_file(&cfg).ok();
    }

    // Strict: a requested-but-missing explicit path is an error, NOT a cue to
    // silently fall back to a valid config path.
    #[test]
    fn missing_explicit_does_not_fall_through_to_config() {
        let cfg = tmp_bridge("ft_cfg");
        let missing = Path::new("missing_explicit_bridge.so");
        assert!(resolve_bridge(Some(missing), Some(&cfg)).is_err());
        fs::remove_file(&cfg).ok();
    }

    // A *relative* requested name (the real-world footgun: `harletty_bridge.dll`)
    // must resolve against the host executable's directory, not just the CWD —
    // so it's found wherever the host was launched from. We drop a uniquely-named
    // file next to the test binary and request it by bare relative name.
    #[test]
    fn relative_resolves_next_to_exe() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let name = format!("orender_{}_reltest_bridge.so", std::process::id());
        let full = dir.join(&name);
        fs::write(&full, b"x").unwrap();
        let got = resolve_bridge(Some(Path::new(&name)), None);
        fs::remove_file(&full).ok();
        assert_eq!(got.unwrap(), full);
    }

    // A scratch directory holding one bridge-looking file.
    fn dir_with_bridge(tag: &str, name: &str) -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "orender_{tag}_{}_{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), b"x").unwrap();
        dir
    }

    /// The packaged layout: the plugin sits in a fixed libdir, nowhere near the
    /// host binary. Auto-discovery must still find it, otherwise every distro
    /// user has to set `render.bridge_path` by hand.
    #[test]
    fn discovery_finds_a_plugin_in_a_later_directory() {
        let sys = dir_with_bridge("sysdir", "libharletty_bridge.so");
        let empty = std::env::temp_dir().join("orender_definitely_absent_dir");
        let got = find_bridge_in_dirs(&[empty, sys.clone()]);
        let found = got.unwrap();
        fs::remove_dir_all(&sys).ok();
        assert_eq!(found, sys.join("libharletty_bridge.so"));
    }

    /// Earlier directories win: a build tree or a portable bundle must never be
    /// shadowed by something installed system-wide.
    #[test]
    fn earlier_directories_win() {
        let near = dir_with_bridge("exedir", "libharletty_bridge.so");
        let sys = dir_with_bridge("sysdir", "libharletty_bridge.so");
        let got = find_bridge_in_dirs(&[near.clone(), sys.clone()]);
        let found = got.unwrap();
        fs::remove_dir_all(&near).ok();
        fs::remove_dir_all(&sys).ok();
        assert_eq!(found.parent().unwrap(), near);
    }

    /// A directory in the chain that does not exist is skipped, not fatal: the
    /// system dir is absent on a portable install and vice versa.
    #[test]
    fn missing_directories_are_skipped_and_listed() {
        let missing = PathBuf::from("/nonexistent/orender/plugins");
        let err = find_bridge_in_dirs(&[missing]).unwrap_err().to_string();
        assert!(err.contains("No bridge plugin found"), "unexpected: {err}");
        assert!(
            err.contains("/nonexistent/orender/plugins"),
            "the error must name what was searched: {err}"
        );
    }

    /// The chain itself: exe dir first, then the runtime override, then the
    /// per-user engine dir, then the system dir. Ordering is the whole
    /// contract, so it is pinned here.
    #[test]
    fn auto_discovery_chain_is_ordered() {
        let dirs = auto_discovery_dirs();
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(dirs.first(), Some(&exe_dir), "exe dir must come first");
        if let Some(sys) = SYSTEM_BRIDGE_DIR.or(SYSTEM_BRIDGE_DIR_DEFAULT) {
            assert_eq!(
                dirs.last(),
                Some(&PathBuf::from(sys)),
                "the system plugin dir must be the last resort"
            );
        }
    }

    /// The environment a test hands [`discovery_dirs`]: the variables every
    /// platform's per-user engine directory rests on, pointed at `root`.
    fn fake_env(root: &Path, override_dir: Option<&Path>) -> impl Fn(&str) -> Option<OsString> {
        let root = root.to_path_buf();
        let override_dir = override_dir.map(Path::to_path_buf);
        move |key| match key {
            "ORENDER_BRIDGE_DIR" => override_dir.clone().map(PathBuf::into_os_string),
            "HOME" => Some(root.join("home").into_os_string()),
            "XDG_DATA_HOME" => Some(root.join("data").into_os_string()),
            "LOCALAPPDATA" => Some(root.join("local").into_os_string()),
            _ => None,
        }
    }

    /// Where mpv-omniphony's loader and Studio's deploy put the engine library.
    fn expected_user_engine_dir(root: &Path) -> PathBuf {
        let data = if cfg!(target_os = "windows") {
            root.join("local")
        } else if cfg!(target_os = "macos") {
            root.join("home")
                .join("Library")
                .join("Application Support")
        } else {
            root.join("data")
        };
        data.join("omniphony").join("lib")
    }

    /// The full order with every candidate present: the host's folder, the
    /// runtime override, the per-user engine dir, the system dir.
    #[test]
    fn the_per_user_engine_dir_comes_after_the_override_and_before_the_system_dir() {
        let root = std::env::temp_dir().join(format!("orender_chain_{}", std::process::id()));
        let exe = root.join("exe");
        let over = root.join("override");
        let dirs = discovery_dirs(Some(exe.clone()), &fake_env(&root, Some(&over)));
        let mut expected = vec![exe, over, expected_user_engine_dir(&root)];
        expected.extend(
            SYSTEM_BRIDGE_DIR
                .or(SYSTEM_BRIDGE_DIR_DEFAULT)
                .map(PathBuf::from),
        );
        assert_eq!(dirs, expected);
    }

    /// Without `XDG_DATA_HOME` (or with it empty), the Linux engine dir is the
    /// XDG default under `$HOME`, as mpv's loader resolves it; with no `HOME`
    /// either there is no per-user candidate at all.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_linux_engine_dir_falls_back_to_the_xdg_default() {
        let env = |key: &str| match key {
            "HOME" => Some(OsString::from("/home/you")),
            "XDG_DATA_HOME" => Some(OsString::new()),
            _ => None,
        };
        assert_eq!(
            user_engine_dir(&env),
            Some(PathBuf::from("/home/you/.local/share/omniphony/lib"))
        );
        assert_eq!(user_engine_dir(&|_: &str| None), None);
    }

    /// A directory that appears twice (the override pointing at the host's own
    /// folder) is searched once, at its first place; an empty override is no
    /// candidate.
    #[test]
    fn a_repeated_or_empty_candidate_is_dropped() {
        let root = std::env::temp_dir().join(format!("orender_dup_{}", std::process::id()));
        let exe = root.join("exe");
        let dirs = discovery_dirs(Some(exe.clone()), &fake_env(&root, Some(&exe)));
        assert_eq!(dirs.iter().filter(|d| **d == exe).count(), 1);
        assert_eq!(dirs.first(), Some(&exe));
        let empty = |key: &str| (key == "ORENDER_BRIDGE_DIR").then(OsString::new);
        assert!(!discovery_dirs(None, &empty).contains(&PathBuf::new()));
    }

    /// The player's case: a bridge only in the per-user engine dir (put there
    /// next to the engine mpv-omniphony loads) is found by a renderer whose own
    /// folder has none, such as Studio's standby `orender`.
    #[test]
    fn a_bridge_in_the_per_user_engine_dir_is_found() {
        let root = std::env::temp_dir().join(format!("orender_user_{}", std::process::id()));
        let exe = root.join("studio");
        fs::create_dir_all(&exe).unwrap();
        let engine_dir = expected_user_engine_dir(&root);
        fs::create_dir_all(&engine_dir).unwrap();
        fs::write(engine_dir.join("libharletty_bridge.so"), b"x").unwrap();
        let dirs = discovery_dirs(Some(exe), &fake_env(&root, None));
        let found = find_bridge_in_dirs(&dirs);
        fs::remove_dir_all(&root).ok();
        assert_eq!(found.unwrap(), engine_dir.join("libharletty_bridge.so"));
    }

    /// Nothing requested and nothing found: the error carries the contract's
    /// marker, so Studio shows "no decoder" rather than a load failure.
    /// A requested path that is missing never does.
    #[test]
    fn only_an_empty_search_carries_the_none_found_marker() {
        let none = discover(
            &|_: &str| None,
            &[PathBuf::from("/nonexistent/orender/plugins")],
        );
        let text = format!("{:#}", none.unwrap_err());
        assert!(
            text.starts_with(osc_contract::BRIDGE_ERROR_NONE_FOUND),
            "{text}"
        );
        assert!(
            text.contains("/nonexistent/orender/plugins"),
            "the searched directories still follow: {text}"
        );
        let missing = Path::new("/nonexistent/requested_bridge.so");
        let requested = format!("{:#}", resolve_bridge(Some(missing), None).unwrap_err());
        assert!(
            !requested.contains(osc_contract::BRIDGE_ERROR_NONE_FOUND),
            "{requested}"
        );
        let configured = format!("{:#}", resolve_bridge(None, Some(missing)).unwrap_err());
        assert!(
            !configured.contains(osc_contract::BRIDGE_ERROR_NONE_FOUND),
            "{configured}"
        );
    }

    /// The variable names one file: with two bridges in its folder, that
    /// file is the one loaded, where a scan of the folder would take the
    /// first by name.
    #[test]
    fn the_bridge_file_variable_selects_that_exact_file() {
        let dir = dir_with_bridge("twobridges", "liba_bridge.so");
        fs::write(dir.join("libz_bridge.so"), b"x").unwrap();
        let named = dir.join("libz_bridge.so");
        let env =
            |key: &str| (key == "ORENDER_BRIDGE_FILE").then(|| named.clone().into_os_string());
        let dirs = [dir.clone()];
        let by_file = auto_discover(&env, &dirs);
        let by_dir = auto_discover(&|_: &str| None, &dirs);
        fs::remove_dir_all(&dir).ok();
        assert_eq!(by_file.unwrap(), named);
        assert_eq!(
            by_dir.unwrap(),
            dir.join("liba_bridge.so"),
            "the scan's own pick"
        );
    }

    /// The named file comes before the folders, the host's own included.
    #[test]
    fn the_bridge_file_comes_before_the_hosts_folder() {
        let exe = dir_with_bridge("exewithbridge", "libexe_bridge.so");
        let other = dir_with_bridge("namedbridge", "libnamed_bridge.so");
        let named = other.join("libnamed_bridge.so");
        let env =
            |key: &str| (key == "ORENDER_BRIDGE_FILE").then(|| named.clone().into_os_string());
        let found = auto_discover(&env, &discovery_dirs(Some(exe.clone()), &env));
        fs::remove_dir_all(&exe).ok();
        fs::remove_dir_all(&other).ok();
        assert_eq!(found.unwrap(), named);
    }

    /// A value naming no file is skipped like a missing folder: the folders
    /// are still searched, and finding nothing there is still "none found".
    #[test]
    fn a_bridge_file_variable_naming_nothing_falls_back_to_the_folders() {
        let dir = dir_with_bridge("fallback", "libfb_bridge.so");
        let gone = |key: &str| {
            (key == "ORENDER_BRIDGE_FILE").then(|| OsString::from("/nonexistent/libgone_bridge.so"))
        };
        let found = auto_discover(&gone, std::slice::from_ref(&dir));
        fs::remove_dir_all(&dir).ok();
        assert_eq!(found.unwrap(), dir.join("libfb_bridge.so"));
        let empty = [PathBuf::from("/nonexistent/orender/plugins")];
        let text = format!("{:#}", discover(&gone, &empty).unwrap_err());
        assert!(
            text.starts_with(osc_contract::BRIDGE_ERROR_NONE_FOUND),
            "{text}"
        );
    }

    /// A requested path still wins over the variable: it only takes part in
    /// auto-discovery.
    #[test]
    fn a_requested_path_wins_over_the_bridge_file_variable() {
        let requested = tmp_bridge("requested_over_file");
        // resolve_bridge reads the real environment; the requested path
        // returns before it is consulted, whatever the variable holds.
        assert_eq!(resolve_bridge(Some(&requested), None).unwrap(), requested);
        assert_eq!(resolve_bridge(None, Some(&requested)).unwrap(), requested);
        fs::remove_file(&requested).ok();
    }
}
