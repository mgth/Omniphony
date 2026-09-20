# Panels: how a Studio panel is laid out

The web Studio drew every panel with the same three frames, and a panel read
the same way whichever one it was: a section, groups inside it, rows inside
the groups. The egui port kept the section and lost the rest — a heading
became a bold label with a gap above it, and the renderer panel became a
column of rows with no visible reason for their order. This document is the
rule that brings the frames back, so that a panel written next year looks
like one written today. It is about layout; `ARCHITECTURE.md` says what a
panel may *do*.

Every constant named here is in `src/ui/theme.rs`; every widget in
`src/ui/section.rs`, `src/ui/group.rs`, `src/ui/widgets.rs` and
`src/ui/help.rs`. The web classes are quoted for the reader who compares with
`omniphony-studio/src/styles/app.css`.

## The three frames

```
 ─────────────────────────────────────────────── rule (SECTION_RULE)
 ▾ ▣ Section title  [header widget]   summary…     ← Section header, 11 px, its icon
   Row that applies to the whole section    [⌄]
   ┌───────────────────────────────────────────┐  ← Group card: hairline,
   │ Group title  status         [key control] │    radius 8, GROUP_FILL
   │     ┌───────────────────────────────────┐ │
   │     │ Row label                   [⌄]   │ │  ← Inset: no border, radius 6,
   │     │ Row label               ──●── 0.5 │ │    GROUP_FILL again, indented
   │     │ note under the rows               │ │    INSET_INDENT
   │     └───────────────────────────────────┘ │
   └───────────────────────────────────────────┘
   ┌───────────────────────────────────────────┐
   │ Group that is its key control  [switch]   │  ← Group::bar: no inset
   └───────────────────────────────────────────┘
```

| Frame | Widget | Web class | Look |
|---|---|---|---|
| **Section** | `Section::new(id, title_key)` | `.info-section` | A rule above (`SECTION_RULE`), a header row — chevron, the section's icon (`Section::icon`, from `ui::icons`, 13 px, `TEXT_MUTED` folded and `TEXT_STRONG` open), title at `FONT_SIZE_SECTION` in `TEXT_STRONG`, an "i" shown on hover, an optional header widget, the summary while folded — and a body that opens in place. |
| **Group** | `Group::new(title)` | `.renderer-subpanel`, `.adaptive-subpanel`, `.input-panel-shell` | A card: `GROUP_FILL` on a `HAIRLINE` stroke, `GROUP_RADIUS`, padding `GROUP_PADDING_X/Y`, `GROUP_GAP` from the group before. Its bar: title at `FONT_SIZE` in `TEXT_WHITE`, an optional status, and the actions at the right end. |
| **Inset** | drawn by `Group::show`; `group::inset` on its own | `.renderer-subpanel-body` | `GROUP_FILL` again with no stroke, `CONTROL_RADIUS`, padding `INSET_PADDING_X/Y`, indented `INSET_INDENT` from the card's left edge. Rows inside are `ROW_GAP` apart. Not drawn at all when its body adds nothing. |

Three frames is the whole vocabulary. Nothing nests deeper: a group holds
rows, never another group. Where the web nested a box in a box (the hybrid
mix inside the backend body), the inner box becomes rows of the same inset,
under a `tab_bar` if it had tabs.

## What goes where

**Section header.** The icon, the title, the section's help ("i" on hover:
`Section::info(prefix)` or `Section::help(key)`), a `header_widget` for a
readout worth keeping in view with the section folded — a gauge, a meter, a
bar — and the `summary`, one line, shown only while folded. No control in the
header: the header opens the section.

**Section icon.** Every collapsible section carries one (`Section::icon`),
a glyph from `ui::icons` in the scene-effects bar's family (24-unit box,
1.8 stroke): a folded panel is told apart by its glyph before its title is
read. A section that the bar already has a button for uses the bar's icon
(Display, Objects, Heatmaps); the others have one of their own, named
`SECTION_*`. The pinned editors have no icon: they are not sections that
fold, they close with their selection.

**Section body, before the groups.** Readouts that belong to the header
widget (the numbers under a gauge), then the rows that apply to the whole
section (the output mode), then a `tab_bar` if the section has tabs. Then
the groups.

**Group bar.** Left to right: the title; a short **status** in its own colour
when the group is doing something (`computing…`, `up to date`, an error) —
nothing when it is at rest; at the right end the **actions**: the group's one
key control, the select or switch or pair of buttons that decides what the
inset shows or acts on the whole group, plus at most one small readout beside
it (what the engine resolved the choice to, only when that differs from the
choice). Two controls in a bar is one too many: the second goes in the
inset.

**Inset.** The rows. The control that gates other rows comes first and the
rows it gates follow it, in the same inset, and disappear with it: conditional
rows sit *under* what toggles them, never above something the user clicks in
series. Notes (`widgets::note`) come last, under the rows they explain.

**A group that is only its key control** — a select, a switch — is drawn with
`Group::bar(ui)`: a single line in a card, no inset. A group whose inset is
conditional from end to end (the distance model's metric, the distance
diffuse's parameters) uses `show` and lets the empty inset vanish on its own.

**What is never a group.** A list (objects, speakers, channels): its rows are
the content, and it takes the section's width. An editor pinned at the foot
of an overlay: it is a section, headed by `section::pinned_header` — the rule
and the title without the chevron, and at the right end which row of the list
it is on — with its own rows and groups under it. A single row: a row.

## Rows

Every row is one of the `widgets::` functions, so that the same control looks
the same in every panel. The label is on the left and truncates first; the
controls are placed from the right and never overflow the panel.

| Row | Function | Use |
|---|---|---|
| Label and a control | `label_row`, `label_row_help`, `label_row_info` | A select (`bounded_combo`, 130–160 px preferred), a `DragValue`, a text field. `_help` opens a card under the row; `_info` opens the overlay, for a label that heads a block. |
| Label and a switch | `switch_row_help` | Every boolean. A switch, never a checkbox. |
| Label, slider, readout | `value_slider_help` | A parameter with a unit: the readout is monospace in `TEXT_STRONG`, formatted by the caller. The track shrinks before the label does. |
| Label with the value, slider | `slider_line_help` | The Display sliders, where the value is part of the label and the track takes the rest. |
| Label and buttons | `label_buttons_help` | Actions on a whole block (layout, delete). Wraps to a line of equal buttons when the panel is narrow. |
| Sub-row | `label_row` with a `RichText` at `FONT_SIZE_SMALL` in `TEXT_FAINT` | The quieter rows under a row they detail (the mirror-axis switches). |
| Caption over a block | `help::label` + `help::card` | A label that heads several widgets on the next lines. |
| Coordinate table | `coord_table` | The editors' position: axis heads over a column of equal fields per axis, a row per representation (normalised, metres). One `Grid`, so the columns line up whatever the row labels measure — never two `horizontal`s of `label, field, label, field`. A cell with nothing to show is a dash. The editors draw it through `coord_table_with_gizmo`, which puts the 3D Edit toggle at its right, or under it on a panel too narrow for both. |
| Numeric field | `number_field` | A lone `DragValue` at the right end of a `label_row`, `FIELD_WIDTH` wide so consecutive fields line up, in the editors' field style (monospace, 11 px) like the cells of the table. |
| Note | `note` | One line of `FONT_SIZE_SMALL` in `TEXT_MUTED` under the rows it explains. |
| Banner | `banner`, `banner_with` | Something the user has to act on: a missing bridge, an update. |
| Tabs | `tab_bar` | Equal-width tabs, one active, above the groups they switch. |

Sizes and colours, so a title is read as a title:

| Text | Size | Colour |
|---|---|---|
| Section title | `FONT_SIZE_SECTION` (11) | `TEXT_STRONG` |
| Group title | `FONT_SIZE` (12) | `TEXT_WHITE` |
| Row label | `FONT_SIZE` (12) | `TEXT` (the default) |
| Sub-row label, note, caption, bar readout | `FONT_SIZE_SMALL` (10) | `TEXT_FAINT` (sub-row), `TEXT_MUTED` (note, caption, readout) |
| Value readout | `FONT_SIZE_SMALL`, monospace | `TEXT_STRONG`; `TEXT_DIM` for a derived one (a grid step) |
| Status | `FONT_SIZE_SMALL` | `OK`, `WARN`, `ERROR` |

## Help

Three levels, one trigger each, and never a "?" button:

- **Section**: the "i" beside the title, shown while the pointer is on the
  header (`Section::info` / `Section::help`). Opens the centred overlay.
- **Group**: the title itself, dotted-underlined (`Group::info`,
  `Group::help`, or `Group::overlay` for a body that depends on state). Opens
  the centred overlay under the group's own name.
- **Row**: the label, dotted-underlined (`…_help`). Opens a card under the
  row. A row whose label heads a block uses `label_row_info` and opens the
  overlay instead.

A tooltip (`on_hover_text`) is for a button, to say what it does in a few
words, or for a truncated label, to show it whole. It is never the only help
of a control.

## Order

**Sections** in an overlay: what comes in on the left, what goes out on the
right, each in the order the signal takes (`app.rs`, `overlays`).

**Groups** in a section: the choice that constrains the others first — the
backend, which says which evaluation modes exist — then what that choice
applies, then how things move in time, and last what the section shares with
another tab. Readouts go before the groups or in the header, never between
two groups.

**Rows** in an inset: the switch or select that gates the others first, then
its parameters in the order the web has them, then notes.

Nothing above a control may change height with the value that control sets
(a readout whose width follows a slider, a message row that appears): the
control would move under the pointer. Reserve the space or put the readout
after the control.

## Widths

A panel can be as narrow as `layout::MIN_WIDTH` (220 px) and every row has
to survive it: the label truncates and shows itself whole on hover, a combo
is bounded to 60 % of its row, a slider track shrinks to 48 px. Never size a
widget as "available width minus what the neighbours should take" — the
estimate is always short, the row overflows, and every row after it inherits
the overflow. Place the fixed parts from the right and give the rest to what
can shrink; that is what `label_row` and the group bar do.

## Writing a panel

1. Name the section (`Section::new("<webId>", "<title key>")`), its help,
   its summary, and the one readout that belongs in its header.
2. List the section's concerns. One group per concern; a concern with a
   single control is a group with a bar only, unless it belongs to the
   section as a whole (then it is a row before the groups).
3. For each group: which control decides what the rest shows? That one is
   the bar's action. Everything else is a row of the inset, gating controls
   first.
4. Pick each row from the table above. If none fits, add one to
   `widgets.rs` rather than laying it out in the panel.
5. Every control calls a typed command in `core/src/host/commands/`
   (`ARCHITECTURE.md`). The bar's action reads its value into a local before
   the group and sends after it, so the group borrows nothing the body needs.
6. Check the panel at 220 px and at its default width, with every group's
   conditional rows on and off.

A group, in code:

```rust
let mut chosen = current.clone();
Group::new(t("distance.model"))
    .overlay(|| help::Overlay::keys("distance.modelInfoTitle", "distance.modelInfoBody"))
    .actions(|ui| {
        widgets::bounded_combo(ui, 150.0, |ui, w| {
            egui::ComboBox::from_id_salt("distance-model")
                .selected_text(label_of(&current))
                .width(w)
                .truncate()
                .show_ui(ui, |ui| { /* selectable_value into `chosen` */ })
        });
    })
    .show(ui, |ui| {
        if current != "none" {
            self.metric_row(ui, /* … */);
        }
    });
if chosen != current {
    render::control_distance_model(&self.host, chosen);
}
```

## Where each panel stands

| Section | Groups | Status |
|---|---|---|
| Renderer (`renderer.rs`) | Backend, Evaluation, Distance model, Distance diffuse, Ramp; Crossover on both tabs | follows this document |
| Renderer › Binaural tab (`binaural.rs`) | HRTF, Distance, Listening room, Head tracking | follows |
| Latency (`latency.rs`) | Global far actions, Local resampling controller (its switch in the bar, pause and the wizard in the inset), Stabilization phases | follows |
| Master (`audio.rs`) | Auto-gain (switch and clip dot in the bar, ceiling in the inset) | follows |
| DRC / Loudness (`drc.rs`) | DRC (mode; weight), Loudness (switch; readouts) | follows |
| Audio input (`audio_input.rs`) | Bridge input or Live source, after the mode row | follows |
| Fixed-channel sources (`sources_2d.rs`) | Height generator (choice; reason and parameters), Phantom extraction (likewise) | follows |
| Display (`display.rs`, the scene panel) | Object appearance, Trails, Speakers — each with its show switch in the bar | follows |
| Heatmaps (`display.rs`) | Objects, Total energy, Speakers, Usage breaks — each with its switch in the bar — and Common parameters | follows |
| Speaker editor (`speaker_editor.rs`) | Edit / Test tabs (`tab_bar`) under a `pinned_header`; Edit: Name and Spatialize rows, then Coordinates (mode in the bar; `coord_table` and 3D edit in the inset) Output (gain, delay in ms and samples, delay tools) and Band (the two limits in the inset; the filter shape as status and the list's filter glyph in the bar); Test: one group, the pink-noise button in the bar, trigger, isolation and level in the inset | follows |
| Channel editor (`channel_editor.rs`) | Gain row under a `pinned_header`, then Routing (Direct / Virtual in the bar; the destination speaker in the inset, direct only) and Coordinates, as the speaker editor's | follows |
| Object injection editor (`object_test.rs`) | Rotation axis (axis in the bar; radius, turn time, free-axis angles in the inset) | follows |
| Audio output, OSC, Room geometry, Diagnostics | — | rows only: nothing to group. OSC's renderer buttons stay a row under a rule for want of a title of their own |
| Objects, Speakers, Headphones lists | — | lists: rows are the content, no group |
