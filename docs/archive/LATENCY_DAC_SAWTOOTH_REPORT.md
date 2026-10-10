# DAC Latency Sawtooth Report

Date: 2026-05-14

## Summary

Current symptom in `clock_mode=dac`:

- output latency still shows an extremely regular sawtooth
- amplitude observed: about `16 ms`
- period observed: about `1 s`
- after several measurement fixes, `latency_downstream/path` stays at `0 ms`
- `latency_control/ctrl` and `latency_instant/raw` still move together
- an additional fixed offset of about `128 ms` was observed and was not removed by the latest attempted fixes

This means the remaining problem is not a vague stability issue. It behaves like a deterministic accounting or scheduling error tied to callback quanta, buffer domains, or a discrete periodic control action.

## Initial Context

The original concern was that a previous latency-stabilization effort appeared to have been lost. That was checked against Git history and was not the case.

Relevant facts:

- tag `v0.2.4` is commit `c3638de`
- the stabilization merge after that tag is commit `b16eb6d`
- the post-`v0.2.4` latency-measurement refactor is still present in `main`

Important commits still present:

- `9a2929c` `fix(audio): separate control and measured latency`
- `70eeaec` `refactor: align latency telemetry semantics`
- `ee11e2f` `fix: account for in-flight resampler latency`
- `214a4ee` `fix: publish post-recovery latency metrics`

Conclusion from that check:

- the previous latency-measurement refactor was not lost
- the current regression must come from later behavior or from a path that was not covered the way we assumed

## Backend Findings

The active input backend paths were checked.

Observed backend split:

- `clock_mode=upstream` uses `PwClientNode`
- `clock_mode=pipewire` uses `PwStream`
- `clock_mode=dac` uses `PwStream`

This matters because:

- `upstream` does not exercise the same scheduling path as `dac`
- the current issue reproduces in `dac`
- `pipewire` was also problematic during the investigation, but the focus later narrowed to `dac`

Runtime visibility was added so Studio can expose the effective PipeWire backend and clock strategy instead of only the requested high-level clock mode.

## What Was Tried

### 1. PipeWire output latency display smoothing

Files changed:

- `omniphony-renderer/audio_output/src/adaptive_runtime.rs`
- `omniphony-renderer/audio_output/src/pipewire.rs`

Intent:

- harden `low-recover/settling`
- separate displayed latency from control latency
- reduce false oscillation caused by the displayed metric

Result:

- did not remove the deterministic sawtooth
- later evidence showed the remaining problem was not only display smoothing

### 2. Check whether the old latency refactor still exists

Intent:

- verify whether the previously merged latency-measurement overhaul had been lost

Result:

- confirmed still present in Git history and current code
- ruled out “lost merge” as the explanation

### 3. Remove `PwStream` callback accumulation on encoded input

Files changed:

- `omniphony-renderer/src/cli/decode/live_input.rs`
- `omniphony-renderer/audio_input/src/pipewire.rs`

Intent:

- reduce artificial burstiness by changing `PW_STREAM_ACCUMULATE_CALLBACKS` from `4` to `1`
- avoid re-bursting IEC61937 input when the parser already buffers fragments internally

Result:

- plausible cleanup for capture cadence
- did not remove the DAC sawtooth

### 4. Expose effective PipeWire backend and clock strategy to runtime/UI

Files changed:

- `omniphony-renderer/audio_input/src/control.rs`
- `omniphony-renderer/src/cli/decode/live_input.rs`
- `omniphony-renderer/runtime_control/src/osc.rs`
- `omniphony-renderer/runtime_control/src/snapshot.rs`
- `omniphony-studio/src/state.js`
- `omniphony-studio/src/init.js`
- `omniphony-studio/src/controls/input.js`

Intent:

- distinguish requested mode from real backend path
- avoid diagnosing the wrong runtime path

Result:

- useful for diagnosis
- not itself a fix for the sawtooth

### 5. PipeWire graph-driven recabling attempt

File changed temporarily:

- `omniphony-renderer/audio_input/src/pipewire.rs`

Intent:

- make `clock_mode=pipewire` stop using the same DRIVER scheduling path as `dac`

Result:

- broke `pipewire` badly, including extremely slow video playback
- reverted to the previous DRIVER behavior
- useful only insofar as it showed that this path was not viable in current form

### 6. Reduce idle polling delay in direct-trigger DAC capture loop

Files changed:

- `omniphony-renderer/audio_input/src/pipewire.rs`
- `omniphony-renderer/src/cli/decode/live_input.rs`

Intent:

- remove up to `20 ms` of mainloop sleep before draining pending output-driven triggers
- avoid scheduling jitter from coarse polling in DAC direct-trigger mode

Result:

- reasonable correction for responsiveness
- did not remove the regular sawtooth

### 7. Slow and deadband the `output_rate_adjust -> input DRIVER interval` feedback

File changed:

- `omniphony-renderer/audio_input/src/pipewire.rs`

Intent:

- avoid coupling two fast control loops
- keep long-term drift correction but suppress short-term reinjection of output servo motion into input cadence

Result:

- did not eliminate the current deterministic DAC sawtooth

### 8. Revisit output latency semantics: make `raw` actually raw

File changed:

- `omniphony-renderer/audio_output/src/pipewire.rs`

Intent:

- remove UI-facing smoothing from `latency_instant/raw`
- sample PipeWire graph delay more often to avoid a fake `~1 s` telemetry alias

Result:

- `latency_downstream/path` ended up observed as `0`
- `ctrl` and `raw` remained equal
- therefore the remaining sawtooth is not coming from downstream graph delay

### 9. Separate control latency from midpoint-of-callback measurement

