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
