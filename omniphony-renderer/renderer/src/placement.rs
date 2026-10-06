//! Where a fixed channel goes: the placement policy, per source family.
//!
//! A fixed channel — a labelled PCM channel of a stream — has to be put
//! somewhere before it can be virtualised, and the formats disagree about
//! where their speakers are. Dolby's bed lives in a cube whose corners are
//! the speakers (`L` is the front-left corner of the room, whatever angle
//! that makes); Auro-3D states an angle for every speaker and asks for them
//! equidistant from the listener. So the policy is chosen per *family*, the
//! format the stream comes from, and has three modes:
//!
//! - **Sphere** — every channel is a direction on the listener's sphere,
//!   independent of the room: the angle the format declares for it (see
//!   `bridge_api::FormatBridge::fixed_channel_poses`), or the renderer's own
//!   nominal angle for the label otherwise.
//! - **Room** — every channel is a corner of the room model, stretched with
//!   the room ratio the way an object at that position is. Declared angles
//!   are ignored. The historical behaviour, and the Dolby one.
//! - **Manual** — the family's own layout entries give the poses; a channel
//!   without an entry falls back to Room.
//!
//! The mode only decides where a *virtualised* channel goes. In every mode
//! the family's entries still say whether a channel is virtualised or routed
//! direct to its speaker (`spatialize`) and what trim it gets (`gain_db`).
//!
//! The renderer knows no format by name. Its family table holds its own two
//! families — `generic`, the base every other inherits from, and `pcm`, its
//! own PCM input — and whatever the loaded bridge declares
//! (`bridge_api::BridgeLib::source_families`): a name, what to call it and
//! its default mode. A family the config names but no bridge declares stays
//! in the table too, so its settings survive a save under another bridge.
//!
//! Families inherit from the generic one: a family without an explicit mode
//! takes the generic mode when one is set, else its default (the bridge's;
//! room for the renderer's own families); a family without a layout uses the
//! generic layout. The generic family is also what a stream declaring no
//! family, or one missing from the table, gets.

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Mapping;

use crate::config::unknown_values::{self, EnumKey, KeepsUnknownValues};
use crate::speaker_layout::SpeakerLayout;

/// How a family's fixed channels are placed. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementMode {
    Sphere,
    Room,
    Manual,
}

impl PlacementMode {
    pub const ALL: [PlacementMode; 3] = [Self::Sphere, Self::Room, Self::Manual];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sphere => "sphere",
            Self::Room => "room",
            Self::Manual => "manual",
        }
    }

    /// The canonical spelling only, case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str().eq_ignore_ascii_case(s))
    }
}

/// The format a stream comes from, as far as placement is concerned: an
/// index into the [`PlacementState`] family table. Entries are only ever
/// appended, so an index stays valid for the life of the renderer; the name
/// a bridge declares is resolved to one once per declaration
/// ([`PlacementState::resolve`]), never per frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceFamily(u16);

impl SourceFamily {
    /// The base family: what an unknown or undeclared format gets, and what
    /// the others inherit from.
    pub const GENERIC: Self = Self(0);
    /// Plain multichannel PCM: the renderer's own PCM input.
    pub const PCM: Self = Self(1);

    fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// A family as the table knows it: its name (the config key and what a
/// bridge's stream declares), what to call it, and its default mode.
#[derive(Debug, Clone, PartialEq)]
pub struct FamilyInfo {
    pub name: String,
    pub label: String,
    /// The mode when neither the family nor the generic one sets one.
    pub default_mode: PlacementMode,
    /// Declared by the renderer itself or by the loaded bridge; `false` for
    /// a family known only from the config, kept so its settings are saved.
    pub declared: bool,
}

/// One family's own settings: both optional, each inherited from the generic
/// family when absent (see the module docs). This is also the config form of
/// a family (`render.placement.<family>`).
///
/// `Deserialize` and `Serialize` wrap the derived ones (`remote = "Self"`): a
/// mode this build does not know is kept rather than failing the file (see
/// `unknown_values`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct FamilyPlacement {
    #[serde(
        default,
        deserialize_with = "kept_mode",
        skip_serializing_if = "Option::is_none"
    )]
    pub mode: Option<PlacementMode>,
    /// The family's entries: `spatialize` and `gain_db` in every mode, the
    /// pose in manual mode. The speaker-layout schema, so the Studio editor
    /// and the config share one format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<SpeakerLayout>,
    /// The family's keys this build does not know, and a mode it does not
    /// know, kept so that a save writes them back.
    #[serde(flatten, default, skip_serializing_if = "Mapping::is_empty")]
    pub extra: Mapping,
}

