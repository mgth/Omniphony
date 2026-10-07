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
//! `dwChannelMask` gives each channel its speaker; without one, the channels are
//! read in the same WAVE order, using the standard layout of counts 1/2/6/8/12
//! and a best-effort 7.1.4 prefix for other counts.

#![allow(non_local_definitions)]

mod bridge;
mod logging;
mod wav;

use abi_stable::std_types::{RSlice, RString, RVec};
use abi_stable::{
    export_root_module, prefix_type::PrefixTypeTrait, sabi_trait::prelude::TD_Opaque,
};
use bridge::WavBridge;
use bridge_api::{
    BridgeLib, BridgeLibRef, FormatBridge_TO, FormatBridgeBox, RInputTransport, RProbe,
    RSourceFamily,
};

// `FormatBridge` is used through the proc-macro generated trait object impl.
#[allow(unused_imports)]
use bridge_api::FormatBridge as _FormatBridgeTrait;

/// Plugin entry point: export the root module so the host can load it.
#[export_root_module]
fn get_library() -> BridgeLibRef {
    BridgeLib {
        new_bridge: create_bridge,
        set_host_log_sink,
        source_families,
        probe,
        input_codecs,
    }
    .leak_into_prefix()
}

extern "C" fn create_bridge(strict: bool) -> FormatBridgeBox {
    FormatBridge_TO::from_value(WavBridge::new(strict), TD_Opaque)
}

extern "C" fn set_host_log_sink(sink: usize) {
    logging::register_host_log_sink(sink);
}

/// None of its own: its streams are `pcm`, the renderer's own family.
extern "C" fn source_families() -> RVec<RSourceFamily> {
    RVec::new()
}

/// A WAV stream starts with `RIFF`, its size, then `WAVE`: those 12 bytes
/// are the whole check. IEC 61937 carries no WAV, so a burst is never ours.
extern "C" fn probe(data: RSlice<'_, u8>, transport: RInputTransport, _data_type: u8) -> RProbe {
    match transport {
        RInputTransport::Raw => probe_wav(data.as_slice()),
        RInputTransport::Iec61937 => RProbe::none(data.len() as u32),
    }
}

/// No player names WAV as a codec: a WAV stream is routed by [`probe`].
extern "C" fn input_codecs() -> RVec<RString> {
    RVec::new()
}

/// The earliest RIFF/WAVE header in `data`: claimed when complete, pending
/// when `data` ends inside one that still matches.
fn probe_wav(data: &[u8]) -> RProbe {
    const RIFF: &[u8] = b"RIFF";
    const WAVE: &[u8] = b"WAVE";
    const HEADER: usize = 12;
    for start in 0..data.len() {
        let rest = &data[start..];
        if rest.len() >= HEADER {
            if &rest[..4] == RIFF && &rest[8..HEADER] == WAVE {
                return RProbe::claim(start as u32);
            }
        } else if RIFF.starts_with(&rest[..rest.len().min(4)])
            && (rest.len() <= 8 || WAVE.starts_with(&rest[8..]))
        {
            return RProbe::pending(start as u32, HEADER as u32);
        }
    }
    RProbe::none(data.len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_api::RProbeVerdict;

    const HEADER: &[u8] = b"RIFF\x24\0\0\0WAVEfmt ";

    #[test]
    fn a_complete_header_is_claimed_where_it_starts() {
        assert_eq!(probe_wav(HEADER), RProbe::claim(0));
        let mut late = b"junk".to_vec();
        late.extend_from_slice(HEADER);
        assert_eq!(probe_wav(&late), RProbe::claim(4));
    }

    #[test]
    fn a_header_cut_anywhere_is_pending_never_lost() {
        for cut in 1..12 {
            assert_eq!(
                probe_wav(&HEADER[..cut]),
                RProbe::pending(0, 12),
                "cut after {cut} bytes"
            );
        }
    }

    #[test]
    fn other_bytes_are_ruled_out() {
        let none = probe_wav(b"\x0b\x77 not a wav file");
        assert_eq!(none.verdict, RProbeVerdict::None);
        assert_eq!(
            probe_wav(b"RIFF\0\0\0\0AVI LIST").verdict,
            RProbeVerdict::None
        );
        let iec = probe(RSlice::from_slice(HEADER), RInputTransport::Iec61937, 0x15);
        assert_eq!(iec.verdict, RProbeVerdict::None);
    }
}
