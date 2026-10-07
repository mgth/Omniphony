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
  - Rate of D against M: our own 2nd-order DLL on the device position
    against M. On PipeWire that is `(pw_time.now, pw_time.ticks)`, never raw
    `rate_diff` (S2). On ASIO and CoreAudio it is the sample position against
    host time.
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
| Plant simulator (tests) | Configurable S/D ppm, D quantum and jitter, decoder bursts (TrueHD 20 ms MAT batches ±1 AU, E-AC-3/AC-3 32 ms AUs, DTS-HD MA start-up backlog), PipeWire graph-clock excursions (−66 ppm for ~100 s, S2), mpv video-frame throttle, pipe buffering, drops, pauses | Gates every PR |
| Frame ring | SPSC, interleaved `f32`, bulk `memcpy`, fixed capacity from max `L` | Replaces per-sample `ArrayQueue` |
| Resampler | Variable ratio, produces exactly N output frames per callback, exposes its fractional input position | Choice made in spike S3 |
| Output core | One callback body for every backend: pull N frames, feed the servo `(t_M, device position, D_dev)`, apply fades | `pipewire.rs` / `cpal_output.rs` become thin adapters |
| Source adapters | `own` driver sink, `follow` reader + arrival DLL, `none` back-pressure at `L` (condvar wake, not 10 ms sleeps) | |

## 4. Phases

Work happens in workflow `resampling-rework` on `feat/resampling-rework`. The
new engine is built alongside the old one and switched over in one cutover PR
that deletes the old code. `main` stays buildable throughout.

### Phase 0 — Spikes (decisions before code) — DONE 2026-10-01, see §6

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
  quantum plus margin. The worst steady-state hold is E-AC-3/AC-3 (one 32 ms
  AU plus one transport burst); TrueHD batches 20 ms (S4). Validate at configuration time
  and surface the minimum.
- **`follow` jitter.** mpv's throttle plus the pipe buffer may give tens of ms
  of arrival jitter. The DLL bandwidth (≈0.01 Hz) must keep the phase error
  well under 1 ms; the simulator decides.

## 6. Phase 0 outcome (2026-10-01)

Reports are in [`docs/resampling-rework/`](resampling-rework/). Throwaway code
is in `spikes/`, standalone and outside the cargo workspace.

### 6.1 S1 — `own` clock: PipeWire timer driver + follower sink ([report](resampling-rework/spike-s1-own-clock.md))

**Decision.** orender creates its own timer driver: `spa-node-factory` →
`support.node.driver`, `clock.id = monotonic`, `priority.driver = 0`, and a
unique `node.group`. The IEC958 `pw_stream` sink joins that group as a plain
follower: `RT_PROCESS`, `node.always-process = true`, no `DRIVER` flag.

- **Latency:** advertised once as `SPA_PARAM_Latency` (INPUT, `min = max = L`).
  `ProcessLatency` on a `pw_stream` is never propagated.
- **Clock the player sees:** exact, `nsec = position·10⁹/rate` with residual
  ≤ 1 ns and `rate_diff = 1`. mpv's `audio-pts` drift was −0.008 ppm.
- **Jitter:** driver p99 17 µs; when orender stalls, the player is still woken
  on time.
- **IEC958:** passthrough works. It needs `node.force-rate` set to the
  negotiated carrier rate in `param_changed` (otherwise 192 kHz runs at ¼
  speed), and Buffers sized for a full quantum.
- **Source time** of a frame `p` is `clock.nsec + (p − clock.position)/rate`,
  not orender's wall time. A Format change, `DISCONT` or `XRUN_RECOVER` is a
  realign event.
- **Driver merge:** detect it every cycle (`clock.id`, `clock.name`,
  `rate_diff`). If the group gets merged onto a hardware driver, fall back to
  `follow`-style estimation and surface a warning.
- **Rejected options:** null-audio-sink (cannot carry IEC958). The app-filled
  driver (option a) works but is fragile; it is kept as a fallback.

### 6.2 S2 — output timestamps and D-rate estimation ([report](resampling-rework/spike-s2-output-timestamps.md))

**PipeWire.**
- `pw_stream_get_time_n` per callback gives `now`, `ticks`, `delay`,
  `queued` and `buffered`.
- The frame queued now is heard at `now + (buffered + N + delay)/fs`. The
  USB/DAC-internal 1–3 ms is not reported anywhere.

**Estimator: our own 2nd-order DLL.**
- Fast start from 1 Hz, narrowing to **B = 0.01 Hz**. It locks within 2 ppm
  in 12–16 s, with 0.2–0.8 ppm rms steady noise.
- Reset on a clock-id change or a discontinuity.
- Raw `rate_diff` is far too noisy to use (7–39 ppm rms per cycle, thousands
  of ppm at start).

**Measured on this machine.**
- DAC −17 ppm vs `CLOCK_MONOTONIC` (−6 ppm vs RAW; NTP slews MONOTONIC by
  ~+11 ppm). Using MONOTONIC on both sides cancels the slew.
- The graph clock made a −66 ppm excursion for ~100 s (PipeWire's ALSA DLL
  re-centring). The simulator must include it.

**cpal (ASIO / CoreAudio).** cpal discards the sample position and the
latencies, and ASIO timestamps are 1 ms coarse. Phase 3 needs a small
cpal/asio-sys patch or thin native adapters. Hardware verification on
Windows and macOS is still to do.

### 6.3 S3 — in-house polyphase resampler ([report](resampling-rework/spike-s3-resampler.md))

**Decision.** Replace rubato.

**Design (drift-only, ≤ 48 kHz).**
- 64-tap windowed sinc, Kaiser β 14, cutoff at the input Nyquist.
- 32-segment Hermite-interpolated phase table (32 KiB, built once).
- Q32.32 position. `position()` is `N_play` directly; the look-ahead into the
  ring is 32 frames.
