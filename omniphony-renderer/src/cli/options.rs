//! The `render` flags of the registered options, generated from the
//! registries: the core's (`renderer::options::LIVE_OPTIONS`) and the
//! standalone host's (`host_audio::options::HOST_OPTIONS`).
//!
//! Each option is `--<key>` (its key with `-` for `_`), or the pair
//! `--<key>` / `--no-<key>` for a boolean. A value given on the command line
//! goes into the run's config through the option's own row, as a save of
//! the same live change would write it, so it is validated and bounded
//! exactly as an OSC write, and `--save-config` keeps it.

use clap::{Arg, ArgAction, ArgMatches, builder::PossibleValuesParser, parser::ValueSource};
use renderer::config::RenderConfig;
use renderer::options::{
    LIVE_OPTIONS, OptionDefault, OptionEnv, OptionFlags, OptionKind, RawOptionValue,
};

/// Registered options left out of the generated flags.
const NOT_GENERATED: &[&str] = &[
    // The flag takes decibels, as the config stores them; the option's wire
    // value is linear (`--master-gain -6` would mean silence).
    "master_gain",
    // Not a tuning lever of the controller as it stands.
    "adaptive_resampling_integral_discharge_ratio",
];

/// One registered option, as the command line sees it.
struct FlagSpec {
    key: &'static str,
    kind: OptionKind,
    default: OptionDefault,
    heading: &'static str,
}

fn flag_specs() -> impl Iterator<Item = FlagSpec> {
    // The standalone renderer has audio I/O of its own: an option only the
    // embedded engine offers is no flag of it.
    let core = LIVE_OPTIONS
        .iter()
        .filter(|spec| !spec.flags.contains(OptionFlags::EMBEDDED_ONLY))
        .map(|spec| FlagSpec {
            key: spec.key,
            kind: spec.kind,
            default: spec.default,
            heading: "Live options",
        });
    let host = host_audio::options::HOST_OPTIONS
        .iter()
        .map(|spec| FlagSpec {
            key: spec.key,
            kind: spec.kind,
            default: spec.default,
            heading: "Audio output and input options",
        });
    core.chain(host)
        .filter(|spec| !NOT_GENERATED.contains(&spec.key))
}

fn leak(s: String) -> &'static str {
    // Built once per process, at argument parsing.
    Box::leak(s.into_boxed_str())
}

fn long(key: &str) -> &'static str {
    leak(key.replace('_', "-"))
}

fn negation_id(key: &str) -> &'static str {
    leak(format!("no_{key}"))
}

fn help(spec: &FlagSpec) -> String {
    let default = match spec.default {
        OptionDefault::Build => "as built".to_string(),
        OptionDefault::Unset => "unset".to_string(),
        default => default.to_json().to_string(),
    };
    match spec.kind {
        // clap lists the values.
        OptionKind::Enum(_) => format!("Default {default}."),
        kind => format!(
            "{}; default {default}.",
            renderer::options::doc_table::kind_cell(kind)
        ),
    }
}

/// The generated flags of the `render` subcommand.
pub fn option_args() -> Vec<Arg> {
    let mut args = Vec::new();
    for spec in flag_specs() {
        let arg = Arg::new(spec.key)
            .long(long(spec.key))
            .help(help(&spec))
            .help_heading(spec.heading);
        match spec.kind {
            OptionKind::Bool => {
                let no = negation_id(spec.key);
                args.push(arg.action(ArgAction::SetTrue).conflicts_with(no));
                args.push(
                    Arg::new(no)
                        .long(leak(format!("no-{}", long(spec.key))))
                        .action(ArgAction::SetTrue)
                        .help(format!("Set `{}` to false.", spec.key))
                        .help_heading(spec.heading)
                        .conflicts_with(spec.key),
                );
            }
            OptionKind::Enum(values) => {
                args.push(
                    arg.value_name("VALUE")
                        .value_parser(PossibleValuesParser::new(values.iter().copied())),
                );
            }
            OptionKind::Float { .. } | OptionKind::Int { .. } => {
                args.push(
                    arg.value_name("NUMBER")
                        .allow_hyphen_values(true)
                        .value_parser(clap::value_parser!(f64)),
                );
            }
            OptionKind::OptionalInt { .. } => {
                args.push(
                    arg.value_name("NUMBER|none")
                        .allow_hyphen_values(true)
                        .value_parser(optional_number),
                );
            }
            OptionKind::FloatArray { len, .. } => {
                args.push(
                    arg.value_name(leak(vec!["N"; len].join(",")))
                        .allow_hyphen_values(true)
                        .value_parser(number_list),
                );
            }
            OptionKind::Str | OptionKind::DynamicEnum { .. } => {
                args.push(arg.value_name("VALUE"));
            }
        }
    }
    args
}

fn optional_number(value: &str) -> Result<Option<f64>, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "none" | "auto" => Ok(None),
        number => number
            .parse()
            .map(Some)
            .map_err(|_| format!("`{value}` is not a number (or `none`)")),
    }
}

