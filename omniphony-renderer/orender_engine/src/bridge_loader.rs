use crate::bridge_set::BridgeSet;
use crate::decode_step::LogLevelSync;
use abi_stable::library::{LibHeader, RootModule, lib_header_from_path};
use abi_stable::sabi_types::VersionNumber;
use abi_stable::std_types::RStr;
use anyhow::{Context, Result, bail};
use bridge_api::{
    BridgeHostLogSink, BridgeLibRef, RLogLevel, RVbapCartesianDefaults, RVbapTableMode,
};
use omniphony_osc_contract as osc_contract;
use renderer::live_params::RendererControl;
use renderer::placement::PlacementMode;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The decoder bridge plugins a host loaded, in load order: what it opens
/// bridge instances from ([`open_bridges`]) and declares source families from.
/// The libraries stay resident for the life of the process (abi_stable never
/// unloads one).
#[derive(Clone)]
pub struct BridgeLibs {
    libs: Vec<BridgeLibRef>,
}

impl BridgeLibs {
    /// One plugin.
    pub fn single(lib: BridgeLibRef) -> Self {
        Self { libs: vec![lib] }
    }

    /// Plugins already loaded, in load order.
    pub fn new(libs: Vec<BridgeLibRef>) -> Result<Self> {
        if libs.is_empty() {
            bail!("no decoder bridge loaded");
        }
        Ok(Self { libs })
    }

    pub fn iter(&self) -> impl Iterator<Item = &BridgeLibRef> {
        self.libs.iter()
    }

    pub fn len(&self) -> usize {
        self.libs.len()
    }

    /// Never true: [`new`](Self::new) refuses an empty list.
    pub fn is_empty(&self) -> bool {
        self.libs.is_empty()
    }
}

/// Loaded bridge plugins + the live set of instances the host decodes with.
pub struct LoadedBridge {
    /// The plugins, for more instances and their source families.
    pub libs: BridgeLibs,
    /// The live bridges (stateful, called per chunk), routed per stream.
    pub bridge: BridgeSet,
    /// The log level `bridge` was opened with, for the host that drives it to
    /// keep in line with its own ([`open_bridges`]).
    pub log_level: LogLevelSync,
}

impl LoadedBridge {
    /// Load a bridge plugin from `path` and create one instance.
    ///
    /// Format-specific options (e.g. presentation index) are applied afterwards via
    /// [`BridgeSet::configure`] before the first [`BridgeSet::push_packet`].
    pub fn load_with_params(path: &Path) -> Result<Self> {
        Self::open(BridgeLibs::single(load_bridge_library(path)?))
    }