Files changed:

- `omniphony-renderer/audio_output/src/adaptive_runtime.rs`
- `omniphony-renderer/audio_output/src/pipewire.rs`
- `omniphony-renderer/audio_output/src/asio.rs`

Intent:

- stop using `callback_input_domain_samples / 2` as part of the control quantity
- keep midpoint compensation only in the measured quantity

Result:

- after this change, `ctrl` and `raw` still moved together in the user-observed DAC case
- a fixed `128 ms` offset also remained
- therefore either:
  - the midpoint correction was not the dominant cause in this path, or
  - another callback-size or domain error still dominates the signal

### 10. Treat PipeWire output callback size as requested quantum instead of mapped capacity

File changed:

- `omniphony-renderer/audio_output/src/pipewire.rs`

Intent:

- stop using `slice.len()` as if it were the real callback block size
- use `pw_stream_get_time().size` instead

Reasoning:

- mapped buffer capacity can be much larger than the active cycle size
- that would corrupt latency accounting and DAC trigger ratios

Result:

- user reported no change

### 11. Convert `pw_time.size` from interleaved samples to frames

File changed:

- `omniphony-renderer/audio_output/src/pipewire.rs`

Intent:

- compensate for the fact that PipeWire reports interleaved raw-audio size in samples, not frames
- with `8` channels, this kind of mistake naturally creates an apparent `8x` inflation, matching the observed `16 ms -> 128 ms` clue

Result:

- user reported no change
- this implies the `128 ms` offset is not explained solely by the `pw_time.size` interpretation in that callback path

## What We Know Now

Based on the latest user observations:

- `latency_downstream/path = 0`
- `latency_control/ctrl` and `latency_instant/raw` are both still sawtoothing
- the sawtooth remains highly regular
- sawtooth amplitude is exactly about `16 ms`
- an additional offset of about `128 ms` is still present

These observations strongly suggest:

- the issue is inside the control-latency accounting or the real controlled buffer level
- the issue is not primarily in downstream PipeWire graph delay
- the issue is likely quantized by a callback/block size or a periodic trigger cadence
- the problem remains deterministic enough that it should be debuggable from exact sample-domain accounting

## Why The Problem Is Still Open

Several plausible causes were eliminated or weakened:

- lost merge / lost stabilization work: ruled out
- downstream graph latency wobble: not supported by current `path = 0`
- simple UI smoothing artifact: not supported anymore
- coarse DAC trigger polling: insufficient to explain the remaining exact periodicity
- straightforward `pw_time.size` capacity-vs-quantum bug in the tested callback path: attempted fix showed no user-visible change

The remaining likely categories are:

1. the control buffer itself is genuinely oscillating with a callback-sized amplitude
2. another discrete quantity is still being interpreted in the wrong domain
3. DAC direct-trigger scheduling is periodically injecting one exact extra/missing quantum
4. a projected/post-recovery control metric is still being republished as if it were the steady-state metric

## Recommended Next Investigation Steps

The next pass should be measurement-first and should avoid more speculative “stability” changes until the exact oscillating quantity is identified.

Recommended next steps:

1. Log exact sample-domain quantities at fixed cadence in DAC mode:
   - ring buffer length
   - output FIFO input-domain samples
   - pending resampler input samples
   - callback input-domain samples
   - control available
   - projected control available
   - direct-trigger pending counter
   - observed capture quantum
   - output callback quantum

2. Log these with exact integer sample counts, not only milliseconds.

3. Correlate one full sawtooth period and verify which integer quantity moves by the amount corresponding to `16 ms`.

4. Check whether the `128 ms` offset equals:
   - `8 ×` a `16 ms` quantum
   - one PipeWire mapped block
   - one internal buffer target unit
   - one resampler chunk-domain conversion mistake

5. Verify whether the DAC direct-trigger loop fires an extra or missing trigger at roughly one-second cadence.

6. Confirm whether the sawtooth is in the true ring buffer level or only in the computed `control_available`.

## Validation Performed

The code changes above were repeatedly validated with:

- `cargo fmt --manifest-path /home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/Cargo.toml`
- `cargo check --manifest-path /home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/Cargo.toml -p omniphony-renderer`

This confirms the attempted fixes build, but runtime behavior is still incorrect.

## Current Status

Status: unresolved

The problem is currently best described as:

- a deterministic DAC-mode latency sawtooth
- approximately `16 ms` amplitude
- approximately `1 s` period
- plus a persistent fixed offset around `128 ms`
- with `ctrl` and `raw` still moving together
- and no downstream/path contribution visible

---

## Session 2026-05-17 — additional findings

### Mode-switch investigation

Confirmed by direct A/B test on the same hardware:

- `clock_mode=dac`: no drift, but the historical sawtooth.
- `clock_mode=pipewire` (default at the time of this session) **with IEC958
  bridge (TRUEHD/EAC3 source)**: drift of `−55_000 ppm` AND a slow latency
  oscillation that the operator originally perceived as `0.5 Hz` /
  `30–40 ms` peak-to-peak on the smoothed control signal.
- `clock_mode=pipewire` **with PCM source** (Live mode, virtual sink
  `omniphony_input_7_1`): NEITHER drift NOR oscillation. The latency
  becomes rock-stable and `rate_adjust_ppm` sits at `0`.

The drift is therefore a real, expected consequence of the SPDIF source
clock running independently from the PipeWire output clock — it is not a
bug, the PI must compensate it in steady state. The oscillation, on the
other hand, is specific to the IEC958 bridge chain and remains the open
question.

