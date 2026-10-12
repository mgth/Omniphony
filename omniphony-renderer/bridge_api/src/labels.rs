//! Canonical naming and alias matching for [`RChannelLabel`].
//!
//! Single source of truth relating a channel label to its canonical short
//! name and to the spellings accepted from layout YAMLs, hand-edited configs
//! and format channel tables. Every mapping between speaker names, channel
//! labels and display names in the stack must go through this module — see
//! `docs/channel-object-contract.md` ("Naming").
//!
//! Matching is alias-tolerant: names are normalised by uppercasing and
//! stripping whitespace, `_` and `-`, so `"Top Front Left"`,
//! `"TOP_FRONT_LEFT"` and `"TFL"` all resolve to [`RChannelLabel::Tfl`].

use crate::RChannelLabel;

/// Accepted spellings per label, in normalised form (uppercase, no
/// whitespace/`_`/`-`). The first entry of each list is only an alias like
/// the others; canonical display names come from [`canonical_name`].
///
/// This table is the union of the historical matchers it replaces; parity
/// with both is pinned by tests here and in the consuming crates
/// (`renderer::speaker_layout` keeps the legacy-alias parity net).
///
/// The `orender_engine` virtual-bed planner matches bed entries through this
/// table too ([`name_matches`]), plus one context-dependent rule of its own:
/// a 4.x/5.x source's surround pair falls back to a back entry when the bed
/// has no surround one.
const ALIASES: &[(RChannelLabel, &[&str])] = &[
    (RChannelLabel::L, &["FL", "L", "FRONTLEFT", "LEFTFRONT"]),
    (RChannelLabel::R, &["FR", "R", "FRONTRIGHT", "RIGHTFRONT"]),
    (
        RChannelLabel::C,
        &["C", "FC", "CENTER", "CENTRE", "FRONTCENTER"],
    ),
    (
        RChannelLabel::LFE,
        &["LFE", "LFE1", "SUB", "SUBWOOFER", "SW"],
    ),
    (RChannelLabel::LFE2, &["LFE2"]),
    (
        RChannelLabel::Ls,
        &["SL", "LS", "SIDELEFT", "SURROUNDLEFT", "LEFTSURROUND"],
    ),
    (
        RChannelLabel::Rs,
        &["SR", "RS", "SIDERIGHT", "SURROUNDRIGHT", "RIGHTSURROUND"],
    ),
    (
        RChannelLabel::Lb,
        &[
            "BL", "LB", "LRS", "BACKLEFT", "LEFTBACK", "REARLEFT", "LEFTREAR",
        ],
    ),
    (
        RChannelLabel::Rb,
        &[
            "BR",
            "RB",
            "RRS",
            "BACKRIGHT",
            "RIGHTBACK",
            "REARRIGHT",
            "RIGHTREAR",
        ],
    ),
    (
        RChannelLabel::Cb,
        &["RC", "BC", "CB", "BACKCENTER", "REARCENTER", "CENTERBACK"],
    ),
    // Front-left/right of center (wide-front center pair).
    (
        RChannelLabel::Lsc,
        &["LSC", "FLC", "FRONTLEFTCENTER", "LEFTCENTER"],
    ),
    (
        RChannelLabel::Rsc,
        &["RSC", "FRC", "FRONTRIGHTCENTER", "RIGHTCENTER"],
    ),
    // Front wide.
    (
        RChannelLabel::Lw,
        &["FWL", "LW", "WL", "WIDELEFT", "FRONTWIDELEFT"],
    ),
    (
        RChannelLabel::Rw,
        &["FWR", "RW", "WR", "WIDERIGHT", "FRONTWIDERIGHT"],
    ),
    // Side direct (between side surround and back), where layouts use it.
    (RChannelLabel::Lsd, &["LSD"]),
    (RChannelLabel::Rsd, &["RSD"]),
    // Height / top tier.
    (
        RChannelLabel::Tfl,
        &[
            "TFL",
            "TPFL",
            "TOPFRONTLEFT",
            "UPPERFRONTLEFT",
            "UFL",
            "LTF",
            "LEFTTOPFRONT",
            "HEIGHTLEFT",
            "HL",
        ],
    ),
    (
        RChannelLabel::Tfr,
        &[
            "TFR",
            "TPFR",
            "TOPFRONTRIGHT",
            "UPPERFRONTRIGHT",
            "UFR",
            "RTF",
            "RIGHTTOPFRONT",
            "HEIGHTRIGHT",
            "HR",
        ],
    ),
    (
        RChannelLabel::Tsl,
        &["TSL", "TPSL", "TOPSIDELEFT", "UPPERSIDELEFT", "USL"],
    ),
    (
        RChannelLabel::Tsr,
        &["TSR", "TPSR", "TOPSIDERIGHT", "UPPERSIDERIGHT", "USR"],
    ),
    (
        RChannelLabel::Tbl,
        &[
            "TBL",
            "TPBL",
            "TOPBACKLEFT",
            "TOPREARLEFT",
            "UBL",
            "TRL",
            "LTR",
            "UPPERBACKLEFT",
        ],
    ),
    (
        RChannelLabel::Tbr,
        &[
            "TBR",
            "TPBR",
            "TOPBACKRIGHT",
            "TOPREARRIGHT",
            "UBR",
            "TRR",
            "RTR",
            "UPPERBACKRIGHT",
        ],
    ),
    (
        RChannelLabel::Tc,
        &["TC", "TPC", "TOPCENTER", "TOPMIDDLECENTER"],
    ),
    (RChannelLabel::Tfc, &["TFC", "TPFC", "TOPFRONTCENTER"]),
    // Height tier: over the floor speaker of the same name at about 30° of
    // elevation (BS.2051 `U+030`/`U+000`/`U+110`), which is not the ceiling
    // corner the top tier means. `HL`/`HR`/`HEIGHTLEFT`/`HEIGHTRIGHT` stay
    // with the top-front pair above: they predate this tier and a saved
    // layout that uses them must keep meaning what it meant.
    (
        RChannelLabel::Lh,
        &[
            "LH",
            "LEFTHEIGHT",
            "FRONTHEIGHTLEFT",
            "FRONTLEFTHEIGHT",
            "FHL",
        ],
    ),
    (
        RChannelLabel::Rh,
        &[
            "RH",
            "RIGHTHEIGHT",
            "FRONTHEIGHTRIGHT",
            "FRONTRIGHTHEIGHT",
            "FHR",
        ],
    ),
    (
        RChannelLabel::Ch,
        &[
            "CH",
            "HC",
            "CENTERHEIGHT",
            "CENTREHEIGHT",
            "HEIGHTCENTER",
            "HEIGHTCENTRE",
            "FRONTHEIGHTCENTER",
            "FHC",
        ],
    ),
    (
        RChannelLabel::Lhs,
        &[
            "LHS",
            "HLS",
            "LEFTHEIGHTSURROUND",
            "LEFTSURROUNDHEIGHT",
            "HEIGHTLEFTSURROUND",
            "SURROUNDHEIGHTLEFT",
            "SHL",
        ],
    ),
    (
        RChannelLabel::Rhs,
        &[
            "RHS",
            "HRS",
            "RIGHTHEIGHTSURROUND",
            "RIGHTSURROUNDHEIGHT",
            "HEIGHTRIGHTSURROUND",
            "SURROUNDHEIGHTRIGHT",
            "SHR",
        ],
    ),
];

