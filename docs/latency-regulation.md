# Latency Regulation Algorithm

This document describes the current realtime output latency regulation used by `omniphony-renderer`, with emphasis on the shared control model and the backend-specific differences between `ASIO` and `PipeWire`.

## Goals

The latency controller has four jobs:

1. Keep the audible output close to a configured target latency.
2. Recover from low-buffer and high-buffer excursions without letting unstable audio leak through.
3. Support adaptive local resampling when enabled.
4. Report enough state to the UI so recovery behavior is observable.

The long-term control target is not "minimum latency". It is "stable latency near a requested setpoint, with predictable recovery behavior".

## Core Model

The shared regulation logic lives in [adaptive_runtime.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/audio_output/src/adaptive_runtime.rs).

### Domains

Two sample domains matter:

- Input domain: decoded/rendered samples written into the backend ring buffer.
- Output domain: samples actually consumed by the backend callback after local resampling.

Latency control is intentionally expressed in the input domain so the controller reasons about the same audio inventory regardless of output sample rate.

### Measured Quantities

For each callback, the backend computes:

- `available_input_samples`: current ring-buffer fill.
- `output_fifo_input_domain_samples`: local resampler FIFO converted back to input-domain samples.
- `callback_input_domain_samples`: callback size converted to input domain.
- `control_available`: `ring + output_fifo - callback/2`.
- `control_latency_ms`: `control_available / (sample_rate * channels)`.
- `measured_latency_ms`: `control_latency_ms + graph/backend latency estimate`.

`control_latency_ms` is the quantity used for regulation. `measured_latency_ms` is the user-facing total estimate.

### Target Fill

The target latency is converted to a target fill level:

- target fill = `target_latency_ms * input_sample_rate * channel_count / 1000`

This fill is the center of the controller.

## Shared Recovery State Machine

The recovery state machine exposes the UI states:

- `stable`
- `low-recover`
- `settling`
- `high-recover`

### Low Recovery

Low recovery is used when the buffer falls too far below target.

State progression:

1. `stable -> low-recover`
2. `low-recover -> settling`
3. `settling -> stable`

During `low-recover`, output is muted.

### Settling

`settling` exists to avoid reopening audio immediately after refill. The goal is to make the effective returned latency less random.

Current behavior:

- output remains muted
- if the buffer falls clearly too low again, go back to `low-recover`
- if the buffer is somewhat too high, trim while muted
- if the buffer stays inside the settling window long enough, transition to `stable`

Current exit timing:

- `200 ms` of accumulated stable callback time inside the settling window

Current settling half-window:

- `max(callback_input_domain_samples / 4, near_far_threshold_samples / 2)`

This means the settling window is no longer based only on callback size; it is also anchored to the configured `near/far` band.

### High Recovery

High recovery is used when the buffer is too far above target.

Behavior:

- aggressively discard buffered audio while muted
- return toward target faster than the slow servo path

## Near/Far Band Logic

The `near/far` band is derived from buffer error relative to target:

- `near` if `abs(control_available - target_fill) < near_far_threshold`
- `far` otherwise

This band is used both for UI and for determining whether far-mode actions are eligible.

The important distinction is:

- the band tells us whether we are near or far from target
- the recovery state tells us what the recovery machine is currently doing

These are related, but not the same thing.

## Adaptive Local Resampling

When adaptive resampling is enabled, a PI servo nudges the local resampling ratio around the base ratio.

Shared logic lives in:

- [lib.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/audio_output/src/lib.rs)
- [adaptive_runtime.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/audio_output/src/adaptive_runtime.rs)

Inputs:

- current control fill
- target fill
- configured gains `kp_near`, `ki`
- `max_adjust`
- `integral_discharge_ratio`

Outputs:

- effective local resampling ratio
- displayed rate-adjust value
- current adaptive band (`near` or `far`)

The PI loop is only one part of the system. It does not replace hard recovery. It attempts to keep the system centered before hard recovery becomes necessary.

## Startup Behavior

### ASIO

ASIO startup now reuses the normal low-recovery state machine instead of using a dedicated pre-fill gate.

Current startup flow:

1. stream starts muted in `low-recover`
2. refill runs using the same logic as ordinary low-buffer recovery
3. `settling` stabilizes the returned latency
4. transition to `stable`

Additionally, when startup recovery finishes, ASIO explicitly resets:

- the local resampler internal state
- the resampler FIFO

and keeps one extra callback muted before the first audible block. This is intended to avoid startup transients leaking out of the local resampler state.

### PipeWire

PipeWire does not use the same dedicated startup path. Its stream lifecycle and callback behavior are already driven by the PipeWire graph, so startup tends to be less dependent on a custom gate.

## ASIO / PipeWire Differences

This is the most important backend-specific section.

### 1. Callback Model

`ASIO`:

- callback size is determined by the driver/CPAL backend
- can be relatively coarse and backend-specific
- this makes threshold-based recovery more sensitive to callback granularity

`PipeWire`:

- callback cadence is tied to the graph quantum
- tends to be more regular
- makes settling and servo behavior easier to tune

### 2. Latency Measurement

`ASIO`:

- does not currently have a true backend graph-latency measurement
- uses a midpoint estimate based on callback size
- total displayed latency is therefore a model, not a direct driver-reported value

`PipeWire`:

- samples downstream graph latency via `pw_stream_get_time()`
- includes real graph scheduling delay in `measured_latency_ms`

This is why two backends can sound similarly stable while reporting different-looking latency numbers.

### 3. Non-Resampling Behavior

`ASIO`:

- without adaptive local resampling, it still relies on the shared far-mode recovery logic
- there is no separate backend-native servo equivalent to PipeWire's non-local-resampler path

`PipeWire`:

- has two regimes:
  - local resampler path
  - native backend rate/latency servo path when local resampling is not used

This makes PipeWire structurally more flexible, but also means the two backends are not exact mirrors.

### 4. Startup Strategy

`ASIO`:

- startup is now explicitly treated as low recovery
- mute/recovery/fade behavior is intentionally unified with ordinary low-buffer recovery

`PipeWire`:

- startup is more naturally absorbed into the backend callback lifecycle
- does not need the same dedicated startup forcing path

### 5. Sensitivity to Thresholds

`ASIO` is more sensitive to:

- settling window size
- refill/settling transition thresholds
- startup transient cleanup

`PipeWire` is more sensitive to:

- graph quantum
- backend latency reporting
- the split between local resampler control and native backend rate control

## Current Practical Interpretation

When debugging the system, interpret states as follows:

- `stable`: no active recovery state machine
- `low-recover`: output is muted because the system is rebuilding latency from below target
- `settling`: output is still muted while the system tries to return at a less random effective latency
- `high-recover`: buffered audio is being dropped because latency is too high
- `near` / `far`: distance from target, not mute state by itself

If audio is wrong, always inspect both:

- band: `near` / `far`
- state: `stable` / `low-recover` / `settling` / `high-recover`

The band explains where the controller is relative to target. The state explains what the recovery machine is actively doing.
