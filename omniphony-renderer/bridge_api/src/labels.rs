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
            "UFR",
            "RTF",
            "RIGHTTOPFRONT",
            "HEIGHTRIGHT",
            "HR",
        ],
    ),
    (RChannelLabel::Tsl, &["TSL", "TPSL", "TOPSIDELEFT", "USL"]),
    (RChannelLabel::Tsr, &["TSR", "TPSR", "TOPSIDERIGHT", "USR"]),
    (
        RChannelLabel::Tbl,
        &["TBL", "TPBL", "TOPBACKLEFT", "UBL", "TRL"],
    ),
    (
        RChannelLabel::Tbr,
        &["TBR", "TPBR", "TOPBACKRIGHT", "UBR", "TRR"],
    ),
    (
        RChannelLabel::Tc,
        &["TC", "TPC", "TOPCENTER", "TOPMIDDLECENTER"],
    ),
    (RChannelLabel::Tfc, &["TFC", "TOPFRONTCENTER"]),
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
        Object => "Object",
        Unknown => "Unknown",
    }
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

fn normalise(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
        .flat_map(|c| c.to_uppercase())
        .collect()
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
    fn matching_tolerates_case_and_separators() {
        assert_eq!(label_for_name("Top Front Left"), Tfl);
        assert_eq!(label_for_name("TOP_FRONT_LEFT"), Tfl);
        assert_eq!(label_for_name("tfl"), Tfl);
        assert_eq!(label_for_name("height-right"), Tfr);
        assert_eq!(label_for_name("nonsense"), Unknown);
    }
}
