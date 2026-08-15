//! orender launch + service management: resolving the launch spec (bundled vs
//! configured paths), spawning/stopping the renderer directly, and install /
//! uninstall / start / stop of the OS service (systemd user unit on Linux, SC on
//! Windows), plus a PipeWire restart helper.
//!
//! The private helpers below are platform glue used only by these commands.

use crate::config::{load_config, save_config};
use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use std::env;
use std::fs::File;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use tauri::{Manager, State};

const ORENDER_SERVICE_NAME: &str = "omniphony-renderer";

struct OrenderLaunchSpec {
    orender_path: PathBuf,
    args: Vec<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct OrenderServiceStatus {
    installed: bool,
    running: bool,
    manager: &'static str,
}

fn first_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.exists()).cloned()
}

fn bundled_orender_candidates(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
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

fn bundled_layouts_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join("layouts"))
}

fn default_orender_input_path() -> PathBuf {
    // An environment that carved out its own runtime namespace pins the pipe,
    // so two renderers never end up reading the same FIFO.
    if let Some(path) = crate::runtime_env::input_pipe() {
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
    if let Some(dir) = crate::runtime_env::config_dir() {
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
    app: &tauri::AppHandle,
    orender_path: Option<String>,
) -> Result<PathBuf, String> {
    let studio_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "failed to resolve studio directory".to_string())?
        .to_path_buf();
    let repo_root = studio_dir
        .parent()
        .ok_or_else(|| "failed to resolve Omniphony repository root".to_string())?
        .to_path_buf();
    let repo_orender_candidates = [
        repo_root.join("omniphony-renderer/target/release/orender"),
        repo_root.join("omniphony-renderer/target/debug/orender"),
    ];

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
        ProcessCommand::new(lookup_cmd)
            .arg("orender")
            .output()
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
    app: &tauri::AppHandle,
    state: &SharedState,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<OrenderLaunchSpec, String> {
    let studio_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "failed to resolve studio directory".to_string())?
        .to_path_buf();
    let repo_root = studio_dir
        .parent()
        .ok_or_else(|| "failed to resolve Omniphony repository root".to_string())?
        .to_path_buf();
    let orender_path = resolve_orender_binary(app, orender_path)?;

    let input_path = default_orender_input_path();

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
        .as_deref()
        .map(str::trim)
        .filter(|s| matches!(*s, "off" | "error" | "warn" | "info" | "debug" | "trace"))
        .unwrap_or("info");
    if level != "info" {
        args.push("--loglevel".to_string());
        args.push(level.to_string());
    }

    if let Some(selected_layout) = state.inner.lock().unwrap().selected_layout_key.clone() {
        let layout_file = format!("{selected_layout}.yaml");
        // The presets dir holds the immersive (with-height) layouts; the older
        // no-height layouts now live in a `legacy/` subfolder. Look in both,
        // bundle first then the repo (dev) fallback.
        let layout_path = bundled_layouts_dir(app)
            .into_iter()
            .flat_map(|dir| {
                [
                    dir.join(&layout_file),
                    dir.join("legacy").join(&layout_file),
                ]
            })
            .chain([
                repo_root.join("layouts").join(&layout_file),
                repo_root.join("layouts").join("legacy").join(&layout_file),
            ])
            .find(|path| path.exists());
        if let Some(layout_path) = layout_path {
            args.push("--speaker-layout".to_string());
            args.push(layout_path.display().to_string());
        }
    }

    // Persist the connection settings used for this launch, preserving the
    // fields this function doesn't manage (auto-start / keep-alive toggles).
    let mut cfg = load_config(&state.config_dir);
    cfg.host = host.trim().to_string();
    cfg.osc_rx_port = osc_rx_port;
    cfg.osc_port = osc_port;
    cfg.osc_metering_enabled = osc_metering_enabled;
    let _ = save_config(&state.config_dir, &cfg);

    Ok(OrenderLaunchSpec { orender_path, args })
}

/// Path of the `orender` binary this Studio would launch, so the UI can compare
/// it with the path the connected renderer reports over OSC.
///
/// A mismatch means Studio is driving a renderer it did not start — typically
/// one left running by another environment, or a system-wide install that
/// happened to hold the OSC port. Every control still *sends*, but anything the
/// other build does not implement is silently dropped, which is close to
/// undiagnosable from the UI. Returns `None` when no binary can be resolved at
/// all; that is a separate, already-reported condition.
#[tauri::command]
pub fn expected_orender_path(
    app: tauri::AppHandle,
    orender_path: Option<String>,
) -> Option<String> {
    // Takes the same optional override the launch commands do, so the caller
    // gets the answer for the settings it is actually about to use.
    resolve_orender_binary(&app, orender_path)
        .ok()
        .map(|path| path.display().to_string())
}

fn run_command(mut cmd: ProcessCommand, action: &str) -> Result<String, String> {
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
        let status = state.inner.lock().unwrap().osc_status.clone();
        if status.as_deref() != Some("connected") {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("timed out while waiting for orender to stop".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn stop_non_service_orender_if_running(state: &SharedState) -> Result<(), String> {
    let is_connected = state.inner.lock().unwrap().osc_status.as_deref() == Some("connected");
    if !is_connected {
        return Ok(());
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/quit".to_string(),
        },
    );
    wait_for_orender_disconnect(state, 10_000)
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn linux_service_unit(exec_path: &PathBuf, args: &[String]) -> String {
    let mut exec = Vec::with_capacity(args.len() + 1);
    exec.push(systemd_escape_arg(&exec_path.display().to_string()));
    exec.extend(args.iter().map(|arg| systemd_escape_arg(arg)));
    format!(
        "[Unit]\nDescription=Omniphony Renderer\nAfter=graphical-session.target pipewire.service wireplumber.service\nWants=graphical-session.target\n\n[Service]\nType=notify\nExecStart={}\nRestart=on-failure\nRestartSec=2\nKillSignal=SIGINT\nTimeoutStopSec=30\n\n[Install]\nWantedBy=default.target\n",
        exec.join(" ")
    )
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
pub(crate) fn orender_service_running() -> bool {
    get_orender_service_status()
        .map(|status| status.running)
        .unwrap_or(false)
}

#[tauri::command]
pub fn get_orender_service_status() -> Result<OrenderServiceStatus, String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let output = ProcessCommand::new("systemctl")
            .args([
                "--user",
                "show",
                "-p",
                "LoadState",
                "--value",
                &service_name,
            ])
            .output()
            .map_err(|e| format!("query service status: {e}"))?;
        let load_state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let installed =
            output.status.success() && load_state != "not-found" && !load_state.is_empty();
        let running = if installed {
            ProcessCommand::new("systemctl")
                .args(["--user", "is-active", "--quiet", &service_name])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        } else {
            false
        };
        return Ok(OrenderServiceStatus {
            installed,
            running,
            manager: "systemd-user",
        });
    }

    #[cfg(target_os = "windows")]
    {
        let output = ProcessCommand::new("sc")
            .args(["query", ORENDER_SERVICE_NAME])
            .output()
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
    let mut cfg = load_config(&state.config_dir);
    if cfg.auto_start_renderer {
        cfg.auto_start_renderer = false;
        let _ = save_config(&state.config_dir, &cfg);
    }
    state.watchdog.lock().unwrap().suppressed = true;
}

#[tauri::command]
pub fn install_orender_service(
    app: tauri::AppHandle,
    state: State<SharedState>,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<serde_json::Value, String> {
    stop_non_service_orender_if_running(&state)?;

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

#[tauri::command]
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

#[tauri::command]
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

#[tauri::command]
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

#[tauri::command]
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
            powershell_single_quote(
                &format!(
                    "Restart-Service -Name '{}' -Force -ErrorAction Stop",
                    ORENDER_SERVICE_NAME
                )
            )
        );
        let mut cmd = ProcessCommand::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
        run_command(cmd, "restart service")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
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
        wd.check_requested_at = None;
    }

    Ok(serde_json::json!({
        "command": format!("{} {}", spec.orender_path.display(), spec.args.join(" ")),
        "logPath": log_path.display().to_string()
    }))
}

/// Watchdog entry point: launch a standby renderer from the saved OSC config
/// (binary discovery only — no user-supplied path or log level).
pub(crate) fn autostart_orender(
    app: &tauri::AppHandle,
    state: &SharedState,
) -> Result<serde_json::Value, String> {
    let cfg = load_config(&state.config_dir);
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

#[tauri::command]
pub fn launch_orender(
    app: tauri::AppHandle,
    state: State<SharedState>,
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

#[tauri::command]
pub fn stop_orender(state: State<SharedState>) {
    // A manual stop is deliberate: suppress the auto-start watchdog so it does
    // not immediately respawn the renderer the user just asked to stop. Cleared
    // on the next manual launch or settings save (both re-arm the watchdog).
    state.watchdog.lock().unwrap().suppressed = true;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/quit".to_string(),
        },
    );
}