impl FamilyPlacement {
    pub fn is_default(&self) -> bool {
        self.mode.is_none() && self.layout.is_none() && self.extra.is_empty()
    }

    /// Set the family's own mode, as a client chose it; `true` when it
    /// changed. A choice drops a mode a newer build wrote and this one kept,
    /// or it would come back the next time the family is set to inherit.
    /// Inherit while already inheriting is no choice (a client re-sending
    /// its state): this build runs the kept mode as inherit anyway.
    pub fn set_mode(&mut self, mode: Option<PlacementMode>) -> bool {
        if mode.is_some() || self.mode.is_some() {
            self.extra.shift_remove("mode");
        }
        std::mem::replace(&mut self.mode, mode) != mode
    }
}

/// `mode`, keeping one this build does not know (see [`unknown_values`]).
fn kept_mode<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PlacementMode>, D::Error> {
    unknown_values::keep_unknown(
        deserializer,
        None,
        "mode",
        "render.placement.<family>.",
        |value| Option::<PlacementMode>::deserialize(value),
    )
}

impl KeepsUnknownValues for FamilyPlacement {
    const ENUM_KEYS: &'static [EnumKey<Self>] = &[EnumKey {
        parent: None,
        key: "mode",
        // Absent inherits, which no mode spells.
        chosen: |own| own.mode.is_some(),
        clear: |own| own.mode = None,
    }];

    fn extra(&self, _parent: Option<&str>) -> Option<&Mapping> {
        Some(&self.extra)
    }

    fn extra_mut(&mut self, _parent: Option<&str>) -> Option<&mut Mapping> {
        Some(&mut self.extra)
    }
}

impl<'de> Deserialize<'de> for FamilyPlacement {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        unknown_values::deserialize(deserializer, FamilyPlacement::deserialize)
    }
}

impl Serialize for FamilyPlacement {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        unknown_values::serialize(self, serializer, |own, serializer| {
            FamilyPlacement::serialize(own, serializer)
        })
    }
}

/// What a family resolves to once inheritance is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectivePlacement<'a> {
    pub mode: PlacementMode,
    pub layout: Option<&'a SpeakerLayout>,
}

#[derive(Debug, Clone, PartialEq)]
struct FamilyEntry {
    info: FamilyInfo,
    own: FamilyPlacement,
}

/// The live placement state: the family table and every family's own
/// settings.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementState {
    families: Vec<FamilyEntry>,
}

impl Default for PlacementState {
    /// The renderer's own families, at their defaults.
    fn default() -> Self {
        let own = |name: &str, label: &str| FamilyEntry {
            info: FamilyInfo {
                name: name.to_owned(),
                label: label.to_owned(),
                default_mode: PlacementMode::Room,
                declared: true,
            },
            own: FamilyPlacement::default(),
        };
        Self {
            families: vec![own("generic", "Generic"), own("pcm", "PCM")],
        }
    }
}

impl PlacementState {
    /// The family named `name`, case-insensitive, declared or not.
    pub fn find(&self, name: &str) -> Option<SourceFamily> {
        let name = name.trim();
        self.families
            .iter()
            .position(|entry| entry.info.name.eq_ignore_ascii_case(name))
            .map(|index| SourceFamily(index as u16))
    }

    /// The family a stream's declaration maps to: the one of that name, or
    /// the generic family for an empty or unknown name.
    pub fn resolve(&self, name: &str) -> SourceFamily {
        if name.trim().is_empty() {
            return SourceFamily::GENERIC;
        }
        self.find(name).unwrap_or(SourceFamily::GENERIC)
    }

