# Spike S2 — Output timestamps per backend

Phase-0 spike of [the resampling rework plan](../resampling-rework-plan.md)
(§2.2 "Device delay", §2.3 "Feed-forward `r_ff`", §4 Phase 0 S2).

The question: at each output callback the servo needs

1. a timestamp `t` in the common reference clock M,
2. the DAC position (frames) at `t`,
3. the device delay `D_dev` until a frame written now is heard,

so that it can (a) estimate the DAC rate against M (feed-forward `r_ff`) and
(b) compute `L_e2e`. This document says what each backend provides, how good
it is, and the exact recipe to use.

- PipeWire was **measured** on this machine (PipeWire 1.6.9, real USB DAC).
- cpal ASIO (Windows) and CoreAudio (macOS) could not be measured here. They
  were **researched** from the cpal 0.15.3 source the renderer uses
  (`omniphony-renderer/Cargo.toml`: `cpal = "0.15.3"`, `asio-sys 0.2.6` in
  `Cargo.lock`), cpal 0.17.3, and the ASIO SDK headers in
  `reference-sources/asio_sdk/`.

Spike code: [`spikes/s2-output-timestamps/`](../../spikes/s2-output-timestamps/)
(standalone cargo project, `pipewire 0.9.2` like the repo; Python analysis).

## 0. Summary

- **PipeWire gives everything the servo needs, from one call.**
  `pw_stream_get_time_n` in the process callback returns:
  - `now`: `CLOCK_MONOTONIC`, the cycle start;
  - `ticks`: frames, continuous;
  - `delay`: frames to the device;
  - `queued` and `buffered`.

  The driver's `spa_io_position.clock` (`nsec`, `position`, `rate_diff`,
  live `delay`, `xrun`, `flags`, `id`) is reachable from a plain `pw_stream`
  through `io_changed(SPA_IO_Position)`. Verified on PipeWire 1.6.9.
- **`now` is already smoothed.** For an ALSA driver in timer mode, `now` is
  the driver DLL's *scheduled* wake-up time (`state->next_time`), not a
  measurement. Its cycle-to-cycle jitter is 0.04–0.8 µs. The callback itself
  runs 23–33 µs later (p99 ≤ 56 µs, max 318 µs).
- **Use our own DLL on `(now, ticks)`, not raw `rate_diff`.**
  - Raw `rate_diff` per cycle has 7–39 ppm rms noise, up to ~950 ppm p-p.
  - It swings by thousands of ppm during PipeWire's first ~10 s.
  - Its smoothness changes over time, because PipeWire adapts its DLL
    bandwidth every 3 s.
  - A 2nd-order DLL with fast-start bandwidth (1 Hz narrowing to `B`) locks
    within 2 ppm in **12–16 s**.
  - At **B = 0.01 Hz** its steady-state output is 0.2–0.8 ppm rms (1–4 ppm
    p-p). At 0.03 Hz it is 0.7–1.3 ppm rms, and at 0.1 Hz 2.4–2.6 ppm rms.
  - Recommended: **B = 0.01 Hz**, fast start, reset on clock-id change or
    discontinuity. The same code serves ASIO and CoreAudio.
- **DAC rate on this machine (USB adaptive endpoint)** against
  `CLOCK_MONOTONIC`: −16.8 to −17.2 ppm in quiet runs. That is −6.2 to
  −6.4 ppm against `CLOCK_MONOTONIC_RAW`. The difference is +10.6 to
  +10.9 ppm of NTP frequency correction applied to `CLOCK_MONOTONIC`.
  - The kernel's own `hw_ptr` agrees to 0.03 ppm over 12.5 min. It shows the
    DAC itself stable to **0.03 ppm** at 120 s.
  - The graph clock that PipeWire presents (`ticks` vs `now`) wanders
    more: 0.3–0.6 ppm at 60–120 s in quiet runs. Twice in about 41 min, both
    at quantum 1024, it made **large excursions**: −6.5 ppm for minutes, and
    **−66 ppm for ~100 s** (a ~250-frame phase walk), then a slow return. This
    is PipeWire's ALSA DLL re-centring the hardware fill level, not the DAC.
    **The simulator must include it.**
- **`delay` is static.** It is the downstream Latency param: quantum
  + ALSA headroom (here 1024 + 512 and 256 + 128). It equals the mean of the
  live ALSA fill (`spa_io_clock.delay`). The live fill is noisy: USB packet
  granularity ±4 frames, and +quantum/4 spikes in 0.6–1.9 % of cycles.
  - The total delay for the first frame queued in the callback is
    `(buffered + N + delay)/fs` after `now`. pw_stream playback is one
    cycle behind: `queued` is 0 before queueing and N after.
  - The USB/DAC internal delay is **not** included in any reported number.
- **cpal hides what ASIO and CoreAudio provide.**
  - ASIO: cpal passes only the driver `systemTime`. That is often 1 ms
    resolution and in `timeGetTime`'s epoch, which cannot be mapped to QPC.
    It drops `samplePosition`, and has no `ASIOGetLatencies` binding.
  - CoreAudio: cpal passes `mHostTime`. That is the buffer's *output* time,
    in the same clock as `Instant` (`CLOCK_UPTIME_RAW`). It drops
    `mSampleTime`/`mRateScalar` and reads no device or stream latency.
  - In both, `timestamp().playback` is "callback + one buffer", not a device
    delay.
  - cpal 0.17.3 does not change this.
  - Recommendation: a small patch to cpal/asio-sys (or thin native adapters)
    to forward the raw timestamp, the position and the latencies.

## 1. Availability per backend

