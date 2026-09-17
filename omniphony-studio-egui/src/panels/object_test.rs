//! The object-injection switch and editor (`#objectTestFeatureRow`,
//! `#objectTestEditSection`, `controls/object-test.js`, host `commands/gain.rs`).
//!
//! Two separate things: the *feature* puts a source in the room — visible,
//! selectable, metered, placeable — and the *transport* decides whether it makes
//! any noise. The source is registered like any other object, so the sphere, the
//! label, the trail, the list row and the meter are the ones every other object
//! gets; nothing here draws them.

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::StudioSpike;
use crate::host::commands::gain;
use crate::host::services::object_test::{ObjectTestMarker, set_object_test_marker};
use crate::i18n::t;
use crate::ui::group::Group;
use crate::ui::{help, theme, widgets};
use crate::view::gizmos;

use super::object_test_sheet as sheet;

pub use crate::host::services::object_test::SOURCE_ID as OBJECT_TEST_SOURCE_ID;

/// How the orbit is drawn: enough samples that the clamp's flats are visible.
const ORBIT_SAMPLES: usize = 96;

const SIGNALS: &[(&str, &str)] = &[
    ("pink", "objectTest.signalPink"),
    ("bursts", "objectTest.signalBursts"),
    ("low", "objectTest.signalLow"),
    ("high", "objectTest.signalHigh"),
    ("band", "objectTest.signalBand"),
    ("tone", "objectTest.signalTone"),
    ("clicks", "objectTest.signalClicks"),
    ("clip", "objectTest.signalClip"),
];

const AXES: &[(&str, &str)] = &[
    ("z", "objectTest.axisZ"),
    ("x", "objectTest.axisX"),
    ("y", "objectTest.axisY"),
    ("free", "objectTest.axisFree"),
];

const ISOLATIONS: &[(&str, &str)] = &[
    ("test_only", "objectTest.isolationTestOnly"),
    ("with_programme", "objectTest.isolationWithProgramme"),
];

// ---------------------------------------------------------------------------
// Radius: marked at the room's own distances
// ---------------------------------------------------------------------------
//
// The marks are not decoration. In a room spanning [-1, 1] on every axis, the
// distance from the centre to a vertical edge is √2 and to a corner is √3 — the
// two radii at which a horizontal orbit passes exactly through the room's
// geometry rather than somewhere near it. Their doubles are the same reach from
// the far wall, which is where the centre goes when the orbit should sweep the
// whole room rather than ring the middle of it.

const RADIUS_MAX: f64 = 4.0;
const RADIUS_SNAP: f64 = 0.04;
const RADIUS_MARKS: &[(f64, &str)] = &[
    (std::f64::consts::SQRT_2, "√2"),
    (1.732_050_807_568_877_2, "√3"),
    (2.0 * std::f64::consts::SQRT_2, "2√2"),
    (3.464_101_615_137_754_4, "2√3"),
];

/// Pull a near-miss onto a landmark, so the marks can actually be hit.
fn snap_radius(v: f64) -> f64 {
    let r = v.clamp(0.0, RADIUS_MAX);
    for (value, _) in RADIUS_MARKS {
        if (r - value).abs() <= RADIUS_SNAP {
            return *value;
        }
    }
    r
}

/// Name the landmark when sitting on one — otherwise the snap is invisible.
fn format_radius(r: f64) -> String {
    if r <= 0.0 {
        return t("objectTest.radiusOff").to_owned();
    }
    match RADIUS_MARKS.iter().find(|(v, _)| (r - v).abs() < 1e-6) {
        Some((_, label)) => format!("{r:.2} {label}"),
        None => format!("{r:.2}"),
    }
}

// ---------------------------------------------------------------------------
// Turn time: a logarithmic control
// ---------------------------------------------------------------------------
//
// A period is chosen by ratio, not by difference: the step from 1 s to 2 s is
// the same change as the one from 10 s to 20 s, and on a linear scale the first
// costs a thirtieth of the travel while the second costs a third.

const PERIOD_MIN: f64 = 0.5;
const PERIOD_MAX: f64 = 30.0;
const PERIOD_STEPS: f64 = 1000.0;

