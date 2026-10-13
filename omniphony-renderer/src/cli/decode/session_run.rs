//! `orender render`: one run after another of the render host
//! ([`crate::cli::sync_host`]), each resolved from the command line and the
//! config as it stands when the run starts (a restart from the config, a
//! reload, begins another).

use super::config_resolution::{
    apply_explicit_renderer_args, apply_osc_settings, apply_render_cfg_overrides,
    effective_to_config, merge_render_config, resolve_osc_settings,
};
use crate::cli::command::{Cli, OutputBackend, RenderArgSources, RenderArgs};
use anyhow::Result;
use orender_engine::bridge_loader::{load_bridges, resolve_bridges};

/// Everything one render iteration resolves from the command line and the
/// config file before it starts.
struct ResolvedRun {
    config_path: Option<std::path::PathBuf>,
    /// The args with the config folded in (flag > config > default).
    args: RenderArgs,
    /// The config file as this run loaded it (live-handoff sidecar included):
    /// the base `--save-config` writes the flags over.
    config: renderer::config::Config,
    /// A live-handoff sidecar came with it: what runs is unsaved.
    live_restored: bool,
    /// The render section this run uses: the file with the CLI flags applied.
    render_cfg: renderer::config::RenderConfig,
}

fn resolve_effective_decode_args(
    args: &RenderArgs,
    cli: &Cli,
    arg_sources: &RenderArgSources<'_>,
) -> Result<ResolvedRun> {
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    // Sidecar-aware load: when a yielded predecessor handed over unsaved live
    // state, the args fold and the render config below must both see it, or
    // the renderer would start on the base file and silently undo the handoff.
    let (config, live_restored) = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default_with_live)
        .unwrap_or_default();

    // The registered options given as flags go into the render section
    // first, through their rows; the args below are resolved from it.
    let mut render_cfg = config.render.clone().unwrap_or_default();
    crate::cli::options::store_given_values(&mut render_cfg, &arg_sources.option_values())?;

    let mut effective = args.clone();
    merge_render_config(&render_cfg, &mut effective, arg_sources);
    let osc = resolve_osc_settings(config.render.as_ref(), &effective, arg_sources);
    apply_osc_settings(&mut effective, osc);

    apply_render_cfg_overrides(&mut render_cfg, &effective);
    apply_explicit_renderer_args(&mut render_cfg, &effective, arg_sources);
    Ok(ResolvedRun {
        config_path,
        args: effective,
        config,
        live_restored,
        render_cfg,
    })
}

fn maybe_save_effective_config(
    cli: &Cli,
    run: &ResolvedRun,
    arg_sources: &RenderArgSources<'_>,
) -> Result<bool> {
    if !cli.save_config {
        return Ok(false);
    }

    let path = run.config_path.clone().ok_or_else(|| {
        anyhow::anyhow!("Cannot determine config path; use --config to specify one")
    })?;

    // `run.config` reads a file that fails to parse as defaults; writing over
    // it would lose the user's layout and profiles.
    renderer::config::Config::load_for_update(&path)?;
    // Nor over a fixed file when this run restored the live state a previous
    // instance handed over while it ran on that fallback.
    if run.config.live_from_parse_error {
        anyhow::bail!(
            "the live state restored for {} is the built-in defaults a previous instance fell \
             back to; reload the config before saving it",
            path.display()
        );
    }
    let config = effective_to_config(&run.args, arg_sources, cli, Some(&run.config))?;
    config.save(&path)?;
    log::info!("Config written to: {}", path.display());
    Ok(true)
}

