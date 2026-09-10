//! The update check (`#updatePanelRoot`, `controls/updates.js`).
//!
//! A banner when a newer release exists, and a switch to stop asking. The web
//! does the HTTP from the webview; a native host has to bring its own client,
//! and has to do it off the frame loop — a release check on a slow link would
//! otherwise freeze the window for as long as the request takes.

use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::Ui;
use serde::{Deserialize, Serialize};

use crate::app::StudioSpike;
use crate::i18n::{t, tf};
use crate::ui::widgets;

/// `https://api.github.com/repos/mgth/Omniphony/releases`.
const RELEASES_API: &str = "https://api.github.com/repos/mgth/Omniphony/releases";
/// Where the banner's link goes when a release carries no URL of its own.
const RELEASES_PAGE: &str = "https://github.com/mgth/Omniphony/releases";
/// At most one check a day. A release is not news that often, and the API
/// counts unauthenticated requests per address.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// A check that has not answered by now was not going to.
const TIMEOUT: Duration = Duration::from_secs(10);

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

/// A release the check accepted.
struct Release {
    tag: String,
    html_url: Option<String>,
}

/// `^v(\d+)\.(\d+)\.(\d+)$` — three numbers and nothing else.
///
/// The dev tags (`v0.x.y.nnn`) and the library's own (`liborender-v*`) are not
/// Studio releases, and offering one as an update would send the user to a tag
/// that does not build a Studio.
fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let rest = tag.strip_prefix('v')?;
    let mut parts = rest.split('.');
    let triple = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(triple)
}

/// The newest release of the ones GitHub returned, ignoring drafts and
/// pre-releases.
fn newest(releases: &serde_json::Value) -> Option<Release> {
    releases
        .as_array()?
        .iter()
        .filter(|r| {
            !r.get("draft")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && !r
                    .get("prerelease")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
        })
        .filter_map(|r| {
            let tag = r.get("tag_name")?.as_str()?;
            let version = parse_version(tag)?;
            Some((
                version,
                Release {
                    tag: tag.to_owned(),
                    html_url: r
                        .get("html_url")
                        .and_then(|u| u.as_str())
                        .map(str::to_owned),
                },
            ))
        })
        .max_by_key(|(version, _)| *version)
        .map(|(_, release)| release)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a finished check hands back to the frame loop.
pub enum CheckResult {
    Found(String, Option<String>),
    None,
    Failed(String),
}

pub type CheckHandle = Receiver<CheckResult>;

impl StudioSpike {
    /// The switch and, when there is one, the banner.
    pub(crate) fn updates_panel(&mut self, ui: &mut Ui) {
        self.poll_update_check();
        // The web checks "on startup"; this panel is drawn from the first
        // frame, and the call is a no-op unless a check is both enabled and
        // due, so drawing it is the same moment.
        self.maybe_check_updates();
        let mut enabled = self.prefs.updates.enabled;
        ui.horizontal(|ui| {
            ui.label(t("updates.checkLabel"));
            widgets::help(ui, "help.updates.check");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::switch(ui, &mut enabled).changed() {
                    self.prefs.updates.enabled = enabled;
                    self.mark_prefs_dirty();
                    if enabled {
                        self.maybe_check_updates();
                    }
                }
            });
        });
        if !enabled {
            return;
        }
        let Some(tag) = self.prefs.updates.tag.clone().filter(|tag| {
            // The stored tag is only news while it is newer than *this* build,
            // which it stops being the moment the user updates.
            match (
                parse_version(tag),
                parse_version(&format!("v{}", env!("CARGO_PKG_VERSION"))),
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
            .unwrap_or_else(|| RELEASES_PAGE.to_owned());
        widgets::banner_with(
            ui,
            widgets::Severity::Info,
            &tf("updates.available", &[("version", &tag)]),
            |ui| {
                ui.hyperlink_to(t("updates.linkText"), url);
            },
        );
    }

    /// Start a check if one is due. Never on the frame loop: the request goes
    /// to its own thread and answers through a channel.
    pub(crate) fn maybe_check_updates(&mut self) {
        if !self.prefs.updates.enabled || self.update_check.is_some() {
            return;
        }
        let now = now_secs();
        if now.saturating_sub(self.prefs.updates.last_check) < CHECK_INTERVAL.as_secs() {
            return;
        }
        self.prefs.updates.last_check = now;
        self.mark_prefs_dirty();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_latest());
        });
        self.update_check = Some(rx);
    }

    fn poll_update_check(&mut self) {
        let Some(rx) = &self.update_check else { return };
        match rx.try_recv() {
            Ok(CheckResult::Found(tag, html_url)) => {
                self.prefs.updates.tag = Some(tag);
                self.prefs.updates.html_url = html_url;
                self.mark_prefs_dirty();
                self.update_check = None;
            }
            Ok(CheckResult::None) => {
                self.prefs.updates.tag = None;
                self.prefs.updates.html_url = None;
                self.mark_prefs_dirty();
                self.update_check = None;
            }
            Ok(CheckResult::Failed(error)) => {
                // A failed check is a log line, not a banner: the user asked
                // about releases, not about the network.
                self.log(
                    "warn",
                    "updates",
                    tf("updates.checkFailed", &[("error", &error)]),
                );
                self.update_check = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.update_check = None,
        }
    }
}

fn fetch_latest() -> CheckResult {
    let agent = ureq::AgentBuilder::new()
        .timeout_read(TIMEOUT)
        .timeout_connect(TIMEOUT)
        .build();
    let response = agent
        .get(RELEASES_API)
        .set("Accept", "application/vnd.github+json")
        .set(
            "User-Agent",
            concat!("omniphony-studio/", env!("CARGO_PKG_VERSION")),
        )
        .call();
    match response.and_then(|r| r.into_json::<serde_json::Value>().map_err(Into::into)) {
        Ok(body) => match newest(&body) {
            Some(release) => CheckResult::Found(release.tag, release.html_url),
            None => CheckResult::None,
        },
        Err(error) => CheckResult::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_three_number_v_tag_is_a_studio_release() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        // A dev tag and the library's own are not Studio releases, and
        // offering one would send the user to a tag that builds no Studio.
        assert_eq!(parse_version("v0.5.2.17"), None);
        assert_eq!(parse_version("liborender-v1.2.3"), None);
        assert_eq!(parse_version("1.2.3"), None);
        assert_eq!(parse_version("v1.2"), None);
    }

    #[test]
    fn the_newest_release_wins_and_drafts_do_not_count() {
        let body = serde_json::json!([
            { "tag_name": "v0.9.0", "html_url": "u9" },
            { "tag_name": "v0.10.0", "html_url": "u10" },
            { "tag_name": "v1.0.0", "draft": true, "html_url": "ud" },
            { "tag_name": "v1.1.0", "prerelease": true, "html_url": "up" },
            { "tag_name": "v0.5.2.9", "html_url": "udev" },
        ]);
        let release = newest(&body).expect("a release");
        // Numeric comparison, not lexical: 10 is newer than 9.
        assert_eq!(release.tag, "v0.10.0");
        assert_eq!(release.html_url.as_deref(), Some("u10"));
        assert!(newest(&serde_json::json!([])).is_none());
    }
}
