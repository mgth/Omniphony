//! The decoder bridge mpv-omniphony is told to use in `mpv.conf`, lent to
//! Studio's own renderer.
//!
//! Studio's standby `orender` finds a bridge only next to itself, in the
//! per-user engine folder or in the system plugin folder (the engine's
//! auto-discovery, `orender_engine::bridge_loader`). A user who installed the
//! player and its bridge in a folder of their own has the bridge nowhere the
//! engine looks, so Studio's renderer runs without a decoder while the player
//! decodes fine. When that user named the bridge in `mpv.conf`
//! (`ad-orender-bridge-path=…`), Studio reads it and hands that exact file to
//! the renderer it spawns as `$ORENDER_BRIDGE_FILE`, which the engine's
//! auto-discovery takes before any folder. A folder would not do: the engine
//! takes the first bridge of a folder by name, which need not be the one the
//! player uses when the folder holds several.
//!
//! Read-only: `mpv.conf` belongs to mpv and nothing here writes to it (or to
//! anything else); the lookup runs once per spawn, off the UI thread.
//!
//! The environment variable never overrides a bridge the engine was told
//! about: a `render.bridge_path` in the engine's config (or `--bridge-path`)
//! is used before it. When Studio's own environment already sets
//! `ORENDER_BRIDGE_FILE` or `ORENDER_BRIDGE_DIR`, the renderer inherits the
//! user's choice untouched and `mpv.conf` is not read.
//!
//! Not covered: a bridge that sits next to an mpv whose `mpv.conf` does not
//! name it. Nothing records where the player was installed.

use std::ffi::OsString;
use std::path::PathBuf;

/// The engine's auto-discovery variable naming one exact bridge file
/// (`orender_engine::bridge_loader::BRIDGE_FILE_ENV`): what Studio sets.
pub const BRIDGE_FILE_ENV: &str = "ORENDER_BRIDGE_FILE";
/// The engine's auto-discovery variable naming a folder to search. Studio
/// never sets it; when the user did, it is their choice and Studio's
/// `mpv.conf` lookup stands aside.
pub const BRIDGE_DIR_ENV: &str = "ORENDER_BRIDGE_DIR";

/// mpv-omniphony's option (`--ad-orender-bridge-path`), as spelled in a
/// config file.
const OPTION: &str = "ad-orender-bridge-path";

/// The inputs of mpv's config-directory lookup (`options/path.c`,
/// `osdep/path-unix.c`, `osdep/path-win.c`), gathered once so tests can give
/// their own.
#[derive(Clone, Debug, Default)]
pub struct MpvPaths {
    pub mpv_home: Option<OsString>,
    pub xdg_config_home: Option<OsString>,
    pub home: Option<OsString>,
    /// Windows `%APPDATA%`.
    pub appdata: Option<OsString>,
    /// Windows `%USERPROFILE%`, mpv's fallback for `~` without `HOME`.
    pub userprofile: Option<OsString>,
    /// mpv's lowest-priority, system-wide config directory: `/etc/mpv` for
    /// a distribution package (the build's `sysconfdir`). Windows derives its
    /// equivalent from the player's folder, which Studio does not know.
    pub global_dir: Option<PathBuf>,
}

impl MpvPaths {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var_os(key);
        Self {
            mpv_home: var("MPV_HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            home: var("HOME"),
            appdata: var("APPDATA"),
            userprofile: var("USERPROFILE"),
            global_dir: (!cfg!(windows)).then(|| PathBuf::from("/etc/mpv")),
        }
    }

    /// `~` as mpv expands it: `$HOME`, else `%USERPROFILE%`.
    fn tilde(&self) -> Option<PathBuf> {
        non_empty(&self.home)
            .or_else(|| non_empty(&self.userprofile))
            .map(PathBuf::from)
    }
}

fn non_empty(value: &Option<OsString>) -> Option<&OsString> {
    value.as_ref().filter(|v| !v.is_empty())
}

