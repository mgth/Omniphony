# Audio Input Plan

This document tracks the implementation plan for adding realtime audio input to
`omniphony-renderer`, starting with:

- Linux: a PipeWire input path exposed as an 8-channel sink in the graph
- object feeding from those 8 incoming channels
- fixed object positions derived from the standard `7.1` layout

The long-term branch scope also includes a Windows input path, but this plan is
intentionally centered on the Linux/PipeWire slice first.

## Goal

Add a new runtime path that:

1. creates a PipeWire endpoint that other applications can send audio to
2. receives 8 channels in realtime
3. treats each incoming channel as a fixed object source
4. positions those sources using the `7.1` layout asset
5. feeds the existing VBAP/object rendering path

Practical intent:

- external software sends `7.1` program audio to the PipeWire endpoint
- `orender` converts those 8 channels into 8 fixed-position objects
- the existing spatial renderer outputs them to the active speaker layout

## Scope Split

### Phase A: Linux / PipeWire input

Target outcome:

- a working PipeWire input endpoint on Linux
- 8-channel capture
- fixed object mapping using `layouts/7.1.yaml`

### Phase B: Windows input parity

Target outcome:

- an equivalent Windows capture/input path
- likely ASIO-based if a suitable multi-channel input device is present

This plan does not implement the Windows path yet; it identifies the shared
design constraints so the Linux work does not paint us into a corner.

## Current Relevant Code

- CLI surface:
  - [`src/cli/command.rs`](src/cli/command.rs)
- audio backend crate:
  - [`audio_output/src/lib.rs`](audio_output/src/lib.rs)
- Linux PipeWire output backend:
  - [`audio_output/src/pipewire.rs`](audio_output/src/pipewire.rs)
- decode/session state and spatial event handling:
  - [`src/cli/decode/state.rs`](src/cli/decode/state.rs)
  - [`src/cli/decode/spatial_metadata.rs`](src/cli/decode/spatial_metadata.rs)
  - [`src/cli/decode/sample_write.rs`](src/cli/decode/sample_write.rs)
- fixed-position helper logic based on layouts:
  - [`src/cli/decode/virtual_bed.rs`](src/cli/decode/virtual_bed.rs)
- reference layout asset:
  - [`../layouts/7.1.yaml`](../layouts/7.1.yaml)

## Working Design Direction

Prefer reusing the existing spatial renderer and event path instead of inventing
a parallel rendering pipeline.

That implies:

- the new input path should generate fixed `SpatialChannelEvent` values
- those events should be injected before `render_frame(...)`
- object positions should remain stable unless the source layout or mapping changes
- no bridge plugin should be required for this mode

## Proposed CLI Surface

The recommended direction is a dedicated subcommand:

```text
orender input-live [OPTIONS]
```

Reason:

- this mode is not bridge-driven
- it should not inherit mandatory decode-only arguments such as `INPUT` or `--bridge-path`
- it keeps validation logic simpler and avoids hidden interactions inside `render`

However, this is no longer sufficient on its own.

New requirement:

- Studio must be able to switch between the existing pipe/stream input mode and
  `input-live`
- Studio must be able to update relevant `input-live` parameters through OSC

This means the CLI shape must coexist with a runtime-selectable input source
model rather than being treated as a once-at-startup-only mode forever.

## PipeWire Bridge Follow-Up

The current `pipewire_bridge` mode has reached a useful diagnosis point:

- `mpv` can now discover and open the `omniphony` sink
- PipeWire format negotiation succeeds with `MediaSubtype::Iec958`
- `orender` fails during buffer allocation for the encoded sink
- the current implementation uses a virtual `pw_stream` as an `Audio/Sink`

This strongly suggests that the remaining limitation is not the codec or
discovery layer, but the PipeWire primitive used to expose the encoded input.

### Current observed behavior

- With a raw-PCM-like sink declaration:
  - `mpv` opened the sink but no buffers ever arrived
- With an explicit `ParamBuffers`:
  - PipeWire negotiated `Iec958` but rejected buffer allocation
  - errors seen:
    - `error alloc buffers: Invalid argument`
    - `error alloc buffers: Operation not supported`
- With the current more standard stream connect:
  - format negotiation still succeeds
  - `mpv` remains paused waiting for audio start
  - no useful bitstream reaches the bridge

