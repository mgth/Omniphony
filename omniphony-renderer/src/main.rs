#![allow(dead_code)]

use anyhow::Result;
use cli::command::{Commands, LogFormat, LogLevel, ParsedCli};
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
mod runtime_osc;
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
        OsString::from("input-live"),
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

/// Entry point when running as a Windows service (called by SCM via sys::windows).
/// Parses args from the service's binPath and runs the render command.
#[cfg(windows)]
fn run_as_service() -> anyhow::Result<()> {
    use cli::command::Commands;
    use cli::decode::cmd_render;

    let parsed = ParsedCli::parse_from(normalize_cli_args(std::env::args_os()))?;
    let render_sources = parsed.render_sources();
    match parsed.cli.command {
        Commands::Render(ref args) => cmd_render(args, &parsed.cli, &render_sources),
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

    let parsed = ParsedCli::parse_from(normalize_cli_args(std::env::args_os()))?;
    let mut cli = parsed.cli.clone();

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
    let effective_loglevel = if parsed.is_explicit("loglevel") {
        cli.loglevel
    } else {
        global_cfg
            .loglevel
            .as_deref()
            .and_then(|s| s.parse::<LogLevel>().ok())
            .unwrap_or(cli.loglevel)
    };

    // Resolve effective log_format.
    let effective_log_format = if parsed.is_explicit("log_format") {
        cli.log_format
    } else {
        global_cfg
            .log_format
            .as_deref()
            .and_then(|s| s.parse::<LogFormat>().ok())
            .unwrap_or(cli.log_format)
    };

    // Resolve effective strict: --strict → true, --no-strict → false, else config.
    let effective_strict = if parsed.is_explicit("strict") {
        true
    } else if parsed.is_explicit("no_strict") {
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
        Commands::Render(ref args) => cmd_render(args, &cli, &parsed.render_sources())?,
        Commands::InputLive(_) => {
            anyhow::bail!("The 'input-live' command is defined but not implemented yet.")
        }
        #[cfg(feature = "saf_vbap")]
        Commands::GenerateVbap(ref args) => cmd_generate_vbap(args)?,
        #[cfg(target_os = "windows")]
        Commands::ListAsioDevices => cmd_list_asio_devices()?,
    }

    Ok(())
}