- 44.1 ↔ 48 kHz folds into the same engine with 96 taps.

**Measured, and re-run during this review.**

| | Polyphase (64 taps) | rubato 5 `Async` | rubato 0.14 (today) |
|---|---|---|---|
| THD+N at 1 kHz | −139 dB | — | — |
| THD+N at 20 kHz | −134.7 dB | −108 dB | — |
| Passband to 20 kHz | ±0.00001 dB | — | — |
| CPU at 24 ch / 48 kHz | 0.29 % of a core | 1.25 % | 2.61 % |
| Fractional position | exact (2.6·10⁻⁵ frame) | not exposed | not exposed |
| Steady-state allocations | none | — | — |

- Results do not change at ±2000 ppm.
- In the 20–24 kHz transition band a 22 kHz tone loses 0.085 dB and shows
  −40 dB THD+N. This is inaudible, and the alias lands within a few Hz of the
  tone.
- The aarch64 build was checked in the asm but never run.

### 6.4 S4 — sample accounting ([report](resampling-rework/spike-s4-sample-accounting.md))

**No path conserves the frame count today**: silent `try_send` drops, the
bootstrap gate, stream-not-ready, partial-frame pushes that rotate the
channels, pacer zero-fills.

**Decision.**
- Count `N_in` in transport time at the capture point.
- Make ingest time-conserving: pause bursts, stuffing and sync loss become
  silence of the same duration.
- Audit decoder output per burst against its nominal length; this catches
  silent bridge drops (TrueHD resync up to ~107 ms, DTS error frames).
- Per-epoch `inserted`/`dropped` counters per stage, so
  `N_in_eff = N_in + Σinserted − Σdropped`, plus a non-RT
  `DiscontinuityEvent` queue.
- Format, rate, source or writer changes start a new epoch.
- Replace the drop points with SPSC rings sized from `L_max`, and push only
  whole frames.

**Codec constants.**
- Transport↔PCM ratio is fixed for every codec.
- Constant hold: AC-3 and plain E-AC-3 1536 samples, DTS core 512, Auro 1000.
  TrueHD, JOC and HRA/DTS:X hold 0.
- DTS-HD MA holds up to 16384 samples at start.

**Prerequisites found.**
- Per-source pipelines: live and pipe frames share a tag today.
- Expose the generators' 1024-sample latency (phantom extraction, DirAC).
- The pipe carrier's bytes-per-frame must be learned from burst spacing.
- mpv `--ao-pcm-waveheader` must be skipped at epoch start.

### 6.5 Defects in today's code found by the spikes

- **S1:** today's DRIVER sink never fills `spa_io_position.clock`. Players
  see a frozen clock, mpv treats the sink's delay as zero, and mpv's
  `audio-pts` shows a 21 ms sawtooth. This is independent of the rewrite and
  is a quick-fix candidate for `main`.
- **S4:** partial-frame back-pressure pushes can rotate the channel order for
  the rest of a stream (`ring_buffer_io.rs`). The decoder tail is lost at EOF
  (no bridge flush).

### 6.6 Plan adjustments

- **Phase 1 simulator:** model 20 ms TrueHD batches, 32 ms E-AC-3 AUs, the
  DTS-HD MA start backlog, graph-clock excursions, mpv untimed throttling plus
  pipe buffering, and pause semantics (silence vs no data).
- **Phase 2:** adopt the S3 design and API sketch.
- **Phase 3:** add the cpal timestamp/position/latency patch as a sub-task,
  with Windows/macOS hardware verification.
- **Phase 4:** implement S1 option (c) for `own`. Add an ingest deframer that
  turns every IEC 61937 span into exact duration. Introduce per-source
  pipelines first.


## 7. Phase 1 outcome (2026-10-01)

The crate `omniphony-renderer/audio_sync` is pure, with no I/O and no
dependencies:

- `dll`: 2nd-order DLL with a narrowing bandwidth;
- `servo`: feed-forward + phase PI + the single start/realign rule;
- `accounting`: per-epoch counters and discontinuity events;
- `sim`: the closed-loop plant. It runs under `cfg(test)` or the `sim`
  feature. It records the **true** source-to-ear latency, which the servo
  never sees.

A trace tool:
`cargo run -p audio_sync --release --features sim --example sim -- <scenario> <seconds> <every> [target_ms] [bandwidth_hz]`.

Acceptance runs (all in `cargo test -p audio_sync`; 0.15 s in release, 1.4 s
in debug):

| Scenario | Median true latency | \|dev\| p99 / max | Ratio p-p (10 s) | Realigns | Floor |
|---|---|---|---|---|---|
| `own`, TrueHD, +100 ppm, **10 h** | 102.000 ms (target + 2 ms unreported device delay) | 0.1 / 1.7 µs | 0.54 ppm | 0 | 94.0 ms |
| `own`, TrueHD, −100 ppm, 1 h | 102.000 ms | 0.1 / 1.6 µs | 0.54 ppm | 0 | 94.0 ms |
| `own`, E-AC-3, target 150 ms | 152.000 ms | 0.1 / 0.3 µs | 0.53 ppm | 0 | 128.7 ms |
| `own`, −66 ppm graph excursion (S2) | 102.000 ms | 117 / 127 µs | 12.5 ppm (tracking lag) | 0 | 94.0 ms |
| `own`, 2 s source pause | 102.000 ms after | 0.2 µs | 0.52 ppm | 1 | — |
| `own`, reported 1-frame loss | 102.000 ms | 0.9 µs | 0.54 ppm | 0 | — |
| `own`, reported 107 ms resync loss | 102.000 ms after | 0.1 µs | 0.53 ppm | 1 | — |
| `follow` (mpv pipe, 23.976 fps bursts, 50 ms ahead, 5 ms jitter, +80 ppm), target 150 ms | 104.55 ms vs mpv's clock (constant) | 224 / 443 µs | 26 ppm | 0 | 117.5 ms |

