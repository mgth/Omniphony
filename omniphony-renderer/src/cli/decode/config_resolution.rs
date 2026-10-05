use crate::cli::command::{
    Cli, LogFormat, LogLevel, OutputBackend, OutputFileFormatArg, RenderArgSources, RenderArgs,
};
use anyhow::Result;
use orender_engine::osc_settings::{OscOverrides, OscSettings};
use orender_engine::renderer_build::{DEFAULT_ROOM_RATIO, SpatialRendererParams};

/// Parse the persisted `output_file_format` string into the CLI enum.
fn parse_output_file_format(s: &str) -> Option<OutputFileFormatArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "caf" => Some(OutputFileFormatArg::Caf),
        "raw_f32" | "rawf32" | "raw" | "f32" => Some(OutputFileFormatArg::RawF32),
        _ => None,
    }
}

/// String form of [`OutputFileFormatArg`] for persistence.
fn output_file_format_str(fmt: OutputFileFormatArg) -> &'static str {
    match fmt {
        OutputFileFormatArg::RawF32 => "raw_f32",
        OutputFileFormatArg::Caf => "caf",
    }
}

/// Apply the override-only render args onto a `RenderConfig`: the backend
/// parameters and the size-to-spread policy, which are not registry options
/// (the registered options' flags are folded by
/// [`crate::cli::options::store_given_values`]). Each arg is `Option`: `Some`
/// overrides, `None` keeps whatever the on-disk config already set. Shared by
/// the save path (`effective_to_config`) and the run.
pub(super) fn apply_render_cfg_overrides(
    render: &mut renderer::config::RenderConfig,
    args: &RenderArgs,
) {
    if let Some(localize) = args.barycenter_localize {
        render.barycenter_localize = Some(localize);
    }
    if let Some(v) = args.experimental_distance_distance_floor {
        render.experimental_distance_distance_floor = Some(v);
    }
    if let Some(v) = args.experimental_distance_min_active_speakers {
        render.experimental_distance_min_active_speakers = Some(v);
    }
    if let Some(v) = args.experimental_distance_max_active_speakers {
        render.experimental_distance_max_active_speakers = Some(v);
    }
    if let Some(v) = args.experimental_distance_position_error_floor {
        render.experimental_distance_position_error_floor = Some(v);
    }
    if let Some(v) = args.experimental_distance_position_error_nearest_scale {
        render.experimental_distance_position_error_nearest_scale = Some(v);
    }
    if let Some(v) = args.experimental_distance_position_error_span_scale {
        render.experimental_distance_position_error_span_scale = Some(v);
    }
    if let Some(mode) = args.size_to_spread_mode {
        render.size_to_spread_mode = Some(mode.into());
    }
}

/// Write the renderer flags given explicitly on the command line that are not
/// registry options (the VBAP build switches and spread keys, the master gain
/// in dB) into a render config, over whatever the file says.
///
/// The CLI's renderer params are [`renderer_params`] of the result, which is
/// [`SpatialRendererParams::from_render_config`] — the resolution the embedded
/// engine runs — so a config key one host honours cannot be missed by the
/// other, nor defaulted differently. Only explicit flags are written: a flag
/// left at its clap default must not mask the file. Values go to the raw
/// fields (never through the skip-if-default `store`), so an explicit default
/// still overrides a non-default file value, and the legacy VBAP spread keys
/// are then migrated into the `vbap` param bag by the config seed like any
/// old config's — last, so they win over a bag entry from the file.
///
/// Shared by the run and by `--save-config`.
pub(super) fn apply_explicit_renderer_args(
    render: &mut renderer::config::RenderConfig,
    args: &RenderArgs,
    sources: &RenderArgSources<'_>,
) {
    let explicit = |id: &str| sources.is_explicit(id);
    if explicit("vbap_allow_negative_z") {
        render.vbap_allow_negative_z = Some(true);
    } else if explicit("no_vbap_allow_negative_z") {
        render.vbap_allow_negative_z = Some(false);
    }
    if explicit("spread_from_distance") {
        render.spread_from_distance = Some(true);
    } else if explicit("no_spread_from_distance") {
        render.spread_from_distance = Some(false);
    }
    if explicit("spread_distance_range") {
        render.spread_distance_range = Some(args.spread_distance_range);
    }
    if explicit("spread_distance_curve") {
        render.spread_distance_curve = Some(args.spread_distance_curve);
    }
    if explicit("vbap_spread_min") {
        render.vbap_spread_min = Some(args.vbap_spread_min);
    }
    if explicit("vbap_spread_max") {
        render.vbap_spread_max = Some(args.vbap_spread_max);
    }
    if explicit("master_gain") {
        render.master_gain = Some(args.master_gain);
    }
}

