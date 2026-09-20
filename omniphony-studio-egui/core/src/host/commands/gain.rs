//! Gain & mute controls: per-speaker / per-object / master gain, object and
//! speaker mute, loudness compensation and auto-gain. The realtime gain commands
//! stamp a monotonic sequence number so the renderer can drop stale updates.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control, send_json_control};
use crate::osc_contract;
use std::sync::atomic::Ordering;

pub fn control_speaker_gain(state: &SharedState, id: i32, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    state
        .inner
        .lock()
        .unwrap()
        .app
        .speaker_gains
        .insert(id.max(0).to_string(), f64::from(clamped));
    let seq = state.realtime_seq.fetch_add(1, Ordering::Relaxed) + 1;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_REALTIME_SPEAKER_GAIN.to_string(),
            args: vec![
                rosc::OscType::Int(id),
                rosc::OscType::Float(clamped),
                rosc::OscType::Int(seq),
            ],
        },
    );
}

pub fn control_object_mute(state: &SharedState, id: i32, muted: i32) {
    set_object_mute_local(state, &id.to_string(), muted != 0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: format!("{}{id}/mute", osc_contract::CONTROL_OBJECT_PREFIX),
            value: if muted != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_speaker_mute(state: &SharedState, id: i32, muted: i32) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .speaker_mutes
        .insert(id.to_string(), u8::from(muted != 0));
    send_json_control(
        &state.osc_tx,
        osc_contract::CONTROL_CONFIG_SPEAKERS,
        serde_json::json!({
            "speakerEdits": [{
                "id": id.max(0),
                "muted": muted != 0
            }]
        }),
    );
}

pub fn control_master_gain(state: &SharedState, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    state.inner.lock().unwrap().app.master_gain = Some(f64::from(clamped));
    let seq = state.realtime_seq.fetch_add(1, Ordering::Relaxed) + 1;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_REALTIME_MASTER_GAIN.to_string(),
            args: vec![rosc::OscType::Float(clamped), rosc::OscType::Int(seq)],
        },
    );
}

/// Mute an object in the model without telling the renderer, for a source
/// this side owns: the injected test source is not addressable by index, so
/// `control_object_mute` would send it as `NaN`. Its signal is stopped by
/// whoever owns the transport.
pub fn set_object_mute_local(state: &SharedState, id: &str, muted: bool) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .object_mutes
        .insert(id.to_owned(), u8::from(muted));
}

pub fn control_loudness(state: &SharedState, enable: i32) {
    state.inner.lock().unwrap().app.loudness = Some(u8::from(enable != 0));
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_LOUDNESS.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_auto_gain(state: &SharedState, enable: i32) {
    state.inner.lock().unwrap().app.auto_gain = Some(enable != 0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_AUTO_GAIN.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_auto_gain_ceiling(state: &SharedState, db: f32) {
    let clamped = db.clamp(-12.0, 0.0);
    state.inner.lock().unwrap().app.auto_gain_ceiling_db = Some(f64::from(clamped));
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_AUTO_GAIN_CEILING.to_string(),
            value: clamped,
        },
    );
}

/// Start or stop the per-speaker test signal (band-limited pink noise).
///
/// `id < 0` stops any running test. The trigger policy — hold, fixed burst or
/// toggle — lives in the UI, so this is the whole renderer-facing contract:
/// start this speaker, or stop.
/// Arm/disarm the speaker-test idle feed: while armed the renderer fabricates
/// silence input frames when nothing is playing, so the output chain is warm
/// and a test is heard immediately. The arm expires renderer-side after a
/// keepalive window; the UI re-sends it periodically while the Test pane is
/// open.
pub fn control_speaker_test_idle_feed(state: &SharedState, enable: bool) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_SPEAKER_TEST_IDLE_FEED.to_string(),
            value: if enable { 1 } else { 0 },
        },
    );
}

/// Place (or stop) the object test signal.
///
/// Sent once per pointer move while the user drags the object across a face, so
/// it stays a plain fire-and-forget message: the renderer ramps to the new
/// position without restarting the noise, which is what makes dragging audible
/// as movement rather than as a series of clicks.
#[allow(clippy::too_many_arguments)]
pub fn control_object_test(
    state: &SharedState,
    on: bool,
    x: f32,
    y: f32,
    z: f32,
    level: f32,
    size: f32,
    isolation: String,
    signal: String,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OBJECT_TEST.to_string(),
            args: vec![
                rosc::OscType::Int(i32::from(on)),
                // Clamped here as well as renderer-side, same reasoning as the
                // speaker test: a stray UI value must not reach the audio path.
                rosc::OscType::Float(x.clamp(-1.0, 1.0)),
                rosc::OscType::Float(y.clamp(-1.0, 1.0)),
                rosc::OscType::Float(z.clamp(-1.0, 1.0)),
                rosc::OscType::Float(level.clamp(0.0, 1.0)),
                rosc::OscType::Float(size.clamp(0.0, 1.0)),
                rosc::OscType::String(isolation),
                rosc::OscType::String(signal),
            ],
        },
    );
}

/// Set the object test's orbit.
///
/// Its own command rather than more arguments on `control_object_test`, for the
/// same reason the OSC address is separate: that one fires on every pointer
/// move while dragging, and this changes only when a knob does.
pub fn control_object_test_rotation(
    state: &SharedState,
    axis: String,
    radius: f32,
    period: f32,
    azimuth: f32,
    elevation: f32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OBJECT_TEST_ROTATION.to_string(),
            args: vec![
                rosc::OscType::String(axis),
                rosc::OscType::Float(radius.clamp(0.0, 4.0)),
                rosc::OscType::Float(period.clamp(0.05, 600.0)),
                rosc::OscType::Float(azimuth),
                rosc::OscType::Float(elevation),
            ],
        },
    );
}

/// Choose (or clear, with an empty path) the WAV file the `clip` signal plays.
///
/// Its own command for the same reason the orbit has one: the placement message
/// fires on every pointer move, and a file path is a long argument to restate.
/// The renderer answers on `/omniphony/state/object_test/clip`, including when
/// it refuses the file.
pub fn control_object_test_clip(state: &SharedState, path: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_OBJECT_TEST_CLIP.to_string(),
            value: path,
        },
    );
}

pub fn control_speaker_test(state: &SharedState, id: i32, level: f32, isolation: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_SPEAKER_TEST.to_string(),
            args: vec![
                rosc::OscType::Int(id),
                // Clamped here as well as renderer-side: this drives a speaker,
                // and a stray value from the UI must not reach the audio path.
                rosc::OscType::Float(level.clamp(0.0, 1.0)),
                rosc::OscType::String(isolation),
            ],
        },
    );
}
