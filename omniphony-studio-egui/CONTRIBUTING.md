# Contributing to the native Studio

Start with a small change in one panel. The renderer, core services and scene
are separate from the UI so a control should not require understanding the
whole application. Read [ARCHITECTURE.md](ARCHITECTURE.md) for the ownership
rules and [PANELS.md](PANELS.md) for layout conventions. The current gaps and
acceptance evidence live in the [completion plan](../docs/studio-native-completion.md);
the phase reports describe earlier checkpoints.

## First build

Run native Cargo commands from this directory: its `rust-toolchain.toml` pins
the compiler and differs from the renderer's workspace. Install the requested
Rust toolchain through rustup. On Linux the windowing dependencies include
`pkg-config`, `libxkbcommon` and Wayland development files (package names vary
by distribution). The authoritative build environment is in
[ci.yml](../.github/workflows/ci.yml). Native Studio tests do not require an
audio device or a running renderer; loopback sockets must be permitted.

```sh
cd omniphony-studio-egui
cargo build --workspace --locked
cargo test --workspace --locked
cargo fmt --all --check
```

After dependencies are cached, `--offline` makes these checks independent of
the network. Keep all three workspace lockfiles in sync when a shared core
manifest changes: native Studio, renderer, and `omniphony-studio/src-tauri`.
A dependency used only by the UI belongs in the native frontend manifest.

For an isolated visualization session on Linux/macOS:

```sh
studio_trial_dir=$(mktemp -d)
OMNIPHONY_CONFIG_DIR="$studio_trial_dir" cargo run --release --locked -- \
  --synthetic 24 --rate 100 --stats-interval 1
```

Synthetic mode uses an ephemeral listener and suppresses renderer autostart.
The temporary config avoids changing the usual Studio preferences. For live
work, use an explicit `--register host:port`; live controls can modify that
renderer. Do not use an installed renderer or service manager in automated
tests. Native dialogs may appear on the UI thread; reading/writing their chosen
files belongs in a core job.

## Where a change belongs

| Change | Start here | Rule |
|---|---|---|
| Labels and translations | `../omniphony-studio/src/i18n/` | Reuse catalogue keys and shared formatting |
| Row or control appearance | `src/ui/widgets.rs`, `PANELS.md` | Accessible label and keyboard state; stable bounds |
| Panel draft, tab or confirmation | `src/panels/` | Explicit per-panel fields; preserve edits across echoes |
| Command validation or protocol | `core/src/host/commands/`, `core/src/osc/` | Typed intent, one authoritative rule, no UI dependency |
| Deadline, subscription or worker | `core/src/host/services/` | Core-owned lifetime and wakeups; never depend on frames |
| Scene geometry and rendering | `scene/src/view/`, `scene/src/render/` | No toolkit; keep costly work outside model locks |
| Saved UI settings | `src/prefs/`, `core/src/host/json_store.rs` | Types in UI, I/O in core; explicit schema migration |

A panel should read a small snapshot, draw its own view state, and emit a typed
intent. The profile panel is the extraction example: `ProfilePanel::show` can
run with a snapshot and input events, without constructing Studio or a GPU.
Its small application adapter passes the action to the core, which validates
it against the latest renderer state. This matters for a confirmation left
open while the active profile changes.

## Add a control

1. Find the existing core command before adding one. The command owns units,
   clamping, capability checks, model changes and OSC addresses. For a new
   protocol address, add it to the shared OSC contract and test both endpoints.
2. Use the shared row/switch/slider helpers. Pass the translated accessible
   name; drawing a separate label does not name a custom control automatically.
   A stepped slider must not turn a rounded renderer echo into another command.
3. Keep editable strings in `TextDraft` or the panel's explicit draft state.
   Define when Enter, blur, Escape, entity selection and panel closure commit
   or abandon the draft. Never reconstruct the draft from the model each frame.
4. Add a focused regression test that fails for the original bug. Drive
   multiple input frames for typing/focus; drive injected time for deadlines;
   inspect emitted commands for no-op echoes, stale results and rejected input.
5. Check a narrow panel and an expanded section. Outer overlays must keep their
   extent; scroll long content internally so the scene viewport never jumps.

Do not place meaningful view state in `ctx.data` as a shortcut. Existing uses
are migration work, not examples to copy. Do not add I/O or timers through a
core helper called synchronously from draw code: changing the location of a
blocking function does not remove the blocking call.

## Choose the smallest useful test

```sh
cargo test -p omniphony-studio-core --locked host::commands::profiles
cargo test -p omniphony-studio-egui --locked panels::profiles
cargo test --test architecture --locked
```

Core tests should use temporary files, ephemeral loopback ports and fake
responses. Test late/duplicate responses, reconnect and shutdown; keep a test
that runs with no UI frames when behavior must continue while minimized.
Headless egui tests can inspect AccessKit nodes and feed keyboard/paste events.
They do not prove actual screen-reader, IME, driver or window-manager behavior.

The architecture ratchet is a hard gate, currently at zero. Move violations to
the correct layer; never increase the baseline to make a test pass. When an
existing count decreases, regenerate it with
`UPDATE_ARCHITECTURE_BASELINE=1 cargo test --test architecture` and commit it.
CI also rejects UI crates in the core's dependency graph and toolkits in the
scene's graph. A failed dependency inspection must fail the check, not yield
an empty list treated as success.

## Upgrade a dependency or compiler

Keep upgrades separate from feature work and record the prior and new versions
in the PR. Read the affected release notes and migration guide. Upgrade the
egui/eframe/egui-wgpu family together, along with the compatible wgpu major;
check feature flags, minimum Rust version and the pinned toolchain. Do not
remove AccessKit, Wayland, or another platform feature merely to pass a local
build. Commit the resolved lockfiles.

Run the native workspace tests, architecture gates and Tauri host compilation
when its shared core changes. For Tauri development without release sidecars,
the CI check supplies `TAURI_CONFIG` with empty `bundle.externalBin` and null
`bundle.resources`; that override is for checking, never for release packaging.
Require green platform CI on the final commit and an independent review.

For a UI/GPU upgrade, also record manual results for keyboard navigation,
focus, CJK/IME, scale-factor changes, narrow panels, help dialogs and viewport
stability. Check picking, trails and volumes on the actual target adapters.
State which platforms were compiled and which were actually exercised.
Compilation alone does not validate a driver or accessibility bridge.

## Performance and release evidence

Use release builds and the same 24/64-object, 100 Hz scenarios before and after
an optimization. Record hardware, window size, settings and frame-time
percentiles, CPU, RSS, model-lock duration and service lateness. Include a quiet
and a minimized window. Keep one-off measurements separate from CI assertions;
compare equivalent workloads rather than mixing debug and release results.
Avoid per-frame rebuilding, unbounded histories, copies of gain tables and
holding the model lock while constructing volume grids.

[release-process.md](../docs/release-process.md) defines packaging and version
alignment. Test an extracted native archive from outside the checkout, with a
fresh config, on each supported platform. Verify resources, renderer launch,
layout selection and shutdown. A build artifact is not a published release;
release publication follows the project's release procedure. Report remaining
intentional differences and unexecuted checks rather than marking parity from
the presence of a panel.