/// Renderer construction params for a run: the shared config resolution of
/// the effective render config (the file with the explicit flags applied, see
/// [`apply_explicit_renderer_args`]) plus the two CLI-only knobs.
pub(super) fn renderer_params(
    effective: &renderer::config::RenderConfig,
    args: &RenderArgs,
) -> SpatialRendererParams {
    let mut params = SpatialRendererParams::from_render_config(Some(effective));
    params.vbap_table = args.vbap_table.clone();
    params.log_object_positions = args.log_object_positions;
    params
}

/// The host overrides the OSC flags given on the command line express.
fn osc_overrides(args: &RenderArgs, sources: &RenderArgSources<'_>) -> OscOverrides {
    let explicit = |id: &str| sources.is_explicit(id);
    let pair = |on: &str, off: &str| {
        if explicit(on) {
            Some(true)
        } else if explicit(off) {
            Some(false)
        } else {
            None
        }
    };
    OscOverrides {
        enabled: pair("osc", "no_osc"),
        host: explicit("osc_host").then(|| args.osc_host.clone()),
        port_out: explicit("osc_port").then_some(args.osc_port),
        port_in: explicit("osc_rx_port").then_some(args.osc_rx_port),
        metering: pair("osc_metering", "no_osc_metering"),
    }
}

/// Resolve the OSC settings through the resolution shared with the embedded
/// engine ([`OscSettings::resolve`]: flag → config → `OMNIPHONY_OSC_PORT` →
/// default).
pub(super) fn resolve_osc_settings(
    cfg: Option<&renderer::config::RenderConfig>,
    args: &RenderArgs,
    sources: &RenderArgSources<'_>,
) -> OscSettings {
    OscSettings::resolve(cfg, &osc_overrides(args, sources))
}

/// Fold resolved OSC settings into the args the rest of the CLI reads.
pub(super) fn apply_osc_settings(args: &mut RenderArgs, osc: OscSettings) {
    args.osc = osc.enabled;
    args.osc_host = osc.host;
    args.osc_port = osc.port_out;
    args.osc_rx_port = osc.port_in;
    args.osc_metering = osc.metering;
}

