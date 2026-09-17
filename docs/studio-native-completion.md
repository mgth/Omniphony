# Native Studio completion and acceptance plan

Status: implementation planned. Reference: `6e6db009` plus the slider echo fix
in PR #511. This is the current execution checklist; the native UI phase notes
remain historical records, not statements of present parity.

Keep the toolkit-free core, toolkit-free wgpu scene and drawing-only UI. Each
concern gets a dedicated branch, tests and review before merging with green CI.
Do not raise the architecture baseline. Update this checklist with evidence as
work lands. A checked box means the acceptance criteria passed, not just that
code was written.

## Delivery sequence

| Lot | Work | Dependencies | Acceptance |
|---|---|---|---|
| 01 | Baseline and reproducible validation | — | Current parity matrix and regression scenarios; isolated test resources |
| 02 | Persistent text drafts | 01 | Multi-frame typing, paste, validation, cancellation and selection changes; one command per commit |
| 03 | Script editor request lifecycle | 01 | Load/save acknowledgements and errors; preserve edits; reject stale results |
| 04 | Connection state and event application | 01 | One core-owned connection state; reconnect/reset and input-pipe events tested |
| 05 | Service wakeups and overlay synchronization | 04 | Interest changes wake the clock; unrelated state causes no overlay traffic |
| 06 | Action availability | 04 | Standalone/embedded/offline/legacy capability matrix exercised |
| 07 | Non-blocking host operations | 04 | Slow DNS, files and service manager never block drawing or service deadlines |
| 08 | Explicit runtime shutdown | 07 | Repeated start/stop leaves no threads or ports; external renderer untouched |
| 09 | Startup modes and paths | 04 | Normal/listen-only/synthetic modes; installed and checkout launches independent of cwd |
| 10 | Durable preferences and migration | 08, 09 | Atomic writes, final flush, visible failures and repeatable migration |
| 11 | Per-panel view state | 02–06 | Representative panels testable without unrelated app state; meaningful state explicit |
| 12 | Typed stable domains and shared host logic | 04, 11 | Common rules implemented once; both frontends remain compatible |
| 13 | Telemetry and scene performance | 04, 07 | Reception-timed bounded histories; volume work outside the shared model lock |
| 14 | Localization, accessibility and portability | 02, 11 | Keyboard, focus, accessible names, CJK/IME and viewport geometry checked |
| 15 | Contributor documentation and CI | incremental | A newcomer can build, reproduce and add a control using the guide |
| 16 | Native distribution and parity sign-off | 02–10, 14, 15 | Installed archives tested on all supported platforms; remaining differences explicit |

## Existing work to reuse

- #511: stepped sliders must not send renderer echoes back as user edits.
- #510: panel groups and their layout conventions; reconcile panel edits after
  this lands rather than implementing a competing layout system.
- #512: help dialog sizing.
- #505: native archives, bundled resources and native cross-platform CI.
- #504: release version alignment; do not publish or bump again independently.

## Parity and regression matrix

The presence of a panel is not proof of behavioural or visual parity.

| User journey | Native implementation | Outstanding acceptance |
|---|---|---|
| Edit speaker names, input/output paths, tracker address | Present; transient strings lose drafts | Typing over multiple frames; external echo while editing; Enter, blur, Escape; change selection |
| Edit renderer-owned scripts | Present; save/error handling incomplete | Ack, rejected write, delayed reply, close/reopen, dirty document |
| Open Studio and reconnect | Present; saved target not used by default | Saved target, explicit override, local/remote, missing renderer, restart |
| Manage standalone renderer/services | Present; blocking operations and inconsistent status | Ownership, timeout, unavailable manager, cancellation, shutdown |
| Drive embedded renderer | Present; capability policy incomplete | Hide/disable irrelevant input/output/resampler/process actions |
| Show meters and timed speaker/object tests | Present, core services | Stops and subscriptions work without frames; quiet/minimized window |
| Mirror display preferences to mpv | Present; revision and wakeup issues | One sync on reconnect, changed values only, external overlay changes |
| Change layouts/profiles/render parameters | Present | Allowed/frozen/unsupported/rejected commands; preserved coordinates and channel contract |
| Use binaural processing and SOFA | Present | Local/remote file flow, failure/cancellation, tracking and calibration |
| Inspect diagnostics and tune resampling | Present | Timestamped samples, gaps, paused display, minimized window, cancel/quit restoration |
| Navigate the 3D scene | Present | Picking, gizmos, trails, heatmaps, head pose; overlays never resize the viewport |
| Save preferences and upgrade | Present; debounce lacks final flush | Immediate quit, write failure, corrupt file, version migration |
| Install and update | Work in #505/#504 | Clean-machine archives, resources, version and correct update artifact |

Intentional differences: obsolete PCM controls stay retired; panel organization
may differ; the native script editor uses the Studio palette rather than the
web editor's theme picker. Other differences require an explicit entry here.

