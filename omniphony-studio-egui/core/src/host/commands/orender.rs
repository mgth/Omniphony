//! orender launch + service management: resolving the launch spec (bundled vs
//! configured paths), spawning/stopping the renderer directly, and install /
//! uninstall / start / stop of the OS service (systemd user unit on Linux, SC on
//! Windows), plus a PipeWire restart helper.
//!
//! The private helpers below are platform glue used only by these commands.

use super::HostPaths;
use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::host::mpv_bridge;
use crate::osc_contract;
use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

const ORENDER_SERVICE_NAME: &str = "omniphony-renderer";

struct OrenderLaunchSpec {
    orender_path: PathBuf,
    args: Vec<String>,
}

/// Public so the UI crate can name it; read serialised, like [`AboutInfo`].
///
/// [`AboutInfo`]: super::app::AboutInfo
#[derive(serde::Serialize)]
pub struct OrenderServiceStatus {
    pub installed: bool,
    pub running: bool,
    pub manager: &'static str,
}

fn first_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.exists()).cloned()
}

fn bundled_orender_candidates(app: &HostPaths) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.resource_dir() {
        candidates.push(resource_dir.join("orender"));
        candidates.push(resource_dir.join("orender.exe"));
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            candidates.push(exe_dir.join("orender"));
            candidates.push(exe_dir.join("orender.exe"));
        }
    }
    candidates
}

/// The Omniphony checkout this Studio was built from: the nearest directory
/// above the crate that holds the renderer's `omniphony-renderer/Cargo.toml`.
///
/// Searched for rather than counted in `parent()` steps. The Tauri host sat
/// one level deeper (in its `src-tauri` directory), and the two steps copied
/// from it climbed out of the checkout to `workflows/<wf>/`, where the
/// renderer build of the checkout was never found. `None` for a binary run
/// away from its source tree, where only the configured, bundled and `PATH`
/// binaries apply.
fn repo_root() -> Option<PathBuf> {
    repo_root_above(Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn repo_root_above(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|dir| dir.join("omniphony-renderer/Cargo.toml").is_file())
        .map(Path::to_path_buf)
}

/// The checkout's own renderer builds, release first.
fn repo_orender_candidates(repo_root: Option<&Path>) -> Vec<PathBuf> {
    repo_root
        .map(|root| {
            vec![
                root.join("omniphony-renderer/target/release/orender"),
                root.join("omniphony-renderer/target/debug/orender"),
            ]
        })
        .unwrap_or_default()
}

fn default_orender_input_path() -> PathBuf {
    // An environment that carved out its own runtime namespace pins the pipe,
    // so two renderers never end up reading the same FIFO.
    if let Some(path) = crate::host::runtime_env::input_pipe() {
        return path;
    }

    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"\\.\pipe\orender.input")
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::temp_dir().join("orender.pipe")
    }
}

fn default_orender_log_path() -> PathBuf {
    // Keep the log inside the environment's own namespace too: two renderers
    // interleaving lines in one file is its own debugging trap.
    if let Some(dir) = crate::host::runtime_env::config_dir() {
        return dir.join("orender.log");
    }
    std::env::temp_dir().join("omniphony-orender.log")
}

/// Resolve the `orender` binary this Studio would launch.
///
/// Split out of the launch-spec builder, which also persists connection
/// settings as a side effect, so the answer can be reported without changing
/// anything — a client needs it to tell whether the renderer that answered on
/// the OSC port is its own or one another environment left running.
fn resolve_orender_binary(
    app: &HostPaths,
    orender_path: Option<String>,
) -> Result<PathBuf, String> {
    let repo_orender_candidates = repo_orender_candidates(repo_root().as_deref());

    if cfg!(debug_assertions) {
        first_existing_path(&repo_orender_candidates)
            .or_else(|| {
                orender_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .filter(|path| path.exists())
            })
            .or_else(|| first_existing_path(&bundled_orender_candidates(app)))
    } else {
        orender_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .or_else(|| first_existing_path(&bundled_orender_candidates(app)))
            .or_else(|| first_existing_path(&repo_orender_candidates))
    }
    .or_else(|| {
        let lookup_cmd = if cfg!(target_os = "windows") {
            "where"
        } else {
            "which"
        };
        crate::host::process::capture(
            ProcessCommand::new(lookup_cmd).arg("orender"),
            std::time::Duration::from_secs(2),
        )
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if resolved.is_empty() {
                None
            } else {
                Some(PathBuf::from(resolved))
            }
        })
    })
    .ok_or_else(|| "orender binary not found".to_string())
}