### What the simulation settled

- **Phase-loop bandwidth depends on the mode.**
  - `own` (exact source): 0.02 Hz.
  - `follow`: 0.005 Hz (`ServoConfig::follow()`). Bursty arrivals leave the
    source DLL with about 70 µs of phase noise; a faster loop turns that into
    ratio wander. Sweep: 0.02 Hz → 64 ppm p-p, 0.01 → 37, 0.005 → 25 (p99
    0.25 ms), 0.003 → 19 (p99 0.38 ms).
  - 25 ppm is 0.04 cent; the `follow` criterion is set at 30 ppm.
- **Ratio criterion.** The 5 ppm p-p criterion holds on steady clocks. During
  a clock *change* (the −66 ppm excursion) the ratio lags by up to ~12 ppm
  while the latency stays within 0.13 ms. That is tracking, not modulation.
- **Minimum latency.**
  - `own` TrueHD at a 1024 quantum: about 94 ms (heard delay 32 + one
    callback 21 + capture quantum 21 + 20 ms batch). **The 100 ms used here is
    tight; default to ≥ 120 ms.**
  - E-AC-3: about 129 ms.
  - `follow`: about 118 ms (one video frame of audio arrives at once).
  - The servo publishes this as `Telemetry::latency_floor_s`, so a target
    below it can be reported instead of underrunning.

### Decisions (before Phase 4) — settled 2026-10-01

Both recommendations below were accepted: (1) refuse an unreachable target at
configuration time from per-codec floors, and fall back to playing at the
measured floor (reported) at runtime; (2) make unreported losses impossible
with the S4 per-burst decoder audit, plus the runtime guard that adopts a
persistent ring deficit as an unaccounted loss and flags it.

1. **Starvation when the target is unreachable.** Today the servo keeps
   underrunning and realigning; `an_infeasible_target_shows_in_the_floor`
   documents it. Options:
   - (a) raise the effective target to the floor and report it;
   - (b) stay silent and report;
   - (c) refuse the target at configuration time from per-codec floors.

   Recommended: (c), with (a) as the runtime fallback.
2. **Unreported losses.** A loss nobody reports leaves the servo blind. At a
   300 ms target the latency is off by the loss for good
   (`an_unreported_loss_shifts_the_latency_unseen`). At 100 ms, playback
   starves, because the ring no longer holds what the servo expects. The S4
   per-burst decoder audit must make this impossible. A runtime guard is
   also possible: adopt a persistent ring deficit as an unaccounted loss
   after ~0.5 s of a flowing source, and flag it.

## 8. Phase 2 outcome (2026-10-01)

The crate `omniphony-renderer/audio_rt` holds the two realtime primitives.

### `ring`: SPSC ring of interleaved `f32` frames

- Bulk two-segment copies, whole frames only. A push that does not fit is cut
  at a frame boundary and reports what went in, so the channel interleaving
  cannot rotate.
- Absolute `written`/`read` frame counters, published Release/Acquire. The
  servo reads `written()` as `available` and the resampler's read front as
  its consumption.
- Replaces the per-sample `ArrayQueue<f32>`, which cost one atomic CAS per
  sample.

### `resampler`: `DriftResampler`

The S3 design, hardened for production:

- `Design::for_rates`: 64 taps / β 14 for drift at ≤ 48 kHz; 32 taps at
  ≥ 88.2 kHz; 96 taps for conversions.
- Exact Q32.32 position (`position()` is `N_play`); `reset(at)` at an
  absolute ring frame.
- `skip(frames)` for the servo's start/realign. Fractional skips are
  allowed, and the frames passed over go straight to `Consumer::discard`
  without entering the history.
- No panics on the realtime path: `n_out`, ratio and destination size are
  clamped, not asserted.

### Measured (tests and `examples/bench.rs`)

**THD+N.** Every output sample is compared with the sine at the exact
reported position, so this bounds the filter error and the position error
together:

| Ratio | 1 kHz | 10 kHz | 20 kHz |
|---|---|---|---|
| 1.0 | −153.8 dB | −157.2 dB | −157.9 dB |
| 1 ± 100 / 500 / 2000 ppm | −139.4 dB | −139.4 dB | −133.9 dB |

**Other results:**
- Passband 100 Hz–20 kHz: within ±0.000001 dB.
- Ratio 1 at phase 0 is bit-exact.
- The position matches the Q32.32 closed form over mixed N and ratios.
- No allocation in a 24-channel steady-state loop with pushes, mixed N,
  ratio ramps and skips (counting allocator).
- The ring is lossless and ordered under a two-thread stress with odd chunk
  sizes.

**CPU** (x86-64 baseline build, one core):

| Channels | % of a core at 48 kHz |
|---|---|
| 2 | 0.14 % |
| 8 | 0.13 % |
| 16 | 0.19 % |
| 24 | 0.28 % |

Today's rubato path costs 2.6 % at 24 channels.

**Not done here:** the aarch64 codegen check from S3 §3.3. It moves to
Phase 7 hardware validation. The quality tests take 12 s in debug builds (CI).

## 9. Phase 3 outcome (2026-10-01)

The new output stage is the module `audio_output::sync_output`. It lives
beside the legacy regulation until the cutover.

### What was built

