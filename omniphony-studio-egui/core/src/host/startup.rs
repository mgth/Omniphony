//! Startup policy shared independently of the window toolkit.
use super::config::OscConfig;
use std::path::PathBuf;

pub struct Startup {
    pub target: Option<(String, u16)>,
    pub listen_port: u16,
    pub passive: bool,
}
impl Startup {
    pub fn new(
        config: &OscConfig,
        register: Option<&str>,
        listen_port: Option<u16>,
        listen_only: bool,
        synthetic: bool,
    ) -> Result<Self, String> {
        if register.is_some() && (listen_only || synthetic) {
            return Err("--register cannot be combined with listen-only or synthetic mode".into());
        }
        let passive = listen_only || synthetic;
        let target = if passive {
            None
        } else if let Some(spec) = register {
            let (host, port) = spec.rsplit_once(':').ok_or("expected host:port")?;
            let host = host.trim().trim_matches(['[', ']']);
            if host.is_empty() {
                return Err("renderer host cannot be empty".into());
            }
            Some((
                host.to_owned(),
                port.parse().map_err(|_| "invalid renderer port")?,
            ))
        } else {
            Some((config.host.clone(), config.osc_rx_port))
        };
        Ok(Self {
            target,
            listen_port: listen_port.unwrap_or(if passive { 0 } else { config.osc_port }),
            passive,
        })
    }
}

/// Use the same default config namespace as the Tauri host. An environment
/// override intentionally selects a separate workflow namespace.
pub fn config_dir() -> Result<PathBuf, String> {
    if let Some(root) = super::runtime_env::config_dir() {
        return Ok(root.join("studio"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home.map(|home| home.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home.map(|home| home.join(".config")))
    };
    base.map(|base| base.join("fr.omniphony.studio"))
        .ok_or_else(|| "cannot determine the Studio configuration directory".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_target_and_port_are_defaults_with_explicit_override() {
        let config = OscConfig {
            host: "saved.example".into(),
            osc_rx_port: 9012,
            osc_port: 9013,
            ..Default::default()
        };
        let plan = Startup::new(&config, None, None, false, false).unwrap();
        assert_eq!(plan.target, Some(("saved.example".into(), 9012)));
        assert_eq!(plan.listen_port, 9013);
        let plan = Startup::new(&config, Some("[::1]:9014"), Some(0), false, false).unwrap();
        assert_eq!(plan.target, Some(("::1".into(), 9014)));
        assert_eq!(plan.listen_port, 0);
    }
    #[test]
    fn passive_modes_never_attach_to_the_saved_renderer() {
        for (listen_only, synthetic) in [(true, false), (false, true)] {
            let plan =
                Startup::new(&OscConfig::default(), None, None, listen_only, synthetic).unwrap();
            assert!(plan.passive && plan.target.is_none());
            assert_eq!(plan.listen_port, 0);
            assert!(
                Startup::new(
                    &OscConfig::default(),
                    Some("127.0.0.1:9000"),
                    None,
                    listen_only,
                    synthetic
                )
                .is_err()
            );
        }
    }
}
