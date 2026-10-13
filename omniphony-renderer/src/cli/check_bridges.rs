//! `orender check-bridges`: load the decoder bridges the way `render` would,
//! without starting a renderer, and say which loaded and which did not
//! (`docs/installer.md`, step 4).
//!
//! The same resolution and the same load as a run: `--bridge-path` when given,
//! else the config's `render.bridge_path(s)`, else auto-discovery
//! (`orender_engine::bridge_loader::resolve_bridges`). The report goes to
//! stdout; the command fails, with the reason, when no bridge loads, which is
//! when a run would have nothing to decode with.

use anyhow::Result;
use orender_engine::bridge_loader::{host_bridge_api_version, load_bridges, resolve_bridges};

use super::command::{CheckBridgesArgs, Cli};

pub fn cmd_check_bridges(args: &CheckBridgesArgs, cli: &Cli) -> Result<()> {
    let host = host_bridge_api_version();
    println!("host: bridge_api {}.{}.x", host.major, host.minor);

    let config_bridges = if args.bridge_paths.is_empty() {
        cli.config
            .clone()
            .or_else(renderer::config::default_config_path)
            .as_deref()
            .map(renderer::config::Config::load_or_default)
            .and_then(|config| config.render)
            .map(|render| render.bridges())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let loaded = resolve_bridges(&args.bridge_paths, &config_bridges)
        .and_then(|request| load_bridges(&request))?;
    for bridge in loaded.status() {
        match bridge.error {
            None => println!("loaded: {} ({})", bridge.path, bridge.families.join(", ")),
            Some(error) => println!("refused: {}: {error}", bridge.path),
        }
    }
    Ok(())
}