- **`core::OutputCore`**: the callback body shared by every backend.
  - It asks the `Servo`, then executes the plan: leading silence, a skip whose
    passed-over frames are `Consumer::discard`ed, then exactly the planned
    frames through the `DriftResampler`.
  - It applies the fade-in and fade-out and maps ring channels onto device
    channels.
  - A short ring read (a producer bug) restarts the resampler at the read
    front, which the servo sees as a latency jump and realigns.
  - It neither allocates nor blocks.
- **`source_tap::SourceTap`**: the capture side's `(t, received)` as a
  seqlock with a bounded read (the reader never waits), plus the accounting
  offset.
- **`telemetry::SyncTelemetry`**: relaxed atomics carrying phase, latency,
  error, floor, source/device/feed-forward/correction ppm, realigns,
  underruns and short reads.
- **`clock::reference_now_s`**: `CLOCK_MONOTONIC` on Unix, the same axis as
  `pw_time.now`.
- **`pipewire::PipewireSyncOutput`**: a playback stream with `RT_PROCESS`.
  Timing comes from one `pw_stream_get_time_n` per cycle: `now`, `ticks`, and
  heard delay `(buffered + N)/fs + delay·rate`.
- **`cpal_adapter::CpalSyncOutput`** (ASIO/CoreAudio): the time is the
  reference clock at callback entry, the position is the frames delivered,
  and the delay is cpal's `playback − callback`. Native sample formats are
  converted from a scratch buffer sized once. It is type-checked on Linux
  through the `cpal-check` feature (ALSA host); a Windows cross-check is
  impossible here because `asio-sys` bindgen fails for `x86_64-pc-windows-gnu`.

### Verified

1. **The real core in closed loop** (`tests/sync_output_closed_loop.rs`). The
   source pushes a ramp whose value is its own frame index, so the audio the
   device plays gives the true latency through the real resampler:

   | Device | Decoder bursts | Target | Worst \|latency − target\| over 90 s | Realigns / underruns / short reads |
   |---|---|---|---|---|
   | +100 ppm | TrueHD | 120 ms | 51 µs | 0 / 0 / 0 |
   | −100 ppm | E-AC-3 | 150 ms | 40 µs | 0 / 0 / 0 |

   About 10 µs of that is the f32 ramp's resolution.

2. **Live on PipeWire** (`examples/sync_pw_live.rs`). The stream played into
   a private `support.null-audio-sink` linked to no hardware, with the
   session-default sink unchanged and no node left behind. The source was
   synthetic, +100 ppm, in 960-frame bursts; target 150 ms; 120 s:

   | Measure | Result |
   |---|---|
   | Source rate measured | +100.00 ppm |
   | Device rate measured | +0.02 ppm |
   | Latency held | 150.000 ms (worst \|error\| 0.001 ms after 40 s) |
   | Correction | settled to 0.00 ppm |
   | Realigns / underruns / short reads | 0 / 0 / 0 |

   The published latency floor is a peak-hold: it rose with the synthetic
   producer's scheduling stalls (it is not an RT thread). Received-minus-pushed
   stayed bounded below one batch.

### Still open in Phase 3

- **The cpal patch.** cpal drops ASIO's `samplePosition`/`systemTime`,
  CoreAudio's `mSampleTime`/`mHostTime`/`mRateScalar`, and both hosts'
  latencies. With the patch, the adapter can use device positions instead of
  callback-entry time.
- **Hardware runs.** None of the following has run yet: ASIO, CoreAudio,
  PipeWire on a real DAC with real input. Real DAC and real input belong to
  Phase 4, with the source modes.

## 10. Phase 4 — host decision and slices (2026-10-01)

