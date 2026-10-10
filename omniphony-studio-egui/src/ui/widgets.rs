//! The Studio's recurring controls. Each one mirrors a CSS class of
//! `styles/app.css`: the switch of `.inline-toggle input[type=checkbox]`, the
//! `.toggle-btn` pair, `.gain-box`, `.meter`, `.info-row`, the status dot and
//! the banners. They are plain functions over `Ui`, so a panel reads like the
//! markup it replaces.

#![allow(dead_code)] // the toolkit is complete before every panel using it
use egui::{Color32, Response, Sense, Ui, Widget, vec2};

pub use super::help::Help;
use super::help::Trigger;
use super::theme;

/// `.inline-toggle input[type="checkbox"]`: 34×18 track, 12 px thumb, green
/// when on.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(34.0, 18.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label)
    });
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let enabled = ui.is_enabled();
        let track = if *on {
            Color32::from_rgba_unmultiplied(82, 226, 162, 115)
        } else {
            theme::FILL_ACTIVE
        };
        let track = if enabled {
            track
        } else {
            track.gamma_multiply(0.5)
        };
        ui.painter().rect(
            rect,
            9.0,
            track,
            if response.has_focus() {
                ui.visuals().selection.stroke
            } else {
                egui::Stroke::new(1.0, theme::CONTROL_BORDER)
            },
            egui::StrokeKind::Inside,
        );
        let x = egui::lerp((rect.left() + 9.0)..=(rect.right() - 9.0), how_on);
        ui.painter().circle_filled(
            egui::pos2(x, rect.center().y),
            6.0,
            if enabled {
                theme::TEXT
            } else {
                theme::TEXT_FAINT
            },
        );
    }
    response
}

/// A label on the left and controls on the right, where the controls win.
///
/// They are placed first, from the right edge, and the label takes what they
/// leave — truncated if it must be, and egui shows it whole on hover. Laid out
/// the other way round, label first and then a right-to-left block, a label
/// longer than its room simply sits *under* the controls: a right-to-left
/// layout draws over what is already there instead of pushing it, and the
/// panel's measured width stays innocent while the row is unreadable.
pub fn label_row<R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    labelled(ui, label.into(), None, None, add_right)
}

/// `label_row` whose label opens `help` in a card under the row (see
/// [`super::help`]). `help` is a `help.*` key, or a [`Help`] built from text.
pub fn label_row_help<'h, R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    help: impl Into<Help<'h>>,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    let help = help.into();
    let out = labelled(ui, label.into(), None, Some(Trigger::Card(help)), add_right);
    super::help::card(ui, help);
    out
}

/// `label_row_info` for a title key and a body key that do not share a
/// prefix.
pub fn label_row_info_keys<R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    title_key: &str,
    body_key: &str,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    labelled(
        ui,
        label.into(),
        None,
        Some(Trigger::InfoKeys(title_key, body_key)),
        add_right,
    )
}

/// `label_row` whose label heads a whole block and opens the centred overlay
/// for its `<prefix>.infoTitle` / `.infoBody` pair.
pub fn label_row_info<R>(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    info_prefix: &str,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    labelled(
        ui,
        label.into(),
        None,
        Some(Trigger::Info(info_prefix)),
        add_right,
    )
}

fn labelled<R>(
    ui: &mut Ui,
    label: egui::WidgetText,
    leading: Option<&mut dyn FnMut(&mut Ui)>,
    help: Option<Trigger<'_>>,
    add_right: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.horizontal(|ui| {
        // Drawn in the row itself, before the label, rather than beside the
        // row in a horizontal of the caller's own: see `switch_row_help_leading`.
        if let Some(draw) = leading {
            draw(ui);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let out = add_right(ui);
            // What the controls left, from the row's own left edge. The label
            // takes exactly that and no more: it is laid out at that width,
            // cut to it, and painted inside it. Left to size itself, a label
            // that could not quite fit made its row a few points wider than
            // the panel — and egui grows a layout's bounds to whatever its
            // children took, so every row after it started those few points
            // further left and lost its first letter to the panel's edge.
            let space = ui.available_rect_before_wrap();
            let text_width = space.width().max(0.0);
            let galley = label.clone().into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                text_width,
                egui::TextStyle::Body,
            );
            // One row high: the space's own height is whatever is left below.
            let row_height = ui.spacing().interact_size.y.max(galley.size().y);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(space.width(), row_height), Sense::hover());
            let text_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
                galley.size(),
            );
            if text_width > 0.0 {
                let truncated = galley.elided;
                let colour = ui.visuals().text_color();
                ui.painter()
                    .with_clip_rect(rect.intersect(ui.clip_rect()))
                    .galley(text_rect.min, galley, colour);
                if truncated {
                    response.on_hover_text(label.text());
                }
                // The label itself is the trigger, over the text only: the
                // blank between it and the controls stays inert.
                if let Some(help) = help {
                    super::help::trigger_any(ui, text_rect.intersect(rect), help);
                }
            }
            out
        })
        .inner
    })
    .inner
}

