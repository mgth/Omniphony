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

| Tier | Modules | Owns | Must not |
|---|---|---|---|
| **Core** | `model/`, `osc/`, `host/`, `auto_tune/`, `i18n.rs`, `stats.rs` | the renderer protocol, the application and session state, everything that happens over time, all I/O | name egui, eframe, wgpu, winit or rfd; import `app`, `panels`, `ui`, `view` or `render` |
| **UI** | `app.rs`, `main.rs`, `panels/`, `ui/`, `view/`, `render/` | drawing, input, view state (camera, selection, tabs, text being typed) | spell an OSC address, send raw control messages, write the model, do I/O, run periodic behaviour |

The UI depends on the core, never the reverse. `view/` and `render/` sit in the
UI tier but are toolkit-neutral by design — wgpu only, with egui confined to
the `ViewportCallback` adapter and a few `Pos2`/`Color32` in `view/` that the
plan removes. Keep them that way.

## Rules

Each machine-checked rule has the id the ratchet reports it under.

| Rule | Tier | What it forbids |
|---|---|---|
| `osc-address` | UI | a string literal starting with `/omniphony/` |
| `raw-send` | UI | `ctl.send*(…)`, `control.send(…)`, `send_control(…)`, `send_json_control(…)` |
| `model-write` | UI | writing the model or host state: `live.app.x = …`, `live.app.sources.insert(…)`, `live.push_log(…)`, `….lock().unwrap().field = …` |
| `model-impl` | UI | `impl AppState` or `impl Live` |
| `side-effect` | UI | spawning a thread or process, `thread::sleep`, file or network I/O (`fs::…`, `UdpSocket`, `to_socket_addrs`, `ureq`) |
| `frame-tick` | UI | defining a `maintain_*` function |
| `toolkit-in-core` | Core | naming `egui`, `eframe`, `egui_wgpu`, `epaint`, `emath`, `ecolor`, `wgpu`, `winit` or `rfd` |
| `core-imports-ui` | Core | `crate::app`, `crate::panels`, `crate::ui`, `crate::view`, `crate::render`, `crate::Args` |

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

1. In `src/host/commands/<area>.rs`, write — or first look for — a typed
   function: `pub fn set_head_radius(state: &SharedState, metres: f32)`. It
   clamps, applies the optimistic value to the model through `state.inner`,
   and sends with `send_control`. A realtime control stamps
   `state.realtime_seq`, the counter panels share through
   `StudioSpike::next_realtime_seq`. About 150 such functions were ported from
   the Tauri host and are still unused: check that the one you reuse still
   sends what the panel sends today.
2. The panel calls it: `crate::host::commands::binaural::set_head_radius(&self.host, v)`.

The address literal then lives only in the core. Once the renderer's
`osc_contract` is shared (plan, phase 2) the core names it instead of spelling
it.

### Something that happens over time

A service in `src/host/` with explicit state and
`fn tick(&mut self, now: Instant) -> Option<Instant>` returning its next
deadline. The UI declares what it wants (`set_idle_feed_wanted(true)` when the
pane opens) rather than the service reading UI state. Until the core has its own
clock (plan, phase 3), `app.rs` calls the tick from `App::logic` and hands the
returned deadline to `request_repaint_after`. That driver is a stopgap: on
Wayland eframe runs no pass at all while the window is minimised, `logic`
included.

### I/O, threads and processes

In a host service. Anything that can block — DNS, HTTP, spawning or waiting on
a process, large files — runs off the UI thread and reports back through state
the UI reads, plus a repaint request.

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
- **a file sits outside every tier.** Add its module to `TIERS` in the test, on
  the side of the boundary it belongs to.

Two PRs that each lower the same line conflict on the baseline. Regenerate
after the rebase; do not merge the two numbers by hand.

A rule is deleted from the test once the compiler enforces it — the core in its
own crate makes `toolkit-in-core` and `core-imports-ui` moot; a read-only model
handle does the same for `model-write`. `osc-address` and `raw-send` stay as
cheap tripwires.
