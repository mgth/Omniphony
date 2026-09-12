//! The release check.
//!
//! A banner when a newer release exists, and a switch to stop asking. The web
//! does the HTTP from the webview; a native host has to bring its own client,
//! and must not do it on the frame loop — a check on a slow link would freeze
//! the window for as long as the request takes. It runs on a thread of its
//! own and leaves its answer in the model, where the panel reads it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::host::commands::{SharedState, app};
use crate::i18n::tf;

/// `https://api.github.com/repos/mgth/Omniphony/releases`.
const RELEASES_API: &str = "https://api.github.com/repos/mgth/Omniphony/releases";
/// Where the banner's link goes when a release carries no URL of its own.
pub const RELEASES_PAGE: &str = "https://github.com/mgth/Omniphony/releases";
/// At most one check a day. A release is not news that often, and the API
/// counts unauthenticated requests per address.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// A check that has not answered by now was not going to.
const TIMEOUT: Duration = Duration::from_secs(10);

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
pub fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
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

/// Where a check is up to. The panel draws from this and remembers the result
/// in its own preferences, so a banner survives a restart without asking
/// again.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateCheck {
    #[default]
    Idle,
    Running,
    /// A release newer than nothing in particular: the panel decides whether
    /// it is newer than this build.
    Found {
        tag: String,
        html_url: Option<String>,
    },
    /// The check ran and found no release worth offering.
    NoneFound,
}

/// Seconds since the epoch, as the stored `last_check` counts them.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a check is due, given when the last one ran.
pub fn is_due(last_check: u64) -> bool {
    now_secs().saturating_sub(last_check) >= CHECK_INTERVAL.as_secs()
}

/// Start a check unless one is already running. The request goes to its own
/// thread and the answer lands in the model; a failure is a log line, not a
/// banner — the user asked about releases, not about the network.
pub fn start_check(state: &std::sync::Arc<SharedState>) {
    {
        let mut live = state.inner.lock().unwrap();
        if live.update_check == UpdateCheck::Running {
            return;
        }
        live.update_check = UpdateCheck::Running;
    }
    let state = std::sync::Arc::clone(state);
    std::thread::Builder::new()
        .name("studio-update-check".into())
        .spawn(move || {
            let found = fetch_latest();
            match found {
                Ok(Some(release)) => {
                    state.inner.lock().unwrap().update_check = UpdateCheck::Found {
                        tag: release.tag,
                        html_url: release.html_url,
                    }
                }
                Ok(None) => state.inner.lock().unwrap().update_check = UpdateCheck::NoneFound,
                Err(error) => {
                    state.inner.lock().unwrap().update_check = UpdateCheck::Idle;
                    app::push_log(
                        &state,
                        "warn",
                        "updates",
                        tf("updates.checkFailed", &[("error", &error)]),
                    );
                }
            }
            (state.waker)();
        })
        .ok();
}

fn fetch_latest() -> Result<Option<Release>, String> {
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
        Ok(body) => Ok(newest(&body)),
        Err(error) => Err(error.to_string()),
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

/// Take a finished check's answer, leaving the model idle. The panel keeps it
/// in its own preferences, which is what survives a restart.
pub fn take_result(state: &SharedState) -> Option<UpdateCheck> {
    let mut live = state.inner.lock().unwrap();
    match live.update_check {
        UpdateCheck::Idle | UpdateCheck::Running => None,
        _ => Some(std::mem::take(&mut live.update_check)),
    }
}