A separate Studio bug was identified while preparing the PCM test: the
input-mode dropdown silently reverts from `pipewire` to `pipewire_bridge`
on `Apply`. Filed as <https://github.com/mgth/Omniphony/issues/11>. Direct
config-file edit (`~/.config/omniphony/config.yaml`,
`input_mode: pipewire`, `live_input.node: omniphony_input_7_1`) was used
as a workaround for the test.

### Generic diagnostic infrastructure built during this session

To narrow down where the oscillation enters the pipeline, a generic
diagnostic registry was added so any producer can publish a named metric
to the Studio diag plot in 2 lines of Rust with no other plumbing change.

- `sys/src/diag.rs` (since moved to its own `diag` crate,
  `diag/src/lib.rs`) defines `DiagRegistry` (name → `Arc<AtomicU64>` of
  `f64` bits) and `DiagAtomicHandle` (bundle a pre-allocated atomic with
  its schema for `register_external`).
- `register_external` repoints the entry when the supplied `Arc` differs
  from the registered one — necessary so a freshly-constructed
  `PipewireWriter` reattaches its new atomics to the registry instead of
  leaving the schema pointed at the dead atomics of the previous writer
  (this caused a subtle bug where new metrics appeared frozen until the
  fix).
- `audio_input::InputControl` owns the registry; `PipewireWriter`
  exposes its diag atomics via `diag_atomic_handles()`; `sample_write.rs`
  hooks the writer's handles into the registry per meter bundle
  (idempotent, no-op when the `Arc` already matches).
- OSC: `/omniphony/state/diag_schema` (JSON, only sent when set of
  registered metrics changes is reflected) and `/omniphony/state/diag_values`
  (JSON map of `name → f64`, sent each meter bundle).
- Tauri parses these as `OscEvent::StateDiagSchema` /
  `StateDiagValues` and forwards them as events the JS bridge parses
  into `app.diagSchema` / `app.diagValues`.
- Studio: `controls/diag-plot.js` polls the values at 10 Hz, with a
  multi-select chip row grouped by metric `group`, per-metric auto-
  scaled stacked panels, selection persisted to `localStorage`.

The previous fixed-purpose `iec958-plot.js` was removed in favour of this
generic plot, which now hosts all subsequent investigation metrics.

### Per-stage measurements (groups visible in the diag plot)

`iec958` group (PipeWire IEC958 capture path):
- `iec958_chunk_bytes`, `iec958_chunk_dt_us`
- `iec958_decode_packets`, `iec958_decode_dt_us` (SPDIF parser side)

`bridge` group (harletty plugin output):
- `bridge_frame_samples`, `bridge_frame_dt_us`, `bridge_frame_count`

`decoder` group (decoder thread → ring):
- `decoder_frame_dt_us`, `decoder_queue_lag_us`

`output` group (audio output backend):
- `output_callback_dt_us`, `output_callback_frames`
- `output_effective_ratio_ppm`
- `output_resampler_in_per_cb`, `output_resampler_out_per_cb` (0-filtered
  so only the callbacks that actually invoke the resampler are kept)
- `output_ring_input_samples` (raw ring level sampled at callback rate)
- `write_samples_dt_us`, `write_samples_count` (per write call)
- `write_rate_sps`, `pop_rate_sps` (per-event EMA throughput)
- `runtime_state_code` (adaptive state machine: 0=stable, 1=low-recover,
  2=settling, 3=high-recover)
