//! The update panel (`#updatePanelRoot`, `controls/updates.js`).
//!
//! A banner when a newer release exists, and a switch to stop asking. The
//! check itself — the HTTP, its thread and its once-a-day rule — is the
//! core's (`services::updates`); what is kept between runs is a preference,
//! so it lives here with the other ones.

use egui::Ui;
use serde::{Deserialize, Serialize};

use crate::app::StudioSpike;
use crate::host::services::updates as check;
use crate::i18n::{t, tf};
use crate::ui::widgets;

/// What the check found, kept between runs so the banner survives a restart
/// without asking again.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdatePrefs {
    /// `omniphony.updateCheck.enabled`. Off until asked for: a program that
    /// phones home on first start without being asked is a program that
    /// surprised its user.
    pub enabled: bool,
    /// `omniphony.updateCheck.lastCheck`, seconds since the epoch.
    pub last_check: u64,
    /// `omniphony.updateCheck.result`.
    pub tag: Option<String>,
    pub html_url: Option<String>,
}

impl StudioSpike {
    /// The switch and, when there is one, the banner.
    pub(crate) fn updates_panel(&mut self, ui: &mut Ui) {
        self.absorb_update_check();
        // The web checks "on startup"; this panel is drawn from the first
        // frame, and asking is a no-op unless a check is both enabled and due,
        // so drawing it is the same moment.
        self.maybe_check_updates();
        let mut enabled = self.prefs.updates.enabled;
        widgets::label_row_help(ui, t("updates.checkLabel"), "help.updates.check", |ui| {
            if widgets::switch(ui, &mut enabled, t("updates.checkLabel")).changed() {
                self.prefs.updates.enabled = enabled;
                self.mark_prefs_dirty();
                if enabled {
                    self.maybe_check_updates();
                }
            }
        });
        if !enabled {
            return;
        }
        let Some(tag) = self.prefs.updates.tag.clone().filter(|tag| {
            // The stored tag is only news while it is newer than *this* build,
            // which it stops being the moment the user updates.
            match (
                check::parse_version(tag),
                check::parse_version(&format!("v{}", env!("CARGO_PKG_VERSION"))),
            ) {
                (Some(found), Some(mine)) => found > mine,
                _ => false,
            }
        }) else {
            return;
        };
        let url = self
            .prefs
            .updates
            .html_url
            .clone()
            .unwrap_or_else(|| check::RELEASES_PAGE.to_owned());
        widgets::banner_with(
            ui,
            widgets::Severity::Info,
            &tf("updates.available", &[("version", &tag)]),
            |ui| {
                ui.hyperlink_to(t("updates.linkText"), url);
            },
        );
    }

    /// Ask the core for a check when one is due. It answers on its own thread,
    /// so a slow link never reaches the frame.
    pub(crate) fn maybe_check_updates(&mut self) {
        if !self.prefs.updates.enabled || !check::is_due(self.prefs.updates.last_check) {
            return;
        }
        self.prefs.updates.last_check = check::now_secs();
        self.mark_prefs_dirty();
        check::start_check(&self.host);
    }

    /// Keep a finished check's answer in the preferences, which is what
    /// survives a restart without asking again.
    fn absorb_update_check(&mut self) {
        let Some(result) = check::take_result(&self.host) else {
            return;
        };
        let (tag, html_url) = match result {
            check::UpdateCheck::Found { tag, html_url } => (Some(tag), html_url),
            _ => (None, None),
        };
        self.prefs.updates.tag = tag;
        self.prefs.updates.html_url = html_url;
        self.mark_prefs_dirty();
    }
}
