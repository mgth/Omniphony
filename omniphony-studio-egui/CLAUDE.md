# omniphony-studio-egui: rules for coding agents

This crate must stay replaceable by another UI toolkit, the way the web
frontend was replaced by egui. Read `ARCHITECTURE.md` before changing it. The
short version:

- **Panels draw, and nothing else.** UI code (this crate: `app.rs`, `main.rs`,
  `panels/`, `prefs/`, `ui/`) never writes an
  `/omniphony/…` address, never calls `ctl.send*` or `control.send`, never
  assigns into the model or host state — `SharedState::read()` gives a
  read-only handle and there is no other way in — never spawns a thread or
  process, sleeps, or does file or network I/O, and never checks a deadline
  inside draw code.
- **A new control** is a typed function in `core/src/host/commands/` that clamps,
  applies the optimistic value and sends. The panel calls it with `&self.host`.
  Look for an existing one first: about 150 were ported from the Tauri host and
  are unused.
- **Periodic behaviour** is a service in `core/src/host/` with
  `tick(now) -> Option<Instant>`. It is never a `maintain_*` function in
  `panels/`.
- **The core** is the crate `core/` (`omniphony-studio-core`). Never add a UI
  crate to its `Cargo.toml`: egui, eframe, wgpu, winit, accesskit and rfd are
  all out, and CI checks.
- **The 3D scene** is the crate `scene/` (`omniphony-studio-scene`): `view/`
  and `render/`. wgpu belongs there; a UI toolkit does not, and CI checks that
  too. Its adapter to egui is `src/ui/scene.rs`. If the core needs something from the UI, take a
  neutral callback, as `osc::Waker` does.

`tests/architecture.rs` enforces this with a per-file ratchet, and CI runs it.

- **The test fails because you added a violation:** move the code. Do not raise
  the baseline. `UPDATE_ARCHITECTURE_BASELINE=allow-increase` is only used when
  the maintainer has explicitly asked for it in the conversation.
- **The test fails because you removed one:** run
  `UPDATE_ARCHITECTURE_BASELINE=1 cargo test --test architecture` and commit the
  lowered `tests/architecture-baseline.txt` with your change.

The refactor that restores the boundary is planned in
`../docs/studio-egui-boundary-plan.md`. When you touch a file that plan covers,
leave it closer to the target, not further away.
