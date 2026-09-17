# Native Studio completion and acceptance plan

Execution record for the native hardening workflow, based on the initial
`6e6db009` audit. The implementation references and acceptance evidence below
replace the initial bug inventory; older phase notes remain historical records.

The user authorized independent agent review and integration with green CI.
Each concern has its own PR. The toolkit-free core, toolkit-free wgpu scene
and drawing-only UI remain enforced; the architecture baseline stays at zero.

Hardware/platform acceptance is delivered as a reproducible procedure, as
requested: [manual validation](studio-native-manual-validation.md). Its rows
are **NOT RUN** until someone executes them on the specified machine. Automated
checks and software-renderer observations do not imply audio, real-GPU,
screen-reader or installed-package acceptance on Windows/macOS.

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

## Related work reused

Slider echo suppression (#511), panel groups (#510) and bounded help sizing
(#512) were integrated before the dependent panel changes. Native archives and
bundled resource discovery reuse #505; version alignment reuses #504. No release
tag, promotion or publication is part of this execution.

## Parity and regression matrix

The presence of a panel is not proof of behavioural or visual parity.

| User journey | Native implementation | Outstanding acceptance |
|---|---|---|
| Edit speaker names, input/output paths, tracker address | Persistent drafts; explicit commit/cancel and target changes (#514) | Real IME and keyboard platform matrix: V04–V07 |
| Edit renderer-owned scripts | Ack/error/deadline lifecycle, dirty guard and tagged requests (#515, #526, #527); session-bound async picks (#540) | Live rejected write/reconnect/late pick: L02/L04 |
| Open Studio and reconnect | Core connection policy and reset, saved target/explicit overrides (#516, #521) | Real endpoints/restart and all-platform exit: L01/V09 |
| Manage standalone renderer/services | Owned, bounded jobs and asynchronous host/config operations (#519, #535, #537, #538) | Disposable OS manager and authorization paths: L09 |
| Drive embedded renderer | Core capability policy and offline/legacy fallback (#518) | Standalone/embedded fixture comparison: L03 |
| Show meters and timed speaker/object tests | Core-owned clock, listener and shutdown; explicit wakeups (#517, #520) | Real audio stop/lateness: L06 |
| Mirror display preferences to mpv | Revision-sensitive synchronization and reconnect wakeup (#517) | mpv integration: L08 |
| Change layouts/profiles/render parameters | Typed profile intents and isolated panel (#529); asynchronous session-safe layout transfers (#539) | L05, including freeze/reconnect while choosing a file |
| Use binaural processing and SOFA | Existing core transfer jobs; async picker/deletion and hidden-panel completion (#540) | Local/remote fixtures, tracking/calibration, cancellation: L08 |
| Inspect diagnostics and tune resampling | Bounded reception histories, gaps and pause semantics (#523, #530) | Live wizard restoration and minimized view: L07 |
| Navigate the 3D scene | Stable overlays; volume work outside model lock (#524); software-GPU observations recorded | Real GPU picking, scale, trails, heatmaps: V01–V03/V07 |
| Save preferences and upgrade | Atomic/coalesced writes and final flush; schema/read-only protection; native legacy migration (#522, #531, #532, #538) | OS permissions and clean upgrade/rollback: P01–P04/P06 |
| Install and update | Native archives/resources and aligned versions (#505, #504) | Actual clean-machine launch, signing/quarantine, update asset: P05 |

Known differences and limits:

- Obsolete PCM controls stay retired. Panel organization may differ. The native
  script editor uses the Studio palette rather than the web editor's theme picker.
- Native JSON preferences migrate from the old checkout namespace; browser
  localStorage is not imported automatically. Transfer web-only preferences
  manually in a backed-up test namespace (P04).
- Legacy renderers without `fileRequestIds` cannot correlate interrupted
  same-parameter replies reliably. Upgrade both sides for strict correlation.
- Some secondary status/error text remains English. English/French resources
  and CJK glyph availability do not establish full translation or IME parity.
- OS dialogs and accepted filesystem/DNS/authorization calls are not forcibly
  killed at quit. Owned jobs are joined, so shutdown can wait on the OS.
- Multiple processes sharing one configuration namespace use last-writer-wins;
  external OSC configuration edits are picked up on restart. Use isolated
  namespaces for simultaneous sessions.
- ProfilePanel is the tested extraction pattern; the remaining panels and
  context-backed help/section state still need gradual extraction. This work
  does not claim that every panel is independent of StudioSpike.

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

## Implementation and evidence record

| Lots | Delivered changes | Evidence / remaining acceptance |
|---|---|---|
| 01 | Baseline, regression matrix, isolated validation scripts | This record and the manual scenario IDs; [observations](studio-native-acceptance.md) |
| 02 | Text drafts (#514) and echo suppression (#511) | Multi-frame egui input/echo/commit/cancel tests; V04–V07 remain manual |
| 03 | Script lifecycle (#515/#526), wire correlation (#527), async choices (#540) | Host and renderer tests; L04 for actual file/connection failures |
| 04–06 | Connection/reset (#516), wakeups/overlay (#517), capabilities (#518) | Core/fake-transport tests; L01/L03/L08 |
| 07 | Host operations (#519), mpv config (#535), runtime config (#538), layout/pickers (#539/#540) | Non-blocking job/poll tests; native OS interaction L05/L08/L09 |
| 08 | Owned runtime (#520) and bounded workers (#537) | Listener/clock/job lifecycle tests; V09/L06/L09 |
| 09 | Startup modes (#521), installed resources (#505) | Startup tests and Linux release staging smoke; P05 on clean OS images |
| 10 | Atomic persistence/migration (#522/#531), schema protection (#532), config cache/writer (#538) | Temp-file failure/final-flush/concurrency tests; P01–P04/P06 |
| 11 | Independent profile panel (#529) | Multi-frame Unicode/focus tests; remaining extraction is incremental |
| 12 | Shared wire parser (#525), typed profile actions (#529), core layout normalization (#539) | Tauri host type-check in CI plus core domain tests |
| 13 | Diagnostic/resampler histories (#523/#530), volume lock reduction (#524), frame percentiles (#534) | Measured CPU capture p95 below 0.05 ms locally; real-GPU frame/service budgets require manual recordings |
| 14 | Named keyboard-accessible controls (#528), visual/input protocol (#534) | Headless AccessKit/input tests and software-renderer smoke; real IME/readers/OS scale remain manual |
| 15 | Contributor guide, dependency/lockfile/build gates (#533) | Relative links verified; fail-closed dependency checks exercised; Linux/Windows/macOS CI |
| 16 | Native package/resources (#505), version alignment (#504), platform recipe (#534) | Publication intentionally not performed; final sign-off requires P05/P06 and all applicable manual rows |

The implementation and reproducible manual procedure are separate deliverables.
Full platform/audio parity sign-off requires completed manual reports and no
blocking regressions; neither green CI nor this table substitutes for them.

### Script editor, first correction

Save acknowledgements finish the pending state without replacing newer typing;
load replies preserve a buffer edited after the request, and renderer errors
reach the editor. A new request clears buffered old replies. These behaviours
have unit coverage. The subsequent #526/#527 corrections add deadlines, dirty-close confirmation
and optional tagged replies. Legacy untagged peers retain the ambiguity recorded
above.

### Startup policy

Normal startup now reads the saved renderer and Studio listen port. `--register`
and `--listen-port` override those values; `--listen-only` and `--synthetic`
start passively with an ephemeral port by default and suppress auto-launch.
DNS runs off the first-paint path. The config namespace matches Tauri by default
and continues to honor `OMNIPHONY_CONFIG_DIR/studio`. Installed resource discovery is covered by #505 and old checkout-relative native
preferences by #522; neither depends on the launch working directory.

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
The resampler plot follows the same reception-based principle in #530;
scene model-lock measurements are recorded below.

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

This does not cancel a renderer write already in flight. The #527 wire extension below correlates late replies on upgraded peers;
legacy untagged responses retain their documented ambiguity.

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

Backend/script, WAV clip and SOFA pickers also run asynchronously with a single
pending picker and an explicit repaint waker. Renderer-directed choices reject a
changed session/profile and a non-local active target; returning to a script
editor rechecks its identity and dirty-document guard. SOFA deletion uses owned
jobs, and SOFA completions are consumed from application logic even while its
panel is hidden. Unused synchronous picker entry points were removed. Closing
an OS dialog or interrupting a filesystem call is not guaranteed by dropping a
Rust future; the owned-job shutdown limitations above still apply.
### mpv configuration work off the UI thread

Opening the OSC section and changing its mpv decoder switch now schedule core
jobs instead of reading or rewriting mpv.conf in the paint callback. The view
polls without waiting, shows pending work and refuses duplicate operations. A
result invalidated by closing the section cannot replace the next fresh read.
Write failures remain visible until reopening the section and are logged by
the core. Tests use controlled channels and never modify the user's mpv file.
Layout/configuration operations and owned background jobs are covered by the
subsequent changes described in this record. OS calls remain non-forcible.

### File choices during connection transitions

Native file-choice tokens also cover the interval between reserving a DNS intent
and draining its reconnect command. The model tracks pending resolution and
queued transport transition separately, and reconnect commands carry the local
intent identifier (not a wire-protocol change). Tokens created in either phase
remain ineligible even if resolution later fails. Only a matching transport
reset clears its queued transition; an old producer restart, a superseded DNS
failure or an older queued command cannot release a newer transition. Tests
exercise all these interleavings without a network lookup or live renderer.
### Connection configuration without per-command disk access

One core-owned configuration snapshot now backs metering, host switches,
connection changes, import-directory memory, the launcher and watchdog. Patches
are serialized in memory, preserving unrelated fields even when commands run
concurrently, and submitted in the same order to an atomic background writer.
The writer coalesces changes, retries failed writes and flushes at shutdown.
The connection header reports persistence failures. Invalid/unreadable input is
read-only for the session, while session changes remain usable and never replace
the original document with defaults.

External edits to osc_config.json are loaded on the next Studio start. This is
one writer per host instance, not cross-process locking: simultaneous instances
still use last-writer-wins persistence. Layout file bytes and directory probes
use the owned jobs described above.

### Generated backend file-path drafts

Generated backend path/file controls use the same persistent draft widget as
speaker/path/tracker fields. Independent fields preserve typing through frames
and external echoes, commit once on Enter/blur and cancel on Escape. Drafts are
keyed by backend and parameter, reset on profile or renderer-session changes, and retained
only for fields visible in the current/previous frame. Browse/Edit structural
clicks discard the pending path draft before any blur can send it. Headless
multi-frame tests cover Unicode paste, multiple fields, echo/commit, changed
backend/session and bounded storage. The core session token also rejects drafts
during connection transitions and revalidates immediately around command
queuing, preventing a path typed in profile A from being applied to profile B.
The targeted workspace suite passes 339 tests; one manual benchmark is ignored.


### Preference schema compatibility

Native preferences now save `schema_version: 1`; unversioned native JSON loads as
version 1 without dropping known preferences. Unknown versions, malformed JSON
and failed legacy migration keep defaults in memory but disable saving for that
session, with the error visible. The original document is never silently replaced
by defaults. Recover by opening it with a compatible Studio, or restoring/moving
the affected preferences file while Studio is closed, then restarting. Future
migrations must be explicit and covered by fixtures. Concurrent Studio instances
still use last-writer-wins; browser localStorage import remains separate work.
### Shared control accessibility

Every custom switch now exposes its translated visible label and checked state
to AccessKit. Keyboard focus draws an outline inside the existing control bounds.
The two shared slider layouts expose their separate labels without duplicating
painted text or changing the native slider's value, range and actions. Headless
tests exercise the accessibility tree, keyboard activation and disabled controls.
This is partial lot14 coverage: specialized controls, actual platform screen
readers, CJK/IME, and visual acceptance still need separate validation.
### Resampler reception history

The latency/rate plot now uses independently timestamped OSC arrivals and the
same bounded core history as diagnostics. Two consumers (plot and tuning wizard)
share a cache without duplicate samples; missing UI frames do not drop arrivals.
Closing both consumers releases history, and producer reset clears it while
preserving collection interest. Gaps over one second break curves; the target
latency is explicitly a guide for the current setting, not a measured trace.
The plot no longer requests continuous repaint. Tests exercise independent
publication timestamps, repeated copies, no-frame reception and reconnect/reopen.
