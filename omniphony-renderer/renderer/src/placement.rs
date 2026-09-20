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
//! Families inherit from [`SourceFamily::Generic`]: a family without an
//! explicit mode takes the generic mode when one is set, else its built-in
//! default (Auro-3D: sphere; everything else: room); a family without a
//! layout uses the generic layout. The generic family is also what a bridge
//! that declares no family, or an unknown one, gets.

use serde::{Deserialize, Serialize};

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

/// The format a stream comes from, as far as placement is concerned.
///
/// Declared by the bridge as a string (`FormatBridge::source_family`), so a
/// format the renderer does not know yet costs no ABI change: it lands on
/// [`Self::Generic`], the base every other family inherits from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceFamily {
    /// The base family: what an unknown or undeclared format gets, and what
    /// the others inherit from.
    Generic,
    /// AC-3, E-AC-3 and TrueHD, with or without objects: the bed is defined
    /// in Dolby's room cube.
    Dolby,
    /// DTS, DTS-HD and DTS:X: ITU-based speaker angles.
    Dts,
    /// An unfolded Auro-3D carrier: its own setup table, a sphere.
    Auro,
    /// Plain multichannel PCM (the reference WAV bridge, host PCM).
    Pcm,
}

impl SourceFamily {
    pub const ALL: [SourceFamily; 5] =
        [Self::Generic, Self::Dolby, Self::Dts, Self::Auro, Self::Pcm];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::Dolby => "dolby",
            Self::Dts => "dts",
            Self::Auro => "auro",
            Self::Pcm => "pcm",
        }
    }

    /// The canonical spelling only, case-insensitive; `None` for anything
    /// else (a control naming a family the renderer does not have).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|family| family.as_str().eq_ignore_ascii_case(s))
    }

    /// The family a bridge's declaration maps to: a known name, or
    /// [`Self::Generic`] for an empty or unknown one.
    pub fn from_declared(s: &str) -> Self {
        Self::parse(s).unwrap_or(Self::Generic)
    }

    /// The mode a family runs in when neither it nor the generic family
    /// sets one: Auro-3D is a sphere by definition, everything else keeps
    /// the room model it always had.
    pub fn builtin_mode(self) -> PlacementMode {
        match self {
            Self::Auro => PlacementMode::Sphere,
            _ => PlacementMode::Room,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Generic => 0,
            Self::Dolby => 1,
            Self::Dts => 2,
            Self::Auro => 3,
            Self::Pcm => 4,
        }
    }
}

/// One family's own settings: both optional, each inherited from the generic
/// family when absent (see the module docs). This is also the config form of
/// a family (`render.placement.<family>`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FamilyPlacement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PlacementMode>,
    /// The family's entries: `spatialize` and `gain_db` in every mode, the
    /// pose in manual mode. The speaker-layout schema, so the Studio editor
    /// and the config share one format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<SpeakerLayout>,
}

impl FamilyPlacement {
    pub fn is_default(&self) -> bool {
        self.mode.is_none() && self.layout.is_none()
    }
}

/// What a family resolves to once inheritance is applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EffectivePlacement<'a> {
    pub mode: PlacementMode,
    pub layout: Option<&'a SpeakerLayout>,
}

/// The live placement state: every family's own settings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlacementState {
    families: [FamilyPlacement; 5],
}

impl PlacementState {
    pub fn family(&self, family: SourceFamily) -> &FamilyPlacement {
        &self.families[family.index()]
    }

    pub fn family_mut(&mut self, family: SourceFamily) -> &mut FamilyPlacement {
        &mut self.families[family.index()]
    }

    /// The mode a family runs in: its own, else the generic one, else its
    /// built-in default.
    pub fn effective_mode(&self, family: SourceFamily) -> PlacementMode {
        self.family(family)
            .mode
            .or(self.family(SourceFamily::Generic).mode)
            .unwrap_or_else(|| family.builtin_mode())
    }

    /// The entries a family uses: its own layout, else the generic one.
    pub fn effective_layout(&self, family: SourceFamily) -> Option<&SpeakerLayout> {
        self.family(family)
            .layout
            .as_ref()
            .or(self.family(SourceFamily::Generic).layout.as_ref())
    }

    pub fn effective(&self, family: SourceFamily) -> EffectivePlacement<'_> {
        EffectivePlacement {
            mode: self.effective_mode(family),
            layout: self.effective_layout(family),
        }
    }

    /// True when no family sets anything: the config key is then omitted.
    pub fn is_default(&self) -> bool {
        self.families.iter().all(FamilyPlacement::is_default)
    }

    /// The config form, `None` when everything is at its default.
    pub fn to_config(&self) -> Option<PlacementConfig> {
        if self.is_default() {
            return None;
        }
        let field = |family: SourceFamily| {
            let own = self.family(family);
            (!own.is_default()).then(|| FamilyPlacement {
                mode: own.mode,
                layout: own.layout.clone().map(|mut layout| {
                    // Round the radius for stable diffs, as the legacy
                    // `virtual_bed` key did.
                    layout.radius_m = (layout.radius_m as f64 * 1e6).round() as f32 / 1e6;
                    layout
                }),
            })
        };
        Some(PlacementConfig {
            generic: field(SourceFamily::Generic),
            dolby: field(SourceFamily::Dolby),
            dts: field(SourceFamily::Dts),
            auro: field(SourceFamily::Auro),
            pcm: field(SourceFamily::Pcm),
        })
    }

    pub fn from_config(config: &PlacementConfig) -> Self {
        let mut state = Self::default();
        let mut take = |family: SourceFamily, own: &Option<FamilyPlacement>| {
            if let Some(own) = own {
                *state.family_mut(family) = own.clone();
            }
        };
        take(SourceFamily::Generic, &config.generic);
        take(SourceFamily::Dolby, &config.dolby);
        take(SourceFamily::Dts, &config.dts);
        take(SourceFamily::Auro, &config.auro);
        take(SourceFamily::Pcm, &config.pcm);
        state
    }

    /// The state a pre-placement config maps to: its single global bed was
    /// applied to every stream, which is the generic family in manual mode
    /// with those entries — the same sound after the upgrade as before it.
    pub fn from_legacy_virtual_bed(layout: SpeakerLayout) -> Self {
        let mut state = Self::default();
        *state.family_mut(SourceFamily::Generic) = FamilyPlacement {
            mode: Some(PlacementMode::Manual),
            layout: Some(layout),
        };
        state
    }
}

