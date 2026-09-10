//! OSC connection status (`#oscPanelRoot`): the dot, the target and the port,
//! plus the reconnect form of `controls/osc.js`.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::app::StudioSpike;
use crate::ui::{theme, widgets};

impl StudioSpike {
    /// The status line: a coloured dot and one line of text, like the web
    /// `#oscStatus` row.
    pub(crate) fn connection_line(&self, ui: &mut egui::Ui) {
        let port = self.osc_stats.listen_port.load(Ordering::Relaxed);
        let target = *self.osc_stats.target.lock().unwrap();
        let (dot, text) = match (
            target,
            self.osc_stats.registered.load(Ordering::Relaxed),
            self.osc_stats.since_last_packet(),
        ) {
            (Some(addr), true, Some(d)) if d < Duration::from_secs(7) => {
                (theme::OK, format!("registered with {addr} · udp/{port}"))
            }
            (Some(addr), _, _) => (theme::WARN, format!("waiting for {addr} · udp/{port}")),
            (None, _, Some(d)) if d < Duration::from_secs(2) => {
                (theme::OK, format!("receiving on udp/{port}"))
            }
            (None, _, _) => (theme::TEXT_FAINT, format!("idle · udp/{port}")),
        };
        widgets::status_dot(ui, dot, text);
    }
}