/// One entry of an editable list (the decoder bridges): its name on the first
/// line with the entry's buttons at the right, and a detail line under it in
/// `colour`. Both lines are one line high whatever they hold — the name and
/// the detail truncate — so an entry never changes height under the buttons
/// of the entries below it. The name shows `full` on hover, the detail
/// itself when cut.
///
/// `mark` highlights the entry (the bridge decoding the stream): an accent
/// outline painted around it, outside its rect, and a tag of that text at the
/// right end of its detail line, in the detail's own font. Neither takes
/// space, so marking an entry moves nothing: only its detail truncates
/// sooner.
pub fn list_entry(
    ui: &mut Ui,
    name: &str,
    full: &str,
    detail: &str,
    colour: Color32,
    mark: Option<&str>,
    add_right: impl FnOnce(&mut Ui),
) {
    // Filled once the entry is laid out, under it.
    let outline = ui.painter().add(egui::Shape::Noop);
    let top = ui.cursor().top();
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            add_right(ui);
            // As `labelled` does: the name takes exactly what the buttons
            // left, laid out, cut and painted at that width.
            let space = ui.available_rect_before_wrap();
            let galley = egui::WidgetText::from(name).into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                space.width().max(0.0),
                egui::TextStyle::Body,
            );
            let row_height = ui.spacing().interact_size.y.max(galley.size().y);
            let (rect, response) =
                ui.allocate_exact_size(vec2(space.width(), row_height), Sense::hover());
            let at = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
            let text = ui.visuals().text_color();
            ui.painter()
                .with_clip_rect(rect.intersect(ui.clip_rect()))
                .galley(at, galley, text);
            response.on_hover_text(full);
        });
    });
    let small = |text: &str, colour: Color32| {
        egui::WidgetText::from(
            egui::RichText::new(text)
                .size(theme::FONT_SIZE_SMALL)
                .color(colour),
        )
    };
    let width = ui.available_width().max(0.0);
    let tag = mark.map(|mark| {
        small(mark, theme::ACCENT).into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Body,
        )
    });
    let tag_width = tag
        .as_ref()
        .map_or(0.0, |tag| tag.size().x + 2.0 * TAG_PADDING_X);
    let galley = small(detail, colour).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        (width - tag_width - if tag.is_some() { TAG_GAP } else { 0.0 }).max(0.0),
        egui::TextStyle::Body,
    );
    // The line is as high as the detail's text, tag or not: the tag is
    // that font too.
    let height = galley.size().y;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let truncated = galley.elided;
    painter.galley(rect.left_top(), galley, colour);
    if truncated {
        response.on_hover_text(detail);
    }
    let Some(tag) = tag else {
        return;
    };
    let pill = egui::Rect::from_min_max(
        egui::pos2(rect.right() - tag_width, rect.top()),
        rect.right_bottom(),
    );
    painter.rect(
        pill,
        pill.height() / 2.0,
        theme::ACCENT.gamma_multiply(0.15),
        egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
    let at = pill.center() - tag.size() / 2.0;
    painter.galley(at, tag, theme::ACCENT);
    // Around the whole entry, in the margin the rows already leave.
    let entry = egui::Rect::from_min_max(egui::pos2(rect.left(), top), rect.max)
        .expand2(vec2(MARK_OUTSET_X, MARK_OUTSET_Y));
    ui.painter().set(
        outline,
        egui::epaint::RectShape::new(
            entry,
            theme::CONTROL_RADIUS,
            theme::ACCENT.gamma_multiply(0.06),
            egui::Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.5)),
            egui::StrokeKind::Inside,
        ),
    );
}

/// A [`list_entry`] tag's text inset, and its gap to the detail it ends.
const TAG_PADDING_X: f32 = 5.0;
const TAG_GAP: f32 = 4.0;
/// How far a marked [`list_entry`]'s outline reaches past the entry: inside
/// the group's padding and the spacing between entries, never into the next.
const MARK_OUTSET_X: f32 = 4.0;
const MARK_OUTSET_Y: f32 = 1.0;

/// The width a dropdown in a `label_row` takes: its usual width, but never more
/// than 60 % of the row, so its label keeps room to be read when the panel is
/// narrow. Called inside the row's controls closure, where the row is still
/// wholly available.
pub fn combo_width(ui: &Ui, preferred: f32) -> f32 {
    (ui.available_width() * 0.6).min(preferred).max(40.0)
}

/// A dropdown held to `combo_width(preferred)`. egui takes `ComboBox::width`
/// as a minimum and, truncating, lays the selected text out against all the
/// width it can see — so a truncated combo grows to fill whatever is left of
/// its row, and a row of label and combo ran past the panel. The combo is
/// drawn inside a scope bounded to its width, so that is all it can see.
/// `add` gets the bounded ui and the width to pass to `ComboBox::width`.
pub fn bounded_combo<R>(ui: &mut Ui, preferred: f32, add: impl FnOnce(&mut Ui, f32) -> R) -> R {
    let width = combo_width(ui, preferred);
    ui.scope(|ui| {
        ui.set_max_width(width);
        add(ui, width)
    })
    .inner
}

/// A label and a few buttons, `(text, enabled)`, returning the index of the
/// one clicked. The buttons sit at the right of the label while they fit, as
/// in the web; on a panel too narrow for them they share a line of their own
/// under it, each an equal part of the width and truncated if it must be,
/// rather than pushing the row past the panel's edge.
pub fn label_buttons_help<'h>(
    ui: &mut Ui,
    label: &str,
    help: impl Into<Help<'h>>,
    buttons: &[(&str, bool)],
) -> Option<usize> {
    let help = help.into();
    let spacing = ui.spacing().item_spacing.x;
    let text_width = |ui: &Ui, text: &str, style: egui::TextStyle| {
        egui::WidgetText::from(text)
            .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, style)
            .size()
            .x
    };
    let needed = text_width(ui, label, egui::TextStyle::Body)
        + buttons
            .iter()
            .map(|(text, _)| {
                text_width(ui, text, egui::TextStyle::Button)
                    + 2.0 * ui.spacing().button_padding.x
                    + spacing
            })
            .sum::<f32>();
    let mut clicked = None;
    if needed <= ui.available_width() {
        labelled(ui, label.into(), None, Some(Trigger::Card(help)), |ui| {
            // Right to left: the last button is placed first.
            for (index, (text, enabled)) in buttons.iter().enumerate().rev() {
                if ui.add_enabled(*enabled, egui::Button::new(*text)).clicked() {
                    clicked = Some(index);
                }
            }
        });
        super::help::card(ui, help);
        return clicked;
    }
    labelled(ui, label.into(), None, Some(Trigger::Card(help)), |_| {});
    super::help::card(ui, help);
    ui.columns(buttons.len().max(1), |columns| {
        for (index, (column, (text, enabled))) in columns.iter_mut().zip(buttons).enumerate() {
            let size = vec2(column.available_width(), column.spacing().interact_size.y);
            let button = egui::Button::new(*text).truncate();
            if column
                .add_enabled_ui(*enabled, |ui| ui.add_sized(size, button))
                .inner
                .clicked()
            {
                clicked = Some(index);
            }
        }
    });
    clicked
}

/// The width each of `fields` equal inputs can take on a line that also
/// holds `texts` (small labels): what is left once the labels and the gaps
/// are paid for, shared out, and never wider than `max` nor narrower than
/// `min` — past `min` the line does overflow, but only on a panel narrower
/// than any the layout allows.
pub fn fitted_field_width(ui: &Ui, texts: &[&str], fields: usize, min: f32, max: f32) -> f32 {
    let spacing = ui.spacing().item_spacing.x;
    let labels: f32 = texts
        .iter()
        .map(|text| {
            egui::WidgetText::from(egui::RichText::new(*text).size(theme::FONT_SIZE_SMALL))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                )
                .size()
                .x
        })
        .sum();
    let gaps = spacing * (texts.len() + fields).saturating_sub(1) as f32;
    let fields = fields.max(1) as f32;
    ((ui.available_width() - labels - gaps) / fields).clamp(min, max)
}