| Need | PipeWire (`pw_stream`) | ASIO via cpal 0.15.3 | ASIO native (SDK) | CoreAudio via cpal 0.15.3 | CoreAudio native (AUHAL/HAL) |
|---|---|---|---|---|---|
| Timestamp in M | ✅ `pw_time.now` (`CLOCK_MONOTONIC`, cycle start, DLL-smoothed) | ⚠️ `callback` = driver `systemTime`: `timeGetTime` epoch (≠ QPC), often 1 ms resolution. Use our own QPC at entry instead | ⚠️ `systemTime` at the buffer switch, same caveats | ✅ `callback` = `mHostTime` (= `Instant` clock), but it is the **output** time, not the callback time | ✅ `mHostTime` |
| DAC position | ✅ `pw_time.ticks` (driver `clock.position` − base; continuous; rebased on clock change/DISCONT) | ❌ not exposed; count frames | ✅ `samplePosition` (block-aligned, since `ASIOStart`) | ❌ not exposed; count frames | ✅ `mSampleTime` |
| Device delay | ✅ `pw_time.delay` (static: quantum + headroom) + `queued`/`buffered`; live fill in `spa_io_clock.delay` | ❌ (`playback` = callback + 1 buffer: a placeholder) | ✅ `ASIOGetLatencies` output latency (+ `kAsioLatenciesChanged`) | ❌ (`playback` = callback + 1 buffer) | ✅ `kAudioDevicePropertyLatency` + `kAudioStreamPropertyLatency` (+ safety offset/buffer for callback-relative delay) |
| Device rate vs M | ✅ `spa_io_clock.rate_diff` (noisy per cycle), or DLL on `(now, ticks)` | DLL on `(QPC, Σframes)` | DLL on `(QPC or systemTime, samplePosition)` | DLL on `(mHostTime, Σframes)` | `1/mRateScalar` (if flagged valid) or DLL on `(mHostTime, mSampleTime)` |
| Discontinuities | ✅ `clock.id`, `SPA_IO_CLOCK_FLAG_DISCONT`/`XRUN_RECOVER`, `clock.xrun`, ticks step ≠ duration | ❌ | ✅ position step; `kAsioResyncRequest` | ❌ | ✅ `mSampleTime` step |

✅ provided · ⚠️ provided with caveats · ❌ missing.

## 2. PipeWire measurements

### 2.1 Setup

- **Host.** PipeWire 1.6.9, WirePlumber, kernel 7.2 (CachyOS), TSC
  clocksource. `systemd-timesyncd` disciplines the clock: kernel frequency
  correction +10.59 ppm, PLL mode.
- **Sink.** `alsa_output.usb-Generic_USB_Audio-00.HiFi_5_1__Speaker__sink`
  (on-board USB audio, `hw:3,0`, S32LE 6 ch 48 kHz).
  - The playback endpoint is **ADAPTIVE** (`/proc/asound/card3/stream0`):
    the DAC clock is recovered from USB SOF.
  - ALSA timer scheduling (tsched), `api.alsa.headroom = 512`,
    `period-size 512`, `htimestamp` off.
  - The spike's stream was the only follower: verified with `pw-link`. The
    card's other PCMs stayed closed: logged every 2 s during run D.
- **Stream.** `rwspike-s2-q<N>`.
  - F32LE 2 ch 48 kHz (graph rate, so no stream resampling).
  - `node.latency = N/48000`, `target.object = <sink>`,
    `node.dont-reconnect`/`dont-fallback`, `RT_PROCESS`.
  - It writes **all-zero buffers**, `requested` frames per cycle, and sets
    `pw_buffer.size` in frames.
  - Per callback it records: `CLOCK_MONOTONIC` and `CLOCK_MONOTONIC_RAW` at
    entry, `pw_time` before dequeue and after queue, and the driver's
    `spa_io_position.clock`. `io_changed(SPA_IO_Position)` delivered the area
    (1688 bytes) before `Streaming`.
  - The driver clock name was checked to be `api.alsa.p-3` before data was
    kept.
  - `adjtimex()` was logged at 1 Hz.
- **Run D extras.** The kernel's `hw_ptr`/`tstamp` from
  `/proc/asound/card3/pcm0p/sub0/status` were polled at 20 Hz. This gives an
  independent view of the DAC position.

| Run | Quantum | Length | Start | Notes |
|---|---|---|---|---|
| A | 1024 | 330 s | 10:40 | `CLOCK_MONOTONIC` only |
| B | 256 | 600 s | 10:47 | + `_RAW`, adjtimex |
| C | 1024 | 600 s | 10:58 | + `_RAW`, adjtimex; graph-clock excursion at 280–440 s |
| D | 256 | 900 s | 11:08 | + `_RAW`, adjtimex, kernel `hw_ptr` |

No xrun (`clock.xrun` = 0), no `flags`, no missing buffer, and `ticks`
advanced by exactly one quantum every cycle in all runs. `now == clock.nsec`
and `ticks − clock.position` is constant (`pw_stream` `copy_position`).

### 2.2 Timing jitter

Values in µs, steady state (t ≥ 30 s). "cb" is `CLOCK_MONOTONIC` at callback
entry.

| | A (1024) | C (1024) | B (256) | D (256) |
|---|---|---|---|---|
| cb − `now`: mean / std | 32.5 / 11.7 | 30.8 / 12.4 | 22.9 / 12.8 | 25.9 / 13.3 |
| cb − `now`: p99 / max | 51 / 272 | 56 / 316 | 44 / 301 | 54 / 318 |
| cb interval − nominal: std | 15.1 | 15.2 | 16.6 | 16.8 |
| cb interval − nominal: p1 / p99 | −19 / +21 | −24 / +25 | −22 / +22 | −28 / +31 |
| cb interval − nominal: min / max | −244 / +248 | −279 / +296 | −290 / +282 | −290 / +277 |
| `now` interval − nominal: std | 0.53 | 0.76 | 0.039 | 0.045 |
| `now` interval − nominal: max | 17.5 | 18.3 | 0.60 | 0.78 |
| Callback body (zero-fill + 2 × `get_time_n`) | 0.9 | 0.8 | 0.35 | 0.42 |