fn slider_to_period(pos: f64) -> f64 {
    let t = (pos / PERIOD_STEPS).clamp(0.0, 1.0);
    let raw = PERIOD_MIN * (PERIOD_MAX / PERIOD_MIN).powf(t);
    // Coarser as the number grows: a hundredth of a second matters at half a
    // second and is noise at twenty.
    if raw < 1.0 {
        (raw * 20.0).round() / 20.0
    } else if raw < 10.0 {
        (raw * 10.0).round() / 10.0
    } else {
        (raw * 2.0).round() / 2.0
    }
}

fn period_to_slider(period: f64) -> f64 {
    let p = period.clamp(PERIOD_MIN, PERIOD_MAX);
    (PERIOD_STEPS * ((p / PERIOD_MIN).ln() / (PERIOD_MAX / PERIOD_MIN).ln())).round()
}

fn format_period(period: f64) -> String {
    if period < 1.0 {
        format!("{period:.2} s")
    } else {
        format!("{period:.1} s")
    }
}

// ---------------------------------------------------------------------------
// Persisted settings
// ---------------------------------------------------------------------------

/// The `objectTest.*` localStorage keys of the web Studio, as one block of the
/// native preferences file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ObjectTestPrefs {
    /// `objectTest.feature.v1`: whether the injected object exists at all.
    pub feature: bool,
    /// `objectTest.signal.v1`.
    pub signal: String,
    /// `objectTest.admView.v1`.
    pub adm_view: bool,
    /// `objectTest.snap.v1`.
    pub snap: bool,
    /// `objectTest.levelDb.v1`, peak dBFS.
    pub level_db: f64,
    /// `objectTest.isolation.v1`.
    pub isolation: String,
    /// `objectTest.position.v1`, ADM cartesian. Front-centre at ear level.
    pub position: [f64; 3],
    /// `objectTest.rotation.v2`.
    pub rotation: Rotation,
}

impl Default for ObjectTestPrefs {
    fn default() -> Self {
        Self {
            feature: false,
            signal: "pink".to_owned(),
            adm_view: false,
            snap: false,
            level_db: -8.0,
            isolation: "test_only".to_owned(),
            position: [0.0, 1.0, 0.0],
            rotation: Rotation::default(),
        }
    }
}

/// `radius: 0` means no orbit, matching the renderer, so there is no separate
/// flag to keep in step with it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Rotation {
    pub axis: String,
    pub radius: f64,
    pub period: f64,
    pub azimuth: f64,
    pub elevation: f64,
}

impl Default for Rotation {
    fn default() -> Self {
        Self {
            axis: "z".to_owned(),
            radius: 0.0,
            period: 4.0,
            azimuth: 0.0,
            elevation: 0.0,
        }
    }
}

impl StudioSpike {
    /// `#objectTestFeatureRow`, shown at the top of the objects section.
    pub(crate) fn object_test_feature_row(&mut self, ui: &mut Ui) {
        let mut on = self.prefs.object_test.feature;
        widgets::label_row_help(
            ui,
            t("objectTest.feature"),
            "help.objectTestFeature",
            |ui| {
                if widgets::switch(ui, &mut on).changed() {
                    self.set_object_test_feature(on);
                }
            },
        );
    }

    fn set_object_test_feature(&mut self, on: bool) {
        self.prefs.object_test.feature = on;
        self.mark_prefs_dirty();
        if on {
            // The renderer starts with no orbit, so a restored one has to be
            // stated rather than assumed.
            self.send_object_test_rotation();
            self.selection = crate::view::Selection {
                object: Some(OBJECT_TEST_SOURCE_ID.to_owned()),
                speaker: None,
            };
        } else {
            self.stop_object_test();
            if self.selection.object.as_deref() == Some(OBJECT_TEST_SOURCE_ID) {
                self.selection.object = None;
            }
        }
    }