## Test layers

1. Core unit tests drive injected time and inspect emitted commands. Include
   negative cases and unchanged-state cases, not only successful sends.
2. Headless egui tests span multiple frames and use real input events. Test
   drafts, focus, selections and renderer echoes; a one-frame paint is not an
   interaction test.
3. A deterministic fake renderer supplies snapshots, capabilities, failures,
   delayed replies and reconnections. Use temporary configuration and ephemeral
   loopback ports; never contact an installed renderer or manage real services.
4. A small real-renderer acceptance matrix covers standalone and embedded
   operation, local/remote connection, renderer restart, window minimization,
   profile/layout edits and active-test shutdown. Report hardware-dependent
   checks separately from automated results.
5. Native build gates cover Linux, Windows and macOS. Archive smoke tests run
   outside the checkout and from an unrelated working directory.

## Performance and upgrade gates

Record hardware, resolution, build profile and settings alongside each result.
Measure the existing synthetic scenarios with 24 and 64 objects, 100 Hz input,
volumes off/on and a quiet/minimized window. Report frame-time percentiles,
process CPU, memory, model-lock hold time and service lateness. Establish
budgets from the initial measurements before optimizing; distinguish measured
improvements from assumptions. Keep histories, pending jobs and notifications
bounded, and reuse hot-path buffers.

Dependency/toolchain upgrades are separate concerns with committed lockfiles,
all-platform compilation and the same interaction/visual acceptance checks.
Core must remain free of UI dependencies; scene may depend on wgpu, not a UI
toolkit. Guard failures must not be hidden by a failed dependency inspection.

## Completion record

- [ ] 01 Baseline and validation foundation
- [ ] 02 Text drafts
- [ ] 03 Script editor
- [ ] 04 Connection and events
- [ ] 05 Wakeups and overlay
- [ ] 06 Capabilities
- [ ] 07 Host operations
- [ ] 08 Runtime lifecycle
- [ ] 09 Startup and paths
- [ ] 10 Persistence
- [ ] 11 Panel boundaries
- [ ] 12 Shared typed domains
- [ ] 13 Telemetry and performance
- [ ] 14 Localization and accessibility
- [ ] 15 Contribution and CI
- [ ] 16 Distribution acceptance

Full completion requires green CI and review, no known blocking regression,
manual platform/audio evidence, and documented remaining intentional differences.

### Script editor, first correction

Save acknowledgements finish the pending state without replacing newer typing;
load replies preserve a buffer edited after the request, and renderer errors
reach the editor. A new request clears buffered old replies. These behaviours
have unit coverage. Lot 03 remains open: the legacy protocol has no correlation
identifier for late same-parameter responses; request deadlines and dirty-close
confirmation still need implementation.

### Startup policy

Normal startup now reads the saved renderer and Studio listen port. `--register`
and `--listen-port` override those values; `--listen-only` and `--synthetic`
start passively with an ephemeral port by default and suppress auto-launch.
DNS runs off the first-paint path. The config namespace matches Tauri by default
and continues to honor `OMNIPHONY_CONFIG_DIR/studio`. Resource discovery for
installed archives belongs to the existing native distribution PR; migration of
the old checkout-relative native preferences remains in lot10.

### Durable preferences

Native preferences use a single owned background writer, with one coalesced
pending snapshot. The core owns the debounce, including while minimized;
shutdown bypasses it and joins the final write. Save failures remain visible
in the connection header and retry at a bounded cadence. OSC configuration
and native preferences replace the destination atomically after serializing
and syncing a sibling temporary file. This protects the prior document when
serialization or writing fails; power-loss durability of the directory entry
is not promised on every filesystem.

Legacy `layouts/.studio-egui` JSON files are validated and copied once when
using the default namespace. Existing destination files, including invalid
ones, are never replaced by migration; sources remain available for rollback.
An explicit `OMNIPHONY_CONFIG_DIR` stays isolated. This migrates native JSON,
not browser localStorage; transfer from the web UI remains an acceptance gap.

### Diagnostic reception history

Diagnostic traces now consume a core-owned history populated at OSC reception,
with a timestamp per packet. Repainting never invents samples; minimized windows
retain arrivals. History is bounded to 64 selected metrics, 12,001 samples each
and a 60-second retention window. The view copies only arrivals after its cursor;
pause freezes its cache and time axis while reception continues. Reconnect clears
the trace. Gaps longer than one second break plotted lines and reject FFT windows.
The separate resampler plot and scene model-lock measurements remain in lot13.

### Volume calculation and model lock

Volume inputs are captured while reading the model; the expensive voxel loops
run after releasing its guard. Decoded gain artifacts are immutable `Arc`s and
the object input buffer reuses its allocation. Regular room/object geometry
still reads the model under a guard, so this is not a claim that every scene
operation is outside the lock.

