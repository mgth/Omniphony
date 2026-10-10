# Adaptive resampling & latency regulation — audit (2026-10-01)

State audited: `main` at `8eb253a6`. This document describes how the output
latency regulation of the `orender` CLI works today and what is wrong with it.
The replacement is planned in [resampling-rework-plan.md](resampling-rework-plan.md).

**Goal the subsystem is meant to serve:** hold a precise, known end-to-end
latency while the audio source runs on **its own clock**. For example, mpv
should be able to sync on the video/display and stop adapting to the audio
device; `orender` absorbs the difference between the source clock and the DAC
clock by resampling.

## 1. Scope

- **mpv `--ad=orender` (liborender, in-process)** — not concerned. mpv's own
  AO drives the device; none of this code runs.
- **`orender` CLI** — concerned. A source (mpv, VLC, Kodi) plays into the
  `omniphony` PipeWire sink (IEC958/PCM), or writes a pipe / file; `orender`
  decodes, renders and plays to the DAC through PipeWire (Linux) or cpal
  (ASIO / CoreAudio).

## 2. How it works today

### 2.1 Data path

```
source ─► capture (PwStream DRIVER sink | pipe | file)
       ─► decoder bridge (bursty: TrueHD emits ~24 access units = 960 frames ≈ 20 ms at a time)
       ─► renderer ─► write_samples ─► [optional pacer FIFO, 64 ms]
       ─► ring  (crossbeam ArrayQueue<f32>, per-sample push/pop)
       ─► output callback ─► rubato SincFixedIn (256 taps, 1024-frame chunks)
       ─► output FIFO ─► DAC
```

### 2.2 Measured quantity

`audio_output/src/adaptive_runtime.rs` (`update_latency_metrics`):

```
control_available = ring + output_fifo/ratio + resampler_pending − callback/2
```

It goes through a 1st- or 2nd-order IIR low-pass (default 0.5 Hz) before
reaching the PI. The raw value drives the recovery state machine; the smoothed
value drives the PI and the Settling dwell.

### 2.3 PI controller

`audio_output/src/lib.rs` (`compute_adaptive_step`):

```
consume_adjust = 1 + kp·e_ms·1e-6 + ki·Σ(e_ms)·1e-6     (clamped to 1 ± max_adjust)
ratio          = base_ratio / consume_adjust             (rubato set_resample_ratio, ramped)
```

- The integral accumulates **once per servo step (callback)**, not per second.
- Deadband: hard-coded 480 interleaved samples on PipeWire, 100 on cpal. It is
  expressed in samples, so it depends on the channel count.
- Anti-windup by conditional integration, and the integral term is clamped to
  `max_adjust`.
- "Discharge": the integral is multiplied by `integral_discharge_ratio` (0.25)
  every time the error changes sign.
- Defaults: `kp_near = 1`, `ki = 1` (ppm/ms), `max_adjust = 0.01`.

### 2.4 Recovery state machine

States `stable`, `low-recover` (Refill → Settling) and `high-recover`, plus a
near/far band. It mutes, fades in over 500 ms, trims, and re-acquires
(resampler + FIFO reset). The PI is frozen outside `stable`. About twenty
tunables in all: four far-mode flags, six margins, an EMA alpha, settle time,
fade time.

### 2.5 Who clocks the source

| `clock_mode` | Mechanism | Consequence |
|---|---|---|
| `dac` | The output callback runs a Bresenham count and posts triggers that the capture mainloop fires with `pw_stream_trigger_process` (`audio_output/src/pipewire.rs`, `audio_input/src/pipewire.rs`) | The source is **slaved to the DAC**. Drift is zero by construction; the resampler only fights decoder-burst noise |
| `pipewire` | System timer re-armed at `now + delay`, delay scaled by the output's `consume_adjust` | Two controllers act on the same error. **The sign was inverted** (§3.1) |
| `upstream` | `PwClientNode` | Stub: not a driver, never reads data, never reports a clock. It is still the bootstrap default when `clock_mode` is omitted with `input_mode: pipewire` (`src/cli/decode/bootstrap.rs`) |
| pipe / file | Blocking `write_samples` back-pressure at **2 × target**, 10 ms sleeps | The producer is paced by the DAC; the ring sits near the cap, not at the target, so the PI saturates |

Note on the pipe with mpv `--ao=pcm`: `ao_pcm` is *untimed*. mpv then times
video on the system/display clock and **throttles audio to follow video**
(`player/audio.c` `update_throttle`); it does no audio-driven frame dropping or
timing correction (`player/video.c`, `!ao_untimed` guards). The pipe source
therefore *has* a clock (mpv's video clock, delivered in bursts of about one
video frame). It is not clockless.

### 2.6 Layers added around the measurement problem (May 2026)

- `use_pre_bridge_clock`: replaces the measurement by
  `input_clock_us − drained`. `input_clock_us` counts delivered transport
  frames at the nominal rate, so in `dac` mode it compares the DAC to itself.
- `use_output_pacing`: a FIFO drained from the input thread at the chunk
  cadence. It adds 64 ms of latency, another producer thread and integer
  truncation per drain.
- `disable_backpressure`: drops samples instead of blocking.
- "Cumulative flow" counters: published, unused (deadlocked at bootstrap).

