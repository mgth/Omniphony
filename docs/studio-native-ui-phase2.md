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

Mute and solo follow `mute-solo.js`: solo mutes every other entry, soloing the
only unmuted entry lifts the mutes, and the injected test source is skipped
because it is addressed by name rather than by index.

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

Specifications for all of it were extracted from the web sources first and live
in the session scratchpad (`spec_panels_right.md`, `spec_design.md`,
`spec_host_contract.md`).

- **Audio panel**: output backend and device, the staged-config apply flow, the
  file/pipe output, latency controls and their readouts, adaptive resampling,
  the diagnostics block, the timing readouts.
- **Renderer panel**: the tab pair, backend selection and its schema-generated
  parameters, the evaluation mode and its resolutions, distance model and
  diffuse, the hybrid backend, ramp mode, crossover, HRTF and head tracking.
- **Speaker editor**: the coordinate tables, gain, delay, band limits,
  spatialise flag, reordering, the test tab, layout import and export.
- **Left overlay**: profiles, updates, the audio input panel, the
  fixed-channel-sources section with its schema-declared options, room
  geometry, DRC, the channel and object-test editors.
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