    /// One instance of every plugin in `libs`, routed per stream.
    pub fn open(libs: BridgeLibs) -> Result<Self> {
        let (bridge, log_level) = open_bridges(&libs)?;
        Ok(Self {
            libs,
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
        self.bridge.configure(key, value)
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

/// One instance of every plugin in `libs`, as a [`BridgeSet`], their logs
/// routed to the host's and filtered at the host's level: how every host
/// opens its bridges, from paths ([`LoadedBridge`]) or from the plugins a
/// session already holds (the PipeWire sink's own set). Later level changes
/// reach them through the [`LogLevelSync`] returned with it, which the host
/// keeps with the set.
pub fn open_bridges(libs: &BridgeLibs) -> Result<(BridgeSet, LogLevelSync)> {
    let mut bridge = BridgeSet::open(libs)?;
    let log_level = LogLevelSync::open(live_log::current_runtime_level(), &mut bridge);
    Ok((bridge, log_level))
}

/// Ask `bridge` to format and forward only the diagnostics at `level` or
/// below, so the ones the host would drop cost it nothing. `false` from a
/// bridge that predates the `log_level` key: it keeps its own level
/// (`HARLETTY_LOG`, info by default), which is no fault worth a warning.
pub fn configure_log_level(bridge: &mut BridgeSet, level: log::LevelFilter) -> bool {
    let name = live_log::level_name(level);
    let accepted = bridge.configure("log_level", name);
    if !accepted {
        log::debug!("bridge does not take log_level {name}; it keeps its own level");
    }
    accepted
}

/// Ask `bridge` for `presentation` (before its first packet); an error naming
/// the value when the bridge refuses it. Whether that is fatal is the host's
/// call: the CLI stops, a player keeps the bridge's default.
pub fn configure_presentation(bridge: &mut BridgeSet, presentation: &str) -> Result<()> {
    if !bridge.configure("presentation", presentation) {
        bail!("Bridge rejected presentation value '{presentation}'");
    }
    Ok(())
}

/// Put the plugins' source families (`BridgeLib::source_families`) in the
/// renderer's family table, so they can be configured — and the config's
/// settings for them apply — before a stream of theirs plays. Called once,
/// after the renderer is built (seeding the config keeps the table, so the
/// order does not matter). A family two plugins declare is the first one's.
pub fn declare_source_families(libs: &BridgeLibs, control: &RendererControl) {
    let mut declared: Vec<String> = Vec::new();
    let mut live = control.live.write();
    for lib in libs.iter() {
        for family in lib.source_families()().iter() {
            if declared.iter().any(|name| name == family.name.as_str()) {
                continue;
            }
            let mode =
                PlacementMode::parse(family.default_mode.as_str()).unwrap_or(PlacementMode::Room);
            live.placement
                .declare(family.name.as_str(), family.label.as_str(), mode);
            declared.push(family.name.as_str().to_owned());
        }
    }
    log::info!("Bridge source families: {}", declared.join(", "));
}

pub fn install_bridge_host_log_sink(lib: &BridgeLibRef) {
    lib.set_host_log_sink()(forward_bridge_log_to_host as BridgeHostLogSink as usize);
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

/// A bridge that was asked for or found but did not load, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeFailure {
    pub path: PathBuf,
    pub error: String,
}

impl std::fmt::Display for BridgeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.error)
    }
}

/// The bridge files a host is to load ([`resolve_bridges`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BridgeRequest {
    /// The files to load, in load order.
    pub files: Vec<PathBuf>,
    /// Asked-for paths that resolve to no file.
    pub failures: Vec<BridgeFailure>,
    /// What the host records as asked for (the live `render.bridge_path(s)`,
    /// which a Save writes): the paths as asked, a combined harletty library
    /// replaced by its family libraries. Empty when the bridges were found
    /// by auto-discovery, which is never saved.
    pub recorded: Vec<PathBuf>,
}

/// Which bridges a host loads (`docs/multi-bridge.md`, "Loading"). Shared by
/// the CLI, the FFI/mpv host (`Engine::from_paths`) and `sync-play`, so they
/// behave identically.
///
/// 1. `explicit` (CLI `--bridge-path`, repeatable; the FFI's `bridge_path`,
///    a path list) when not empty, else `config` (`render.bridge_path(s)`):
///    every path is a strict instruction. It must name an existing file and
///    is never replaced by a discovered one; a path that does not is a
///    failure, reported while the others load. Only when none resolves is it
///    an error. A **relative** path is tried against the working directory,
///    then the host executable's directory. The one exception is the combined
///    harletty library of `bridge_api` 0.5, replaced by its family libraries
///    ([`family_libraries_beside`]).
/// 2. Nothing asked for: auto-discovery ([`discover_bridges`]), every bridge
///    of the first folder that holds a usable one. Finding none is not a
///    failure of the engine: the error then contains
///    [`osc_contract::BRIDGE_ERROR_NONE_FOUND`].
pub fn resolve_bridges(explicit: &[PathBuf], config: &[PathBuf]) -> Result<BridgeRequest> {
    resolve_bridges_with(
        explicit,
        config,
        &|key| std::env::var_os(key),
        &auto_discovery_dirs(),
        &check_bridge_header,
    )
}

