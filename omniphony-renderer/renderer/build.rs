use anyhow::Result;
#[cfg(feature = "saf_vbap")]
use std::env;

fn main() -> Result<()> {
    #[cfg(feature = "saf_vbap")]
    {
        generate_saf_bindings()?;
    }

    #[cfg(not(feature = "saf_vbap"))]
    {
        eprintln!("Note: Building renderer without SAF VBAP support (feature 'saf_vbap' disabled)");
        eprintln!("      Runtime VBAP table generation will not be available.");
        eprintln!("      Use pre-generated .vbap files with --vbap-table option.");
    }

    Ok(())
}

/// Configure SAF library linking for Linux
#[cfg(feature = "saf_vbap")]
fn configure_saf_linux(saf_root: &str) -> Result<bool> {
    let saf_lib_path = format!("{}/build/framework/libsaf.a", saf_root);
    if !std::path::Path::new(&saf_lib_path).exists() {
        anyhow::bail!(
            "SAF library not found at {}.\n\
             Build Spatial_Audio_Framework first or export SAF_ROOT to a valid SAF tree.\n\
             Example:\n\
               cd {} && cmake -S . -B build \\\n\
                 -DSAF_PERFORMANCE_LIB=SAF_USE_OPEN_BLAS_AND_LAPACKE \\\n\
                 -DSAF_BUILD_EXAMPLES=OFF -DBUILD_SHARED_LIBS=OFF \\\n\
                 -DCMAKE_BUILD_TYPE=Release \\\n\
                 -DCMAKE_C_FLAGS=\"-I/usr/include/openblas\" \\\n\
                 -DCMAKE_CXX_FLAGS=\"-I/usr/include/openblas\" && \\\n\
               cmake --build build -j$(nproc)",
            saf_lib_path,
            saf_root
        );
    }

    println!(
        "cargo:rustc-link-search=native={}/build/framework",
        saf_root
    );
    println!("cargo:rustc-link-lib=static=saf");
    println!("cargo:rustc-link-lib=dylib=openblas");
    println!("cargo:rustc-link-lib=dylib=lapacke");

    Ok(true)
}

/// Configure SAF library linking for Windows
#[cfg(feature = "saf_vbap")]
fn configure_saf_windows(saf_root: &str) -> Result<bool> {
    use std::path::{Path, PathBuf};

    let possible_lib_paths = vec![
        format!("{}/build-win/framework/saf.lib", saf_root),
        format!("{}/build-win/framework/Release/saf.lib", saf_root),
        format!("{}/build-win/framework/Debug/saf.lib", saf_root),
        format!("{}/build/framework/Release/saf.lib", saf_root),
        format!("{}/build/framework/Debug/saf.lib", saf_root),
        format!("{}/build/framework/saf.lib", saf_root),
    ];

    let saf_lib_path = possible_lib_paths
        .iter()
        .find(|p| Path::new(p).exists())
        .cloned();

    if saf_lib_path.is_none() {
        let mut msg = String::from("SAF library not found in any of:\n");
        for path in &possible_lib_paths {
            msg.push_str(&format!("  - {}\n", path));
        }
        msg.push_str("\nBuild SAF first or export SAF_ROOT to a valid SAF tree.\n");
        msg.push_str(&format!(
            "Example:\n  cd {} && cmake -S . -B build -G \"Visual Studio 17 2022\" -A x64 ^\n    -DSAF_PERFORMANCE_LIB=SAF_USE_OPEN_BLAS_AND_LAPACKE ^\n    -DSAF_BUILD_EXAMPLES=OFF -DBUILD_SHARED_LIBS=OFF ^\n    -DCMAKE_BUILD_TYPE=Release\n  cmake --build build --config Release",
            saf_root
        ));
        anyhow::bail!(msg);
    }

    let lib_dir = Path::new(saf_lib_path.as_ref().unwrap())
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid SAF library path"))?;

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=saf");

    println!("cargo:rerun-if-env-changed=OPENBLAS_PATH");

    if let Ok(openblas_path) = env::var("OPENBLAS_PATH") {
        let openblas_lib = PathBuf::from(&openblas_path);
        if openblas_lib.join("lib").exists() {
            println!(
                "cargo:rustc-link-search=native={}",
                openblas_lib.join("lib").display()
            );
        } else {
            println!("cargo:rustc-link-search=native={}", openblas_lib.display());
        }
        println!("cargo:rustc-link-lib=static=openblas");
    } else if let Ok(vcpkg_root) = env::var("VCPKG_ROOT") {
        let vcpkg_installed = PathBuf::from(&vcpkg_root)
            .join("installed")
            .join("x64-windows");

        if vcpkg_installed.exists() {
            let vcpkg_lib = vcpkg_installed.join("lib");
            let vcpkg_bin = vcpkg_installed.join("bin");

            println!("cargo:rustc-link-search=native={}", vcpkg_lib.display());
            println!("cargo:rustc-link-search=native={}", vcpkg_bin.display());

            println!("cargo:rustc-link-lib=static=openblas");
            if vcpkg_lib.join("lapacke.lib").exists() {
                println!("cargo:rustc-link-lib=dylib=lapacke");
            } else if vcpkg_lib.join("lapack.lib").exists() {
                println!("cargo:rustc-link-lib=static=lapack");
            }
        }
    } else {
        println!("cargo:rustc-link-lib=static=openblas");
    }

    Ok(true)
}