pub(super) fn merge_render_config(
    cfg: &renderer::config::RenderConfig,
    args: &mut RenderArgs,
    arg_sources: &RenderArgSources<'_>,
) {
    use std::str::FromStr;

    // --- Option fields: fill only when None ---
    if args.speaker_layout.is_none() {
        args.speaker_layout = cfg.speaker_layout.clone();
    }
    if args.vbap_table.is_none() {
        args.vbap_table = cfg.vbap_table.clone();
    }
    // The registered options' flags are already folded into `cfg`, so the
    // fields below that mirror them are read from it unconditionally.
    args.output_sample_rate = cfg.output_sample_rate;
    if args.bridge_path.is_none() {
        args.bridge_path = cfg.bridge_path.clone();
    }
    // In continuous (studio bridge) mode the config is the source of truth for the
    // input pipe, overriding the positional default the studio passes at launch.
    if args.continuous {
        if let Some(ref input_pipe) = cfg.input_pipe {
            args.input = Some(input_pipe.clone());
        }
    }
    // Note: drc_mode currently doesn't have a CLI arg, it's OSC/config only.
    // --- Fields with defaults: apply config only when value equals the clap default ---
    // (If the user explicitly passes the default value, config is ignored — acceptable edge case.)
    if let Some(ref s) = cfg.output_backend {
        match OutputBackend::from_str(s) {
            Ok(backend) => args.output_backend = Some(backend),
            Err(err) => log::warn!("{err}; using the platform default"),
        }
    }
    if let Some(ref s) = cfg.output_file {
        args.output_file = s.clone();
    }
    if let Some(ref s) = cfg.output_file_format {
        match parse_output_file_format(s) {
            Some(fmt) => args.output_file_format = fmt,
            None => log::warn!("Unknown output file format `{s}`; writing raw_f32"),
        }
    }
    if !arg_sources.is_explicit("presentation") {
        if let Some(p) = renderer::config_fields::presentation::get(cfg) {
            args.presentation = p.to_string();
        }
    }
    if !arg_sources.is_explicit("vbap_spread") {
        if let Some(v) = renderer::config_fields::vbap_spread::get(cfg) {
            args.vbap_spread = v;
        }
    }

    // Platform-specific Option fields
    #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
    {
        args.latency_target_ms = cfg.latency_target;
        args.output_device = cfg.output_device.clone();
    }

    // --- Bool fields: CLI enable/disable flags override config; absent → use config ---
    // enable_vbap
    if !arg_sources.is_explicit("enable_vbap") && !arg_sources.is_explicit("disable_vbap") {
        args.enable_vbap = renderer::config_fields::enable_vbap::get(cfg)
            .unwrap_or(renderer::config_fields::enable_vbap::DEFAULT);
    } else if args.disable_vbap {
        args.enable_vbap = false;
    }
    // continuous
    if !arg_sources.is_explicit("continuous") && !arg_sources.is_explicit("no_continuous") {
        args.continuous = renderer::config_fields::continuous::get(cfg)
            .unwrap_or(renderer::config_fields::continuous::DEFAULT);
    } else if args.no_continuous {
        args.continuous = false;
    }
    // bed_conform
    if !arg_sources.is_explicit("bed_conform") && !arg_sources.is_explicit("no_bed_conform") {
        args.bed_conform = renderer::config_fields::bed_conform::get(cfg)
            .unwrap_or(renderer::config_fields::bed_conform::DEFAULT);
    } else if args.no_bed_conform {
        args.bed_conform = false;
    }
    args.enable_adaptive_resampling = renderer::config_fields::enable_adaptive_resampling::get(cfg)
        .unwrap_or(renderer::config_fields::enable_adaptive_resampling::DEFAULT);
    // The file/FIFO/stdout backend has no device clock to track, so adaptive
    // resampling is meaningless there — force it off (warn if it was asked for).
    if args.output_backend == Some(OutputBackend::File) && args.enable_adaptive_resampling {
        if arg_sources.is_explicit("enable_adaptive_resampling") {
            log::warn!(
                "Adaptive resampling has no effect with --output-backend file; ignoring it."
            );
        }
        args.enable_adaptive_resampling = false;
    }
}

