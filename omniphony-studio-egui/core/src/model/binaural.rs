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

/// The path the render takes to the output: what the scene depicts, and
/// what the panels stand down for. Resolved from the binaural document and
/// from whether the topology renders on a BRIR set's own loudspeakers
/// (`binaural.brir.layout` present, `AppState::brir_speakers`): a `brir`
/// source whose set is not resident, or does not fit the speaker stage,
/// renders the virtual room on the HRTF stage, which is what the view must
/// show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderPath {
    /// The speaker stage feeds the loudspeakers.
    Speakers,
    /// Each source through its own HRTF pair, read as a direction straight
    /// off its normalized position: no room warp, the unit cube.
    Direct,
    /// The speaker stage on the editable layout, warped by the live room,
    /// then one HRTF pair per virtual speaker.
    VirtualRoom,
    /// The speaker stage on the BRIR set's loudspeakers, each convolved with
    /// its measured pair.
    MeasuredRoom,
}

impl RenderPath {
    pub fn of(binaural: Option<&Value>, brir_layout: bool) -> Self {
        match OutputMode::from_state(binaural) {
            OutputMode::Speaker => RenderPath::Speakers,
            OutputMode::BinauralDirect => RenderPath::Direct,
            OutputMode::BinauralCascaded if brir_layout => RenderPath::MeasuredRoom,
            OutputMode::BinauralCascaded => RenderPath::VirtualRoom,
        }
    }

    /// Headphones, by any path.
    pub fn is_binaural(self) -> bool {
        self != RenderPath::Speakers
    }

    /// Whether a normalized position is warped by the live room before it
    /// is rendered: on every path through the speaker stage, and not on the
    /// direct one, which reads the direction straight off the position
    /// (the renderer's `RoomRatios::for_output`). The scene places things
    /// the same way.
    pub fn warps_with_room(self) -> bool {
        self != RenderPath::Direct
    }

    /// Whether the speakers drawn are what renders: the loudspeakers, or
    /// the virtual ones of a room. On the direct path nothing feeds them.
    pub fn speakers_render(self) -> bool {
        self != RenderPath::Direct
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

    /// The path follows the output and the mode that renders, and a
    /// measured room only once the topology is on the set's loudspeakers:
    /// a `brir` source without a resident set is the virtual room.
    #[test]
    fn the_render_path_is_what_the_engine_does() {
        assert_eq!(RenderPath::of(None, false), RenderPath::Speakers);
        let speakers = doc("speaker", "cascaded", Some("cascaded"), BRIR);
        assert_eq!(RenderPath::of(Some(&speakers), true), RenderPath::Speakers);
        let direct = doc("binaural", "direct", Some("direct"), "saf");
        assert_eq!(RenderPath::of(Some(&direct), false), RenderPath::Direct);
        let virtual_room = doc("binaural", "cascaded", Some("cascaded"), "sofa");
        assert_eq!(
            RenderPath::of(Some(&virtual_room), false),
            RenderPath::VirtualRoom
        );
        let room_loading = doc("binaural", "direct", Some("cascaded"), BRIR);
        assert_eq!(
            RenderPath::of(Some(&room_loading), false),
            RenderPath::VirtualRoom
        );
        assert_eq!(
            RenderPath::of(Some(&room_loading), true),
            RenderPath::MeasuredRoom
        );
        assert!(!RenderPath::Speakers.is_binaural() && RenderPath::Direct.is_binaural());
        assert!(!RenderPath::Direct.warps_with_room() && RenderPath::VirtualRoom.warps_with_room());
        assert!(
            !RenderPath::Direct.speakers_render() && RenderPath::MeasuredRoom.speakers_render()
        );
    }

    /// The frame everything is drawn in and converted through: the live
    /// room through the speaker stage, the unit room (at the renderer's
    /// distance scale) on the direct path.
    #[test]
    fn the_display_room_is_the_unit_room_on_the_direct_path_only() {
        use crate::model::app_state::{AppState, RoomRatio};
        let mut app = AppState::new(Vec::new());
        app.room_ratio = RoomRatio {
            length: 2.0,
            scale_m: 1.7,
            ..RoomRatio::default()
        };
        app.binaural = Some(json!({
            "outputMode": "binaural", "mode": "direct", "modeEffective": "direct",
            "hrirSource": "saf", "unitScaleM": 3.0,
        }));
        let direct = app.display_room();
        assert_eq!((direct.length, direct.rear, direct.lower), (1.0, 1.0, 1.0));
        assert_eq!(direct.center_blend, 0.0);
        assert_eq!(direct.scale_m, 3.0);
        app.binaural = Some(json!({
            "outputMode": "binaural", "mode": "cascaded", "modeEffective": "cascaded",
            "hrirSource": "saf", "unitScaleM": 3.0,
        }));
        let cascaded = app.display_room();
        assert_eq!((cascaded.length, cascaded.scale_m), (2.0, 1.7));
        app.binaural = Some(json!({ "outputMode": "speaker", "mode": "direct" }));
        assert_eq!(app.display_room().length, 2.0);
    }

    /// The listening room is read while the reflections are on: the room in
    /// use when the renderer publishes it, the configured one from an older
    /// renderer, nothing with the reflections off or a malformed room.
    #[test]
    fn the_reflection_room_is_the_one_in_use_while_reflections_are_on() {
        use crate::model::app_state::AppState;
        let mut app = AppState::new(Vec::new());
        app.binaural = Some(json!({ "reflections": {
            "enabled": true, "roomM": [4.0, 5.0, 2.7], "roomEffectiveM": [6.7, 6.7, 6.7],
        }}));
        assert_eq!(app.binaural_reflection_room_m(), Some([6.7, 6.7, 6.7]));
        app.binaural = Some(json!({ "reflections": { "enabled": true, "roomM": [4.0, 5.0, 2.7] }}));
        assert_eq!(app.binaural_reflection_room_m(), Some([4.0, 5.0, 2.7]));
        app.binaural =
            Some(json!({ "reflections": { "enabled": false, "roomM": [4.0, 5.0, 2.7] }}));
        assert_eq!(app.binaural_reflection_room_m(), None);
        app.binaural = Some(json!({ "reflections": { "enabled": true, "roomM": [4.0, 0.0, 2.7] }}));
        assert_eq!(app.binaural_reflection_room_m(), None);
        app.binaural = None;
        assert_eq!(app.binaural_reflection_room_m(), None);
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
