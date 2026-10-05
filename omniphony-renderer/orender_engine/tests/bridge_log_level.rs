//! The engine hands its bridge the host's log level, and the level the
//! `log_level` OSC command sets later, before the next packet: what makes a
//! bridge's debug diagnostics reach the Studio's log view. Its own test binary,
//! as it installs the process-wide live logger.

use std::sync::{Arc, Mutex};

use abi_stable::std_types::{RSlice, RStr, RString, RVec};
use abi_stable::{prefix_type::PrefixTypeTrait, sabi_trait::prelude::TD_Opaque};
use bridge_api::*;
use log::LevelFilter;
use orender_engine::Engine;
use orender_engine::bridge_loader::LoadedBridge;
use orender_engine::decode_step::LogLevelSync;
use orender_engine::renderer_build::{SpatialRendererParams, build_spatial_renderer};
use renderer::speaker_layout::SpeakerLayout;

/// A bridge that decodes nothing and records each `configure` call.
struct ConfigureRecorder {
    configured: Arc<Mutex<Vec<String>>>,
}

impl FormatBridge for ConfigureRecorder {
    fn push_packet(&mut self, _: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RPushResult {
        RPushResult {
            frames: RVec::new(),
            error_message: RString::new(),
            did_reset: false,
        }
    }
    fn reset(&mut self) {}
    fn is_ready(&self) -> bool {
        true
    }
    fn has_objects(&self) -> bool {
        false
    }
    fn configure(&mut self, key: RStr<'_>, value: RStr<'_>) -> bool {
        self.configured
            .lock()
            .unwrap()
            .push(format!("{key}={value}"));
        true
    }
    fn coordinate_format(&self) -> RCoordinateFormat {
        RCoordinateFormat::Cartesian
    }
    fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
        RVbapCartesianDefaults {
            x_size: 3,
            y_size: 3,
            z_size: 3,
            allow_negative_z: false,
        }
    }
    fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
        RVbapTableMode::Cartesian
    }
    fn supported_drc_modes(&self) -> RVec<RString> {
        RVec::new()
    }
    fn set_drc_mode(&mut self, _: RStr<'_>) -> bool {
        false
    }
    fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
        RVec::new()
    }
}

extern "C" fn new_bridge(_: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(
        ConfigureRecorder {
            configured: Arc::default(),
        },
        TD_Opaque,
    )
}
extern "C" fn log_sink(_: usize) {}
extern "C" fn source_families() -> RVec<RSourceFamily> {
    RVec::new()
}

fn engine(configured: &Arc<Mutex<Vec<String>>>) -> Engine {
    let bridge = FormatBridge_TO::from_value(
        ConfigureRecorder {
            configured: Arc::clone(configured),
        },
        TD_Opaque,
    );
    let renderer = build_spatial_renderer(
        &SpatialRendererParams::from_render_config(None),
        SpeakerLayout::preset_stereo().unwrap(),
        48_000,
        bridge.vbap_cartesian_defaults(),
        bridge.preferred_vbap_table_mode(),
        None,
    )
    .unwrap();
    let lib = BridgeLib {
        new_bridge,
        set_host_log_sink: log_sink,
        source_families,
    }
    .leak_into_prefix();
    Engine::new(
        LoadedBridge {
            lib,
            bridge,
            log_level: LogLevelSync::new(),
        },
        renderer,
        48_000,
    )
}

#[test]
fn the_bridge_follows_the_runtime_log_level() {
    orender_engine::init_live_logging(LevelFilter::Info, false).unwrap();
    for decode_thread in [false, true] {
        live_log::set_runtime_level(LevelFilter::Info);
        let configured = Arc::default();
        let mut engine = engine(&configured);
        engine.set_decode_thread(decode_thread).unwrap();
        let packet = |engine: &mut Engine| {
            let chunks = engine.process_raw(&[0]).unwrap();
            engine.recycle(chunks);
        };
        packet(&mut engine);
        packet(&mut engine);
        // What `RuntimeCommand::SetLogLevel` does on `log_level debug`.
        live_log::set_runtime_level(LevelFilter::Debug);
        packet(&mut engine);
        packet(&mut engine);
        live_log::set_runtime_level(LevelFilter::Off);
        packet(&mut engine);
        assert_eq!(
            *configured.lock().unwrap(),
            ["log_level=info", "log_level=debug", "log_level=off"],
            "decode thread {decode_thread}: pushed first, then on changes only"
        );
    }
}
