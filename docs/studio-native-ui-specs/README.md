# Studio native UI: extracted specifications

These documents describe the **web Studio** (`omniphony-studio/`) — what it
draws, what its controls do, where every value comes from and what each action
sends. They were written by reading the JavaScript, the CSS and the Tauri host
before porting any of it, so the native egui host
(`omniphony-studio-egui/`) could be built against a written contract instead of
against a reading of the sources done twice.

They are a **snapshot of the web sources on 10 September 2026**. Line numbers
and file paths were accurate then. Where a spec and the code disagree today,
the code wins: treat these as a map, not as a normative reference.

| Document | Covers | Used by |
|---|---|---|
| [`scene.md`](scene.md) | Scene setup, camera, room, axes, listener head, coordinate frames, edit gizmos, the hybrid iso shape | phase 1 |
| [`objects.md`](objects.md) | Object display modes, colours, level scaling, badges, labels, effective-render markers, picking | phase 1 |
| [`speakers.md`](speakers.md) | Speaker cubes and gauges, band colours, per-speaker heatmaps, the chunked gain-table transport | phase 1 |
| [`trails_volumes.md`](trails_volumes.md) | Object trails and the four ray-marched energy volumes, with their GLSL translated to WGSL | phase 1 |
| [`design.md`](design.md) | The visual system: colour, type and spacing tokens, layout geometry, every generic widget and its states, animation, i18n rules | phase 2 |
| [`host_contract.md`](host_contract.md) | Every event the Tauri host emits, the numbers it derives first, the connection and auto-start state machine, the `app` state object, the repaint scheduling, the log pipeline, the `get_state` snapshot | phase 2 |
| [`panels_left.md`](panels_left.md) | The left overlay: about and profiles, updates, OSC, audio input, fixed-channel sources, room geometry, display, DRC, objects and the pinned editors | phase 2 |
| [`panels_right.md`](panels_right.md) | The right overlay: the audio panel, the renderer panel, and the save footer, scene-effects bar and band cursor | phase 2 |

Each document ends with a section listing what its author could not determine
from the sources. Those are open questions, not settled facts.

The port itself is documented in
[`../studio-native-ui-spike.md`](../studio-native-ui-spike.md) (phase 0),
[`../studio-native-ui-phase1.md`](../studio-native-ui-phase1.md) (viewport) and
[`../studio-native-ui-phase2.md`](../studio-native-ui-phase2.md) (panels).