/// mpv's config directories, highest priority first. mpv reads the `mpv.conf`
/// of every one of them, lowest priority first, so a value in an earlier one
/// wins.
///
/// - `$MPV_HOME` replaces them all (and an empty one disables config files).
/// - Unix and macOS: `$XDG_CONFIG_HOME/mpv` (non-empty), else
///   `~/.config/mpv`; then the legacy `~/.mpv`, which takes the first place
///   instead when it exists and the XDG one does not; then the global dir.
/// - Windows: `%APPDATA%\mpv`. Its `portable_config` and the player's own
///   folder need the player's location, which Studio does not know.
pub fn config_dirs(paths: &MpvPaths) -> Vec<PathBuf> {
    if let Some(home) = &paths.mpv_home {
        return if home.is_empty() {
            Vec::new()
        } else {
            vec![PathBuf::from(home)]
        };
    }
    let mut dirs = Vec::new();
    if cfg!(windows) {
        dirs.extend(non_empty(&paths.appdata).map(|d| PathBuf::from(d).join("mpv")));
    } else {
        let home = non_empty(&paths.home).map(PathBuf::from);
        let user = non_empty(&paths.xdg_config_home)
            .map(|x| PathBuf::from(x).join("mpv"))
            .or_else(|| home.as_ref().map(|h| h.join(".config").join("mpv")));
        let legacy = home.map(|h| h.join(".mpv"));
        match (user, legacy) {
            (Some(user), Some(legacy)) if legacy.exists() && !user.exists() => dirs.push(legacy),
            (user, legacy) => dirs.extend(user.into_iter().chain(legacy)),
        }
    }
    dirs.extend(paths.global_dir.clone());
    dirs
}

/// The value one `mpv.conf` gives `ad-orender-bridge-path` in its default
/// profile: the options before any `[profile]` header, or under
/// `[default]`. The last one wins, as mpv applies them in order. Follows
/// `options/parse_configfile.c`: a leading `--` is optional, `#` starts a
/// comment, a value may be `"quoted"`, `'quoted'` or `%N%`-length quoted.
pub fn bridge_path_in(conf: &str) -> Option<String> {
    let conf = conf.strip_prefix('\u{feff}').unwrap_or(conf);
    let mut in_default = true;
    let mut found = None;
    for line in conf.lines() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            in_default = rest
                .split_once(']')
                .is_some_and(|(name, _)| name == "default");
            continue;
        }
        if !in_default {
            continue;
        }
        let line = line.strip_prefix("--").unwrap_or(line);
        let name_len = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(line.len());
        if &line[..name_len] != OPTION {
            continue;
        }
        let Some(value) = line[name_len..].trim_start().strip_prefix('=') else {
            continue;
        };
        if let Some(value) = parse_value(value.trim_start()) {
            found = Some(value);
        }
    }
    found
}

/// One option value, as mpv's config parser reads it; `None` where mpv
/// reports an error (an unterminated quote, a bad length).
fn parse_value(value: &str) -> Option<String> {
    let (value, rest) = if let Some(quote @ ('"' | '\'')) = value.chars().next() {
        let inner = &value[1..];
        let end = inner.find(quote)?;
        (&inner[..end], &inner[end + 1..])
    } else if let Some(rest) = value.strip_prefix('%') {
        let (len, rest) = rest.split_once('%')?;
        let len: usize = len.parse().ok()?;
        let value = rest.get(..len)?;
        (value, &rest[len..])
    } else {
        let end = value.find('#').unwrap_or(value.len());
        (value[..end].trim(), "")
    };
    // mpv refuses anything but a comment after the value.
    let rest = rest.trim_start();
    (rest.is_empty() || rest.starts_with('#')).then(|| value.to_owned())
}