**Decision: the new sync host is built on `orender_engine::Engine`**, the
decode-and-render pipeline that liborender/mpv already use. It will replace
`src/cli/decode/*` at the cutover, so mpv and the CLI end up with **one**
render host. The two-host divergence (e.g. #250) disappears with it, and the
legacy handler's drop points E1–E9 (S4) are not ported at all.

What the Engine gives:
- bytes in;
- rendered blocks out, carrying their absolute `sample_pos`;
- OSC/Studio already wired.

What the host adds:
- the capture side (reader, deframer, clock tap);
- the ring and the sync output;
- the CLI-only features: device choice, latency target, test idle feed, file
  output.

Slices, each testable end to end:

- **4a — `follow` for the mpv pipe.**
  - A reader thread that never blocks on downstream, timestamps every arrival
    on the reference clock, and hands chunks to an engine thread.
  - The engine thread parses IEC 61937, decodes and renders through the
    Engine, pushes blocks into the ring, and publishes
    `(arrival time, frames available)` to the `SourceTap`.
  - `N_in` at this slice is the decoded position at the arrival of the bytes
    that completed it, plus the codec's constant hold (S4 table). It is exact
    in rate; in absolute latency it is exact up to that table. 4d replaces it
    with transport-time counting.
  - An epoch per stream (pipe open → EOF).
- **4b — `none` for files and faster-than-real-time writers.** Back-pressure
  at `L`, the ratio stays nominal.
- **4c — `own` for the PipeWire sink.** S1 recipe: a timer driver plus a
  follower sink, Latency param, `node.force-rate`, full-quantum Buffers.
- **4d — accounting.** A time-conserving IEC 61937 deframer (pause and
  stuffing become silence), a per-burst decoder audit, and
  `DiscontinuityEvent`s.

## 11. Phase 4a status (2026-10-01) — pipe `follow` slice, end to end

`orender sync-play <fifo>` is a hidden command, Linux only.

### What is built

```
reader thread ─► engine thread: IEC 61937 deframe ─► Engine ─► ring ─► OutputCore ─► PipeWire
```

- **Reader thread.** It opens the FIFO read-write, so the pipe always has a
  writer: reads never return end-of-file between streams, and mpv never gets
  `SIGPIPE`. It polls, stamps every chunk on the reference clock, and never
  waits on downstream: a full queue counts a dropped chunk. A stream starts
  with its first byte and ends after 0.5 s of silence.
- **Engine thread.** It deframes IEC 61937 (`SpdifParser`), then decodes and
  renders through `orender_engine::Engine`.
- **Ring.** Blocks are pushed into the SPSC ring.
- **Clock tap.** `(arrival time, frames pushed + codec hold)` goes to the
  `SourceTap`.
- **Epochs.** One epoch per stream: its own ring, servo and PipeWire stream.
  An end of stream is a deadline, never a wait.
- **Diagnostics.** `ORENDER_SYNC_TRACE=1` prints one line per chunk.

### Live, real mpv

`mpv --no-config --vo=null --audio-spdif=eac3 --ao=pcm` → FIFO, E-AC-3 5.1 at
23.976 fps, into a private null sink (nothing audible):

| Duration | Result |
|---|---|
| 180 s | 8 552 448 frames pushed (= 178.2 s), no chunk or ring drop |
| After 30 s | latency 200 ± 0.4 ms; correction within ±30 ppm |
| Start-up | 1 realign; correction saturated at +500 ppm around 15 s |

### Finding: the follow source's jitter is one-sided

Measured arrival pattern from mpv: no start-up burst (mpv stays 50–90 ms
ahead), but **±20 ms of arrival jitter**, always late. The consequences, in
the closed-loop simulation reproducing it:

1. **A DLL on raw arrivals is the wrong estimator.** It gave ±5 ms and
   545 ppm, and a true latency that drifted by 6 ms as the mean-lateness bias
   settled.
2. **The fit that works.** The source's line is fitted to the **earliest
   arrivals**: the upper convex hull over 30 s, picking the edge at the mean
   time (Moon/Skelly/Towsley LP). A DLL at 0.01 Hz tracks that line. This
   removes the bias at any drift, including 1000 ppm (display-resample).
3. **Latency is now measured against the source's true clock.** The buffer
   must therefore also cover the lateness: the latency floor for mpv E-AC-3
   through the pipe rises from ~123 ms to ~155 ms. Default the `follow`
   target to 200 ms.
4. **Tried and rejected,** each measured:
   - a nominal-rate min-filter fed to a slow DLL (fine at 0 ppm, minutes to
     learn 80–1000 ppm);
   - an envelope drawn at the DLL's own rate (runs away: it hands the DLL its
     own slope);
   - rate reseeding (steps);
   - a low-passed hull rate without the DLL (rate and phase disagree).

### Open item (blocks the 4a acceptance)

At the measured jitter, the simulation gives a p99 latency deviation of
0.75 ms and ~85 ppm peak-to-peak ratio wander. The criteria are 0.5 ms and
30 ppm. The test `follow_mpv_pipe_with_measured_jitter` is `#[ignore]`d with
these numbers.

The wander is slow (0.08 cent at most, far below audibility), but the
criteria were set on purpose and are not relaxed here. The start-up transient
(first ~30 s) is part of the same item.

Two leads:
- (a) a joint position/rate estimator built for one-sided noise (a Kalman
  filter with a censored-noise update, or an LP fit with explicit rate
  continuity);
- (b) for video playback, steer users to the `own` mode (4c). Through the
  PipeWire sink, mpv's AO is timed by orender's own clock: no lateness, an
  exact clock (S1).

## 12. Windows constraint (2026-10-01)

On Windows there is no PipeWire: a standalone orender fed by mpv can only
take its input from a **pipe**. So the `follow` open item of §11 is not
optional, and two tracks follow from it.

- **Track 1 — a `follow` estimator that meets the criteria** under one-sided
  jitter: a joint position/rate estimator for censored noise (Kalman or LP
  with rate continuity). It serves any untimed writer (mpv `--ao=pcm`, VLC,
  ffmpeg).
- **Track 2 — "own over a pipe".** The ±20 ms comes from `ao_pcm` being
  untimed: mpv throttles audio per video frame. A **timed** pipe AO in
  mpv-omniphony (patch-based, see the release model) would write on the
  system clock in small periods and report a fixed delay `L`. mpv's audio
  clock would then be the system clock, display-resample would work against
  it, and orender would receive near-jitter-free arrivals on a known clock:
  the S1 result, through a pipe.
- **Windows specifics, not yet done:**
  - the reference clock (QPC) for the sync host;
  - the named-pipe reader (`sys::input` has an overlapped server);
  - the cpal/ASIO adapter with the timestamp patch (§9);
  - measuring the Windows pipe's own arrival pattern. The legacy decoder
    thread already logs "below real-time delivery (mpv ao=pcm on Windows)".

Order: 4c (PipeWire `own`) now, then Track 1, which is needed on every
platform. Track 2 is worth doing as soon as mpv-omniphony is touched for
Windows.

## 13. Phase 4c status (2026-10-01) — `own` sink on PipeWire

`orender sync-play --source own [--sink-name N]` publishes orender's own sink,
clocked by orender's own timer driver (S1 option c).

### What is built

- **Driver.** A `support.node.driver` node on `CLOCK_MONOTONIC` in a private
  `node.group`.
- **Sink.** The IEC 958 sink stream follows it: `RT_PROCESS` and
  `node.always-process`, no `DRIVER` flag.
  - It advertises `SPA_PARAM_Latency = L` once.
  - It sets `node.force-rate` to the carrier rate, written only when the value
    changes: writing it renegotiates, and rewriting it on every
    `param_changed` looped 400 times.
- **Realtime callback.** It drains **every** queued buffer of the cycle and
  only copies:
  - the bytes into an SPSC byte ring (`audio_rt` ring, now generic);
  - one stamp per cycle (`clock.nsec`, bytes so far, format, epoch);
  - DISCONT/XRUN_RECOVER and real format changes start a new epoch.