/// Canonical short name for a label — the form used in bundled layout YAMLs,
/// Studio source lists and diagnostics. Lower-tier names follow the compact
/// convention (`L`, `Ls`, `Lb`); the top tier keeps its uppercase trigrams
/// (`TFL`, `TBR`) to match the bundled layouts.
pub fn canonical_name(label: RChannelLabel) -> &'static str {
    use RChannelLabel::*;
    match label {
        L => "L",
        R => "R",
        C => "C",
        LFE => "LFE",
        LFE2 => "LFE2",
        Ls => "Ls",
        Rs => "Rs",
        Lb => "Lb",
        Rb => "Rb",
        Cb => "Cb",
        Lsc => "Lsc",
        Rsc => "Rsc",
        Lw => "Lw",
        Rw => "Rw",
        Lsd => "Lsd",
        Rsd => "Rsd",
        Tfl => "TFL",
        Tfr => "TFR",
        Tsl => "TSL",
        Tsr => "TSR",
        Tbl => "TBL",
        Tbr => "TBR",
        Tc => "TC",
        Tfc => "TFC",
        Lh => "Lh",
        Rh => "Rh",
        Ch => "Ch",
        Lhs => "Lhs",
        Rhs => "Rhs",
        Object => "Object",
        Unknown => "Unknown",
    }
}

/// Accepted spellings for a label in normalised form (uppercase, no
/// whitespace/`_`/`-`) — the same list [`label_for_name`] matches against,
/// including the short canonical forms. Non-fixed labels ([`RChannelLabel::Object],
/// [`RChannelLabel::Unknown`]) have no aliases and get an empty slice.
pub fn aliases_for(label: RChannelLabel) -> &'static [&'static str] {
    ALIASES
        .iter()
        .find(|(l, _)| *l == label)
        .map_or(&[], |(_, aliases)| *aliases)
}