/// The bridge file a value names. Absolute paths are taken as they are;
/// `~/…` and `~~/…` (mpv's config directory: the first config dir holding
/// the file, else the first config dir) are expanded the way mpv expands
/// user paths (`options/path.c`, `mp_get_user_path`). mpv itself hands this
/// option to liborender verbatim, without that expansion, so only an
/// absolute path works in the player too; the expansion here only reads the
/// user's evident intent. A relative path is left out: liborender inside mpv
/// resolves it against the player's working directory and folder, which
/// Studio does not know.
fn resolve(value: &str, paths: &MpvPaths, dirs: &[PathBuf]) -> Option<PathBuf> {
    if let Some(rest) = value.strip_prefix("~~/") {
        let found = dirs.iter().map(|d| d.join(rest)).find(|p| p.exists());
        return found.or_else(|| dirs.first().map(|d| d.join(rest)));
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return paths.tilde().map(|home| home.join(rest));
    }
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

/// What `mpv.conf` says about the bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MpvBridge {
    /// The `mpv.conf` the value came from.
    pub conf: PathBuf,
    /// The bridge file it names, which exists.
    pub bridge: PathBuf,
}

/// The bridge the player is configured with, if its `mpv.conf` names an
/// existing file. The highest-priority `mpv.conf` that sets the option
/// decides, as it does for mpv: a value naming nothing usable there is not
/// replaced by one from a lower-priority file.
pub fn find(paths: &MpvPaths) -> Option<MpvBridge> {
    let dirs = config_dirs(paths);
    for dir in &dirs {
        let conf = dir.join("mpv.conf");
        let Ok(text) = std::fs::read_to_string(&conf) else {
            continue;
        };
        let Some(value) = bridge_path_in(&text) else {
            continue;
        };
        let bridge = resolve(&value, paths, &dirs).filter(|p| p.is_file());
        if bridge.is_none() {
            log::info!(
                "{} sets {OPTION}={value}, which is no file Studio can find \
                 (an absolute path, ~/ or ~~/); its renderer does not use it",
                conf.display()
            );
        }
        return bridge.map(|bridge| MpvBridge { conf, bridge });
    }
    None
}

