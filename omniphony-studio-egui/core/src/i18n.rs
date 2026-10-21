//! Studio strings. The catalogues are `omniphony-studio-egui/i18n/*.json`,
//! embedded at build time.
//!
//! Every locale overrides English with its own entries: a missing translation
//! reads in English rather than as a raw key. Tests check the raw catalogues
//! for key and placeholder parity before this fallback can hide a gap.
//! `t` resolves a key and `tf` substitutes `{name}` placeholders.

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

fn parse_catalogue(json: &str) -> Result<HashMap<String, String>, serde_json::Error> {
    serde_json::from_str(json)
}

/// Every catalogue, parsed once and merged over English.
fn catalogues() -> &'static Vec<HashMap<String, String>> {
    static ALL: OnceLock<Vec<HashMap<String, String>>> = OnceLock::new();
    ALL.get_or_init(|| {
        let parse = |name: &str, json: &str| {
            parse_catalogue(json).unwrap_or_else(|e| {
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
/// The POSIX variables come first, most specific first, so a shell can still
/// force a language. Windows and a macOS app started from the Finder set none
/// of them, so the system's preferred UI languages follow, in the user's order
/// — what the web read from `navigator.languages`.
fn detect_system_locale() -> String {
    let variables = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok());
    locale_from(variables.chain(sys_locale::get_locales()))
}

/// [`detect_system_locale`] over any list of tags, so its rules are tested
/// without touching the process environment. Region matters for exactly two
/// of the catalogues — Brazilian Portuguese and simplified Chinese — so those
/// are matched on the full tag and everything else on the language alone.
fn locale_from(candidates: impl IntoIterator<Item = String>) -> String {
    for value in candidates {
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
    use std::collections::BTreeSet;

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
            // Probe the runtime maps as well as the raw catalogue gate below.
            assert!(all[index].contains_key("app.title"), "{name}");
        }
    }

    fn placeholders(value: &str) -> BTreeSet<&str> {
        value
            .split('{')
            .skip(1)
            .filter_map(|tail| tail.split_once('}').map(|(name, _)| name))
            .collect()
    }

    fn catalogue_structure_errors(
        english: &HashMap<String, String>,
        translated: &HashMap<String, String>,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        for (key, source) in english {
            match translated.get(key) {
                None => errors.push(format!("{key} (missing translation)")),
                Some(value) => {
                    let expected = placeholders(source);
                    let actual = placeholders(value);
                    if expected != actual {
                        errors.push(format!(
                            "{key} (placeholders: expected {expected:?}, found {actual:?})"
                        ));
                    }
                }
            }
        }
        for key in translated.keys() {
            if !english.contains_key(key) {
                errors.push(format!("{key} (absent from en.json)"));
            }
        }
        errors.sort();
        errors
    }

    #[test]
    fn every_catalogue_matches_english_structure() {
        // Read the raw catalogues: catalogues() merges in English, hiding
        // missing translations. With every locale complete, the ratchet is
        // simply zero missing keys; there is no debt baseline to maintain.
        let parse = |name: &str, json: &str| {
            parse_catalogue(json).unwrap_or_else(|error| panic!("{name}.json: {error}"))
        };
        let english = parse(CATALOGUES[0].0, CATALOGUES[0].1);
        let mut errors = Vec::new();
        for (name, json) in &CATALOGUES[1..] {
            let translated = parse(name, json);
            errors.extend(
                catalogue_structure_errors(&english, &translated)
                    .into_iter()
                    .map(|error| format!("{name}.json: {error}")),
            );
        }
        assert!(
            errors.is_empty(),
            "Every i18n catalogue must match en.json's keys and placeholders:\n{}",
            errors.join("\n")
        );
    }

    #[test]
    fn the_structure_gate_rejects_missing_and_orphaned_keys() {
        let english = parse_catalogue(r#"{"kept":"Title", "missing":"Message"}"#).unwrap();
        let translated = parse_catalogue(r#"{"kept":"Titel", "orphan":"Nachricht"}"#).unwrap();
        assert_eq!(
            catalogue_structure_errors(&english, &translated),
            [
                "missing (missing translation)",
                "orphan (absent from en.json)"
            ]
        );
    }

    #[test]
    fn the_structure_gate_checks_placeholder_sets() {
        let english = HashMap::from([("status".into(), "Error: {error} at {path}".into())]);
        for (value, valid) in [
            ("Fehler bei {path}", false),
            ("{fehler} bei {path}", false),
            ("{error} bei {path}: {extra}", false),
            ("{path}：エラー {error}", true),
            ("{path}：{error}（{error}）", true),
        ] {
            let translated = HashMap::from([("status".into(), value.into())]);
            let errors = catalogue_structure_errors(&english, &translated);
            assert_eq!(errors.is_empty(), valid, "{value:?}: {errors:?}");
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
        let one = |value: &str| locale_from([value.to_owned()]);
        // Region matters for exactly these two catalogues.
        assert_eq!(one("pt_BR.UTF-8"), "pt-BR");
        assert_eq!(one("zh_CN.UTF-8"), "zh-CN");
        // Everything else matches on the language alone.
        assert_eq!(one("fr_CA.UTF-8"), "fr");
        // The C locale says nothing about a language.
        assert_eq!(one("C"), "en");
    }

    #[test]
    fn a_silent_candidate_defers_to_the_next_one() {
        let tags = |list: &[&str]| locale_from(list.iter().map(|s| s.to_string()));
        assert_eq!(tags(&["C", "de_DE.UTF-8"]), "de");
        assert_eq!(tags(&[]), "en");
    }

    #[test]
    fn system_language_tags_map_like_the_posix_variables() {
        // What Windows and macOS report: BCP 47 tags, scripts included.
        let one = |value: &str| locale_from([value.to_owned()]);
        assert_eq!(one("fr-FR"), "fr");
        assert_eq!(one("pt-BR"), "pt-BR");
        assert_eq!(one("zh-Hans-CN"), "zh-CN");
        assert_eq!(one("ja"), "ja");
    }

    #[test]
    fn an_unknown_system_language_lets_a_later_preference_through() {
        // A user whose first language has no catalogue gets their second
        // choice, not English.
        let tags = |list: &[&str]| locale_from(list.iter().map(|s| s.to_string()));
        assert_eq!(tags(&["ru-RU", "de-DE", "en-US"]), "de");
    }
}
