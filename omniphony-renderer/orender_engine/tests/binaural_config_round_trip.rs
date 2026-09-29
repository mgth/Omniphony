//! `render.binaural` through a boot and a full Save: what the boot seed reads
//! from the file must come back out of `store_live_into_config`, and keys this
//! version does not know must survive the save untouched.

use orender_engine::renderer_build::{
    SpatialRendererParams, build_spatial_renderer, seed_runtime_state_from_render_config,
};
use renderer::config::Config;
use renderer::live_params::HrirUpdateLattice;
use renderer::speaker_layout::SpeakerLayout;

const CONFIG: &str = "\
render:
  binaural:
    output_mode: binaural
    hrir_update_lattice: coarse
    future_binaural_key: 1
    head_tracking:
      osc_address: /android/rotationvector
      smoothing: 0.6
      invert: true
      future_tracking_key: kept
    reflections:
      enabled: true
      future_reflections_key: [1, 2]
    reverb:
      enabled: true
      future_reverb_key: {nested: true}
";

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "orender-binaural-round-trip-{}-{name}.yaml",
        std::process::id()
    ))
}

#[test]
fn binaural_section_survives_boot_and_save() {
    let path = temp_path("config");
    std::fs::write(&path, CONFIG).expect("write config");
    let mut config = Config::load(&path).expect("parse config");
    let render_cfg = config.render.clone().expect("render section");

    // The boot path both hosts share (CLI bootstrap and `Engine::from_paths`).
    let renderer = build_spatial_renderer(
        &SpatialRendererParams::from_render_config(Some(&render_cfg)),
        SpeakerLayout::preset("7.1.4").expect("preset layout"),
        48_000,
        bridge_api::RVbapCartesianDefaults {
            x_size: 9,
            y_size: 9,
            z_size: 5,
            allow_negative_z: true,
        },
        bridge_api::RVbapTableMode::Cartesian,
        Some(&render_cfg),
    )
    .expect("renderer");
    let control = renderer.renderer_control();
    seed_runtime_state_from_render_config(&control, Some(&render_cfg));
    {
        let live = control.live.read();
        assert_eq!(live.binaural.tracking.smoothing, 0.6);
        assert!(live.binaural.tracking.invert);
        assert_eq!(live.binaural.hrir_update_lattice, HrirUpdateLattice::Coarse);
    }

    runtime_control::persist::store_live_into_config(&control, None, &mut config);
    config.save(&path).expect("save config");
    let saved = std::fs::read_to_string(&path).expect("read saved config");
    let back = Config::load(&path).expect("reload saved config");
    let _ = std::fs::remove_file(&path);

    for key in [
        "future_binaural_key",
        "future_tracking_key",
        "future_reflections_key",
        "future_reverb_key",
    ] {
        assert!(saved.contains(key), "{key} erased by the save:\n{saved}");
    }
    let bin = back
        .render
        .and_then(|r| r.binaural)
        .expect("binaural section saved");
    assert_eq!(bin.hrir_update_lattice.as_deref(), Some("coarse"));
    // Each unknown key back in the section it came from.
    assert!(bin.extra.contains_key("future_binaural_key"));
    let reflections = bin.reflections.as_ref().expect("reflections saved");
    assert!(reflections.extra.contains_key("future_reflections_key"));
    let reverb = bin.reverb.as_ref().expect("reverb saved");
    assert!(reverb.extra.contains_key("future_reverb_key"));
    let ht = bin.head_tracking.expect("head_tracking saved");
    assert!(ht.extra.contains_key("future_tracking_key"));
    assert_eq!(ht.smoothing, Some(0.6));
    assert_eq!(ht.invert, Some(true));
    assert_eq!(ht.osc_address.as_deref(), Some("/android/rotationvector"));
}
