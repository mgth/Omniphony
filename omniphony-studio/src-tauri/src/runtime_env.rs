//! Per-environment overrides for the runtime resources the stack occupies.
//!
//! Studio normally resolves its OSC port, input pipe and config directory from
//! well-known locations that are identical for every checkout of the tree —
//! `app_config_dir()` is keyed by the bundle identifier, and the port and pipe
//! have fixed defaults. That means the second environment to start finds the
//! port already taken and silently attaches to a renderer belonging to the
//! first: the UI looks connected, but every control it sends lands in a build
//! that may not understand it.
//!
//! A launcher sets these to give an environment a private namespace:
//!
//! | variable | overrides |
//! |---|---|
//! | `OMNIPHONY_CONFIG_DIR` | config root; Studio uses the `studio/` subdirectory |
//! | `OMNIPHONY_OSC_PORT` | default OSC listener port |
//! | `OMNIPHONY_INPUT_PIPE` | default renderer input pipe / FIFO |
//!
//! These mirror `renderer::runtime_env` deliberately rather than sharing it:
//! this crate stays free of the DSP workspace (see Cargo.toml), so the variable
//! names are the contract, exactly like the OSC addresses it also spells out.
//! An empty or unparsable value falls back to the built-in default instead of
//! failing, so a launcher may export them unconditionally.

use std::path::PathBuf;

/// Built-in OSC listener port, used when the environment pins nothing. Must
/// match `renderer::config_fields::osc_rx_port::DEFAULT`.
pub const DEFAULT_OSC_RX_PORT: u16 = 9000;

/// Config root this environment is assigned.
pub fn config_dir() -> Option<PathBuf> {
    non_empty_var("OMNIPHONY_CONFIG_DIR").map(PathBuf::from)
}

/// Renderer input pipe this environment is assigned.
pub fn input_pipe() -> Option<PathBuf> {
    non_empty_var("OMNIPHONY_INPUT_PIPE").map(PathBuf::from)
}

/// OSC listener port default: the environment's, else the built-in.
pub fn default_osc_rx_port() -> u16 {
    non_empty_var("OMNIPHONY_OSC_PORT")
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_OSC_RX_PORT)
}

fn non_empty_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env is process-global, so these run under one lock and restore what
    /// they change.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_var<T>(name: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn an_unset_port_leaves_the_built_in_default() {
        with_var("OMNIPHONY_OSC_PORT", None, || {
            assert_eq!(default_osc_rx_port(), DEFAULT_OSC_RX_PORT);
        });
    }

    #[test]
    fn a_port_from_the_environment_becomes_the_default() {
        with_var("OMNIPHONY_OSC_PORT", Some("9012"), || {
            assert_eq!(default_osc_rx_port(), 9012);
        });
    }

    #[test]
    fn a_blank_or_unparsable_value_falls_back() {
        with_var("OMNIPHONY_OSC_PORT", Some("  "), || {
            assert_eq!(default_osc_rx_port(), DEFAULT_OSC_RX_PORT);
        });
        with_var("OMNIPHONY_OSC_PORT", Some("nope"), || {
            assert_eq!(default_osc_rx_port(), DEFAULT_OSC_RX_PORT);
        });
        with_var("OMNIPHONY_CONFIG_DIR", Some(""), || {
            assert_eq!(config_dir(), None);
        });
    }
}
