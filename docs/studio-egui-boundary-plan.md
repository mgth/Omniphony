# RFC: restoring the native Studio's UI boundary

Status: accepted, 2026-09-11.

Progress:

- **Phase 0 landed**: the architecture ratchet, `ARCHITECTURE.md` and the
  agent rules.
- **Phase 1 landed**: `omniphony-studio-core` exists and has no UI crate in
  its graph. What changed from the plan below is recorded under the phase.

The rules this plan converges on are stated in
[`omniphony-studio-egui/ARCHITECTURE.md`](../omniphony-studio-egui/ARCHITECTURE.md).
This document is the path there.

## Why

Moving the Studio from the web frontend to egui worked because Tauri kept the
UI behind a boundary. The JavaScript UI called 135 typed host commands and knew
no OSC address, so the whole Rust host carried over and only the drawing was
rewritten.

The egui port kept the host's code but not the boundary. An audit of `dc269f67`
found:

- **Clean already.** The core (`model`, `osc`, `host`, `auto_tune`, about 15k
  lines) names exactly one egui type: the `egui::Context` the OSC listener
  wakes the UI with. The 3D engine (`render`, `view`, about 6k lines) is plain
  wgpu behind a 50-line `egui_wgpu::CallbackTrait` adapter.
- **No boundary between application and UI.** `StudioSpike` has 109 fields and
  is at once the core, the controller and the view. Every panel is an
  `impl StudioSpike` block. In `panels/`, 145 methods draw nothing, against 168
  that do. The panels hold 129 `/omniphony/` literals and 131 raw sends, and
  write the model in 76 places. None of the 149 typed commands ported from the
  Tauri host is called.
- **The frame loop is the application's clock.** `App::ui` runs ten controller
  ticks after drawing, and more deadlines are checked inside draw functions.
  The fixes in #447 (meter decay, speaker-test safety stop, orender path) are
  all logic that was re-ported into UI code and broke on the way.

A toolkit swap today would rewrite about 18k lines, 5k of them behaviour. After
this plan, it rewrites about 13k lines of presentation and re-ports nothing.

## Target

### Crates

```
omniphony-renderer/omniphony-osc-contract   addresses only, no dependencies
        ▲                        ▲
runtime_control (re-exports)   omniphony-studio-core   model, protocol, commands,
                                        ▲               services, config: no UI crate
                                        │
                               omniphony-studio-egui    app, panels, ui, view, render
```

`omniphony-studio-core` lives in `omniphony-studio-egui/core/`, in a workspace
with the egui crate. Sitting there, it shares the lock file, the toolchain pin
and the CI cache. The Tauri host can still depend on it by path if it outlives
the cutover (phase 5).

Splitting `render` and `view` into an `omniphony-studio-scene` crate is
optional and only worth it when a migration is actually on the table.

### The API the UI sees

```rust
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub struct Studio { /* the Live model, the control channel, the services, paths */ }

impl Studio {
    pub fn start(config: StudioConfig, waker: Waker) -> io::Result<Studio>;

    /// Read access for drawing: `Deref<Target = Live>`, no `DerefMut`.
    pub fn read(&self) -> LiveRead<'_>;

    // One method per user action. Each clamps, applies the optimistic value,
    // stamps the realtime sequence where the protocol wants one, and sends.
    pub fn set_master_gain(&self, db: f32);
    pub fn move_speaker(&self, from: usize, to: usize);
    // …

    // What the UI wants, not how the core gets it: the core owns the timers.
    pub fn set_idle_feed_wanted(&self, client: FeedClient, wanted: bool);
    pub fn set_gain_tables_wanted(&self, targets: &[i64]);
    pub fn set_diagnostics_wanted(&self, wanted: bool);
}
```

These are typed methods rather than a message enum: they map one-to-one onto
the messages of an Elm-style toolkit (iced, Slint callbacks), and they are what
the panels already mean when they write `ctl.send(...)`. `lists.rs` shows the
view side of it, with `Row` in and `RowAction` out.

### Where behaviour runs

A scheduler lives in the core, on the OSC listener thread, which already wakes
every 100 ms. It waits on `recv_timeout(min(100 ms, next_deadline))`, runs
`Services::tick(now) -> Option<Instant>`, and calls the waker when the state
changed. Every service takes `now` as an argument, so its tests drive time
directly. Nothing depends on a frame being drawn any more, and minimising the
window no longer stops the watchdog: on Wayland eframe runs no pass at all
while minimised, `App::logic` included.

## Phases

