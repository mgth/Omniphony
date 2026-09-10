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
| Audio input | The status line, the mode, the bridge path, the pipe or the PipeWire node, description and clock, and Apply with its two paths | `/omniphony/control/render/bridge_path`, `…/render/input_pipe`, `…/config/input` and its apply, `…/input/live/clock_mode`, `…/save_config`, `…/reload_config` |
| Fixed-channel sources | Stream state, rear-channel placement, the synthetic-objects switch, the height generator and the phantom extractor with the parameters each declares, and why each stage is or is not running | `/omniphony/control/option`, `…/object_generator/param`, `…/phantom_extract/param` |
| Room geometry | The five metre dimensions, the derived scale, and the front/rear blend when the two depths differ | `/omniphony/control/config/layout` (`radiusM`), `…/room_ratio`, `…/room_ratio_rear`, `…/room_ratio_lower`, `…/room_ratio_center_blend` |
| DRC and loudness | The compression mode and weight, the loudness switch with its three readouts, and the gain gauge while metering is on | `/omniphony/control/input/drc_mode`, `…/input/drc_weight`, `…/loudness` |
| Speaker editor | Reorder, delete, name, the cartesian and polar coordinate tables in normalised units and metres, gain, delay, spatialise, band limits, and the Test tab with its trigger, isolation, level and idle feed | `/omniphony/control/config/layout` (`speakerEdits`, `moveSpeaker`, `removeSpeaker`) and its apply, `…/config/speakers` for the delay, `…/realtime/speaker_gain`, `…/speaker_test`, `…/speaker_test/idle_feed` |
| Renderer | Output mode, the Renderer/Binaural tab pair, the evaluation mode with its cartesian and polar grids and their step readouts, position interpolation, object size intervals, ramp mode, the backend with its status and its schema-generated parameters, distance diffuse, the distance model, the crossover with what the engine built | `/omniphony/control/output_mode`, `…/binaural_mode`, `…/render_evaluation_mode`, `…/render_evaluation/*`, `…/ramp_mode`, `…/render_backend`, `…/backend/param`, `…/distance_diffuse/*`, `…/distance_model*`, `…/option` |

Mute and solo follow `mute-solo.js`: solo mutes every other entry, soloing the
only unmuted entry lifts the mutes, and the injected test source is skipped
because it is addressed by name rather than by index.

Two rules from the web are worth naming because they are easy to lose. The
backend parameters are generated from the schema the renderer publishes, so a
new parameter appears without a line of UI code; a translated label wins over
the schema's own, as in `vbap.js`. And every control that forces a gain
recompute arms the same eight-second watchdog: the panel says "computing"
immediately and, if no broadcast comes back, says the engine never answered
rather than lying about being up to date.

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

- **Audio panel**: latency controls and their readouts, adaptive resampling,
  the diagnostics block, the timing readouts, the sample-rate preset menu (the
  native select offers the presets but not a free-text rate).
- **Renderer panel**: the binaural tab in full (HRTF, distance, listening
  room, head tracking), the hybrid backend's own controls, the file-parameter
  Browse and Edit buttons, the performance gauges, the info modals.
- **Speakers**: layout import, export and presets, the position thumbnail and
  filter glyph of a list row, the band contribution bars, drag-to-reorder, the
  headphone channel rows and their ear mute.
- **Left overlay**: profiles, updates, the channel editor and the object-test
  editor, and the dead rows of the audio input panel (backend, imported
  layout, channel count, sample rate, map, LFE mode) which belong to the
  legacy PCM mode.
- **Elsewhere**: the save footer, the scene-effects bar, the band cursor, the
  modals, the gradient editor, the plots, the code editor, the SOFA browser,
  the auto-tune wizard, mpv overlay mirroring.
- **Host services** the native app does not have yet: the local-renderer
  auto-start watchdog, foreign-renderer detection, the bridge-error banner, the
  timing statistics, the derived master meter, and the eight locales.

## Running

```bash
cd omniphony-studio-egui
cargo run --release -- --synthetic 24 --rate 100
cargo run --release -- --register 127.0.0.1:9000
```

Nothing is sent to the renderer until a control is used. `--register` alone
still only registers, keeps the heartbeat and subscribes to the gain tables the
enabled volumes need.