### 2.7 Latency reported to the source

- The PipeWire sink republishes its Latency param at 1 Hz from the
  **instantaneous** measured latency, so the player sees it move.
- `$TMPDIR/omniphony_delay` is written from the decode thread; no reader was
  found.

## 3. Findings

### 3.1 Positive feedback in `pipewire` clock mode (fixed on `fix/input-trigger-feedback-sign`)

`audio_input/src/pipewire.rs` scaled the trigger interval by
`(1 / consume_adjust).clamp(0.95, 1.05)`. `consume_adjust > 1` means the ring is
above target because the source is ahead. Dividing shortened the interval and
sped the source up further, so the loop latched on the ±5 % clamp.

This is almost certainly the **"−55 000 ppm hardware drift"**. That figure was
only ever observed in `pipewire` mode; `dac` mode showed none. A crystal drifts
by less than 200 ppm. A 5.5 % resample shifts pitch by about 93 cents, close to
a semitone. The figure also justified raising `max_adjust` to 0.10–0.15.

### 3.2 No mode serves the goal

`dac` slaves the source to the DAC. `pipewire` re-slaves the source to the
output state. `upstream` does not work. The pipe is DAC-paced through
back-pressure. In no mode does the source run on an independent clock that
`orender` then compensates.

### 3.3 The PI loop is badly scaled; the discharge hides it

The plant is a pure integrator with a known gain:
`de/dt [ms/s] = drift_ppm·1e-3 − (consume−1)·1e3`.

With the defaults (1024-frame callbacks ≈ 46.9 Hz):

- P-only time constant: `1000 / kp` = **1000 s**. The P term is negligible.
- I loop: ωn = √(1e-3 · 46.9 · ki) ≈ 0.22 rad/s, a period of ~29 s, and
  ζ = 1e-3·kp / (2ωn) ≈ **0.002**. An oscillator.

A faithful simulation of `compute_adaptive_step` with the 0.5 Hz IIR and
100 ppm of drift gives:

| Configuration | Error | Ratio |
|---|---|---|
| defaults, discharge 0.25 | ±1.3 ms | −35…+241 ppm |
| defaults, discharge 1.0 (off) | **±40 ms, 29 s limit cycle** | **±9000 ppm** |
| analytic gains (kp = 200, ki·f_cb = 10 /s, ζ = 1), discharge off, 40 ms sawtooth on the measurement | −0.25…+0.05 ms | −470…+915 ppm |

So the discharge is a nonlinear damper on an undamped loop. Other consequences:

- `ki` is per callback, so the effective gain depends on the quantum. ASIO at
  256 frames has about 4× the integral gain of PipeWire at 1024.
- Ziegler–Nichols tuning (`PI_TUNING_PROCEDURE.md`, the Studio auto-tune
  wizard) does not fit: there is no critical gain for an integrator plant,
  and the gains can be computed from a single bandwidth.

### 3.4 The wrong quantity is regulated

Ring fill includes the decoder's burst pattern. For TrueHD that is 20 ms
bursts, which beat against the 1024-frame resampler chunk into the
~3.1 Hz / ~320 ms sawtooth seen in the plots (spike S4 corrected the earlier
reading of it as 320 ms bursts). Every layer in §2.6 and the IIR exists to hide that. The quantity to
regulate is the **end-to-end latency**: source frames received minus frames
played, taken at the same timestamped instant, plus the graph/device delay. It
is constant while the decoder bursts, because the samples only move between
stages.

### 3.5 Duplication and divergence between backends

`cpal_output.rs` carries its own copy of the callback:

- It never adopted the shared trim fix (the output→input round trip).
- `recovery_reacquire_pending` is never cleared.
- It takes two blocking `Mutex::lock()` per realtime callback.
- `dt` is computed from output frames over the input rate, so the IIR cutoff is
  2× off when resampling.
- Deadband 100 vs 480.
- It never publishes the smoothed latency, so the auto-tuner reads the raw
  value on ASIO/CoreAudio.

### 3.6 Other defects

- On Linux `max_latency_ms` only ever grows: lowering the target from 500 to
  100 ms keeps a 1000 ms back-pressure cap (`output_runtime_sync.rs`).
- `push_samples_with_backpressure` busy-spins if the cap exceeds
  `OUTPUT_RING_CAPACITY`.
- Live PipeWire input drops packets and decoded frames on full channels
  (`try_send`), which breaks any sample-count accounting.
- The pacer drain truncates per chunk with no fractional carry.
- Per-sample `ArrayQueue` push/pop is one atomic CAS per sample.
- The 256-tap sinc runs on every channel permanently, though a ±0.1 % ratio
  needs far less.
- The native-rate adaptive branch of the PipeWire output stores
  `1/consume_adjust` as the PipeWire rate (also inverted), but it is
  unreachable: adaptive always selects the local resampler.
- There is no closed-loop test; only open-loop unit tests.

## 4. Conclusion

The subsystem regulates a noisy proxy (ring fill) with a loop that has no
damping, inside a clock topology that never lets the source run free. Each
symptom was patched where it showed (EMA→IIR, discharge, pacer, pre-bridge
clock, settle-on-smoothed, back-pressure toggle). A rewrite around the right
measurement and clock topology is preferable to further tuning; see the plan.