### Working conclusion

The `pw_stream`-based virtual sink approach is likely sufficient for PCM live
input, but not for a virtual encoded `IEC958/TRUEHD` sink intended to receive
passthrough-style audio from another client.

Follow-up from the first replacement attempt:

- a minimal `pw_filter` backend compiles and publishes a node
- but in its current form it is still not accepted by `mpv` as a valid target
  and `mpv` falls back to `no target node available`
- this suggests that `pw_filter` by itself is not enough unless it is completed
  with the rest of the sink-facing publication details expected by PipeWire
- the likely final direction is now a true exported PipeWire node rather than a
  stream-shaped abstraction

The next implementation step should therefore move away from a simple
`pw_stream` sink for `pipewire_bridge` and instead expose a PipeWire node that
is closer to how the graph expects an encoded input port to behave.

## PipeWire Bridge Replacement Plan

### Goal

Replace the current `pipewire_bridge` `pw_stream` sink implementation with a
PipeWire node/filter design that can:

1. appear as a valid `Audio/Sink` target for `mpv`
2. negotiate `Iec958`/`TRUEHD`
3. allocate or receive buffers in a supported way
4. hand the captured bitstream to the existing bridge path

### Implementation direction

Phase 1 should stay inside `omniphony-renderer` and preserve the runtime/OSC/UI
surface already built. The change should be confined to the PipeWire backend
implementation behind `InputMode::PipewireBridge`.

Recommended direction:

- keep `InputMode::PipewireBridge`, OSC control, Studio UI, config persistence,
  and the bridge injection path unchanged
- replace only the backend used by
  [`src/cli/decode/live_input.rs`](src/cli/decode/live_input.rs)
- prefer a true PipeWire node/export path over a generic `pw_stream`
  pretending to be an encoded sink
- keep the current `pw_filter` attempt only as a short-lived exploration
  checkpoint, not as the presumed final design

### Development steps

1. Isolate the current `pipewire_bridge` backend code path.
   - Split the current `run_pipewire_bridge_capture_loop(...)` into:
     - runtime/bridge ingestion logic
     - PipeWire sink implementation
   - This makes it possible to swap the backend primitive without touching the
     `IEC61937 -> bridge -> DecoderMessage` path.

2. Add a dedicated backend abstraction for encoded bridge sinks.
   - Example internal shape:
     - `trait PipewireBridgeSource`
     - `fn run(self, tx, bridge, stop) -> Result<()>`
   - Start with the current `pw_stream` implementation as `LegacyPwStreamSink`
     to preserve a fallback while developing the replacement.

3. Implement a new PipeWire backend based on a node/filter/adapter primitive.
   - Publish the same user-facing node properties:
     - `node.name`
     - `node.description`
     - `iec958.codecs = [ "TRUEHD" ]`
     - `audio.position = [ FL FR C LFE SL SR RL RR ]`
     - `resample.disable = true`
     - `node.latency = ...`
   - Ensure the port format is advertised as encoded `Iec958`.

4. If `pw_filter` still fails to appear as a valid external sink target,
   replace it with an exported PipeWire node.
   - Investigate `pw_core_export` / `pw_impl_node`
   - Model the node explicitly as an encoded sink endpoint rather than a
     generic processing filter
   - Preserve the already extracted ingest runtime so only the PipeWire-facing
     backend changes

5. Reattach the existing bitstream ingest path.
   - Preserve:
     - `SpdifParser`
     - `RInputTransport::Iec61937`
     - bridge `push_packet(...)`
     - `DecoderMessage::AudioData(DecodedSource::Live, ...)`
   - Do not change the bridge contract as part of this backend swap.

6. Re-test with `mpv`.
   - Expected success criteria:
     - `mpv` no longer stalls in `paused`
     - `mpv` no longer reports `no target node available`
     - `orender` logs either `ingest idle` or `ingest`
     - if data arrives, `sync_buffers > 0`
     - if packets are reconstructed, `PipeWire bridge packet: ...`

7. Only after the new backend works, remove the legacy experiment.
   - Keep the old `pw_stream` path behind a short-lived internal toggle during
     development if needed
   - delete it once the replacement has proven stable

