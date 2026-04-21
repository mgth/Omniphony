fn main() {
    println!("cargo:rustc-check-cfg=cfg(pipewire_1_0)");
    // Detect the installed PipeWire version and emit a cfg flag when it is >= 1.0.
    // PipeWire 1.0 removed several SPA/PW constants that the 0.3.x bindings expose,
    // so call sites that use those constants must be conditionally compiled out.
    if let Ok(lib) = pkg_config::probe_library("libpipewire-0.3") {
        let parts: Vec<u32> = lib
            .version
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect();
        if parts.first().copied().unwrap_or(0) >= 1 {
            println!("cargo:rustc-cfg=pipewire_1_0");
        }
    }
}