- **Pump thread.** It turns stamps and bytes into the same messages the pipe
  reader produces, with the exact transport position attached.
- **Engine thread.** In `own` mode, `N_in` is **transport time**: bytes on
  the carrier converted to PCM frames, counted from the first burst's
  preamble. The parser now reports each burst's absolute `start_byte`. The
  decoder's hold and batching are inside the measurement, as they are inside
  the latency.

### Two traps found on the way

1. **One Buffers param with ranges, not one per format.** S1 recipe:
   buffers 2–16, size up to 8192×16 B, stride 1–16. With one fixed-stride
   param per format, PipeWire paired a format with another one's stride: mpv
   was asked for 2048 frames of a 4096-frame cycle and delivered half real
   time (`src = −500 000 ppm`, realigns every 70 ms).
2. **A player may queue several buffers per cycle.** The callback drains them
   all.

### Live, real mpv

`--ao=pipewire --audio-device=pipewire/<sink> --audio-spdif=eac3`, E-AC-3 5.1
on a 192 kHz carrier, into a private null sink:

| Duration | Latency | Source | Correction | Realigns / underruns / drops | Frames | Floor |
|---|---|---|---|---|---|---|
| 180 s | **150.000 ms, max \|error\| 0.001 ms** | 0.00 ppm | 0.00 ppm | 0 / 0 / 0 | 8 640 000 (= 180.0 s exactly) | 65 ms |

- The source's clock is orender's own, so there is nothing to estimate and
  the loop does nothing.
- The latency floor is 65 ms: `own` can run far lower than the pipe.

### Not yet

- **PCM** (desktop audio): the raw format is not offered by the `own` sink
  yet.
- **AC-3/DTS at 48 kHz, TrueHD/DTS-HD at 192 kHz × 8**: offered, not exercised
  live yet.
- **mpv's own view of the latency** (`audio-pts` over IPC, as in S1): not
  re-measured with the full host.
- **The real DAC** as output (only the null sink so far).

## 14. Track 1 — the `follow` estimator (2026-10-01)

### Method

The estimator was redesigned **open-loop first**, then validated in closed
loop. A Python harness reproduces the measured mpv arrival model:
- 23.976 fps bursts, 50 ms ahead, 0–40 ms one-sided lateness;
- drift at 0, ±80, +300 and ±1000 ppm, and a 0→500 ppm ramp.

It scores each candidate's position error and rate noise against the truth.

### Why the hull edge was wrong

The previous estimator fitted the upper convex hull and extrapolated its
mean-time edge to the present. Two flaws:
- the edge's slope rests on two points;
- the extrapolation spans half the window, so the slope error is amplified.

Open-loop it gave 0.5–1.9 ms p99 and 27–86 ppm.

### What replaced it (`audio_sync::source::FollowEstimator`)

Each quantity is estimated over the span suited to it:

- **Rate**: the hull's slope over **120 s**, low-passed (τ grows with the
  fit's span, up to 10 s).
- **Offset**: the earliest arrival of the **last 30 s** against a line at that
  rate. No extrapolation. Its error is the window's smallest lateness, whose
  p99 is ≈ jitter·ln(100)/N. It rises at once and relaxes down over 15 s: a
  1 s relax let an expiring earliest reading step the phase.

Open-loop, after 100 s: position p99 0.33–0.47 ms, rate within 7 ppm p-p, at
every drift tested.

### Servo changes

- **The phase loop narrows** for `follow`: 0.02 Hz at start, halving every
  15 s, down to 0.005 Hz.
- **The integral now accumulates `∫Ki·e dt`**, so narrowing gains do not
  rescale what was integrated.
- **The example's `mpv` scenario now takes the full `follow` config.** Before,
  it silently missed the new fields.

### Closed loop (steady state from 60 s, 900 s runs, measured jitter, 200 ms target)

| Source drift | Latency p99 / max | Ratio p-p (10 s) | Realigns |
|---|---|---|---|
| 0 ppm | 0.84 / 0.98 ms | 19.7 ppm | 0 |
| +80 ppm | 1.01 / 1.16 ms | 19.8 ppm | 0 |
| +1000 ppm | 0.82 / 0.98 ms | 19.9 ppm | 0 |
| −1000 ppm | 1.11 / 1.15 ms | 19.6 ppm | 0 |

Before: 0.75 ms p99 and 85 ppm, at 80 ppm only.

- **The ratio criterion (30 ppm) is met at every drift.**
- **The latency deviation is ~1 ms.** It comes from the minimum-lateness
  statistics: the earliest reading of a 30 s window entering and leaving, and
  the slope settling over the first minutes. A longer offset window did not
  help (0.83–0.95 ms at 45–60 s).
- **The first minute is convergence:** up to ~1.5 ms and a few hundred ppm in
  the first 30 s at ±1000 ppm.

### Live (real mpv `--ao=pcm` → pipe, E-AC-3, 200 ms target, 180 s)

- The source rate estimate is steady at **−6.2 to −6.6 ppm**. Before it
  wandered between −11 and −30 ppm.
- The latency error converges from −0.9 ms to −0.1 ms over the run.
- No drops.
- Start-up: 2 realigns and 1 underrun in the first 15 s.

### Decision (2026-10-01): `follow` p99 criterion = 1 ms

For reference, lip-sync detectability is about ±15 ms, and ITU-R BT.1359
tolerates +45/−125 ms.

Measured over a steady state starting at **2 min** (where the test now
starts), the p99 is **0.45–0.47 ms** at every drift (max 0.48 ms). The ~1 ms
quoted above was the 60–120 s convergence. The first minute is convergence:
- up to ~1.2 ms from 60 s;
- ~1.5 ms and a few hundred ppm in the first 30 s at ±1000 ppm.