fn resolve_orender_launch_spec(
    app: &HostPaths,
    state: &SharedState,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<OrenderLaunchSpec, String> {
    let orender_path = resolve_orender_binary(app, orender_path)?;
    Ok(orender_launch_spec(
        state,
        orender_path,
        host,
        osc_rx_port,
        osc_port,
        osc_metering_enabled,
        log_level,
    ))
}

/// The launch spec for an already resolved binary, persisting the connection
/// settings it was built from.
fn orender_launch_spec(
    state: &SharedState,
    orender_path: PathBuf,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    log_level: Option<String>,
) -> OrenderLaunchSpec {
    let args = orender_render_args(
        &default_orender_input_path(),
        &host,
        osc_rx_port,
        osc_metering_enabled,
        log_level.as_deref(),
    );

    // Persist the connection settings used for this launch, preserving the
    // fields this function doesn't manage (auto-start / keep-alive toggles).
    if let Err(error) = state.config.update(|cfg| {
        cfg.host = host.trim().to_string();
        cfg.osc_rx_port = osc_rx_port;
        cfg.osc_port = osc_port;
        cfg.osc_metering_enabled = osc_metering_enabled;
    }) {
        log::warn!("[osc] {error}");
    }

    OrenderLaunchSpec { orender_path, args }
}

/// The command line after `orender` for the renderer Studio launches and for
/// the service it installs. Pure, so the unit file the packages ship can be
/// checked against it (see the tests).
fn orender_render_args(
    input_path: &Path,
    host: &str,
    osc_rx_port: u16,
    osc_metering_enabled: bool,
    log_level: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "render".to_string(),
        input_path.display().to_string(),
        "--continuous".to_string(),
        "--enable-vbap".to_string(),
        "--osc".to_string(),
        "--osc-host".to_string(),
        host.trim().to_string(),
        "--osc-port".to_string(),
        osc_rx_port.to_string(),
        "--osc-rx-port".to_string(),
        osc_rx_port.to_string(),
        // Every Studio-launched local renderer is a yieldable standby: an
        // mpv-embedded renderer must be able to take the OSC port over.
        "--osc-yield".to_string(),
    ];

    if osc_metering_enabled {
        args.push("--osc-metering".to_string());
    }

    let level = log_level
        .map(str::trim)
        .filter(|s| matches!(*s, "off" | "error" | "warn" | "info" | "debug" | "trace"))
        .unwrap_or("info");
    if level != "info" {
        args.push("--loglevel".to_string());
        args.push(level.to_string());
    }

    // No `--speaker-layout`: the renderer's config (`current_layout` of the
    // active profile) is the one source of truth for the speaker layout. The
    // Studio's selection before a renderer connects is only a display default
    // (7.1.4); forwarding it overrode the saved layout, and the live-state
    // handoff then carried that override from instance to instance.
    args
}

/// Whether the renderer this Studio launched is still running.
///
/// One lock and one `try_wait`, nothing that waits: the restart banner asks
/// this on every frame it is drawn, and a quit under way on the worker (see
/// [`quit_launched_renderer`]) must not hold the answer back.
pub fn launched_renderer_running(state: &SharedState) -> bool {
    state
        .renderer_child
        .lock()
        .unwrap()
        .as_mut()
        .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
}

/// Whether quitting Studio stops the renderer: one it launched and still
/// runs, unless the user asked to keep it alive. What the quit prompt tells
/// the user about their unsaved edits depends on it.
pub fn quitting_stops_renderer(state: &SharedState) -> bool {
    launched_renderer_running(state) && !state.config.snapshot().keep_renderer_alive_on_quit
}

/// At quit, take a renderer this Studio launched down with it, unless the
/// user asked to keep it. A renderer this Studio did not start (a service,
/// mpv's own) is left alone.
pub fn stop_launched_renderer(state: &SharedState) {
    if !quitting_stops_renderer(state) {
        return;
    }
    quit_launched_renderer(state);
}

/// Restart the renderer this Studio launched from the binary on disk, so a
/// rebuilt or updated `orender` is the one that runs. The old instance quits
/// first, handing its live state over, and the new one is launched with the
/// saved OSC settings, like the Launch button.
pub fn restart_launched_renderer(
    app: &HostPaths,
    state: &SharedState,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
) -> Result<serde_json::Value, String> {
    if !launched_renderer_running(state) {
        return Err("the running renderer was not launched by this Studio".to_string());
    }
    // Keep the watchdog from starting a standby in the gap; the launch below
    // re-arms it.
    state.watchdog.lock().unwrap().suppressed = true;
    quit_launched_renderer(state);
    launch_orender(
        app,
        state,
        host,
        osc_rx_port,
        osc_port,
        osc_metering_enabled,
        None,
        None,
    )
}