/// What the host is given for a `render` run: the source the config's
/// `input_mode` names (the PipeWire sink on orender's clock, or the input
/// pipe followed on its writer's), the latency target, the output device and
/// the OSC server, as this run resolved them.
///
/// The engine is built on the config as this run resolved it, so the flags
/// reach it as they reach the legacy host: the registered options, the
/// renderer flags, the presentation, the speaker layout file and the channel
/// render mode. Not yet: the two CLI-only renderer knobs (`--vbap-table`,
/// `--log-object-positions`).
///
/// The input is the PipeWire sink, or INPUT: a regular file or `-` (standard
/// input), read to its end with no clock, or a pipe. The output is the
/// PipeWire device or the `file` backend; a file out without `--continuous`
/// is an offline render, of INPUT.
fn host_config_from(
    args: &RenderArgs,
    config: &renderer::config::Config,
    live_restored: bool,
    render_cfg: &renderer::config::RenderConfig,
    config_path: Option<&std::path::Path>,
) -> Result<crate::cli::sync_host::HostConfig> {
    use crate::cli::command::OutputFileFormatArg;
    use crate::cli::sync_host::{
        HostConfig, InputRequest, OutputKind, resolve_realign_threshold_ms, resolve_source_clock,
        resolve_target_latency_ms,
    };
    use renderer::config::InputModeConfig;

    let backend = args.output_backend.or_else(OutputBackend::platform_default);
    let output = match backend {
        Some(OutputBackend::File) => OutputKind::File {
            destination: args.output_file.clone(),
            format: match args.output_file_format {
                OutputFileFormatArg::RawF32 => audio_output::FileSinkFormat::RawF32,
                OutputFileFormatArg::Caf => audio_output::FileSinkFormat::Caf,
            },
        },
        Some(OutputBackend::Unsupported) | None => {
            anyhow::bail!(
                "no realtime output backend on this platform: use `--output-backend file`"
            )
        }
        // This platform's device: PipeWire, ASIO or WASAPI, CoreAudio.
        Some(_) => OutputKind::Device,
    };
    // An offline render — a file out, once — reads its INPUT whatever the
    // config's input mode says: the live sink is none of its business.
    let offline = output != OutputKind::Device && !args.continuous;
    // The input as the config states it; the run starts on what that names,
    // and its controls start from it.
    let live = render_cfg.live_input.as_ref();
    let input_request = InputRequest {
        mode: match render_cfg.input_mode_or_default() {
            InputModeConfig::Bridge => audio_input::InputMode::Bridge,
            InputModeConfig::Pipewire => audio_input::InputMode::Pipewire,
        },
        node: live.and_then(|l| l.node.clone()),
        description: live.and_then(|l| l.description.clone()),
        // (Bounded as a client's write of the option is.)
        carrier_channels: live
            .and_then(|l| l.carrier_channels)
            .filter(|channels| *channels >= audio_input::CARRIER_MIN_CHANNELS)
            .map(|channels| channels.min(audio_input::CARRIER_MAX_CHANNELS)),
    };
    let drain_pipe = !args.no_drain_pipe;
    let source = if offline {
        InputRequest {
            mode: audio_input::InputMode::Bridge,
            ..input_request.clone()
        }
    } else {
        input_request.clone()
    }
    .source(args.input.clone(), drain_pipe, None)?;
    // What the engine is built on: this run's render section (the file with
    // the flags folded in) in place of the file's, and the presentation,
    // which the args resolution keeps on its own.
    let mut effective = render_cfg.clone();
    renderer::config_fields::presentation::store(&mut effective, &args.presentation);
    let resolved = renderer::config::Config {
        render: Some(effective),
        ..config.clone()
    };
    Ok(HostConfig {
        config_path: config_path.map(std::path::Path::to_path_buf),
        resolved_config: Some((resolved, live_restored)),
        speaker_layout: args.speaker_layout.clone(),
        channel_render_mode: Some(args.channel_render_mode.into()),
        bridge_paths: args.bridge_paths.clone(),
        source,
        input_request,
        input_path: args.input.clone(),
        drain_pipe,
        output_file: render_cfg.output_file.clone(),
        vbap_table: args.vbap_table.clone(),
        log_object_positions: args.log_object_positions,
        target_latency_ms: resolve_target_latency_ms(None, args.latency_target_ms),
        loop_bandwidth_hz: render_cfg.loop_bandwidth_hz.map(f64::from),
        realign_threshold_ms: resolve_realign_threshold_ms(render_cfg.realign_threshold_ms),
        output_target: args.output_device.clone(),
        // (0 is the option's way of saying unset.)
        output_rate_hz: args
            .output_sample_rate
            .or(render_cfg.output_sample_rate)
            .filter(|hz| *hz > 0),
        output,
        source_clock: resolve_source_clock(render_cfg.source_clock.as_deref()),
        continuous: args.continuous,
        offline,
        quantum: device_quantum(args),
        osc: orender_engine::OscSettings {
            enabled: args.osc,
            host: args.osc_host.clone(),
            port_out: args.osc_port,
            port_in: args.osc_rx_port,
            metering: args.osc_metering,
        },
        seconds: None,
        report_every: 0.0,
    })
}

/// The device cycle a run asks for (frames at 48 kHz): PipeWire's quantum
/// where there is one to ask for.
fn device_quantum(args: &RenderArgs) -> u32 {
    #[cfg(target_os = "linux")]
    let quantum = args.pw_quantum;
    #[cfg(not(target_os = "linux"))]
    let quantum = {
        let _ = args;
        None
    };
    quantum.unwrap_or(1024)
}

/// [`host_config_from`] for a resolved run.
fn host_config(run: &ResolvedRun) -> Result<crate::cli::sync_host::HostConfig> {
    host_config_from(
        &run.args,
        &run.config,
        run.live_restored,
        &run.render_cfg,
        run.config_path.as_deref(),
    )
}

/// Say once, for the life of the process, which keys of the config are no
/// longer used. They are left in the file: nothing here removes them.
fn warn_of_retired_keys(render_cfg: &renderer::config::RenderConfig) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let keys = render_cfg.retired_keys();
    if keys.is_empty() || SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    log::warn!(
        "config: {} key(s) are no longer used and are ignored (the output now holds an \
         end-to-end latency, `latency_target`, instead of tuning a resampler): {}",
        keys.len(),
        keys.join(", ")
    );
}

/// One `render` run. Returns the bridges it ended on, for a restart from the
/// config.
///
/// A run whose decoder bridges cannot be loaded has nothing to decode with.
/// With OSC on it stays up all the same, idle, with the error published: a
/// client can then set a working bridge path and reload. Without OSC there is
/// nobody to tell, and it is an error.
fn run_once(
    run: &ResolvedRun,
    session: &mut crate::cli::sync_host::HostSession,
) -> Result<Vec<std::path::PathBuf>> {
    let config = host_config(run)?;
    warn_of_retired_keys(&run.render_cfg);
    let _shutdown = sys::shutdown::ShutdownHandle::install()?;
    let bridges =
        resolve_bridges(&run.args.bridge_paths, &[]).and_then(|request| load_bridges(&request));
    let ended = match bridges {
        Ok(_) => {
            log::info!("Rendering ({:?})", config.source);
            crate::cli::sync_host::run(&config, session)?
        }
        Err(err) if run.args.osc => crate::cli::sync_host::run_without_bridge(&config, &err)?,
        Err(err) => return Err(err.context("format bridge unavailable")),
    };
    Ok(ended.bridge_paths)
}

/// Pre-flight OSC port negotiation, run BEFORE any config load of a render
/// iteration: a yielded holder writes its live-state sidecar while still
/// holding the port, and everything downstream (args fold, render-config
/// seeding) must see that sidecar. OSC enablement and the RX port are never
/// live-modified, so peeking them from the base config is exact.
fn negotiate_osc_port_if_enabled(args: &RenderArgs, cli: &Cli, arg_sources: &RenderArgSources<'_>) {
    let config_path = cli
        .config
        .clone()
        .or_else(renderer::config::default_config_path);
    let render_cfg = config_path
        .as_deref()
        .map(renderer::config::Config::load_or_default)
        .unwrap_or_default()
        .render;
    // The same resolution the run itself folds into its args.
    let osc = resolve_osc_settings(render_cfg.as_ref(), args, arg_sources);
    if osc.enabled {
        let _ = orender_engine::osc::negotiate_rx_port(osc.port_in);
    }
}