    /// `#objectTestEditSection`: shown while the feature is on and the injected
    /// source is the selected one.
    pub(crate) fn object_test_editor(&mut self, ui: &mut Ui) {
        if !self.prefs.object_test.feature
            || self.selection.object.as_deref() != Some(OBJECT_TEST_SOURCE_ID)
        {
            return;
        }
        ui.add_space(theme::PANEL_GAP);
        ui.separator();
        help::overlay_title(
            ui,
            RichText::new(t("section.objectTest"))
                .size(theme::FONT_SIZE_SECTION)
                .color(theme::TEXT_STRONG),
            || help::Overlay::titled(t("section.objectTest"), "help.objectTest"),
        );
        self.object_test_transport(ui);
        // The signal's help is about the whole transport, so it opens under
        // it (`data-help-anchor=".object-test-transport"`).
        help::card(ui, "help.objectTestSignal");
        self.object_test_clip_row(ui);
        let mut adm = self.prefs.object_test.adm_view;
        widgets::label_row_help(
            ui,
            t("objectTest.admView"),
            "help.objectTestAdmView",
            |ui| {
                if widgets::switch(ui, &mut adm).changed() {
                    self.prefs.object_test.adm_view = adm;
                    self.mark_prefs_dirty();
                }
            },
        );
        widgets::note(ui, t("objectTest.hint"));
        self.object_test_sheet(ui);
        let p = self.prefs.object_test.position;
        ui.label(
            RichText::new(format!("x {:.2}   y {:.2}   z {:.2}", p[0], p[1], p[2]))
                .size(theme::FONT_SIZE_SMALL)
                .monospace()
                .color(theme::TEXT_MUTED),
        );
        self.object_test_snap_row(ui);
        let mut level = self.prefs.object_test.level_db as f32;
        if widgets::value_slider_help(
            ui,
            t("objectTest.level"),
            "help.objectTestLevel",
            &mut level,
            -60.0..=0.0,
            1.0,
            |v| format!("{v:.0} dBFS"),
        ) {
            self.prefs.object_test.level_db = f64::from(level);
            self.mark_prefs_dirty();
            if self.object_test_playing {
                self.send_object_test();
            }
        }
        self.object_test_orbit_controls(ui);
        let isolation = self.prefs.object_test.isolation.clone();
        if let Some(chosen) = select_row(
            ui,
            t("objectTest.isolation"),
            "help.objectTestIsolation",
            "object-test-isolation",
            &isolation,
            ISOLATIONS,
        ) {
            self.prefs.object_test.isolation = chosen;
            self.mark_prefs_dirty();
            if self.object_test_playing {
                self.send_object_test();
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t("objectTest.centre")).clicked() {
                self.set_object_test_position([0.0; 3]);
            }
        });
    }

    /// The play/stop button and the stimulus picker.
    fn object_test_transport(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let playing = self.object_test_playing;
            let label = if playing { "❚❚" } else { "▶" };
            let button = egui::Button::new(RichText::new(label).size(14.0).color(if playing {
                theme::OK_BRIGHT
            } else {
                theme::TEXT
            }))
            .min_size(egui::vec2(34.0, 34.0))
            .corner_radius(17)
            .fill(if playing {
                theme::OK.gamma_multiply(0.18)
            } else {
                theme::FILL
            });
            let response = ui.add(button).on_hover_text(t(if playing {
                "objectTest.stop"
            } else {
                "objectTest.play"
            }));
            if response.clicked() {
                if playing {
                    self.stop_object_test();
                } else {
                    // State the orbit on every start, not only when the panel
                    // first appeared: a renderer restarted underneath Studio
                    // comes back with no orbit at all.
                    self.send_object_test_rotation();
                    self.object_test_playing = true;
                    self.send_object_test();
                }
            }
            ui.vertical(|ui| {
                help::label(
                    ui,
                    RichText::new(t("objectTest.signal"))
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED),
                    "help.objectTestSignal",
                );
                let signal = self.prefs.object_test.signal.clone();
                if let Some(chosen) = combo(ui, "object-test-signal", &signal, SIGNALS, 190.0) {
                    self.prefs.object_test.signal = chosen;
                    self.mark_prefs_dirty();
                    if self.object_test_playing {
                        // Changing the stimulus restarts it.
                        self.send_object_test();
                    }
                }
            });
        });
    }

    /// `#objectTestClipRow`: only while the signal is the audio file.
    fn object_test_clip_row(&mut self, ui: &mut Ui) {
        if self.prefs.object_test.signal != "clip" {
            return;
        }
        let clip = self.host.read().object_test_clip.clone();
        ui.horizontal(|ui| {
            // Only the renderer knows: the file may be unreadable, at the wrong
            // rate, or longer than the cap.
            match clip.as_ref() {
                Some(doc) if doc.get("error").is_some() => {
                    let message = doc
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or_default()
                        .to_owned();
                    ui.label(RichText::new(message).color(theme::ERROR));
                }
                Some(doc) => {
                    let name = doc.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let seconds = doc.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let mut text = format!("{name} · {seconds:.1} s");
                    if doc
                        .get("truncated")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        text.push_str(" · ");
                        text.push_str(t("objectTest.clipTruncated"));
                    }
                    widgets::note(ui, &text);
                }
                None => widgets::note(ui, t("objectTest.clipNone")),
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("objectTest.clipChoose")).clicked()
                    && let Some(path) = rfd::FileDialog::new()
                        .add_filter("WAV", &["wav"])
                        .pick_file()
                {
                    gain::control_object_test_clip(&self.host, path.to_string_lossy().into_owned());
                }
            });
        });
    }

    fn object_test_snap_row(&mut self, ui: &mut Ui) {
        let has_grid = self.ensure_vbap_grid();
        let mut snap = self.prefs.object_test.snap;
        widgets::label_row_help(ui, t("objectTest.snap"), "help.objectTestSnap", |ui| {
            ui.add_enabled_ui(has_grid, |ui| {
                if widgets::switch(ui, &mut snap).changed() {
                    self.prefs.object_test.snap = snap;
                    self.mark_prefs_dirty();
                    if snap {
                        // Turning it on re-snaps where the source already is.
                        self.set_object_test_position(self.prefs.object_test.position);
                    }
                }
            });
        });
        if snap && !has_grid {
            widgets::note(ui, t("objectTest.snapNoGrid"));
        }
    }

    /// The orbit: the axis in the bar; radius, turn time and the free
    /// axis's angles in the inset.
    fn object_test_orbit_controls(&mut self, ui: &mut Ui) {
        let axis = self.prefs.object_test.rotation.axis.clone();
        let mut chosen = None;
        Group::new(t("objectTest.rotationAxis"))
            .help("help.objectTestRotation")
            .actions(|ui| {
                widgets::bounded_combo(ui, 150.0, |ui, w| {
                    chosen = combo(ui, "object-test-axis", &axis, AXES, w);
                });
            })
            .show(ui, |ui| self.object_test_orbit_rows(ui, &axis));
        if let Some(chosen) = chosen {
            self.prefs.object_test.rotation.axis = chosen;
            self.apply_object_test_rotation();
        }
    }

    fn object_test_orbit_rows(&mut self, ui: &mut Ui, axis: &str) {
        let mut radius = self.prefs.object_test.rotation.radius;
        if self.radius_row(ui, &mut radius) {
            self.prefs.object_test.rotation.radius = snap_radius(radius);
            self.apply_object_test_rotation();
        }
        let mut pos = period_to_slider(self.prefs.object_test.rotation.period);
        widgets::label_row_help(ui, t("objectTest.period"), "help.objectTestPeriod", |ui| {
            ui.add_sized(
                egui::vec2(56.0, ui.spacing().interact_size.y),
                egui::Label::new(
                    RichText::new(format_period(self.prefs.object_test.rotation.period))
                        .monospace()
                        .color(theme::TEXT_STRONG),
                ),
            );
            if ui
                .add(
                    widgets::stepped(egui::Slider::new(&mut pos, 0.0..=PERIOD_STEPS), 1.0)
                        .show_value(false),
                )
                .changed()
            {
                self.prefs.object_test.rotation.period = slider_to_period(pos);
                self.apply_object_test_rotation();
            }
        });
        if axis != "free" {
            return;
        }
        let mut azimuth = self.prefs.object_test.rotation.azimuth as f32;
        if widgets::value_slider_help(
            ui,
            t("objectTest.axisAzimuth"),
            "help.objectTest.axisAzimuth",
            &mut azimuth,
            -180.0..=180.0,
            1.0,
            |v| format!("{v:.0}°"),
        ) {
            self.prefs.object_test.rotation.azimuth = f64::from(azimuth);
            self.apply_object_test_rotation();
        }
        let mut elevation = self.prefs.object_test.rotation.elevation as f32;
        if widgets::value_slider_help(
            ui,
            t("objectTest.axisElevation"),
            "help.objectTest.axisElevation",
            &mut elevation,
            -90.0..=90.0,
            1.0,
            |v| format!("{v:.0}°"),
        ) {
            self.prefs.object_test.rotation.elevation = f64::from(elevation);
            self.apply_object_test_rotation();
        }
    }

    /// The radius slider, with the room's own distances marked on the track.
    fn radius_row(&mut self, ui: &mut Ui, radius: &mut f64) -> bool {
        let mut changed = false;
        widgets::label_row_help(ui, t("objectTest.radius"), "help.objectTestRadius", |ui| {
            ui.add_sized(
                egui::vec2(64.0, ui.spacing().interact_size.y),
                egui::Label::new(
                    RichText::new(format_radius(*radius))
                        .monospace()
                        .color(theme::TEXT_STRONG),
                ),
            );
            let response = ui.add(
                widgets::stepped(egui::Slider::new(radius, 0.0..=RADIUS_MAX), 0.01)
                    .show_value(false),
            );
            changed = response.changed();
            // The marks are guides; `snap_radius` is what lands on them.
            let track = response.rect;
            for (value, _) in RADIUS_MARKS {
                let x = track.left() + track.width() * (value / RADIUS_MAX) as f32;
                ui.painter().vline(
                    x,
                    (track.bottom() - 4.0)..=track.bottom(),
                    egui::Stroke::new(1.0, theme::TEXT_FAINT),
                );
            }
        });
        changed
    }

    // -----------------------------------------------------------------------
    // The CAD sheet
    // -----------------------------------------------------------------------

    fn object_test_sheet(&mut self, ui: &mut Ui) {
        let room = self.host.read().app.room_ratio.clone();
        let space = sheet::Space {
            adm_view: self.prefs.object_test.adm_view,
            room: &room,
        };
        let layout = sheet::layout(space.extent());
        let avail = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), sheet::MAX_HEIGHT),
        );
        let (draw, transform) = sheet::fit(&layout, avail);
        let (_, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), draw.height()),
            egui::Sense::click_and_drag(),
        );

        // Track Alt for the whole gesture: snapping is a help until the moment
        // the position *between* two nodes is the one wanted, and a switch to
        // flip and flip back for one drag is worse than the drag itself.
        let alt = ui.input(|i| i.modifiers.alt);
        if response.drag_started() {
            self.object_test_drag = response
                .interact_pointer_pos()
                .map(|p| transform.to_sheet(p))
                .and_then(|(x, y)| sheet::target_at(&layout, x, y));
            if let Some(sheet::Target::Slider(index)) = self.object_test_drag {
                self.object_test_focus = Some(index);
            }
        }
        if response.drag_stopped() {
            self.object_test_drag = None;
        }
        if let Some(target) = self.object_test_drag
            && let Some(p) = response.interact_pointer_pos()
        {
            let (x, y) = transform.to_sheet(p);
            let next = sheet::position_at(
                &layout,
                target,
                x,
                y,
                self.prefs.object_test.position,
                space,
            );
            self.set_object_test_position_bypass(next, alt);
        }
        // The gutter sliders name what they drive; the faces say it themselves.
        if self.object_test_drag.is_none()
            && let Some(p) = response.hover_pos()
        {
            let (x, y) = transform.to_sheet(p);
            if let Some(sheet::Target::Slider(index)) = sheet::target_at(&layout, x, y) {
                response
                    .clone()
                    .on_hover_text(t(sheet::SLIDERS[index].label_key));
            }
        }
        self.object_test_keys(ui);

        self.rebuild_object_test_orbit();
        let snap = self.prefs.object_test.snap && self.ensure_vbap_grid();
        let grid = snap
            .then(|| self.vbap_grid_cache.as_ref().map(|(_, a)| a))
            .flatten();
        let overlay = sheet::Overlay {
            position: self.prefs.object_test.position,
            orbit: &self.object_test_orbit,
            grid,
            focused: self.object_test_focus,
        };
        sheet::paint(
            &ui.painter().with_clip_rect(draw.expand(4.0)),
            &layout,
            &transform,
            space,
            &overlay,
        );
    }

    /// Arrow keys nudge the focused gutter slider, page keys step, home and end
    /// jump to the walls.
    fn object_test_keys(&mut self, ui: &mut Ui) {
        let Some(index) = self.object_test_focus else {
            return;
        };
        let (mut delta, mut absolute) = (0.0, None);
        ui.input(|i| {
            for (key, d) in [
                (egui::Key::ArrowRight, 0.02),
                (egui::Key::ArrowUp, 0.02),
                (egui::Key::ArrowLeft, -0.02),
                (egui::Key::ArrowDown, -0.02),
                (egui::Key::PageUp, 0.1),
                (egui::Key::PageDown, -0.1),
            ] {
                if i.key_pressed(key) {
                    delta = d;
                }
            }
            if i.key_pressed(egui::Key::Home) {
                absolute = Some(-1.0);
            }
            if i.key_pressed(egui::Key::End) {
                absolute = Some(1.0);
            }
        });
        if delta == 0.0 && absolute.is_none() {
            return;
        }
        let next = sheet::SLIDERS[index].nudge(self.prefs.object_test.position, delta, absolute);
        self.set_object_test_position(next);
    }

    // -----------------------------------------------------------------------
    // State
    // -----------------------------------------------------------------------

    pub(crate) fn set_object_test_position(&mut self, next: [f64; 3]) {
        self.set_object_test_position_bypass(next, false);
    }

    /// Placement snaps; the orbit does not. Rounding a continuous sweep onto a
    /// grid would turn smooth motion into a series of jumps, which is the one
    /// thing this feature exists to avoid.
    fn set_object_test_position_bypass(&mut self, next: [f64; 3], bypass_snap: bool) {
        let clamped = [
            next[0].clamp(-1.0, 1.0),
            next[1].clamp(-1.0, 1.0),
            next[2].clamp(-1.0, 1.0),
        ];
        self.ensure_vbap_grid();
        let snapped = match (
            self.prefs.object_test.snap && !bypass_snap,
            self.vbap_grid_cache.as_ref(),
        ) {
            (true, Some((_, axes))) => [
                gizmos::snap_to_nodes(clamped[0], &axes[0]),
                gizmos::snap_to_nodes(clamped[1], &axes[1]),
                gizmos::snap_to_nodes(clamped[2], &axes[2]),
            ],
            _ => clamped,
        };
        if snapped == self.prefs.object_test.position {
            return;
        }
        self.prefs.object_test.position = snapped;
        self.mark_prefs_dirty();
        if self.object_test_playing {
            // Straight out: the renderer ramps to it, so a drag is heard as
            // movement rather than as a series of clicks.
            self.send_object_test();
        }
    }

    fn apply_object_test_rotation(&mut self) {
        self.mark_prefs_dirty();
        self.send_object_test_rotation();
    }

    /// `send()`: muted counts as off, since the renderer has no separate mute
    /// for a source it does not otherwise know about.
    pub(crate) fn send_object_test(&mut self) {
        let o = &self.prefs.object_test;
        let level = 10f64.powf(o.level_db / 20.0) as f32;
        let on = self.object_test_playing && !self.object_test_muted;
        gain::control_object_test(
            &self.host,
            on,
            o.position[0] as f32,
            o.position[1] as f32,
            o.position[2] as f32,
            level,
            0.0,
            o.isolation.clone(),
            o.signal.clone(),
        );
    }

    fn send_object_test_rotation(&mut self) {
        let r = &self.prefs.object_test.rotation;
        gain::control_object_test_rotation(
            &self.host,
            r.axis.clone(),
            r.radius as f32,
            r.period as f32,
            r.azimuth as f32,
            r.elevation as f32,
        );
    }

    pub(crate) fn stop_object_test(&mut self) {
        if !self.object_test_playing {
            return;
        }
        self.object_test_playing = false;
        self.object_test_muted = false;
        self.send_object_test();
    }

    /// Mute from the object list's M button: stop sending while remembering
    /// that it was playing, so unmuting resumes.
    pub(crate) fn set_object_test_muted(&mut self, muted: bool) {
        if muted == self.object_test_muted {
            return;
        }
        self.object_test_muted = muted;
        self.send_object_test();
    }

    /// Say whether the injected object belongs in the room and where the user
    /// put it. Publishing it — and taking it away again — is the core service's
    /// job, so it keeps happening while nothing is being drawn.
    pub(crate) fn declare_object_test_marker(&mut self) {
        set_object_test_marker(
            &self.host,
            ObjectTestMarker {
                shown: self.prefs.object_test.feature,
                playing: self.object_test_playing,
                placed: self.prefs.object_test.position,
            },
        );
    }

    /// The orbit, mirrored from the renderer's `position_at` for display only.
    ///
    /// The renderer owns the phase, so Studio can never know where the source is
    /// at a given instant — but it can know the *path*, and drawing it is the
    /// only way the radius and axis controls mean anything before play. It
    /// mirrors the room clamp too: a drawn circle where the heard one is
    /// flattened against a wall would be a picture of something not happening.
    fn rebuild_object_test_orbit(&mut self) {
        let r = &self.prefs.object_test.rotation;
        if r.radius <= 0.0 {
            self.object_test_orbit.clear();
            return;
        }
        let (u, v) = orbit_plane(r);
        let base = self.prefs.object_test.position;
        let radius = r.radius;
        self.object_test_orbit.clear();
        for i in 0..=ORBIT_SAMPLES {
            let theta = (i as f64 / ORBIT_SAMPLES as f64) * std::f64::consts::TAU;
            let (s, c) = theta.sin_cos();
            self.object_test_orbit.push([
                (base[0] + radius * (u[0] * c + v[0] * s)).clamp(-1.0, 1.0),
                (base[1] + radius * (u[1] * c + v[1] * s)).clamp(-1.0, 1.0),
                (base[2] + radius * (u[2] * c + v[2] * s)).clamp(-1.0, 1.0),
            ]);
        }
    }

    /// Node positions of the renderer's cartesian gain table, per axis.
    ///
    /// Rebuilt only when the published interval counts change: they are stable
    /// for a whole session, and rebuilding three node lists per frame to draw
    /// the same ticks would be pure churn. Returns whether there is a grid at
    /// all; the nodes themselves are read from the cache. The speaker gizmo's
    /// cartesian drag snaps to the same nodes.
    pub(crate) fn ensure_vbap_grid(&mut self) -> bool {
        let key = {
            let live = self.host.read();
            let c = &live.app.vbap_cartesian;
            [
                c.x_size.unwrap_or(0),
                c.y_size.unwrap_or(0),
                c.z_size.unwrap_or(0),
                c.z_neg_size.unwrap_or(0),
            ]
        };
        // The published sizes are INTERVAL counts, not node counts: the
        // renderer adds one before handing them to the axis builder.
        if key[0] < 1 || key[1] < 1 || key[2] < 1 {
            self.vbap_grid_cache = None;
            return false;
        }
        if self.vbap_grid_cache.as_ref().map(|(k, _)| *k) != Some(key) {
            use omniphony_geometry::f64 as g;
            let axes = [
                g::evenly_spaced_axis(key[0] as usize + 1, -1.0, 1.0),
                g::evenly_spaced_axis(key[1] as usize + 1, -1.0, 1.0),
                g::cartesian_z_axis(key[2] as usize + 1, key[3] as usize),
            ];
            self.vbap_grid_cache = Some((key, axes));
        }
        true
    }

    /// True while the editor is on screen, which is also the only time the
    /// renderer's idle feed needs to be held open for it.
    pub(crate) fn object_test_editor_open(&self) -> bool {
        self.prefs.object_test.feature
            && self.selection.object.as_deref() == Some(OBJECT_TEST_SOURCE_ID)
    }
}

