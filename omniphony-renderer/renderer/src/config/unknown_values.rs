//! Enum-typed config values this build does not know.
//!
//! A newer build may write a value into an enum-typed key that this one cannot
//! read: a new crossover type, a new input clock. Read as is, that one value
//! fails the whole file, which then runs on defaults and, since the parse
//! guard, refuses every save. Instead the field reads it leniently
//! ([`keep_unknown`]): it falls back to its default with a warning, and the
//! value is kept in the section's `extra` mapping under its own key, where
//! unknown keys already live, so that a save writes it back unchanged.
//!
//! Only the enum field itself is read through a [`Value`]. The rest of the
//! section goes through the format's own deserializer as before, with its
//! conversions (YAML reads `output_device: 0` into a string; a `Value` would
//! not).
//!
//! On save, a kept value yields to the field only when the field holds a
//! choice of its own, a value other than the one an absent key stands for. A
//! save writes every live setting, so the field is set by then; written as is,
//! it would replace the newer build's value with this build's fallback. A
//! value this build reads back exactly as it reads the kept one is not a
//! choice, so the kept value is written in its place.

use std::cell::RefCell;

use serde::{Deserialize, Deserializer, Serializer};
use serde_yaml_ng::{Mapping, Value};

/// One enum-typed key of a config section, for the save side.
pub(crate) struct EnumKey<S> {
    /// The mapping inside the section that holds the key (`live_input`), or
    /// `None` for a key of the section itself.
    pub parent: Option<&'static str>,
    pub key: &'static str,
    /// Whether the field holds a choice of its own: a value other than the
    /// one an absent key stands for.
    pub chosen: fn(&S) -> bool,
    /// Empty the field, so that a kept value is written in its place.
    pub clear: fn(&mut S),
}

/// A config section that keeps the enum values it does not know.
pub(crate) trait KeepsUnknownValues: Clone + Sized + 'static {
    const ENUM_KEYS: &'static [EnumKey<Self>];
    /// The `extra` mapping of the section (`parent: None`) or of the mapping
    /// `parent` inside it, when it is present.
    fn extra(&self, parent: Option<&str>) -> Option<&Mapping>;
    fn extra_mut(&mut self, parent: Option<&str>) -> Option<&mut Mapping>;
}

/// A value [`keep_unknown`] took out, waiting for its section.
struct Kept {
    parent: Option<&'static str>,
    key: &'static str,
    value: Value,
}

#[derive(Default)]
struct Stash {
    /// Sections being read on this thread; outside one, nothing is kept.
    depth: usize,
    kept: Vec<Kept>,
}

thread_local! {
    // Deserialization is synchronous: a section's fields run on the thread
    // that reads the section, between its `deserialize` entry and exit.
    static STASH: RefCell<Stash> = RefCell::default();
}

/// `deserialize_with` body of an enum-typed field: `read` (the field's own
/// reading, aliases and retired values included) or, for a value it refuses,
/// `None` with the value kept for the enclosing section's `extra`.
/// `section_path` names the key for the warning.
pub(crate) fn keep_unknown<'de, D, T>(
    deserializer: D,
    parent: Option<&'static str>,
    key: &'static str,
    section_path: &str,
    read: fn(Value) -> Result<Option<T>, serde_yaml_ng::Error>,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    if let Ok(known) = read(value.clone()) {
        return Ok(known);
    }
    log::warn!(
        "config: {section_path}{key} '{}' is not a value this build knows (written by a newer \
         one?); using the default and keeping the value for the next save",
        serde_yaml_ng::to_string(&value)
            .unwrap_or_default()
            .trim_end()
    );
    STASH.with(|stash| {
        let mut stash = stash.borrow_mut();
        if stash.depth > 0 {
            stash.kept.push(Kept { parent, key, value });
        }
    });
    Ok(None)
}

/// Deserialize a section through `fields` (its derived deserializer),
/// putting the values its enum fields kept into its `extra` mappings.
pub(crate) fn deserialize<'de, S, D>(
    deserializer: D,
    fields: impl FnOnce(D) -> Result<S, D::Error>,
) -> Result<S, D::Error>
where
    S: KeepsUnknownValues,
    D: Deserializer<'de>,
{
    /// Closes the section on every exit, an error included.
    struct Scope(usize);
    impl Drop for Scope {
        fn drop(&mut self) {
            STASH.with(|stash| {
                let mut stash = stash.borrow_mut();
                stash.depth -= 1;
                stash.kept.truncate(self.0);
            });
        }
    }
    let scope = STASH.with(|stash| {
        let mut stash = stash.borrow_mut();
        stash.depth += 1;
        Scope(stash.kept.len())
    });
    let mut section = fields(deserializer)?;
    // A nested section took its own values out before returning: what is
    // left past the mark is this section's.
    let kept = STASH.with(|stash| stash.borrow_mut().kept.split_off(scope.0));
    for Kept { parent, key, value } in kept {
        if let Some(extra) = section.extra_mut(parent) {
            extra.insert(Value::String(key.to_owned()), value);
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