pub(super) fn effective_to_config(
    args: &RenderArgs,
    sources: &RenderArgSources<'_>,
    cli: &Cli,
    existing: Option<&renderer::config::Config>,
) -> Result<renderer::config::Config> {
    use renderer::config::{Config, GlobalConfig};
    use renderer::speaker_layout::SpeakerLayout;

    let existing_render_cfg = existing.and_then(|c| c.render.as_ref());

    let global = GlobalConfig {
        loglevel: if cli.loglevel != LogLevel::default() {
            Some(format!("{:?}", cli.loglevel).to_lowercase())
        } else {
            None
        },
        log_format: if cli.log_format != LogFormat::default() {
            Some(format!("{:?}", cli.log_format).to_lowercase())
        } else {
            None
        },
        extra: Default::default(),
    };

    // Start from the existing config so every field the CLI cannot express
    // (live_input, embedded current_layout, DRC, monitoring cadences, any
    // unknown `extra` keys) is preserved verbatim instead of being erased.
    let mut render = existing_render_cfg.cloned().unwrap_or_default();

    render.input_pipe = if args.continuous {
        args.input.clone()
    } else {
        None
    };
    renderer::config_fields::presentation::store(&mut render, &args.presentation);
    render.bridge_path = args.bridge_path.clone();
    renderer::config_fields::enable_vbap::store(&mut render, args.enable_vbap);
    // Persist the embedded layout instead of a path link. Only override when a
    // layout path is supplied on the CLI; otherwise keep the config's existing
    // embedded `current_layout` (Studio-saved) intact. Before the options
    // below: the room is stored in metres against this layout's radius.
    if let Some(ref layout_path) = args.speaker_layout {
        render.current_layout = Some(SpeakerLayout::from_file(layout_path)?);
        render.speaker_layout = None;
    }
    render.vbap_table = args.vbap_table.clone();
    renderer::config_fields::vbap_spread::store(&mut render, args.vbap_spread);
    // The room was loaded as ratios (metres are derived into them at load);
    // drop the metre fields so the ratios stay authoritative, unless a room
    // flag below stores the room again (in metres, as a live save does).
    render.room_width_m = None;
    render.room_front_m = None;
    render.room_rear_m = None;
    render.room_height_m = None;
    render.room_lower_m = None;
    // The registered options given as flags, through their rows.
    crate::cli::options::store_given_values(&mut render, &sources.option_values())?;
    // The other renderer flags over the file's values — the same effective
    // config the run builds its renderer from.
    apply_explicit_renderer_args(&mut render, args, sources);
    if render.room_ratio.as_deref() == Some(DEFAULT_ROOM_RATIO) {
        render.room_ratio = None;
    }
    // VBAP spread tuning lives in the `vbap` param bag, as the live save
    // (`runtime_control::persist`) writes it; the legacy dedicated keys are
    // only read, as a migration. Move them into the bag here too, in the
    // precedence the config seed replays them with (legacy over bag).
    migrate_legacy_spread_keys(&mut render);
    renderer::config_fields::osc::store(&mut render, args.osc);
    renderer::config_fields::osc_metering::store(&mut render, args.osc_metering);
    renderer::config_fields::osc_rx_port::store(&mut render, args.osc_rx_port);
    renderer::config_fields::osc_host::store(&mut render, &args.osc_host);
    renderer::config_fields::osc_port::store(&mut render, args.osc_port);
    renderer::config_fields::continuous::store(&mut render, args.continuous);
    renderer::config_fields::bed_conform::store(&mut render, args.bed_conform);
    render.channel_render_mode = None;
    // Backend parameters and size-to-spread: override-only fields shared
    // with the runtime path.
    apply_render_cfg_overrides(&mut render, args);

    let global_opt = if global.loglevel.is_none() && global.log_format.is_none() {
        None
    } else {
        Some(global)
    };

    // Preserve everything the CLI does not model at the top level: named
    // profiles (docs/config-profiles.md — wiping them here would destroy
    // every non-active profile on `--save-config`) and unknown keys.
    Ok(Config {
        // A save stamps this build's own.
        schema_version: existing.and_then(|c| c.schema_version),
        global: global_opt,
        render: Some(render),
        active_profile: existing.and_then(|c| c.active_profile.clone()),
        profiles: existing.map(|c| c.profiles.clone()).unwrap_or_default(),
        extra: existing.map(|c| c.extra.clone()).unwrap_or_default(),
        // A sidecar mark, never written to the persistent file.
        live_from_parse_error: false,
    })
}