- `recovery_discard_count` (cumulative samples discarded by ANY recovery
  path — confirmed always 0 in the user's setup)
- `cumulative_written_input_samples`, `cumulative_drained_input_samples`,
  `cumulative_flow_control_available` (the new PI input signal — see
  below)
- `writes_per_250ms` (sliding window + EMA — see below)

A sentinel metric `_diag_alive = 1` is registered in `InputControl::new`
so the chain liveness can always be confirmed even before any audio path
publishes anything.

### What is uniform across all per-event measurements

With the source active (TRUEHD over SPDIF, `clock_mode=pipewire`) and the
PI either active or paused:

- chunk arrival cadence and chunk size (PipeWire capture) — uniform
- SPDIF parser output (packets per call, inter-call dt) — uniform
- harletty plugin frame output — `bridge_frame_count` increments
  smoothly; `bridge_frame_samples` constant (`40` per frame); the
  `bridge_frame_dt_us` metric only became readable after filtering
  intra-batch sub-millisecond values (the plugin emits multiple frames
  per `push_packet` in a tight loop)
- decoder thread mpsc dt and queue lag — uniform
- output callback dt — uniform (300 µs noise around the quantum)
- output callback frames per call — uniform
- effective resample ratio — constant when PI paused (freeze verified)
- recovery state — stays `stable` (0), `recovery_discard_count` stays at
  `0` (the user explicitly tuned `low_recover_*` thresholds so the state
  machine cannot fire on short timescales)

### What does oscillate

- `output_ring_input_samples` (ring level sampled at callback rate)
  shows a real `~1 Hz` pattern with `30–40 ms` peak-to-peak amplitude
- `pop_rate_sps` (EMA of resampler-invocation rate) shows a clear
  `~0.4 Hz` oscillation
- `cumulative_flow_control_available = cumulative_written -
  cumulative_drained` (the new flow-counter PI input) shows the same
  `~0.4 Hz` oscillation — even though both counter slopes APPEAR
  constant and identical at the cumulative scale

Critical observation: the oscillation **persists with PI paused**. So
this is not a closed-loop PI instability — it is a real system dynamic
in the open-loop measurement.

### Cancellation attempts (in chronological order)

1. **Replaced the raw `output_fifo` level by its expected steady-state
   mean** (= `chunk_input_samples / 2` in input domain) in the
   `control_available` calculation. The FIFO is structurally a sawtooth
   between `0` and one chunk's worth of samples; its long-term mean is
   constant in steady state. Implemented in `audio_output/src/pipewire.rs`
   and `audio_output/src/asio.rs`. Outcome: the latency gauge still
   showed the sawtooth — the input ring still contributes its own
   chunk-induced wobble.

2. **Switched to a cumulative-flow signal for the PI**
   (`cumulative_written - cumulative_drained`, both as monotonic u64
   counters incremented per write-event and per callback in input
   domain). The intent was a chunk-noise-free signal by construction.
   Outcome: the new signal still oscillates at `~0.4 Hz`. Confirmed
   through dedicated `cumulative_written_diag` and
   `cumulative_drained_diag` snapshot metrics that both counters do
   grow, and the diff is genuinely modulated.

### Where the oscillation comes from (current hypothesis)

By elimination, with PI paused:
- the drain counter is incremented by `callback_input_domain_samples`
  per callback, both of which are uniform → drained grows at strictly
  constant rate
- the write counter is incremented per write event by `samples.len()`,
  with per-event `samples.len()` uniform and per-event dt uniform on
  short scales (the EMA `write_rate_sps` is stable)
- but the bridge plugin emits frames in BATCHES per `push_packet` call
  (visible in the intra-batch sub-ms `bridge_frame_dt_us`), so writes
  arrive in bursts

The sliding-window metric `writes_per_250ms` was added on the writer
thread to expose any slow modulation of the write count per fixed
window. It immediately showed a strong two-mode alternation (high/low
every other publication) caused by the bridge plugin's two-phase
batching (N vs N+1 frames per `push_packet`).

Three display-side smoothing attempts were tried on this metric to
expose any slow trend underneath the alternation:
1. fixed 250 ms windows — bi-modal alternation visible
2. sliding 250 ms window updated per event — same
3. throttled publication (50 ms cadence) + EMA on top of the sliding sum
   at `α = 0.05` then `α = 0.005` (τ = 10 s, kills any signal up to and
   including the 0.4 Hz we are hunting)

As of the end of this session, the bi-modal alternation in
`writes_per_250ms` is **still visible despite the τ = 10 s EMA**, which
should mathematically suppress any sub-Hz oscillation by ~95 %.

**This either means the most recent code is not in the active binary, or
there is a bug in the smoothing implementation that the next session
should pin down before drawing further conclusions.** The smoothing code
lives in `audio_output/src/pipewire.rs` near the `write_samples` method
(fields `write_window_events`, `write_window_sum`,
`write_window_last_publish_at`, `write_window_ema`).

### Bridge plugin output characteristics

The harletty bridge plugin's emission pattern is the most suspicious
upstream source not yet directly instrumented per-emission:
- `bridge_frame_samples = 40` per frame (per-channel? unclear; with 8
  channels expected, this is much smaller than a typical TRUEHD access
  unit which is 360 samples)
- frames are emitted in bursts per `push_packet`, in a tight `for frame
  in result.frames` loop
- the number of frames per burst was not directly measured (only the
  per-frame interval was, which collapses inside a burst)

A `bridge_frames_per_push_packet` and `bridge_push_packet_dt_us` pair
would directly expose whether the plugin alternates burst sizes (which
would explain the two-mode behaviour in `writes_per_250ms`). This was
not implemented in this session.

### Conclusions and unfinished business

- The `0.4 Hz` (perceived as `0.5 Hz`/`1 Hz` depending on smoothing
  and aliasing) oscillation in the `pipewire_bridge` latency signal is
  **independent of the PI** (visible with PI paused, unchanged when PI
  is reactivated).
- The substitution of the FIFO contribution by a constant mean is
  correct in principle but not sufficient on its own — the input ring
  level itself wobbles from chunk drains.
- The cumulative-flow PI input is the right architectural choice (no
  chunk discretisation by construction), but the diff `written -
  drained` still oscillates because at least one of the two counters is
  driven by a non-uniform event stream.
- The most credible remaining suspect is the bridge plugin's
  two-phase batching pattern. Confirming this requires either
  instrumenting `bridge_frames_per_push_packet` directly OR fixing the
  display smoothing on `writes_per_250ms` so a slow trend can actually
  be read off the plot.
- The diag infrastructure is in place and reusable for any further
  metric (2 lines of Rust per metric, zero touches anywhere else).

### Files touched in this session

- `sys/src/diag.rs` (new module, moved from `audio_input`)
- `sys/Cargo.toml` (adds `serde`, `serde_json`)
- `sys/src/lib.rs` (re-exports `diag`)
- `audio_input/src/control.rs` (registry owned by `InputControl`)
- `audio_input/Cargo.toml` (drops `serde_json`)
- `audio_output/Cargo.toml` (adds `sys`)
- `audio_output/src/pipewire.rs` (all output-side diag atomics,
  cumulative-flow PI signal, sliding window + EMA for `writes_per_250ms`,
  removed `DAC_TRACE` spam, removed `log_dac_sample_domain_trace`)
- `audio_output/src/asio.rs` (mirror fix for the FIFO-mean substitution)
- `src/cli/decode/output.rs` (`AudioWriter::diag_atomic_handles`)
- `src/cli/decode/sample_write.rs` (hooks writer handles into registry
  per meter bundle, plumbs `diag_schema` / `diag_values` JSON to
  `send_meter_bundle`)
- `src/cli/decode/handler.rs` (decoder-side diag metrics)
- `src/cli/decode/state.rs` (cleanup of obsolete `last_write_pcm_at`)
- `src/runtime_osc/state_emit.rs` (publishes `diag_schema` /
  `diag_values` OSC addresses)
- `src/cli/decode/live_input.rs` (bridge-frame diag, cleanup of obsolete
  IEC958 raw log spam)
- `audio_input/src/{pipewire,pipewire_legacy,pipewire_exported}.rs`
  (registry-handed handles for IEC958 cadence metrics)
- Studio: `src-tauri/src/{osc_parser,osc_listener,app_state}.rs`,
  `src/state.js`, `src/tauri-bridge.js` (parses string payload — Tauri
  was forwarding the parsed `serde_json::Value` as a string blob),
  `src/ui/audio-panel.js`, `src/listeners/audio-panel-listeners.js`,
  `src/controls/diag-plot.js` (new), removal of `src/controls/iec958-plot.js`

### Pointer for the next session

Start by verifying the active binary really contains the latest
`audio_output/src/pipewire.rs` `write_samples` body (e.g. add a
`log::info!("write_samples ema=...")` once per second and grep the
renderer log). If it does, the bi-modal pattern surviving a 10 s EMA
is a genuine puzzle and probably points at something the metric reads
that has its own internal alternation. If it doesn't, the build/run
loop is the first thing to fix — likely a sidecar that bundles the
renderer binary at studio build time and was not refreshed.

### Bootstrap deadlock — cumulative-flow override REVERTED at end of session

After the operator activated `hard_recover_high` for a separate test,
the audio failed to start. Log signature:

```
WARN Decoded frame cadence anomaly: ... queue_ms=2021 wall_gap_ms=2020
WARN Buffer drain timeout after 2s - dropping 440 remaining samples
```

`queue_ms` grew by ~2 s at every cycle, `wall_gap_ms` was ~2 s — the
exact back-pressure timeout in `write_samples`. The bridge thread was
producing frames, the mpsc was filling up, and the writer was blocked
on `push_samples_with_backpressure` because the ring buffer was not
being drained.

Root cause: the cumulative-flow override of `control_available` cannot
survive the bootstrap. At startup the output callback fires before any
write_samples call:
- `cumulative_drained_for_callback.fetch_add(callback_input_domain_samples)`
  on each callback
- `cumulative_written` stays at 0 until writes begin
- → `drained > written` immediately, `saturating_sub` clamps to 0
- → `control_available_override = 0` forever
- → `is_far_band` true (because `|0 - target_buffer_fill|` exceeds the
  near/far threshold), `low_recover` enters Refill perpetually
- → callback never consumes from the ring (mute_far + no real drain)
- → ring fills, writes hit back-pressure timeout, decoder thread blocks

`hard_recover_high` was unrelated — it just made the operator test the
audio start and discover the deadlock. Reviewing the log, the
`Buffer drain timeout` warnings are present from the very first second,
before any hard-recover flag was toggled.

**Action taken at end of session**: the override is reverted in
`audio_output/src/pipewire.rs` so the PI again receives the classical
`update_latency_metrics` output (`available + output_fifo +
pending - callback/2`). The cumulative counters and the
`cumulative_flow_control_available` diag metric are kept (writes/
drains still incremented, atomic still stored), so the next session
can inspect the flow signal without breaking audio playback.

Next session must:
1. Restore an audio-safe path that exposes the chunk-noise-free PI
   signal — likely either initialise `cumulative_drained` so that
   `written - drained ≈ target_buffer_fill` from the start, or only
   switch the PI to the cumulative-flow signal AFTER the bootstrap has
   completed (e.g. when `available >= min_buffer_fill` for the first
   time).
2. Verify the `writes_per_250ms` smoothing actually runs in the binary
   (the τ = 10 s EMA failing to flatten a sub-Hz signal still has no
   convincing explanation).
3. Decide whether to keep pursuing the cumulative-flow PI input, or
   accept that the 0.4 Hz oscillation in the latency display is an
   inherent measurement artefact that cannot be cancelled at the
   source without breaking the bootstrap.

---

## Session 2026-05-18 — codec asymmetry in clock_mode=dac

### Controlled A/B between TRUEHD and EAC3

Same hardware, same `clock_mode=dac` (confirmed via `live_input` log line
`clock_mode=Dac backend=PwStream`, the renderer's source of truth — the
Studio UI still mis-labels modes on `v0.2.4`, see the `e425f6e` UI-apply
fix), identical Studio configuration:

- **TRUEHD source**: deterministic latency sawtooth, as previously
  described (~`16 ms` peak-to-peak, ~`1 s` period).
- **EAC3 source**: flat latency, no sawtooth.

Reproduced on **both `v0.2.4` (`c3638de`) and `main` (`f0b0348`)**. The
behaviour is unchanged between the two endpoints of the
post-`v0.2.4`/pre-`main` range.

### What this rules out

- It is not a regression introduced between `v0.2.4` and `main`. Any
  bisect over that range would not converge on a guilty commit — both
  endpoints show the same TRUEHD-only sawtooth.
- It is not a generic property of `clock_mode=dac`. The DAC capture
  and trigger path is the same for both codecs; only the bridge-side
  emission pattern differs.
- It is not a downstream PipeWire graph artefact (already excluded by
  `latency_downstream/path = 0`), and the codec dichotomy further
  rules out anything below the bridge.

### What this confirms

The dominant source of the DAC sawtooth in the IEC958/bridge chain is
**codec-dependent bridge emission cadence**, not the output servo, not
the DAC trigger loop, and not PI tuning. This is the strongest
single-variable signal we have in this investigation, and it aligns
exactly with the suspicion already raised in the 2026-05-17 session
(harletty plugin's two-phase batching: N vs N+1 frames per
`push_packet`).

EAC3 has small fixed-size access units (`1536` samples per substream
AU), which divide cleanly into bridge-emission units and produce a
uniform write cadence. TRUEHD uses MAT framing with substream
alignment whose access-unit size is not a small multiple of the
bridge's `40`-sample frame size, so the per-`push_packet` burst count
alternates — directly producing the bi-modal `writes_per_250ms`
pattern noted previously.

### Next investigation steps (revised)

1. Instrument `bridge_frames_per_push_packet` and
   `bridge_push_packet_dt_us` per emission (still missing — flagged in
   the 2026-05-17 session). Both codecs in parallel A/B should show:
   - EAC3 → constant `frames_per_push_packet`
   - TRUEHD → alternating, with period matching the observed sawtooth.
2. If confirmed, evaluate equalising emission downstream of the bridge:
   either a small pacing buffer at the bridge output that emits a
   constant number of frames per tick regardless of codec AU boundary,
   or a write-side aggregation that smooths the per-event sample count
   before it reaches the ring.
3. Do **not** continue speculative output-side fixes
   (servo/PI/recovery) until the bridge-emission hypothesis is either
   confirmed or refuted by direct measurement.

### Direct measurement of frames-per-push_packet (added 2026-05-18)

Added two diag metrics in group `bridge`:

- `bridge_frames_per_push_packet` — number of decoded frames returned by
  the most recent `bridge.push_packet` call.
- `bridge_push_packet_dt_us` — wall-clock interval between consecutive
  `push_packet` entries (µs).

Both are populated by the live IEC958 bridge decode worker
(`audio_input/src/bridge.rs::spawn_bridge_decode_worker`) — the
file-decode path in `decoder_thread.rs` is **not** active for live
SPDIF input, instrumenting it there yields zero.

Implementation: a new `BridgeDecodeDiag` struct bundling the two atomics
is passed as an optional parameter to `spawn_bridge_decode_worker`;
`live_input.rs` registers them via `diag.register("bridge_..", "bridge",
..)` alongside the existing `bridge_frame_samples` / `bridge_frame_dt_us`
metrics, and hands the bundle to the worker.

### Findings from the new metrics

| Codec  | `bridge_frames_per_push_packet`     |
| ------ | ----------------------------------- |
| EAC3   | constant `1`                        |
| TRUEHD | range `[23, 25]`, mostly `24`       |

This **partially** contradicts the bridge-batching hypothesis:

- EAC3 has zero variation and zero sawtooth in the expected pattern —
  consistent.
- TRUEHD has only small variation (`±1` around `24`, occasionally `23`),
  not the strong bi-modal alternation predicted. **And** the sawtooth is
  still present during long flat stretches at exactly `24` frames per
  `push_packet`.

So the codec-asymmetric emission cadence is real but **not** the
dominant cause of the sawtooth — the sawtooth survives a constant
`frames_per_push_packet` value.

### Contradictory observation in the same session

In a later run the operator also saw the sawtooth reappear on EAC3,
where the previous session 2026-05-18 controlled A/B had shown EAC3 as
flat. The variable that changed is not yet identified. The operator's
working hypothesis is that the perceived "flat" periods were actually
the adaptive runtime stuck in `settling` (state code `2`) rather than
truly `stable` (state code `0`) — i.e. what we read as a stability
property may be a state-machine artefact.

### Next investigation steps (revised)

1. Cross-check `runtime_state_code` (already published by
   `audio_output/src/pipewire.rs` as a diag metric, group `output`)
   against the latency sawtooth across both codecs. If "flat" coincides
   with `2 = settling` instead of `0 = stable`, the sawtooth-vs-flat
   distinction is about the controller state, not the source.
2. Capture both states with TRUEHD and EAC3, with the new
   `bridge_frames_per_push_packet` and `bridge_push_packet_dt_us`
   visible, to separate source-side variability (small, but real on
   TRUEHD) from controller-side state effects.
3. The bridge-batching hypothesis is **demoted** but not eliminated —
   the small `±1` jitter around `24` may still couple with the servo in
   ways that the constant `1` of EAC3 does not. Keep the metrics in
   place.

---

## Session 2026-05-19 — characterisation via FFT + pre-bridge clock servo

### Tooling additions

1. **Studio diag-plot FFT mode** — selected metrics are resampled onto a
   uniform 20 ms grid (50 Hz sample rate, 25 Hz Nyquist), DC-subtracted,
   Hanning-windowed, then run through a radix-2 Cooley-Tukey FFT. The
   visible window length sets `N` (max 2048 at the 60 s window for
   ~0.024 Hz bin resolution). The Y axis is absolute amplitude in dB
   relative to 1 unit of the metric — so amplitudes are directly
   comparable between captures. Peak marker labelled with `f Hz, dB`,
   selection rectangle reads the spectral value at the pointer (not the
   meaningless cursor Y geometry).

2. **`d/dt` mode** — first-derivative view in either time or FFT domain.
   Differences are taken between **actual value changes**, not between
   consecutive polls: the studio polls at 50 Hz but the renderer
   republishes at its own diag-rate cadence, so naively diffing adjacent
   polls produces a poll/publish beat artefact. Skipping unchanged polls
   recovers the true rate.

3. **`input_clock_us` cumulative diag metric** (group `iec958`) — a
   pre-decoder source-clock reference built from per-IEC958-chunk
   subframe counts in `audio_input/src/pipewire.rs`. By construction
   smooth: each S/PDIF subframe arrives at the source clock rate, and
   the IEC61937 zero-padding keeps the subframe rate constant across
   compressed bursts. Comparing its d/dt spectrum to that of
   `cumulative_written_diag` (post-decode) shows whether a frequency
   component lives in the source clock or only emerges after decoding.

4. **`AdaptiveResamplingConfig::use_pre_bridge_clock` flag** (default
   `false`, end-to-end plumbing through OSC `usePreBridgeClock` and a
   studio toggle in the Adaptive Resampling advanced panel). When on,
   the PI servo's input is overridden with `(input_clock_us · sample_rate
   · channel_count / 1e6) − cumulative_drained_input_samples`, calibrated
   against a one-shot offset captured at the first reading so it starts
   exactly on `target_buffer_fill` and only deviates as the two clocks
   genuinely drift. Ring level, FIFO, and resampler-pending continue to
   feed `control_latency_ms`, the low-recover state machine, and
   underrun/overrun safety paths. Bootstrap freeze (PI paused while
   `input_clock_us == 0`) and recalibration on `recovery_reacquire_pending`
   guard the discontinuities.

### Calibration fixes uncovered along the way

- `param_changed` in `audio_input/src/pipewire.rs` only updated
  `user_data.rate_hz` / `user_data.channels` for the `Raw` subtype.
  For IEC958 these stayed at the config defaults (2 ch / 192 kHz),
  while PipeWire negotiated the 8-channel TrueHD alternative at
  runtime. Result: `input_clock_us` was miscalibrated by ×4 on TrueHD.
  Fix: call `AudioInfoRaw::parse` regardless of subtype —
  `spa_format_audio_raw_parse` is a property-key lookup that works on
  IEC958 pods identically to Raw pods.

### Findings — characterising the source clock

With `use_pre_bridge_clock = false` (baseline) and the FFT + d/dt
overlay, on `clock_mode=Dac`:

- `input_clock_us` d/dt → spectrum is **flat at 3.1 Hz** (just thermal
  noise above the floor). Baseline ≈ 1 000 000 µs/s on Dac mode,
  ≈ 950 000 µs/s on Pipewire mode (= −50 000 ppm drift, matches the
  documented `~−55 000 ppm` hardware steady-state).
- `cumulative_written_diag` d/dt → also flat at 3.1 Hz, because the
  derivative is taken **between value changes**, which averages the
  per-batch bursts away on cumulative counters.

The sawtooth is therefore not in either cumulative signal — it lives in
the **instantaneous buffer level** (= `cumulative_written −
cumulative_drained`), which combines a bursty input (decoder pushes 24
frames at a time on TrueHD) with a smooth output (DAC drains at a
constant rate). Conclusion: the 3.1 Hz sawtooth is **decoder batching
absorbed by the ring**, not a real latency oscillation. The PI sees it
because it consumes the ring level; it should not.

### EMA transfer-function verification

Before settling on the architectural fix, the EMA was probed in three
points by setting `control_smoothing_alpha` live and reading the FFT
peak on `latency_smoothed_ms`:

| `α`   | Measured peak | Theoretical EMA H(3.1 Hz) at 21 ms callback |
| ----- | ------------- | ------------------------------------------- |
| 0.2   | −6 dB         | −6.4 dB                                     |
| 0.02  | −26 dB        | −26.1 dB                                    |
| 0.002 | −46 dB        | −46.2 dB                                    |

Match within < 0.5 dB across the range. **Identical with the local
resampler off or on** — confirming the PI loop is not amplifying
anything; the EMA acts as a passive low-pass on an external 3.1 Hz
input. The architectural fix (move the PI's reference upstream of the
decoder) is therefore strictly preferable to tuning the EMA: filtering
adds lag, the upstream signal has no 3.1 Hz to filter in the first
place.

### Findings — pre-bridge clock servo (the architectural fix)

After toggling `use_pre_bridge_clock = true` live via OSC / studio
toggle, on `clock_mode=Dac`:

- `runtime_state_code` stays at `0 = stable` across the toggle.
- `latency_smoothed_ms` (the value the PI sees, now the override) drops
  from its 18.5 ms p-p sawtooth to a much smaller residual.
- `latency_control_ms` (raw ring, unchanged) keeps its 3.1 Hz sawtooth
  as expected — the override does not touch the actual buffer.
- `rate_adjust_ppm` becomes substantially smoother, no longer tracking
  the batching.

A small residual sawtooth survives. Different amplitude and **different
period** from the pre-toggle 3.1 Hz, and the residual differs between
TrueHD and EAC3. Quantitative characterisation deferred to the next
session; the variables to capture for both codecs are:

- residual peak f and dB on `latency_smoothed_ms` (override mode)
- residual peak f and dB on `rate_adjust_ppm`
- relationship between the residual period and the decoder's
  `bridge_push_packet_dt_us` cadence (if any)

### Status of prior hypotheses

- **Closed-loop resonance EMA + PI** (carried over from earlier
  sessions): **invalidated**. The EMA transfer function matched theory
  exactly with the resampler on or off — the loop adds nothing.
- **Decoder batching as the 3.1 Hz source**: **confirmed**. Pre-bridge
  clock removes the dominant component.
- **Source-side burst at the IEC958 transport layer**: **invalidated**.
  `input_clock_us` d/dt is flat — the S/PDIF subframe stream is smooth
  at the resolution of our diag plot.
- **Residual oscillation after pre-bridge clock**: open. Codec-dependent,
  smaller than the original, period yet to be measured precisely. To be
  interpreted in the next session.

### Next investigation steps

1. Capture FFT peaks on `latency_smoothed_ms` and `rate_adjust_ppm`
   with `use_pre_bridge_clock = true` for both TrueHD and EAC3 —
   document the (f, dB) values per codec.
2. Overlay `bridge_push_packet_dt_us` to see if the residual period
   correlates with the bridge cadence (would suggest the residual is
   still source-side jitter that the cumulative metric absorbs but the
   instantaneous calibration captures somewhere).
3. Consider the calibration-offset interplay: the offset is captured
   once at first chunk, then the diff is taken. If the very first
   chunks are atypical (boot transient), the offset may be biased and
   the steady-state values would carry that bias. Test by forcing a
   recalibration after a few seconds.

---

## Session 2026-05-20 — output pacer (the structural fix) + IIR + latency accounting

### Pivot away from pre-bridge clock

The pre-bridge clock servo (previous session) filtered the *symptom* on
the PI side but left the ring buffer's raw `latency_control_ms` with its
3.1 Hz sawtooth, and its one-shot calibration offset was polluted by the
decoder batching phase + bootstrap conditions → a variable, "random"
80 ms offset between raw and smoothed on TrueHD (≈0 on EAC3).

The operator's insight, adopted here: **don't filter the PI signal,
smooth the data written into the ring itself**. The IEC958 source clock
is provably smooth (`input_clock_us` d/dt flat). So pace the bridge
output to the IEC958 chunk cadence and buffer the decoder's bursts.

### Output pacer — design

A FIFO is inserted **after** the spatial renderer (post-decode,
post-render) so the renderer keeps its metadata-accurate sample
processing and harletty stays untouched — the pacer only sees plain
speaker PCM that can be sliced freely.

- `audio_output::pacer::PacerHandle` bundles the pacer FIFO, the ring,
  pre-roll state, ratios and diag atomics; shared cross-crate via
  `InputControl::install_output_pacer` (new `audio_input → audio_output`
  dep, one direction only).
- `PipewireWriter::write_samples` routes rendered PCM into `pacer_fifo`
  when `use_output_pacing` is on, otherwise straight to the ring.
- The **PipeWire input thread** drains `pacer_fifo → ring` in the same
  callback that just read an IEC958 chunk, by an amount derived
  arithmetically from that chunk's `byte_len` (strict 1:1 coupling,
  no atomic-delta drift). Underrun → zero-fill + diag counter.
- Pre-roll: until the FIFO holds `pre_roll_threshold` (64 ms default),
  the drain zero-fills the ring so the DAC never starves at startup.
- Diag metrics added (group `output`): `pacer_fifo_level`,
  `pacer_drain_total`, `pacer_underrun_total`.

Result: the ring sees a smooth flow regardless of decoder batching, so
the classic ring-mode PI works on a clean `control_available` with no
downstream filtering tricks. `use_pre_bridge_clock` becomes redundant
(kept for now as an A/B control; flagged for removal).

### Latency accounting (the part that bit us)

First cut added `pacer_fifo.len()` to `control_available` — wrong: it
re-injected the batching oscillation into the PI input and destabilised
the servo. Then it was split (PI input excludes pacer; displayed latency
includes it) but still reported only the instantaneous level.

The operator's correct framing: **the pacer adds a fixed, known latency
= its capacity. When the user asks for 500 ms they want 500 ms total.**
Final design:

1. **Pacer clamped to its capacity** — `write_samples` uses
   `pre_roll_threshold_samples` as the back-pressure cap when pacing is
   on, so the renderer blocks rather than letting the FIFO drift up to
   seconds of buffered audio. Pacer depth is now genuinely fixed.
2. **PI setpoint compensated** — `runtime_target_buffer_fill =
   user_target − pacer_capacity` while pacing is on. The PI aims the
   ring at `user_target − 64 ms`; ring + pacer sums to `user_target`.
3. **Displayed latency uses the fixed capacity**, not the live FIFO
   level, consistent with the clamped depth.

Net: toggling pacing no longer changes the audible end-to-end latency;
the user's configured `latency_ms` stays the total.

### IIR replaces the EMA on the PI input

Independently, the EMA on `control_available` was replaced by a
parametric IIR (`audio_output::iir::IirLowPassState`):

- Config: `control_smoothing_alpha` → `control_smoothing_cutoff_hz`
  (default 0.5 Hz) + `control_smoothing_order` (1 = single pole
  6 dB/oct, 2 = Butterworth biquad 12 dB/oct). Hard rename, no compat.
- Coefficients recomputed each callback from the measured callback dt
  (`output_callback_dt_us`), so the cutoff is invariant under quantum
  changes — fixes the old bug where α-per-callback silently rescaled
  the effective cutoff when the callback shrank from 256 ms to 21 ms
  (commit `d43ddab`).
- Studio: numeric cutoff field + order select in the adaptive advanced
  panel.

### Status of hypotheses

- **Decoder batching as the 3.1 Hz source**: confirmed (prior session)
  and now *structurally addressed* by the pacer rather than filtered.
- **Pre-bridge clock**: superseded by the pacer. Redundant, slated for
  removal once the pacer is validated in the field.
- **Residual oscillation**: expected to be gone from the ring with the
  pacer active (to be confirmed in-runtime via FFT on
  `latency_control_ms`, which should lose its 3.1 Hz peak).

### Next investigation steps

1. Field-validate: toggle `use_output_pacing`, confirm
   `latency_control_ms` FFT loses the 3.1 Hz peak and the audible
   end-to-end latency equals the configured `latency_ms`.
2. Watch `pacer_underrun_total` in steady state — if non-zero, raise the
   pre-roll capacity (currently a hard-coded 64 ms; expose as config if
   tuning proves necessary).
3. Watch `write_samples` back-pressure timeouts — if the renderer blocks
   too often, the pre-roll cap is too tight for the renderer's push
   burst size.
4. Remove `use_pre_bridge_clock` and its plumbing once the pacer is
   confirmed as the single solution.
