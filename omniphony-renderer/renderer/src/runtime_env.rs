//! Per-environment overrides for the runtime resources the stack occupies.
//!
//! The renderer, Studio and mpv normally share one set of well-known
//! locations: the OSC port, the input pipe and the config directory. That is
//! right for an installed stack, but it means several checkouts of the tree
//! cannot run side by side — whichever one starts second finds the port already
//! taken and silently attaches to a renderer belonging to the first, which is
//! near-impossible to diagnose from the UI.
//!
//! These variables let a launcher carve out a private namespace per
//! environment. They are read once, at startup, and only supply *defaults*:
//! anything given explicitly on the command line or already stored in the
//! config still wins, so setting them can never override a deliberate choice.
//!
//! | variable | overrides |
//! |---|---|
//! | `OMNIPHONY_CONFIG_DIR` | directory holding `config.yaml` and its live sidecar |
//! | `OMNIPHONY_OSC_PORT` | default OSC target and listener port |
//! | `OMNIPHONY_INPUT_PIPE` | default renderer input pipe / FIFO |
//!
//! An empty or unparsable value is ignored rather than treated as an error: a
//! launcher that exports the variable unconditionally, with nothing to put in
//! it, should get the built-in default instead of a failure.

use std::path::PathBuf;

/// Directory holding `config.yaml`, when the environment pins one.
pub fn config_dir() -> Option<PathBuf> {
    non_empty_var("OMNIPHONY_CONFIG_DIR").map(PathBuf::from)
}

/// OSC port this environment is assigned, used as the default for both the
/// target and the registration listener.
pub fn osc_port() -> Option<u16> {
    non_empty_var("OMNIPHONY_OSC_PORT")?.parse().ok()
}

/// Renderer input pipe this environment is assigned.
pub fn input_pipe() -> Option<PathBuf> {
    non_empty_var("OMNIPHONY_INPUT_PIPE").map(PathBuf::from)
}

/// OSC port default: the environment's, else the built-in.
///
/// Written as a function so it can be used as a clap `default_value_t`, which
/// is evaluated when the command is built rather than baked in at compile time.
pub fn default_osc_port() -> u16 {
    osc_port().unwrap_or(crate::config_fields::osc_port::DEFAULT)
}

/// OSC registration-listener port default: the environment's, else the built-in.
pub fn default_osc_rx_port() -> u16 {
    osc_port().unwrap_or(crate::config_fields::osc_rx_port::DEFAULT)
}

fn non_empty_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Run `f` with the variable `name` set to `value` (unset for `None`), then
/// restore it, even if `f` panics.
///
/// The environment is process-global and this crate's tests run in parallel in
/// one process: every test that changes a variable goes through here, under one
/// lock, so none observes another's half-set value. A second lock elsewhere
/// would not exclude this one.
#[cfg(test)]
pub(crate) fn with_var<T>(name: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Restore<'a> {
        name: &'a str,
        previous: Option<std::ffi::OsString>,
    }
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            // SAFETY: under ENV_LOCK, held by the caller for this guard's life.
            match self.previous.take() {
                Some(v) => unsafe { std::env::set_var(self.name, v) },
                None => unsafe { std::env::remove_var(self.name) },
            }
        }
    }

    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _restore = Restore {
        name,
        previous: std::env::var_os(name),
    };
    // SAFETY: under ENV_LOCK; see above.
    match value {
        Some(v) => unsafe { std::env::set_var(name, v) },
        None => unsafe { std::env::remove_var(name) },
    }
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_variable_leaves_the_built_in_default() {
        with_var("OMNIPHONY_OSC_PORT", None, || {
            assert_eq!(default_osc_port(), crate::config_fields::osc_port::DEFAULT);
        });
    }

    #[test]
    fn a_port_from_the_environment_becomes_the_default() {
        with_var("OMNIPHONY_OSC_PORT", Some("9012"), || {
            assert_eq!(default_osc_port(), 9012);
            assert_eq!(default_osc_rx_port(), 9012);
        });
    }

    #[test]
    fn an_empty_value_is_ignored_rather_than_failing() {
        // A launcher may export the variable with nothing to put in it.
        with_var("OMNIPHONY_OSC_PORT", Some("   "), || {
            assert_eq!(default_osc_port(), crate::config_fields::osc_port::DEFAULT);
        });
        with_var("OMNIPHONY_CONFIG_DIR", Some(""), || {
            assert_eq!(config_dir(), None);
        });
    }

    #[test]
    fn an_unparsable_port_falls_back_instead_of_panicking() {
        with_var("OMNIPHONY_OSC_PORT", Some("not-a-port"), || {
            assert_eq!(default_osc_port(), crate::config_fields::osc_port::DEFAULT);
        });
    }

    #[test]
    fn paths_are_taken_verbatim_once_trimmed() {
        with_var(
            "OMNIPHONY_INPUT_PIPE",
            Some(" /tmp/orender-wf.pipe "),
            || {
                assert_eq!(input_pipe(), Some(PathBuf::from("/tmp/orender-wf.pipe")));
            },
        );
    }
}