/// `render.placement`: one optional block per family. Absent families are at
/// their defaults (inheriting from `generic`, itself at the built-in
/// defaults).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PlacementConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generic: Option<FamilyPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dolby: Option<FamilyPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dts: Option<FamilyPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auro: Option<FamilyPlacement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm: Option<FamilyPlacement>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bed() -> SpeakerLayout {
        SpeakerLayout::preset("5.1").expect("5.1 preset")
    }

    #[test]
    fn builtin_defaults_make_auro_a_sphere_and_the_rest_a_room() {
        let state = PlacementState::default();
        assert!(state.is_default());
        assert_eq!(
            state.effective_mode(SourceFamily::Auro),
            PlacementMode::Sphere
        );
        for family in [
            SourceFamily::Generic,
            SourceFamily::Dolby,
            SourceFamily::Dts,
            SourceFamily::Pcm,
        ] {
            assert_eq!(
                state.effective_mode(family),
                PlacementMode::Room,
                "{family:?}"
            );
            assert!(state.effective_layout(family).is_none());
        }
    }

    #[test]
    fn a_family_inherits_from_generic_unless_it_says_otherwise() {
        let mut state = PlacementState::default();
        state.family_mut(SourceFamily::Generic).mode = Some(PlacementMode::Manual);
        state.family_mut(SourceFamily::Generic).layout = Some(bed());
        // An explicit generic mode beats the built-in default, Auro's too.
        assert_eq!(
            state.effective_mode(SourceFamily::Auro),
            PlacementMode::Manual
        );
        assert_eq!(
            state.effective_mode(SourceFamily::Dts),
            PlacementMode::Manual
        );
        assert!(state.effective_layout(SourceFamily::Dts).is_some());
        // The family's own setting wins over generic.
        state.family_mut(SourceFamily::Auro).mode = Some(PlacementMode::Sphere);
        assert_eq!(
            state.effective_mode(SourceFamily::Auro),
            PlacementMode::Sphere
        );
        // …and its own layout too, while the mode keeps inheriting.
        let mut own = bed();
        own.radius_m = 2.0;
        state.family_mut(SourceFamily::Dts).layout = Some(own);
        assert_eq!(
            state
                .effective_layout(SourceFamily::Dts)
                .map(|l| l.radius_m),
            Some(2.0)
        );
        assert_eq!(
            state.effective_mode(SourceFamily::Dts),
            PlacementMode::Manual
        );
    }

    #[test]
    fn config_round_trips_and_omits_defaults() {
        let mut state = PlacementState::default();
        assert!(state.to_config().is_none());
        state.family_mut(SourceFamily::Auro).mode = Some(PlacementMode::Room);
        state.family_mut(SourceFamily::Generic).layout = Some(bed());
        let config = state.to_config().expect("non-default");
        assert!(config.dolby.is_none() && config.dts.is_none() && config.pcm.is_none());
        let yaml = serde_yaml_ng::to_string(&config).expect("serialises");
        assert!(yaml.contains("auro:"), "{yaml}");
        assert!(yaml.contains("mode: room"), "{yaml}");
        assert!(!yaml.contains("dolby"), "{yaml}");
        let back: PlacementConfig = serde_yaml_ng::from_str(&yaml).expect("parses");
        assert_eq!(PlacementState::from_config(&back), state);
    }

    #[test]
    fn a_legacy_bed_becomes_the_generic_family_in_manual_mode() {
        let state = PlacementState::from_legacy_virtual_bed(bed());
        for family in SourceFamily::ALL {
            assert_eq!(
                state.effective_mode(family),
                PlacementMode::Manual,
                "{family:?}"
            );
            assert!(state.effective_layout(family).is_some(), "{family:?}");
        }
    }

    #[test]
    fn names_parse_canonically_and_unknown_families_are_generic() {
        assert_eq!(
            PlacementMode::parse(" Sphere "),
            Some(PlacementMode::Sphere)
        );
        assert_eq!(PlacementMode::parse("cube"), None);
        assert_eq!(SourceFamily::parse("DTS"), Some(SourceFamily::Dts));
        assert_eq!(SourceFamily::from_declared("mpeg-h"), SourceFamily::Generic);
        assert_eq!(SourceFamily::from_declared(""), SourceFamily::Generic);
        for family in SourceFamily::ALL {
            assert_eq!(SourceFamily::parse(family.as_str()), Some(family));
        }
    }
}
