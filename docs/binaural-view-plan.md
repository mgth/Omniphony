# Binaural mode: the Studio shows what the engine renders

Plan for the next release (drafted 2026-10-09 on main `c06fa20e`, after #778
and #783). Two requests drive it: the 3D view must depict the binaural path
in force (a BRIR renders a measured room, so that is the room to draw), and
the output mode must drive the HRTF select, not the reverse. The rest is a
tour of the mode with the visualisation changes it suggests.

## 0. What renders, and what the view draws

The engine has three headphone paths (`BinauralLiveParams`,
`renderer/src/live_params.rs`):

| Path | Geometry the engine uses | Added around the direct sound |
|---|---|---|
| **Direct** (`mode: direct`, HRTF source) | each source read as a direction straight off its normalized position: **no room warp** (`RoomRatios::for_output` returns `UNIT`, #783); distance cues measured against the unit cube's surface (#760), scaled by `unit_scale_m` | ITD, air absorption, shoebox early reflections (listener-centred room `reflections.roomM`, grown to contain the scene), late reverb |
| **Virtual room** (`mode: cascaded`, HRTF source) | the speaker stage on the **editable layout**, with the **live room warp** (the user's room ratio), then one HRTF pair per virtual speaker; the virtual bus is metered (`render_metering.rs`) | the same cues, applied to the virtual speakers |
| **Measured room** (`brir` source, mode forced to cascaded) | the speaker stage on the **set's loudspeakers** projected onto the cube (`SpeakerLayout::from_brir_emitters`), still with the live room warp; one measured pair per virtual speaker | nothing: the measurement is the room |

The Studio's scene (`omniphony-studio-egui/scene/src/view/`) draws one
picture whatever the path:

- the user's room box from `roomRatio`, with the speaker room's metre guides;
- objects, beds and speakers all placed through the live room warp
  (`scene_position(adm, room)`);
- every speaker cube at 18 % opacity as soon as `outputMode == "binaural"`
  (`speakers.rs`, `GHOST`), labels dimmed;
- the head rotating with the pose.

Mismatches, by path:

- **M1 Direct.** The engine applies no warp; the view does. A Sphere bed `L`
  at −30° is drawn near −16° in the default 1×2×1 room (its normalized
  position is pushed to the front wall at depth 2). The room box drawn plays
  no part in the render, and the distance scale (`unitScaleM`) is invisible.
- **M2 Virtual room.** The virtual speakers are what renders and they are
  metered, yet they are ghosted as if unused. The listening room that
  produces the reflections (a second, listener-centred shoebox, grown to
  contain the scene) is not drawn at all; its sliders in the Binaural tab
  have no counterpart in the scene.
- **M3 Measured room.** The set's loudspeakers are drawn through the user's
  room box and warp (`binaural.brir.layout` is the normalized layout), not at
  their measured positions in metres; the user's room is drawn although the
  editable layout does not render; the engine still warps the panning onto
  the set with the user's room ratio (§3).
- **M4 Select coupling.** `hrirSource == brir` removes *Headphones* from the
  output-mode select and the select follows `modeEffective`
  (`renderer.rs: room_forces_virtual`). Leaving `brir` for another source
  loses the file: `config_store` writes `brir_sofa_path: None`, and a bare
  `brir` sent later parses as `Brir("")` ("No BRIR file selected", KEMAR
  plays). The same holds for `sofa` / `hrtf_sofa_path`.
- **M5.** `binaural.brir.layoutError` (a set wider than the speaker stage,
  rendered on the editable layout instead) is published and shown nowhere.

## 1. The output mode drives the HRTF source (M4)

Considered and not retained: making the measured room a fourth output mode.
It would duplicate the source select (file, status, load options) and the
request keeps BRIR a source. The rule becomes: **the output mode says which
sources are offered; picking a mode that cannot use the current source
resets the source to KEMAR.**

Studio (`src/panels/renderer.rs`, `src/panels/binaural.rs`, both views):

- `output_mode_row` always offers the three modes. Changing to *Headphones*
  while the source is `brir` sends `hrir_source = saf` first, then
  `binaural_mode = direct` (and `output_mode = binaural`). Changing to
  *Headphones (virtual room)* keeps the source. Changing to *Speakers*
  changes nothing else. Not optimistic, as today: the echo moves the select.
- The HRTF select lists `brir` only when the mode in force is the virtual
  room (`OutputMode::from_state == BinauralCascaded`). If the state still
  says `brir` under direct (an OSC client, an older engine), the entry is
  shown anyway so the select never hides the truth.
- `OutputMode::from_state` keeps reading `modeEffective`; the engine's
  forcing stays as the safety net. `room_forces_virtual` and the
  `outputMode.brirForcesVirtual` hover go; a note under the HRTF select in
  direct mode says where the measured room lives. i18n in the 8 languages.
- Essentials inherits it (same row, `essentials_hrtf`).

Engine (`renderer/src/options.rs`, `hrir_source`):

- Remember the last BRIR and SOFA paths beside the source
  (`BinauralLiveParams::last_brir_path`, `last_sofa_path`, set whenever a
  `brir:<p>` / `sofa:<p>` arrives or the config seeds one). A bare `brir` or
  `sofa` resolves against them, then against the config key, as
  `config_seed` already does; `config_store` writes the remembered paths
  even when the live source is another one. Kemar ↔ BRIR round trips keep
  the file across Save.
- Publish `brirSofaPath` / `hrtfSofaPath` from the remembered paths too, so
  the select can show the file name next to the entry while it is inactive.
- Tests: option set/get/config round trip with the remembered paths; a
  snapshot test for the two keys; a Studio test for the select's contents
  per mode and for the source reset on the mode change.

Persistence: unchanged class (render-affecting, reaches `config.yaml`
through Save only); no new write, no tripwire touched.

## 2. The view follows the rendered path (M1–M3, M5)

### 2a. One resolution of the path, in the core

A `RenderPath` (`core/src/model`, `Speakers | Direct | VirtualRoom |
MeasuredRoom`) resolved from `outputMode`, `modeEffective`, `hrirSource` and
the presence of `brir.layout`, replacing the three ad-hoc readers
(`BinauralPath::of`, `OutputMode::from_state`'s callers in the scene,
`speakers.rs`'s ghost test). Panels and the scene read it; nothing else
parses the binaural document for the path. Unit tests on the resolution,
including the fallback cases (`brir` source but no resident set → the
virtual room on KEMAR, as the engine renders it).

### 2b. Direct: the unit cube, no warp

- Positions placed with a unit `RoomRatio` (the mirror of
  `RoomRatios::for_output`): `scene_position` takes the cube for objects,
  beds and the reference speakers whenever the path is `Direct`.
- The unit cube (listener-centred, −1..1 on every axis) is drawn in place of
  the user's room box, in its own colour, with one metre guide: "1 unit =
  `unitScaleM` m". The *Distance scale* slider moves the guide live.
- Speakers hidden by default: nothing feeds them. A Display switch "Show the
  speaker layout on headphones" (display pref, kept live) brings them back
  ghosted, as a reference.
- Head pose unchanged.

### 2c. Virtual room: the virtual speakers, and the listening room

- Keep the warp and the user's room box: that *is* the virtual room.
- Speakers drawn as virtual speakers (the *Virtual* look of §2e), full
  opacity, labels, scaled by the virtual bus meters the engine already
  publishes.
- The listening room drawn as a second, dashed, listener-centred box in
  metres, only while reflections are enabled. The engine publishes the
  extents it actually uses (`reflections.roomEffectiveM`, after the growth
  rule of `reflections.rs`) rather than the Studio re-deriving the floor.
  Applies to Direct too (the cues are the same there).

### 2d. Measured room: the set's own geometry

Engine (`runtime_control/src/snapshot.rs`, `brir_stage.rs::BrirSummary`):

- Publish `brir.emittersM` (each loudspeaker's position in metres, renderer
  frame, set order), `brir.listenerM`, and, when the file carries them,
  `brir.roomCornersM` (`RoomCornerA`/`RoomCornerB` of the SOFA shoebox
  rooms) and `brir.roomType`. The reader exposes attributes
  (`sofar::reader::get_attribute`); the corners are variables to read in
  `brir.rs` beside `ListenerPosition`. Snapshot test with a synthetic set.

Studio:

- With the path `MeasuredRoom`, draw the measured room: loudspeakers at their
  metre positions (the *Measured* look of §2e), named as `brir.layout` names
  them (`L`, `R`, … `E7`), a room box from the corners or else the
  loudspeakers' bounding box with a margin, in the measured room's colour;
  the user's room box hidden; the dimension guides in the set's metres. The
  scene's unit stays the half-width so the camera needs no change.
- Objects and beds keep the live warp (that is how they are panned onto the
  set, §3 pending).
- `brir.layoutError` shown as a warning in the HRTF group and in the badge
  (§2f): the view must not draw the measured room while the editable layout
  renders.
- Speaker list: unchanged (already the set's layout, read-only).

### 2e. Real, virtual, measured: one look per kind of speaker

A speaker cube already spends its colour (crossover band, selection, the
selected object's gain mix), its opacity (`spatialize: false`, ghosting)
and its size (level) on other meanings. Shape is the free axis, so the kind
of speaker is said by the shape of the cube:

| Kind | When | Look |
|---|---|---|
| **Real** | speaker output | solid cube, band colour, driver disc — today's look |
| **Virtual** | virtual room (HRTF cascade) | **wireframe cube**: the twelve edges, depth-tested (`frame.lines`), in the band colour; the driver disc kept, so the orientation still reads; the level still scales it; selection and gain mix recolour the edges |
| **Measured** | measured room (BRIR) | wireframe cube in the **measured room's colour** (the set's loudspeakers carry no band), the same colour as its room box; the names the set was matched to |
| **Reference** | direct, with the "show the speaker layout" switch | today's ghost: solid, 18 % |

A wireframe reads as "not a physical thing" at any zoom and keeps every
other cue. Considered and not retained: a ring around the cube (the objects'
ring, two meanings for one mark), a hue shift (collides with the bands),
dashed edges (the line pipeline has no dash; eight short segments per edge
if a solid 1 px wire turns out too close to the room edges, which are
overlay lines in another colour).

Implementation: `SpeakerVisual::look` (`Real | Virtual | Measured |
Reference`) resolved from the `RenderPath` in `speakers::collect`;
`speakers::emit` draws the cube mesh for *Real* and *Reference* and a
`wire_cube(model, colour)` of twelve segments for the other two. Picking,
band bars and labels unchanged.

The speaker list carries the same cue: the row's position thumbnail
(`row_glyphs`) draws its frame dashed for a virtual or measured speaker, and
the Speakers section summary names the kind ("Virtual room · 7.1.4",
"Measured room · bbcrdlr_systemG"), so the list and the scene agree.

### 2f. A badge in the viewport

A small label beside the scene-fx bar naming the path and the set in force:
"Headphones · KEMAR", "Virtual room · HUTUBS pp12", "Measured room ·
bbcrdlr_systemG, 13 loudspeakers". The fallback states in the warn colour
("KEMAR, fallback: <hrirError>", the BRIR load error, the layout error).
Clicking it opens the Listening / Renderer section. The Essentials user then
reads the path without the Advanced board.

## 3. Engine question, listening required: the warp in the measured room

`RoomRatios::for_output` returns the live room for every cascade, the BRIR
layout included: the user's loudspeaker room shapes the panning onto a
measured room that has its own geometry. Candidates: (a) `UNIT` for the BRIR
layout (its loudspeakers are already placed by direction); (b) a ratio
derived from the loudspeakers' extents. Own issue and PR, after a listening
pass with the BBC System G set (`dumps/brir/bbcrdlr_systemG.sofa`); not part
of the view work, whose §2d draws what the engine does either way.

## 4. Further visualisation proposals

- **Ear meters on the head**: two bars at the ears from the ear meters, in
  every headphone path.
- **Image sources of the selected object**: its six first-order reflections
  as ghost points across the listening room's walls (Direct and Virtual
  room, reflections on) — the model drawn as it computes.
- **Head ring**: a thin yaw ring around the head with the front mark; with a
  BRIR under head tracking, the measured orientations as ticks and the one
  in use highlighted (publish `brir.orientationInUse`).
- **Sphere reading** (ties to #773): a toggle projecting the objects'
  directions on a wire sphere around the head, in Direct.
- **Scale bar**: a "1 m" bar in the scene whenever a metre frame is drawn.

## 5. Sequencing

| PR | Content | Side |
|---|---|---|
| A | §1: the output mode drives the source; remembered paths | Studio + engine |
| B | §2a `RenderPath` in the core; §2b Direct on the unit cube; speaker visibility switch; §2e speaker looks (wire cube, list thumbnail) | Studio |
| C | §2c effective listening room published and drawn | engine + Studio |
| D | §2d BRIR geometry published; measured room drawn; `layoutError` surfaced | engine + Studio |
| E | §2f badge, then the §4 picks | Studio |

§3 as its own issue after listening. Each PR: the architecture ratchet
(panels draw, the path resolution and the OSC reading live in the core),
the persistence classification (new switches are display prefs; nothing
new reaches `config.yaml`), i18n ×8, `BINAURAL.md` ("What Studio shows")
and `docs/studio-native-ui-specs/scene.md` updated. Verification on an
isolated orender + Studio pair, with the BBC set for D.