/// Label left, switch right (`.inline-toggle`). Returns true when toggled.
pub fn switch_row(ui: &mut Ui, label: &str, on: &mut bool) -> bool {
    label_row(ui, label, |ui| switch(ui, on, label).changed())
}

/// `switch_row` whose label opens `help`.
pub fn switch_row_help<'h>(
    ui: &mut Ui,
    label: &str,
    help: impl Into<Help<'h>>,
    on: &mut bool,
) -> bool {
    label_row_help(ui, label, help, |ui| switch(ui, on, label).changed())
}

/// `switch_row_help` with a glyph of its own before the label — the web draws
/// the clip dot inside the auto-gain label's span, not beside its row.
///
/// It is a variant rather than a `ui.horizontal` at the call site because the
/// help card cannot live in a horizontal: the row takes the whole width it is
/// offered, so a card drawn after it there is allocated nothing, and an opened
/// card becomes a tall sliver of nothing instead of a paragraph.
pub fn switch_row_help_leading<'h>(
    ui: &mut Ui,
    mut leading: impl FnMut(&mut Ui),
    label: &str,
    help: impl Into<Help<'h>>,
    on: &mut bool,
) -> bool {
    let help = help.into();
    let out = labelled(
        ui,
        label.into(),
        Some(&mut leading),
        Some(Trigger::Card(help)),
        |ui| switch(ui, on, label).changed(),
    );
    super::help::card(ui, help);
    out
}

/// A `.toggle-btn` group: one active value out of a list of (value, label).
/// Returns the newly picked value.
pub fn toggle_buttons<'a, T: PartialEq + Clone>(
    ui: &mut Ui,
    current: &T,
    options: &[(T, &'a str)],
) -> Option<T> {
    let mut picked = None;
    // Inside a right-to-left row — the controls of a `label_row` — a
    // horizontal group runs right to left too, and the first option would
    // land on the right: "Back | Side" for the web's "Side | Back". Placing
    // them last-first keeps the order they are given in.
    let reversed = ui.layout().prefer_right_to_left();
    ui.horizontal(|ui| {
        let count = options.len();
        for i in 0..count {
            let (value, label) = &options[if reversed { count - 1 - i } else { i }];
            let active = value == current;
            let mut text = egui::RichText::new(*label);
            if active {
                text = text.color(theme::TEXT_STRONG);
            }
            if ui.selectable_label(active, text).clicked() && !active {
                picked = Some(value.clone());
            }
        }
    });
    picked
}

/// A slider on a grid: the user's edits snap to `step`, the value already
/// there is left alone. egui's default snaps the existing value too, while
/// drawing, and reports that as a change — and a value the renderer echoed
/// as an `f32` rarely sits on the grid (`59 %` comes back as `58.999996`), so
/// a form that committed on change re-planned the layout on every frame, for
/// as long as its section stayed open.
pub fn stepped<'a>(slider: egui::Slider<'a>, step: f64) -> egui::Slider<'a> {
    slider.step_by(step).clamping(egui::SliderClamping::Edits)
}

/// `.gain-box`: a label, a slider, and the value with its unit. `format`
/// renders the readout so each caller keeps the web's exact formatting.
///
/// Narrowing the panel takes room from the track first, down to `MIN_TRACK`,
/// and only then from the label: a slider that is merely shorter still works,
/// a label cut to three letters says nothing.
pub fn value_slider(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    slider_row(ui, label, None, value, range, step, format)
}

/// `value_slider` whose label opens `help`.
pub fn value_slider_help<'h>(
    ui: &mut Ui,
    label: &str,
    help: impl Into<Help<'h>>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    let help = help.into();
    let changed = slider_row(
        ui,
        label,
        Some(Trigger::Card(help)),
        value,
        range,
        step,
        format,
    );
    super::help::card(ui, help);
    changed
}

fn slider_row(
    ui: &mut Ui,
    label: &str,
    help: Option<Trigger<'_>>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
    format: impl Fn(f32) -> String,
) -> bool {
    let label_width = egui::WidgetText::from(label)
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Extend),
            f32::INFINITY,
            egui::TextStyle::Body,
        )
        .size()
        .x;
    let full_track = ui.spacing().slider_width;
    labelled(ui, label.into(), None, help, |ui| {
        ui.add_sized(
            vec2(64.0, ui.spacing().interact_size.y),
            egui::Label::new(
                egui::RichText::new(format(*value))
                    .monospace()
                    .color(theme::TEXT_STRONG),
            ),
        );
        let room = ui.available_width() - label_width - ui.spacing().item_spacing.x;
        ui.spacing_mut().slider_width = room.clamp(MIN_TRACK, full_track.max(MIN_TRACK));
        let response = stepped(egui::Slider::new(value, range), step)
            .show_value(false)
            .ui(ui);
        name_control(&response, label);
        response.changed()
    })
}

/// Name a control whose visible label is drawn separately. Preserve the
/// built-in slider's numeric range/actions and emit no duplicate change event.
fn name_control(response: &Response, label: &str) {
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_label(label);
    });
}

/// The connection dot of the OSC panel: a filled circle plus a caption.
pub fn status_dot(ui: &mut Ui, colour: Color32, text: impl Into<egui::WidgetText>) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.0, colour);
        ui.add(egui::Label::new(text).truncate());
    });
}

/// What a [`progress_button`] shows behind its label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ButtonProgress {
    /// A plain button.
    Idle,
    /// Filled from the left up to this share, 0 to 1.
    Fill(f32),
    /// Working on it: a band sweeps across, and the button takes no clicks.
    Busy,
}

/// How long the busy band takes to cross the button.
const BUSY_SWEEP_SECS: f64 = 1.4;
/// The busy band's width, as a share of the button's.
const BUSY_BAND: f32 = 0.35;

