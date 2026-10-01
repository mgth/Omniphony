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