/// The plane the source circles in: two unit vectors spanning it
/// (`RotationAxis::frame`).
fn orbit_plane(r: &Rotation) -> ([f64; 3], [f64; 3]) {
    match r.axis.as_str() {
        "x" => ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        "y" => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        "free" => {
            let az = r.azimuth.to_radians();
            let el = r.elevation.to_radians();
            let axis = [el.cos() * az.sin(), el.cos() * az.cos(), el.sin()];
            let seed = if axis[2].abs() < 0.9 {
                [0.0, 0.0, 1.0]
            } else {
                [1.0, 0.0, 0.0]
            };
            let u = normalize3(cross3(seed, axis));
            (u, normalize3(cross3(axis, u)))
        }
        _ => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    }
}

fn cross3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(v: [f64; 3]) -> [f64; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n < 1e-6 {
        [1.0, 0.0, 0.0]
    } else {
        [v[0] / n, v[1] / n, v[2] / n]
    }
}

/// Label on the left, a select on the right.
fn select_row(
    ui: &mut Ui,
    label: &str,
    help_key: &str,
    id: &str,
    current: &str,
    options: &[(&str, &str)],
) -> Option<String> {
    widgets::label_row_help(ui, label, help_key, |ui| {
        widgets::bounded_combo(ui, 170.0, |ui, w| combo(ui, id, current, options, w))
    })
}

