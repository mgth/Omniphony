//! What `orender` says it pairs with, and `orender check-bridges`: the
//! decoder bridges loaded the way `render` loads them, without a renderer
//! (`docs/installer.md`, step 4).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The reference bridge cdylib, which the dev-dependency on
/// `reference_bridge` builds into `deps/` beside the binary.
fn reference_bridge_path() -> PathBuf {
    let dir = Path::new(env!("CARGO_BIN_EXE_orender"))
        .parent()
        .expect("binary directory");
    let name = format!(
        "{}reference_bridge{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    [dir.join("deps").join(&name), dir.join(&name)]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("reference bridge {name} not built in {}", dir.display()))
}

/// `orender <args>` with an empty config of its own, so that neither the
/// config nor the bridge variables of the machine running the test are read.
fn orender(name: &str, args: &[&str]) -> Output {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let config = work.join("config.yaml");
    std::fs::write(&config, "").unwrap();
    Command::new(env!("CARGO_BIN_EXE_orender"))
        .arg("--config")
        .arg(&config)
        .args(args)
        .env_remove("ORENDER_BRIDGE_FILE")
        .env_remove("ORENDER_BRIDGE_DIR")
        .output()
        .expect("run orender")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `pub const <name>: u32 = N;` of the liborender source.
fn liborender_constant(name: &str) -> String {
    let prefix = format!("pub const {name}: u32 =");
    include_str!("../orender_ffi/src/lib.rs")
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("{name} not found"))
        .trim()
        .trim_end_matches(';')
        .to_owned()
}

/// `--version` names the liborender ABI of the same tree and the `bridge_api`
/// series a bridge must be built against; `-V` stays one line.
#[test]
fn version_names_the_liborender_abi_and_the_bridge_api_series() {
    let long = stdout(&orender("version_long", &["--version"]));
    let lines: Vec<&str> = long.lines().collect();
    assert_eq!(lines.len(), 3, "{long}");

    let abi = format!(
        "liborender ABI: {}.{}",
        liborender_constant("ORENDER_ABI_MAJOR"),
        liborender_constant("ORENDER_ABI_MINOR")
    );
    assert_eq!(lines[1], abi, "{long}");

    let bridge_api = include_str!("../bridge_api/Cargo.toml")
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .expect("bridge_api version")
        .trim_matches('"');
    let (series, _patch) = bridge_api.rsplit_once('.').expect("major.minor.patch");
    assert_eq!(lines[2], format!("bridge_api: {series}.x"), "{long}");

    let short = stdout(&orender("version_short", &["-V"]));
    assert_eq!(short.lines().collect::<Vec<_>>(), [lines[0]], "{short}");
}

#[test]
fn check_bridges_reports_a_bridge_that_loads() {
    let bridge = reference_bridge_path();
    let output = orender(
        "check_bridges_loads",
        &["check-bridges", "--bridge-path", bridge.to_str().unwrap()],
    );
    let report = stdout(&output);
    assert!(output.status.success(), "{report}");
    assert!(report.starts_with("host: bridge_api "), "{report}");
    assert!(
        report
            .lines()
            .any(|line| line.starts_with(&format!("loaded: {} (", bridge.display()))),
        "{report}"
    );
}

/// No bridge to load is an error, with the reason: a run would have nothing
/// to decode with.
#[test]
fn check_bridges_fails_when_no_bridge_loads() {
    let missing = "/nonexistent/libnone_bridge.so";
    let output = orender(
        "check_bridges_missing",
        &["check-bridges", "--bridge-path", missing],
    );
    assert!(!output.status.success());
    let reason = String::from_utf8_lossy(&output.stderr);
    assert!(reason.contains(missing), "{reason}");

    // A file that is not a bridge is named with why it was refused.
    let not_a_bridge = Path::new(env!("CARGO_TARGET_TMPDIR")).join("libnot_a_bridge.so");
    std::fs::write(&not_a_bridge, b"not a library").unwrap();
    let output = orender(
        "check_bridges_not_a_bridge",
        &[
            "check-bridges",
            "--bridge-path",
            not_a_bridge.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success());
    let reason = String::from_utf8_lossy(&output.stderr);
    assert!(
        reason.contains("no decoder bridge loaded") && reason.contains("libnot_a_bridge.so"),
        "{reason}"
    );
}
