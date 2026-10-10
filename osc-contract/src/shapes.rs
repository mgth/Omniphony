//! The arguments each state address carries, as both ends of the wire check
//! them.
//!
//! Until this table, the shape of a message lived twice: in the engine code
//! that builds it and in the client parser that reads it, with nothing to say
//! the two agree. Now the engine's tests hold what it sends to this table, and
//! Studio's conformance test feeds its parser a message of every shape here, so
//! a state address the parser does not read fails a test instead of being
//! dropped at run time.
//!
//! What it holds is the arguments every sender sends, in order. A few
//! addresses take optional trailing ones (a backend file reply's request id);
//! the notes in `lib.rs` say so. The JSON carried in [`Arg::Json`] arguments is
//! not described here: it is still two structs, one on each side.

use crate::*;

/// One OSC argument, as this contract types it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arg {
    /// OSC `i` (32-bit).
    Int,
    /// OSC `f` (32-bit).
    Float,
    /// OSC `s`, read as text.
    String,
    /// OSC `s` holding a JSON document.
    Json,
    /// OSC `b`.
    Blob,
}

use Arg::*;

/// The arguments of every address in [`ALL_STATE`], in no particular order.
pub const STATE: &[(&str, &[Arg])] = &[
    (STATE_ADAPTIVE_RESAMPLING_BAND, &[String]),
    (STATE_ADAPTIVE_RESAMPLING_STATE, &[String]),
    (STATE_AUDIO, &[Json]),
    // backend, key, name, content; then the request id when one was sent.
    (
        STATE_BACKEND_FILE_CONTENT,
        &[String, String, String, String],
    ),
    // backend, key, message; then the request id when one was sent.
    (STATE_BACKEND_FILE_ERROR, &[String, String, String]),
    (STATE_BACKEND_FILE_LIST, &[String, Json]),
    (STATE_CAPABILITIES, &[Json]),
    (STATE_CLIP, &[Int]),
    (STATE_CONTROL_ERROR, &[String, String, String]),
    (STATE_OBJECT_TEST_CLIP, &[Json]),
    (STATE_OVERLAY, &[Json]),
    (STATE_CONFIG_SAVED, &[Int]),
    (STATE_CONFIG_SAVE_ERROR, &[String]),
    (STATE_CROSSOVER_TIME_MS, &[Float]),
    (STATE_DEBUG_SPEAKER_GAINTABLE_CHUNK, &[Blob]),
    (STATE_DEBUG_SPEAKER_GAINTABLE_META, &[Json]),
    (STATE_DEBUG_SPEAKER_GAINTABLE_UNAVAILABLE, &[Json]),
    (STATE_DEBUG_SPEAKER_GAINTABLE_UPTODATE, &[Int]),
    (STATE_DECODE_TIME_MS, &[Float]),
    (STATE_DIAG_SCHEMA, &[Json]),
    (STATE_DIAG_VALUES, &[Json]),
    (STATE_FRAME_DURATION_MS, &[Float]),
    // generation, full, part, parts.
    (STATE_GENERATION, &[Int, Int, Int, Int]),
    (STATE_INPUT, &[Json]),
    (STATE_INPUT_PIPE, &[String]),
    (STATE_LATENCY, &[Float]),
    (STATE_LATENCY_AVAIL_INPUT, &[Float]),
    (STATE_LATENCY_CONTROL, &[Float]),
    (STATE_LATENCY_DOWNSTREAM, &[Float]),
    (STATE_LATENCY_INSTANT, &[Float]),
    (STATE_LATENCY_OUTPUT_FIFO, &[Float]),
    (STATE_LATENCY_RESAMPLER_PENDING, &[Float]),
    (STATE_LATENCY_SMOOTHED, &[Float]),
    (STATE_LATENCY_TARGET, &[Float]),
    (STATE_LAYOUT, &[Json]),
    (STATE_LOG_LEVEL, &[String]),
    (STATE_LOUDNESS, &[Json]),
    (STATE_MONITORING, &[Json]),
    (STATE_OPTIONS_SCHEMA, &[Json]),
    (STATE_HOST_OPTIONS, &[Json]),
    (STATE_PROFILES, &[Json]),
    (STATE_OSC_DIAG, &[Int]),
    (STATE_OSC_METERING, &[Int]),
    // value, sequence.
    (STATE_REALTIME_MASTER_GAIN, &[Float, Int]),
    // speaker, value, sequence.
    (STATE_REALTIME_SPEAKER_GAIN, &[Int, Float, Int]),
    (STATE_RENDER_ABI, &[String]),
    (STATE_RENDER_BRIDGE_API, &[String]),
    (STATE_RENDER_BRIDGE_ERROR, &[String]),
    (STATE_RENDER_BRIDGE_PATH, &[String]),
    (STATE_RENDER_BRIDGES, &[Json]),
    (STATE_RENDER_CONFIG_PATH, &[String]),
    (STATE_RENDER_CONFIG_STATUS, &[String]),
    (STATE_RENDERER, &[Json]),
    (STATE_RENDER_EVALUATION_CARTESIAN_X_SIZE, &[Int]),
    (STATE_RENDER_EVALUATION_CARTESIAN_Y_SIZE, &[Int]),
    (STATE_RENDER_EVALUATION_CARTESIAN_Z_NEG_SIZE, &[Int]),
    (STATE_RENDER_EVALUATION_CARTESIAN_Z_SIZE, &[Int]),
    (STATE_RENDER_EVALUATION_OBJECT_SIZE_INTERVALS, &[Int]),
    (STATE_RENDER_EVALUATION_POLAR_AZIMUTH_RESOLUTION, &[Int]),
    (STATE_RENDER_EVALUATION_POLAR_DISTANCE_MAX, &[Float]),
    (STATE_RENDER_EVALUATION_POLAR_DISTANCE_RES, &[Int]),
    (STATE_RENDER_EVALUATION_POLAR_ELEVATION_RESOLUTION, &[Int]),
    (STATE_RENDER_EVALUATION_POSITION_INTERPOLATION, &[Int]),
    (STATE_RENDER_TIME_MS, &[Float]),
    (STATE_RENDER_VERSION, &[String]),
    (STATE_RENDER_EXECUTABLE, &[String]),
    (STATE_RESAMPLE_RATIO, &[Float]),
    (STATE_SHUTDOWN, &[String]),
    (STATE_SNAPSHOT_COMPLETE, &[Int]),
    (STATE_SPEAKERS, &[Json]),
    (STATE_SPEAKERS_RECOMPUTE_ERROR, &[String]),
    (STATE_SPEAKERS_RECOMPUTING, &[Int]),
    (STATE_VBAP_ALLOW_NEGATIVE_Z, &[Int]),
    (STATE_WRITE_TIME_MS, &[Float]),
    // w, x, y, z.
    (STATE_HEAD_POSE, &[Float, Float, Float, Float]),
    (STATE_OBJECT_GENERATORS, &[Json]),
    // x, y, z, peak dBFS, RMS dBFS.
    (
        STATE_OBJECT_TEST_POSITION,
        &[Float, Float, Float, Float, Float],
    ),
    (STATE_PHANTOM, &[Json]),
];

/// The arguments `address` carries, when it is a state address.
pub fn state(address: &str) -> Option<&'static [Arg]> {
    STATE
        .iter()
        .find(|(candidate, _)| *candidate == address)
        .map(|(_, args)| *args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Exactly one shape per state address: a new address fails here until
    /// its arguments are written down, which is what makes Studio's
    /// conformance test try it.
    #[test]
    fn every_state_address_has_exactly_one_shape() {
        let mut seen = HashSet::new();
        for (address, _) in STATE {
            assert!(seen.insert(*address), "two shapes for {address}");
            assert!(ALL_STATE.contains(address), "{address} is not in ALL_STATE");
        }
        let missing: Vec<&str> = ALL_STATE
            .iter()
            .copied()
            .filter(|address| !seen.contains(address))
            .collect();
        assert!(
            missing.is_empty(),
            "state addresses with no shape: {missing:?}"
        );
    }
}