/// A button with its progress drawn behind the label, inside its own rect.
///
/// `labels` lists every label the button can show: it takes the width of the
/// widest, so a label that changes with the progress never moves what sits
/// beside it. The fill goes under the button's own (translucent) frame, in a
/// slot reserved before it, so the label stays on top.
pub fn progress_button(
    ui: &mut Ui,
    text: &str,
    labels: &[&str],
    progress: ButtonProgress,
) -> Response {
    let widest = labels
        .iter()
        .copied()
        .chain(std::iter::once(text))
        .map(|label| {
            egui::WidgetText::from(label)
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    let width = widest + 2.0 * ui.spacing().button_padding.x;
    let slot = ui.painter().add(egui::Shape::Noop);
    let busy = progress == ButtonProgress::Busy;
    let response = ui.add_enabled(!busy, egui::Button::new(text).min_size(vec2(width, 0.0)));
    let rect = response.rect;
    let span = match progress {
        ButtonProgress::Idle => None,
        ButtonProgress::Fill(share) => Some((
            rect.left(),
            egui::lerp(rect.x_range(), share.clamp(0.0, 1.0)),
        )),
        ButtonProgress::Busy => {
            let phase = (ui.input(|i| i.time) / BUSY_SWEEP_SECS).fract() as f32;
            let band = rect.width() * BUSY_BAND;
            let left = egui::lerp((rect.left() - band)..=rect.right(), phase);
            Some((left, left + band))
        }
    };
    if let Some((left, right)) = span
        && right > left
    {
        let clip = egui::Rect::from_x_y_ranges(left..=right, rect.y_range()).intersect(rect);
        ui.painter().with_clip_rect(clip).set(
            slot,
            egui::epaint::RectShape::filled(
                rect,
                ui.visuals().widgets.inactive.corner_radius,
                theme::ACCENT.gamma_multiply(0.3),
            ),
        );
    }
    response
}

/// Severity of a banner row (`#bridgeErrorBanner`, `#foreignRendererBanner`,
/// `#updateAvailableBanner`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    fn colour(self) -> Color32 {
        match self {
            Severity::Info => theme::ACCENT,
            Severity::Warning => theme::WARN,
            Severity::Error => theme::ERROR,
        }
    }
}

/// A tinted, outlined block of text.
pub fn banner(ui: &mut Ui, severity: Severity, title: &str, detail: Option<&str>) {
    banner_with(ui, severity, title, |ui| {
        if let Some(detail) = detail {
            ui.label(
                egui::RichText::new(detail)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(theme::TEXT_MUTED),
            );
        }
    });
}

/// A banner whose body the caller draws — for the ones that carry a link or a
/// control rather than a line of prose.
pub fn banner_with(ui: &mut Ui, severity: Severity, title: &str, body: impl FnOnce(&mut Ui)) {
    let colour = severity.colour();
    egui::Frame::new()
        .fill(colour.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, colour.gamma_multiply(0.5)))
        .corner_radius(theme::CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(colour));
                body(ui);
            });
        });
}

/// The frame a modal sits in (`.modal-card`).
///
/// Opaque, unlike the side overlays: a dialog painted at the panels' 78 % lets
/// the scene and the controls behind it show through its own text, and a box
/// that has to be read is the one place the translucency costs more than it
/// buys.
pub fn modal_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(theme::PAGE_BG)
        .stroke(egui::Stroke::new(1.0, theme::PANEL_BORDER))
        .corner_radius(theme::PANEL_RADIUS)
        .inner_margin(egui::Margin::symmetric(
            theme::PANEL_PADDING_X as i8,
            theme::PANEL_PADDING_Y as i8,
        ))
}

/// `.option-status-note`: 11 px muted text under a control.
pub fn note(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(theme::FONT_SIZE_SMALL)
            .color(theme::TEXT_MUTED),
    );
}

/// The shortest track a slider row keeps; its label truncates before the track
/// goes below it.
const MIN_TRACK: f32 = 48.0;

/// The web's Display-panel slider row (`.control-row`, `grid auto 1fr`): the
/// label with its current value on the left, the track taking what is left.
///
/// `Slider::text` cannot do this. It lays a track of the style's fixed width,
/// a value box and then the label on one line, and none of the three shrinks,
/// so every such row ran past a panel at its minimum width — "Object sphere
/// size" alone does. Here the track is the part that gives, as the web's `1fr`
/// column does, and past a short track the label truncates and shows itself
/// whole on hover.
pub fn slider_line(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
) -> Response {
    slider_line_with(ui, label, None, value, range, step)
}

/// `slider_line` whose label opens `help`.
pub fn slider_line_help<'h>(
    ui: &mut Ui,
    label: &str,
    help: impl Into<Help<'h>>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
) -> Response {
    let help = help.into();
    let response = slider_line_with(ui, label, Some(help), value, range, step);
    super::help::card(ui, help);
    response
}

fn slider_line_with(
    ui: &mut Ui,
    label: &str,
    help: Option<Help<'_>>,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    step: f64,
) -> Response {
    let decimals = decimals_for(step);
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let label_max = (ui.available_width() - MIN_TRACK - spacing).max(0.0);
        let text = format!("{label} {:.*}", decimals, *value);
        ui.scope(|ui| {
            ui.set_max_width(label_max);
            let shown = ui
                .add(egui::Label::new(egui::RichText::new(text).size(theme::FONT_SIZE)).truncate());
            if let Some(help) = help {
                super::help::trigger(ui, shown.rect, help);
            }
        });
        ui.spacing_mut().slider_width = ui.available_width().max(MIN_TRACK);
        let response = ui.add(stepped(egui::Slider::new(value, range), step).show_value(false));
        name_control(&response, label);
        response
    })
    .inner
}

/// As many decimals as the step can move the value by: a 0.002 step reads
/// `0.070`, a 0.5 step `7.0`, a whole step `12`.
fn decimals_for(step: f64) -> usize {
    if step <= 0.0 || !step.is_finite() {
        return 2;
    }
    (-step.log10()).ceil().max(0.0) as usize
}

#[cfg(test)]
mod choice_card_tests {
    use super::choice_cards;
    use crate::ui::icons::{HEADPHONES, SECTION_SPEAKERS};

