//! The binaural document's choices as the Studio offers them: the output
//! mode read from `outputMode` and the mode that renders, and which HRTF
//! sources that mode can use. The document is the renderer's `binaural`
//! block, passed through as JSON (`AppState::binaural`).
//!
//! The output mode drives the source, not the reverse: a measured room
//! (`brir`) renders through the virtual room only, so it is offered there,
//! and choosing the direct path over one brings the source back to KEMAR
//! (`host::commands::binaural::select_output_mode`).

use serde_json::Value;

/// The HRTF sources, in the select's order, with their label keys.
pub const HRIR_SOURCES: &[(&str, &str)] = &[
    ("saf", "binaural.hrtfSource.kemar"),
    ("synthetic", "binaural.hrtfSource.synthetic"),
    ("pinna", "binaural.hrtfSource.pinna"),
    ("prtf", "binaural.hrtfSource.prtf"),
    ("sofa", "binaural.hrtfSource.sofa"),
    ("brir", "binaural.hrtfSource.brir"),
];

/// The source id of a measured room.
pub const BRIR: &str = "brir";

/// The output-mode select: the pair `(outputMode, mode)` of the binaural
/// state flattened into one choice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputMode {
    Speaker,
    BinauralDirect,
    BinauralCascaded,
}

impl OutputMode {
    /// The select's entries, in order.
    pub const ALL: [OutputMode; 3] = [
        OutputMode::Speaker,
        OutputMode::BinauralDirect,
        OutputMode::BinauralCascaded,
    ];

    pub fn i18n_key(self) -> &'static str {
        match self {
            OutputMode::Speaker => "outputMode.speakers",
            OutputMode::BinauralDirect => "outputMode.headphones",
            OutputMode::BinauralCascaded => "outputMode.headphonesVirtual",
        }
    }

    /// The flattened value of the binaural document. The mode shown is the
    /// one that renders (`modeEffective`): a measured room forces the
    /// virtual-speaker path whatever `mode` says.
    pub fn from_state(binaural: Option<&Value>) -> Self {
        if text(binaural, "outputMode") != Some("binaural") {
            return OutputMode::Speaker;
        }
        match mode_effective(binaural) {
            Some("cascaded") => OutputMode::BinauralCascaded,
            _ => OutputMode::BinauralDirect,
        }
    }

    /// Whether the virtual-speaker path is the headphone path in force —
    /// rendering now, or the one headphones would take while the output is
    /// the speakers. What decides which sources are offered.
    pub fn virtual_room(binaural: Option<&Value>) -> bool {
        mode_effective(binaural) == Some("cascaded")
    }
}

fn text<'a>(binaural: Option<&'a Value>, key: &str) -> Option<&'a str> {
    binaural
        .and_then(|b| b.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// `modeEffective`, from renderers that publish it, else the chosen `mode`.
fn mode_effective(binaural: Option<&Value>) -> Option<&str> {
    text(binaural, "modeEffective").or_else(|| text(binaural, "mode"))
}

/// The HRTF source the renderer was asked for (`hrirSource`).
pub fn hrir_source(binaural: Option<&Value>) -> Option<&str> {
    text(binaural, "hrirSource")
}

/// Whether the source `id` is offered under the document's mode: a
/// measured room renders through the virtual room only, so it is listed
/// there, and wherever it is already the source so the select never hides
/// what renders — never under the direct path, where choosing it would
/// only switch the mode under the listener.
pub fn hrir_source_offered(binaural: Option<&Value>, id: &str) -> bool {
    id != BRIR || OutputMode::virtual_room(binaural) || hrir_source(binaural) == Some(BRIR)
}

/// The file a bare `sofa` / `brir` choice reopens, by name: the renderer
/// keeps the last file each source named (`hrtfSofaPathLast`,
/// `brirSofaPathLast`). `None` for the other sources, and while none was
/// ever named.
pub fn last_hrir_file(binaural: Option<&Value>, id: &str) -> Option<String> {
    let key = match id {
        "sofa" => "hrtfSofaPathLast",
        BRIR => "brirSofaPathLast",
        _ => return None,
    };
    let path = text(binaural, key)?;
    Some(path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc(output: &str, mode: &str, effective: Option<&str>, source: &str) -> Value {
        let mut d = json!({ "outputMode": output, "mode": mode, "hrirSource": source });
        if let Some(effective) = effective {
            d["modeEffective"] = json!(effective);
        }
        d
    }

    /// The select shows the path that renders: a measured room is the
    /// virtual room whatever `mode` says, and a renderer without
    /// `modeEffective` is read from `mode`.
    #[test]
    fn the_output_mode_is_the_path_that_renders() {
        assert_eq!(OutputMode::from_state(None), OutputMode::Speaker);
        let speakers = doc("speaker", "cascaded", Some("cascaded"), "saf");
        assert_eq!(OutputMode::from_state(Some(&speakers)), OutputMode::Speaker);
        let direct = doc("binaural", "direct", Some("direct"), "saf");
        assert_eq!(
            OutputMode::from_state(Some(&direct)),
            OutputMode::BinauralDirect
        );
        let room = doc("binaural", "direct", Some("cascaded"), "brir");
        assert_eq!(
            OutputMode::from_state(Some(&room)),
            OutputMode::BinauralCascaded
        );
        let older = doc("binaural", "cascaded", None, "saf");
        assert_eq!(
            OutputMode::from_state(Some(&older)),
            OutputMode::BinauralCascaded
        );
    }

    /// A measured room is offered under the virtual room, on either output,
    /// and wherever it already is the source; the other sources always.
    #[test]
    fn a_measured_room_is_offered_under_the_virtual_room_only() {
        let direct = doc("binaural", "direct", Some("direct"), "saf");
        assert!(!hrir_source_offered(Some(&direct), BRIR));
        assert!(hrir_source_offered(Some(&direct), "sofa"));
        assert!(hrir_source_offered(Some(&direct), "saf"));
        let virtual_room = doc("binaural", "cascaded", Some("cascaded"), "sofa");
        assert!(hrir_source_offered(Some(&virtual_room), BRIR));
        // On the speakers, the headphone mode that would apply decides.
        let speakers_cascaded = doc("speaker", "cascaded", Some("cascaded"), "saf");
        assert!(hrir_source_offered(Some(&speakers_cascaded), BRIR));
        let speakers_direct = doc("speaker", "direct", Some("direct"), "saf");
        assert!(!hrir_source_offered(Some(&speakers_direct), BRIR));
        // Already the source: shown, so the select never hides what renders.
        let room_under_direct = doc("binaural", "direct", Some("cascaded"), BRIR);
        assert!(hrir_source_offered(Some(&room_under_direct), BRIR));
        assert!(!hrir_source_offered(None, BRIR));
    }

    /// The file sources name the file a bare choice reopens; nothing named,
    /// or another source, says nothing.
    #[test]
    fn the_file_sources_name_their_last_file() {
        let d = json!({
            "hrtfSofaPathLast": "/hrtf/pp12.sofa",
            "brirSofaPathLast": "C:\\\\rooms\\\\g.sofa",
        });
        assert_eq!(
            last_hrir_file(Some(&d), "sofa").as_deref(),
            Some("pp12.sofa")
        );
        assert_eq!(last_hrir_file(Some(&d), BRIR).as_deref(), Some("g.sofa"));
        assert_eq!(last_hrir_file(Some(&d), "saf"), None);
        let none = json!({ "hrtfSofaPathLast": "", "brirSofaPathLast": "" });
        assert_eq!(last_hrir_file(Some(&none), "sofa"), None);
        assert_eq!(last_hrir_file(None, BRIR), None);
    }
}
