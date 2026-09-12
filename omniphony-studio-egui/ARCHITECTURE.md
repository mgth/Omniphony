# Native Studio architecture: the UI boundary

The native Studio replaced the web frontend by rewriting the UI and keeping the
Rust host almost verbatim. That was possible because Tauri enforced a boundary:
the JavaScript UI reached the renderer only through the host's typed commands
(156 `invoke()` calls to 135 commands) and did not know a single OSC address.

This crate has to stay as replaceable. egui is a choice, not a foundation: the
day another toolkit serves the Studio better, moving should mean rewriting the
drawing, not re-porting the behaviour. The port lost the boundary on the way in
— panels send raw OSC, write the model and run timers from draw code — and
three of its regressions came from exactly that (#447). This document states
the boundary; `tests/architecture.rs` enforces it; the refactor that restores
it is planned in [`docs/studio-egui-boundary-plan.md`](../docs/studio-egui-boundary-plan.md).

## Two tiers

| Tier | Where | Owns | Must not |
|---|---|---|---|
| **Core** | `core/`, the crate `omniphony-studio-core`: `model/`, `osc/`, `host/`, `auto_tune/`, `i18n.rs`, `stats.rs` | the renderer protocol, the application and session state, everything that happens over time, all I/O | depend on any UI crate |
| **UI** | this crate: `app.rs`, `main.rs`, `panels/`, `prefs/`, `ui/`, `view/`, `render/` | drawing, input, view state (camera, selection, tabs, text being typed), native dialogs | spell an OSC address, send raw control messages, write the model, do I/O, run periodic behaviour |

The UI depends on the core, never the reverse. The app binds the core's
modules at its root (`use omniphony_studio_core::{model, osc, …}` in
`main.rs`), so UI code still names them `crate::model`, `crate::osc`, … .

`view/` and `render/` sit in the UI tier but are toolkit-neutral by design —
wgpu only, with egui confined to the `ViewportCallback` adapter and a few
`Pos2`/`Color32` in `view/` that the plan removes. Keep them that way.

## Rules

The core's side is held by the toolchain:

- **The core cannot import the UI.** It is a separate crate the app depends
  on, so there is no path back.
- **No UI crate in the core's graph.** A CI step fails when egui, eframe,
  wgpu, winit, accesskit or rfd appears in `cargo tree -p omniphony-studio-core`.
- **The UI cannot extend the model.** Rust's orphan rule rejects an
  `impl AppState` or `impl Live` outside the core crate. Build the view's own
  type instead (`LatencyView::of(&AppState)`).

The UI's side is held by the ratchet, under these rule ids:

| Rule | What it forbids in the UI crate |
|---|---|
| `osc-address` | a string literal starting with `/omniphony/` |
| `raw-send` | `ctl.send*(…)`, `control.send(…)`, `send_control(…)`, `send_json_control(…)` |
| `model-write` | writing the model or host state: `live.app.x = …`, `live.app.sources.insert(…)`, `live.push_log(…)`, `….lock().unwrap().field = …` |
| `side-effect` | spawning a thread or process, `thread::sleep`, file or network I/O (`fs::…`, `UdpSocket`, `to_socket_addrs`, `ureq`) |
| `frame-tick` | defining a `maintain_*` function |

Two exemptions are written into the test with their reason: `main.rs` reads the
CJK font and `render/head.rs` reads the head mesh, once, before the first
frame. Native file *dialogs* are UI and allowed; reading or writing the file
the user picked is I/O and belongs to the core.

Not machine-checked, same rule:

- **Draw code never checks a deadline.** A deadline belongs to a core service
  that returns it, and something must wake for it. A check placed in a section's
  draw function runs only while that section is on screen: the speaker-test
  safety stop did exactly that, and kept a test tone playing behind a
  collapsed panel.
- **The model holds codes, not translated text.** Translation happens when
  drawing.
- **State another toolkit would need lives in fields**, not in egui's memory
  (`ctx.data`, `ctx.memory`): which section is open, which help card is shown,
  a panel's measured rect.

## Recipes

### A new control

1. In `core/src/host/commands/<area>.rs`, write — or first look for — a typed
   function: `pub fn set_head_radius(state: &SharedState, metres: f32)`. It
   clamps, applies the optimistic value to the model through `state.inner`,
   and sends with `send_control`. Apply what it *sent*, not what it was
   given: a value the command rejected and the panel applied anyway reads as
   accepted. A realtime control stamps `state.realtime_seq`, the counter panels
   share through `StudioSpike::next_realtime_seq`. Some of the functions ported
   from the Tauri host are still unused: check that the one you reuse still
   sends what the panel sends today.
2. The panel calls it: `crate::host::commands::binaural::set_head_radius(&self.host, v)`.

The address literal then lives only in the core. Once the renderer's
`osc_contract` is shared (plan, phase 2) the core names it instead of spelling
it.

### Something that happens over time

A service in `core/src/host/services/` with explicit state and
`fn tick(&mut self, state: &SharedState, now: Instant) -> Tick`, returning
whether the model changed and when it is next due. Add it to `Services` and it
runs on the core's own clock thread, which sleeps until the earliest deadline
or until something nudges it. The UI declares what it wants
(`set_idle_feed_wanted(true)` when the pane opens, `set_overlay_prefs` every
draw) rather than the service reading UI state.

Take `now` rather than reading the clock, so the test can drive time. Never
check a deadline from draw code: on Wayland eframe runs no pass at all while
the window is minimised, so a timer hung off a frame stops with the frames.

### I/O, threads and processes

In a host service. Anything that can block — DNS, HTTP, spawning or waiting on
a process, large files — runs off the UI thread and reports back through state
the UI reads, plus a repaint request. `services::jobs::run` is the general
form: it takes a closure, runs it on a named thread, and hands back a
`Receiver` the UI polls.

### New state

Application and session state (what a test is doing, what was last sent, when
something expires) belongs to the core. View state (camera, selection, the open
tab, the text being typed) belongs to the UI, in `StudioSpike` or in a
per-panel struct.

## The ratchet

`tests/architecture.rs` scans `src/` lexically — comments skipped, string
literals looked into only by `osc-address` — and counts each rule's violations
per file. `tests/architecture-baseline.txt` holds the counts the port started
with. `cargo test` fails when:

- **a count grew.** Move the code as the recipes above say. Raising the
  baseline (`UPDATE_ARCHITECTURE_BASELINE=allow-increase`) is the maintainer's
  decision, made explicitly, never a way to get a change through;
- **a count shrank.** Record the progress in the same PR:

  ```bash
  UPDATE_ARCHITECTURE_BASELINE=1 cargo test --test architecture
  ```

  This mode rewrites the file only when nothing grew;
- **a file sits in no declared module.** Declare the module in `MODULES` in
  the test if it draws; if it holds protocol, state or behaviour, it belongs in
  `core/`.

Two PRs that each lower the same line conflict on the baseline. Regenerate
after the rebase; do not merge the two numbers by hand.

A rule is deleted from the test once the compiler enforces it. The crate split
retired three that way: `toolkit-in-core`, `core-imports-ui` and `model-impl`.
A read-only model handle will do the same for `model-write`. A rule at zero is
not deleted: `osc-address`, `raw-send`, `side-effect` and `frame-tick` all
stand at zero and stay as cheap tripwires.