    /// One click anywhere on a card — its icon, its label, its padding —
    /// picks it, the first time; a click on the card in force picks nothing.
    /// The cards share the width equally and are all the same height,
    /// whatever their labels take.
    #[test]
    fn a_click_anywhere_on_a_card_picks_it() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(440.0, 200.0));
        let ctx = egui::Context::default();
        let rects = std::cell::RefCell::new(Vec::new());
        let frame = |events: Vec<egui::Event>| {
            let mut picked = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let top = ui.cursor().top();
                    let width = ui.available_width();
                    picked = choice_cards(
                        ui,
                        &0,
                        &[
                            (0, &[&SECTION_SPEAKERS], "Speakers"),
                            (1, &[&HEADPHONES], "Headphones"),
                            (
                                2,
                                &[&HEADPHONES, &SECTION_SPEAKERS],
                                "Headphones (a virtual room, with a long name)",
                            ),
                        ],
                    );
                    let height = ui.cursor().top() - top - ui.spacing().item_spacing.y;
                    let share = (width - 2.0 * super::theme::GROUP_GAP) / 3.0;
                    *rects.borrow_mut() = (0..3)
                        .map(|i| {
                            egui::Rect::from_min_size(
                                egui::pos2(
                                    ui.max_rect().left()
                                        + (share.floor() + super::theme::GROUP_GAP) * i as f32,
                                    top,
                                ),
                                egui::vec2(share.floor(), height),
                            )
                        })
                        .collect();
                },
            );
            output.textures_delta.clear();
            picked
        };
        assert_eq!(frame(Vec::new()), None);
        let cards = rects.borrow().clone();
        let click = |pos: egui::Pos2| {
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            frame(vec![egui::Event::PointerMoved(pos)]);
            frame(vec![button(true)]);
            frame(vec![button(false)])
        };
        // The icon, the label and a corner of the padding.
        assert_eq!(
            click(cards[1].center_top() + egui::vec2(0.0, 20.0)),
            Some(1)
        );
        assert_eq!(
            click(cards[2].center_bottom() - egui::vec2(0.0, 16.0)),
            Some(2)
        );
        assert_eq!(click(cards[2].left_top() + egui::vec2(3.0, 3.0)), Some(2));
        // The one in force.
        assert_eq!(click(cards[0].center()), None);
    }
}

#[cfg(test)]
mod row_tests {
    use super::{Sense, switch_row_help_leading};

    /// A help row must stay a child of the vertical flow it is given: the row
    /// takes the whole width offered, so whatever follows it inside a
    /// horizontal gets nothing — and what follows it is its own help card.
    /// That is why the clip dot is the row's leading glyph and not a sibling
    /// in a `ui.horizontal` around it.
    #[test]
    fn a_row_with_a_leading_glyph_leaves_its_card_the_whole_width() {
        let ctx = egui::Context::default();
        let (mut before, mut after) = (0.0, 0.0);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.vertical(|ui| {
                before = ui.available_width();
                let mut on = false;
                switch_row_help_leading(
                    ui,
                    |ui| {
                        ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
                    },
                    "Auto-gain (anti-clip)",
                    "help.master.autoGain",
                    &mut on,
                );
                after = ui.available_width();
            });
        });
        // Nothing paints here; egui still wants its font atlas taken.
        output.textures_delta.clear();
        assert!(before > 0.0);
        assert_eq!(after, before, "the row ate the width its card needs");
    }
}

#[cfg(test)]
mod slider_line_tests {
    use super::decimals_for;

    /// The readout keeps the digits the step can actually change, and no more:
    /// these are the steps the Display sliders use.
    #[test]
    fn the_readout_follows_the_step() {
        assert_eq!(decimals_for(0.002), 3);
        assert_eq!(decimals_for(0.01), 2);
        assert_eq!(decimals_for(0.05), 2);
        assert_eq!(decimals_for(0.1), 1);
        assert_eq!(decimals_for(0.5), 1);
        assert_eq!(decimals_for(1.0), 0);
        assert_eq!(decimals_for(10.0), 0);
    }
}

/// A row of equal tabs (`#rendererTabsBar`, the speaker editor's Edit/Test):
/// one active out of `options`, each an equal share of the width, the
/// inactive ones quieter. Returns the newly picked value.
pub fn tab_bar<T: PartialEq + Clone>(ui: &mut Ui, current: &T, options: &[(T, &str)]) -> Option<T> {
    let mut picked = None;
    ui.columns(options.len().max(1), |columns| {
        for (column, (value, label)) in columns.iter_mut().zip(options) {
            let active = value == current;
            let size = vec2(column.available_width(), column.spacing().interact_size.y);
            let mut text = egui::RichText::new(*label);
            if !active {
                text = text.color(theme::TEXT_MUTED);
            }
            if column
                .add_sized(size, egui::Button::selectable(active, text))
                .clicked()
                && !active
            {
                picked = Some(value.clone());
            }
        }
    });
    picked
}

/// A row of cards, one chosen: the few-way choice that decides what the rest
/// of a section is about (the output mode), where a select would hide the
/// alternatives. Each card is an equal share of the width, its icons — one,
/// or several side by side for a choice that combines others — over its
/// label; the one in force carries the accent, the others stay quiet until
/// hovered. The whole card is the click target. Returns the newly picked
/// value.
///
/// Every card is as high as an icon and two lines of label, whatever its own
/// label takes, so a longer translation wraps without moving what follows.
pub fn choice_cards<T: PartialEq + Clone>(
    ui: &mut Ui,
    current: &T,
    options: &[(T, &[&super::icons::Icon], &str)],
) -> Option<T> {
    const ICON: f32 = 26.0;
    const ICON_SPACING: f32 = 8.0;
    const PADDING: f32 = 8.0;
    const ICON_GAP: f32 = 5.0;
    const LABEL_ROWS: usize = 2;
    let count = options.len().max(1) as f32;
    let gap = theme::GROUP_GAP;
    let width = ((ui.available_width() - gap * (count - 1.0)) / count)
        .floor()
        .max(0.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let line = ui.fonts_mut(|f| f.row_height(&font));
    let height = PADDING + ICON + ICON_GAP + line * LABEL_ROWS as f32 + PADDING;
    let enabled = ui.is_enabled();
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (value, icons, label) in options {
            let active = value == current;
            let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, active, *label)
            });
            if response.clicked() && !active {
                picked = Some(value.clone());
            }
            if ui.is_rect_visible(rect) {
                let hovered = response.hovered() && enabled;
                let (fill, stroke, colour) = if active {
                    (
                        theme::ACCENT.gamma_multiply(0.14),
                        egui::Stroke::new(1.5, theme::ACCENT),
                        theme::TEXT_STRONG,
                    )
                } else if hovered {
                    (
                        theme::FILL_HOVER,
                        egui::Stroke::new(1.0, theme::CONTROL_BORDER),
                        theme::TEXT,
                    )
                } else {
                    (
                        theme::FILL,
                        egui::Stroke::new(1.0, theme::CONTROL_BORDER),
                        theme::TEXT_MUTED,
                    )
                };
                let colour = if enabled {
                    colour
                } else {
                    colour.gamma_multiply(0.5)
                };
                let stroke = if response.has_focus() {
                    ui.visuals().selection.stroke
                } else {
                    stroke
                };
                let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                painter.rect(
                    rect,
                    theme::GROUP_RADIUS,
                    fill,
                    stroke,
                    egui::StrokeKind::Inside,
                );
                let icons_width =
                    ICON * icons.len() as f32 + ICON_SPACING * icons.len().saturating_sub(1) as f32;
                let icons_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.center().x, rect.top() + PADDING + ICON / 2.0),
                    vec2(icons_width, ICON),
                );
                for (i, icon) in icons.iter().enumerate() {
                    let left = icons_rect.left() + (ICON + ICON_SPACING) * i as f32;
                    super::icons::paint(
                        &painter,
                        egui::Rect::from_min_size(
                            egui::pos2(left, icons_rect.top()),
                            vec2(ICON, ICON),
                        ),
                        icon,
                        if active { theme::ACCENT } else { colour },
                    );
                }
                let mut job = egui::text::LayoutJob::simple(
                    (*label).to_owned(),
                    font.clone(),
                    colour,
                    (width - 2.0 * PADDING).max(0.0),
                );
                job.halign = egui::Align::Center;
                job.wrap.max_rows = LABEL_ROWS;
                let galley = painter.layout_job(job);
                // Centred in the two lines kept for it: a one-line label
                // sits level with the middle of a two-line one.
                let label_top = icons_rect.bottom() + ICON_GAP;
                let slack = (line * LABEL_ROWS as f32 - galley.size().y).max(0.0);
                painter.galley(
                    egui::pos2(rect.center().x, label_top + slack / 2.0),
                    galley,
                    colour,
                );
            }
            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    });
    picked
}

/// Tabs for a list the renderer supplies, of any length: each tab as wide as
/// its label, wrapping between tabs rather than inside a label. `marked`
/// tabs carry a trailing dot (the one playing); every tab keeps the dot's
/// room, invisible when unmarked, so a mark moving never shifts the row.
pub fn wrapping_tab_bar<T: PartialEq + Clone>(
    ui: &mut Ui,
    current: &T,
    options: &[(T, &str, bool)],
) -> Option<T> {
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        for (value, label, marked) in options {
            let active = value == current;
            let color = if active {
                ui.visuals().text_color()
            } else {
                theme::TEXT_MUTED
            };
            let font = egui::TextStyle::Button.resolve(ui.style());
            let mut job = egui::text::LayoutJob::default();
            job.append(label, 0.0, egui::TextFormat::simple(font.clone(), color));
            let dot = if *marked { color } else { Color32::TRANSPARENT };
            job.append(" \u{25CF}", 0.0, egui::TextFormat::simple(font, dot));
            job.wrap.max_width = f32::INFINITY;
            let response = ui.add(egui::Button::selectable(active, job));
            if response.clicked() && !active {
                picked = Some(value.clone());
            }
        }
    });
    picked
}

#[cfg(test)]
mod accessibility_tests {
    use super::*;
    use egui::accesskit::{Action, Role, Toggled};

    #[test]
    fn switches_and_shared_sliders_expose_labels_values_and_actions() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut on = true;
        let mut gain = 0.5;
        let mut radius = 0.2;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            switch_row(ui, "Mesures audio", &mut on);
            value_slider(ui, "Gain", &mut gain, 0.0..=1.0, 0.1, |v| v.to_string());
            slider_line(ui, "Rayon", &mut radius, 0.0..=1.0, 0.1);
            ui.add_enabled_ui(false, |ui| {
                switch(ui, &mut on, "Indisponible");
            });
        });
        let tree = output.platform_output.accesskit_update.as_ref().unwrap();
        let node = |label: &str| {
            &tree
                .nodes
                .iter()
                .find(|(_, n)| n.label() == Some(label))
                .unwrap()
                .1
        };
        assert_eq!(node("Mesures audio").role(), Role::CheckBox);
        assert_eq!(node("Mesures audio").toggled(), Some(Toggled::True));
        assert!(node("Mesures audio").supports_action(Action::Click));
        for label in ["Gain", "Rayon"] {
            assert_eq!(node(label).role(), Role::Slider);
            assert!(node(label).numeric_value().is_some());
            assert_eq!(node(label).min_numeric_value(), Some(0.0));
            assert_eq!(node(label).max_numeric_value(), Some(1.0));
            assert!(node(label).supports_action(Action::Increment));
        }
        assert!(node("Indisponible").is_disabled());
        output.textures_delta.clear();
    }

    #[test]
    fn keyboard_activation_toggles_once_and_respects_disabled_state() {
        let ctx = egui::Context::default();
        let mut on = false;
        let mut output = ctx.run_ui(Default::default(), |ui| {
            switch(ui, &mut on, "Mesures audio").request_focus();
        });
        output.textures_delta.clear();
        let key = || egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let mut changed = false;
        let mut focused = false;
        let mut output = ctx.run_ui(key(), |ui| {
            let response = switch(ui, &mut on, "Mesures audio");
            changed = response.changed();
            focused = response.has_focus();
            assert_eq!(response.rect.size(), vec2(34.0, 18.0));
        });
        output.textures_delta.clear();
        assert!(on && changed && focused);
        let mut output = ctx.run_ui(key(), |ui| {
            ui.disable();
            assert!(!switch(ui, &mut on, "Mesures audio").changed());
        });
        output.textures_delta.clear();
        assert!(on);
    }
}

/// The narrowest and the widest a `coord_table` field is drawn. The narrowest
/// is what `-0.500` takes in the field style with `FIELD_PADDING_X` either
/// side: a `DragValue` never draws narrower than its text, so a smaller
/// minimum would only pretend.
const COORD_FIELD_MIN: f32 = 48.0;
const COORD_FIELD_MAX: f32 = 72.0;
/// The side padding of a numeric field: tighter than a button's, a cell of
/// digits having no word to breathe around.
const FIELD_PADDING_X: f32 = 4.0;
/// The height of a `coord_table`'s head row: a small caption, not a control.
const COORD_HEAD_HEIGHT: f32 = 14.0;
/// The width of a lone numeric field at the right end of a `label_row` — the
/// delays, the band limits — so the fields of consecutive rows line up.
pub const FIELD_WIDTH: f32 = 72.0;

/// The text style of every numeric field of the editors: monospace at
/// `FONT_SIZE_SECTION`, as the web's 11 px inputs. Digits of one width, so a
/// column of them reads as a column, and one size for the lot, whatever text
/// style the row a `DragValue` sits in would otherwise hand it.
fn field_text_style() -> egui::TextStyle {
    egui::TextStyle::Name("editor-field".into())
}

/// `add`, with every `DragValue` in it drawn in the editors' field style.
fn with_field_style<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let style = ui.style_mut();
        style.text_styles.insert(
            field_text_style(),
            egui::FontId::new(theme::FONT_SIZE_SECTION, egui::FontFamily::Monospace),
        );
        style.drag_value_text_style = field_text_style();
        style.spacing.button_padding.x = FIELD_PADDING_X;
        add(ui)
    })
    .inner
}

/// A numeric field `width` wide in the editors' field style, for the right
/// end of a `label_row`.
pub fn number_field(ui: &mut Ui, width: f32, drag: egui::DragValue<'_>) -> Response {
    with_field_style(ui, |ui| {
        ui.add_sized(vec2(width, ui.spacing().interact_size.y), drag)
    })
}

/// An editable number in a [`coord_table`] cell.
pub struct CoordField {
    pub value: f32,
    /// How far one point of drag moves it.
    pub speed: f64,
    pub range: Option<std::ops::RangeInclusive<f32>>,
    /// The decimals shown — the same for every value of a column, so the
    /// column reads as one.
    pub decimals: usize,
}

/// One cell of a [`coord_table`].
pub enum CoordCell {
    Field(CoordField),
    /// A value there is nothing to show for — a direct channel with no speaker
    /// to stand at — drawn as a dash rather than filled with a made-up zero.
    Blank,
    /// No cell at all: the polar table's metres row has only a distance.
    Empty,
}

impl CoordCell {
    pub fn field(value: f32, speed: f64, decimals: usize) -> Self {
        Self::Field(CoordField {
            value,
            speed,
            range: None,
            decimals,
        })
    }

    /// Hold the field to `range`; a blank or empty cell is left as it is.
    pub fn in_range(self, range: std::ops::RangeInclusive<f32>) -> Self {
        match self {
            Self::Field(field) => Self::Field(CoordField {
                range: Some(range),
                ..field
            }),
            other => other,
        }
    }
}

/// One row of a [`coord_table`]: its label, the help the label opens in a
/// card under the table, and a cell under each head.
pub struct CoordRow<'a> {
    pub label: &'a str,
    pub help: Option<Help<'a>>,
    pub cells: [CoordCell; 3],
}

/// The width of a `coord_table`'s label column: its widest label.
fn coord_label_width<'a>(ui: &Ui, labels: impl Iterator<Item = &'a str>) -> f32 {
    labels
        .map(|label| {
            egui::WidgetText::from(label)
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Body,
                )
                .size()
                .x
        })
        .fold(0.0_f32, f32::max)
}

/// The narrowest a `coord_table` with these row labels is drawn: its fields
/// at their minimum. What a caller placing something beside the table has
/// to leave it.
pub fn coord_table_min_width(ui: &Ui, labels: &[&str]) -> f32 {
    coord_label_width(ui, labels.iter().copied())
        + 3.0 * COORD_FIELD_MIN
        + 3.0 * ui.spacing().item_spacing.x
}

