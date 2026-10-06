//! Studio strings. The catalogues are `omniphony-studio-egui/i18n/*.json`,
//! embedded at build time. The deprecated web Studio imports the same files,
//! so both hosts stay key-for-key identical while both ship.
//!
//! Every locale is English overridden by its own entries, exactly as the web
//! spreads `{...enTranslations, ...frTranslations}`: a key a translator has not
//! reached yet reads in English rather than as a raw key. `t` resolves a key
//! and `tf` substitutes `{name}` placeholders.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// The locales, in the order the language picker offers them. The first is the
/// base every other one falls back to.
const CATALOGUES: &[(&str, &str)] = &[
    ("en", include_str!("../../i18n/en.json")),
    ("fr", include_str!("../../i18n/fr.json")),
    ("de", include_str!("../../i18n/de.json")),
    ("ja", include_str!("../../i18n/ja.json")),
    ("es", include_str!("../../i18n/es.json")),
    ("it", include_str!("../../i18n/it.json")),
    ("pt-BR", include_str!("../../i18n/pt-BR.json")),
    ("zh-CN", include_str!("../../i18n/zh-CN.json")),
];

/// Which catalogue `t` reads. An index rather than a name, so resolving a key
/// costs an atomic load and a hash lookup.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Every catalogue, parsed once and merged over English.
fn catalogues() -> &'static Vec<HashMap<String, String>> {
    static ALL: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();
    ALL.get_or_init(|| {
        let parse = |name: &str, json: &str| {
            serde_json::from_str::<HashMap<String, String>>(json).unwrap_or_else(|e| {
                log::error!("[i18n] {name}.json: {e}");
                HashMap::new()
            })
        };
        let base = parse(CATALOGUES[0].0, CATALOGUES[0].1);
        let mut all = vec![base.clone()];
        for (name, json) in &CATALOGUES[1..] {
            let mut merged = base.clone();
            merged.extend(parse(name, json));
            all.push(merged);
        }
        all
    })
}

/// The languages the picker offers, `auto` first. The label is the language's
/// own name: a reader looking for their language recognises it written the way
/// they write it, not the way English writes it.
pub const LOCALE_OPTIONS: &[(&str, &str)] = &[
    ("auto", "Auto"),
    ("en", "English"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("ja", "日本語"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("pt-BR", "Português (Brasil)"),
    ("zh-CN", "简体中文"),
];

/// Apply a preference: a locale name, or `auto` to follow the environment.
/// Anything unknown falls back to English, as `normalizeLocale` does.
pub fn set_locale(preference: &str) {
    let name = if preference == "auto" {
        detect_system_locale()
    } else {
        preference.to_owned()
    };
    let index = CATALOGUES
        .iter()
        .position(|(id, _)| *id == name)
        .unwrap_or(0);
    ACTIVE.store(index, Ordering::Relaxed);
}

/// The locale currently in force.
pub fn active_locale() -> &'static str {
    CATALOGUES[ACTIVE.load(Ordering::Relaxed).min(CATALOGUES.len() - 1)].0
}

/// The environment's language, mapped onto a catalogue.
///
/// The web reads `navigator.languages`; a native process reads the POSIX
/// variables, most specific first. Region matters for exactly two of the
/// catalogues — Brazilian Portuguese and simplified Chinese — so those are
/// matched on the full tag and everything else on the language alone.
fn detect_system_locale() -> String {
    locale_from(|key| std::env::var(key).ok())
}

/// [`detect_system_locale`] over any variable lookup, so its rules are tested
/// without touching the process environment.
fn locale_from(var: impl Fn(&str) -> Option<String>) -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Some(value) = var(key) else {
            continue;
        };
        let tag = value
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .replace('_', "-")
            .to_ascii_lowercase();
        if tag.is_empty() || tag == "c" || tag == "posix" {
            continue;
        }
        if tag.starts_with("pt-br") {
            return "pt-BR".to_owned();
        }
        if tag.starts_with("zh-cn") || tag.starts_with("zh-hans") {
            return "zh-CN".to_owned();
        }
        for name in ["fr", "de", "ja", "es", "it", "en"] {
            if tag.starts_with(name) {
                return name.to_owned();
            }
        }
    }
    "en".to_owned()
}

