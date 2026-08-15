// The renderer-state `serde_json::json!` literal in `snapshot.rs` is large; raise
// the macro recursion limit so it expands.
#![recursion_limit = "256"]

pub mod command;
pub mod context;
pub mod host_control;
pub mod osc;
pub mod osc_contract;
pub mod persist;
pub mod snapshot;

pub use host_control::HostControlHandler;

/// Build fingerprint of this workspace build (`<git-describe> (built <ts>)`),
/// stamped by this crate's build.rs. Both renderer hosts (the `orender` CLI
/// and the `liborender` cdylib) link this crate, so the string identifies the
/// engine build regardless of packaging.
pub fn build_fingerprint() -> String {
    format!(
        "{} (built {})",
        env!("VERGEN_GIT_DESCRIBE"),
        env!("BUILD_TIMESTAMP"),
    )
}

/// Absolute path of the process serving this engine — the `orender` binary, or
/// the host (mpv) when the engine is embedded through `liborender`.
///
/// Published alongside the build fingerprint because the fingerprint alone
/// cannot separate two builds of the same commit living in different checkouts,
/// which is exactly the case a client hits when it believes it is driving its
/// own renderer but has silently attached to one another environment left
/// running. Empty when the platform will not tell us.
pub fn executable_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_default()
}
