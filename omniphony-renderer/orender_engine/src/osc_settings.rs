//! Resolution of the OSC server settings, shared by every host.
//!
//! The CLI (`orender render`) and the embedded engine (liborender inside a
//! player) used to resolve these separately and disagreed: a workflow port in
//! `OMNIPHONY_OSC_PORT` turned OSC on in one host only, and `render.osc_metering`
//! was honoured by the other only. Both now call [`OscSettings::resolve`], each
//! passing its own overrides (command-line flags, or the C config struct).
//!
//! The one thing the hosts choose differently is the last resort, when neither
//! they, the config nor the environment decide: the embedded engine turns OSC
//! on when no config file exists, the CLI keeps it off. A player has no
//! terminal to report a failure in and nobody to pass it a flag on a first
//! start, so without OSC Studio could neither see the engine nor show why the
//! bridge did not load. The CLI is typed in a terminal, which shows its errors;
//! Studio launches it with `--osc`; and with OSC on, a CLI whose bridge is
//! missing waits for Studio to set one instead of exiting with the error.

use crate::engine::OscOptions;
use renderer::config::RenderConfig;
use renderer::config_fields;

/// Settings the host itself pins, above the config: explicit CLI flags, or the
/// non-zero fields of the C config struct. `None` defers to the config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OscOverrides {
    pub enabled: Option<bool>,
    pub host: Option<String>,
    pub port_out: Option<u16>,
    pub port_in: Option<u16>,
    pub metering: Option<bool>,
}

/// The resolved OSC server settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscSettings {
    /// Whether the host runs an OSC server at all.
    pub enabled: bool,
    /// Monitoring target host (where outgoing bundles go).
    pub host: String,
    /// Monitoring target port.
    pub port_out: u16,
    /// Registration/listener port.
    pub port_in: u16,
    /// Pre-subscribe the monitoring target to meter bundles, so a headless or
    /// config-driven target receives them without registering first.
    pub metering: bool,
}

impl OscSettings {
    /// Host override → config (`render.osc*`) → environment → built-in default.
    ///
    /// The environment is `OMNIPHONY_OSC_PORT`: a workflow launcher that sets
    /// it assigns this renderer a control port, so it supplies both port
    /// defaults AND turns OSC on when neither the host nor the config decides.
    /// An explicit `render.osc: false` (or `--no-osc`) still wins.
    ///
    /// `default_enabled` is the host's own last resort for OSC being on, when
    /// neither its overrides, the config nor the environment decide (see the
    /// module documentation): the embedded engine passes "no config file
    /// exists", the CLI `false`. A config that exists but has no `osc` key
    /// does not decide either, so it gets this default too: the embedded
    /// engine gives it OSC off, as before, since a save with OSC off leaves
    /// the key out (`osc` is stored skip-if-default).
    pub fn resolve(
        render_cfg: Option<&RenderConfig>,
        overrides: &OscOverrides,
        default_enabled: bool,
    ) -> Self {
        let env_port = renderer::runtime_env::osc_port();
        Self {
            enabled: overrides
                .enabled
                .or_else(|| render_cfg.and_then(config_fields::osc::get))
                .unwrap_or(default_enabled || env_port.is_some()),
            host: overrides
                .host
                .clone()
                .or_else(|| render_cfg.and_then(config_fields::osc_host::get))
                .unwrap_or_else(|| config_fields::osc_host::DEFAULT.to_string()),
            port_out: overrides
                .port_out
                .or_else(|| render_cfg.and_then(config_fields::osc_port::get))
                .unwrap_or_else(renderer::runtime_env::default_osc_port),
            port_in: overrides
                .port_in
                .or_else(|| render_cfg.and_then(config_fields::osc_rx_port::get))
                .unwrap_or_else(renderer::runtime_env::default_osc_rx_port),
            metering: overrides
                .metering
                .or_else(|| render_cfg.and_then(config_fields::osc_metering::get))
                .unwrap_or(config_fields::osc_metering::DEFAULT),
        }
    }