fn resolve_bridges_with(
    explicit: &[PathBuf],
    config: &[PathBuf],
    env: &dyn Fn(&str) -> Option<OsString>,
    dirs: &[PathBuf],
    usable: &dyn Fn(&Path) -> Result<()>,
) -> Result<BridgeRequest> {
    let (requested, origin) = if !explicit.is_empty() {
        (explicit, "bridge path")
    } else if !config.is_empty() {
        (config, "render.bridge_path (from config)")
    } else {
        return discover_bridges(env, dirs, usable);
    };
    if requested
        .iter()
        .all(|path| is_combined_library(path) && family_libraries_beside(path).is_empty())
    {
        // Only the old library was asked for, with no family library beside
        // it: as if nothing had been, and nothing discovered is recorded.
        log::warn!(
            "{origin} names only the combined harletty bridge, which bridge_api {} replaced \
             with one library per codec family, and none is next to it; searching the \
             auto-discovery folders",
            bridge_api::VERSION
        );
        return discover_bridges(env, dirs, usable);
    }
    let mut request = BridgeRequest::default();
    let search = || discover_bridges(env, dirs, usable);
    let mut fallback = CombinedFallback::new(&search);
    for path in requested {
        if is_combined_library(path) {
            // The replacements take the old library's place in the order,
            // which is the priority between bridges, and in what a Save writes.
            match fallback.replace(path, origin) {
                Ok((files, failures)) => {
                    for file in files {
                        if !request.files.contains(&file) {
                            request.recorded.push(file.clone());
                            request.files.push(file);
                        }
                    }
                    request.failures.extend(failures);
                }
                Err(failure) => {
                    // Still asked for: kept in what a Save writes.
                    request.recorded.push(path.clone());
                    request.failures.push(failure);
                }
            }
            continue;
        }
        match resolve_requested(path) {
            Some(found) => {
                request.files.push(found);
                request.recorded.push(path.clone());
            }
            None => request.failures.push(BridgeFailure {
                // Still asked for: kept in what a Save writes.
                path: {
                    request.recorded.push(path.clone());
                    path.clone()
                },
                error: format!(
                    "{origin} '{}' does not exist or is not a file{}",
                    path.display(),
                    searched_locations_hint(path)
                ),
            }),
        }
    }
    if !request.files.is_empty() {
        return Ok(request);
    }
    bail!(
        "{}. Give an existing path to each decoder bridge (an absolute path is safest), \
         or remove the explicit path and drop *_bridge.{{so,dll,dylib}} next to the host binary.",
        request
            .failures
            .iter()
            .map(|f| f.error.clone())
            .collect::<Vec<_>>()
            .join("; ")
    )
}

/// After the host recorded the bridges it was asked for (`record_bridge_paths`):
/// when resolving replaced some (a combined harletty library by its family
/// libraries, or by auto-discovery), record what was resolved instead, as
/// unsaved state, so the next Save writes it.
pub fn record_bridge_request(control: &RendererControl, request: &BridgeRequest) {
    let asked = control.bridge_paths();
    if !asked.is_empty() && request.recorded != asked {
        control.set_bridge_paths(request.recorded.clone());
        control.mark_dirty();
    }
}

/// Publish what the host loaded and what it could not
/// (`/omniphony/state/render/bridges`).
pub fn publish_bridges(control: &RendererControl, loaded: &LoadedBridges) {
    control.set_bridges_status(loaded.status());
}

/// What a path naming the combined harletty library stands for: the family
/// libraries beside it, else what the fallback search finds (run once, at
/// the first such path; later ones add nothing it has not).
struct CombinedFallback<'a> {
    search: &'a dyn Fn() -> Result<BridgeRequest>,
    searched: Option<std::result::Result<BridgeRequest, String>>,
}

impl<'a> CombinedFallback<'a> {
    fn new(search: &'a dyn Fn() -> Result<BridgeRequest>) -> Self {
        Self {
            search,
            searched: None,
        }
    }

    /// The files `combined` stands for, with the failures found on the way;
    /// a failure for `combined` itself when there are none.
    fn replace(
        &mut self,
        combined: &Path,
        origin: &str,
    ) -> std::result::Result<(Vec<PathBuf>, Vec<BridgeFailure>), BridgeFailure> {
        let families = family_libraries_beside(combined);
        if !families.is_empty() {
            log::info!(
                "{origin} '{}' is the combined harletty bridge: loading the family \
                 libraries beside it instead ({})",
                combined.display(),
                families
                    .iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Ok((families, Vec::new()));
        }
        log::warn!(
            "{origin} '{}' is the combined harletty bridge, which bridge_api {} replaced \
             with one library per codec family; none is next to it, searching further",
            combined.display(),
            bridge_api::VERSION
        );
        let first = self.searched.is_none();
        let searched = self
            .searched
            .get_or_insert_with(|| (self.search)().map_err(|error| format!("{error:#}")));
        match searched {
            Ok(found) => Ok((
                found.files.clone(),
                if first {
                    found.failures.clone()
                } else {
                    Vec::new()
                },
            )),
            Err(error) => Err(BridgeFailure {
                path: combined.to_path_buf(),
                error: format!(
                    "the combined harletty bridge, replaced by one library per codec family; \
                     none is next to it and none was found elsewhere: {error}"
                ),
            }),
        }
    }
}

/// The combined harletty library's file name of `bridge_api` 0.5, which a
/// config may still name. A transition rule, the only place this host knows
/// a plugin's name: removed in the minor release after the one that ships
/// the family libraries (`docs/multi-bridge.md`).
fn is_combined_library(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("libharletty_bridge.so" | "harletty_bridge.dll" | "libharletty_bridge.dylib")
    )
}