    /// Declare a family (the loaded bridge's catalogue): added to the table,
    /// or, when the config already named it, given its label and default.
    /// The renderer's own families keep theirs. An empty name is ignored.
    pub fn declare(&mut self, name: &str, label: &str, default_mode: PlacementMode) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match self.find(name) {
            Some(family) if family.index() < 2 => {}
            Some(family) => {
                let info = &mut self.families[family.index()].info;
                info.label = label.to_owned();
                info.default_mode = default_mode;
                info.declared = true;
            }
            None => self.families.push(FamilyEntry {
                info: FamilyInfo {
                    name: name.to_ascii_lowercase(),
                    label: label.to_owned(),
                    default_mode,
                    declared: true,
                },
                own: FamilyPlacement::default(),
            }),
        }
    }

    /// Every family in table order: the generic one first.
    pub fn families(&self) -> impl Iterator<Item = (SourceFamily, &FamilyInfo)> {
        self.families
            .iter()
            .enumerate()
            .map(|(index, entry)| (SourceFamily(index as u16), &entry.info))
    }

    pub fn info(&self, family: SourceFamily) -> &FamilyInfo {
        &self.families[family.index()].info
    }

    pub fn family(&self, family: SourceFamily) -> &FamilyPlacement {
        &self.families[family.index()].own
    }

    pub fn family_mut(&mut self, family: SourceFamily) -> &mut FamilyPlacement {
        &mut self.families[family.index()].own
    }

    /// The mode a family runs in: its own, else the generic one, else its
    /// default.
    pub fn effective_mode(&self, family: SourceFamily) -> PlacementMode {
        self.family(family)
            .mode
            .or(self.family(SourceFamily::GENERIC).mode)
            .unwrap_or(self.info(family).default_mode)
    }

    /// The entries a family uses: its own layout, else the generic one.
    pub fn effective_layout(&self, family: SourceFamily) -> Option<&SpeakerLayout> {
        self.family(family)
            .layout
            .as_ref()
            .or(self.family(SourceFamily::GENERIC).layout.as_ref())
    }

    pub fn effective(&self, family: SourceFamily) -> EffectivePlacement<'_> {
        EffectivePlacement {
            mode: self.effective_mode(family),
            layout: self.effective_layout(family),
        }
    }

    /// True when no family sets anything: the config key is then omitted.
    pub fn is_default(&self) -> bool {
        self.families.iter().all(|entry| entry.own.is_default())
    }

    /// Every family back to its defaults; the table itself is kept.
    pub fn reset_settings(&mut self) {
        for entry in &mut self.families {
            entry.own = FamilyPlacement::default();
        }
    }

    /// The config form, `None` when everything is at its default.
    pub fn to_config(&self) -> Option<PlacementConfig> {
        if self.is_default() {
            return None;
        }
        let families = self
            .families
            .iter()
            .filter(|entry| !entry.own.is_default())
            .map(|entry| {
                let own = FamilyPlacement {
                    mode: entry.own.mode,
                    layout: entry.own.layout.clone().map(|mut layout| {
                        // Round the radius for stable diffs, as the legacy
                        // `virtual_bed` key did.
                        layout.radius_m = (layout.radius_m as f64 * 1e6).round() as f32 / 1e6;
                        layout
                    }),
                    extra: entry.own.extra.clone(),
                };
                (entry.info.name.clone(), own)
            })
            .collect();
        Some(PlacementConfig { families })
    }

    /// Take a config's settings: every family's are replaced, and a family
    /// the table does not have yet is added, undeclared, so they are kept.
    pub fn load_config(&mut self, config: &PlacementConfig) {
        self.reset_settings();
        for (name, own) in &config.families {
            let family = match self.find(name) {
                Some(family) => family,
                None if name.trim().is_empty() => continue,
                None => {
                    self.families.push(FamilyEntry {
                        info: FamilyInfo {
                            name: name.trim().to_ascii_lowercase(),
                            label: name.trim().to_owned(),
                            default_mode: PlacementMode::Room,
                            declared: false,
                        },
                        own: FamilyPlacement::default(),
                    });
                    SourceFamily((self.families.len() - 1) as u16)
                }
            };
            *self.family_mut(family) = own.clone();
        }
    }

    /// Take a pre-placement config: its single global bed was applied to
    /// every stream, which is the generic family in manual mode with those
    /// entries — the same sound after the upgrade as before it.
    pub fn load_legacy_virtual_bed(&mut self, layout: SpeakerLayout) {
        self.reset_settings();
        *self.family_mut(SourceFamily::GENERIC) = FamilyPlacement {
            mode: Some(PlacementMode::Manual),
            layout: Some(layout),
            ..Default::default()
        };
    }
}

