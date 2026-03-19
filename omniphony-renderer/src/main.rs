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

/// Pre-create JACK client named pipe instances with a null DACL before libjack
/// does, establishing a world-accessible device-object security descriptor.
///
/// Windows sets a named pipe's device-object security descriptor from the FIRST
/// `CreateNamedPipeW` call for each pipe name.  By pre-creating with null DACL,
/// all subsequent instances — including those libjack creates with `NULL`
/// lpSecurityAttributes — use the already-established null-DACL descriptor.
/// jackd in Session 1 can then connect without ACCESS_DENIED (err = 5).
///
/// The returned `Vec<HANDLE>` keeps the pre-created instances alive for the
/// service lifetime.  Because `ConnectNamedPipe` is never called on them,
/// jackd will not accidentally connect to our dummy instances; it will connect
/// to libjack's listening instances instead.
///
/// Pipe parameters must match what libjack uses (PIPE_ACCESS_INBOUND |
/// FILE_FLAG_OVERLAPPED, PIPE_TYPE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES)
/// so that libjack's subsequent `CreateNamedPipeW` call succeeds.
#[cfg(windows)]
fn pre_create_jack_client_pipes() -> Vec<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::Security::{
        InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl,
    };
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::System::Pipes::{CreateNamedPipeW, NAMED_PIPE_MODE};
    use windows::core::PCWSTR;

    // Match libjack's CreateNamedPipeW parameters exactly.
    // All instances of a named pipe must use the same access/type flags.
    const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
    const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    const PIPE_TYPE_BYTE: u32 = 0x0000_0000;
    const PIPE_WAIT: u32 = 0x0000_0000;
    const PIPE_UNLIMITED_INSTANCES: u32 = 255;

    let mut sd = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
    let psd = PSECURITY_DESCRIPTOR(&mut sd as *mut SECURITY_DESCRIPTOR as *mut _);
    if unsafe { InitializeSecurityDescriptor(psd, 1) }.is_err()
        || unsafe { SetSecurityDescriptorDacl(psd, BOOL(1), None, BOOL(0)) }.is_err()
    {
        log::warn!("pre_create_jack_client_pipes: failed to build null DACL SD");
        return Vec::new();
    }
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: BOOL(0),
    };

    let mut handles = Vec::new();
    for i in 0u32..16 {
        let name: Vec<u16> = format!("\\\\.\\pipe\\client_jack_orender_{i}\0")
            .encode_utf16()
            .collect();
        let h = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_INBOUND | FILE_FLAG_OVERLAPPED),
                NAMED_PIPE_MODE(PIPE_TYPE_BYTE | PIPE_WAIT),
                PIPE_UNLIMITED_INSTANCES,
                65536,
                65536,
                0,
                Some(&sa),
            )
        };
        use windows::Win32::Foundation::INVALID_HANDLE_VALUE;
        if h != INVALID_HANDLE_VALUE {
            log::info!("pre_create_jack_client_pipes: seeded client_jack_orender_{i}");
            handles.push(h);
        } else {
            log::debug!(
                "pre_create_jack_client_pipes: client_jack_orender_{i} failed: {:?}",
                windows::core::Error::from_win32()
            );
        }
    }
    handles
}

/// Entry point when running as a Windows service (called by SCM via sys::windows).
/// Parses args from the service's binPath and runs the render command.
#[cfg(windows)]
fn run_as_service() -> anyhow::Result<()> {
    use clap::Parser as ClapParser;
    use cli::command::{Cli, Commands};
    use cli::decode::cmd_render;

    // Pre-create JACK client pipe names with a null DACL so that libjack's
    // subsequent CreateNamedPipeW calls inherit the world-accessible device-
    // object security descriptor we established.  jackd in Session 1 can then
    // connect without ACCESS_DENIED (err = 5).  Handles kept alive for the
    // service lifetime; ConnectNamedPipe is never called on them.
    let _jack_pipe_guards = pre_create_jack_client_pipes();

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
