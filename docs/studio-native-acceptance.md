# Native Studio acceptance evidence

This records executed checks, not a declaration of complete port parity. Keep
new results tied to a commit and distinguish a software rendering environment
from supported hardware/OS acceptance. The [completion plan](studio-native-completion.md)
defines the remaining scenarios.

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
