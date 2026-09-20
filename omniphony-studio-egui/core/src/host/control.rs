//! Raw control layer: one message, one call. It was the UI's handle on the
//! renderer during the port; phase 2 of the boundary plan replaced every use
//! of it with a typed command in `host::commands`, so nothing outside this
//! crate sends by address any more.
//!
//! Kept `pub(crate)` for the commands that still find it convenient, and as
//! the place where a new raw send would have to be justified.

#![allow(dead_code)] // one method per host command, ported ahead of its panel

use rosc::OscType;

use crate::osc::{Control, ControlTx};

#[derive(Clone)]
pub(crate) struct Ctl {
    tx: ControlTx,
}

impl Ctl {
    pub fn new(tx: ControlTx) -> Self {
        Self { tx }
    }

    pub fn send(&self, address: &str, args: Vec<OscType>) {
        let _ = self.tx.send(Control::Send {
            address: address.to_owned(),
            args,
        });
    }

    pub fn send_float(&self, address: &str, value: f32) {
        self.send(address, vec![OscType::Float(value)]);
    }

    pub fn send_int(&self, address: &str, value: i32) {
        self.send(address, vec![OscType::Int(value)]);
    }

    pub fn send_no_args(&self, address: &str) {
        self.send(address, Vec::new());
    }

    pub fn send_string(&self, address: &str, value: &str) {
        self.send(address, vec![OscType::String(value.to_owned())]);
    }

    pub fn send_floats3(&self, address: &str, a: f32, b: f32, c: f32) {
        self.send(
            address,
            vec![OscType::Float(a), OscType::Float(b), OscType::Float(c)],
        );
    }

    /// `send_json_control`: a JSON document as one string argument.
    pub fn send_json(&self, address: &str, payload: &serde_json::Value) {
        if let Ok(value) = serde_json::to_string(payload) {
            self.send_string(address, &value);
        }
    }

    pub fn set_metering(&self, enabled: bool) {
        let _ = self.tx.send(Control::SetMetering { enabled });
    }

    pub fn subscribe_gaintable(&self, have_version: i32, speaker_index: i32) {
        let _ = self.tx.send(Control::SubscribeGainTable {
            have_version,
            speaker_index,
        });
    }

    pub fn unsubscribe_gaintable(&self) {
        let _ = self.tx.send(Control::UnsubscribeGainTable);
    }
}