- `now` is `clock.nsec`. For an ALSA driver woken by its timer,
  `alsa_timer_wakeup_event()` sets `current_time = state->next_time`, the
  instant its own DLL **scheduled**, not a measured time (`spa/plugins/alsa/alsa-pcm.c`).
  - `now` is therefore already a filtered timeline: sub-µs cycle-to-cycle
    jitter at q256, a few hundred ns at q1024 with rare 18 µs steps.
  - `next_nsec` of cycle k equals `now` of cycle k+1 exactly.
- The callback-entry time adds 12–17 µs rms of scheduling jitter with
  ≈ 0.3 ms outliers. It brings nothing that `now` lacks. Feeding either into
  the DLL gives the same rate (§2.6).
- With `htimestamp` on (an ALSA property, not ours to change here) the ALSA
  driver can use measured hardware timestamps instead. Not tested.

### 2.3 DAC rate estimates (ppm, positive = DAC faster than the reference)

| Estimator | A (1024) | B (256) | C (1024) pre-event | D (256) |
|---|---|---|---|---|
| Regression `ticks` vs `now` (whole run after 30 s) | −22.24 | −17.19 | −16.63 (30–270 s) | −16.85 |
| Regression `ticks` vs callback entry | −22.23 | −17.19 | — | −16.85 |
| Regression `ticks` vs `CLOCK_MONOTONIC_RAW` | n/a | −6.37 | −5.91 (30–270 s) | −6.24 |
| `CLOCK_MONOTONIC` vs `_RAW` (NTP correction) | n/a | +10.86 (10.97 → 10.84) | +10.67 | +10.61 |
| Mean of `rate_diff` (t ≥ 60 s; C: 30–270 s) | −21.95 | −17.24 | −16.88 | −16.98 |
| Kernel `hw_ptr` vs `tstamp` (`CLOCK_MONOTONIC`) | — | — | — | **−16.87** |
| Own DLL on `(now, ticks)`, B = 0.01 Hz, mean | −22.7 | −17.19 | — | −16.87 |

- All methods agree on the mean to ±0.1 ppm over the same span.
  - Run D: PipeWire −16.83 vs kernel −16.87 over the same 12.5 min.
  - The ~5 ppm difference between run A and runs B/D is the graph clock's
    behaviour (§2.5), not the DAC.
- `rate_diff` follows the definition in `spa/node/io.h`: > 1 means the driver
  is faster than the monotonic clock. Its mean equals the slope of
  `(now, ticks)`, as it must, since `now` advances by `duration/(rate·rate_diff)`
  each cycle.

### 2.4 Noise vs averaging time

These are non-overlapping windows; each cell is the std / p-p (ppm) of the
per-window regression slope. They show how noisy an estimate is for a given
averaging time.

| Window | A `now` (1024) | B `now` (256) | D `now` (256) | D `_RAW` (256) | D kernel `hw_ptr` | C `now` (1024, with event) |
|---|---|---|---|---|---|---|
| 1 s | 17.6 / 240 | 6.4 / 63 | 6.9 / 65 | 9.6 / 75 | 51 / 1530 | 27.7 / 285 |
| 3 s | 6.6 / 49 | 3.4 / 22 | 3.7 / 35 | 4.2 / 35 | 10.6 / 165 | 25.2 / 95 |
| **10 s** | **3.0 / 14.4** | **1.34 / 5.8** | **1.69 / 9.3** | 1.63 / 10.5 | 1.41 / 11.9 | 24.0 / 79 |
| 30 s | 2.3 / 6.8 | 0.38 / 1.4 | 1.02 / 4.9 | 0.99 / 4.7 | 0.35 / 1.7 | 23.6 / 73 |
| **60 s** | **2.4 / 6.0** | **0.08 / 0.26** | **0.58 / 2.25** | 0.57 / 2.2 | **0.12 / 0.53** | 22.6 / 72 |
| 120 s | — | 0.04 / 0.11 | 0.34 / 0.85 | 0.33 / 0.85 | 0.03 / 0.10 | 23.9 / 62 |

How to read it:

- Up to ~10 s, every source is dominated by measurement (phase) noise. The
  slope noise falls roughly as τ^−1.5, and the kernel `hw_ptr` behaves the
  same way.
- Beyond that, the **DAC itself** keeps improving: `hw_ptr` reaches 0.12 ppm
  at 60 s and 0.03 ppm at 120 s.
- PipeWire's graph clock flattens instead: 0.3–0.6 ppm (run D), or 2–24 ppm
  when an excursion is in the window (runs A, C).
  - In run D the 60-s windows of `hw_ptr` and of `ticks` are **uncorrelated**
    (r = −0.09; std of the difference 0.36 ppm).
  - So the extra wander is the PipeWire ALSA DLL phase-regulating the
    hardware fill, not the DAC.
  - That wander is still what the stream must follow: the graph pulls our
    frames at that rate, and the hardware fill stays at target.
- `_RAW` vs `MONOTONIC` makes no difference within a run: the NTP correction
  only drifted 0.13 ppm over 9 min. No `timesyncd` poll happened during
  runs B–D (poll interval 34 min).
  - Run A's −6.5 ppm step cannot be attributed: no `_RAW` was recorded.
    Its shape (step, then a slow ramp back) matches run C's excursion, so the
    same mechanism is the likelier cause.