    /// The engine's server options, or `None` when OSC is off.
    pub fn options(&self) -> Option<OscOptions> {
        self.enabled.then(|| OscOptions {
            host: self.host.clone(),
            port_out: self.port_out,
            port_in: self.port_in,
            metering: self.metering,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment is process-global: serialise the tests touching it and
    /// restore the previous value.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env_port<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let name = "OMNIPHONY_OSC_PORT";
        let previous = std::env::var(name).ok();
        match value {
            Some(v) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
        let out = f();
        match previous {
            Some(v) => unsafe { std::env::set_var(name, v) },
            None => unsafe { std::env::remove_var(name) },
        }
        out
    }

    #[test]
    fn defaults_are_off_on_the_built_in_ports() {
        with_env_port(None, || {
            let s = OscSettings::resolve(None, &OscOverrides::default(), false);
            assert!(!s.enabled);
            assert!(!s.metering);
            assert_eq!(s.host, config_fields::osc_host::DEFAULT);
            assert_eq!(s.port_out, config_fields::osc_port::DEFAULT);
            assert_eq!(s.port_in, config_fields::osc_rx_port::DEFAULT);
            assert!(s.options().is_none());
        });
    }

    #[test]
    fn a_workflow_port_turns_osc_on_unless_the_config_says_no() {
        with_env_port(Some("9010"), || {
            let s = OscSettings::resolve(None, &OscOverrides::default(), false);
            assert!(s.enabled);
            assert_eq!((s.port_in, s.port_out), (9010, 9010));

            let off = RenderConfig {
                osc: Some(false),
                ..RenderConfig::default()
            };
            assert!(!OscSettings::resolve(Some(&off), &OscOverrides::default(), false).enabled);
        });
    }

    #[test]
    fn host_overrides_beat_the_config_which_beats_the_environment() {
        with_env_port(Some("9010"), || {
            let cfg = RenderConfig {
                osc: Some(false),
                osc_rx_port: Some(9005),
                osc_metering: Some(true),
                ..RenderConfig::default()
            };
            let s = OscSettings::resolve(Some(&cfg), &OscOverrides::default(), false);
            assert!(!s.enabled);
            assert_eq!(s.port_in, 9005);
            assert_eq!(s.port_out, 9010);
            assert!(s.metering);

            let s = OscSettings::resolve(
                Some(&cfg),
                &OscOverrides {
                    enabled: Some(true),
                    port_in: Some(9020),
                    metering: Some(false),
                    ..OscOverrides::default()
                },
                false,
            );
            assert!(s.enabled);
            assert_eq!(s.port_in, 9020);
            assert!(!s.metering);
            assert!(
                s.options()
                    .is_some_and(|o| o.port_in == 9020 && !o.metering)
            );
        });
    }

    #[test]
    fn the_host_default_applies_only_when_nothing_else_decides() {
        with_env_port(None, || {
            // Nothing decides: the host's default.
            let s = OscSettings::resolve(None, &OscOverrides::default(), true);
            assert!(s.enabled);
            assert_eq!(s.port_in, config_fields::osc_rx_port::DEFAULT);
            assert_eq!(s.port_out, config_fields::osc_port::DEFAULT);
            // A config without the key does not decide either.
            let no_key = RenderConfig::default();
            assert!(OscSettings::resolve(Some(&no_key), &OscOverrides::default(), true).enabled);
            assert!(!OscSettings::resolve(Some(&no_key), &OscOverrides::default(), false).enabled);

            // An explicit `osc: false` and a host override off both win.
            let off = RenderConfig {
                osc: Some(false),
                ..RenderConfig::default()
            };
            assert!(!OscSettings::resolve(Some(&off), &OscOverrides::default(), true).enabled);
            let no_osc = OscOverrides {
                enabled: Some(false),
                ..OscOverrides::default()
            };
            assert!(!OscSettings::resolve(None, &no_osc, true).enabled);
        });
    }
}
