# Native Studio: platform acceptance procedure

Run this on Linux with a real GPU, Windows x64 and macOS arm64. Automated CI
compilation and headless tests are separate evidence; a green build does not
mark a manual row passed. The software-Vulkan observations and their known
window-close failure are in [studio-native-acceptance.md](studio-native-acceptance.md).

## Record the environment

Copy this header into the result report:

```text
Commit / release and archive name:
OS version / architecture:
CPU / RAM:
GPU / driver / graphics backend:
Display resolution / scale / refresh rate:
Audio device / driver / buffer / sample rate (for live tests):
Studio / renderer / mpv versions:
Locale / input method / screen reader:
Scenario ID: PASS | FAIL | NOT RUN
Observed result, reproduction steps and log/capture paths:
```

Use an unpacked candidate built from the reviewed commit, including its
renderer, layouts and assets. Keep the older version separately for rollback.
Do not use the source checkout as the working directory. Preserve logs locally;
use anonymous input names and remove private paths before publishing a report.

## Isolated launch

The synthetic and listen-only modes do not auto-launch a renderer. Every run
must use a fresh configuration namespace. Do not run the service-management
cases against a production service.

Linux/macOS, from a terminal (replace the executable path):

```sh
STUDIO_BIN="/absolute/path/to/unpacked/omniphony-studio-egui"
STUDIO_RUN="$(mktemp -d)"
export OMNIPHONY_CONFIG_DIR="$STUDIO_RUN/config"
cd "$STUDIO_RUN"
"$STUDIO_BIN" --synthetic 24 --rate 100 --synthetic-stop-after 60 \
  --stats-interval 1 > "$STUDIO_RUN/run.log" 2>&1
```

Windows PowerShell:

```powershell
$studioBin = 'C:\path\to\unpacked\omniphony-studio-egui.exe'
$studioRun = Join-Path $env:TEMP ('studio-acceptance-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $studioRun | Out-Null
$env:OMNIPHONY_CONFIG_DIR = Join-Path $studioRun 'config'
Set-Location $studioRun
Start-Process -FilePath $studioBin -Wait -ArgumentList @(
  '--synthetic','24','--rate','100','--synthetic-stop-after','60',
  '--stats-interval','1'
) -RedirectStandardOutput (Join-Path $studioRun 'stdout.log') `
  -RedirectStandardError (Join-Path $studioRun 'stderr.log')
