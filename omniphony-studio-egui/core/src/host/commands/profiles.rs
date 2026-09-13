//! Named config profile controls: switch, create, delete and rename.
//!
//! Each command forwards the profile name(s) to the renderer over OSC; the
//! renderer answers with a fresh `/omniphony/state/profiles` broadcast (and,
//! after a switch, the full state bundle plus a topology recompute).

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

pub fn control_profile_switch(state: &SharedState, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_PROFILE_SWITCH.to_string(),
            value: name,
        },
    );
}

pub fn control_profile_create(state: &SharedState, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_PROFILE_CREATE.to_string(),
            value: name,
        },
    );
}

pub fn control_profile_delete(state: &SharedState, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_PROFILE_DELETE.to_string(),
            value: name,
        },
    );
}

pub fn control_profile_rename(state: &SharedState, old: String, new: String) {
    let old = old.trim().to_string();
    let new = new.trim().to_string();
    if old.is_empty() || new.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_PROFILE_RENAME.to_string(),
            args: vec![rosc::OscType::String(old), rosc::OscType::String(new)],
        },
    );
}