Reproducible CPU-only measurement, 2026-09-17: Linux x86_64, Ryzen 9 9950X,
release build, default room, 64³ object volume, 20 forced rebuilds per case.
No window, GPU, audio or 100 Hz network workload is included. Values below are
milliseconds; capture excludes lock acquisition and other scene geometry.

| Objects | Previous rebuild p50/p95 (under lock) | New capture p50/p95 | New total p50/p95 |
|---|---|---|---|
| 24 | 5.474 / 6.339 | 0.001070 / 0.003550 | 4.997 / 5.301 |
| 64 | 13.358 / 14.153 | 0.006410 / 0.009690 | 12.798 / 13.004 |

Volumes disabled measured below 0.001 ms in both cases. Total-time differences
are small enough not to infer a throughput improvement from this short run;
the demonstrated improvement is removal of the rebuild from the model lock.
The initial local regression target is capture p95 below 0.05 ms for 64 objects;
it is not a portable CI timing assertion. Run from `omniphony-studio-egui`:

```sh
cargo test -p omniphony-studio-scene --release --locked volume_cpu_benchmark -- --ignored --nocapture
```

Full frame/GPU, minimized CPU/RSS and service-deadline measurements remain
separate acceptance work in lot13.

### Shared wire parser

The Tauri host now re-exports the core's OSC parser and event types instead of
maintaining a second 1,444-line copy. Existing parser tests remain in the core;
serialization and public event variants are unchanged. Linux CI type-checks the
Tauri host with its committed lockfile, without requiring a release sidecar
bundle, to catch shared-core dependency and API drift before release.
### Script draft protection and deadlines

File requests now have a 15-second deadline on the core service clock, so hiding
or minimizing the editor cannot suspend timeout handling. Connection changes
interrupt pending requests with a typed failure; local UI code translates that
failure. Close, quit, New, Reload and file selection ask before discarding a
dirty document or abandoning a pending request. Save acknowledgements mark
only the submitted revision as saved; typing after Save remains dirty.

This does not cancel a renderer write already in flight. Correlation of late
same-parameter responses from the legacy untagged protocol remains open in
lot03 and requires a compatible wire-protocol extension.

### Correlated script file requests

The renderer advertises `fileRequestIds` and echoes an optional opaque request
identifier on file content and error replies. The native host tags each request,
rejects late/wrong/duplicate tagged responses, and requires the tag from a
renderer advertising support. This extension preserves original argument shapes
for older clients. It correlates acknowledgements; it does not make a write
idempotent or cancel work already accepted by the renderer.

Legacy renderers remain usable through their untagged replies, with the
unavoidable old ambiguity after an interrupted same-parameter request. Upgrade
the renderer to obtain strict correlation. Both standalone and embedded
renderer capability documents advertise the extension.

### Independent profile panel

The profile picker owns its editor, draft, focus request and confirmation in a
`ProfilePanel`. It draws from a core snapshot and returns a typed intent; it can
be tested without constructing Studio, a renderer, a window or a GPU. The core
revalidates create/rename/delete against the latest echo and sends multi-command
operations in order. Tests cover changed selections, duplicates, deletion of
the last or removed profile, and multiple UI frames with Unicode typing/paste,
Enter and Escape. This establishes the lot11 extraction pattern; remaining
panels and context-backed help/section state still need migration.
### Owned background work

The host now owns generic jobs and release checks, accepts at most eight active
jobs and refuses excess work instead of creating an unbounded thread/queue set.
Completed handles are reaped. Shutdown stops audio services, rejects new jobs
and joins accepted operations before stopping a Studio-launched renderer and
the listener. Rejected release checks leave their running state and report an
error. Tests cover capacity, completion, rejection, panic handling and dropping
the last host reference from a worker.

This is cooperative lifecycle ownership, not forcible cancellation of an OS
call. Shutdown can still wait for a file operation, resolver or interactive
service authorization already in progress. Those operations are deliberately
not killed halfway through a system change; cancellation and platform timing
acceptance remain part of lots07–08.

### Non-blocking layout transfers

Layout imports/exports use asynchronous native pickers and host-owned workers
for directory probing, parsing and writing. Only one transfer per panel can be
pending. Application logic consumes completions even when the panel is hidden;
worker/future completion wakes it without a polling timer. Export captures the
chosen layout before the dialog; import revalidates connection intent, session
epoch, active profile and frozen speakers before changing the model or sending
the replacement. Cancelling the picker leaves the model/draft untouched, and
errors remain visible. Layout replacement normalization now belongs to the core.
Tests cover stale sessions, profiles, freeze, duplicate keys and worker file
round trips. Native dialog behavior on each OS remains manual acceptance.

Other native file pickers (backend files, evaluator and executable selection)
still use modal platform APIs; their file processing has separate ownership.
Closing an OS dialog or interrupting a filesystem call is not guaranteed by
dropping a Rust future. The owned-job shutdown limitations above still apply.