### 2.5 Graph-clock excursions (run C)

Phase of `ticks` against the pre-event line (`_RAW` time base):

| t (s) | 0–270 | 280 | 300 | 340 | 380 | 420 | 460 | 520 | 580 |
|---|---|---|---|---|---|---|---|---|---|
| phase (frames) | ±4 | −3 | −43 | −151 | −247 | −249 | −219 | −173 | −126 |
| mean `rate_diff` (ppm vs `MONOTONIC`) | −17 | −34 | −71 | −73 | −38 | −8 | −1 | −0.3 | +0.6 |

- For ~100 s the graph ran 66 ppm slow (≈ 250 frames = 5.2 ms of phase).
  Then it ran ~16 ppm fast to recover, at about 0.8 frame/s.
- Meanwhile the reported fill `clock.delay` stayed at target (1532–1536).
  It shows −256-frame dips only around 380–420 s.
- The +256 spikes (1790) present before 280 s stopped afterwards.
- This looks like the ALSA DLL (adaptive bandwidth, re-tuned every 3 s from
  the error statistics) reacting to the USB pointer granularity. The same
  shape, smaller (−6.5 ppm, ≈ 1–2 ms), appeared in run A. Both were at
  quantum 1024; none appeared in 25 min at 256. That is too few events to
  conclude.
- **Consequence for the servo.** A PipeWire output stream can see its pull
  rate move by tens of ppm for a minute or two, independently of the source
  and of the DAC.
  - The DLL follows it with a lag. The PI absorbs the residual.
  - The plant simulator must contain a disturbance of this size: a
    ~250-frame phase walk over ~100 s, then a slow return.

### 2.6 DLL vs `rate_diff` (steady state)

Our DLL: Adriaensen 2nd-order form, ζ = 0.707, on `(now, ticks)`, with a
fast start (bandwidth `max(B, 1/(2t+1))` Hz, the first 5 s of PipeWire
start-up skipped). Cells give std / p-p of the per-cycle output (ppm) over
the stated span:

| Estimator | A 150–330 s (1024) | B 150–600 s (256) | C 60–270 s (1024) | D 150–900 s (256) |
|---|---|---|---|---|
| DLL B = 0.01 Hz | 0.77 / 2.5 | **0.19 / 1.06** | **1.18 / 5.1** | **0.76 / 3.8** |
| DLL B = 0.02 Hz | 0.71 / 2.5 | 0.47 / 3.1 | 1.63 / 7.1 | 1.08 / 6.5 |
| DLL B = 0.03 Hz | 0.71 / 2.5 | 0.75 / 4.2 | 2.31 / 11.1 | 1.30 / 7.6 |
| DLL B = 0.1 Hz | 0.71 / 2.5 | 2.36 / 15.2 | 7.54 / 68 | 2.60 / 19.3 |
| raw `rate_diff` per cycle | 0.7 / 2 | 7.7 / 120 | 31 / 897 | 7.7 / 158 |
| `rate_diff`, 10 s boxcar | 2.6 / 22 (t ≥ 60) | 1.14 / 6.9 | — | 1.57 / 10.0 |
| `rate_diff`, 60 s boxcar | 0.84 / 5.5 (t ≥ 60) | 0.20 / 1.1 | — | 0.60 / 2.9 |
| DLL lock time to ±2 ppm (fast start) | 105–150 s¹ | 16 s | — | 12.5 s |

¹ Run A's step at ~90 s made the lock look late. Run D, which is clean,
locked in 12.5 s.

- The p-p within any 10-s window is 0.16–0.24 ppm (median) at B = 0.01 Hz,
  0.6 at 0.02 Hz, and 1.1–1.3 at 0.03 Hz. This is the short-term ratio
  modulation the feed-forward injects.
- In run A after 150 s, PipeWire's DLL had narrowed itself: `rate_diff` per
  cycle was 0.7 ppm rms, and every estimator output the same smooth ramp. In
  run C it was 31 ppm rms.
  - The quality of `rate_diff` therefore depends on PipeWire's internal state
    (`SPA_ALSA_DLL_BW_MIN` 0.001 … `SPA_DLL_BW_MAX` 0.128, re-tuned every
    `BW_PERIOD` = 3 s).
  - First 5 s of every run: `rate_diff` ≈ +2500 ppm mean, 11 000–16 000 ppm
    p-p.
- **Timestamp-quality sensitivity** (run D, replayed through the same DLL,
  std / p-p ppm after 150 s). This stands in for what ASIO/CoreAudio entry
  timestamps could look like:

| Time base | B = 0.01 | 0.02 | 0.03 | 0.1 |
|---|---|---|---|---|
| `now` (reference) | 0.76 / 3.8 | 1.08 / 6.5 | 1.30 / 7.6 | 2.6 / 19 |
| callback entry (15 µs rms jitter) | 0.76 / 4.0 | 1.10 / 6.9 | 1.32 / 7.6 | 2.7 / 21 |
| `now` + N(0, 100 µs) | 0.78 / 4.0 | 1.12 / 6.9 | 1.36 / 8.7 | 3.4 / 28 |
| `now` + N(0, 500 µs) | 0.89 / 5.2 | 1.57 / 10.9 | 2.36 / 15.3 | 11.6 / 84 |
| `now` floored to 1 ms (`timeGetTime`-like) | 1.23 / 6.9 | 4.0 / 18 | 8.4 / 29 | 31 / 100 |

### 2.7 What `delay` contains

