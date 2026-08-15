//! Named config profile controls: switch, create, delete and rename.
//!
//! Each command forwards the profile name(s) to the renderer over OSC; the
//! renderer answers with a fresh `/omniphony/state/profiles` broadcast (and,
//! after a switch, the full state bundle plus a topology recompute).

use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use tauri::State;

#[tauri::command]
pub fn control_profile_switch(state: State<SharedState>, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/profile/switch".to_string(),
            value: name,
        },
    );
}

#[tauri::command]
pub fn control_profile_create(state: State<SharedState>, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/profile/create".to_string(),
            value: name,
        },
    );
}

#[tauri::command]
pub fn control_profile_delete(state: State<SharedState>, value: String) {
    let name = value.trim().to_string();
    if name.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/profile/delete".to_string(),
            value: name,
        },
    );
}

#[tauri::command]
pub fn control_profile_rename(state: State<SharedState>, old: String, new: String) {
    let old = old.trim().to_string();
    let new = new.trim().to_string();
    if old.is_empty() || new.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/profile/rename".to_string(),
            args: vec![rosc::OscType::String(old), rosc::OscType::String(new)],
        },
    );
}