/// The `ORENDER_BRIDGE_FILE` to give the renderer Studio spawns: the bridge
/// `mpv.conf` names, unless Studio's own environment already sets
/// `ORENDER_BRIDGE_FILE` or `ORENDER_BRIDGE_DIR` (`own_file`, `own_dir`):
/// the spawned process inherits those, and the user's choice wins.
pub fn bridge_file_for_renderer(
    own_file: Option<&OsString>,
    own_dir: Option<&OsString>,
    paths: &MpvPaths,
) -> Option<PathBuf> {
    for (name, value) in [(BRIDGE_FILE_ENV, own_file), (BRIDGE_DIR_ENV, own_dir)] {
        if value.is_some_and(|v| !v.is_empty()) {
            log::info!("{name} is set in Studio's environment; mpv.conf is not read");
            return None;
        }
    }
    let found = find(paths)?;
    log::info!(
        "{} names the bridge {}: its renderer gets {BRIDGE_FILE_ENV}",
        found.conf.display(),
        found.bridge.display(),
    );
    Some(found.bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn the_default_profile_value_is_read_in_every_spelling() {
        for (conf, expected) in [
            (
                "ad-orender-bridge-path=/b/libh_bridge.so\n",
                "/b/libh_bridge.so",
            ),
            ("--ad-orender-bridge-path=/b/x_bridge.so", "/b/x_bridge.so"),
            (
                "  ad-orender-bridge-path = /b/x_bridge.so  # mine\n",
                "/b/x_bridge.so",
            ),
            (
                "ad-orender-bridge-path=\"/my dir/x_bridge.so\"",
                "/my dir/x_bridge.so",
            ),
            (
                "ad-orender-bridge-path='/a#b/x_bridge.so' # c",
                "/a#b/x_bridge.so",
            ),
            (
                "ad-orender-bridge-path=%14%/q/x_bridge.so",
                "/q/x_bridge.so",
            ),
            (
                "\u{feff}ad-orender-bridge-path=/bom_bridge.so",
                "/bom_bridge.so",
            ),
            (
                "[default]\nad-orender-bridge-path=/d_bridge.so\n",
                "/d_bridge.so",
            ),
        ] {
            assert_eq!(bridge_path_in(conf).as_deref(), Some(expected), "{conf:?}");
        }
    }

    #[test]
    fn profiles_comments_and_lookalikes_are_ignored() {
        let conf = "\
# ad-orender-bridge-path=/commented_bridge.so
ad-orender-bridge-path-extra=/other_bridge.so
ad=orender
ad-orender-bridge-path=/first_bridge.so
ad-orender-bridge-path=/second_bridge.so
[film]
ad-orender-bridge-path=/profile_bridge.so
";
        assert_eq!(bridge_path_in(conf).as_deref(), Some("/second_bridge.so"));
        assert_eq!(
            bridge_path_in("ad=orender\n[x]\nad-orender-bridge-path=/p.so"),
            None
        );
        // mpv refuses these lines; so do we.
        assert_eq!(bridge_path_in("ad-orender-bridge-path=\"/open.so"), None);
        assert_eq!(
            bridge_path_in("ad-orender-bridge-path=\"/a.so\" junk"),
            None
        );
        assert_eq!(bridge_path_in("ad-orender-bridge-path"), None);
    }

    fn unix_paths(root: &Path) -> MpvPaths {
        MpvPaths {
            home: Some(root.join("home").into_os_string()),
            appdata: Some(root.join("appdata").into_os_string()),
            global_dir: Some(root.join("etc")),
            ..MpvPaths::default()
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_lookup_order_follows_mpv() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let mut paths = unix_paths(root);
        let xdg = root.join("home/.config/mpv");
        let legacy = root.join("home/.mpv");
        let global = root.join("etc");
        // Neither user dir exists: both are listed, XDG first.
        assert_eq!(
            config_dirs(&paths),
            [xdg.clone(), legacy.clone(), global.clone()]
        );
        // Only the legacy one exists: it alone stands for the user dir.
        fs::create_dir_all(&legacy).unwrap();
        assert_eq!(config_dirs(&paths), [legacy.clone(), global.clone()]);
        // $XDG_CONFIG_HOME moves the user dir; an empty one does not count.
        paths.xdg_config_home = Some(OsString::new());
        assert_eq!(config_dirs(&paths)[0], legacy);
        // Once the XDG dir exists, it comes first and the legacy one after.
        paths.xdg_config_home = Some(root.join("xdg").into_os_string());
        fs::create_dir_all(root.join("xdg/mpv")).unwrap();
        assert_eq!(
            config_dirs(&paths),
            [root.join("xdg/mpv"), legacy.clone(), global.clone()]
        );
        // $MPV_HOME replaces everything; empty, it disables config files.
        paths.mpv_home = Some(root.join("mh").into_os_string());
        assert_eq!(config_dirs(&paths), [root.join("mh")]);
        paths.mpv_home = Some(OsString::new());
        assert!(config_dirs(&paths).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_lookup_is_appdata() {
        let root = tempfile::tempdir().unwrap();
        let paths = MpvPaths {
            global_dir: None,
            ..unix_paths(root.path())
        };
        assert_eq!(
            config_dirs(&paths),
            [root.path().join("appdata").join("mpv")]
        );
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// Where a test's user-level `mpv.conf` lives on this platform.
    fn user_dir(root: &Path) -> PathBuf {
        if cfg!(windows) {
            root.join("appdata").join("mpv")
        } else {
            root.join("home").join(".config").join("mpv")
        }
    }

    #[test]
    fn the_named_bridge_file_is_handed_over() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bridge = root.join("player").join("libharletty_bridge.so");
        write(&bridge, "x");
        write(
            &user_dir(root).join("mpv.conf"),
            &format!("ad=orender\nad-orender-bridge-path={}\n", bridge.display()),
        );
        let paths = unix_paths(root);
        let found = find(&paths).unwrap();
        assert_eq!(found.conf, user_dir(root).join("mpv.conf"));
        assert_eq!(found.bridge, bridge);
        assert_eq!(bridge_file_for_renderer(None, None, &paths), Some(bridge));
    }

    /// The review's case: the folder holds another bridge that sorts first.
    /// The file handed over is still the one mpv.conf names, not the folder.
    #[test]
    fn the_named_file_is_handed_over_when_its_folder_holds_two() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let player = root.join("player");
        write(&player.join("liba_bridge.so"), "x");
        let named = player.join("libz_bridge.so");
        write(&named, "x");
        write(
            &user_dir(root).join("mpv.conf"),
            &format!("ad-orender-bridge-path={}\n", named.display()),
        );
        assert_eq!(
            bridge_file_for_renderer(None, None, &unix_paths(root)),
            Some(named)
        );
    }

    #[test]
    fn studios_own_variable_wins_and_mpv_conf_is_not_read() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let bridge = root.join("player").join("libh_bridge.so");
        write(&bridge, "x");
        write(
            &user_dir(root).join("mpv.conf"),
            &format!("ad-orender-bridge-path={}\n", bridge.display()),
        );
        let paths = unix_paths(root);
        let own = OsString::from("/elsewhere");
        assert_eq!(bridge_file_for_renderer(Some(&own), None, &paths), None);
        assert_eq!(bridge_file_for_renderer(None, Some(&own), &paths), None);
        // An empty variable is no choice.
        let empty = OsString::new();
        assert_eq!(
            bridge_file_for_renderer(Some(&empty), Some(&empty), &paths),
            Some(bridge)
        );
    }

    #[test]
    fn nothing_usable_means_no_variable() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let paths = unix_paths(root);
        // No mpv.conf at all.
        assert_eq!(bridge_file_for_renderer(None, None, &paths), None);
        // An mpv.conf without the option.
        write(&user_dir(root).join("mpv.conf"), "ad=orender\n");
        assert_eq!(bridge_file_for_renderer(None, None, &paths), None);
        // A path to nothing, and a relative one Studio cannot resolve.
        for value in [
            root.join("missing_bridge.so").display().to_string(),
            "libh_bridge.so".to_string(),
        ] {
            write(
                &user_dir(root).join("mpv.conf"),
                &format!("ad-orender-bridge-path={value}\n"),
            );
            assert_eq!(
                bridge_file_for_renderer(None, None, &paths),
                None,
                "{value}"
            );
        }
    }

    /// The highest-priority file that sets the option decides, even when what
    /// it names is missing; a lower one is only read when it does not set it.
    #[cfg(not(windows))]
    #[test]
    fn the_user_conf_wins_over_the_global_one() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let paths = unix_paths(root);
        let global_bridge = root.join("sys").join("libg_bridge.so");
        write(&global_bridge, "x");
        write(
            &root.join("etc/mpv.conf"),
            &format!("ad-orender-bridge-path={}\n", global_bridge.display()),
        );
        assert_eq!(find(&paths).unwrap().bridge, global_bridge);
        let user_bridge = root.join("mine").join("libu_bridge.so");
        write(&user_bridge, "x");
        write(
            &user_dir(root).join("mpv.conf"),
            &format!("ad-orender-bridge-path={}\n", user_bridge.display()),
        );
        assert_eq!(find(&paths).unwrap().bridge, user_bridge);
        write(
            &user_dir(root).join("mpv.conf"),
            "ad-orender-bridge-path=/nonexistent/libu_bridge.so\n",
        );
        assert_eq!(find(&paths), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn tilde_paths_are_expanded_as_mpv_does() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let paths = unix_paths(root);
        let in_home = root.join("home/omniphony/libh_bridge.so");
        write(&in_home, "x");
        write(
            &user_dir(root).join("mpv.conf"),
            "ad-orender-bridge-path=~/omniphony/libh_bridge.so\n",
        );
        assert_eq!(find(&paths).unwrap().bridge, in_home);
        let in_config = user_dir(root).join("bridges/libh_bridge.so");
        write(&in_config, "x");
        write(
            &user_dir(root).join("mpv.conf"),
            "ad-orender-bridge-path=~~/bridges/libh_bridge.so\n",
        );
        assert_eq!(find(&paths).unwrap().bridge, in_config);
    }
}
