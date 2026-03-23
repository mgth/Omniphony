# Runtime Control Refactor

## Goal

Separate renderer-domain live control from audio-output-domain live control so the
project is easier to understand, safer to evolve, and easier to share with new
contributors.

This refactor is intentionally structural rather than minimal. The target is a
clean ownership model for runtime control, not a local patch around the current
`requested_adaptive_resampling_*` fields.

## Problems in the Current Design

- `renderer::live_params::RendererControl` mixes two domains:
  - spatial renderer live state
  - audio output runtime control
- adaptive resampling parameters are stored as a flat set of
  `requested_adaptive_resampling_*` fields in `RendererControl`, even though
  their true domain is `audio_output`
- `decode::handler` rebuilds `audio_output::AdaptiveResamplingConfig`
  field-by-field from `RendererControl`
- `renderer::osc_output` also rebuilds audio config field-by-field for:
  - OSC state broadcasting
  - config save
- requested state and applied state are not cleanly separated
- adding a new audio parameter requires edits in too many places

## Ownership Rules

- `renderer`
  - owns spatial rendering control
  - owns topology / VBAP / object and speaker live parameters
  - owns `ramp_mode`
- `audio_output`
  - owns output-device selection
  - owns output sample-rate request
  - owns latency target request
  - owns adaptive resampling enable/tuning
  - owns applied backend audio state
- `decode`
  - orchestrates runtime interaction between renderer and audio output
  - should not reconstruct audio config field-by-field if a structured config is
    already available

## Target Architecture

### `audio_output::control`

Introduce a new module:

- `omniphony-renderer/audio_output/src/control.rs`

Types:

- `OutputDeviceOption`
- `RequestedAudioOutputConfig`
- `AppliedAudioOutputState`
- `AudioControl`

### `RequestedAudioOutputConfig`

Fields:

- `output_device: Option<String>`
- `output_sample_rate_hz: Option<u32>`
- `latency_target_ms: Option<u32>`
- `adaptive_enabled: bool`
- `adaptive: AdaptiveResamplingConfig`

### `AppliedAudioOutputState`

Fields:

- `output_sample_rate_hz: Option<u32>`
- `sample_format: String`
- `audio_error: Option<String>`

Possible future extensions:

- `backend_name`
- `adaptive_band`
- `effective_ratio`

### `AudioControl`

Fields:

- `requested: Mutex<RequestedAudioOutputConfig>`
- `applied: Mutex<AppliedAudioOutputState>`
- `available_output_devices: Mutex<Vec<OutputDeviceOption>>`
- `device_list_fetcher: Mutex<Option<Box<dyn Fn() -> Vec<OutputDeviceOption> + Send + Sync>>>`

Responsibilities:

- own all audio runtime requests
- own all audio runtime applied state
- provide typed accessors and update helpers

## Renderer Side After Refactor

`renderer::live_params::RendererControl` should keep only renderer concerns:

- `live`
- `topology`
- layout / VBAP rebuild machinery
- dirty flags
- `config_path`
- `requested_ramp_mode`

It should no longer own:

- requested output device
- requested sample rate
- requested latency target
- requested adaptive resampling enable
- requested adaptive resampling tuning fields
- available output devices
- device list fetcher
- current audio sample rate
- current sample format
- current audio error

## Runtime Orchestration

Introduce a runtime-level container in decode:

- `RuntimeControl`

Fields:

- `renderer: Arc<RendererControl>`
- `audio: Arc<AudioControl>`

Purpose:

- keep renderer and audio control together at the process/session level
- avoid making `renderer` the owner of audio-domain state

## OSC Design After Refactor

### Short-Term Target

Keep the current OSC feature set, but route audio messages through `AudioControl`
instead of `RendererControl`.

### State Semantics

Make the distinction explicit:

- requested state
- applied state

Compatibility path:

- existing `/omniphony/state/...` addresses may continue to reflect requested
  values where Studio expects them
- internal code must still distinguish requested vs applied

Future path:

- add explicit `.../requested/...` and `.../applied/...` routes if needed

## Refactor Steps

1. Add `audio_output::control`
2. Move `OutputDeviceOption` there
3. Add `AudioControl`
4. Add `RuntimeControl` in decode
5. Initialize `AudioControl` from CLI/config defaults in `decode_impl`
6. Update `handler` to read audio requests from `AudioControl`
7. Update audio state reporting to write applied state to `AudioControl`
8. Update OSC control/state code to use `AudioControl` for audio routes
9. Update config save/load mapping to read/write audio config from `AudioControl`
10. Remove old audio fields from `RendererControl`
11. Validate build and behavior

## Notes

- `ramp_mode` stays in renderer by design
- `AdaptiveResamplingConfig` remains the single audio-domain config type
- the remaining architecture smell after this refactor may be that
  `renderer::osc_output` still contains runtime/audio OSC logic; that can be
  extracted later if needed, but it should no longer require audio state to live
  inside `RendererControl`