/// How long a renderer asked to quit gets to write its live-state handoff
/// and go before it is killed.
const QUIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Quit the renderer this Studio launched: a graceful quit first, so it
/// writes its live-state handoff, then a kill if it has not gone within
/// [`QUIT_GRACE`].
///
/// The child's lock is taken for one `try_wait` at a time and released while
/// this sleeps. Holding it across the grace period stalled everyone asking
/// whether the renderer still runs, the restart banner first: its frame waited
/// up to the full two seconds on a renderer slow to answer.
fn quit_launched_renderer(state: &SharedState) {
    if state.renderer_child.lock().unwrap().is_none() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_QUIT.to_string(),
        },
    );
    let deadline = std::time::Instant::now() + QUIT_GRACE;
    loop {
        let mut guard = state.renderer_child.lock().unwrap();
        // Reaped by the watchdog in the meantime: it has gone.
        let Some(child) = guard.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if std::time::Instant::now() < deadline => {
                drop(guard);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Path of the `orender` binary this Studio would launch, so the UI can compare
/// it with the path the connected renderer reports over OSC (see
/// [`renderer_mismatch`]). Returns `None` when no binary can be resolved at
/// all; that is a separate, already-reported condition.
pub fn expected_orender_path(app: &HostPaths, orender_path: Option<String>) -> Option<String> {
    // Takes the same optional override the launch commands do, so the caller
    // gets the answer for the settings it is actually about to use.
    resolve_orender_binary(&app, orender_path)
        .ok()
        .map(|path| path.display().to_string())
}

/// What Linux appends to `/proc/self/exe` (and so to `current_exe()`) once
/// the file a process was started from has been replaced or removed. The
/// renderer reports that raw answer; [`renderer_mismatch`] reads it.
const REPLACED_EXECUTABLE_SUFFIX: &str = " (deleted)";

/// How the renderer answering differs from the one this Studio would launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererMismatch {
    /// The same executable, but its file was replaced since the renderer
    /// started — typically rebuilt: it still runs the older build, and a
    /// restart loads the new one.
    Replaced { path: String },
    /// Another executable altogether: one left running by another
    /// environment, or a system-wide install that happened to hold the OSC
    /// port. Every control still *sends*, but anything that build does not
    /// implement is silently dropped, which is close to undiagnosable from
    /// the UI.
    Foreign { running: String, expected: String },
}

/// Compare the executable the renderer reports (`running`) with the one this
/// Studio would launch (`expected`). `None` when they match, while either is
/// unknown, and always for an embedded producer, which was never ours to
/// start.
pub fn renderer_mismatch(
    embedded: bool,
    running: Option<&str>,
    expected: Option<&str>,
) -> Option<RendererMismatch> {
    if embedded {
        return None;
    }
    let running = running?.trim();
    let expected = expected?.trim();
    if running.is_empty() || expected.is_empty() || running == expected {
        return None;
    }
    if running.strip_suffix(REPLACED_EXECUTABLE_SUFFIX) == Some(expected) {
        return Some(RendererMismatch::Replaced {
            path: expected.to_owned(),
        });
    }
    Some(RendererMismatch::Foreign {
        running: running.to_owned(),
        expected: expected.to_owned(),
    })
}

fn run_command(mut cmd: ProcessCommand, action: &str) -> Result<String, String> {
    // Actions can include an interactive elevation prompt; timing out its
    // parent does not cancel the elevated action. Short limits apply only to
    // non-interactive discovery/status queries below.
    let output = cmd.output().map_err(|e| format!("{action}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(if detail.is_empty() {
            format!("{action}: command failed")
        } else {
            format!("{action}: {detail}")
        })
    }
}

