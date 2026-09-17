# Native Studio acceptance evidence

This records executed checks, not a declaration of complete port parity. Keep
new results tied to a commit and distinguish a software rendering environment
from supported hardware/OS acceptance. The [completion plan](studio-native-completion.md)
defines the remaining scenarios. The [manual platform procedure](studio-native-manual-validation.md)
provides isolated commands, expected outcomes and a sign-off template.

## Linux software-rendering smoke, 2026-09-17

Source: `40f330c4734caa310168d69c400055fd5d711e2a` (diagnostic history
integration candidate, subsequently merged). Debug build, egui/eframe 0.36.2,
wgpu 30.0.1, winit 0.30.13, Xvfb 21.1.24, Vulkan llvmpipe Mesa 26.2.2 with
LLVM 22.1.8. A private X server and temporary configuration were used; no
installed renderer, audio device or service manager was controlled.

| Scenario | Observed result |
|---|---|
| Start with 24 synthetic objects at 100 Hz, 1400×900 | Window, head model, room, speakers, labels and moving objects rendered; receive rate around 100 packets/s |
| CJK labels | System fallback font loaded; Japanese/Chinese synthetic labels displayed |
| Stop synthetic publication after two seconds | Receive rate reached zero; view settled without continuous UI sampling |
| Open Room Geometry on the settled view | Outer panel extents stayed fixed; scene projection and head position stayed fixed; content scrolled inside the left panel |
| Resize to 1000×700 | UI and scene rendered; the two default wide panels left little scene visible, so users need to resize/collapse them |
| Close via WM_DELETE_WINDOW | Did not complete within ten seconds in this environment; reproduced both with and without optional Vulkan layers |

The closure investigation located the main thread in Vulkan texture-view
retirement during `Queue::submit`, before application `on_exit`. The same
failure reproduced with a minimal eframe window containing only a label and
an `on_exit` message, without the Studio scene, listener, jobs or preferences.
This does not establish an application shutdown regression; it prevents
claiming successful end-to-end shutdown on this software stack. The test
processes were explicitly terminated after observation.

The small reproduction is retained as:

```sh
cd omniphony-studio-egui
cargo run --example window_smoke --locked
```

Close its window using the window manager. Successful teardown prints
`smoke on_exit reached`. Run it and Studio on the same display/adapter when
separating an application failure from a toolkit or driver failure. Never
weaken production validation flags to turn this test result green.

The debug software-rendered runs are not hardware performance benchmarks.
They do not validate actual IME composition, a platform screen reader, audio
routing, clean-machine archives, Wayland, Windows or macOS. Those remain
explicit release acceptance work. Core headless shutdown, port release and
no-frame deadline tests provide separate automated evidence.

## Packaged-resource Linux release smoke

Source: `82b1bfe37f9e53769442bcd95582dab70da69f98` (native packaging
candidate rebased over the volume-lock and shared-parser changes). Release
profile, Ryzen 9 9950X, the same Xvfb/software Vulkan stack as above, default
1400×900 window. The executable, layouts and head asset were copied into a
standalone staging directory and launched from an unrelated working directory.
Both CLI defaults and startup logs resolved the staged resources; four layouts
and the head mesh loaded successfully. This staging check did not include the
renderer sidecar or test audio, an extracted release archive, a clean OS image
or an actual installation/update.

Synthetic input: 100 Hz, stop after 16 seconds, fresh isolated configuration for
each run. CPU/RSS sampled at 5 and 14 seconds under load, then at 20 and 25
seconds after settling. FPS and frame interval are means of ten one-second
application reports (t=5 through t=14), not per-frame percentiles or GPU timings.
CPU is percent of one logical core, so software rendering can exceed 100%.

| Objects | Object field | Mean FPS | Mean reported frame interval | Active CPU | Active RSS | Settled CPU |
|---|---|---|---|---|---|---|
| 24 | off | 43.7 | 23.11 ms | 1024% | 210–211 MiB | 0% |
| 64 | off | 23.5 | 43.24 ms | 944% | 211–213 MiB | 0% |
| 64 | on | 18.5 | 54.40 ms | 1177% | 228–232 MiB | 0% |

Receive reports remained around 100 packets/s while active. The settled samples
showed no CPU ticks and unchanged RSS over five seconds, with the window still
visible. This is a short idle observation, not a leak test or a minimized-window
check. Processes were terminated by the harness; these runs do not resolve the
window-close failure above. Hardware frame percentiles, service lateness and
Windows/macOS acceptance remain open.

The Linux `omniphony-studio-egui/scripts/measure.sh` helper now uses a fresh
configuration and temporary output directory and never captures the user's
desktop. It accepts extra Studio flags after its three numeric arguments and
`STUDIO_BINARY` for an installed executable. Its timing windows differ from the
short observations above; retain each run's log and environment when comparing.


## Reproducible frame-interval percentiles

Stats output now includes nearest-rank p50/p95 of the last 256 raw UI frame
intervals and their sample count. Collection uses a fixed array without per-frame
allocation; sorting occurs only when the user requested a stats print. These
values include scheduling/idle gaps and are not GPU execution times. This
instrumentation was added after the software-rendering measurements above;
those historical results remain averages and are not retroactively percentiles.
