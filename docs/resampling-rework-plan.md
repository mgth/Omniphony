# Adaptive resampling — rework plan

Companion to [resampling-audit-2026-10.md](resampling-audit-2026-10.md). This is
a clean-slate redesign: the current regulation (PI + far mode + low/high
recover + IIR + pacer + pre-bridge clock + trigger feedback) is **replaced, not
refactored**. No compatibility with its tunables is kept beyond reading old
config keys without failing.

## 1. Requirements

1. **Fixed, exact end-to-end latency `L`.** The time from a source sample
   reaching `orender` to the same sample leaving the DAC is held at `L`. `L` is
   set by the user, and `orender` reports it as a constant.
2. **The source runs on its own clock.** `orender` never re-times the source
   to the DAC. mpv must be free to sync on video/display.
3. **Drift is compensated by resampling.** Correction stays within a few
   hundred ppm in steady state, never percent, with no audible frequency
   modulation.
4. **One code path for every backend** (PipeWire, ASIO, CoreAudio). Backends
   only translate device APIs.
5. **Realtime discipline.** No allocation, lock or blocking call in the
   output callback, and no per-sample atomics. CPU must scale to 24 channels on
   modest hardware.
6. **Few, physical parameters**: latency (ms), loop bandwidth (Hz) and the
   realign threshold (ms). Everything else is derived.
7. **A closed-loop simulator in the test suite** gates every change.

## 2. Clock model

Three clocks exist:

- **S** — the source.
- **D** — the DAC.
- **M** — `CLOCK_MONOTONIC`, the common reference against which everything
  is timestamped.

### 2.1 Source clock modes (replace `dac` / `pipewire` / `upstream`)

| Mode | Used for | What S is | How `orender` learns S |
|---|---|---|---|
| `own` | live input through the `omniphony` PipeWire sink | `orender` is the sink's **driver**, clocked on M with **absolute deadlines**. S ≡ M by construction | Known exactly: no estimation |
| `follow` | pipe from mpv (`--ao=pcm`, untimed → mpv paces audio on its video/system clock), network, any self-paced writer | Whatever the writer uses | 2nd-order DLL on arrival timestamps `(t_M, frames_received)` |
| `none` | files, `cat`, anything faster than real time | Does not exist | Not needed: D is master and back-pressure paces the producer at `L`. The ratio stays nominal |

`own` gives mpv a stable clock that is not the DAC. With `--ao=pipewire` into
the sink and `--video-sync=display-resample`, mpv adapts audio to the display
against M, which is the same time base its vsync timestamps use, and sees a
constant latency `L`. This is the recommended configuration for video
playback.

`follow` exists because the pipe is used in practice. With mpv `--ao=pcm`,
mpv does not adapt to anything downstream: `ao_pcm` is untimed, so mpv times
video on the system/display clock and throttles audio to follow it
(`player/audio.c` `update_throttle`). The pipe is a clocked source delivered in
bursts of about one video frame.

**Back-pressure is never engaged in `own` or `follow`.** Blocking the writer
there would substitute D for S. mpv does not drop or duplicate audio when
blocked, but its audio would fall progressively behind its video by the S/D
drift (e.g. 50 ppm ≈ 180 ms per hour), uncorrected. The ring is sized so that a
full ring is a fault (→ realign), not a pacing mechanism.

### 2.2 Measurements (all timestamped in M)

- **Input counter** `N_in(t)`: source frames received, in source nominal
  frames, counted before decoding. Stamped at the capture callback (`own`) or
  at the pipe read (`follow`). The reader must never block in steady state, so
  arrival time is the writer's time.
- **Played counter** `N_play(t)`: source frames that have left the resampler
  toward the device. It is the resampler's exact fractional input position
  minus its group delay, minus nothing else.
- **Device delay** `D_dev(t)`: from the backend (`pw_time.delay` +
  `buffered`; ASIO/CoreAudio output timestamps).
- **End-to-end latency** at output callback time `t_c`:

  ```
  L_e2e = (N_in(t_c) − N_play(t_c)) / f_s + D_dev(t_c)
  ```

  This includes the decoder's look-ahead and its bursts. It is constant while
  the decoder batches, because samples only move between stages. **No
  low-pass, pacer or calibration is needed.** In `follow` mode, `N_in(t_c)` is
  read from the DLL (interpolated), not the last burst.