pub fn cmd_render(args: &RenderArgs, cli: &Cli, arg_sources: &RenderArgSources<'_>) -> Result<()> {
    sys::shutdown::set_yieldable(args.osc_yield);
    sys::shutdown::set_restartable(true);
    let mut restart_bridge_path_override: Option<Vec<std::path::PathBuf>> = None;
    let mut session = crate::cli::sync_host::HostSession::default();
    loop {
        negotiate_osc_port_if_enabled(args, cli, arg_sources);
        let mut run = resolve_effective_decode_args(args, cli, arg_sources)?;
        if let Some(bridge_path) = restart_bridge_path_override.take() {
            run.args.bridge_paths = bridge_path;
        }

        if maybe_save_effective_config(cli, &run, arg_sources)? {
            return Ok(());
        }

        let bridge_path_after_run = run_once(&run, &mut session)?;

        if sys::ShutdownHandle::is_restart_from_config_requested() {
            sys::ShutdownHandle::clear_restart_from_config();
            if sys::ShutdownHandle::is_requested() {
                return Ok(());
            }
            restart_bridge_path_override = Some(bridge_path_after_run);
            // reload_config discards live state: forget any consumed handoff
            // overlay so the next iteration re-reads the config from disk. (A
            // restart that keeps the live state wrote a fresh sidecar on the
            // way down, which the next iteration reads before the cache.)
            if let Some(path) = run.config_path.as_deref() {
                renderer::config::clear_live_overlay_cache(path);
            }
            log::info!("Restarting render pipeline from config");
            continue;
        }

        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::command::{Commands, ParsedCli};

    /// A `render` run takes its source from the config's
    /// input mode: the PipeWire sink under the configured name (orender's
    /// clock), or the input pipe (its writer's clock); and its latency,
    /// device and OSC server from what the run resolved.
    #[test]
    fn the_host_is_given_what_the_render_run_resolved() {
        use crate::cli::sync_host::Source;
        let parse = |argv: &[&str]| match ParsedCli::parse_from(argv.iter().copied())
            .expect("parse render args")
            .cli
            .command
        {
            Commands::Render(args) => args,
            _ => unreachable!("render subcommand"),
        };
        let config = |yaml: &str| -> renderer::config::RenderConfig {
            serde_yaml_ng::from_str(yaml).expect("render config")
        };

        let mut args = parse(&[
            "orender",
            "render",
            "in.pipe",
            "--osc",
            "--osc-port",
            "9123",
            "--presentation",
            "3",
            "--speaker-layout",
            "room.yaml",
            "--channel-render-mode",
            "host",
        ]);
        args.latency_target_ms = Some(80);
        args.output_device = Some("dac".into());
        let file = renderer::config::Config::default();
        let sink = host_config_from(
            &args,
            &file,
            false,
            &config(
                "input_mode: pipewire\nlive_input:\n  node: my-sink\nmaster_gain: -6.0\n\
                 loop_bandwidth_hz: 0.01\nrealign_threshold_ms: 35.0\n",
            ),
            None,
        )
        .expect("a PipeWire run");
        assert_eq!(
            sink.source,
            Source::OwnSink {
                node_name: "my-sink".into(),
                description: "Omniphony Bridge Input".into(),
                session_priority: None,
                carrier_channels: None,
            }
        );
        assert_eq!(sink.target_latency_ms, 80.0);
        assert_eq!(sink.output_rate_hz, None);
        // A carrier sink is the config's `live_input.carrier_channels`,
        // bounded as the option is: too few channels to hold a burst's
        // preamble ask for none, too many for the most there are.
        let carrier = |channels: u32| {
            host_config_from(
                &args,
                &file,
                false,
                &config(&format!(
                    "input_mode: pipewire
live_input:
  carrier_channels: {channels}
"
                )),
                None,
            )
            .expect("a carrier sink run")
            .source
        };
        for (asked, got) in [(16, Some(16)), (2, None), (500, Some(64))] {
            assert!(
                matches!(carrier(asked), Source::OwnSink { carrier_channels, .. } if carrier_channels == got),
                "{asked} channels"
            );
        }
        let at_96k = host_config_from(
            &args,
            &file,
            false,
            &config("output_sample_rate: 96000\n"),
            None,
        )
        .expect("a run at a device rate asked for");
        assert_eq!(at_96k.output_rate_hz, Some(96_000));
        assert_eq!(sink.output_target.as_deref(), Some("dac"));
        assert!(sink.osc.enabled);
        assert_eq!(sink.osc.port_out, 9123);
        assert_eq!(sink.loop_bandwidth_hz, Some(f64::from(0.01f32)));
        assert_eq!(sink.realign_threshold_ms, 35.0);
        // The engine is built on this run's render section, flags included:
        // not on a second reading of the file.
        let (resolved, live_restored) = sink.resolved_config.as_ref().expect("resolved");
        assert!(!live_restored);
        let render = resolved.render.as_ref().expect("the run's render section");
        assert_eq!(render.master_gain, Some(-6.0));
        assert_eq!(render.presentation, Some(3));
        assert_eq!(
            sink.speaker_layout.as_deref(),
            Some(std::path::Path::new("room.yaml"))
        );
        assert_eq!(
            sink.channel_render_mode,
            Some(renderer::live_params::ChannelRenderMode::Host)
        );

        // No input mode is the pipe; no latency target is the host's default.
        args.latency_target_ms = None;
        let pipe = host_config_from(&args, &file, false, &config("{}"), None).expect("a pipe run");
        assert_eq!(
            pipe.source,
            Source::Pipe {
                path: "in.pipe".into(),
                drain: true,
            }
        );
        assert_eq!(pipe.target_latency_ms, 150.0);

        // A file out, once: an offline render of INPUT, whatever the input
        // mode says. A path that is a regular file is read to its end.
        use crate::cli::sync_host::OutputKind;
        let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        args.input = Some(input.clone());
        args.output_backend = Some(OutputBackend::File);
        args.output_file = "out.f32".into();
        let offline = host_config_from(
            &args,
            &file,
            false,
            &config("input_mode: pipewire\nsource_clock: none\n"),
            None,
        )
        .expect("an offline run");
        assert_eq!(offline.source, Source::File { path: input });
        assert_eq!(
            offline.output,
            OutputKind::File {
                destination: "out.f32".into(),
                format: audio_output::FileSinkFormat::RawF32,
            }
        );
        assert!(offline.offline && !offline.continuous);
        assert_eq!(
            offline.source_clock,
            audio_output::control::SourceClock::None
        );
        // Standard input is a file too; the same output kept up for more
        // streams is not offline, and takes the configured input.
        args.input = Some("-".into());
        args.continuous = true;
        let live = host_config_from(&args, &file, false, &config("{}"), None).expect("live");
        assert_eq!(live.source, Source::File { path: "-".into() });
        assert!(!live.offline && live.continuous);
    }

    fn published_state(
        control: &std::sync::Arc<renderer::live_params::RendererControl>,
        has_host_audio: bool,
    ) -> std::collections::BTreeMap<String, Vec<rosc::OscType>> {
        let mut state: std::collections::BTreeMap<_, _> =
            runtime_control::snapshot::build_live_state_bundle(
                control,
                has_host_audio,
                has_host_audio,
            )
            .into_iter()
            .filter_map(|packet| match packet {
                rosc::OscPacket::Message(msg) => Some((msg.addr, msg.args)),
                rosc::OscPacket::Bundle(_) => None,
            })
            .collect();
        state.insert(
            "object_generators".into(),
            vec![rosc::OscType::String(control.object_generators_json())],
        );
        state.insert(
            "phantom".into(),
            vec![rosc::OscType::String(control.phantom_json())],
        );
        state
    }

    /// Both hosts come up without a bridge through the same runtime
    /// (`orender_engine::degraded::NoBridgeRuntime`) and publish the same
    /// state — layout, bridge error and path, config path/status/profiles,
    /// seeded runtime state, catalogues — save for what is each host's own:
    /// the embedded host's C-ABI, the CLI's input pipe, audio/input domains
    /// and faster monitoring cadence. The embedded setup is the one liborender
    /// builds (`NoBridgeSetup::embedded`); this host's is the one its run
    /// without a bridge keeps up.
    #[test]
    fn both_hosts_publish_the_same_no_bridge_state() {
        const HOST_SPECIFIC: &[&str] = &[
            runtime_control::osc_contract::STATE_RENDER_ABI,
            runtime_control::osc_contract::STATE_INPUT_PIPE,
            runtime_control::osc_contract::STATE_MONITORING,
            runtime_control::osc_contract::STATE_CAPABILITIES,
        ];
        let dir =
            std::env::temp_dir().join(format!("orender-no-bridge-parity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let config = dir.join("config.yaml");
        // A bridge path of its own (the flag's is unsaved state in both
        // hosts), a seeded field, and a small grid so the renderers build fast.
        std::fs::write(
            &config,
            "render:
  bridge_path: /nonexistent/libconfig_bridge.so
  ramp_mode: sample
  evaluation_cartesian_x_size: 9
  evaluation_cartesian_y_size: 9
  evaluation_cartesian_z_size: 5
",
        )
        .expect("write config");
        let rx_port = std::net::UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("free port")
            .port()
            .to_string();
        // Where the state is broadcast: a socket of the test's own, never the
        // default port (9000, or the shell's OMNIPHONY_OSC_PORT) a live
        // instance may be listening on.
        let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("sink socket");
        let tx_port = sink.local_addr().expect("sink address").port().to_string();
        let bridge = "/nonexistent/libnone_bridge.so";
        let error = format!("bridge path '{bridge}' does not exist");

        let parsed = ParsedCli::parse_from([
            "orender",
            "--config",
            config.to_str().expect("utf-8 path"),
            "render",
            "--output-backend",
            "file",
            "--bridge-path",
            bridge,
            "--osc",
            "--osc-port",
            &tx_port,
            "--osc-rx-port",
            &rx_port,
            "in.thd",
        ])
        .expect("parse render args");
        let Commands::Render(args) = &parsed.cli.command else {
            unreachable!("render subcommand")
        };
        let run = resolve_effective_decode_args(args, &parsed.cli, &parsed.render_sources())
            .expect("resolved");
        let host_config = host_config(&run).expect("a run's config");
        let (runtime, _controls) = crate::cli::sync_host::no_bridge_runtime(&host_config, &error)
            .expect("CLI no-bridge runtime");
        let cli_control = runtime.control();
        let (_renderer, osc) = runtime.into_parts();
        assert!(
            osc.as_ref().is_some_and(|osc| osc.is_listening()),
            "the CLI's no-bridge runtime serves OSC"
        );

        let embedded =
            orender_engine::NoBridgeRuntime::build(orender_engine::NoBridgeSetup::embedded(
                Some(config.clone()),
                renderer::config::Config::load_or_default_with_live(&config)
                    .0
                    .render,
                None,
                vec![bridge.into()],
                48_000,
                error.clone(),
                Some((0, 1)),
            ))
            .expect("embedded no-bridge runtime");
        let embedded_control = embedded.control();

        assert_eq!(cli_control.bridge_error(), Some(error));
        assert_eq!(
            cli_control.bridge_paths(),
            [std::path::PathBuf::from(bridge)]
        );
        let mut cli_state = published_state(&cli_control, true);
        let mut embedded_state = published_state(&embedded_control, false);
        // Both publish the embedded engine's own options (`decode_thread`):
        // the standalone host leaves them inert, and a client sets them there.
        for (host, state) in [("cli", &cli_state), ("embedded", &embedded_state)] {
            match state
                .get(runtime_control::osc_contract::STATE_OPTIONS_SCHEMA)
                .and_then(|args| args.first())
            {
                Some(rosc::OscType::String(json)) => {
                    assert!(json.contains("\"key\":\"decode_thread\""), "{host}: {json}")
                }
                _ => panic!("{host}: no options schema published"),
            }
        }
        for addr in HOST_SPECIFIC {
            assert!(cli_state.remove(*addr).is_some(), "{addr} not published");
            embedded_state.remove(*addr);
        }
        let differing: std::collections::BTreeSet<_> = cli_state
            .keys()
            .chain(embedded_state.keys())
            .filter(|addr| cli_state.get(*addr) != embedded_state.get(*addr))
            .collect();
        assert!(
            differing.is_empty(),
            "the hosts' no-bridge states differ at {differing:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