## Problem Areas To Watch

### PipeWire primitive mismatch

The key risk is that encoded passthrough-style input may require a different
kind of PipeWire object than the one used successfully for PCM capture.

Current evidence points to:

- `pw_stream`: discoverable but not viable for encoded sink buffering
- `pw_filter`: closer, but still not yet a valid external sink target as
  currently implemented
- likely endpoint: exported node implementation

### Buffer ownership model

The failing area so far is buffer allocation. The replacement must match the
buffer ownership model expected by PipeWire for an encoded sink target.

### Avoid regressions to PCM live input

The PCM `live` input path should remain on the simpler current mechanism unless
there is a demonstrated need to refactor both modes together.

### Logging discipline

Keep the current useful diagnostics while removing the retry spam once the new
backend is in place:

- format negotiated
- state changes
- process/buffer activity
- IEC61937 packet counts

## Immediate Next Actions

- extract the `pipewire_bridge` ingest logic from the current sink publication
  logic in `live_input.rs`
- introduce a backend-local abstraction so the PipeWire primitive can be swapped
  without touching the bridge decode path
- replace the exploratory `pw_filter` backend with a true exported PipeWire node
  if the filter cannot be completed into a valid sink target quickly
- preserve the existing `mpv`/Studio/runtime surface while iterating on the
  backend internals

### Proposed command

```text
orender input-live \
  --input-backend pipewire \
  --input-node omniphony_input_7_1 \
  --input-layout ../layouts/7.1.yaml \
  --input-channels 8 \
  --input-sample-rate 48000 \
  --speaker-layout ../layouts/7.1.4.yaml \
  --output-backend pipewire \
  --output-device omniphony_router \
  --enable-vbap
```

### Proposed option set

#### Core mode selection

- `input-live`
  - dedicated command for realtime input-driven rendering

#### Input backend selection

- `--input-backend <pipewire|asio>`
  - Linux phase A only needs `pipewire`
  - keeping `asio` in the public shape now helps avoid a Linux-only abstraction

#### Input endpoint identity

- `--input-node <NAME>`
  - PipeWire node name exposed to the graph
  - default proposal: `omniphony_input_7_1`

- `--input-description <LABEL>`
  - human-readable PipeWire node description
  - default proposal: `Omniphony 7.1 Input`

#### Input stream contract

- `--input-layout <LAYOUT>`
  - layout used to derive fixed source positions
  - default proposal for phase A: `../layouts/7.1.yaml`

- `--input-channels <N>`
  - default proposal: `8`
  - phase A should reject values other than `8`

- `--input-sample-rate <HZ>`
  - default proposal: `48000`
  - phase A can start as fixed-only even if the CLI shape looks generic

- `--input-format <f32|s16>`
  - default proposal: `f32`
  - may remain hidden/advanced initially if PipeWire negotiation is fixed in code

#### Mapping behavior

- `--input-map <PRESET>`
  - default proposal: `7.1-fixed`
  - reserves space for future modes such as `5.1-fixed`, `stereo-pair`, or custom maps

- `--input-lfe-mode <object|direct|drop>`
  - default proposal: `direct`
  - this is the cleanest way to surface the major semantic ambiguity instead of burying it

#### Existing output/render options that should remain valid

- `--speaker-layout`
- `--output-backend`
- `--output-device`
- `--latency-target-ms`
- `--pw-quantum`
- `--enable-vbap`
- `--osc`
- `--osc-host`
- `--osc-port`
- `--osc-rx-port`
- `--master-gain`
- `--room-ratio`
- VBAP-related tuning flags

#### Options that should not apply to `input-live`

- positional `INPUT`
- `--bridge-path`
- `--presentation`
- bridge/decode-specific options
- bed-conformance options unless a later hybrid design needs them

### Why this exact shape

- `input-live` makes the runtime mode explicit
- `input-backend` keeps Linux and Windows on the same conceptual surface
- `input-node` and `input-description` matter immediately for PipeWire UX
- `input-layout` cleanly separates source positions from output speaker layout
- `input-map` avoids baking "7.1 fixed objects" into every future code path
- `input-lfe-mode` exposes the biggest semantic decision instead of hiding it

### Rejected alternatives

#### Overloading `render`

Rejected because:

- `render` is currently bridge-centric
- making `--bridge-path` optional only in some sub-modes will complicate parsing and validation
- the user mental model becomes muddy

#### Hardcoding every phase-A choice with no CLI surface

Rejected because:

- node naming and output routing are user-facing operational concerns
- even the first Linux version needs a way to coexist with other graph nodes cleanly

## Recommended Phase-A Defaults

These should be the initial CLI defaults for Linux:

- command: `input-live`
- `--input-backend pipewire`
- `--input-node omniphony_input_7_1`
- `--input-description "Omniphony 7.1 Input"`
- `--input-layout ../layouts/7.1.yaml`
- `--input-channels 8`
- `--input-sample-rate 48000`
- `--input-format f32`
- `--input-map 7.1-fixed`
- `--input-lfe-mode direct`

## Runtime Mode Switching Requirement

This requirement is now explicit:

- the renderer must be able to switch between:
  - bridge/pipe-driven decode input
  - `input-live` PipeWire input
- the switch must be controllable from Studio
- the switch should not require ad-hoc CLI rebuilding by Studio

Implication:

- the internal runtime model should move toward a selectable `InputMode`
  abstraction even if the first user-visible entry point is still `input-live`

## Proposed Runtime Input Model

Introduce an internal source selection layer:

- `InputMode::Bridge`
- `InputMode::Live`

With a corresponding runtime-config shape:

- `RuntimeInputConfig`
  - `mode`
  - bridge-related parameters
  - live-input-related parameters

This lets us:

- start in one mode from CLI/config
- switch mode later via OSC + reload/restart
- keep Studio control aligned with a single config model

## OSC Control Requirements

Existing runtime control already supports:

- `/omniphony/control/reload_config`
- output device/sample-rate controls
- speaker editing and apply/reset

For `input-live`, the minimum additional OSC surface should be:

- `/omniphony/control/input/mode s <bridge|live>`
- `/omniphony/control/input/live/backend s <pipewire|asio>`
- `/omniphony/control/input/live/node s <name>`
- `/omniphony/control/input/live/description s <label>`
- `/omniphony/control/input/live/layout s <path-or-preset>`
- `/omniphony/control/input/live/channels i <count>`
- `/omniphony/control/input/live/sample_rate i <hz>`
- `/omniphony/control/input/live/map s <7.1-fixed>`
- `/omniphony/control/input/live/lfe_mode s <object|direct|drop>`
- `/omniphony/control/input/apply`

Recommended behavior:

- control messages stage requested changes in runtime/config state
- `/omniphony/control/input/apply` triggers a controlled restart of the active
  input path only
- `/omniphony/control/reload_config` remains the full "reload everything" path

Why not apply every change immediately:

- input backend recreation is disruptive
- PipeWire node teardown/recreation should be atomic from the user's point of view
- Studio benefits from a staged-edit then apply workflow, like speaker edits already do

## CLI / OSC Relationship

Recommended model:

- CLI/config defines initial input mode and defaults
- OSC can override the live runtime state
- `save_config` persists the currently requested input mode and parameters

This matches the existing renderer-control direction better than treating
`input-live` as a one-shot isolated command with no runtime configurability.

## Validation Rules

The command should reject at parse/config resolution time when:

- `--input-backend pipewire` is used on non-Linux builds
- `--input-backend asio` is used on non-Windows builds
- `--input-layout` is missing
- `--speaker-layout` is missing when VBAP output is required
- `--input-channels` is not `8` in phase A
- `--enable-vbap` is disabled while the selected mapping mode requires object rendering

## Proposed Internal Types

These are the minimum new config concepts suggested by the CLI shape:

- `InputBackend`
  - `Pipewire`
  - `Asio`

- `InputMapMode`
  - `SevenOneFixed`

- `InputLfeMode`
  - `Object`
  - `Direct`
  - `Drop`

- `InputLiveArgs`
  - dedicated CLI/config struct for the `input-live` mode

- `InputMode`
  - `Bridge`
  - `Live`

- `RuntimeInputConfig`
  - current/desired input mode and parameters

- `InputControl`
  - runtime control surface analogous to existing audio output control

## Current Recommendation

Implement this exact surface first:

- add `Commands::InputLive(InputLiveArgs)`
- Linux-only backend implementation for `--input-backend pipewire`
- keep `--input-map` limited to `7.1-fixed`
- keep `--input-channels` effectively fixed to `8`
- default `--input-lfe-mode` to `direct`

But design the internal configuration so that:

- `render`/bridge mode and `input-live` mode are both representable in a shared
  runtime input config
- OSC can switch between them later without redesigning the config model

## Functional Breakdown

### 1. Add a realtime input mode

Tasks:

- define how the new mode is selected from CLI/config
- ensure bridge-specific requirements are disabled in this mode
- define which existing render options still apply
- define how runtime source switching interacts with CLI startup mode

Decision proposal:

- new command: `input-live`

Additional requirement:

- mirror this command into a runtime-selectable `InputMode::Live`

### 2. Implement PipeWire input backend

Tasks:

- add a PipeWire capture/input module in `audio_output`
- create a node visible in PipeWire with 8 channels
- receive interleaved or deinterleaved audio safely
- expose a pull or callback interface usable by the main runtime

Likely design:

- mirror parts of `audio_output/src/pipewire.rs`
- create a dedicated reader/capture type instead of stretching the writer

Open questions:

- should the PipeWire node be an `Audio/Sink` target for apps to play into,
  or an `Audio/Source` that applications record from
- exact PipeWire media class and properties required for the desired routing UX
- whether the endpoint should always be named predictably

### 3. Define channel-to-object mapping

Tasks:

- load `layouts/7.1.yaml`
- map incoming channel indices to fixed object positions
- assign stable logical IDs per channel
- define stable names for OSC/telemetry

Proposed default mapping:

- channel 0 -> `FL`
- channel 1 -> `FR`
- channel 2 -> `C`
- channel 3 -> `LFE`
- channel 4 -> `SL`
- channel 5 -> `SR`
- channel 6 -> `BL`
- channel 7 -> `BR`

Open questions:

- should `LFE` become a fixed object like the others, or stay direct/non-spatial
- if `LFE` is non-spatial, should the mode still claim 8 objects or 7 objects + 1 bed/direct path

### 4. Feed fixed events into the renderer

Tasks:

- create the fixed-position event list once after startup or layout change
- reuse it for every audio block
- bind each incoming channel to the corresponding fixed object position
- ensure channel count and event count stay aligned

Important:

- the fixed-event list should not be rebuilt per callback
- allocations must stay off the hot path

### 5. Integrate with the existing output path

Tasks:

- send captured audio blocks into `render_frame(...)`
- preserve the current audio output backends
- preserve OSC metering and object-state output where practical

Open question:

- whether the input path should share the current `SampleWriteCoordinator`, or get
  a smaller dedicated coordinator for live input

Additional requirement:

- mode switching from Studio should restart only the affected input pipeline when possible

### 6. Device and format handling

Tasks:

- lock the PipeWire input format for phase A
- choose sample format and rate for the capture side
- define resampling behavior if the input rate differs from the render/output rate

Current recommended starting point:

- 8 channels
- `f32` or `s16` depending on PipeWire callback ergonomics
- 48 kHz fixed for the first implementation

Open questions:

- fixed 48 kHz vs negotiated sample rate
- whether input-side adaptive resampling is needed immediately
- how to handle partial or malformed channel layouts from PipeWire peers

## Main Technical Problems

### PipeWire node semantics

The request says "create a sink PipeWire en entrée".

That is conceptually subtle:

- if other apps must play audio into our endpoint, they generally expect us to
  appear as a sink in the graph
- internally, however, our code is implementing audio capture/ingest

This must be nailed down before coding the backend properties.

### Separation from bridge-driven decode

The current runtime assumes a bridge-fed decode path producing `RDecodedFrame`.

The new live input path is different:

- no compressed bitstream
- no bridge plugin
- direct audio block ingestion

We need a clean entry point rather than forcing fake decoded frames through the
wrong abstraction layer.

### Fixed objects vs bed/direct channels

Using `7.1` speaker positions as fixed object locations is straightforward for
the seven full-range speakers.

`LFE` is the problematic channel:

- the layout assets now mark `LFE` as non-spatialized
- treating it as a normal object is semantically questionable
- treating it as a direct speaker path complicates a "8 channels -> 8 objects" design