**Accounting invariant:** every frame dropped or inserted anywhere between
capture and DAC is reported as a discontinuity event `(position, ±frames)`.
Today's silent `try_send` drops on the live path must become counted events,
or disappear.

### 2.3 Control law

```
ratio_total = r_ff · (1 + u)
r_ff        = rate(S) / rate(D)          feed-forward
u           = Kp·e + Ki·∫e dt            e = L_e2e − L, in seconds
```

- **Feed-forward `r_ff`.**
  - Rate of D against M: PipeWire's driver `rate_diff` / `pw_time` ticks
    against `now`; the ASIO/CoreAudio sample position against host time, via a
    DLL.
  - Rate of S against M: 1 in `own`, the arrival DLL in `follow`.
  - This carries the actual drift (tens of ppm) with almost no noise.
- **PI on phase.** The plant is `de/dt = −u + (residual rate error)`. The
  closed loop is `s² + Kp·s + Ki`, so:

  ```
  ω  = 2π·B          (B = loop bandwidth, default 0.02 Hz)
  Kp = 2ζω           (ζ = 1)
  Ki = ω²
  ```

  - Per **second**, integrated with the measured `dt`, so it does not depend
    on the quantum or the backend.
  - Anti-windup by clamping `u` to ±500 ppm.
  - No deadband, no discharge, no smoothing filter.
- **Sanity bounds.** If `|r_ff − 1| > 1000 ppm`, it is a configuration error
  (wrong nominal rate, broken clock) and is surfaced as such. It is never
  silently resampled.

### 2.4 Startup and realign — one rule

- **Start.** Output silence until `L_e2e` would equal `L`, then start on the
  exact frame: drop or insert silence to the frame. The first audible sample
  lands at `L`.
- **Realign** when `|e| > E_max` (default 20 ms), on an underrun, or on a ring
  overflow:
  1. fade out over 5 ms;
  2. drop or insert to put `e` back at 0, exactly;
  3. fade in over 5 ms;
  4. keep `r_ff` and the integrator (they encode drift, not phase);
  5. count the event.
- **Source pause / discontinuity.** Same path. A source silence longer than
  the ring is an underrun.

Nothing else is kept: no near/far bands, low/high recover, settling dwell,
predictive refill, trim plans or mute-on-far.

## 3. Components

| Component | Content | Notes |
|---|---|---|
| `sync` core (pure, no I/O) | DLL, `LatencyServo` (feed-forward + PI), `Realigner`, discontinuity accounting | Deterministic, unit- and simulation-tested |
| Plant simulator (tests) | Configurable S/D ppm, D quantum and jitter, decoder bursts (TrueHD ~320 ms, E-AC-3), mpv video-frame throttle, pipe buffering, drops, pauses | Gates every PR |
| Frame ring | SPSC, interleaved `f32`, bulk `memcpy`, fixed capacity from max `L` | Replaces per-sample `ArrayQueue` |
| Resampler | Variable ratio, produces exactly N output frames per callback, exposes its fractional input position | Choice made in spike S3 |
| Output core | One callback body for every backend: pull N frames, feed the servo `(t_M, device position, D_dev)`, apply fades | `pipewire.rs` / `cpal_output.rs` become thin adapters |
| Source adapters | `own` driver sink, `follow` reader + arrival DLL, `none` back-pressure at `L` (condvar wake, not 10 ms sleeps) | |

## 4. Phases

Work happens in workflow `resampling-rework` on `feat/resampling-rework`. The
new engine is built alongside the old one and switched over in one cutover PR
that deletes the old code. `main` stays buildable throughout.

### Phase 0 — Spikes (decisions before code)

- **S1 — `own` mode mechanics.** Can a PipeWire DRIVER stream triggered from
  a `timerfd` (`CLOCK_MONOTONIC`, `TFD_TIMER_ABSTIME`) present mpv with a clean
  clock and a correct delay? Alternative: let PipeWire's own timer driver
  (`support.null-audio-sink`, `node.driver=true`) clock the sink and capture
  its monitor. Check IEC958 passthrough negotiation in both. Measure the clock
  (`nsec`, `rate_diff`) and the latency as mpv sees them.
