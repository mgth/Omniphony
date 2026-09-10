# Studio native UI, phase 2: panels

Date: 10 September 2026. Branch `feat/studio-egui-panels`, crate
[`omniphony-studio-egui/`](../omniphony-studio-egui/). Follows phase 1
([viewport parity](studio-native-ui-phase1.md)). Goal of the phase: replace the
web frontend's panels — the controls, lists, meters and the log — with native
ones driving the same renderer over the same OSC.

This document records the first pass: the control plane, the chrome and the
panels that pay for themselves immediately. The remaining panels are listed at
the end with the specification they will be built from.

## The control plane

Phase 1 was read-only for audio: the listener could register, keep a heartbeat
alive and subscribe to gain tables, nothing else. A panel has to send.

- The listener's control channel now carries any OSC message, a reconnect and
  the metering toggle. Registering restates the metering choice, and while the
  renderer's state bundle is incomplete the client re-registers once a second,
  both as the Tauri host does. Its heartbeat timeout is now the host's ten
  seconds instead of sixteen.
- `src/host/commands/` is the host's `commands/*.rs`, ported mechanically: the
  `#[tauri::command]` attribute and the state extractor are gone, the
  `AppHandle` path lookups became a `HostPaths` struct, and the bodies are
  otherwise unchanged. Both hosts therefore send byte-identical OSC, including
  the clamps, the JSON documents and the realtime sequence numbers.
- `src/host/{config,runtime_env,audio_config,peak_hold,timing_stats}.rs` are
  verbatim copies. The native host reads and writes the same `osc_config.json`
  in the same per-environment directory, so pointing one host at a renderer
  points the other at it too.
- `src/host/control.rs` is what the panels call: one method per host command
  family, each sending through the listener's socket.
- `src/i18n.rs` resolves keys against the web Studio's `en.json`, embedded at
  build time. Labels cannot drift from the web UI before the cutover; the other
  seven locales come with the cutover, when the JSON files move into the crate.

## Chrome

`src/ui/` is the Studio's look, ported from `styles/app.css`:

| Module | What |
|---|---|
| `theme.rs` | Colour tokens, the 12 px control font, the 20 px control height, the panel frame, and the `Style` the app installs |
| `layout.rs` | Side-panel widths with the web's clamp: at least 220 px, at most what the other panel leaves, so the two can meet but never overlap |
| `overlay.rs` | One floating overlay: frame, collapse button (hamburger left, speaker right), and the 10 px drag handle on the inner edge |
| `section.rs` | `.info-section`: rule, title, one-line summary, chevron, and a body bounded at `min(44vh, 420px)` that scrolls internally |
| `widgets.rs` | Switch, toggle group, value slider, meter with peak-hold cursor, status dot, banner, note, help affordance |

The overlays float above the viewport, which is drawn at full window size
behind them. No panel action can resize the scene, which is the rule
`CLAUDE.md` states for the web Studio; here it holds by construction.

Widths and collapsed flags persist in `studio-egui-prefs.json` next to the OSC
config, debounced 600 ms so a drag writes once.

## Panels in this pass