| Field | q1024 | q256 | Meaning |
|---|---|---|---|
| `pw_time.delay` | 1536 (1152 during the first second) | 384 (768 during the first second) | Downstream port Latency param, averaged: `minQuantum = 1` × quantum + `minRate` = 1024 + **512** / 256 + **128** frames. `minRate` is the ALSA headroom (`recalc_headroom`); here it equals quantum/2. **Static**: read from the Latency param, not measured (`pw_stream_get_time_n`) |
| `spa_io_clock.delay` (live) | mean 1535.9; 1532/1534/1536 (99 %), 1788–1792 (≈ 0.6 %), rare 1276–1280 | mean 384.6; 380/382/384 (98 %), 508–512 (≈ 1.6–1.9 %) | ALSA hardware fill (`buffer − avail`) at the wake-up + resampler delay. The ALSA DLL regulates it to `threshold + headroom`, so its mean is `pw_time.delay`. Values move in 2-frame USB steps, with ±quantum/4 spikes (pointer/URB granularity) |
| `queued` before → after queueing | 0 → 1024 | 0 → 256 | Our buffer, consumed in the **next** cycle (`impl_node_process_output` pops the buffer for this cycle before calling `process`). Counted only because the spike sets `pw_buffer.size` in frames |
| `buffered` | 0 | 0 | Stream resampler delay; 0 because the stream rate = graph rate |
| `size` / `requested` | 1024 | 256 | = quantum (no resampling) |

The first frame queued in a callback reaches the DAC input about
`(buffered + queued_after + delay)/fs` after `now`: 53.3 ms at q1024 and
13.3 ms at q256. That matches the formula in `pipewire/stream.h`.

**Not included** in any of these: the USB host controller's isochronous
schedule, the DAC's FIFO, and its digital filter group delay. These are
fixed per device, typically 1–3 ms. They can only be measured acoustically
or electrically (loopback); Phase 7.

## 3. cpal ASIO (Windows) — research

### 3.1 What cpal gives

`cpal 0.15.3` `src/host/asio/stream.rs` (unchanged in 0.17.3):

```rust
let callback = system_time_to_stream_instant(asio_info.system_time);
let delay    = frames_to_duration(asio_stream.buffer_size, sample_rate);
let playback = callback.add(delay);           // "callback + one buffer"
```

- `timestamp().callback` is the driver's `ASIOTime.timeInfo.systemTime`
  (nanoseconds) from `bufferSwitchTimeInfo`. With ASIO 1.0 drivers,
  `asio-sys` emulates it with `ASIOGetSamplePosition`.
  - The SDK says this time "must be derived from `timeGetTime()`" on
    Windows. Many drivers therefore return **millisecond-quantised** values.
    Some use QPC. Which one is driver-specific.
  - Its epoch is `timeGetTime`'s (system interrupt time), **not** QPC, which
    is what `std::time::Instant` uses on Windows. cpal offers no way to map a
    `StreamInstant` to `Instant`.
- `timestamp().playback` is a guess: callback time + one buffer. It ignores
  `ASIOGetLatencies`, the double buffer, and the converter/USB/FIFO delay.
  It is **not** a device delay.
- **Not exposed**:
  - `ASIOTime.timeInfo.samplePosition`: `asio-sys`'s `CallbackInfo`
    (`buffer_index`, `system_time`, `callback_flag`) drops it.
  - `timeInfo.speed`, `sampleRate`, and the flags.
  - `ASIOGetLatencies`: it is not even in `asio-sys`'s bindgen allowlist
    (`build.rs`). `ASIOGetSamplePosition` is bound.
  - `kAsioLatenciesChanged` / `kAsioResyncRequest` are answered with `1` and
    never forwarded.

### 3.2 What ASIO itself provides (SDK `common/asio.h`)

- `bufferSwitchTimeInfo(ASIOTime*)`: `samplePosition` (frames since
  `ASIOStart`, block-aligned, "must always be valid") and `systemTime` (the
  time that position was latched, i.e. the buffer switch). This is an exact
  `(time, DAC position)` pair, as precise as the driver makes it.
- `ASIOGetLatencies(&in, &out)`: `out` is "the time between the buffer switch
  and the time when the next play buffer will start to sound". Usually 1
  block (direct DMA) or 2+ (latched by the driver), plus device FIFOs. This is
  `D_dev` at the switch instant.
- `kAsioLatenciesChanged`: the host must re-read the latencies.

### 3.3 Assessment

- The timestamps cpal passes through are usable for a rate DLL **only if** the
  driver's `systemTime` is good. A 1 ms quantisation is tolerable at 0.01 Hz
  but not at 0.1 Hz (§2.6).
- The position is not exposed. Counting callbacks × `buffer_size` is exact
  only while no buffer switch is missed. ASIO hosts that miss a switch get the
  old buffer replayed, and the hardware position moves on regardless.
- There is **no device delay** at all.

## 4. cpal CoreAudio (macOS) — research

### 4.1 What cpal gives

`cpal 0.15.3` `src/host/coreaudio/macos/mod.rs` (AUHAL render callback):

```rust
let callback = host_time_to_stream_instant(args.time_stamp.mHostTime)?; // mach ticks → ns
// "TODO: Need a better way to get delay, for now we assume a double-buffer offset."
let playback = callback.add(frames_to_duration(buffer_frames, sample_rate));
```

- cpal 0.17.3 is the same, except `playback` adds the device buffer size
  (`kAudioDevicePropertyBufferFrameSize`) instead of the callback size.
- `timestamp().callback` is `AudioTimeStamp.mHostTime` converted with
  `mach_timebase_info`. The clock is `mach_absolute_time`, which equals
  `CLOCK_UPTIME_RAW`.
  - Rust's `Instant` on Apple targets uses `CLOCK_UPTIME_RAW` (std
    `sys/pal/unix/time.rs`). So the cpal instant and our own `Instant`/M are
    **the same clock**: the mapping is exact. `clock_gettime_nsec_np(CLOCK_UPTIME_RAW)`
    gives the same ns value.
  - It is not slewed by NTP and stops during sleep.