/// Move the legacy VBAP spread keys (`vbap_spread_min/max`,
/// `spread_from_distance`, `spread_distance_range/curve`) into the `vbap` param
/// bag, where the live save keeps them. A legacy key overrides the bag entry,
/// as when the config seed migrates it at startup.
fn migrate_legacy_spread_keys(render: &mut renderer::config::RenderConfig) {
    use renderer::backend_params::ParamValue;
    let moves = [
        (
            "spread_min",
            render.vbap_spread_min.take().map(ParamValue::Float),
        ),
        (
            "spread_max",
            render.vbap_spread_max.take().map(ParamValue::Float),
        ),
        (
            "spread_from_distance",
            render.spread_from_distance.take().map(ParamValue::Bool),
        ),
        (
            "spread_distance_range",
            render.spread_distance_range.take().map(ParamValue::Float),
        ),
        (
            "spread_distance_curve",
            render.spread_distance_curve.take().map(ParamValue::Float),
        ),
    ];
    for (key, value) in moves {
        if let Some(value) = value {
            render
                .backend_params
                .entry("vbap".to_string())
                .or_default()
                .insert(key.to_string(), value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::effective_to_config;
    use crate::cli::command::{Commands, ParsedCli};

    /// Parse an `orender render …` command line straight through clap, so the
    /// tests exercise the real defaults and value sources rather than a
    /// hand-rolled struct.
    fn render_invocation(extra: &[&str]) -> (ParsedCli, crate::cli::command::RenderArgs) {
        let argv = ["orender", "render"].iter().chain(extra).copied();
        let parsed = ParsedCli::parse_from(argv).expect("parse render args");
        let args = match parsed.cli.command {
            Commands::Render(ref a) => a.clone(),
            _ => unreachable!("explicit render subcommand"),
        };
        (parsed, args)
    }

    /// Build a fully-defaulted `render` arg set + `Cli`.
    fn default_render_invocation() -> (ParsedCli, crate::cli::command::RenderArgs) {
        render_invocation(&[])
    }

    /// Regression guard for the report's §8.2: `--save-config` must NOT erase
    /// fields the CLI cannot express. Previously `effective_to_config` rebuilt
    /// the config from scratch and dropped these; now it mutates the existing
    /// config, so they survive a save with no `--speaker-layout`.
    #[test]
    fn save_config_preserves_cli_unexpressible_fields() {
        let (cli, args) = default_render_invocation();

        let existing = renderer::config::RenderConfig {
            live_input: Some(renderer::config::LiveInputConfig {
                node: Some("omniphony_live".to_string()),
                ..Default::default()
            }),
            input_mode: Some(renderer::config::InputModeConfig::Pipewire),
            current_layout: Some(renderer::speaker_layout::SpeakerLayout {
                radius_m: 1.5,
                speakers: vec![],
            }),
            hybrid_external_backend: Some("cube".to_string()),
            experimental_distance_min_active_speakers: Some(3),
            distance_model_metric: Some("chebyshev".to_string()),
            render_backend: Some("barycenter".to_string()),
            ..Default::default()
        };
        let existing = renderer::config::Config {
            render: Some(existing),
            ..Default::default()
        };

        let out = effective_to_config(&args, &cli.render_sources(), &cli.cli, Some(&existing))
            .expect("build config");
        let render = out.render.expect("render section present");

        assert!(render.live_input.is_some(), "live_input erased");
        assert_eq!(
            render.input_mode,
            Some(renderer::config::InputModeConfig::Pipewire)
        );
        assert_eq!(
            render.current_layout.map(|l| l.radius_m),
            Some(1.5),
            "embedded current_layout erased"
        );
        assert_eq!(render.hybrid_external_backend.as_deref(), Some("cube"));
        assert_eq!(render.experimental_distance_min_active_speakers, Some(3));
        assert_eq!(render.distance_model_metric.as_deref(), Some("chebyshev"));
        assert_eq!(render.render_backend.as_deref(), Some("barycenter"));
    }

    /// `--save-config` must not destroy the named config profiles: they are
    /// typed top-level keys (not `extra`), so the rebuilt Config has to carry
    /// them over explicitly (docs/config-profiles.md).
    #[test]
    fn save_config_preserves_named_profiles() {
        let (cli, args) = default_render_invocation();

        let mut existing = renderer::config::Config {
            render: Some(renderer::config::RenderConfig::default()),
            active_profile: Some("headphones".to_string()),
            ..Default::default()
        };
        existing.profiles.insert(
            "headphones".to_string(),
            renderer::config::RenderConfig::default(),
        );
        existing.profiles.insert(
            "night".to_string(),
            renderer::config::RenderConfig {
                render_backend: Some("barycenter".to_string()),
                ..Default::default()
            },
        );

        let out = effective_to_config(&args, &cli.render_sources(), &cli.cli, Some(&existing))
            .expect("build config");
        assert_eq!(out.active_profile.as_deref(), Some("headphones"));
        assert_eq!(
            out.profiles.keys().cloned().collect::<Vec<_>>(),
            vec!["headphones".to_string(), "night".to_string()]
        );
        assert_eq!(
            out.profiles["night"].render_backend.as_deref(),
            Some("barycenter")
        );
    }

    /// A CLI value still wins and is persisted even when starting from an
    /// existing config.
    #[test]
    fn save_config_persists_pilot_field_from_args() {
        let (cli, args) = render_invocation(&["--vbap-distance-res", "12"]);
        let existing = renderer::config::Config {
            render: Some(renderer::config::RenderConfig {
                vbap_distance_res: Some(4),
                ..Default::default()
            }),
            ..Default::default()
        };

        let out = effective_to_config(&args, &cli.render_sources(), &cli.cli, Some(&existing))
            .expect("build config");
        assert_eq!(out.render.unwrap().vbap_distance_res, Some(12));
    }

    /// A flag left at its default does not mask the file: the saved value is
    /// the file's, not clap's.
    #[test]
    fn save_config_keeps_file_values_the_command_line_does_not_set() {
        let (cli, args) = default_render_invocation();
        let existing = renderer::config::Config {
            render: Some(renderer::config::RenderConfig {
                master_gain: Some(-6.0),
                render_evaluation_mode: Some("precomputed_polar".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let out = effective_to_config(&args, &cli.render_sources(), &cli.cli, Some(&existing))
            .expect("build config");
        let render = out.render.unwrap();
        assert_eq!(render.master_gain, Some(-6.0));
        // Polar is the flag's default, but here it is the file's choice: it
        // must survive the save (it used to be dropped, and the next run then
        // followed the bridge's preferred cartesian table).
        assert_eq!(
            render.render_evaluation_mode.as_deref(),
            Some("precomputed_polar")
        );
    }

    /// The VBAP spread tuning is saved where the live save keeps it — the
    /// `vbap` param bag — never in the legacy dedicated keys the live save
    /// deletes; a legacy key in the file is migrated into the bag on the way.
    #[test]
    fn save_config_writes_spread_tuning_into_the_vbap_bag() {
        use renderer::backend_params::ParamValue;
        let (cli, args) = render_invocation(&["--vbap-spread-min", "0.25"]);
        let existing = renderer::config::Config {
            render: Some(renderer::config::RenderConfig {
                spread_distance_curve: Some(2.0),
                ..Default::default()
            }),
            ..Default::default()
        };

        let out = effective_to_config(&args, &cli.render_sources(), &cli.cli, Some(&existing))
            .expect("build config");
        let render = out.render.unwrap();
        assert_eq!(render.vbap_spread_min, None);
        assert_eq!(render.vbap_spread_max, None);
        assert_eq!(render.spread_from_distance, None);
        assert_eq!(render.spread_distance_range, None);
        assert_eq!(render.spread_distance_curve, None);
        let bag = &render.backend_params["vbap"];
        assert_eq!(bag.get("spread_min"), Some(&ParamValue::Float(0.25)));
        assert_eq!(
            bag.get("spread_distance_curve"),
            Some(&ParamValue::Float(2.0))
        );
        assert_eq!(bag.len(), 2, "only what the file or the flags set: {bag:?}");
    }

    /// An explicit spread flag reaches the renderer even when the file keeps
    /// that setting in the `vbap` param bag: the bag is replayed at startup
    /// and used to override the flag silently.
    #[test]
    fn an_explicit_spread_flag_beats_the_files_param_bag() {
        use renderer::backend_params::ParamValue;
        let (cli, args) = render_invocation(&["--vbap-spread-max", "0.5"]);
        let mut render = renderer::config::RenderConfig::default();
        render
            .backend_params
            .entry("vbap".to_string())
            .or_default()
            .insert("spread_max".to_string(), ParamValue::Float(1.0));
        super::apply_explicit_renderer_args(&mut render, &args, &cli.render_sources());

        let renderer = orender_engine::renderer_build::build_spatial_renderer(
            &super::renderer_params(&render, &args),
            renderer::speaker_layout::SpeakerLayout::preset("7.1.4").expect("preset"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Cartesian,
            Some(&render),
        )
        .expect("renderer");
        let bag = renderer.renderer_control().all_backend_params();
        assert_eq!(bag["vbap"].get("spread_max"), Some(&ParamValue::Float(0.5)));
    }

    /// The CLI's renderer params are the shared config resolution — the call
    /// the embedded engine makes — with the explicit flags over it, and
    /// nothing else: with no flags, the two hosts build the same renderer.
    #[test]
    fn renderer_params_are_the_engine_resolution_plus_explicit_flags() {
        let file = renderer::config::RenderConfig {
            master_gain: Some(-6.0),
            room_ratio: Some("1.0,1.5,0.8".to_string()),
            options: renderer::options::DeclaredOptionsConfig {
                auto_gain: Some(true),
                ..Default::default()
            },
            render_evaluation_mode: Some("precomputed_cartesian".to_string()),
            vbap_distance_model: Some("linear".to_string()),
            ..Default::default()
        };
        let resolve = |extra: &[&str]| {
            let (cli, args) = render_invocation(extra);
            let mut render = file.clone();
            crate::cli::options::store_given_values(
                &mut render,
                &cli.render_sources().option_values(),
            )
            .expect("valid flags");
            super::apply_explicit_renderer_args(&mut render, &args, &cli.render_sources());
            super::renderer_params(&render, &args)
        };

        let engine =
            orender_engine::renderer_build::SpatialRendererParams::from_render_config(Some(&file));
        assert_eq!(format!("{:?}", resolve(&[])), format!("{engine:?}"));

        let cli = resolve(&[
            "--master-gain",
            "0",
            "--no-auto-gain",
            "--render-evaluation-mode",
            "precomputed_polar",
        ]);
        // An explicit default still beats the file.
        assert_eq!(cli.master_gain, 0.0);
        assert!(!cli.auto_gain);
        assert_eq!(
            cli.render_evaluation_mode,
            Some(orender_engine::renderer_build::EvalMode::Polar)
        );
        // Untouched by the flags: the file's.
        assert_eq!(cli.room_ratio, "1.0,1.5,0.8");
        assert_eq!(cli.vbap_distance_model, "linear");
    }

    /// The `file` output backend destination + format survive a save → load
    /// round-trip: `effective_to_config` persists them, `merge_render_config`
    /// reads them back into a fresh (un-overridden) arg set.
    #[test]
    fn file_backend_args_round_trip_through_config() {
        use crate::cli::command::{OutputBackend, OutputFileFormatArg};

        // Save direction.
        let parsed = ParsedCli::parse_from([
            "orender",
            "render",
            "--output-backend",
            "file",
            "--output-file",
            "/tmp/out.caf",
            "--output-file-format",
            "caf",
        ])
        .expect("parse file-backend args");
        let cli = parsed.cli.clone();
        let args = match cli.command {
            Commands::Render(ref a) => a.clone(),
            _ => unreachable!("render subcommand"),
        };
        let out =
            effective_to_config(&args, &parsed.render_sources(), &cli, None).expect("build config");
        let render = out.render.expect("render section present");
        assert_eq!(render.output_backend.as_deref(), Some("file"));
        assert_eq!(render.output_file.as_deref(), Some("/tmp/out.caf"));
        assert_eq!(render.output_file_format.as_deref(), Some("caf"));

        // Load direction: merge into fresh defaults with no CLI overrides.
        let defaults = ParsedCli::parse_from(["orender", "render"]).expect("parse defaults");
        let mut merged = match defaults.cli.command {
            Commands::Render(ref a) => a.clone(),
            _ => unreachable!("render subcommand"),
        };
        super::merge_render_config(&render, &mut merged, &defaults.render_sources());
        assert_eq!(merged.output_backend, Some(OutputBackend::File));
        assert_eq!(merged.output_file, "/tmp/out.caf");
        assert_eq!(merged.output_file_format, OutputFileFormatArg::Caf);
    }
}