| Panel | What works | Sends |
|---|---|---|
| OSC configuration | Host, renderer port, listen port, metering switch, Connect | re-register, `/omniphony/control/metering`; saves `osc_config.json` |
| Log overlay | The 120-entry ring newest-first, level chips, filter, copy, clear, backend level select | `/omniphony/control/log_level` |
| Master | Meter with the backend peak-hold cursor and the RMS readout, gain slider with its dB label, clip dot, auto-gain and its ceiling | `/omniphony/control/realtime/master_gain` (with the sequence number), `/omniphony/control/auto_gain`, `…/auto_gain_ceiling` |
| Objects | One row per source: name in its scene colour, bed tag, meter, RMS, mute, solo | `/omniphony/control/object/<id>/mute` |
| Speakers | One row per speaker of the live layout: name, gain offset, meter, RMS, mute, solo | `/omniphony/control/config/speakers` (`speakerEdits`) |
| Display, Trails, Heatmaps | The phase 1 view controls, now in Studio sections with the web's labels | nothing (client-side view state) |
| Audio output | Format line, output backend, device with its refresh, named pipe and its destination and format, channel mapping with the unroutable-speaker warning, sample rate | `/omniphony/control/audio/output_backend`, `…/output_file`, `…/output_file_format`, `…/output_devices/refresh`, and the batched `/omniphony/control/config/audio` + its apply |
| Latency | The latency meter with the spread over four seconds, its control, smoothed and target markers, the readouts, the resampling deviation meter, the target latency with its Apply, the controller's phase and band, and the whole adaptive resampling form | `/omniphony/control/latency_target`, `…/adaptive_resampling/reset_ratio`, and the batched `/omniphony/control/config/audio`, which now carries the adaptive block |
| Binaural | The HRTF source with its parametric pinna and PRTF variants, diffuse-field EQ, head radius, the HRIR update lattice, distance scale and air absorption, early reflections and late reverb with their room, and head tracking with recentre, axis calibration, address, format, smoothing, inversion and the live pose | `/omniphony/control/binaural/*`, `/omniphony/control/head/*` |
| Audio input | The status line, the mode, the bridge path, the pipe or the PipeWire node, description and clock, and Apply with its two paths | `/omniphony/control/render/bridge_path`, `…/render/input_pipe`, `…/config/input` and its apply, `…/input/live/clock_mode`, `…/save_config`, `…/reload_config` |
| Fixed-channel sources | Stream state, rear-channel placement, the synthetic-objects switch, the height generator and the phantom extractor with the parameters each declares, and why each stage is or is not running | `/omniphony/control/option`, `…/object_generator/param`, `…/phantom_extract/param` |
| Room geometry | The five metre dimensions, the derived scale, and the front/rear blend when the two depths differ | `/omniphony/control/config/layout` (`radiusM`), `…/room_ratio`, `…/room_ratio_rear`, `…/room_ratio_lower`, `…/room_ratio_center_blend` |
| DRC and loudness | The compression mode and weight, the loudness switch with its three readouts, and the gain gauge while metering is on | `/omniphony/control/input/drc_mode`, `…/input/drc_weight`, `…/loudness` |
| Speaker editor | Reorder, delete, name, the cartesian and polar coordinate tables in normalised units and metres, gain, delay, spatialise, band limits, and the Test tab with its trigger, isolation, level and idle feed | `/omniphony/control/config/layout` (`speakerEdits`, `moveSpeaker`, `removeSpeaker`) and its apply, `…/config/speakers` for the delay, `…/realtime/speaker_gain`, `…/speaker_test`, `…/speaker_test/idle_feed` |
| Config profiles | The picker at the top of the left overlay, create, rename and delete, each re-populated from the renderer's echo rather than applied optimistically | `/omniphony/control/profile/switch`, `…/create`, `…/rename`, `…/delete` |
| OSC status and banners | The status line reports the four states with the web's colours and names the connected renderer's flavour; the three banners say a renderer is missing, that one came up without its decoder bridge, or that the one answering is not the one this Studio would start | nothing |
| About | The brand row and the `?` beside the connection line open it: name, description, version, licence, repository link, which renderer is answering (with its ABI, and its executable in the tooltip) and which configuration that renderer is running on — including that it read none and is on built-in defaults | nothing |
| Channel editor | The per-channel gain, Virtual/Direct, the destination speaker of a direct channel, and the cartesian and polar coordinate tables in normalised units and metres; plus the layout reset in the fixed-channel section and the at-rest bed markers that make a channel selectable with nothing playing | `/omniphony/control/virtual_bed` (the whole bed, as the renderer takes a layout rather than a diff) |
| Object injection | The feature switch on the objects list, and the editor: transport, stimulus, the WAV clip with what the renderer says about it, the ADM/room view, the CAD sheet with its three views, gutter sliders, snap grid and orbit path, the level, the orbit's axis, radius and turn time, the programme isolation and the centre button | `/omniphony/control/object_test`, `…/object_test/rotation`, `…/object_test/clip`, `…/speaker_test/idle_feed` |
| Renderer | Output mode, the Renderer/Binaural tab pair, the evaluation mode with its cartesian and polar grids and their step readouts, position interpolation, object size intervals, ramp mode, the backend with its status and its schema-generated parameters, distance diffuse, the distance model, the crossover with what the engine built | `/omniphony/control/output_mode`, `…/binaural_mode`, `…/render_evaluation_mode`, `…/render_evaluation/*`, `…/ramp_mode`, `…/render_backend`, `…/backend/param`, `…/distance_diffuse/*`, `…/distance_model*`, `…/option` |