fn number_list(value: &str) -> Result<Vec<f64>, String> {
    value
        .split(',')
        .map(|part| {
            part.trim()
                .parse()
                .map_err(|_| format!("`{part}` is not a number"))
        })
        .collect()
}

/// A value given for a generated flag.
#[derive(Debug, Clone, PartialEq)]
pub enum CliValue {
    Bool(bool),
    Number(f64),
    Numbers(Vec<f64>),
    Str(String),
    Null,
}

impl CliValue {
    fn raw(&self) -> RawOptionValue<'_> {
        match self {
            Self::Bool(b) => RawOptionValue::Bool(*b),
            Self::Number(n) => RawOptionValue::Number(*n),
            Self::Numbers(ns) => RawOptionValue::Numbers(ns),
            Self::Str(s) => RawOptionValue::Str(s),
            Self::Null => RawOptionValue::Null,
        }
    }
}

fn explicit(matches: &ArgMatches, id: &str) -> bool {
    matches
        .value_source(id)
        .is_some_and(|source| matches!(source, ValueSource::CommandLine | ValueSource::EnvVariable))
}

/// The generated flags given on the command line, in registry order.
pub fn given_values(matches: &ArgMatches) -> Vec<(&'static str, CliValue)> {
    let mut values = Vec::new();
    for spec in flag_specs() {
        let value = match spec.kind {
            OptionKind::Bool if explicit(matches, spec.key) => CliValue::Bool(true),
            OptionKind::Bool if explicit(matches, negation_id(spec.key)) => CliValue::Bool(false),
            OptionKind::Bool => continue,
            _ if !explicit(matches, spec.key) => continue,
            OptionKind::Float { .. } | OptionKind::Int { .. } => {
                CliValue::Number(*matches.get_one::<f64>(spec.key).expect("parsed"))
            }
            OptionKind::OptionalInt { .. } => {
                match matches.get_one::<Option<f64>>(spec.key).expect("parsed") {
                    Some(n) => CliValue::Number(*n),
                    None => CliValue::Null,
                }
            }
            OptionKind::FloatArray { .. } => CliValue::Numbers(
                matches
                    .get_one::<Vec<f64>>(spec.key)
                    .expect("parsed")
                    .clone(),
            ),
            OptionKind::Enum(_) | OptionKind::Str | OptionKind::DynamicEnum { .. } => {
                CliValue::Str(matches.get_one::<String>(spec.key).expect("parsed").clone())
            }
        };
        values.push((spec.key, value));
    }
    values
}