/// Resolve a speaker/channel name to its label. Case-insensitive and
/// separator-tolerant; returns [`RChannelLabel::Unknown`] for names that
/// don't resolve (the caller then falls back to its own policy, e.g. a
/// custom order or a plain count).
pub fn label_for_name(name: &str) -> RChannelLabel {
    let key = normalise(name);
    for (label, aliases) in ALIASES {
        if aliases.contains(&key.as_str()) {
            return *label;
        }
    }
    RChannelLabel::Unknown
}

/// Whether `name` is one of the spellings of `label` — the same answer as
/// `label_for_name(name) == label` (the alias table is unambiguous), without
/// allocating the normalised name, so a matcher can run it per entry.
pub fn name_matches(name: &str, label: RChannelLabel) -> bool {
    aliases_for(label)
        .iter()
        .any(|alias| normalised_chars(name).eq(alias.chars()))
}

fn normalise(name: &str) -> String {
    normalised_chars(name).collect()
}

fn normalised_chars(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .flat_map(|c| c.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use RChannelLabel::*;

    #[test]
    fn canonical_names_resolve_back_to_their_label() {
        for (label, _) in ALIASES {
            assert_eq!(
                label_for_name(canonical_name(*label)),
                *label,
                "canonical name of {label:?} must round-trip"
            );
        }
    }

    #[test]
    fn aliases_are_unambiguous() {
        let mut seen = std::collections::HashMap::new();
        for (label, aliases) in ALIASES {
            for alias in *aliases {
                if let Some(prev) = seen.insert(*alias, *label) {
                    panic!("alias {alias:?} claimed by both {prev:?} and {label:?}");
                }
            }
        }
    }

    #[test]
    fn name_matches_agrees_with_label_for_name() {
        let names = [
            "Top Front Left",
            "top_front_left",
            "HL",
            "hr",
            "Lfe-1",
            "SideLeft",
            "BL",
            "Rear Right",
            "Object",
            "",
            "spk-12",
        ];
        let spellings = ALIASES
            .iter()
            .flat_map(|(_, aliases)| aliases.iter().copied());
        for name in names.into_iter().chain(spellings) {
            for (label, _) in ALIASES {
                assert_eq!(
                    name_matches(name, *label),
                    label_for_name(name) == *label,
                    "{name:?} vs {label:?}"
                );
            }
            assert!(!name_matches(name, Unknown) && !name_matches(name, Object));
        }
    }

    #[test]
    fn matching_tolerates_case_and_separators() {
        assert_eq!(label_for_name("Top Front Left"), Tfl);
        assert_eq!(label_for_name("TOP_FRONT_LEFT"), Tfl);
        assert_eq!(label_for_name("tfl"), Tfl);
        assert_eq!(label_for_name("height-right"), Tfr);
        // Height-tier spellings accepted by the bed-planner matcher and by
        // hand-edited beds must resolve through this table too.
        assert_eq!(label_for_name("UpperFrontLeft"), Tfl);
        assert_eq!(label_for_name("UpperSideLeft"), Tsl);
        assert_eq!(label_for_name("upper side right"), Tsr);
        assert_eq!(label_for_name("upper back right"), Tbr);
        assert_eq!(label_for_name("LTR"), Tbl);
        assert_eq!(label_for_name("RTR"), Tbr);
        assert_eq!(label_for_name("TopMiddleCenter"), Tc);
        // The height tier under its Auro-3D, DTS-HD and long spellings; the
        // legacy `HL`/`HR` spellings keep resolving to the top-front pair.
        assert_eq!(label_for_name("Lh"), Lh);
        assert_eq!(label_for_name("left height"), Lh);
        assert_eq!(label_for_name("HC"), Ch);
        assert_eq!(label_for_name("HLs"), Lhs);
        assert_eq!(label_for_name("HRs"), Rhs);
        assert_eq!(label_for_name("right_surround_height"), Rhs);
        assert_eq!(label_for_name("HL"), Tfl);
        assert_eq!(label_for_name("HR"), Tfr);
        assert_eq!(label_for_name("nonsense"), Unknown);
    }

    #[test]
    fn aliases_for_matches_the_alias_table() {
        for (label, aliases) in ALIASES {
            assert_eq!(aliases_for(*label), *aliases);
            for alias in *aliases {
                assert_eq!(
                    label_for_name(alias),
                    *label,
                    "alias {alias:?} must resolve back to its own label"
                );
            }
        }
        assert!(aliases_for(Object).is_empty());
        assert!(aliases_for(Unknown).is_empty());
    }
}