Mute and solo follow `mute-solo.js`: solo mutes every other entry, soloing the
only unmuted entry lifts the mutes, and the injected test source is skipped by
`control_object_mute` because it is addressed by name rather than by index —
its M button stops the test signal instead, remembering that it was playing so
unmuting resumes.

Two rules from the web are worth naming because they are easy to lose. The
backend parameters are generated from the schema the renderer publishes, so a
new parameter appears without a line of UI code; a translated label wins over
the schema's own, as in `vbap.js`. And every control that forces a gain
recompute arms the same eight-second watchdog: the panel says "computing"
immediately and, if no broadcast comes back, says the engine never answered
rather than lying about being up to date.

The status line is where the web is told its state by its own connection
machinery; this host derives the same four states from what its listener
actually knows, which is the same information one layer down. Three banners sit
under it, and each answers a different question. *No renderer connected* says
how to bring one up, and stands down when there is something more specific to
say. *Decoder bridge not found* is the one that matters most: the renderer is
running, the connection is healthy, and there is no spatial audio — without the
banner that reads as a bug in everything else. *Connected to a renderer this
Studio did not start* is the same shape of problem: the connection looks
perfectly healthy, so it has to be said out loud or every control that renderer
does not implement simply vanishes. The expected binary it compares against is
resolved once at start-up, and a half-known comparison is reported as no answer
rather than as a mismatch.

Two pieces of the web's line are not here yet: the "service" flavour, which
reads a flag refreshed by a host command that shells out to the service manager
and belongs with the service controls themselves, and the `error` state, which
only the auto-start watchdog can distinguish from a renderer that has not come
up yet.

Half of the About box is fixed at build time and half of it is whatever the
renderer last said about itself. That second half is why the box is worth
porting early: it answers "which renderer am I actually driving, and which
configuration is it running on", which is the first question when something
sounds wrong — and a renderer silently running on built-in defaults explains a
whole class of "why does it not sound like the settings say". Two details of the
native box: the version it reports is this crate's own (the Tauri Studio is at
0.5.2, the native host at 0.1.0) because the host command is the ported one, and
it becomes the right number at the cutover; and there is no logo, because the
web's is an SVG and one image is not worth a rasteriser dependency.

Modals get an opaque frame rather than the side panels' 78 %: a dialog painted
at that transparency lets the scene and the controls behind it show through its
own text, and a box that has to be read is the one place the translucency costs
more than it buys.

The channel editor is the speaker editor's mechanic applied to the input side:
each channel of a channel-based stream is either routed straight to its speaker
(LFE → sub) or virtualised as an object at a position, and the whole set is a
speaker layout of its own. Every edit therefore pushes the entire bed — there is
no diff to send — and each channel ships the block matching its own coordinate
mode: forcing polar would replace a cartesian edit with a Studio-side conversion
the renderer does not make, and the channel would land somewhere else. Names are
matched through the renderer's own published alias table, so the editor and the
layout matcher accept exactly the same spellings.