The work lands as small PRs. Each one is green on CI, changes no visible
behaviour unless it says so, and lowers the ratchet baseline in the same commit.

### Phase 0: lock the practice (landed)

- `tests/architecture.rs`: the per-file ratchet over eight rules, with the
  baseline in `tests/architecture-baseline.txt`.
- `ARCHITECTURE.md`: the tiers, the rules and the recipes. The crate's
  `CLAUDE.md` (and the `AGENTS.md` link to it) carries the short version for
  coding agents.
- A single realtime sequence counter: panels now stamp with the host's, so a
  control moved into `host::commands` cannot interleave a second sequence.

From here the debt can only shrink.

### Phase 1: the compiler holds the core boundary (landed)

1. **Waker.** `osc::spawn_listener` and the SOFA workers take a `Waker`
   instead of `egui::Context`. Also add the missing trailing repaint: a change
   that lands inside the 2.5 ms coalescing window is flushed on the next
   receive timeout.
2. **Dependencies pointed the right way.**
   - `host/prefs.rs` and `host/display_prefs.rs` persist UI preferences and move
     to the UI side. The core keeps a generic `json_store::{load, save}`.
   - `Live` moves from `osc/dispatch.rs` to `model/`.
   - The state types `StudioSpike` borrows from `panels/` move to the tier that
     owns them: `ChannelCatalog` and `PendingMove` to the core, `GizmoDrag` to
     the UI.
   - The `impl AppState` in `panels/latency.rs` moves to `model/`.
