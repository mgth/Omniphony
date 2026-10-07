//! Two bridge files in one process are two bridges.
//!
//! abi_stable's `RootModule::load_from` caches the first root module it loads
//! for the whole process, so a host that loaded one bridge and then another
//! (a config reloaded with a different `bridge_path`, a second engine in the
//! same player) kept decoding with the first. This target loads nothing else,
//! so the first load it makes is the one such a cache would keep.

mod common;

use orender_engine::bridge_loader::load_bridge_library;
use std::path::{Path, PathBuf};

/// `source` copied under `name` in a directory of this run's own. A copy is
/// another file to the dynamic loader, so it maps a second instance of the
/// library, as a second bridge would be.
fn copy_as(source: &Path, dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    std::fs::copy(source, &path).expect("copy the reference bridge");
    path
}

#[test]
fn each_bridge_file_loads_its_own_root_module() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bridge-per-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let source = common::reference_bridge_path();
    let first = copy_as(&source, &dir, "first_bridge");
    let second = copy_as(&source, &dir, "second_bridge");

    let first_lib = load_bridge_library(&first).expect("load the first bridge");
    let second_lib = load_bridge_library(&second).expect("load the second bridge");
    let first_again = load_bridge_library(&first).expect("load the first bridge again");

    // The constructor lives in each mapped library, so its address tells the
    // libraries apart.
    let new_bridge = |lib: &bridge_api::BridgeLibRef| lib.new_bridge() as usize;
    assert_ne!(
        new_bridge(&first_lib),
        new_bridge(&second_lib),
        "the second bridge file must not resolve to the first one's root module"
    );
    assert_eq!(
        new_bridge(&first_lib),
        new_bridge(&first_again),
        "one file opened twice is one library"
    );

    // Each one decodes on its own.
    let a = first_lib.new_bridge()(false);
    let b = second_lib.new_bridge()(false);
    assert!(!a.is_ready() && !b.is_ready());

    let _ = std::fs::remove_dir_all(&dir);
}