/// `render.placement`: one optional block per family, keyed by the family's
/// name, in table order (the generic family first). Absent families are at
/// their defaults (inheriting from `generic`, itself at the defaults). A
/// family no loaded bridge declares is read and written back unchanged.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlacementConfig {
    pub families: Vec<(String, FamilyPlacement)>,
}

impl PlacementConfig {
    /// The block for `name`, if the config has one.
    pub fn get(&self, name: &str) -> Option<&FamilyPlacement> {
        self.families
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, own)| own)
    }
}

impl Serialize for PlacementConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.families.len()))?;
        for (name, own) in &self.families {
            map.serialize_entry(name, own)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for PlacementConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Families;
        impl<'de> serde::de::Visitor<'de> for Families {
            type Value = PlacementConfig;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of source family names to placement settings")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut families = Vec::new();
                while let Some((name, own)) =
                    access.next_entry::<String, Option<FamilyPlacement>>()?
                {
                    // `dolby: ~` is a family at its defaults.
                    families.push((name, own.unwrap_or_default()));
                }
                Ok(PlacementConfig { families })
            }
        }
        deserializer.deserialize_map(Families)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bed() -> SpeakerLayout {
        SpeakerLayout::preset("5.1").expect("5.1 preset")
    }

    /// The table a bridge declaring two families leaves.
    fn with_bridge() -> (PlacementState, SourceFamily, SourceFamily) {
        let mut state = PlacementState::default();
        state.declare("dts", "DTS", PlacementMode::Room);
        state.declare("auro", "Auro-3D", PlacementMode::Sphere);
        let dts = state.find("dts").expect("declared");
        let auro = state.find("auro").expect("declared");
        (state, dts, auro)
    }

    #[test]
    fn the_renderer_knows_only_its_own_families() {
        let state = PlacementState::default();
        let names: Vec<&str> = state
            .families()
            .map(|(_, info)| info.name.as_str())
            .collect();
        assert_eq!(names, ["generic", "pcm"]);
        assert_eq!(state.find("pcm"), Some(SourceFamily::PCM));
        assert_eq!(state.resolve("dolby"), SourceFamily::GENERIC);
        assert_eq!(state.resolve(""), SourceFamily::GENERIC);
    }

    #[test]
    fn declared_defaults_apply_and_the_rest_is_a_room() {
        let (state, dts, auro) = with_bridge();
        assert!(state.is_default());
        assert_eq!(state.effective_mode(auro), PlacementMode::Sphere);
        for family in [SourceFamily::GENERIC, SourceFamily::PCM, dts] {
            assert_eq!(
                state.effective_mode(family),
                PlacementMode::Room,
                "{family:?}"
            );
            assert!(state.effective_layout(family).is_none());
        }
        assert_eq!(state.resolve("AURO"), auro);
        assert_eq!(state.info(auro).label, "Auro-3D");
    }

    #[test]
    fn a_bridge_cannot_redefine_the_renderer_families() {
        let mut state = PlacementState::default();
        state.declare("pcm", "Linear PCM", PlacementMode::Sphere);
        state.declare("generic", "Other", PlacementMode::Sphere);
        assert_eq!(state.families().count(), 2);
        assert_eq!(state.info(SourceFamily::PCM).label, "PCM");
        assert_eq!(state.effective_mode(SourceFamily::PCM), PlacementMode::Room);
    }

    #[test]
    fn a_family_inherits_from_generic_unless_it_says_otherwise() {
        let (mut state, dts, auro) = with_bridge();
        state.family_mut(SourceFamily::GENERIC).mode = Some(PlacementMode::Manual);
        state.family_mut(SourceFamily::GENERIC).layout = Some(bed());
        // An explicit generic mode beats a declared default, Auro's too.
        assert_eq!(state.effective_mode(auro), PlacementMode::Manual);
        assert_eq!(state.effective_mode(dts), PlacementMode::Manual);
        assert!(state.effective_layout(dts).is_some());
        // The family's own setting wins over generic.
        state.family_mut(auro).mode = Some(PlacementMode::Sphere);
        assert_eq!(state.effective_mode(auro), PlacementMode::Sphere);
        // …and its own layout too, while the mode keeps inheriting.
        let mut own = bed();
        own.radius_m = 2.0;
        state.family_mut(dts).layout = Some(own);
        assert_eq!(state.effective_layout(dts).map(|l| l.radius_m), Some(2.0));
        assert_eq!(state.effective_mode(dts), PlacementMode::Manual);
    }

    #[test]
    fn config_round_trips_in_table_order_and_omits_defaults() {
        let (mut state, _, auro) = with_bridge();
        assert!(state.to_config().is_none());
        state.family_mut(auro).mode = Some(PlacementMode::Room);
        state.family_mut(SourceFamily::GENERIC).layout = Some(bed());
        let config = state.to_config().expect("non-default");
        assert!(config.get("dts").is_none() && config.get("pcm").is_none());
        let yaml = serde_yaml_ng::to_string(&config).expect("serialises");
        assert!(yaml.starts_with("generic:"), "{yaml}");
        assert!(yaml.contains("auro:"), "{yaml}");
        assert!(yaml.contains("mode: room"), "{yaml}");
        assert!(!yaml.contains("dts"), "{yaml}");
        let back: PlacementConfig = serde_yaml_ng::from_str(&yaml).expect("parses");
        let mut reloaded = with_bridge().0;
        reloaded.load_config(&back);
        assert_eq!(reloaded, state);
    }

    #[test]
    fn a_family_no_bridge_declares_is_kept_and_takes_its_declaration_later() {
        let config: PlacementConfig =
            serde_yaml_ng::from_str("iamf:\n  mode: room\ndolby: ~\n").expect("parses");
        let mut state = PlacementState::default();
        state.load_config(&config);
        let iamf = state.find("iamf").expect("kept from the config");
        assert!(!state.info(iamf).declared);
        assert_eq!(state.family(iamf).mode, Some(PlacementMode::Room));
        // Saved back although nothing declares it; the empty block is not.
        let saved = serde_yaml_ng::to_string(&state.to_config().expect("set")).unwrap();
        assert_eq!(saved, "iamf:\n  mode: room\n");
        // The bridge loads afterwards: same entry, its label and default.
        state.declare("iamf", "IAMF", PlacementMode::Sphere);
        assert_eq!(state.find("iamf"), Some(iamf));
        assert!(state.info(iamf).declared);
        assert_eq!(state.info(iamf).label, "IAMF");
        assert_eq!(
            state.effective_mode(iamf),
            PlacementMode::Room,
            "own mode kept"
        );
    }

    #[test]
    fn loading_a_config_keeps_the_table_and_its_indices() {
        let (mut state, dts, _) = with_bridge();
        state.family_mut(dts).mode = Some(PlacementMode::Sphere);
        state.load_config(&PlacementConfig::default());
        assert!(state.is_default());
        assert_eq!(state.find("dts"), Some(dts));
    }

    #[test]
    fn a_legacy_bed_becomes_the_generic_family_in_manual_mode() {
        let (mut state, _, _) = with_bridge();
        state.load_legacy_virtual_bed(bed());
        let families: Vec<SourceFamily> = state.families().map(|(family, _)| family).collect();
        for family in families {
            assert_eq!(
                state.effective_mode(family),
                PlacementMode::Manual,
                "{family:?}"
            );
            assert!(state.effective_layout(family).is_some(), "{family:?}");
        }
    }

    #[test]
    fn modes_parse_canonically() {
        assert_eq!(
            PlacementMode::parse(" Sphere "),
            Some(PlacementMode::Sphere)
        );
        assert_eq!(PlacementMode::parse("cube"), None);
    }
}
