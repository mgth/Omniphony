# omniphony-studio-egui: rules for coding agents

This crate must stay replaceable by another UI toolkit, the way the web
frontend was replaced by egui. Read `ARCHITECTURE.md` before changing it. The
short version:

- **Panels draw, and nothing else.** UI code (`app.rs`, `main.rs`, `panels/`,
  `ui/`, `view/`, `render/`) never writes an `/omniphony/…` address, never calls
  `ctl.send*` or `control.send`, never assigns into `live.app` or host state,
  never spawns a thread or process, sleeps, or does file or network I/O, and
  never checks a deadline inside draw code.
- **A new control** is a typed function in `src/host/commands/` that clamps,
  applies the optimistic value and sends. The panel calls it with `&self.host`.
  Look for an existing one first: about 150 were ported from the Tauri host and
  are unused.
- **Periodic behaviour** is a host service with
  `tick(now) -> Option<Instant>`. It is never a `maintain_*` function in
  `panels/`.
- **The core** (`model/`, `osc/`, `host/`, `auto_tune/`, `i18n.rs`, `stats.rs`)
  never names egui, eframe, wgpu, winit or rfd, and never imports `app`,
  `panels`, `ui`, `view` or `render`.

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