/// Write `values` into `render`, each through its row (core or host). An
/// error names every flag whose value its option refused.
pub fn store_given_values(
    render: &mut RenderConfig,
    values: &[(&'static str, CliValue)],
) -> anyhow::Result<()> {
    let host_keys: Vec<&str> = host_audio::options::HOST_OPTIONS
        .iter()
        .map(|spec| spec.key)
        .collect();
    let (host, core): (Vec<_>, Vec<_>) = values
        .iter()
        .map(|(key, value)| (*key, value.raw()))
        .partition(|(key, _)| host_keys.contains(key));
    let env = OptionEnv::detached().with_host_io(true);
    let mut refused = renderer::options::store_client_values(render, &core, &env);
    refused.extend(host_audio::store_host_values(render, &host));
    if refused.is_empty() {
        return Ok(());
    }
    let flags: Vec<String> = refused
        .iter()
        .map(|key| {
            let value = values
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| format!("{v:?}"))
                .unwrap_or_default();
            format!("--{} {value}", long(key))
        })
        .collect();
    anyhow::bail!("invalid option value(s): {}", flags.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::command::{Cli, ParsedCli};
    use clap::CommandFactory;

    #[test]
    fn the_generated_flags_sit_beside_the_declared_ones_without_a_clash() {
        Cli::command()
            .mut_subcommand("render", |render| render.args(option_args()))
            .debug_assert();
    }

    /// Free-form values a test can give (a string kind has no other value to
    /// derive).
    fn string_sample(key: &str) -> &'static str {
        match key {
            "object_generator_id" => "copy_up",
            "drc_mode" => "Light",
            "hrir_source" => "sofa:/tmp/hrtf.sofa",
            "head_tracking_osc_address" => "/head/quat",
            "render_backend" | "hybrid_external_backend" | "hybrid_internal_backend" => {
                "barycenter"
            }
            "output_device" => "studio_monitors",
            "output_backend" => "file",
            "output_file" => "/tmp/render.caf",
            "output_file_format" => "caf",
            "live_input_node" => "omniphony-in",
            "live_input_description" => "Omniphony input",
            "live_input_backend" => "pipewire",
            "live_input_layout" => "7.1.4",
            // Its only value so far.
            "live_input_map" => "7.1-fixed",
            other => panic!("no command-line sample for string option `{other}`"),
        }
    }

    /// Command-line words setting `spec` to a value other than its default.
    fn sample_words(spec: &FlagSpec) -> Vec<String> {
        let flag = format!("--{}", long(spec.key));
        match (spec.kind, spec.default) {
            (OptionKind::Bool, OptionDefault::Bool(true)) => {
                vec![format!("--no-{}", long(spec.key))]
            }
            (OptionKind::Bool, _) => vec![flag],
            (OptionKind::Enum(names), default) => {
                let default = match default {
                    OptionDefault::Str(s) => s,
                    _ => "",
                };
                let other = names
                    .iter()
                    .find(|name| **name != default)
                    .expect("2 values");
                vec![flag, other.to_string()]
            }
            (OptionKind::Float { min, max, step }, default) => {
                let base = match default {
                    OptionDefault::Float(f) => f as f64,
                    _ => min as f64,
                };
                let up = base + step.max(0.01) as f64;
                let value = if up <= max as f64 {
                    up
                } else {
                    base - step as f64
                };
                vec![flag, value.to_string()]
            }
            (OptionKind::Int { min, max }, default) => {
                let base = match default {
                    OptionDefault::Int(i) => i,
                    _ => min,
                };
                // Halved where it can be: a polar grid size is quantized to
                // whole degrees, which 360 → 180 cells survives.
                let value = if base >= 2 * min.max(1) {
                    base / 2
                } else if base < max {
                    base + 1
                } else {
                    base - 1
                };
                vec![flag, value.max(min).to_string()]
            }
            (OptionKind::OptionalInt { min, max }, _) => {
                vec![flag, ((min + max.min(min + 1000)) / 2).max(min).to_string()]
            }
            (OptionKind::FloatArray { len, min, max, .. }, default) => {
                let mid = ((min + max.min(4.0)) / 2.0) as f64;
                let values: Vec<String> = match default {
                    OptionDefault::FloatArray(d) => {
                        d.iter().map(|v| (*v as f64 + 0.25).to_string()).collect()
                    }
                    _ => vec![mid.to_string(); len],
                };
                vec![flag, values.join(",")]
            }
            (OptionKind::DynamicEnum { .. }, default) => {
                let other = if default == OptionDefault::Str("barycenter") {
                    "vbap"
                } else {
                    "barycenter"
                };
                vec![flag, other.to_string()]
            }
            (OptionKind::Str, _) => vec![flag, string_sample(spec.key).to_string()],
        }
    }

    /// Every generated flag parses, is accepted by its option and lands in
    /// the config; a core option's value reads back from it unchanged.
    #[test]
    fn every_generated_flag_reaches_the_config_through_its_row() {
        let env = OptionEnv::detached().with_host_io(true);
        for spec in flag_specs() {
            let words = sample_words(&spec);
            let argv = ["orender", "render"]
                .into_iter()
                .map(String::from)
                .chain(words.clone());
            let parsed = ParsedCli::parse_from(argv)
                .unwrap_or_else(|e| panic!("{}: {words:?} does not parse: {e}", spec.key));
            let values = parsed.render_sources().option_values();
            assert_eq!(values.len(), 1, "{}: {values:?}", spec.key);
            let mut render = RenderConfig::default();
            store_given_values(&mut render, &values)
                .unwrap_or_else(|e| panic!("{}: {words:?} refused: {e}", spec.key));
            // An option with a single value has no other to write.
            let only_value = matches!(
                (spec.kind, spec.default),
                (OptionKind::Str, OptionDefault::Str(d)) if words.get(1).is_some_and(|w| w == d)
            );
            assert!(
                only_value || format!("{render:?}") != format!("{:?}", RenderConfig::default()),
                "{}: {words:?} wrote nothing",
                spec.key
            );
            if let Some(row) = renderer::options::find(spec.key) {
                let mut direct = renderer::live_params::LiveParams::default();
                renderer::options::reset_live_to_defaults(&mut direct, &env);
                (row.set)(&mut direct, &values[0].1.raw(), &env).expect("accepted");
                let mut seeded = renderer::live_params::LiveParams::default();
                renderer::options::reset_live_to_defaults(&mut seeded, &env);
                renderer::options::seed_live_from_config(&mut seeded, &render, &env);
                assert_eq!(
                    (row.get_json)(&seeded),
                    (row.get_json)(&direct),
                    "{}: {words:?} did not read back",
                    spec.key
                );
            }
        }
    }

    #[test]
    fn a_value_its_option_refuses_fails_the_run_naming_the_flag() {
        let parsed =
            ParsedCli::parse_from(["orender", "render", "--render-backend", "no_such_backend"])
                .expect("a string parses");
        let err = store_given_values(
            &mut RenderConfig::default(),
            &parsed.render_sources().option_values(),
        )
        .expect_err("refused");
        assert!(err.to_string().contains("--render-backend"), "{err}");
    }

    #[test]
    fn the_master_gain_flag_stays_in_decibels() {
        assert!(flag_specs().all(|spec| spec.key != "master_gain"));
    }
}