With no stream playing, Studio materialises one scene marker per channel so the
bed stays visible and editable at rest; they stand down as soon as spatial frames
arrive, and the stale live objects — which get no removal message when the engine
simply stops emitting them — are swept at the same time. Two departures from the
web, for the same reason as the injection editor's idle feed: the bed is not
materialised into the renderer's config on the first snapshot that reports it
missing (that would write to a live renderer before the user had touched
anything, so it is the Reset button's job), and the "3D Edit" arming buttons wait
for the viewport gizmos of a later phase.

The injected object is a real source: it is written into the same registry
every other object lives in, so the sphere, the label, the trail, the list row
and the meter are the ones everything else gets, and none of them know it is
invented. The 3D scene shows where the renderer says the source *is* while it
plays; the CAD sheet keeps showing where it was *placed*, because those markers
are the handle being dragged and a handle that runs away from the pointer is
not a handle.

The sheet is one drawing rather than three: a single scale means a unit of room
is the same number of sheet units in every view, so the side view's depth is
visibly the same length as the plan's, and the 45° mitre in the empty corner
carries depth between the two views that show it. The faces follow the room's
true extents and its depth warp, so a marker on a face and the object in the
room are in the same place; the ADM view swaps them for the renderer's unit
cube, where the sampling grid is evenly spaced and a circle is a circle. The
orbit path mirrors the renderer's `position_at`, clamp included — a drawn
circle where the heard one is flattened against a wall would be a picture of
something that is not happening.

One departure from the web, and the reason for it: the renderer's idle feed is
armed while the injection editor is *open*, not merely while the feature is
configured on. The web arms it from the stored preference at boot; a native host
that did the same would send a control message to a live renderer before the
user had touched anything, which this host must never do.

The batched audio configuration goes through the host's own resolver
(`audio_config.rs`, copied verbatim), so the values sent on the wire are the
ones the Tauri host would have sent, clamps and defaults included.

## Model events the panels needed

The phase 1 dispatcher applied what the viewport draws and dropped the rest.
The events the panels read are now applied too: the evaluation-grid sizes (with
the web's "zero means unset" rule, and its exception for the negative-Z size),
the VBAP recompute status and its error, the configuration save feedback, the
object-test clip document, the backend script file listings, the log lines and
the whole adaptive-resampling block.

Meters now feed the peak-hold cursors through the host's own `peak_hold.rs`,
keyed as the host keys them (`master`, `spk:<id>`, `src:<id>`, `ear:<id>`), so
a transient stays readable after it has passed.

## Not covered by this pass

Specifications for all of it were extracted from the web sources first and are
in [`studio-native-ui-specs/`](studio-native-ui-specs/).

- **Audio panel**: the diagnostics block, the per-stage timing readouts, the
  resample plot, and the sample-rate preset menu (the native select offers the
  presets but not a free-text rate).
- **Renderer panel**: the hybrid backend's own controls, the file-parameter
  Browse and Edit buttons, the performance gauges, the info modals, and the
  SOFA browser the binaural tab's file source needs.
- **Speakers**: layout import, export and presets, the position thumbnail and
  filter glyph of a list row, the band contribution bars, drag-to-reorder, the
  headphone channel rows and their ear mute.
- **Left overlay**: updates (the web fetches GitHub from the webview; a native
  host needs its own HTTP client) and the dead rows of the audio input panel
  (backend, imported layout, channel count, sample rate, map, LFE mode) which
  belong to the legacy PCM mode.
- **Elsewhere**: the save footer, the scene-effects bar, the band cursor, the
  modals, the gradient editor, the plots, the code editor, the SOFA browser,
  the auto-tune wizard, mpv overlay mirroring.
- **Host services** the native app does not have yet: the local-renderer
  auto-start watchdog, the OS-service controls, the timing statistics, the
  derived master meter, and the eight locales.

## Running

```bash
cd omniphony-studio-egui
cargo run --release -- --synthetic 24 --rate 100
cargo run --release -- --register 127.0.0.1:9000
```

Nothing is sent to the renderer until a control is used. `--register` alone
still only registers, keeps the heartbeat and subscribes to the gain tables the
enabled volumes need.