3. **`omniphony-studio-core` crate.** It takes model, osc, host, auto_tune,
   i18n and `ProcStats`, and drops `#![allow(dead_code)]` so the compiler
   lists what nothing calls. Three blockers go first:
   - `rfd` in `host/commands/layout_io.rs`: the `pick_*` functions move up to
     the UI;
   - the `CARGO_MANIFEST_DIR` lookups (already reworked in #447);
   - `include_str!` into the web tree: one directory deeper from the new
     crate, and it keeps working until phase 5.

Done when a CI step checks that `cargo tree -p omniphony-studio-core -e normal`
contains no egui, eframe, wgpu, winit or rfd. The `toolkit-in-core` and
`core-imports-ui` rules are then deleted from the ratchet, because the compiler
enforces them.

The file move in step 3 conflicts with every open Studio PR. Land it when few
are open, and land it fast; git follows the renames on rebase.

**As landed:**

- The core crate lives inside the app's directory (`core/`), in a workspace,
  rather than at the repository root.
- The app binds the core's modules at its root, so `crate::model::…` and the
  like still resolve and no panel changed.
- `impl AppState` in `panels/latency.rs` became `LatencyView::of(&AppState)`.
  The orphan rule now forbids the pattern, so `model-impl` was retired with the
  two core rules.
- The file dialogs moved to `ui::file_dialogs`. Their start directory, their
  memory and their default names stayed in the core.
- The SOFA workers still hold the egui context: they are UI-side threads
  today, and move to a core service with the other side effects in phase 3.
- Deferred to phase 2: moving `Live` into `model/` (a question internal to the
  core, not a boundary one) and the state types `StudioSpike` borrows from
  `panels/` (they move with the logic that owns them).
- Dropping `#![allow(dead_code)]` does not list the unused commands: in a
  library, a public item is never dead. Phase 2 deletes the unused ones by
  hand.
- The CI step also rejects `accesskit`. It was tested both ways, by adding
  `egui` to the core's manifest.

### Phase 2: an action API (about 1 to 1.5 weeks)

1. **Shared contract.** Extract `runtime_control::osc_contract` into
   `omniphony-osc-contract`, a crate with no dependencies that `runtime_control`
   re-exports. It covers 84 of the 98 addresses the panels send; add the
   missing ones and the address families. The renderer workspace rebuilds once.
2. **`Studio` handle.**
   - Built from `host/commands`, which already carries the clamps, the JSON
     documents and the sequence numbers.
   - Commands a panel needs are revived and checked against what the panel
     sends today; the rest are deleted.
   - `Ctl::send*` becomes private to the core.
3. **Port the panels**, one or two files per PR. Replace each
   `live.app.x = …; ctl.send("/omniphony/…", …)` pair with `studio.set_x(…)`.
   Order by debt: `renderer`, `binaural`, `speaker_editor`, `audio_input`,
   `audio_output`, `mpv_overlay`, `profiles`, `room`, `hybrid`, then the rest.
   For each PR, compare the OSC the panel emits before and after with the
   renderer log at debug level, exercising every control.
4. **Read-only model.** `Studio::read()` returns a guard without `DerefMut`,
   and the UI crate loses every path to `&mut Live`. The listener holds the
   only writer.

Done when `osc-address`, `raw-send` and `model-write` are at zero.
`model-write` is then deleted, since the type system forbids it. `osc-address`
and `raw-send` stay as tripwires.

### Phase 3: a clock for the core (about 1 week)

1. **Scheduler.** `Services` runs on the listener thread as described under
   Target, with an injected-time test per service.
2. **Move the ticks**, each with the input the UI now declares instead of the
   service reading it:

| Tick today | Becomes | UI input |
|---|---|---|
| `maintain_meters` | meter ballistics in the model, computed on read | none |
| speaker-test safety stop | `SpeakerTest` service with its own deadline | start and stop commands |
| `maintain_test_idle_feed` | idle-feed service, reference-counted | `set_idle_feed_wanted` |
| `maintain_gaintable_subscriptions` | subscription service with the 5 s repair heartbeat | `set_gain_tables_wanted` |
| `maintain_renderer_watchdog`, `service_status` | watchdog service, `systemctl` off the UI thread | none |
| `check_recompute_ack` | 8 s deadline inside the recompute command | none |
| `maintain_mpv_overlay` | overlay sync service, deduplicating | `set_overlay_prefs` |
| `maintain_object_test_source`, `sync_virtual_bed_objects` | synthetic-source service | object-test commands |
| `maintain_diag_publication` | diagnostics subscription | `set_diagnostics_wanted` |
| `maintain_auto_tune` | the auto-tune runner, on the scheduler | wizard commands |

3. **Move the side effects**: the update check (thread and HTTP), the SOFA jobs,
   the DNS resolution in `connect()`, the blocking stop loop in
   `stop_launched_renderer`, and the `mpv.conf` read and write.

Done when `frame-tick` and `side-effect` are at zero. `App::ui` then only draws,
and `App::logic` only handles UI concerns such as the preferences debounce.

### Phase 4: a neutral 3D engine (about 2 days, can wait for a real migration)

- `view/` uses `glam::Vec2`, `[u8; 4]` (unmultiplied) and a `ScreenRect`, and
  `BandBar::paint` moves into the app.
- `SceneRenderer::new(device, format, samples, srgb_target)` with public
  `prepare` and `paint`. `ViewportCallback` becomes a roughly 20-line adapter in
  the app. The `srgb_target` flag is what lets a host with an sRGB surface
  (winit, iced, Bevy) avoid encoding twice.
- Optionally, extract `omniphony-studio-scene`.

### Phase 5: the Tauri Studio

The Tauri host still receives features, and about 10.6k lines are copied
between it and the egui crate. Decide at the cutover:

- **Retire it.** Freeze it, move the i18n catalogues and the head model into
  the core's assets, then delete it.
- **Keep it.** Make `src-tauri` depend on `omniphony-studio-core` and delete its
  copies.

## Definition of done

- `omniphony-studio-core` builds and tests without any UI crate in its graph,
  and CI checks it.
- The UI crate has no way to write the model or send a raw message: the API
  offers neither.
- No behaviour advances because a frame was drawn.
- Swapping egui means rewriting `omniphony-studio-egui` apart from `render/` and
  `view/`: about 13k lines of presentation, and no protocol, timers or state
  machines.

## Estimate

| Phase | Work | Size |
|---|---|---|
| 0 | guardrails | this PR |
| 1 | waker, dependencies, core crate | landed |
| 2 | contract crate, `Studio` API, about 25 panel PRs, read-only model | 5 to 8 days |
| 3 | scheduler, 10 ticks, side effects | 4 to 6 days |
| 4 | neutral 3D engine | about 2 days, deferrable |
| 5 | Tauri decision | a decision |

About three weeks of small, independent PRs. Phases 1 to 3 carry the value.
Phase 4 is cheap insurance, and can wait until a migration is actually on the
table.

## Risks

- **Drift while porting panels.** Mitigated by one-file PRs, the before/after
  OSC comparison, and the typed commands keeping the Tauri host's clamps.
- **Conflicts with feature work.** The Studio changes daily. Phase 1 step 3 is
  the only mass move. The panel ports are local, and the ratchet stops new debt
  from arriving in the meantime.
- **Lock contention.** `build_frame` already runs under the `Live` lock and
  blocks the listener. The read guard keeps that behaviour. A snapshot or
  double buffer is a separate change, not part of this plan.