fn wait_for_orender_disconnect(state: &SharedState, timeout_ms: u64) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if state.stats.connection_state() != crate::osc::ConnectionState::Connected {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("timed out while waiting for orender to stop".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn stop_non_service_orender_if_running(state: &SharedState) -> Result<(), String> {
    let is_connected = state.stats.connection_state() == crate::osc::ConnectionState::Connected;
    if !is_connected {
        return Ok(());
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_QUIT.to_string(),
        },
    );
    // A lost goodbye is detected by the 10s ack timeout on a 5s heartbeat
    // cadence. Allow that full bound plus queue/scheduler slack.
    wait_for_orender_disconnect(state, 20_000)
}

#[cfg(target_os = "windows")]
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(target_os = "windows")]
fn run_elevated_windows(program: &str, args: &[String], action: &str) -> Result<String, String> {
    let arg_list = if args.is_empty() {
        "@()".to_string()
    } else {
        format!(
            "@({})",
            args.iter()
                .map(|arg| powershell_single_quote(arg))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let command = format!(
        "$p = Start-Process -FilePath {} -ArgumentList {} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        powershell_single_quote(program),
        arg_list
    );
    let mut cmd = ProcessCommand::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
    run_command(cmd, action)
}

#[cfg(any(target_os = "linux", test))]
fn systemd_escape_arg(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            ' ' | '\t' | '\n' | '\\' | '"' | '\'' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// The systemd user unit for `exec_path args…`. The Studio deb and the AUR
/// `orender` package ship the text this returns for `/usr/bin/orender` and the
/// default settings (`packaging/systemd/omniphony-renderer.service`, checked by
/// a test), so a packaged unit and one Studio installs replace each other.
#[cfg(any(target_os = "linux", test))]
fn linux_service_unit(exec_path: &Path, args: &[String]) -> String {
    let mut exec = Vec::with_capacity(args.len() + 1);
    exec.push(systemd_escape_arg(&exec_path.display().to_string()));
    exec.extend(args.iter().map(|arg| systemd_escape_arg(arg)));
    format!(
        "[Unit]\nDescription=Omniphony Renderer\nAfter=graphical-session.target pipewire.service wireplumber.service\nWants=graphical-session.target\n\n[Service]\nType=notify\nExecStart={}\nRestart=on-failure\nRestartSec=2\nKillSignal=SIGINT\nTimeoutStopSec=30\n\n[Install]\nWantedBy=default.target\n",
        exec.join(" ")
    )
}

/// What the AppImage runtime tells the process it starts, read once by the
/// install command and built by hand in the tests.
#[cfg(any(target_os = "linux", test))]
#[derive(Default)]
struct AppImageEnv {
    /// `$APPDIR`: where the image is mounted (or extracted, with
    /// `--appimage-extract-and-run`), removed when the AppImage exits.
    appdir: Option<PathBuf>,
    /// `$TMPDIR`, under which the runtime makes its `.mount_*` directory when
    /// it is set; `/tmp` otherwise.
    tmpdir: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
impl AppImageEnv {
    fn from_env() -> Self {
        let var = |name| {
            env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            appdir: var("APPDIR"),
            tmpdir: var("TMPDIR"),
        }
    }
}

/// Whether `path` lies inside an AppImage's mount: under `$APPDIR`, or in a
/// `.mount_*` directory right under `/tmp` or `$TMPDIR`, the runtime's mount
/// point (which also catches an orender found in *another* running AppImage).
/// A unit naming such a path stops working when that AppImage exits.
#[cfg(any(target_os = "linux", test))]
fn inside_appimage_mount(path: &Path, env: &AppImageEnv) -> bool {
    if env
        .appdir
        .as_deref()
        .is_some_and(|appdir| path.starts_with(appdir))
    {
        return true;
    }
    let tmp_roots = [Some(Path::new("/tmp")), env.tmpdir.as_deref()];
    path.ancestors().any(|dir| {
        let is_mount = dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".mount_"));
        is_mount
            && tmp_roots
                .iter()
                .flatten()
                .any(|root| dir.parent() == Some(*root))
    })
}

/// `(installed, running)` from `systemctl --user show -p LoadState -p
/// UnitFileState -p ActiveState`. Installed means enabled, or running: the
/// unit the Studio deb and the AUR package ship is loaded but disabled until
/// the user enables it, and is offered for installation like a missing one.
#[cfg(any(target_os = "linux", test))]
fn systemd_service_state(show: &str) -> (bool, bool) {
    let mut loaded = false;
    let mut enabled = false;
    let mut running = false;
    for line in show.lines() {
        match line.trim().split_once('=') {
            Some(("LoadState", value)) => loaded = value == "loaded",
            Some(("UnitFileState", value)) => {
                enabled = matches!(
                    value,
                    "enabled" | "enabled-runtime" | "linked" | "linked-runtime" | "alias"
                )
            }
            Some(("ActiveState", value)) => {
                running = matches!(value, "active" | "reloading" | "refreshing")
            }
            _ => {}
        }
    }
    let running = loaded && running;
    (loaded && (enabled || running), running)
}

#[cfg(target_os = "linux")]
fn linux_user_service_name() -> String {
    format!("{ORENDER_SERVICE_NAME}.service")
}

#[cfg(target_os = "linux")]
fn linux_user_service_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

#[cfg(target_os = "linux")]
fn run_user_systemctl(args: &[&str], action: &str) -> Result<String, String> {
    let mut cmd = ProcessCommand::new("systemctl");
    cmd.arg("--user").args(args);
    run_command(cmd, action)
}

#[cfg(target_os = "windows")]
fn windows_service_bin_path(exec_path: &PathBuf, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(format!("\"{}\"", exec_path.display()));
    for arg in args {
        let escaped = arg.replace('"', "\\\"");
        if escaped.contains(' ') || escaped.contains('\t') {
            parts.push(format!("\"{}\"", escaped));
        } else {
            parts.push(escaped);
        }
    }
    parts.join(" ")
}

/// Whether a managed orender service instance is running (watchdog gate: a
/// service-owned renderer must not be doubled by an auto-started one).
pub fn orender_service_running() -> bool {
    get_orender_service_status()
        .map(|status| status.running)
        .unwrap_or(false)
}

pub fn get_orender_service_status() -> Result<OrenderServiceStatus, String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let output = crate::host::process::capture(
            ProcessCommand::new("systemctl").args([
                "--user",
                "show",
                "-p",
                "LoadState",
                "-p",
                "UnitFileState",
                "-p",
                "ActiveState",
                &service_name,
            ]),
            std::time::Duration::from_secs(2),
        )
        .map_err(|e| format!("query service status: {e}"))?;
        let (installed, running) = if output.status.success() {
            systemd_service_state(&String::from_utf8_lossy(&output.stdout))
        } else {
            (false, false)
        };
        return Ok(OrenderServiceStatus {
            installed,
            running,
            manager: "systemd-user",
        });
    }

    #[cfg(target_os = "windows")]
    {
        let output = crate::host::process::capture(
            ProcessCommand::new("sc").args(["query", ORENDER_SERVICE_NAME]),
            std::time::Duration::from_secs(2),
        )
        .map_err(|e| format!("query service status: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let missing =
            stdout.contains("1060") || stderr.contains("1060") || stdout.contains("does not exist");
        let installed = output.status.success() && !missing;
        let running = installed && stdout.contains("RUNNING");
        return Ok(OrenderServiceStatus {
            installed,
            running,
            manager: "scm",
        });
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

/// Installing the service makes it the owner of the renderer, so turn off the
/// auto-start watchdog — otherwise it would spawn a competing CLI standby — and
/// suppress any in-flight check. The user can re-enable auto-start afterwards.
fn disable_autostart_for_service(state: &SharedState) {
    if let Err(error) = state.config.update(|cfg| cfg.auto_start_renderer = false) {
        log::warn!("[osc] {error}");
    }
    state.watchdog.lock().unwrap().suppressed = true;
}

pub fn install_orender_service(
    app: &HostPaths,
    state: &SharedState,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<serde_json::Value, String> {
    let orender_binary = resolve_orender_binary(app, orender_path)?;
    // Refused before anything is stopped or written.
    #[cfg(target_os = "linux")]
    if inside_appimage_mount(&orender_binary, &AppImageEnv::from_env()) {
        return Err(crate::i18n::tf(
            "osc.service.appImageRefused",
            &[("path", &orender_binary.display().to_string())],
        ));
    }

    stop_non_service_orender_if_running(state)?;

    let spec = orender_launch_spec(
        state,
        orender_binary,
        host,
        osc_rx_port,
        osc_port,
        osc_metering_enabled,
        log_level,
    );

    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let unit_dir = linux_user_service_dir()?;
        let unit_path = unit_dir.join(&service_name);
        std::fs::create_dir_all(&unit_dir).map_err(|e| {
            format!(
                "install service: failed to create {}: {e}",
                unit_dir.display()
            )
        })?;
        std::fs::write(
            &unit_path,
            linux_service_unit(&spec.orender_path, &spec.args),
        )
        .map_err(|e| {
            format!(
                "install service: failed to write {}: {e}",
                unit_path.display()
            )
        })?;
        run_user_systemctl(&["daemon-reload"], "install service")?;
        run_user_systemctl(&["enable", &service_name], "install service")?;
        disable_autostart_for_service(&state);
        return Ok(serde_json::json!({
            "command": format!("systemctl --user enable {service_name}")
        }));
    }

    #[cfg(target_os = "windows")]
    {
        let bin_path = windows_service_bin_path(&spec.orender_path, &spec.args);

        // Use a temp .ps1 script with New-Service so that the BinaryPathName
        // (which contains embedded double-quotes) is passed directly to the
        // CreateService Win32 API as a PS parameter, bypassing Win32
        // command-line parsing entirely.  Start-Process -ArgumentList mangles
        // embedded double-quotes when building sc.exe's command line, which is
        // why the sc.exe create approach silently fails.
        let script_path = {
            let mut p = std::env::temp_dir();
            p.push(format!("omniphony-install-{}.ps1", std::process::id()));
            p
        };
        let script = format!(
            "try {{\r\n\
             $cfgDir = Join-Path $env:ProgramData 'omniphony'\r\n\
             New-Item -ItemType Directory -Force -Path $cfgDir | Out-Null\r\n\
             icacls $cfgDir /grant '*S-1-5-32-545:(OI)(CI)M' /T | Out-Null\r\n\
             New-Service -Name {name} -BinaryPathName {bin} \
             -DisplayName {display} -StartupType Manual -ErrorAction Stop\r\n\
             & sc.exe description {name} {desc}\r\n\
             exit 0\r\n\
             }} catch {{\r\n\
             Write-Error $_\r\n\
             exit 1\r\n\
             }}\r\n",
            name = powershell_single_quote(ORENDER_SERVICE_NAME),
            bin = powershell_single_quote(&bin_path),
            display = powershell_single_quote("Omniphony Renderer"),
            desc = powershell_single_quote("Omniphony spatial audio renderer"),
        );
        std::fs::write(&script_path, script)
            .map_err(|e| format!("install service: failed to write temp script: {e}"))?;

        let ps_path = script_path.display().to_string();
        let command = format!(
            "$p = Start-Process powershell \
             -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',{}) \
             -Verb RunAs -Wait -PassThru; \
             if ($p) {{ exit $p.ExitCode }} else {{ exit 1 }}",
            powershell_single_quote(&ps_path),
        );
        let mut ps_cmd = ProcessCommand::new("powershell");
        ps_cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
        let result = run_command(ps_cmd, "install service");
        let _ = std::fs::remove_file(&script_path);
        result?;

        disable_autostart_for_service(&state);
        return Ok(serde_json::json!({
            "command": format!("sc create {} binPath= {}", ORENDER_SERVICE_NAME, bin_path)
        }));
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

pub fn uninstall_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let unit_path = linux_user_service_dir()?.join(&service_name);
        let _ = run_user_systemctl(&["stop", &service_name], "uninstall service");
        let _ = run_user_systemctl(&["disable", &service_name], "uninstall service");
        if unit_path.exists() {
            std::fs::remove_file(&unit_path).map_err(|e| {
                format!(
                    "uninstall service: failed to remove {}: {e}",
                    unit_path.display()
                )
            })?;
        }
        run_user_systemctl(&["daemon-reload"], "uninstall service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let _ = run_elevated_windows(
            "sc.exe",
            &["stop".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "uninstall service",
        );
        run_elevated_windows(
            "sc.exe",
            &["delete".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "uninstall service",
        )?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

pub fn start_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        run_user_systemctl(&["start", &service_name], "start service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        run_elevated_windows(
            "sc.exe",
            &["start".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "start service",
        )?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

pub fn stop_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        run_user_systemctl(&["stop", &service_name], "stop service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        run_elevated_windows(
            "sc.exe",
            &["stop".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "stop service",
        )?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

pub fn restart_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        run_user_systemctl(&["restart", &service_name], "restart service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let command = format!(
            "$p = Start-Process -FilePath powershell -ArgumentList @('-NoProfile','-NonInteractive','-Command',{}) -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
            powershell_single_quote(&format!(
                "Restart-Service -Name '{}' -Force -ErrorAction Stop",
                ORENDER_SERVICE_NAME
            ))
        );
        let mut cmd = ProcessCommand::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
        run_command(cmd, "restart service")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

pub fn restart_pipewire_services() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        run_user_systemctl(&["restart", "pipewire", "wireplumber"], "restart pipewire")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("PipeWire restart is only supported on Linux".to_string())
}

/// Spawn the renderer from a resolved launch spec, keeping the `Child` in the
/// shared state so the watchdog can detect fast failures and the exit hook can
/// take it down with Studio. Used by the manual `launch_orender` command and
/// by the auto-start watchdog.
fn spawn_orender_process(
    state: &SharedState,
    spec: &OrenderLaunchSpec,
) -> Result<serde_json::Value, String> {
    let log_path = default_orender_log_path();
    // The environment's namespace may not exist yet on a first launch, unlike
    // the temp dir the built-in default lands in.
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let stdout = File::create(&log_path).map_err(|e| format!("failed to create log file: {e}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("failed to clone log file handle: {e}"))?;

    #[allow(unused_mut)]
    let mut cmd = ProcessCommand::new(&spec.orender_path);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    // The bridges the player is configured with in mpv.conf, as the exact
    // files, a path list (a folder would let the engine load other bridges
    // sitting beside them). They only take part in the engine's
    // auto-discovery: `render.bridge_paths` in its config wins over them. A
    // bridge variable set in Studio's own environment is inherited and wins
    // over mpv.conf.
    if let Some(files) = mpv_bridge::bridge_file_for_renderer(
        std::env::var_os(mpv_bridge::BRIDGE_FILE_ENV).as_ref(),
        std::env::var_os(mpv_bridge::BRIDGE_DIR_ENV).as_ref(),
        &mpv_bridge::MpvPaths::from_env(),
    ) {
        cmd.env(mpv_bridge::BRIDGE_FILE_ENV, files);
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const NORMAL_PRIORITY_CLASS: u32 = 0x0000_0020;
        cmd.creation_flags(CREATE_NO_WINDOW | NORMAL_PRIORITY_CLASS);
    }

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch orender: {e}"))?;

    {
        let mut guard = state.renderer_child.lock().unwrap();
        // A previously tracked child that already exited is just dropped;
        // a still-running one is left alone (it owns the OSC port and the
        // new instance will negotiate it via --osc-yield).
        *guard = Some(child);
    }
    {
        let mut wd = state.watchdog.lock().unwrap();
        wd.last_spawn_at = Some(std::time::Instant::now());
        wd.awaiting_answer = true;
    }
    // The goodbye has been answered: this is the renderer that replaces it.
    state.stats.goodbye.forget();

    Ok(serde_json::json!({
        "command": format!("{} {}", spec.orender_path.display(), spec.args.join(" ")),
        "logPath": log_path.display().to_string()
    }))
}

/// Why the local renderer's last automatic start failed, and the log it
/// writes to, for the banner shown while no engine answers. `None` when the
/// last start did not fail, after a re-arm, and once a renderer connected.
pub fn autostart_failure(state: &SharedState) -> Option<(String, PathBuf)> {
    let failure = state.watchdog.lock().unwrap().last_failure.clone()?;
    Some((failure, default_orender_log_path()))
}

/// Watchdog entry point: launch a standby renderer from the saved OSC config
/// (binary discovery only — no user-supplied path or log level).
pub fn autostart_orender(
    app: &HostPaths,
    state: &SharedState,
) -> Result<serde_json::Value, String> {
    let mut cfg = state.config.snapshot();
    let target = (*state.stats.target.lock().unwrap()).ok_or("no active renderer target")?;
    cfg.host = target.ip().to_string();
    cfg.osc_rx_port = target.port();
    let spec = resolve_orender_launch_spec(
        app,
        state,
        cfg.host.clone(),
        cfg.osc_rx_port,
        cfg.osc_port,
        cfg.osc_metering_enabled,
        None,
        None,
    )?;
    spawn_orender_process(state, &spec)
}

pub fn launch_orender(
    app: &HostPaths,
    state: &SharedState,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<serde_json::Value, String> {
    let spec = resolve_orender_launch_spec(
        &app,
        &state,
        host,
        osc_rx_port,
        osc_port,
        osc_metering_enabled,
        orender_path,
        log_level,
    )?;
    // A manual launch is a deliberate user action: re-arm the watchdog.
    state.watchdog.lock().unwrap().rearm();
    spawn_orender_process(&state, &spec)
}

pub fn stop_orender(state: &SharedState) {
    // A manual stop is deliberate: suppress the auto-start watchdog so it does
    // not immediately respawn the renderer the user just asked to stop. Cleared
    // on the next manual launch or settings save (both re-arm the watchdog).
    state.watchdog.lock().unwrap().suppressed = true;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: osc_contract::CONTROL_QUIT.to_string(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mismatch_is_only_claimed_when_both_paths_are_known_and_differ() {
        let ours = Some("/usr/bin/orender");
        let theirs = Some("/opt/other/orender");
        assert_eq!(
            renderer_mismatch(false, theirs, ours),
            Some(RendererMismatch::Foreign {
                running: "/opt/other/orender".into(),
                expected: "/usr/bin/orender".into(),
            })
        );
        assert_eq!(renderer_mismatch(false, ours, ours), None);
        // Half the answer is no answer: an unknown path must not be reported
        // as a mismatch.
        assert_eq!(renderer_mismatch(false, None, ours), None);
        assert_eq!(renderer_mismatch(false, theirs, None), None);
        assert_eq!(renderer_mismatch(false, Some("  "), ours), None);
        // An embedded producer is never one this Studio started.
        assert_eq!(renderer_mismatch(true, theirs, ours), None);
    }

    #[test]
    fn a_rebuilt_binary_is_the_same_renderer_running_an_older_build() {
        let ours = Some("/w/omniphony-renderer/target/release/orender");
        // What Linux reports once the binary was rebuilt under the process.
        let replaced = Some("/w/omniphony-renderer/target/release/orender (deleted)");
        assert_eq!(
            renderer_mismatch(false, replaced, ours),
            Some(RendererMismatch::Replaced {
                path: "/w/omniphony-renderer/target/release/orender".into(),
            })
        );
        // A replaced binary elsewhere is still someone else's renderer, shown
        // as reported.
        assert_eq!(
            renderer_mismatch(false, Some("/opt/other/orender (deleted)"), ours),
            Some(RendererMismatch::Foreign {
                running: "/opt/other/orender (deleted)".into(),
                expected: "/w/omniphony-renderer/target/release/orender".into(),
            })
        );
        assert_eq!(renderer_mismatch(true, replaced, ours), None);
    }

    /// The restart banner's question, asked every frame, must come back at
    /// once while the worker is quitting the renderer: a renderer that does
    /// not answer the quit keeps the worker in its grace loop for two seconds,
    /// and the lock it took across that loop held every frame back with it.
    #[cfg(unix)]
    #[test]
    fn asking_whether_the_renderer_runs_never_waits_on_its_quit() {
        use crate::host::services::operations::{Action, Operations};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let (state, outbox) = crate::host::commands::tests::state_with_outbox(Arc::new(|| {}));
        let state = Arc::new(state);
        // Stands in for a renderer deaf to OSC: the quit runs its whole grace
        // period to the kill.
        let child = ProcessCommand::new("sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .expect("sh and sleep are available on unix");
        *state.renderer_child.lock().unwrap() = Some(child);
        assert!(launched_renderer_running(&state));

        let quitting = {
            let state = Arc::clone(&state);
            std::thread::spawn(move || quit_launched_renderer(&state))
        };
        // The quit message goes out before the grace loop starts: once it is
        // here, the worker is in the loop.
        outbox
            .recv_timeout(Duration::from_secs(1))
            .expect("the quit is sent before the grace loop");

        let asked = Instant::now();
        let action = Operations::default().restart_action(&state);
        let waited = asked.elapsed();
        assert!(
            matches!(action, Some(Action::Restart)),
            "the renderer is still ours while it is being quit"
        );
        assert!(
            waited < Duration::from_millis(500),
            "restart_action waited {waited:?} on the quit in progress"
        );

        quitting.join().unwrap();
        assert!(
            !launched_renderer_running(&state),
            "the grace period ended in a kill"
        );
    }

    /// The unit the Studio deb and the AUR `orender` package install, from
    /// the repository root. Both put `orender` at `/usr/bin/orender`, so one
    /// file serves both.
    const SHIPPED_UNIT: &str = "packaging/systemd/omniphony-renderer.service";

    #[test]
    fn the_shipped_unit_is_the_one_install_service_writes() {
        let root = repo_root().expect("tests run from a source tree");
        let shipped = std::fs::read_to_string(root.join(SHIPPED_UNIT))
            .unwrap_or_else(|e| panic!("{SHIPPED_UNIT}: {e}"))
            .replace("\r\n", "\n");
        // The leading `#` block explains the file; systemd ignores it, and the
        // rest must be byte for byte what Studio writes.
        let body = shipped
            .lines()
            .skip_while(|line| line.starts_with('#') || line.is_empty())
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        assert!(
            shipped.starts_with('#'),
            "{SHIPPED_UNIT} keeps its header saying where its settings come from"
        );
        let defaults = crate::host::config::OscConfig::default();
        // Studio's defaults: `default_orender_input_path()` with neither
        // `$TMPDIR` nor `OMNIPHONY_INPUT_PIPE` set, the default OSC target,
        // the default metering switch and the default log level.
        let args = orender_render_args(
            Path::new("/tmp/orender.pipe"),
            &defaults.host,
            crate::host::runtime_env::DEFAULT_OSC_RX_PORT,
            defaults.osc_metering_enabled,
            None,
        );
        assert_eq!(
            body,
            linux_service_unit(Path::new("/usr/bin/orender"), &args),
            "{SHIPPED_UNIT} drifted from linux_service_unit(); regenerate it"
        );
    }

    #[test]
    fn the_render_args_carry_only_what_differs_from_the_defaults() {
        let base = orender_render_args(Path::new("/p"), " 10.0.0.2 ", 9100, false, None);
        assert_eq!(
            base.join(" "),
            "render /p --continuous --enable-vbap --osc --osc-host 10.0.0.2 \
             --osc-port 9100 --osc-rx-port 9100 --osc-yield"
        );
        assert_eq!(
            orender_render_args(Path::new("/p"), "h", 1, false, Some(" info ")),
            orender_render_args(Path::new("/p"), "h", 1, false, None)
        );
        assert_eq!(
            orender_render_args(Path::new("/p"), "h", 1, false, Some("nonsense")),
            orender_render_args(Path::new("/p"), "h", 1, false, None)
        );
        let full = orender_render_args(Path::new("/p"), "h", 1, true, Some("debug"));
        assert!(full.ends_with(&[
            "--osc-yield".to_string(),
            "--osc-metering".to_string(),
            "--loglevel".to_string(),
            "debug".to_string(),
        ]));
    }

    #[test]
    fn an_orender_inside_an_appimage_mount_is_recognised() {
        let none = AppImageEnv::default();
        let running = AppImageEnv {
            appdir: Some("/tmp/.mount_OmniphA1b2C3".into()),
            tmpdir: None,
        };
        let inside = Path::new("/tmp/.mount_OmniphA1b2C3/usr/bin/orender");
        assert!(inside_appimage_mount(inside, &running));
        // Without the runtime's variables (an orender path remembered from an
        // earlier run, another AppImage's mount), the mount point still tells.
        assert!(inside_appimage_mount(inside, &none));
        assert!(inside_appimage_mount(
            Path::new("/tmp/.mount_Other999/usr/bin/orender"),
            &running
        ));
        // `--appimage-extract-and-run` extracts under a name of its own and
        // deletes it on exit; `$APPDIR` names it.
        let extracted = AppImageEnv {
            appdir: Some("/tmp/appimage_extracted_0123abcd".into()),
            tmpdir: None,
        };
        assert!(inside_appimage_mount(
            Path::new("/tmp/appimage_extracted_0123abcd/usr/bin/orender"),
            &extracted
        ));
        // The runtime mounts under `$TMPDIR` when it is set.
        let moved_tmp = AppImageEnv {
            appdir: None,
            tmpdir: Some("/run/user/1000/tmp".into()),
        };
        assert!(inside_appimage_mount(
            Path::new("/run/user/1000/tmp/.mount_OmniphX/usr/bin/orender"),
            &moved_tmp
        ));
        assert!(!inside_appimage_mount(
            Path::new("/run/user/1000/tmp/.mount_OmniphX/usr/bin/orender"),
            &none
        ));

        // Installed binaries, whether Studio runs from an AppImage or not.
        for installed in [
            "/usr/bin/orender",
            "/home/u/.local/bin/orender",
            "/home/u/src/omniphony-renderer/target/release/orender",
            // A component that merely looks alike, away from the temp dir.
            "/home/u/.mount_backup/orender",
            "/tmp/backup/.mount_x/orender",
        ] {
            for env in [&none, &running, &moved_tmp] {
                assert!(
                    !inside_appimage_mount(Path::new(installed), env),
                    "{installed}"
                );
            }
        }
    }

    #[test]
    fn a_shipped_but_disabled_unit_is_not_installed() {
        let show = |load: &str, file: &str, active: &str| {
            systemd_service_state(&format!(
                "LoadState={load}\nUnitFileState={file}\nActiveState={active}\n"
            ))
        };
        // The unit the deb or the AUR package ships, before the user enables it.
        assert_eq!(show("loaded", "disabled", "inactive"), (false, false));
        // Installed by Studio, or enabled by hand, and running or not.
        assert_eq!(show("loaded", "enabled", "inactive"), (true, false));
        assert_eq!(show("loaded", "enabled", "active"), (true, true));
        assert_eq!(show("loaded", "linked", "inactive"), (true, false));
        // Started without being enabled: it runs, so it is Studio's to stop.
        assert_eq!(show("loaded", "disabled", "active"), (true, true));
        // No unit at all.
        assert_eq!(show("not-found", "", "inactive"), (false, false));
        assert_eq!(systemd_service_state(""), (false, false));
    }

    #[test]
    fn the_repo_root_is_the_checkout_that_holds_this_crate() {
        // The search walks up from this crate's manifest until it finds the
        // renderer, so it holds however deep the crate sits in the checkout —
        // `omniphony-studio-egui/core/` since the core became its own crate.
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repo_root().expect("tests run from a source tree");
        assert!(
            crate_dir.starts_with(&root),
            "{} is not inside {}",
            crate_dir.display(),
            root.display()
        );
        assert!(root.join("omniphony-studio-egui/Cargo.toml").is_file());
        // However deep the search starts inside the checkout.
        for start in [crate_dir.join("src/host/commands"), root.clone()] {
            assert_eq!(
                repo_root_above(&start),
                Some(root.clone()),
                "from {}",
                start.display()
            );
        }
    }

    #[test]
    fn the_checkouts_own_renderer_builds_are_the_ones_looked_for() {
        // What `expected_orender_path` is compared against in a dev build.
        let root = repo_root().expect("tests run from a source tree");
        assert_eq!(
            repo_orender_candidates(Some(&root)),
            [
                root.join("omniphony-renderer/target/release/orender"),
                root.join("omniphony-renderer/target/debug/orender"),
            ]
        );
        // Away from a source tree there is nothing to look for.
        assert!(repo_orender_candidates(None).is_empty());
    }
}
