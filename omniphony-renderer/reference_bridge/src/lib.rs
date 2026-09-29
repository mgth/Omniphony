//! Reference decoder bridge for the orender spatial renderer.
//!
//! This plugin reads a multichannel **WAV/PCM** file and presents it to the
//! engine as a channel bed, so the standalone `orender` CLI can spatialise and
//! binaurally render ordinary multichannel audio to headphones with no external
//! player and no proprietary decoder. It implements the [`bridge_api`] plugin
//! ABI and is loaded exactly like any other format bridge (`--bridge-path`).
//!
//! Supported input: RIFF/WAVE with PCM 16/24/32-bit integer or 32-bit float
//! samples (including `WAVE_FORMAT_EXTENSIBLE`). An extensible header's
//! `dwChannelMask` gives each channel its speaker; without one, channel counts
//! 1/2/6/8/12 are mapped to canonical speaker labels and other counts are
//! labelled best-effort.

#![allow(non_local_definitions)]

mod bridge;
mod logging;
mod wav;

use abi_stable::{
    export_root_module, prefix_type::PrefixTypeTrait, sabi_trait::prelude::TD_Opaque,
};
use bridge::WavBridge;
use bridge_api::{BridgeLib, BridgeLibRef, FormatBridge_TO, FormatBridgeBox};

// `FormatBridge` is used through the proc-macro generated trait object impl.
#[allow(unused_imports)]
use bridge_api::FormatBridge as _FormatBridgeTrait;

/// Plugin entry point: export the root module so the host can load it.
#[export_root_module]
fn get_library() -> BridgeLibRef {
    BridgeLib {
        new_bridge: create_bridge,
        set_host_log_sink,
    }
    .leak_into_prefix()
}

extern "C" fn create_bridge(strict: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(WavBridge::new(strict), TD_Opaque)
}

extern "C" fn set_host_log_sink(sink: usize) {
    logging::register_host_log_sink(sink);
}
