//! Studio strings. The English catalogue is the web Studio's `en.json`,
//! embedded at build time so both hosts stay key-for-key identical until the
//! cutover moves the locales into this crate. `t` resolves a key, `tf`
//! substitutes `{name}` placeholders like the web `tf`.

use std::collections::HashMap;
use std::sync::OnceLock;

const EN_JSON: &str = include_str!("../../omniphony-studio/src/i18n/en.json");

fn catalogue() -> &'static HashMap<String, String> {
    static CAT: OnceLock<HashMap<String, String>> = OnceLock::new();
    CAT.get_or_init(|| {
        serde_json::from_str::<HashMap<String, String>>(EN_JSON).unwrap_or_else(|e| {
            log::error!("[i18n] en.json: {e}");
            HashMap::new()
        })
    })
}

/// The string for `key`, or the key itself when it is missing (like the web).
pub fn t(key: &str) -> &'static str {
    match catalogue().get(key) {
        Some(s) => s.as_str(),
        None => {
            log::debug!("[i18n] missing key {key}");
            leak_key(key)
        }
    }
}

fn leak_key(key: &str) -> &'static str {
    // Missing keys are a development-time defect; a small leak per distinct
    // key keeps the signature borrow-free.
    Box::leak(key.to_owned().into_boxed_str())
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
    fn catalogue_loads_and_substitutes() {
        assert_eq!(t("app.title"), "Omniphony Studio");
        assert_eq!(
            tf("updates.available", &[("version", "1.2.3")]),
            "Update available: 1.2.3"
        );
        assert_eq!(t("no.such.key"), "no.such.key");
    }
}