This decision changes the render graph and OSC semantics.

### Latency model

Realtime input introduces a new latency chain:

- source app -> PipeWire graph -> input callback -> object renderer -> output backend

Questions:

- do we need an input ring buffer separate from the output ring buffer
- where do we want the latency target to live
- how much buffering is acceptable before object motion and monitoring feel wrong

### Cross-platform API shape

Linux wants PipeWire now, Windows later wants ASIO.

If we shape the abstraction badly now, the Windows work will fork the runtime.

The abstraction should probably be:

- `AudioInputBackend`
- `read_block(...)` or callback-driven ingestion
- fixed channel count / sample rate description

instead of "PipeWire-only logic embedded in Linux CLI code".

## Unknowns To Clarify

### 1. Exact PipeWire graph object to create

Need clarification:

- should `orender` expose itself as a PipeWire sink node that other apps select as playback destination
- or should it create a source/monitor-style capture endpoint

This is the most important Linux-specific unknown.

### 2. LFE policy

Need clarification:

- does channel 4 (`LFE`) become a fixed object
- or is it routed directly/non-spatially

### 3. Command-line UX

Proposed answer:

- new command `input-live`

Remaining question:

- whether phase A ships with the full option set or with a trimmed subset plus defaults

Additional requirement:

- the same conceptual parameters must exist in OSC/runtime control, not only in CLI

### 4. Runtime control / OSC UX

Need clarification:

- should input mode changes apply immediately or require `/omniphony/control/input/apply`
- should `save_config` persist staged input settings automatically or only after apply
- should Studio switch modes by writing config + `reload_config`, or by dedicated input control messages

Current recommendation:

- staged OSC edits + explicit `/omniphony/control/input/apply`
- keep `/omniphony/control/reload_config` as the coarse full-restart fallback

### 4. Input format contract

Need clarification:

- fixed 48 kHz, 8 channels
- or negotiated rate/channels with validation

### 5. Output combination

Need clarification:

- should this mode always require an output backend
- or can it run metadata-only / OSC-only

### 6. Windows parity target

Need clarification:

- is the Windows goal also fixed 8-channel input first
- and must it mirror Linux behavior exactly at feature level

## Implementation Plan

### Phase 1: Design freeze

Tasks:

- confirm PipeWire endpoint semantics
- confirm LFE policy
- confirm CLI shape
- confirm fixed 7.1 mapping

Exit criteria:

- one approved runtime shape for Linux phase A

### Phase 2: Backend abstraction

Tasks:

- introduce an input-backend abstraction
- keep platform-specific code inside `audio_output`
- avoid coupling input ingestion to bridge code

Exit criteria:

- runtime can host an input backend without bridge involvement

### Phase 3: PipeWire input implementation

Tasks:

- add Linux PipeWire input backend
- create the 8-channel endpoint
- capture audio blocks reliably
- expose device/name/config controls

Exit criteria:

- audio blocks are received from PipeWire into the runtime

### Phase 4: Fixed object injection

Tasks:

- load `7.1.yaml`
- build fixed object/event mapping
- feed incoming channels into the existing spatial renderer

Exit criteria:

- incoming 8-channel audio renders as fixed-position sources

### Phase 5: CLI, config, and telemetry

Tasks:

- add CLI/config support
- define logs and status messages
- wire OSC/object naming if desired

Exit criteria:

- mode is usable without code changes

### Phase 6: Validation

Tasks:

- test with a real PipeWire client playing 8-channel audio
- verify positions match the `7.1` layout
- measure latency and callback stability
- verify behavior when fewer/more than 8 channels are sent

Exit criteria:

- Linux input path is operational and predictable

## Immediate Next Actions

These can start right now without resolving every unknown.

- decide where the new mode lives in the CLI surface
- sketch an `AudioInputBackend` trait and the minimum runtime state it needs
- split shared PipeWire utility code out of `audio_output/src/pipewire.rs` if needed
- define the fixed 7.1 channel-index mapping in code and document it
- identify the cleanest insertion point for fixed `SpatialChannelEvent` values
- prototype a minimal Linux PipeWire capture backend returning 8-channel blocks

## Decision Log

- Date:
- Topic:
- Decision:
- Impact:
