//! A bridge is sent the host's log level once when it is opened, and the
//! [`LogLevelSync`] that comes with it does not send that level again: not to
//! a bridge that took it, nor, ever, to one that refused it. No live logger is
//! installed here, so the host's level stays `info`.

use std::sync::Mutex;

use abi_stable::std_types::{RSlice, RStr, RString, RVec};
use abi_stable::{prefix_type::PrefixTypeTrait, sabi_trait::prelude::TD_Opaque};
use bridge_api::*;
use log::LevelFilter;
use orender_engine::bridge_loader::open_bridge;

/// Every `configure` call, as `key=value`, from both bridges below.
static CONFIGURED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A bridge that decodes nothing and records each `configure` call; it
/// refuses `log_level` like one that predates the key unless `knows_log_level`.
struct ConfigureRecorder {
    knows_log_level: bool,
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
        CONFIGURED.lock().unwrap().push(format!("{key}={value}"));
        self.knows_log_level || key.as_str() != "log_level"
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

extern "C" fn new_current_bridge(_: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(
        ConfigureRecorder {
            knows_log_level: true,
        },
        TD_Opaque,
    )
}
extern "C" fn new_older_bridge(_: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(
        ConfigureRecorder {
            knows_log_level: false,
        },
        TD_Opaque,
    )
}
extern "C" fn log_sink(_: usize) {}
extern "C" fn source_families() -> RVec<RSourceFamily> {
    RVec::new()
}

fn lib(new_bridge: extern "C" fn(bool) -> FormatBridgeBox) -> BridgeLibRef {
    BridgeLib {
        new_bridge,
        set_host_log_sink: log_sink,
        source_families,
    }
    .leak_into_prefix()
}

/// What `bridge` is sent when it is opened and then driven through packets at
/// `info`, `info`, `debug`.
fn configured_over_a_stream(new_bridge: extern "C" fn(bool) -> FormatBridgeBox) -> Vec<String> {
    CONFIGURED.lock().unwrap().clear();
    let (mut bridge, mut log_level) = open_bridge(&lib(new_bridge));
    for level in [LevelFilter::Info, LevelFilter::Info, LevelFilter::Debug] {
        log_level.apply(level, &mut bridge);
    }
    std::mem::take(&mut *CONFIGURED.lock().unwrap())
}

#[test]
fn the_level_a_bridge_is_opened_with_is_sent_once() {
    // One test, as both bridges record into the same list.
    assert_eq!(
        configured_over_a_stream(new_current_bridge),
        ["log_level=info", "log_level=debug"]
    );
    assert_eq!(
        configured_over_a_stream(new_older_bridge),
        ["log_level=info"]
    );
}