```

Close the window after the scenario. Confirm the process exits and its printed
UDP port can be reused. If closing hangs, retain the log and compare the minimal
`window_smoke` example on the same display/adapter before attributing the failure
to Studio. A forced termination is cleanup, not a successful shutdown result.

## Visual, input and idle scenarios

| ID | Steps | Pass criteria |
|---|---|---|
| V01 Resources | Launch from the unrelated directory above, without explicit resource flags. | Log resolves layouts and head mesh inside the unpacked archive; room, head and speakers render. No checkout path or placeholder sphere is used. |
| V02 Geometry | Capture the settled scene, expand Room Geometry and other long sections, scroll their content, then collapse them. Repeat at 1400×900 and 1000×700. | Expansion keeps the outer overlay bounds and scene projection stable. Overflow scrolls internally. Narrow panels can be resized/collapsed without losing controls. |
| V03 Navigation | Orbit, zoom, pan, select a speaker/object, move its gizmo, toggle trails and volume. | Picking follows the displayed geometry; labels remain legible; no unexpected scene jump or stale selection. |
| V04 Keyboard | Tab through connection, speaker and profile controls. Toggle switches with Space, use arrows on sliders, Enter/Escape in text fields. | Visible focus; each control works without a pointer; a disabled control cannot send a command. |
| V05 Text and IME | Type/paste accents and CJK text in a name, generated backend file/path field and script draft; use real IME composition before committing. Change selection or profile, or reconnect, while editing. | Composition is not committed early; typing survives frames and renderer echoes; in single-line fields Enter/blur commits once and Escape cancels; script Enter inserts a newline; selection changes do not edit the old entity, and old path drafts never reach the replacement profile/connection. |
| V06 Screen reader | With the platform reader enabled, inspect shared switches and sliders, their enabled state and values. | Accessible names match visible labels; roles, checked/value state and keyboard actions are exposed. Record unnamed custom controls as failures. |
| V07 Locale/scale | Repeat key controls in English/French, at 100% and 200% scale where supported; switch monitors with different scales. | No essential label is clipped or untranslated; glyphs and focus remain usable; the 3D callback covers the viewport after scale changes. |
| V08 Quiet/hidden | After the synthetic feed stops, wait five seconds; sample CPU/RSS for ten seconds. Repeat while minimized, then restore. | Receive rate goes to zero, the view settles, CPU returns close to the platform idle baseline and RSS does not grow continuously. Restoring displays current state. |
| V09 Exit/restart | Close normally during active synthetic traffic and after it has stopped. Repeat five times with fresh namespaces. | No surviving Studio worker/process or occupied listen port; next start succeeds. |

## Performance recordings

Repeat 24 and 64 objects at 100 Hz with object field off/on, fresh configuration,
the same resolution, scale and driver, using `--synthetic-stop-after 60` and
`--stats-interval 1`. Add `--object-field` for the on cases. Record whether
trails and vsync are enabled; a separate `--no-vsync` run measures headroom and
must not be compared directly to a vsynced run.

Each stats line includes FPS, packet rate, RSS, CPU and raw frame-interval
p50/p95 over the last **256 UI frames** (`frame_samples` reports the available
count). Discard warmup and record steady-state windows with 256 samples. These
are wall-clock intervals between UI frames, including scheduling gaps, not GPU
execution times. Do not average percentiles to claim a whole-run percentile.
The existing `frame_ms` is an exponential average, not a percentile. CPU in the
built-in report is implemented on Linux only; use the OS process monitor on
Windows/macOS rather than interpreting its zero as a measurement.

On Linux, the isolated helper also samples `/proc` while active and after the
feed stops (the GUI must be closed normally in a separate V09 run):

```sh
STUDIO_BINARY="$STUDIO_BIN" /path/to/checkout/omniphony-studio-egui/scripts/measure.sh 64 100 60 --object-field
```

Compare with a recorded baseline on the same machine. Report regressions with
both logs; do not substitute the software-renderer numbers for hardware budgets.
The volume-input capture target is documented separately in the completion
plan and its CPU benchmark does not measure full GPU/frame latency.

## Live standalone and embedded fixtures

Use disposable renderer/mpv fixtures, a test layout and an anonymous input such
as `input.mkv`. Record their OSC endpoints and versions. Start the fixture
explicitly and connect Studio with `--register host:port`; keep its configuration
namespace isolated. Begin audio checks at a low test level with the intended
device selected. Do not install/uninstall an existing personal or system service.

| ID | Steps | Pass criteria |
|---|---|---|
| L01 Startup/reconnect | Connect locally, quit, restart normally; override the saved target with another explicit endpoint; stop/restart the fixture. | Saved target works, explicit override wins, reconnect resets old live data and resubscribes once. A slow old lookup cannot restore the previous target. |
| L02 Remote files | Connect to a remote fixture while the saved configuration names localhost; leave a script editor open across local/remote changes. | Local Browse is offered only for the active local target. Remote load/save uses the renderer file protocol. |
| L03 Capabilities | Compare standalone, embedded and offline states; also exercise a compatible older renderer if available. | Unsupported process/input/output/resampler actions are unavailable; supported controls still work. Legacy limitations are reported, not claimed as modern protocol guarantees. |
| L04 Drafts/scripts | Edit while state echoes arrive, save and keep typing, provoke a rejected path, interrupt the connection during a request, then close/New/Reload/quit with a dirty buffer. Leave a native picker open while typing or closing/reopening/navigating the editor; change connection before confirming discard. | The submitted revision alone becomes saved; newer typing survives; errors/timeouts become visible; dirty work requires an explicit discard. Tagged stale replies cannot complete another request (also covered automatically). A stale native picker cannot load into another document/session, even after a delayed discard confirmation. |
| L05 Profiles/layouts | Create/switch/rename/delete a profile; change the selection during editing; import/export a temporary layout and try changes during backend freeze. While its picker/read is pending, reconnect or request a profile switch before receiving its echo. | Correct profile is targeted, duplicates/stale names are rejected, the last profile is preserved; layout roundtrip is coherent and forbidden edits are refused. The UI remains responsive, duplicate transfers are disabled and stale imports are rejected. Export writes the layout selected before opening the picker. |
| L06 Timed tests | Start a timed speaker/object test and minimize immediately. Test cancel and quit during a burst. | Test ends at its configured duration, cancel/quit leaves no continuing tone, and an externally started renderer remains alive. Record measured lateness and audio buffer settings. |
| L07 Resampler | Collect latency/ppm traces, minimize while samples arrive, reconnect, pause/resume and run/cancel the tune wizard. | Traces follow reception timestamps, gaps are explicit, paused view freezes, bounded histories reset on reconnect; cancel/quit restores the prior tuning state. |
| L08 Overlay/SOFA | Change display preferences with an mpv fixture; browse/import a temporary SOFA file, including error, cancellation, hidden-panel completion and a busy operation when a picker returns. | Changed overlay preferences synchronize once; unrelated state creates no repeated traffic. SOFA failures leave a usable UI and report the reason. |
| L09 Host management | In a disposable OS user/VM only, test unavailable manager, launch/stop, service install/restart/uninstall and rejected authorization. | UI remains usable, errors are visible, no duplicate pending action is accepted, and owned/external renderer lifetime is respected. Record time spent waiting for OS authorization at quit. |

## Persistence, installation and upgrade

Use only copies inside the isolated namespace. Its files are under
`$OMNIPHONY_CONFIG_DIR/studio` (PowerShell: `(Join-Path $env:OMNIPHONY_CONFIG_DIR 'studio')`).

| ID | Steps | Pass criteria |
|---|---|---|
| P01 Final flush | Change a display/host preference and immediately quit; reopen the same namespace. | Newest setting survives, with no truncated JSON or temporary-file residue. |
| P02 Write failure | Make the disposable configuration destination unwritable, edit, restore permissions and retry/restart. | Failure is visible; previous valid data survives; writable sessions retry and final flush succeeds. |
| P03 Corrupt/future | With Studio closed, replace a copied `studio-egui-prefs.json` with invalid JSON, then with `{"schema_version":999}`; also test invalid `osc_config.json`. | Studio reports the error and preserves the original bytes; session defaults/changes do not overwrite an incompatible document. Restore a compatible copy while closed, then restart. |
| P04 Legacy | Test unversioned preferences in the isolated namespace. For checkout migration, use a disposable OS user/VM, unset the explicit namespace override, provide a copied legacy `layouts/.studio-egui` through `--layouts-dir`, and run twice with `--listen-only`. | Supported values survive; an existing destination is not overwritten; original files remain available. Explicit namespace overrides intentionally skip checkout migration. Browser localStorage is not automatically migrated: record this known difference. |
| P05 Clean archive | On a clean supported OS image, unpack the candidate, run from another directory, select the bundled renderer and exercise V01/V09/L01. | No developer toolchain/checkout is required; resources and sidecar are present, versions agree, and the correct native archive is discoverable for update. Record OS signing/quarantine prompts. |
| P06 Upgrade/rollback | Keep old/new archives separately, back up the test namespace, open it with the new version, then restore the backup and old version. | Upgrade preserves supported settings and rollback from the backup works. Never rely on an older binary rewriting an unsupported new schema. |

## Sign-off

Attach the environment header and a result for every applicable row. Mark
unavailable fixtures **NOT RUN**, with a reason. Do not mark a platform accepted
if a required row failed or was not run. Keep failures linked to a reproducer,
including the minimal window comparison for graphics/close failures. Release
publication is a separate decision after these results have been reviewed.
