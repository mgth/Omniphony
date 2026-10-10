# Documentation

What each page is for. The contracts and design pages stay at the top of
`docs/`, where the code, the tests and the tooling cite them by path; what no
longer describes the code is under [`archive/`](archive/).

New pages are written in English.

## Guides: using Omniphony

| Page | For |
|---|---|
| [install/linux.md](install/linux.md), [install/windows.md](install/windows.md), [install/macos.md](install/macos.md) | From nothing to a film playing, per OS |
| [mpv-omniphony.md](mpv-omniphony.md) | The player: options, OSC, overlay |
| [placement.md](placement.md) | Where fixed channels go: Sphere, Room, Manual |
| [channel-render-modes.md](channel-render-modes.md) | How fixed-channel sources are processed |
| [phantom-extraction-methods.md](phantom-extraction-methods.md) | The phantom-extraction pre-stage and its two methods |
| [config-profiles.md](config-profiles.md) | Named configuration profiles |
| [option-surface-parity.md](option-surface-parity.md) | Which option can be set from the CLI, Studio and mpv |

Building the engine from source: [`../omniphony-renderer/QUICKSTART.md`](../omniphony-renderer/QUICKSTART.md).

## Design: contracts and architecture

Contracts held by tests; change the page and the test together.

| Page | Contract or design |
|---|---|
| [osc-control-contract.md](osc-control-contract.md) | Every OSC control and state address |
| [persistence-policy.md](persistence-policy.md) | What is saved when, and through what |
| [channel-object-contract.md](channel-object-contract.md) | One channel/object contract from bridge to host |
| [plugin-contract.md](plugin-contract.md) | Render-backend plugins |
| [custom-render-backend-integration.md](custom-render-backend-integration.md) | Writing a render backend |
| [volumetric-backend.md](volumetric-backend.md) | The volumetric backend: VBAP with the object's depth |
| [live-options-registry.md](live-options-registry.md) | The declared registry of live options |
| [from-file-evaluator-architecture.md](from-file-evaluator-architecture.md) | Serialized evaluation artifacts |
| [latency-regulation.md](latency-regulation.md) | The output latency controller |
| [iamf-matroska-mapping.md](iamf-matroska-mapping.md) | Draft IAMF-in-Matroska codec mapping |
| [iamf-transport-plan.md](iamf-transport-plan.md) | Proposal: raw OBU stream over the Raw transport and a private IEC 61937 encapsulation for IAMF |
| [dsp-validation-report.md](dsp-validation-report.md), [superpowers/](superpowers/) | DSP validation harness: design and measurements |
| [studio-egui-boundary-plan.md](studio-egui-boundary-plan.md) | The native Studio's core/UI boundary |
| [studio-native-ui-specs/](studio-native-ui-specs/) | The native Studio's UI, panel by panel |
| [studio-native-completion.md](studio-native-completion.md), [studio-native-acceptance.md](studio-native-acceptance.md), [studio-native-manual-validation.md](studio-native-manual-validation.md) | Native Studio acceptance plan, evidence and procedure |

Engine-side references live next to the code: the bridge ABI
([`BRIDGE_API.md`](../omniphony-renderer/BRIDGE_API.md)), the C ABI
([`ABI.md`](../omniphony-renderer/ABI.md)), OSC sessions
([`OSC_PROTOCOL.md`](../omniphony-renderer/OSC_PROTOCOL.md)) and binaural output
([`BINAURAL.md`](../omniphony-renderer/BINAURAL.md)).

## Process

| Page | For |
|---|---|
| [release-process.md](release-process.md) | Cutting and publishing a release |

## Archive

[`archive/`](archive/) keeps plans, investigations and reports that describe
the code as it was: the audio-input plan, the PipeWire bridge investigation,
the runtime-control refactor plan, the DAC latency sawtooth report, the native
Studio's spike and phase plans, the Tauri/WebGL notes, and the early design
notes written in French (gain algorithm, crossover, ramp strategy, render
backend and evaluation refactors, Studio/orender state). They are not kept up
to date; read them for the reasoning, not as a description of the code.
