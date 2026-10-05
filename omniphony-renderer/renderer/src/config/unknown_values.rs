//! Enum-typed config values this build does not know.
//!
//! A newer build may write a value into an enum-typed key that this one cannot
//! read: a new crossover type, a new input clock. Read as is, that one value
//! fails the whole file, which then runs on defaults and, since the parse
//! guard, refuses every save. Instead the value is taken out before the
//! section is read, so the field falls back to its default with a warning, and
//! kept in the section's `extra` mapping under its own key, where unknown keys
//! already live: a save writes it back unchanged.
//!
//! On save, a kept value yields to the field only when the field holds a
//! choice of its own, a value other than the one an absent key stands for. A
//! save writes every live setting, so the field is set by then; written as is,
//! it would replace the newer build's value with this build's fallback. A
//! value this build reads back exactly as it reads the kept one is not a
//! choice, so the kept value is written in its place.

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serializer};
use serde_yaml_ng::{Mapping, Value};

/// One enum-typed key of a config section.
pub(crate) struct EnumKey<S> {
    /// The mapping inside the section that holds the key (`live_input`), or
    /// `None` for a key of the section itself.
    pub parent: Option<&'static str>,
    pub key: &'static str,
    /// Whether this build reads `value`: the field's own deserializer, with
    /// its aliases and retired values.
    pub understood: fn(&Value) -> bool,
    /// Whether the field holds a choice of its own: a value other than the
    /// one an absent key stands for.
    pub chosen: fn(&S) -> bool,
    /// Empty the field, so that a kept value is written in its place.
    pub clear: fn(&mut S),
}

/// A config section that keeps the enum values it does not know.
pub(crate) trait KeepsUnknownValues: Clone + Sized + 'static {
    /// The section's name, for the warning.
    const SECTION: &'static str;
    const ENUM_KEYS: &'static [EnumKey<Self>];
    /// The `extra` mapping of the section (`parent: None`) or of the mapping
    /// `parent` inside it, when it is present.
    fn extra(&self, parent: Option<&str>) -> Option<&Mapping>;
    fn extra_mut(&mut self, parent: Option<&str>) -> Option<&mut Mapping>;
}

/// [`EnumKey::understood`] for a field read by `T`'s own `Deserialize`.
pub(crate) fn understood<T: DeserializeOwned>(value: &Value) -> bool {
    serde_yaml_ng::from_value::<Option<T>>(value.clone()).is_ok()
}

/// Deserialize a section through `fields` (its derived deserializer), after
/// taking out the enum values it does not know; they end up in the section's
/// `extra` mappings.
pub(crate) fn deserialize<'de, S, D>(
    deserializer: D,
    fields: fn(Value) -> Result<S, serde_yaml_ng::Error>,
) -> Result<S, D::Error>
where
    S: KeepsUnknownValues,
    D: Deserializer<'de>,
{
    let mut value = Value::deserialize(deserializer)?;
    let mut kept = Vec::new();
    for key in S::ENUM_KEYS {
        let map = match key.parent {
            None => value.as_mapping_mut(),
            Some(parent) => value.get_mut(parent).and_then(Value::as_mapping_mut),
        };
        let Some(map) = map else { continue };
        if map.get(key.key).is_none_or(key.understood) {
            continue;
        }
        let Some(raw) = map.shift_remove(key.key) else {
            continue;
        };
        log::warn!(
            "config: {} '{}' is not a value this build knows (written by a newer one?); using \
             the default and keeping the value for the next save",
            key_path(S::SECTION, key),
            serde_yaml_ng::to_string(&raw)
                .unwrap_or_default()
                .trim_end()
        );
        kept.push((key, raw));
    }
    let mut section = fields(value).map_err(D::Error::custom)?;
    for (key, raw) in kept {
        // The parent mapping held the key, so the section read it.
        if let Some(extra) = section.extra_mut(key.parent) {
            extra.insert(Value::String(key.key.to_owned()), raw);
        }
    }
    Ok(section)
}

/// Serialize a section through `fields` (its derived serializer): every kept
/// value once, either in place of the field or dropped for the field's own
/// choice.
pub(crate) fn serialize<S, Z>(
    section: &S,
    serializer: Z,
    fields: fn(&S, Z) -> Result<Z::Ok, Z::Error>,
) -> Result<Z::Ok, Z::Error>
where
    S: KeepsUnknownValues,
    Z: Serializer,
{
    let is_kept = |key: &&EnumKey<S>| {
        section
            .extra(key.parent)
            .is_some_and(|extra| extra.contains_key(key.key))
    };
    // The common case: nothing kept, nothing to copy.
    if !S::ENUM_KEYS.iter().any(|key| is_kept(&key)) {
        return fields(section, serializer);
    }
    let mut out = section.clone();
    for key in S::ENUM_KEYS.iter().filter(is_kept) {
        if (key.chosen)(section) {
            if let Some(extra) = out.extra_mut(key.parent) {
                extra.shift_remove(key.key);
            }
        } else {
            (key.clear)(&mut out);
        }
    }
    fields(&out, serializer)
}

fn key_path<S>(section: &str, key: &EnumKey<S>) -> String {
    match key.parent {
        Some(parent) => format!("{section}.{parent}.{}", key.key),
        None => format!("{section}.{}", key.key),
    }
}