- **Semantics.** In an output render callback the HAL/AUHAL timestamp is the
  *output time* of the buffer: when its first frame crosses the IO boundary.
  This is in the future relative to the callback entry by about the safety
  offset plus a buffer.
  - cpal calls it `callback`, which is wrong.
  - Its `playback` adds one more buffer and ignores the device and stream
    latency.
- **Not exposed**:
  - `mSampleTime`: AUHAL's sample counter, which shows skipped cycles.
  - `mRateScalar`: the HAL's own measured ratio of actual to nominal host
    ticks per frame, i.e. the HAL's DLL output, the analogue of PipeWire's
    `rate_diff`.
  - `mFlags`.
  - `kAudioDevicePropertyLatency`, `kAudioStreamPropertyLatency`,
    `kAudioDevicePropertySafetyOffset`.

### 4.2 Assessment

- Time is good and directly in M (on macOS M must be `CLOCK_UPTIME_RAW` /
  `Instant`, see §6).
- Position is only available by counting frames unless `mSampleTime` is
  exposed.
- The device delay is absent from cpal. It needs the HAL properties of the
  device.
- `mHostTime` is already HAL-smoothed: the HAL timestamps come from its
  zero-timestamp filter, much like PipeWire's `nsec`. A DLL on
  `(mHostTime, frames)` should therefore behave like the PipeWire `now` case
  below. **To be verified on hardware.**

## 5. Recommended estimator

**Use our own 2nd-order DLL on `(t_M, position)` for every backend. Do not
use `rate_diff` directly.**

- **Why not `rate_diff` as `r_ff`.**
  - Per cycle it carries 7–39 ppm rms of noise, with steps up to ~900 ppm,
    and its noise level depends on PipeWire's adaptive DLL state.
  - It is wildly off for the first ~10 s.
  - It exists only on PipeWire, and its meaning depends on the driver: an
    ALSA follower, a `NO_RATE` clock or a network driver behave differently.
  - Averaging it is mathematically the same as regressing `ticks` on `now`.
    The DLL does exactly that, with a known bandwidth, and works unchanged
    on ASIO/CoreAudio positions (requirement 4: one code path).
  - Keep `rate_diff` only as a diagnostic and a sanity check
    (`|rate_diff − 1|` ≫ 1000 ppm means misconfiguration, plan §2.3).
- **Settings.**
  - Bandwidth **B = 0.01 Hz** (ζ = 0.707). It gives 0.2–0.8 ppm rms,
    ≤ 0.25 ppm p-p within 10 s, and tolerates timestamps up to 0.5 ms rms
    or 1 ms quantised.
  - 0.02 Hz is acceptable, at 0.5–1.1 ppm rms. 0.03 Hz and above inject
    > 1 ppm p-p per 10 s. 0.1 Hz is too noisy (2.4–2.6 ppm rms, 15–20 ppm
    p-p, worse with poor timestamps).
  - **Fast start**: `B(t) = max(B, 1/(2t + 1))` Hz from the first valid
    sample. It locks to ±2 ppm in 12–16 s. With a fixed 0.01 Hz from a
    nominal start it took > 100 s and overshot by 10 ppm.
  - Initialise the period from the nominal rate. Discard the first ~5 s of
    a fresh PipeWire driver, or simply let the fast start handle it.
  - **Reset** on a driver clock-id change, `DISCONT`/`XRUN_RECOVER`, an
    `xrun` count change, or a position step ≠ quantum. Re-arm the fast
    start. Keep the old estimate as the initial period: same device,
    same rate.
- **Interaction with the PI (plan §2.3).**
  - The feed-forward DLL (0.01 Hz) is slower than the PI loop (default
    0.02 Hz). The PI absorbs what the DLL lags.
  - The largest lag case observed is run C: −66 ppm for ~100 s. At
    0.01 Hz the DLL follows such a step with a ~22 s time constant
    (1/(ζω)). Before the PI reacts, that is of the order of
    66 ppm × 22 s ≈ 1.5 ms of phase error, which the PI must then remove.
  - The simulator should confirm this against the plan's p99 < 0.5 ms
    target. If it fails, 0.02 Hz for the DLL is the fallback.
- **Ratio modulation criterion.** The plan's "< 5 ppm p-p once locked" is
  met *within 10 s* at 0.01–0.02 Hz.
  - It is not met over minutes on PipeWire: the graph clock itself moves by
    2–6 ppm (and once by 66 ppm), and the ratio must follow it.
  - The acceptance criterion should be stated per time window (for example
    p-p within 10 s), not over the whole run.

## 6. API recipe per backend

### 6.0 The common reference clock M

M must be the clock that the source timestamps (pipe reads, capture
callbacks) and the output timestamps share:

| OS | M | Why |
|---|---|---|
| Linux | `CLOCK_MONOTONIC` (= `Instant`) | PipeWire `nsec`/`now` are `CLOCK_MONOTONIC`; `timerfd` for the `own` driver supports it (not `_RAW`) |
| macOS | `CLOCK_UPTIME_RAW` (= `Instant`, = `mach_absolute_time`) | `mHostTime` is in it; no conversion error |
| Windows | QPC (= `Instant`) | The only clock we can read at callback entry; ASIO `systemTime` cannot be mapped to it |

The output core receives `(t_M, frames_position, d_dev_frames)` per callback
plus a `discontinuity` flag. Each adapter below produces exactly that.

### 6.1 PipeWire (`pw_stream`, playback, `RT_PROCESS`)

