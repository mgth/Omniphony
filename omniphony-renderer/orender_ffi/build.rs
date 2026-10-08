//! Generate the C header (include/orender.h) from the FFI surface via cbindgen,
//! and stamp the shared library's platform version metadata (Linux soname).

use std::path::PathBuf;

/// Parse `pub const ORENDER_ABI_MAJOR: u32 = N;` out of src/lib.rs so the
/// soname has a single source of truth: bumping the const IS the whole bump
/// (`cargo:rerun-if-changed=src/lib.rs` keeps this in sync).
fn abi_major(crate_dir: &str) -> u32 {
    let lib = std::fs::read_to_string(PathBuf::from(crate_dir).join("src/lib.rs"))
        .expect("src/lib.rs must be readable");
    for line in lib.lines() {
        if let Some(value) = line
            .trim()
            .strip_prefix("pub const ORENDER_ABI_MAJOR: u32 =")
        {
            return value
                .trim()
                .trim_end_matches(';')
                .parse()
                .expect("ORENDER_ABI_MAJOR must be a plain integer literal");
        }
    }
    panic!("ORENDER_ABI_MAJOR not found in src/lib.rs");
}

fn main() {
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out = PathBuf::from(&crate_dir).join("include/orender.h");

    println!("cargo:rerun-if-changed=src/lib.rs");
    println!("cargo:rerun-if-changed=cbindgen.toml");
    println!("cargo:rerun-if-env-changed=CI");

    // On Linux, stamp the release cdylib with a SemVer soname
    // (`liborender.so.<ABI major>`) so the packaged library participates in
    // normal shared-object versioning: consumers (mpv) record it as DT_NEEDED
    // or dlopen it by that name, resolved at runtime via the symlinks the
    // PKGBUILD installs. Debug builds skip this so the C smoke test can link
    // and run against the bare `target/debug/liborender.so` without needing
    // the versioned symlinks.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_default();
    if target_os == "linux" && profile == "release" {
        println!(
            "cargo:rustc-cdylib-link-arg=-Wl,-soname,liborender.so.{}",
            abi_major(&crate_dir)
        );
    }
    // On macOS, stamp the release dylib with an `@rpath` install name so a
    // bundled consumer (mpv, Studio) resolves `liborender.dylib` via its own
    // rpath / `@loader_path` rather than the absolute build-tree path that the
    // linker would otherwise record. Debug builds keep the default install
    // name so the C smoke test links against `target/debug/liborender.dylib`
    // directly, mirroring the Linux soname handling above.
    if target_os == "macos" && profile == "release" {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/liborender.dylib");
    }

    // With MSVC, the linker names a program database after its output, so
    // this library (`orender.dll`) and the `orender` executable both write
    // `orender.pdb` into the same `deps/` folder: cargo warns of the collision,
    // and when the two links overlap one fails with LNK1201. Keep this
    // library's database in its own build folder instead.
    //
    // rustc records only the file name in the DLL (`/PDBALTPATH:%_PDB%`), and
    // next to the DLL that name is now the executable's database. A debug
    // build records the full path instead (the later option wins), so a
    // debugger finds this one; a release DLL keeps the bare name, recording
    // no build-machine path, and ships without its database anyway.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
        let pdb = PathBuf::from(out_dir).join("orender.pdb");
        println!("cargo:rustc-cdylib-link-arg=/PDB:{}", pdb.display());
        if profile != "release" {
            println!("cargo:rustc-cdylib-link-arg=/PDBALTPATH:{}", pdb.display());
        }

        // The static C runtime comes from the workspace's .cargo/config.toml,
        // which a RUSTFLAGS variable silently replaces: the player's workflow
        // builds this library with one. Without the flag the DLL needs the
        // Visual C++ Redistributable, and fails to load on a fresh Windows.
        let features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
        if !features.split(',').any(|f| f == "crt-static") {
            println!(
                "cargo:warning=orender.dll is built without the static C runtime and will \
                 need the Visual C++ Redistributable: add `-C target-feature=+crt-static` \
                 to RUSTFLAGS (it replaces omniphony-renderer/.cargo/config.toml)"
            );
        }
    }

    // Load cbindgen.toml explicitly: the library Builder (unlike the cbindgen
    // CLI) does not pick it up on its own, and the export/enum settings there
    // (forced OrenderChannelLabel emission, name-prefixed variants) are part of
    // the ABI surface.
    let cfg = cbindgen::Config::from_file(PathBuf::from(&crate_dir).join("cbindgen.toml"))
        .expect("cbindgen.toml must parse");
    match cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(cfg)
        .generate()
    {
        Ok(bindings) => {
            bindings.write_to_file(&out);
        }
        // Don't fail the cdylib build if header generation hits a snag during
        // development; surface it as a warning instead. In CI it is an error:
        // a stale committed header would otherwise pass the drift check that
        // compares it against this build's output.
        Err(e) if std::env::var_os("CI").is_none() => {
            println!("cargo:warning=cbindgen failed to generate orender.h: {e}");
        }
        Err(e) => panic!("cbindgen failed to generate orender.h: {e}"),
    }
}