/// The harletty family libraries (`harletty_*_bridge`) in the folder of
/// `combined`, sorted by name, whether `combined` itself is still there or
/// not; a relative folder is tried where a relative bridge path is.
fn family_libraries_beside(combined: &Path) -> Vec<PathBuf> {
    let parent = combined.parent().unwrap_or(Path::new(""));
    let dirs: Vec<PathBuf> = if parent.is_absolute() {
        vec![parent.to_path_buf()]
    } else {
        requested_search_bases()
            .into_iter()
            .map(|base| base.join(parent))
            .collect()
    };
    for dir in dirs {
        let Ok(mut found) = find_bridge_candidates(&dir) else {
            continue;
        };
        found.retain(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    (name.starts_with("libharletty_") || name.starts_with("harletty_"))
                        && !is_combined_library(path)
                })
        });
        if !found.is_empty() {
            found.sort();
            return found;
        }
    }
    Vec::new()
}

/// Load every file of `request`, in order. A file that does not load is a
/// failure, reported with the request's own; an error only when none loads.
pub fn load_bridges(request: &BridgeRequest) -> Result<LoadedBridges> {
    let mut libs = Vec::with_capacity(request.files.len());
    let mut loaded = Vec::with_capacity(request.files.len());
    let mut failures = request.failures.clone();
    for path in &request.files {
        match load_bridge_library(path) {
            Ok(lib) => {
                libs.push(lib);
                loaded.push(path.clone());
            }
            Err(error) => failures.push(BridgeFailure {
                path: path.clone(),
                error: format!("{error:#}"),
            }),
        }
    }
    if libs.is_empty() {
        bail!(
            "no decoder bridge loaded: {}",
            failures
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    for failure in &failures {
        log::warn!("decoder bridge skipped: {failure}");
    }
    Ok(LoadedBridges {
        libs: BridgeLibs::new(libs)?,
        loaded,
        failures,
    })
}

/// The most bytes of one failure's error in the published bridges state.
pub const BRIDGE_STATUS_ERROR_MAX_BYTES: usize = 512;
/// The most failures the published bridges state lists.
pub const BRIDGE_STATUS_MAX_FAILURES: usize = 16;

/// What [`load_bridges`] loaded, and what it could not.
pub struct LoadedBridges {
    pub libs: BridgeLibs,
    /// The file of each library, in `libs` order.
    pub loaded: Vec<PathBuf>,
    pub failures: Vec<BridgeFailure>,
}

impl LoadedBridges {
    /// What the host publishes about its bridges
    /// (`/omniphony/state/render/bridges`): each one loaded, with the
    /// families it declares, then each one that failed, with why. Bounded to
    /// fit one datagram with the rest of the state: each error is summarised
    /// ([`BRIDGE_STATUS_ERROR_MAX_BYTES`]; abi_stable's full layout report
    /// runs to tens of kilobytes and stays in the log) and at most
    /// [`BRIDGE_STATUS_MAX_FAILURES`] failures are listed, the last entry
    /// counting the rest.
    pub fn status(&self) -> Vec<renderer::live_params::BridgeStatus> {
        let mut status: Vec<_> = self
            .libs
            .iter()
            .zip(&self.loaded)
            .map(|(lib, path)| renderer::live_params::BridgeStatus {
                path: path.display().to_string(),
                families: lib.source_families()()
                    .iter()
                    .map(|family| family.name.as_str().to_owned())
                    .collect(),
                error: None,
            })
            .collect();
        let shown = self.failures.len().min(BRIDGE_STATUS_MAX_FAILURES);
        status.extend(self.failures[..shown].iter().map(|failure| {
            renderer::live_params::BridgeStatus {
                path: failure.path.display().to_string(),
                families: Vec::new(),
                error: Some(crate::degraded::summarize_bridge_error_within(
                    &failure.error,
                    BRIDGE_STATUS_ERROR_MAX_BYTES,
                )),
            }
        }));
        let hidden = self.failures.len() - shown;
        if hidden > 0
            && let Some(last) = status.last_mut()
        {
            let error = last.error.get_or_insert_with(String::new);
            error.push_str(&format!(
                "\n({hidden} more bridges failed to load; see the renderer log)"
            ));
        }
        status
    }
}

/// The version and layout check a bridge passes before it loads, read from
/// its header alone: what makes a discovered candidate usable.
fn check_bridge_header(path: &Path) -> Result<()> {
    let header = lib_header_from_path(path)
        .with_context(|| format!("Failed to load bridge plugin from {}", path.display()))?;
    check_bridge_api_version(path, header)?;
    header
        .ensure_layout::<BridgeLibRef>()
        .with_context(|| format!("Failed to load bridge plugin from {}", path.display()))
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

/// The runtime variable naming the exact bridge files for auto-discovery: a
/// path list in the platform's syntax (`:` on Unix, `;` on Windows).
pub const BRIDGE_FILE_ENV: &str = "ORENDER_BRIDGE_FILE";

/// Auto-discovery: the files `$ORENDER_BRIDGE_FILE` names, when any is one,
/// else every `*_bridge.*` of the first of `dirs` ([`auto_discovery_dirs`]
/// outside tests) that holds a usable one (`usable`: the header check).
///
/// The variable is how a host that knows *which* bridges it wants hands them
/// over without making them requested paths: Studio passes the bridge
/// mpv-omniphony is configured with (`ad-orender-bridge-path` in
/// `mpv.conf`) to the renderer it spawns. It sits at auto-discovery's level,
/// below `--bridge-path` and `render.bridge_path(s)`, and is checked before
/// the folders because it names files. Files it names that do not exist are
/// logged and skipped; when none does, the folders are searched.
///
/// Folders are not merged: a stale per-user bridge must not be added to the
/// system ones. A folder that only holds refused candidates (a leftover
/// bridge of another `bridge_api` minor next to the executable) does not stop
/// the search; its refusals are reported with what is found later.
fn discover_bridges(
    env: &dyn Fn(&str) -> Option<OsString>,
    dirs: &[PathBuf],
    usable: &dyn Fn(&Path) -> Result<()>,
) -> Result<BridgeRequest> {
    if let Some(value) = env(BRIDGE_FILE_ENV).filter(|value| !value.is_empty()) {
        let mut found = BridgeRequest::default();
        let search = || find_bridges_in_dirs(dirs, usable);
        let mut fallback = CombinedFallback::new(&search);
        for path in std::env::split_paths(&value).filter(|path| !path.as_os_str().is_empty()) {
            if is_combined_library(&path) {
                // As for a requested path: the family libraries beside it,
                // else the folders' (never the old file again: a folder of
                // refused bridges does not stop that search).
                match fallback.replace(&path, &format!("${BRIDGE_FILE_ENV}")) {
                    Ok((files, failures)) => {
                        for file in files {
                            if !found.files.contains(&file) {
                                found.files.push(file);
                            }
                        }
                        found.failures.extend(failures);
                    }
                    Err(failure) => found.failures.push(failure),
                }
            } else if path.is_file() {
                if !found.files.contains(&path) {
                    found.files.push(path);
                }
            } else {
                log::warn!(
                    "${BRIDGE_FILE_ENV} names '{}', which is not a file",
                    path.display()
                );
            }
        }
        if !found.files.is_empty() {
            return Ok(found);
        }
        log::warn!("${BRIDGE_FILE_ENV} names no usable file; searching the auto-discovery folders");
    }
    find_bridges_in_dirs(dirs, usable)
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

/// Every `*_bridge.*` of the first of `dirs` holding a usable one, sorted
/// by name: the refused ones of that folder go along to be reported when
/// loading fails them. Split out from [`discover_bridges`] so the rules are
/// testable without touching the process environment, the test binary's own
/// directory or real plugins.
fn find_bridges_in_dirs(
    dirs: &[PathBuf],
    usable: &dyn Fn(&Path) -> Result<()>,
) -> Result<BridgeRequest> {
    let mut refused: Vec<BridgeFailure> = Vec::new();
    for dir in dirs {
        // A missing or unreadable directory is not an error here: the list is
        // speculative by nature (the system dir is absent on a portable install,
        // and vice versa). Only an empty *search* is worth reporting.
        let Ok(mut candidates) = find_bridge_candidates(dir) else {
            continue;
        };
        candidates.sort();
        let mut here = Vec::new();
        for path in &candidates {
            if let Err(error) = usable(path) {
                here.push(BridgeFailure {
                    path: path.clone(),
                    error: format!("{error:#}"),
                });
            }
        }
        if here.len() < candidates.len() {
            let files = candidates
                .into_iter()
                .filter(|path| !here.iter().any(|failure| &failure.path == path))
                .collect();
            refused.extend(here);
            return Ok(BridgeRequest {
                files,
                failures: refused,
                recorded: Vec::new(),
            });
        }
        refused.extend(here);
    }
    let searched = dirs
        .iter()
        .map(|d| d.display().to_string())
        .collect::<Vec<_>>()
        .join("\n  ");
    if !refused.is_empty() {
        bail!(
            "no usable decoder bridge in the auto-discovery folders: {}",
            refused
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    bail!(
        "{}: none requested (no explicit path, no render.bridge_path) and none \
         in the auto-discovery directories.\n\
         Searched in:\n  {searched}\n\
         Expected files matching: *_bridge.so / *_bridge.dll / *_bridge.dylib",
        osc_contract::BRIDGE_ERROR_NONE_FOUND
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

    /// Every candidate passes the header check: these tests use stand-in
    /// files, not plugins.
    fn usable(_: &Path) -> Result<()> {
        Ok(())
    }

    /// The first file a request asked for one bridge resolves to.
    fn resolve_bridge(explicit: Option<&Path>, config: Option<&Path>) -> Result<PathBuf> {
        let explicit: Vec<PathBuf> = explicit.into_iter().map(Path::to_path_buf).collect();
        let config: Vec<PathBuf> = config.into_iter().map(Path::to_path_buf).collect();
        resolve_bridges(&explicit, &config).map(|request| request.files[0].clone())
    }

    fn find_bridge_in_dirs(dirs: &[PathBuf]) -> Result<PathBuf> {
        find_bridges_in_dirs(dirs, &usable).map(|request| request.files[0].clone())
    }

    fn auto_discover(env: &dyn Fn(&str) -> Option<OsString>, dirs: &[PathBuf]) -> Result<PathBuf> {
        discover(env, dirs).map(|request| request.files[0].clone())
    }

    fn discover(env: &dyn Fn(&str) -> Option<OsString>, dirs: &[PathBuf]) -> Result<BridgeRequest> {
        discover_bridges(env, dirs, &usable)
    }

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
        assert!(
            err.contains(osc_contract::BRIDGE_ERROR_NONE_FOUND),
            "unexpected: {err}"
        );
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

    /// Every bridge of the first folder holding one is loaded, in name order;
    /// a later folder's are not added.
    #[test]
    fn discovery_takes_every_bridge_of_the_first_folder_only() {
        let near = dir_with_bridge("allnear", "libb_bridge.so");
        fs::write(near.join("liba_bridge.so"), b"x").unwrap();
        let later = dir_with_bridge("alllater", "libc_bridge.so");
        let found = find_bridges_in_dirs(&[near.clone(), later.clone()], &usable).unwrap();
        fs::remove_dir_all(&near).ok();
        fs::remove_dir_all(&later).ok();
        assert_eq!(
            found.files,
            [near.join("liba_bridge.so"), near.join("libb_bridge.so")]
        );
        assert!(
            found.recorded.is_empty(),
            "a discovered bridge is never saved"
        );
    }

    /// A folder that only holds refused candidates (a leftover bridge of
    /// another bridge_api minor) does not stop the search; its refusals are
    /// reported with what is found later. A refused candidate next to usable
    /// ones is reported and left out.
    #[test]
    fn a_folder_of_refused_bridges_does_not_stop_the_search() {
        let stale = dir_with_bridge("stale", "libharletty_bridge.so");
        let fresh = dir_with_bridge("fresh", "libharletty_dolby_bridge.so");
        fs::write(fresh.join("libold_bridge.so"), b"x").unwrap();
        let refuse_old = |path: &Path| -> Result<()> {
            let name = path.file_name().unwrap().to_str().unwrap();
            if name == "libharletty_bridge.so" || name == "libold_bridge.so" {
                bail!("built against bridge_api 0.5.0")
            }
            Ok(())
        };
        let found = find_bridges_in_dirs(&[stale.clone(), fresh.clone()], &refuse_old).unwrap();
        let refused_only = find_bridges_in_dirs(std::slice::from_ref(&stale), &refuse_old);
        fs::remove_dir_all(&stale).ok();
        fs::remove_dir_all(&fresh).ok();
        assert_eq!(found.files, [fresh.join("libharletty_dolby_bridge.so")]);
        let failed: Vec<&PathBuf> = found.failures.iter().map(|f| &f.path).collect();
        assert_eq!(
            failed,
            [
                &stale.join("libharletty_bridge.so"),
                &fresh.join("libold_bridge.so")
            ]
        );
        let text = format!("{:#}", refused_only.unwrap_err());
        assert!(text.contains("bridge_api 0.5.0"), "{text}");
        assert!(
            !text.contains(osc_contract::BRIDGE_ERROR_NONE_FOUND),
            "a refused bridge is a failure, not an empty search: {text}"
        );
    }

    /// The variable takes a path list; names that are no file are skipped.
    #[test]
    fn the_bridge_file_variable_takes_a_list() {
        let dir = dir_with_bridge("filelist", "liba_bridge.so");
        fs::write(dir.join("libb_bridge.so"), b"x").unwrap();
        let list = std::env::join_paths([
            dir.join("libb_bridge.so"),
            PathBuf::from("/nonexistent/libgone_bridge.so"),
            dir.join("liba_bridge.so"),
        ])
        .unwrap();
        let env = |key: &str| (key == BRIDGE_FILE_ENV).then(|| list.clone());
        let found = discover(&env, &[]).unwrap();
        fs::remove_dir_all(&dir).ok();
        assert_eq!(
            found.files,
            [dir.join("libb_bridge.so"), dir.join("liba_bridge.so")]
        );
    }

    /// Several requested bridges load in the order asked; one that is
    /// missing is a failure while the others resolve, and is kept in what a
    /// Save writes. Only when none resolves is it an error.
    #[test]
    fn requested_bridges_resolve_in_order_and_a_missing_one_is_reported() {
        let a = tmp_bridge("multi_a");
        let b = tmp_bridge("multi_b");
        let missing = PathBuf::from("/nonexistent/libmissing_bridge.so");
        let request = resolve_bridges(&[b.clone(), missing.clone(), a.clone()], &[]).unwrap();
        let none = resolve_bridges(&[], std::slice::from_ref(&missing));
        fs::remove_file(&a).ok();
        fs::remove_file(&b).ok();
        assert_eq!(request.files, [b.clone(), a.clone()]);
        assert_eq!(request.recorded, [b, missing.clone(), a]);
        assert_eq!(request.failures.len(), 1);
        assert_eq!(request.failures[0].path, missing);
        let text = format!("{:#}", none.unwrap_err());
        assert!(text.contains("libmissing_bridge.so"), "{text}");
    }

    /// A path naming the combined harletty library stands for the family
    /// libraries in its folder, whether it is still there or not, and they
    /// are what a Save records.
    #[test]
    fn the_combined_library_stands_for_the_family_libraries_beside_it() {
        let dir = dir_with_bridge("combined", "libharletty_bridge.so");
        fs::write(dir.join("libharletty_dts_bridge.so"), b"x").unwrap();
        fs::write(dir.join("libharletty_dolby_bridge.so"), b"x").unwrap();
        fs::write(dir.join("libother_bridge.so"), b"x").unwrap();
        let combined = dir.join("libharletty_bridge.so");
        let with_old = resolve_bridges(std::slice::from_ref(&combined), &[]).unwrap();
        fs::remove_file(&combined).unwrap();
        let without_old = resolve_bridges(&[], std::slice::from_ref(&combined)).unwrap();
        fs::remove_dir_all(&dir).ok();
        let families = [
            dir.join("libharletty_dolby_bridge.so"),
            dir.join("libharletty_dts_bridge.so"),
        ];
        assert_eq!(with_old.files, families);
        assert_eq!(with_old.recorded, families);
        assert_eq!(without_old.files, families);
    }

    /// With no family library beside it, the combined library's path falls
    /// back to auto-discovery; any other missing path stays an error.
    #[test]
    fn a_combined_library_without_families_falls_back_to_discovery() {
        let old = dir_with_bridge("combinedalone", "libharletty_bridge.so");
        let installed = dir_with_bridge("installed", "libharletty_iamf_bridge.so");
        let combined = old.join("libharletty_bridge.so");
        let alone = resolve_bridges_with(
            std::slice::from_ref(&combined),
            &[],
            &|_: &str| None,
            std::slice::from_ref(&installed),
            &usable,
        )
        .unwrap();
        let other = tmp_bridge("mixed_other");
        let mixed = resolve_bridges_with(
            &[other.clone(), combined.clone()],
            &[],
            &|_: &str| None,
            std::slice::from_ref(&installed),
            &usable,
        )
        .unwrap();
        let nothing = resolve_bridges_with(
            &[other.clone(), combined.clone()],
            &[],
            &|_: &str| None,
            &[PathBuf::from("/nonexistent/orender/plugins")],
            &usable,
        )
        .unwrap();
        fs::remove_dir_all(&old).ok();
        fs::remove_dir_all(&installed).ok();
        fs::remove_file(&other).ok();
        let family = installed.join("libharletty_iamf_bridge.so");
        // Alone: plain auto-discovery, nothing recorded.
        assert_eq!(alone.files, std::slice::from_ref(&family));
        assert!(alone.recorded.is_empty());
        // Next to another bridge: the discovered family joins it and is
        // what a Save writes instead of the old library.
        assert_eq!(mixed.files, [other.clone(), family.clone()]);
        assert_eq!(mixed.recorded, [other.clone(), family]);
        // Nothing discovered: the old library stays asked for, reported.
        assert_eq!(nothing.files, std::slice::from_ref(&other));
        assert_eq!(nothing.recorded, [other, combined.clone()]);
        assert_eq!(nothing.failures.len(), 1);
        assert_eq!(nothing.failures[0].path, combined);
        let missing = resolve_bridges(&[PathBuf::from("/nonexistent/liby_bridge.so")], &[]);
        assert!(missing.is_err());
    }

    /// The replacements take the old library's place in the order, which is
    /// the priority between bridges: first when it was asked for first.
    #[test]
    fn the_combined_library_s_replacements_keep_its_place() {
        let old = dir_with_bridge("combinedfirst", "libharletty_bridge.so");
        let installed = dir_with_bridge("installedfirst", "libharletty_iamf_bridge.so");
        let combined = old.join("libharletty_bridge.so");
        let other = tmp_bridge("after_combined");
        let request = resolve_bridges_with(
            &[combined, other.clone()],
            &[],
            &|_: &str| None,
            std::slice::from_ref(&installed),
            &usable,
        )
        .unwrap();
        fs::remove_dir_all(&old).ok();
        fs::remove_dir_all(&installed).ok();
        fs::remove_file(&other).ok();
        let family = installed.join("libharletty_iamf_bridge.so");
        assert_eq!(request.files, [family.clone(), other.clone()]);
        assert_eq!(request.recorded, [family, other]);
    }

    /// `$ORENDER_BRIDGE_FILE` naming the combined library (how Studio passes
    /// mpv's bridge) stands for the family libraries beside it, else for the
    /// folders' bridges, as a requested path does.
    #[test]
    fn the_bridge_file_variable_migrates_the_combined_library() {
        let dir = dir_with_bridge("envcombined", "libharletty_bridge.so");
        fs::write(dir.join("libharletty_dolby_bridge.so"), b"x").unwrap();
        let combined = dir.join("libharletty_bridge.so").into_os_string();
        let env = |key: &str| (key == BRIDGE_FILE_ENV).then(|| combined.clone());
        let beside = discover(&env, &[]).unwrap();

        let alone = dir_with_bridge("envcombinedalone", "libharletty_bridge.so");
        let installed = dir_with_bridge("envinstalled", "libharletty_dts_bridge.so");
        let alone_combined = alone.join("libharletty_bridge.so").into_os_string();
        let alone_env = |key: &str| (key == BRIDGE_FILE_ENV).then(|| alone_combined.clone());
        // The old file's own folder comes first among the folders; refused
        // there, it does not stop the search.
        let refuse_old = |path: &Path| -> Result<()> {
            if path.file_name().unwrap() == "libharletty_bridge.so" {
                bail!("built against bridge_api 0.5.0")
            }
            Ok(())
        };
        let folders =
            discover_bridges(&alone_env, &[alone.clone(), installed.clone()], &refuse_old).unwrap();
        fs::remove_dir_all(&dir).ok();
        fs::remove_dir_all(&alone).ok();
        fs::remove_dir_all(&installed).ok();
        assert_eq!(beside.files, [dir.join("libharletty_dolby_bridge.so")]);
        assert_eq!(folders.files, [installed.join("libharletty_dts_bridge.so")]);
    }
}