```c
/* io_changed: keep the position area (RT-safe to read during process) */
if (id == SPA_IO_Position) pos = (struct spa_io_position *)area;

/* process callback */
struct pw_time t;
pw_stream_get_time_n(s, &t, sizeof t);        /* before dequeue */
b = pw_stream_dequeue_buffer(s);
fill b with N frames;
b->size = N;                                  /* app units = frames, so pw_time.queued is in frames */
pw_stream_queue_buffer(s, b);

t_M      = t.now;                             /* ns, CLOCK_MONOTONIC, cycle start (driver's DLL-smoothed time) */
position = t.ticks;                           /* frames at t.rate, consumed by the graph up to t_M */
/* delay (frames) from t_M until the FIRST frame just queued is at the device edge: */
d_dev    = t.buffered                         /* stream resampler, stream rate; 0 when rates match */
         + N                                  /* our buffer: pw_stream output is one cycle behind */
         + t.delay;                           /* graph + device latency (static Latency param, graph rate) */
discont  = pos->clock.id != last_id || (pos->clock.flags & SPA_IO_CLOCK_FLAG_DISCONT)
         || pos->clock.xrun != last_xrun || t.ticks - last_ticks != pos->clock.duration;
```

Notes:

- Use `t.now`, not the callback-entry time. It is the same instant for every
  node in the cycle, with about 0.5 µs of jitter instead of about 15 µs (§2.2).
- `pw_time.queued` read **after** queueing equals `N` only if `pw_buffer.size`
  is set in frames. The spike verified `queued` = 0 before and `N` after. Use
  `N` directly rather than relying on it.
- `t.rate` must equal the stream rate, or `buffered`/`queued` (stream domain)
  and `delay`/`ticks` (graph domain) must be converted separately.
- Feed `(t.now, t.ticks)` to the DLL (§5). Reset it on `discont`.
- `t.delay` is static: on this device it is quantum + ALSA headroom. A
  per-device fixed offset (DAC/USB) remains unknown; see §7.

### 6.2 ASIO (Windows)

cpal 0.15/0.17 does not expose enough, as shown in §3. Either:

- **(a) Patch `asio-sys` and cpal (preferred).** The change is small:
  - add `ASIOGetLatencies` to the `build.rs` allowlist and wrap it;
  - carry `time_info.sample_position` (and `flags`) in `CallbackInfo`;
  - forward `kAsioLatenciesChanged` / `kAsioResyncRequest` as events.
- **(b) Stock cpal fallback.**
  - `t_M` = `Instant::now()` (QPC) taken first thing in the callback;
  - `position` = Σ frames delivered;
  - detect a missed switch when the interval between two callbacks is
    > 1.5 × buffer and add the missing blocks;
  - `d_dev` = a configured or estimated constant (2 × buffer by default).

With (a), per callback:

```
t_M      = Instant::now() at entry (QPC)          -- M
          (systemTime is the switch time, but in timeGetTime's domain and
           possibly 1 ms-quantised: use it only to measure the entry lag
           relative to its own previous values, never as M)
position = samplePosition (block-aligned, frames since ASIOStart)
d_dev    = outputLatency (ASIOGetLatencies, frames; re-read on kAsioLatenciesChanged)
           − entry lag (t_M − switch time); ≈ 0 to tens of µs
discont  = samplePosition − last != buffer_size || kAsioResyncRequest
```

- Rate: DLL on `(t_M, samplePosition)`, same settings as PipeWire.
- QPC entry jitter of ≤ 0.5 ms σ costs < 1 ppm at 0.01 Hz (§2.6).
- If `ASIOOutputReady` is used (cpal calls it when supported), check whether
  the driver's reported `outputLatency` assumes it.

### 6.3 CoreAudio (macOS)

Bypass or patch cpal to forward the raw `AudioTimeStamp`. coreaudio-rs's
`render_callback::Args` already has `time_stamp`; cpal only reads
`mHostTime`. Read the HAL properties once on start and on
property-change notifications.

```
t_M      = mHostTime → ns          (same clock as Instant; this is the buffer's output time)
position = mSampleTime            (AUHAL sample counter; gaps reveal skipped cycles)
d_dev    = kAudioDevicePropertyLatency (output scope)
         + kAudioStreamPropertyLatency (the output stream in use)
           -- frames from the output time t_M to the converter/speaker
           (plus kAudioUnitProperty_Latency of the AUHAL, normally 0)
rate     = DLL on (mHostTime, mSampleTime);
           cross-check with 1/mRateScalar when kAudioTimeStampRateScalarValid
discont  = mSampleTime − last != frames of last callback
```

- Since `t_M` is already the output time, the safety offset and the IO buffer
  are **not** added. Here `d_dev` is relative to `t_M`, not to the callback
  entry.
- Stock-cpal fallback:
  - `t_M` = `timestamp().callback` (it *is* `mHostTime`);
  - `position` = Σ frames;
  - `d_dev` from the HAL properties, read separately via the device ID.
    cpal keeps that ID private, so a patch is needed anyway.

## 7. Risks and unknowns

### 7.1 PipeWire (measured here, but one device)

- **One sink, one machine.** The USB DAC uses an adaptive endpoint on an
  ALSA timer-scheduled driver.
  - Not measured:
    - PCI/HDA (IRQ-free timer mode, different pointer granularity);
    - HDMI;
    - `htimestamp`;
    - Bluetooth (A2DP: the delay includes the codec and the radio, and is far
      larger and more variable);
    - a follower ALSA sink resampled to another driver (`rate_diff` then
      belongs to the other driver; `buffered` ≠ 0);
    - the production case, where `orender` writes to the default sink of the
      `omniphony` graph.
  - Repeat the spike on the target box and on Bluetooth before trusting
    `delay` there.