- **S2 — Output timestamps per backend.** `pw_stream_get_time_n` (`now`,
  `ticks`, `rate_diff`, `delay`, `buffered`), cpal ASIO and CoreAudio
  `OutputCallbackInfo` timestamps. Measure the jitter of the derived D rate on
  real hardware.
- **S3 — Resampler.**
  - Candidates: rubato `SincFixedOut`; an in-house polyphase/Farrow
    fractional-delay FIR (32–64 taps, SIMD-friendly).
  - Criteria: THD+N and passband at ratio 1 ± 500 ppm, CPU at 16/24 ch,
    exact position reporting, no allocation.
  - Real SRC (44.1 → 48 kHz) may stay a separate fixed stage.
- **S4 — Sample accounting through the decoder bridge.** Confirm each
  codec's constant decode delay. List every drop/insert point on the live and
  pipe paths.

### Phase 1 — `sync` core + simulator

- DLL, servo, realigner and accounting, as pure code.
- The simulator, with these acceptance thresholds:
  - steady state `|e|` p99 < 0.5 ms;
  - ratio modulation < 5 ppm p-p once locked;
  - lock within 30 s of start;
  - no realign over 10 simulated hours with ±100 ppm drift, TrueHD bursts and
    mpv-like 41.7 ms throttling;
  - correct recovery from a 2 s source pause and from a 1-frame drop.

### Phase 2 — Ring + resampler

- SPSC frame ring and the chosen resampler (S3), with benches and quality tests.

### Phase 3 — Output core + backends

- The shared callback body. PipeWire adapter, then cpal (ASIO / CoreAudio).
- `D_dev` and timestamps per S2.

### Phase 4 — Source modes

- `own` (per S1), `follow`, `none`.
- Ring-full and underrun → realign.
- Explicit config; a warning when a `follow` source behaves clockless
  (sustained faster than real time).

### Phase 5 — Latency contract

- The sink advertises the constant `L` (plus the device delay as reported),
  never the instantaneous value.
- Pipe: publish `L` over OSC and document the matching mpv `--audio-delay`.
- Drop `$TMPDIR/omniphony_delay`.

### Phase 6 — Surface and cutover

- **Options.** Keep `latency_target_ms`, `source_clock` (`own` / `follow` /
  `none`), and advanced `loop_bandwidth_hz` and `realign_threshold_ms`.
  - All of them change the render, so per `docs/persistence-policy.md` they
    persist through Save only.
  - Old `adaptive_resampling_*`, `clock_mode`, `use_output_pacing` etc. are
    read, ignored with a one-line warning, and preserved on write (unknown-key
    passthrough).
- **Studio** (both front ends). Remove the auto-tune wizard and the PI/margin
  controls. The plot shows `e`, `r_ff`, `u` (ppm), the realign count and
  underruns. Respect the `omniphony-studio-egui` architecture ratchet.
- **Delete:**
  - `adaptive_runtime.rs`, `iir.rs`, `pacer.rs`, `callback_state.rs` and the
    far-mode logic;
  - the Bresenham trigger path, `PwClientNode`, `PI_TUNING_PROCEDURE.md`;
  - the old `latency-regulation*.md` (rewritten for the new design).
- One cutover PR into `main`.

### Phase 7 — Hardware validation

- TrueHD, E-AC-3 and PCM through the sink (`own`) and the pipe (`follow`).
- Two hours of mpv with video: measure A/V offset drift (start vs end), the
  realign count, and the ratio trace.
- ASIO and CoreAudio smoke tests.
- CPU at 24 ch on the target box.
- Listening by the user before cutover.

## 5. Open questions

- **A DAC-locked mode for bit-transparent playback?** In this mode the
  source follows D and there is no resampling. It is the only mode with zero
  resampling, but it is not the goal here. It would sit behind `none`-like
  pacing of a clocked source and needs PipeWire driver-group membership (the
  `node.group` attempt ran at 25 % speed). Deferred.
- **Default `L` per codec.** It must exceed the decoder's maximum burst plus a
  quantum plus margin; TrueHD pushes ~320 ms. Validate at configuration time
  and surface the minimum.
- **`follow` jitter.** mpv's throttle plus the pipe buffer may give tens of ms
  of arrival jitter. The DLL bandwidth (≈0.01 Hz) must keep the phase error
  well under 1 ms; the simulator decides.