/// Generate Rust bindings for SAF's `saf_vbap` module.
#[cfg(feature = "saf_vbap")]
fn generate_saf_bindings() -> Result<()> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let saf_root = env::var("SAF_ROOT").unwrap_or_else(|_| {
        let default =
            std::path::Path::new(&manifest_dir).join("../../SPARTA/SDKs/Spatial_Audio_Framework");
        default
            .canonicalize()
            .unwrap_or(default)
            .to_string_lossy()
            .into_owned()
    });
    let saf_root = saf_root.as_str();
    println!("cargo:rerun-if-env-changed=SAF_ROOT");
    let saf_include = format!("{}/framework/include", saf_root);
    let saf_vbap_include = format!("{}/framework/modules/saf_vbap", saf_root);

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "unknown".to_string());

    let saf_available = match target_os.as_str() {
        "windows" => configure_saf_windows(saf_root)?,
        "linux" => configure_saf_linux(saf_root)?,
        other => {
            eprintln!("Warning: Unsupported platform '{}' for SAF.", other);
            return Ok(());
        }
    };

    if !saf_available {
        anyhow::bail!("SAF VBAP support requested but SAF could not be configured");
    }

    println!(
        "cargo:rerun-if-changed={}/framework/modules/saf_vbap/saf_vbap.h",
        saf_root
    );

    let mut builder = bindgen::Builder::default()
        .header(format!("{}/saf_vbap.h", saf_vbap_include))
        .clang_arg(format!("-I{}", saf_include))
        .clang_arg(format!("-I{}", saf_vbap_include))
        .allowlist_function("generateVBAPgainTable3D")
        .allowlist_function("getVBAPgains.*")
        .allowlist_function("vbap.*")
        .layout_tests(false)
        .use_core()
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    match target_os.as_str() {
        "linux" => {
            builder = builder.clang_arg("-I/usr/include/openblas");
        }
        "windows" => {
            if let Ok(vcpkg_root) = env::var("VCPKG_ROOT") {
                use std::path::PathBuf;
                let vcpkg_include = PathBuf::from(&vcpkg_root)
                    .join("installed")
                    .join("x64-windows")
                    .join("include");
                if vcpkg_include.exists() {
                    builder = builder.clang_arg(format!("-I{}", vcpkg_include.display()));
                }
            }
        }
        _ => {}
    }

    if env::var("CARGO_PKG_RUST_VERSION")
        .ok()
        .map_or(false, |v| v >= "1.87.0".to_string())
    {
        builder = builder.wrap_unsafe_ops(true);
    }

    let bindings = builder
        .generate()
        .map_err(|e| anyhow::anyhow!("Failed to generate SAF bindings: {}", e))?;

    let out_path = std::path::PathBuf::from(env::var("OUT_DIR")?);
    bindings
        .write_to_file(out_path.join("saf_vbap_bindings.rs"))
        .map_err(|e| anyhow::anyhow!("Failed to write SAF bindings: {}", e))?;

    println!("cargo:info=SAF VBAP bindings generated successfully");
    Ok(())
}
