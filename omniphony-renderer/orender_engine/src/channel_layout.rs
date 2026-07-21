//! Output channel-layout export.
//!
//! Maps each output speaker (by its layout name) to an ABI-stable
//! [`RChannelLabel`], so a host like mpv can turn the renderer's output into a
//! channel map (`mp_chmap`). Names come from the user-editable layout YAML, so
//! the matcher is alias-tolerant and case-insensitive; anything unrecognised
//! maps to [`RChannelLabel::Unknown`] (the host then falls back to a custom
//! order or a plain count).

use bridge_api::RChannelLabel;

/// Resolve one speaker name to its channel label. Thin wrapper over the
/// shared alias table (`bridge_api::labels`) — the single source of truth
/// for name↔label matching, per `docs/channel-object-contract.md` ("Naming").
pub fn label_for_speaker_name(name: &str) -> RChannelLabel {
    bridge_api::labels::label_for_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use RChannelLabel::*;

    #[test]
    fn maps_the_7_1_4_layout_names() {
        // Order matches Omniphony/layouts/7.1.4.yaml.
        let names = [
            "FL", "FR", "C", "LFE", "BL", "BR", "SL", "SR", "TFL", "TFR", "TBL", "TBR",
        ];
        let labels: Vec<_> = names.iter().map(|n| label_for_speaker_name(n)).collect();
        assert_eq!(
            labels,
            vec![L, R, C, LFE, Lb, Rb, Ls, Rs, Tfl, Tfr, Tbl, Tbr]
        );
    }

    #[test]
    fn maps_the_9_1_6_layout_names() {
        let names = [
            "FL", "FR", "C", "LFE", "FWL", "FWR", "SL", "SR", "BL", "BR", "TFL", "TFR", "TSL",
            "TSR", "TBL", "TBR",
        ];
        let labels: Vec<_> = names.iter().map(|n| label_for_speaker_name(n)).collect();
        assert_eq!(
            labels,
            vec![
                L, R, C, LFE, Lw, Rw, Ls, Rs, Lb, Rb, Tfl, Tfr, Tsl, Tsr, Tbl, Tbr
            ]
        );
    }

    #[test]
    fn is_case_and_separator_insensitive() {
        assert_eq!(label_for_speaker_name("tfl"), Tfl);
        assert_eq!(label_for_speaker_name("Top Front Left"), Tfl);
        assert_eq!(label_for_speaker_name("top_back_right"), Tbr);
        assert_eq!(label_for_speaker_name("Front-Left"), L);
    }

    #[test]
    fn unknown_names_fall_back() {
        assert_eq!(label_for_speaker_name("WeirdSpeaker"), Unknown);
        assert_eq!(label_for_speaker_name(""), Unknown);
    }
}
