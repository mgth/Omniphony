#![allow(dead_code)]

use anyhow::Result;
use clap::Parser as ClapParser;
use cli::command::{Cli, Commands, LogFormat, LogLevel};
use cli::decode::cmd_render;
#[cfg(feature = "saf_vbap")]
use cli::generate_vbap::cmd_generate_vbap;
#[cfg(target_os = "windows")]
use cli::list_asio_devices::cmd_list_asio_devices;
use log::info;
use std::ffi::OsString;

mod bridge_loader;
mod cli;
mod events;
mod input;
pub(crate) mod timestamp;

fn normalize_cli_args<I>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args: Vec<OsString> = args.into_iter().collect();
    if args.len() <= 1 {
        return args;
    }

    let known_subcommands = [
        OsString::from("render"),
        #[cfg(feature = "saf_vbap")]
        OsString::from("generate-vbap"),
        #[cfg(target_os = "windows")]
        OsString::from("list-asio-devices"),
        OsString::from("help"),
    ];
    let top_level_passthrough = [
        OsString::from("-h"),
        OsString::from("--help"),
        OsString::from("-V"),
        OsString::from("--version"),
    ];

    let mut insert_at = 1usize;
    while insert_at < args.len() {
        let arg = &args[insert_at];
        if top_level_passthrough.iter().any(|v| v == arg) {
            return args;
        }
        if known_subcommands.iter().any(|v| v == arg) {
            return args;
        }

        let arg_str = arg.to_string_lossy();
        if !arg_str.starts_with('-') || arg_str == "-" {
            break;
        }

        let consumes_next = matches!(arg_str.as_ref(), "--config" | "--loglevel" | "--log-format");
        insert_at += 1;
        if consumes_next && insert_at < args.len() {
            insert_at += 1;
        }
    }

    args.insert(insert_at, OsString::from("render"));
    args
}

/// Spawn a background thread that polls for JACK client named pipes created by
/// libjack during `jack_client_open` and patches their DACL to null (world-accessible).
///
/// When orender runs as a Windows service (Session 0 / LocalSystem), libjack
/// calls `CreateNamedPipeW` with `NULL` lpSecurityAttributes.  Windows then
/// applies a *hardcoded* security descriptor (LocalSystem=FULL, Admins=FULL,
/// Everyone=READ_only) — it does NOT use the process token's default DACL.
/// This blocks the JACK server in Session 1 from connecting (err = 5).
///
/// The fix: as LocalSystem we have WRITE_DAC rights on our own pipes.  We
/// open each pipe with `WRITE_DAC` and call `SetKernelObjectSecurity` to
/// replace the DACL with a null (world-accessible) one.
#[cfg(windows)]
fn spawn_jack_pipe_dacl_watcher() {
    use windows::Win32::Foundation::{BOOL, CloseHandle, INVALID_HANDLE_VALUE};
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
        SECURITY_DESCRIPTOR, SetKernelObjectSecurity, SetSecurityDescriptorDacl,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    std::thread::Builder::new()
        .name("jack-pipe-dacl-watcher".into())
        .spawn(move || {
            // Build a null-DACL security descriptor once — reused for every patch.
            let mut sd = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
            let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut SECURITY_DESCRIPTOR as *mut _);
            if unsafe { InitializeSecurityDescriptor(psd, 1) }.is_err() {
                log::error!("jack-pipe-dacl-watcher: InitializeSecurityDescriptor failed");
                return;
            }
            if unsafe { SetSecurityDescriptorDacl(psd, BOOL(1), None, BOOL(0)) }.is_err() {
                log::error!("jack-pipe-dacl-watcher: SetSecurityDescriptorDacl failed");
                return;
            }

            // Track which pipe indices have already been patched this session.
            let mut patched = [false; 16];

            loop {
                if sys::ShutdownHandle::is_requested() {
                    break;
                }

                for i in 0u32..16 {
                    if patched[i as usize] {
                        continue;
                    }

                    // Pipe name: \\.\pipe\client_jack_orender_N
                    let name: Vec<u16> = format!("\\\\.\\pipe\\client_jack_orender_{i}\0")
                        .encode_utf16()
                        .collect();

                    let handle = unsafe {
                        CreateFileW(
                            PCWSTR(name.as_ptr()),
                            // WRITE_DAC (0x00040000) lets us call SetKernelObjectSecurity.
                            // We do not need to read or write pipe data.
                            0x00040000,
                            FILE_SHARE_READ | FILE_SHARE_WRITE,
                            None,
                            OPEN_EXISTING,
                            Default::default(),
                            None,
                        )
                    };

                    match handle {
                        Ok(h) if h != INVALID_HANDLE_VALUE => {
                            let res = unsafe {
                                SetKernelObjectSecurity(h, DACL_SECURITY_INFORMATION, psd)
                            };
                            unsafe { let _ = CloseHandle(h); }
                            if res.is_ok() {
                                log::info!(
                                    "jack-pipe-dacl-watcher: patched DACL on \
                                     \\\\.\\pipe\\client_jack_orender_{i}"
                                );
                                patched[i as usize] = true;
                            } else {
                                log::warn!(
                                    "jack-pipe-dacl-watcher: SetKernelObjectSecurity \
                                     failed for index {i}: {:?}",
                                    res
                                );
                            }
                        }
                        _ => {} // pipe not yet created — will retry
                    }
                }

                // All 16 slots patched — nothing left to watch.
                if patched.iter().all(|&p| p) {
                    break;
                }

                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        })
        .ok(); // If spawn fails we continue without the watcher; JACK may not work.
}