- **Graph-clock excursions** (§2.5): two in ~41 min, both at quantum 1024.
  - Cause not proven. The likely cause is the ALSA DLL reacting to USB
    pointer granularity.
  - Their frequency and amplitude on other devices are unknown. The
    simulator must cover them.
- **`delay` is static and excludes the device-internal delay** (USB
  schedule, DAC FIFO, filter: ~1–3 ms).
  - `L` is defined up to the DAC input as reported by PipeWire. An absolute
    "sample leaves the DAC at exactly `L`" requires a per-device offset
    measured by loopback (Phase 7) or entered by the user.
  - `pw_time.delay` should also include any user `latencyOffsetNsec` set on
    the sink. Not tested.
- **Startup.** `pw_time.delay` was transiently 1152 (q1024) and 768 (q256)
  during the first second. `rate_diff` was ±10⁴ ppm for ~5 s. The start rule
  (plan §2.4) must not latch `D_dev` in the first second. Re-read it each
  callback; it costs nothing.
- **Quantum changes** made by other clients joining the same driver: `ticks`
  stays continuous, but `delay` and N change. The servo must use the
  per-callback N and `delay`, never cached values.
- **NTP slewing.** `CLOCK_MONOTONIC` runs +10.6 ppm off `_RAW` here. The
  offset correction moved it by 0.13 ppm over 9 min, and a poll can step it
  further.
  - This is common-mode for `follow`: S is timestamped in M too.
  - In `own` mode it is real drift between S ≡ M and D. That is acceptable:
    mpv's video clock is also `CLOCK_MONOTONIC`, so it is the right
    reference.
  - `timerfd` cannot use `_RAW`, so M stays `CLOCK_MONOTONIC` on Linux.
  - A large NTP step (e.g. after resume) shows up as a rate transient that
    the DLL and PI must absorb. Not observed in these runs.

### 7.2 ASIO (not measured; verify on Windows hardware)

- **`systemTime` quality per driver.** Is it ms-quantised (`timeGetTime`) or
  QPC? Does it refer to the buffer switch or to the callback?
  - Measure the same table as §2.4 with `(systemTime, samplePosition)` and
    with `(QPC at entry, samplePosition)` on 2–3 drivers: RME, a USB class
    device with a vendor driver, and ASIO4ALL/FlexASIO (a WASAPI wrapper
    whose position may be synthetic).
- **`ASIOGetLatencies` accuracy.** It is self-reported by the driver and
  often wrong or missing the converter delay. It differs with
  `ASIOOutputReady`. Verify with a loopback cable.
- **Missed buffer switches.** Check whether `samplePosition` jumps (it
  should) and whether cpal's callback ever runs twice per switch.
  - The `callback_flag`/`buffer_index` logic in asio-sys allows multiple
    callbacks per switch.
- **`kAsioResetRequest` / `kAsioResyncRequest` / sample-rate changes**
  happen mid-stream. The adapter must report a discontinuity.
- **Patching `asio-sys`/cpal.** This carries a maintenance cost. Check
  whether a cpal release after 0.17.3 exposes the time info before forking.

### 7.3 CoreAudio (not measured; verify on macOS hardware)

- **`mFlags` in the AUHAL output render callback.** Are `mSampleTime`,
  `mHostTime` and `mRateScalar` all valid (expected 0x7)?
  - Is `mSampleTime` continuous across cycles, and does it jump on overloads?
  - Is `mRateScalar` ≈ 1/(our DLL rate)?
- **The `mHostTime` meaning.** We expect the output time: in the future
  relative to callback entry by ≈ safety offset + buffer. Measure
  `mHostTime − now` at entry.
  - AUHAL with a converter (sample-rate mismatch) may present its own
    timeline. Prefer opening at the device's nominal rate.
- **Latency properties.** `kAudioDevicePropertyLatency` and
  `kAudioStreamPropertyLatency` are often 0 or wrong on USB and aggregate
  devices. AirPlay/Bluetooth add large, variable delays. Verify by loopback.
- **Sleep.** `mach_absolute_time` stops during sleep. Treat wake as a
  discontinuity (reset the DLL, realign).
- **Default-device switching.** cpal re-targets the device. The adapter must
  reset on a device change.

### 7.4 Spike limitations

- Silence only, no load. CPU contention would mostly widen the
  callback-entry jitter, which the design does not use on PipeWire.
- Four runs, 41 min in all, on one afternoon. Temperature drift of the DAC
  and long-term NTP behaviour were not characterised. A 2-hour run belongs
  in Phase 7.

## 8. Files

`spikes/s2-output-timestamps/`:

- `src/main.rs`: capture program.
  `s2-output-timestamps <sink node.name> <quantum> <seconds> <out.csv> [clock-prefix]`.
  - Silence only; refuses `omniphony`; aborts unless the driver clock name
    starts with `api.alsa.`.
  - Writes a per-callback CSV and an `adjtimex` log (`<out>.ntp`).
- `run.sh`: one capture against the USB sink.
  - Aborts if anything else is linked to it.
  - Snapshots Latency params and links.
- `analyze.py`: jitter, regressions, windowed noise, DLLs (including
  `dll_faststart`).
- `steady.py`: steady-state DLL / `rate_diff` comparison and lock time.
- `simulate_ts_degradation.py`: degraded-timestamp replay.
- `hwptr_compare.py`: kernel `hw_ptr` vs PipeWire `ticks`.
- `data/` (~70 MB, ignored by the spike's own `.gitignore`; regenerate with
  `run.sh`): `q1024*` (runs
  A, C), `q256*` (runs B, D), analysis JSON, the USB PCM state log, and the
  `hw_ptr` log.