fn combo(
    ui: &mut Ui,
    id: &str,
    current: &str,
    options: &[(&str, &str)],
    width: f32,
) -> Option<String> {
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(t(options
            .iter()
            .find(|(v, _)| *v == current)
            .map(|(_, key)| *key)
            .unwrap_or(options[0].1)))
        .width(width)
        .truncate()
        .show_ui(ui, |ui| {
            for (value, key) in options {
                if ui.selectable_label(*value == current, t(key)).clicked() && *value != current {
                    chosen = Some((*value).to_owned());
                }
            }
        });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_turn_time_slider_round_trips_through_its_own_rounding() {
        for pos in [0.0, 1.0, 250.0, 508.0, 700.0, 999.0, PERIOD_STEPS] {
            let period = slider_to_period(pos);
            assert!((PERIOD_MIN..=PERIOD_MAX).contains(&period), "{period}");
            // The position is quantised to the rounding the readout shows, so
            // a round trip must land within one step rather than exactly.
            assert!(
                (period_to_slider(period) - pos).abs() <= 2.0,
                "{pos} -> {period}"
            );
        }
        // The stored default sits where the web's slider default put it.
        assert_eq!(period_to_slider(4.0), 508.0);
    }

    #[test]
    fn a_near_miss_lands_on_a_landmark_and_a_deliberate_miss_does_not() {
        let root2 = std::f64::consts::SQRT_2;
        assert_eq!(snap_radius(root2 - 0.03), root2);
        assert_eq!(snap_radius(root2 + 0.035), root2);
        assert_ne!(snap_radius(root2 + 0.09), root2);
        assert_eq!(snap_radius(-1.0), 0.0);
        assert_eq!(snap_radius(9.0), RADIUS_MAX);
        assert_eq!(format_radius(root2), "1.41 √2");
        assert_eq!(format_radius(0.0), "off");
    }

    #[test]
    fn the_orbit_is_a_circle_of_the_asked_radius_until_the_room_clamps_it() {
        let rotation = Rotation {
            axis: "z".to_owned(),
            radius: 0.5,
            ..Rotation::default()
        };
        let (u, v) = orbit_plane(&rotation);
        // The horizontal plane: the height coordinate never moves.
        assert_eq!(u[2], 0.0);
        assert_eq!(v[2], 0.0);
        // A free axis straight up spans the same plane as `z`.
        let free = Rotation {
            axis: "free".to_owned(),
            elevation: 90.0,
            ..rotation.clone()
        };
        let (fu, fv) = orbit_plane(&free);
        assert!(fu[2].abs() < 1e-9 && fv[2].abs() < 1e-9);
    }
}