/// Entry point when running as a Windows service (called by SCM via sys::windows).
/// Parses args from the service's binPath and runs the render command.
#[cfg(windows)]
fn run_as_service() -> anyhow::Result<()> {
    use clap::Parser as ClapParser;
    use cli::command::{Cli, Commands};
    use cli::decode::cmd_render;

    // Start the JACK pipe DACL watcher before entering the render loop.
    // libjack creates client pipes (\\.\pipe\client_jack_orender_N) with a
    // hardcoded restrictive DACL when called from a service; the watcher
    // patches them to null (world-accessible) so jackdmp in Session 1 can
    // connect (avoiding err = 5 / ERROR_ACCESS_DENIED).
    spawn_jack_pipe_dacl_watcher();

    let cli = Cli::parse_from(normalize_cli_args(std::env::args_os()));
    match cli.command {
        Commands::Render(ref args) => cmd_render(args, &cli),
        _ => anyhow::bail!(
            "Only the 'render' command is supported in Windows service mode. \
             Embed the full command in the service binPath, e.g.:\n  \
             sc create omniphony-renderer binPath= \"orender.exe --output-backend asio ...\""
        ),
    }
}

fn main() -> Result<()> {
    // When started by the Windows Service Control Manager, enter the SCM
    // dispatch loop and run to completion; otherwise fall through to the
    // normal console CLI flow.
    #[cfg(windows)]
    if sys::windows::try_start_service(run_as_service) {
        return Ok(());
    }

    let mut cli = Cli::parse_from(normalize_cli_args(std::env::args_os()));

    // Load global config before initializing the logger so we can apply the
    // configured log level and format.  Config errors use eprintln! directly
    // because the logger is not yet available.
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    let global_cfg = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default()
        .global
        .unwrap_or_default();

    // Resolve effective loglevel: explicit CLI value beats config; config beats default.
    let effective_loglevel = if cli.loglevel != LogLevel::default() {
        cli.loglevel
    } else {
        global_cfg
            .loglevel
            .as_deref()
            .and_then(|s| s.parse::<LogLevel>().ok())
            .unwrap_or(cli.loglevel)
    };

    // Resolve effective log_format.
    let effective_log_format = if cli.log_format != LogFormat::default() {
        cli.log_format
    } else {
        global_cfg
            .log_format
            .as_deref()
            .and_then(|s| s.parse::<LogFormat>().ok())
            .unwrap_or(cli.log_format)
    };

    // Resolve effective strict: --strict → true, --no-strict → false, else config.
    let effective_strict = if cli.strict {
        true
    } else if cli.no_strict {
        false
    } else {
        global_cfg.strict.unwrap_or(false)
    };

    // Apply effective values back to cli so downstream code (cmd_render etc.) sees them.
    cli.loglevel = effective_loglevel;
    cli.log_format = effective_log_format;
    cli.strict = effective_strict;

    let base_level = cli.loglevel.to_level_filter();

    sys::live_log::init_logger(base_level, matches!(cli.log_format, LogFormat::Json))?;

    info!("{}", cli::command::VERSION_INFO);

    match cli.command {
        Commands::Render(ref args) => cmd_render(args, &cli)?,
        #[cfg(feature = "saf_vbap")]
        Commands::GenerateVbap(ref args) => cmd_generate_vbap(args)?,
        #[cfg(target_os = "windows")]
        Commands::ListAsioDevices => cmd_list_asio_devices()?,
    }

    Ok(())
}