/// The web's `.cart-coord-table`: a corner, the three axis heads, then one
/// row per representation — its label, then a field under each head.
///
/// Every field is drawn the same width in one `Grid`, so the columns line up
/// whatever the row labels measure. Two runs of `label, field, label, field`
/// on a `horizontal` each — which is what stood here — put the X of the
/// metres row a dozen points right of the X above it, "Real (m)" being wider
/// than "Norm.", and every other field with it.
///
/// The fields share what the label column leaves, `COORD_FIELD_MIN` to
/// `COORD_FIELD_MAX` each; past the minimum the table does overflow, on a
/// panel narrower than the layout allows. Returns the `(row, axis, value)` of
/// the cell edited this frame, if any, for the caller to map back to its own
/// quantity.
pub fn coord_table(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    heads: [&str; 3],
    rows: &mut [CoordRow<'_>],
) -> Option<(usize, usize, f32)> {
    let spacing = ui.spacing().item_spacing;
    let row_height = ui.spacing().interact_size.y;
    let label_width = coord_label_width(ui, rows.iter().map(|row| row.label));
    let field_width = ((ui.available_width() - label_width - 3.0 * spacing.x) / 3.0)
        .clamp(COORD_FIELD_MIN, COORD_FIELD_MAX);
    let mut edited = None;
    with_field_style(ui, |ui| {
        egui::Grid::new(id)
            .num_columns(4)
            .min_col_width(0.0)
            .spacing(spacing)
            .show(ui, |ui| {
                // The corner, then a head centred over each column.
                ui.allocate_exact_size(vec2(label_width, COORD_HEAD_HEIGHT), Sense::hover());
                for head in heads {
                    ui.add_sized(
                        vec2(field_width, COORD_HEAD_HEIGHT),
                        egui::Label::new(
                            egui::RichText::new(head)
                                .size(theme::FONT_SIZE_SECTION)
                                .color(theme::TEXT_MUTED),
                        )
                        .selectable(false),
                    );
                }
                ui.end_row();
                for (row_index, row) in rows.iter_mut().enumerate() {
                    // Every label cell is the label column's width, so the
                    // grid needs no second pass to line the columns up.
                    let label = ui
                        .allocate_ui_with_layout(
                            vec2(label_width, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| ui.add(egui::Label::new(row.label).selectable(false)),
                        )
                        .inner;
                    if let Some(help) = row.help {
                        super::help::trigger(ui, label.rect, help);
                    }
                    for (axis, cell) in row.cells.iter_mut().enumerate() {
                        let size = vec2(field_width, row_height);
                        match cell {
                            CoordCell::Field(field) => {
                                let mut drag = egui::DragValue::new(&mut field.value)
                                    .speed(field.speed)
                                    .fixed_decimals(field.decimals);
                                if let Some(range) = &field.range {
                                    drag = drag.range(range.clone());
                                }
                                if ui.add_sized(size, drag).changed() {
                                    edited = Some((row_index, axis, field.value));
                                }
                            }
                            CoordCell::Blank => {
                                ui.add_sized(
                                    size,
                                    egui::Label::new(
                                        egui::RichText::new("—").color(theme::TEXT_DIM),
                                    )
                                    .selectable(false),
                                );
                            }
                            CoordCell::Empty => {
                                ui.allocate_exact_size(size, Sense::hover());
                            }
                        }
                    }
                    ui.end_row();
                }
            });
    });
    for row in rows.iter() {
        if let Some(help) = row.help {
            super::help::card(ui, help);
        }
    }
    edited
}

#[cfg(test)]
mod coord_table_tests {
    use super::*;

    fn rows<'a>(first: &'a str, second: &'a str) -> [CoordRow<'a>; 2] {
        [
            CoordRow {
                label: first,
                help: None,
                cells: [0.5, -0.25, 1.0].map(|v| CoordCell::field(v, 0.001, 3)),
            },
            CoordRow {
                label: second,
                help: None,
                cells: [1.5, -0.75, 3.0].map(|v| CoordCell::field(v, 0.01, 2)),
            },
        ]
    }

    /// The fields the table painted: one filled rect per `DragValue`, keyed
    /// by the left edge of the column it stands in.
    fn field_lefts(output: &egui::FullOutput) -> Vec<f32> {
        let mut lefts: Vec<f32> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) if rect.fill != Color32::TRANSPARENT => {
                    Some(rect.rect.left())
                }
                _ => None,
            })
            .collect();
        lefts.sort_by(f32::total_cmp);
        lefts
    }

    /// The X of the metres row stands under the X of the normalised row,
    /// however much longer one label is than the other: the bug this pins had
    /// each row lay its own label and fields out in turn, so a wider label
    /// pushed its whole row of fields to the right.
    #[test]
    fn the_fields_of_a_column_share_a_left_edge_whatever_the_labels_measure() {
        let ctx = egui::Context::default();
        let mut table = rows("N.", "A much longer row label");
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.set_max_width(320.0);
            coord_table(ui, "t", ["X", "Y", "Z"], &mut table);
        });
        output.textures_delta.clear();
        let lefts = field_lefts(&output);
        assert_eq!(lefts.len(), 6, "six fields, one rect each: {lefts:?}");
        for column in lefts.chunks(2) {
            assert!(
                (column[0] - column[1]).abs() < 0.5,
                "a column's two fields do not share a left edge: {lefts:?}"
            );
        }
    }

    /// The table stays inside the width it is given: a group inset's width
    /// on a panel at its default width, and on one 120 pt narrower. (On the
    /// narrowest panel the inset is 142 pt, and no table of six fields fits
    /// that; `PANELS.md` allows the overflow there and nowhere else.)
    #[test]
    fn the_table_keeps_to_the_width_it_is_given() {
        for width in [362.0, 242.0] {
            let ctx = egui::Context::default();
            let mut table = rows("Norm.", "Real (m)");
            let mut taken = 0.0;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                ui.set_max_width(width);
                coord_table(ui, "t", ["X", "Y", "Z"], &mut table);
                taken = ui.min_rect().width();
            });
            output.textures_delta.clear();
            assert!(taken <= width + 0.5, "{taken} pt of {width}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame with a progress button showing `text` in `progress`, after
    /// `events`: its rect, and whether it was clicked.
    fn frame(
        ctx: &egui::Context,
        text: &str,
        progress: ButtonProgress,
        events: Vec<egui::Event>,
    ) -> (egui::Rect, bool) {
        let mut out = (egui::Rect::NOTHING, false);
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    vec2(400.0, 100.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                let response =
                    progress_button(ui, text, &["Start", "Starting the engine…"], progress);
                out = (response.rect, response.clicked());
            },
        );
        output.textures_delta.clear();
        out
    }

    fn click(ctx: &egui::Context, text: &str, progress: ButtonProgress, at: egui::Pos2) -> bool {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(ctx, text, progress, vec![egui::Event::PointerMoved(at)]);
        frame(ctx, text, progress, vec![button(true)]);
        frame(ctx, text, progress, vec![button(false)]).1
    }

    #[test]
    fn the_button_keeps_its_rect_whatever_it_shows() {
        let ctx = egui::Context::default();
        let rect = frame(&ctx, "Start", ButtonProgress::Idle, Vec::new()).0;
        for (text, progress) in [
            ("Start", ButtonProgress::Fill(0.0)),
            ("Start", ButtonProgress::Fill(0.6)),
            ("Start", ButtonProgress::Fill(1.0)),
            ("Starting the engine…", ButtonProgress::Busy),
            ("Start", ButtonProgress::Idle),
        ] {
            assert_eq!(
                frame(&ctx, text, progress, Vec::new()).0,
                rect,
                "{text} {progress:?}"
            );
        }
    }

    #[test]
    fn the_fill_is_painted_under_the_label_up_to_its_share() {
        let ctx = egui::Context::default();
        let fill = theme::ACCENT.gamma_multiply(0.3);
        let painted = |progress| {
            let mut rect = egui::Rect::NOTHING;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                rect = progress_button(ui, "Start", &[], progress).rect;
            });
            output.textures_delta.clear();
            let fills: Vec<_> = output
                .shapes
                .iter()
                .enumerate()
                .filter(|(_, s)| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == fill))
                .map(|(i, s)| (i, s.clip_rect))
                .collect();
            let label = output
                .shapes
                .iter()
                .position(|s| matches!(s.shape, egui::Shape::Text(_)))
                .expect("a label");
            (rect, fills, label)
        };
        let (rect, fills, label) = painted(ButtonProgress::Fill(0.5));
        let [(index, clip)] = fills[..] else {
            panic!("one fill expected, got {fills:?}")
        };
        assert!(index < label, "the fill must sit under the label");
        assert!((clip.left() - rect.left()).abs() < 0.5);
        assert!((clip.right() - rect.center().x).abs() < 0.5);
        assert!(painted(ButtonProgress::Idle).1.is_empty());
        assert!(painted(ButtonProgress::Fill(0.0)).1.is_empty());
        let (rect, fills, _) = painted(ButtonProgress::Busy);
        assert!(fills.iter().all(|(_, clip)| rect.contains_rect(*clip)));
    }

    #[test]
    fn a_counting_down_button_takes_the_first_click_and_a_busy_one_none() {
        let ctx = egui::Context::default();
        let centre = frame(&ctx, "Start", ButtonProgress::Idle, Vec::new())
            .0
            .center();
        assert!(click(&ctx, "Start", ButtonProgress::Fill(0.4), centre));
        assert!(click(&ctx, "Start", ButtonProgress::Idle, centre));
        assert!(!click(
            &ctx,
            "Starting the engine…",
            ButtonProgress::Busy,
            centre
        ));
    }
}