Track 2 (a timed pipe AO) is meant to shorten that convergence and remove
the start-up realigns.

## 15. Track 2 — a timed pipe writer (2026-10-01)

### What is built

- **mpv: `--ao-pcm-timed=yes`** (mgth/mpv, branch `feat/timed-pcm-ao` off
  `orender`). `ao_pcm` behaves as an audio device playing at the nominal rate
  on mpv's clock:
  - a virtual buffer of `--ao-pcm-buffer` (default 40 ms) drains on `mp_time`;
  - mpv's AO thread refills it every quarter of it (10 ms), and each write is
    flushed to the pipe at once;
  - the reported delay is `--ao-pcm-latency` (the sink's target latency),
    counting down between writes. mpv's A/V sync therefore matches what is
    heard.

  Audio is paced like a sound card output and video syncs to it. The
  untimed writer, by contrast, runs as fast as the player's throttle lets it.
- **orender: a pipe counts in transport time.** The sync host now counts a
  pipe's `N_in` in carrier bytes from the first burst, as the `own` sink
  does. The carrier comes from the IEC 61937 data type, as mpv's spdif
  wrapper sets it:
  - AC-3 and DTS core: 48 kHz × 2;
  - E-AC-3: 192 kHz × 2;
  - TrueHD/MAT: 192 kHz × 8.

  DTS-HD (2 or 8 channels) and raw PCM keep the decoded-frame count.

### Why the transport count was needed

With timed arrivals the remaining start-up error was the decoder's burst
granularity. Decoded frames grow in 32 ms E-AC-3 steps, but chunks arrive
every 10 ms. Until a chunk boundary happened to fall on a burst boundary,
the follow offset sat up to a chunk below the true line. It then rose
(+8 to +12 ms) just after `establish`, and the loop took ~40 s at its
500 ppm clamp to remove it. Counting bytes makes every reading exact.

### Live (real mpv → FIFO, E-AC-3, private null sink, 180 s)

| | untimed `ao_pcm` | timed `ao_pcm` |
|---|---|---|
| Writes | 32 ms, every 42 ms | 10 ms, every 10.06 ms (p99 10.09) |
| Arrival lateness p99 (after 5 s) | 66.8 ms | **0.03 ms** |
| Arrival lateness p99 (first 5 s) | 329 ms | **0.03 ms** |
| Latency error, 5 s → 180 s | +5.7 → −0.9 → −0.1 ms | **within ±0.03 ms** |
| Phase correction | +500 ppm (clamped) for 10 s | ≤ 6 ppm |
| Realigns | 1 | **0** |
| Latency floor | ~140 ms | **~86 ms** |

- **At a 100 ms target** (60 s, timed), the error stays within ±0.03 ms,
  with 0 realigns and an 80 ms floor. Untimed cannot run there: its floor
  is above 100 ms during start-up.
- **Start-up convergence is gone.** The latency is exact from the first
  callback; only the source rate estimate settles, over ~15 s, and it does
  not move the latency.
- **The source reads −8.7 ppm, not 0.** mpv's timer is `CLOCK_MONOTONIC_RAW`
  (`osdep/timer-linux.c`), while orender's reference is `CLOCK_MONOTONIC`,
  which NTP slews. That is a genuine foreign clock, and `follow` tracks it.
  The same holds for untimed mpv (−6 to −9 ppm).
- The single underrun in each run is the end of the stream.

### Open items

- **Release (2026-10-01).** The timed `ao_pcm` is in place everywhere except
  FEL:
  - **mpv fork:** merged into `orender` (mgth/mpv#20) and ported to
    `orender-master` (mgth/mpv#21), with the other pending integration
    commits.
  - **mpv-omniphony:** both patch sets are regenerated in
    mgth/mpv-omniphony#79. This also repairs the live-master build: the
    manual `9001-f_swresample` was obsolete upstream.
  - **FEL beta:** covered by the same `patches-master/`. FEL has been native
    in mpv master since mpv-omniphony #53, so the beta is "mpv master +
    `patches-master/`". A local FEL build against libplacebo and ffmpeg
    master builds and plays timed. (`feat/dv-fel` is a stale branch,
    already contained in `main`.)
  - **Dev launchers:** `mpvo` and `mpvo-fel` take
    `--orender-pipe[=PATH] [--orender-latency=MS]`. This swaps in-process
    decoding for passthrough to a FIFO, through the timed `ao_pcm`.
    - PATH defaults to `$XDG_RUNTIME_DIR/orender-sync.pipe` (never the
      production `/tmp/orender.pipe`), and MS to 150.
    - A missing FIFO is created; a path that exists and is not a FIFO is
      refused.
    - A build without the timed mode is refused.
    - Checked live through `mpvo`, with the user's `mpv.conf` (`ad=orender`)
      in force: passthrough is still chosen, and the latency holds at
      200 ms ±0.02 ms with 0 realigns.
  - No product launcher uses the pipe path yet. The Studio launches mpv with
    the in-process engine, and `sync-play` is still hidden.
- **Windows.** The writer side carries over unchanged (mpv's timer is QPC).
  The orender side still needs the QPC reference clock and the named-pipe
  reader (§12).
- **DTS-HD over a pipe** keeps the decoded-frame count. Telling HRA (2 ch)
  from MA (8 ch) needs the burst's repetition period read against the core
  frame size, or a WAV header from the writer (`--ao-pcm-waveheader=yes`
  carries the carrier format exactly, raw PCM included).

### Pause and seek (2026-10-01)

Scripted over mpv's IPC: pauses of 0.2, 0.4 and 3 s, then seeks of +60, −30
and +2 s, with timed mpv against `sync-play` at 200 ms.

**Before the fixes, both broke `follow`.**
- **A 0.2 s pause deadlocked the servo.** That gap is under the 0.25 s stall
  threshold, so it was taken for jitter, and the line kept extrapolating
  past data that had stopped. The ring ran dry. The realign then had to skip
  to frames that would never come, and stayed silent for 18 s: latency grew
  1 s per second and the ring overflowed. It only ended with the next epoch.
- **Seeks drove the source rate to +1350 ppm.** Each seek was a clean
  +19.5 ms step ahead, because `ao_pcm`'s `reset()` emptied the virtual
  buffer. The step sat just under any jitter threshold, so the young slope
  fit (14 s of span) spanned it.

**Fixes.**
- **`FollowEstimator` detects phase breaks** and keeps the rate through them:
  - a reading more than **20 ms ahead** of the line restarts the slope fit
    (the offset already rises at once);
  - readings more than 20 ms behind for **0.5 s** start a new phase from the
    first of them;
  - **the reading after an underrun** starts a new phase at once.

  Untimed mpv never stays more than 20 ms late for longer than 84 ms in
  steady state, so neither condition fires on it (checked live: no spurious
  rephase, same start-up as before). The count is reported as `rephases`.

  The ahead threshold is deliberately not lower. While the slope fit is
  young, a 1000 ppm source drifts several ms off the line within seconds, so
  a few-ms threshold would keep restarting the fit.
- **mpv: `reset()` keeps the virtual buffer**, and its clock runs whether or
  not the player counts the AO as playing. What was written cannot be taken
  back: the reader plays it out. A seek is then continuous for the reader,
  and mpv's delay accounts for the old audio still ahead of the new.

**After (live, same script):**

| Event | orender | mpv `avsync` |
|---|---|---|
| Pause 0.2 s / 0.4 s | 1 underrun, 1 rephase, 1 realign each; then 200.005–200.010 ms | 0 |
| Pause 3 s | epoch ends; resume opens a new one at 200.007 ms | 0 |
| Seek +60 / −30 / +2 s | **seamless**: 0 realigns, 0 rephases, error < 0.015 ms throughout | −8 / +3 / −8 ms, settled in ~2 s |

**What a pipe cannot avoid.** orender cannot see the player's pause or
seek, only the bytes. So:
- the last `L` of audio already sent still plays after the user pauses or
  seeks (200 ms at a 200 ms target);
- after a short pause the realign leads with silence.

A/V stays right, because mpv's delay is defined from the write. Removing the
tail needs a side channel (a flush command), which is out of scope for a
plain pipe.

Tests:
- `a_short_pause_of_a_follow_source_realigns_and_resumes` (servo): fails
  without the phase-break handling, stuck at 31.2 s as live;
- `a_source_that_loses_time_starts_a_new_phase`,
  `a_step_ahead_moves_the_offset_and_spares_the_rate`,
  `jitter_is_not_a_phase_break`,
  `the_reading_after_an_underrun_starts_a_new_phase` (estimator).

### Audio track switch (2026-10-01)

Test file: three audio tracks: E-AC-3 "A", AC-3 (re-encoded from it), and
E-AC-3 "B" (a copy of A). Played with `--audio-spdif=ac3,eac3`, switching
A → B → AC-3 → A → AC-3 → B every 15 s over IPC.

**Same codec (A → B): seamless.** mpv keeps its AO, so the reader sees the
same as a seek: no realign, no error.

**Codec change: three defects, fixed one after the other.**

1. **The pipe's carrier was learned once.** AC-3 rides a 48 kHz carrier,
   E-AC-3 a 192 kHz one, and AC-3 bytes were counted as E-AC-3, at a quarter
   of their rate. The latency slid away, the estimator rephased every 0.5 s,
   and 900 k frames were dropped. **Fix:** the carrier follows each burst's
   data type.
2. **A sub-threshold step corrupted the rate.** For a codec change mpv
   rebuilds its AO: a new `ao_pcm` with an empty virtual buffer, so the
   arrivals step by a few ms (+7 ms here). That is under the 20 ms
   threshold, so the slope fit spanned it and the source rate went to
   +230 ppm. **Fix:** orender knows a new stream started (the IEC 61937 data
   type changed).
   - The sync host marks a **source break** on the `SourceTap`.
   - The servo fades out and restarts the phase on the next fresh reading
     (the `follow` fit or the `own` DLL alike).
   - It stays silent until that reading has come, then sets the latency
     exactly.

   The content is discontinuous there anyway, so a realign costs nothing
   audible that the track change did not already cost.
3. **The floor crept up at each change** (+40 to 80 ms), until a 200 ms
   target underran in a loop. The old stream's last frames were counted on
   the transport but never came out of the decoder. **Fix:** at a break the
   transport count restarts from the frames actually pushed into the ring,
   as at the start of an epoch.

The servo now owns "restart the phase on the next reading" for every cause:
- a gap (the stall gap, as before);
- a `follow` underrun;
- a reported break.

The estimator counts only the breaks it detects itself. `rephases` reports
the sum.

**After (live, 4 codec changes + 1 same-codec switch):**
- each codec change costs 1 realign and 1 rephase, with the error at most
  0.02 ms throughout;
- the source rate stays at −8.5 ppm;
- the floor holds per codec (~80 ms E-AC-3, ~54 ms AC-3);
- no mid-stream underrun, no drops.

mpv's own `avsync` reads −9 to −41 ms right after a switch and settles under
1 ms in ~1.5 s. With mpv's ideal `ao_null` the same switches read −19 to
−27 ms with the same decay, so this is mpv's track-switch resync, not the
pipe.

The pause/seek script gives the same results after this change.

Test: `a_reported_break_restarts_the_phase_and_realigns` (servo). Without the
break handling, a 7.2 ms error persists.