/// The string for `key`, or the key itself when it is missing (like the web).
pub fn t(key: &str) -> &'static str {
    let all = catalogues();
    let index = ACTIVE.load(Ordering::Relaxed).min(all.len() - 1);
    match all[index].get(key) {
        Some(s) => s.as_str(),
        None => {
            log::debug!("[i18n] missing key {key}");
            leak_key(key)
        }
    }
}

/// `t(key)` when the key exists, for callers that probe for an optional
/// string (a translated parameter help, say) and fall back to their own.
pub fn lookup(key: &str) -> Option<&'static str> {
    let all = catalogues();
    let index = ACTIVE.load(Ordering::Relaxed).min(all.len() - 1);
    all[index].get(key).map(String::as_str)
}

fn leak_key(key: &str) -> &'static str {
    // Missing keys are a development-time defect; a small leak per distinct
    // key keeps the signature borrow-free. Per distinct key, not per call: a
    // missing key asked for on every frame would otherwise grow the heap for
    // as long as the window repaints.
    static LEAKED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut leaked = LEAKED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(interned) = leaked.get(key) {
        return interned;
    }
    let interned: &'static str = Box::leak(key.to_owned().into_boxed_str());
    leaked.insert(interned);
    interned
}

/// `t(key)` with `{name}` placeholders replaced.
pub fn tf(key: &str, values: &[(&str, &str)]) -> String {
    let mut out = t(key).to_owned();
    for (name, value) in values {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_key_is_leaked_once_not_once_per_call() {
        let first = t("no.such.key.anywhere");
        let second = t("no.such.key.anywhere");
        assert_eq!(first, "no.such.key.anywhere");
        assert!(std::ptr::eq(first, second));
        assert_eq!(lookup("no.such.key.anywhere"), None);
    }

    /// The active locale is process-wide, so the tests read the catalogues
    /// directly rather than switching it: a test that changed it would decide
    /// what every other test in the binary resolves to.
    fn catalogue(name: &str) -> &'static HashMap<String, String> {
        let index = CATALOGUES.iter().position(|(id, _)| *id == name).unwrap();
        &catalogues()[index]
    }

    #[test]
    fn catalogue_loads_and_substitutes() {
        assert_eq!(t("app.title"), "Omniphony Studio");
        assert_eq!(
            tf("updates.available", &[("version", "1.2.3")]),
            "Update available: 1.2.3"
        );
        assert_eq!(t("no.such.key"), "no.such.key");
    }

    #[test]
    fn every_locale_parses_and_falls_back_to_english() {
        let all = catalogues();
        assert_eq!(all.len(), CATALOGUES.len());
        for (index, (name, _)) in CATALOGUES.iter().enumerate() {
            assert!(
                all[index].len() >= all[0].len(),
                "{name} lost keys English has"
            );
            // The fallback is what makes a partial translation usable: every
            // key English knows must resolve in every locale.
            assert!(all[index].contains_key("app.title"), "{name}");
        }
    }

    #[test]
    fn each_catalogue_actually_translates() {
        assert_eq!(catalogue("en")["common.cancel"], "Cancel");
        for name in ["fr", "de", "ja", "es", "it", "pt-BR", "zh-CN"] {
            assert_ne!(
                catalogue(name)["common.cancel"],
                "Cancel",
                "{name} left a common key untranslated"
            );
        }
    }

    #[test]
    fn the_two_region_sensitive_tags_are_matched_on_the_full_tag() {
        let lc_all =
            |value: &'static str| locale_from(move |k| (k == "LC_ALL").then(|| value.into()));
        // Region matters for exactly these two catalogues.
        assert_eq!(lc_all("pt_BR.UTF-8"), "pt-BR");
        assert_eq!(lc_all("zh_CN.UTF-8"), "zh-CN");
        // Everything else matches on the language alone.
        assert_eq!(lc_all("fr_CA.UTF-8"), "fr");
        // The C locale says nothing about a language.
        assert_eq!(lc_all("C"), "en");
    }

    #[test]
    fn a_silent_variable_defers_to_the_next_one() {
        let vars = |k: &str| match k {
            "LC_ALL" => Some("C".to_owned()),
            "LANG" => Some("de_DE.UTF-8".to_owned()),
            _ => None,
        };
        assert_eq!(locale_from(vars), "de");
        assert_eq!(locale_from(|_| None), "en");
    }
}
