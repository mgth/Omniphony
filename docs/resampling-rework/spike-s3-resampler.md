# Spike S3 — Variable-ratio resampler for the output core

Phase-0 spike of [the resampling rework plan](../resampling-rework-plan.md)
(§1 req. 3 and 5, §2.2 `N_play`, §3 "Resampler", §4 Phase 0 S3). State:
workflow `resampling-rework`, Omniphony `8450cbff`. Code, harnesses and raw
results: [`spikes/s3-resampler/`](../../spikes/s3-resampler/), a standalone
cargo project outside the workspace. Measured on a Ryzen 9 9950X, single
thread, `nice -n 19`, pinned to one core.

## 0. Summary

- **Recommendation: an in-house polyphase windowed-sinc fractional-delay FIR.**
  - 64 taps, Kaiser β = 14, cutoff at the input Nyquist.
  - Coefficients come from a piecewise-cubic (Hermite) table over 32
    segments of the fractional phase: 32 KiB of f32, built once at init.
  - The inner loop is frame-interleaved and vectorised across channels, in
    safe Rust.
  - A Q32.32 fixed-point position counter is exposed as the `N_play` counter.
- **Quality.**
  - THD+N is −139 dB at 1 kHz and −134.7 dB at 20 kHz, at any ratio within
    1 ± 2000 ppm, with a modulated ratio, and with N varying per call. That is
    the f32 arithmetic floor.
  - The passband is flat within ±0.00001 dB from 20 Hz to 20 kHz.
  - At ratio 1 and phase 0 the output is bit-exact.
- **CPU.** 24 ch at 48 kHz costs 61 ns per output frame: **0.29 % of one
  core** on the baseline x86-64 build and 0.26 % with AVX2. That is **9×
  cheaper than today's rubato 0.14 path** (2.6 %) and 4.5× cheaper than
  rubato 5 with today's parameters. The steady state makes zero allocations.
- **Position.** It is exact by construction: Q32.32, a closed-form unit test,
  and a measured played position that matches the reported one within
  2.6·10⁻⁵ frames (the probe's resolution).
  - The played position is the kernel centre. The resampler holds a constant
    **T/2 = 32 frames** (0.67 ms at 48 kHz) of look-ahead between the ring
    read cursor and `N_play`.
- **rubato is rejected.**
  - rubato 5.0.1 (the latest) `Async` with `FixedAsync::Output` meets the
    "exactly N per call" and "ratio ramp per call" requirements.
  - However, it does not expose its fractional position: `last_index` is
    private, and `output_delay()` is a truncated integer (128, measured
    127.996).
  - It costs 4.5× more at worse quality with today's parameters: −108 dB at
    20 kHz, because linear interpolation over 256× oversampling limits it.
  - The polynomial (`FastFixedOut`-style) variants have no anti-imaging and
    reach −11 dB at 20 kHz.
- **No separate fixed SRC stage.** The same engine folds the nominal ratio
  into its step: `ratio = fs_in/fs_out · r_drift`, with a table designed for
  that pair at init.
  - 44.1 ↔ 48 kHz needs 96 taps (the transition band is only 4.1 kHz wide).
    It measures −136 dB in both directions for 0.43 % of a core at 24 ch.

## 1. Candidates

| # | Candidate | What it is |
|---|---|---|
| R0 | rubato **0.14.1** `SincFixedIn` (today) | 256 taps, 256× oversampling, linear interpolation, BlackmanHarris², fc 0.95, 1024-frame input chunks plus an output FIFO. The crate has no `Resizable`, so `SincFixedOut` in 0.14 has a fixed chunk and cannot do "N per call" without a FIFO |
| R1 | rubato **5.0.1** `Async::new_sinc`, `FixedAsync::Output` (successor of `SincFixedOut`) | With today's parameters (`rb5_sinc256_lin`), with rubato 5 defaults (256 taps, auto cutoff, 128×, cubic: `rb5_sinc256_cub`), and a 64-tap variant (`rb5_sinc64_cub`) |
| R2 | rubato 5.0.1 `Async::new_poly`, `FixedAsync::Output` (successor of `FastFixedOut`) | Septic and cubic polynomial, with no anti-imaging filter |
| P | **in-house polyphase** (`spikes/s3-resampler/src/polyphase.rs`) | T taps, Kaiser β, coefficients over L phase segments, with linear or Hermite-cubic interpolation between segments. Variants: T = 32/48/64/96, Hermite L = 32, and linear L = 256/1024 |

A pure Farrow structure (one polynomial over the whole phase range) was not
built. The piecewise-cubic table is the same idea with a far lower order, and
it already reaches the f32 floor at L = 32 (§4.1).

## 2. API fit

| Requirement | R0 rubato 0.14 | R1 rubato 5 sinc | R2 rubato 5 poly | P in-house |
|---|---|---|---|---|
| Exactly N output frames per call, N = 64…4096 | ✗ fixed chunk → FIFO needed (today's design) | ✓ `set_chunk_size(n)` (`Resizable`) + `FixedAsync::Output` | ✓ same | ✓ `prepare(n, ratio)` / `render(out)` |
| Variable input from an interleaved SPSC ring | planar `Vec<Vec>`, deinterleave by hand | ✓ `input_frames_next()`. The `Adapter` must be contiguous: copy to scratch, or write a two-segment ring `Adapter`. The input is copied into an internal planar buffer anyway | ✓ same | ✓ ring `memcpy` straight into the resampler's history (`input_slot()`), no intermediate copy |
| Smooth ratio change every call | ✓ `set_resample_ratio(r, ramp=true)` | ✓ linear ramp in step space over the chunk | ✓ | ✓ the same linear step ramp, in Q32.32 |
| **Exact fractional input position** | ✗ | ✗ `last_index` private; `output_delay()` is a truncated `usize` (128 vs 127.996 measured); index stepped before sampling. Recoverable only by mirroring private internals, or via an upstream API | ✗ (same) | ✓ `position()` → `{frame: i64, frac: u32}`, exact and deterministic |
| Zero allocation / no locks in steady state | ✓ with `process_into_buffer` (measured 0) | ✓ (measured 0) | ✓ (measured 0) | ✓ (measured 0, counting global allocator) |
| Interleaved multichannel 2…24 | planar only | ✓ via `audioadapter`, but the output is written through `dyn AdapterMut` one sample at a time | ✓ | ✓ native interleaved, vectorised across channels |
| Transparent quality (§4) | −108 dB at 20 kHz | ✓ cubic −127 dB; ✗ linear −108 dB | ✗ −11 dB at 20 kHz | ✓ −134.7 dB at 20 kHz |
| CPU at 24 ch / 48 kHz (§5) | 2.6 % | 1.3–1.4 % | 0.3–0.8 % | **0.29 %** |
| Fixed SRC (44.1 ↔ 48 kHz) in the same stage | ✓ | ✓ | (quality ✗) | ✓ table designed for the nominal pair (§4.3) |
| Dependencies / ownership | external | external, `Box<dyn>` inner, AVX/NEON runtime dispatch | external | ~300 lines, no dependency |

## 3. In-house design

### 3.1 Model

The output frame at fractional input position `p` is the band-limited
reconstruction `x(p)`:

```
i = floor(p), mu = p − i
y = Σ_{k=0}^{T−1} c_k(mu) · x[i − T/2 + 1 + k]
c_k(mu) = h(mu + T/2 − 1 − k)
h(t)    = fc · sinc(fc · t) · I0(β·√(1 − (t/(T/2))²)) / I0(β)
```

- **`p` is the played position.** The kernel is linear-phase and centred on
  `p`, so no delay is added on top. `N_play = position()`, and nothing is
  subtracted.
- **Look-ahead.** Producing `x(p)` needs frames up to `i + T/2`. Right after
  a call, `consumed − position ∈ (T/2 − s, T/2 + 1 − s]`: about **T/2 frames
  of constant group delay** relative to the ring read cursor. That is 32
  frames (0.667 ms) for T = 64 at 48 kHz, and 48 frames for T = 96.
  - `L_e2e = (N_in − N_play)/f_s + D_dev` counts these frames automatically,
    because `N_play` is the position and not the read cursor.
- **Step.** `p` advances by `s` = input frames per output frame, the plan's
  `ratio_total = r_ff·(1+u)`, times `fs_in/fs_out` when the rates differ.
- **Fixed point.**
  - `p` and `s` are Q32.32, so accumulation is exact and reproducible. The
    servo reads the position back rather than integrating the ratio itself.
  - A ratio quantum is 2⁻³² ≈ 0.0002 ppm.
- **Ramp.**
  - Within a call, `s` ramps linearly from the previous value to the
    requested one: `s_n = s_prev + (n+1)·d`, increment first, the same law as
    rubato.
  - `d` is truncated, and the step snaps to the target at the end of the
    call.
  - The reported position integrates the steps actually used, so it stays
    exact. It differs from an ideal continuous ramp by at most `N²/2·2⁻³²`
    frames: 0.002 frames at N = 4096, and invisible to the servo.
- **Start.**
  - `reset()` puts `T/2 − 1` zero frames before frame 0, so the first output
    frame is `x(0)`.
  - The first call pulls `N + T/2` frames, and every later call pulls about
    `N·s`.
  - The fade-in from §2.4 of the plan covers the half-empty kernel during the
    first T/2 frames.

### 3.2 Coefficient table (built once)

1. For every segment end `mu = m/L` (m = 0…L), compute the kernel row in f64
   and **normalise it to unit DC gain**.
   - With `fc = 1` the zero crossings are exact, so the `mu = 0` row is an
     exact unit impulse. Ratio 1 at phase 0 is therefore bit-transparent
     (unit test `unity_ratio_phase_zero_is_bit_exact`).
2. Compute `d/dmu` of the normalised row at both ends of each segment
   (central difference, δ = 10⁻⁶, f64).
3. Store the cubic Hermite polynomial of each segment in Horner form:
   - `c0 = a`
   - `c1 = a'`
   - `c2 = 3(b−a) − 2a' − b'`
   - `c3 = 2(a−b) + a' + b'`

   That is 4 rows of T f32 values per segment.
   - Size: `L·4·T·4 B` = **32 KiB** for T = 64 and L = 32, or 48 KiB for
     T = 96.
4. **Per output frame:**
   - `m` comes from the top `log2 L` bits of the Q0.32 fraction, and `f` from
     the rest.
   - `kern[k] = c0 + f(c1 + f(c2 + f·c3))` costs 3 FMAs per tap, vectorised
     across taps and amortised across channels.

Why Hermite rather than linear interpolation between more phases:

- Linear interpolation loses 12 dB per halving of L. Reaching −130 dB at
  20 kHz needs L = 1024, a 256 KiB table.
- Cubic Hermite reaches the f32 floor at L = 32 (§4.1).
- At ratio ≈ 1 the phase drifts slowly, so either table touches only one or
  two segments per callback. For a real SRC the phase sweeps the whole table,
  so table size matters there.

### 3.3 Inner loop

The history is a linear interleaved buffer of `(T + max_in)·C` f32. Each call
then does the following:

1. The ring reader copies exactly `prepare()`'s frame count into
   `input_slot()`: a two-segment `memcpy`, and the only copy.
2. Each output frame builds the kernel, then takes a dot product over T
   frames.
3. Afterwards, the T-frame tail is moved to the front of the history.

The dot product is vectorised **across channels**:

- Channels go in groups of 16, 8 and 4 lanes (`[f32; 4]` lane groups), with
  two accumulators per group for even and odd taps. Any 1–3 remaining
  channels go through a scalar path.
- The accumulators stay in registers across the taps.

Codegen notes from the spike:

- Wider arrays (`[f32; 16]`, `[f32; 24]`) or 4-way tap splitting were
  sometimes faster on one target and 2–4× slower on another (spills). On
  aarch64, `[f32; 16]` did not vectorise at all.
- The `[f32; 4]` lane-group form vectorises on all three tested targets:
  - x86-64 baseline (SSE2, `mulps`/`addps`);
  - x86-64-v3 (AVX2, FMA);
  - aarch64, checked in the emitted asm: 8 `fmla v.4s` accumulators and
    `ldp q` loads, with no spills in the loop.
- `fmadd` uses `mul_add` only where FMA exists (`target_feature = "fma"` or
  aarch64). Otherwise it falls back to `a*b + c`, because `mul_add` would call
  libm.
- No `unsafe` and no explicit SIMD are needed.

## 4. Quality

### 4.1 Method

**Analytic** (`analysis/design_explorer.py`).

- For each design, the f32 table and its interpolation rule are evaluated on
  2048 off-grid phases. For each phase `mu`, `R_mu(ω) = Σ c_k(mu)·e^{jω(k−T/2+1−mu)}`
  is the gain applied to a tone relative to the ideal `x(p)`.
- The LTI part is `G = mean_mu R_mu`, which gives the passband.
- The time-varying part, `mean|R_mu − G|²/|G|²`, is the THD+N of a tone at
  any ratio ≠ 1, because the phase then sweeps uniformly.

**Measured** (`src/bin/quality.rs`).

- Signals are analytic, and every run uses 3 channels:
  - ch0 carries one of four signals:
    - a tone at −1 dBFS (997 Hz, 9973 Hz or 19 997 Hz);
    - a 31-tone multitone, log-spaced from 20 Hz to 20 kHz at −30 dBFS each;
    - a 22 001 Hz tone (outside the passband).
  - ch1 and ch2 carry a 100 Hz sin/cos pair, used as the position probe.
- Each run produces 3 s of output with 4096 frames of warm-up skipped.
- **THD+N:** all tones of ch0 are fitted jointly by least squares against
  sin/cos at the positions the resampler reports. For rubato, the nominal
  ramp law is used, shifted one step because rubato steps before sampling.
  - A constant delay is absorbed by the fit.
  - THD+N = residual power / fitted power, over the full band (0 to fs/2).
  - Passband = the fitted gain of each multitone component.
- **Position:** the unwrapped `atan2(ch1, ch2)` gives the actually played
  position, to the resolution of f32 output rounding (about 2·10⁻⁵ frames).
- Ratio schedules:
  - constant 1, 1 ± 100 ppm, 1 ± 500 ppm, 1 ± 2000 ppm;
  - `mod`: 1 + 500 ppm·sin(2π t / 4 s), updated every 256-frame call;
  - `varN`: the `mod` ratio, with N cycling through 64/256/1000/4096/333/128
    per call.

### 4.2 Results at 48 kHz

THD+N is the **worst** value over all non-unity schedules: ±100, ±500 and
±2000 ppm, and `mod`. `varN` is listed separately.

| Candidate | THD+N 1 kHz | THD+N 10 kHz | THD+N 20 kHz | Multitone | Ratio exactly 1 (1 kHz) | `varN` 20 kHz | Passband 20 Hz–20 kHz (dB) | 22 kHz tone: gain / THD+N | Delay vs position (frames) | Measured − reported position |
|---|---|---|---|---|---|---|---|---|---|---|
| **P T64 β14 Hermite L32** | **−139.2** | **−138.6** | **−134.7** | **−138.7** | −153.0 (bit-exact) | −134.7 | **±0.00000** | −0.09 / −40.1 | 0.0000 | 2.6e-5 |
| P T96 β14 Hermite L32 | −137.8 | −137.6 | −136.7 | −137.6 | −153.0 | −136.7 | ±0.00000 | −0.00 / −86.7 | 0.0000 | 3.0e-5 |
| P T48 β12 Hermite L32 | −133.7 | −134.6 | −117.9 | −129.6 | −153.0 | −117.9 | +0.00001 | −0.28 / −29.8 | 0.0000 | 2.8e-5 |
| P T32 β9 Hermite L32 | −103.3 | −100.0 | −67.1 | −81.8 | −153.0 | −67.1 | −0.0041 | −0.71 / −21.4 | 0.0000 | 1.4e-4 |
| P T64 β14 linear L256 | −139.2 | −120.2 | −108.2 | −120.8 | −153.0 | −108.1 | −0.00007 | −0.09 / −40.1 | 0.0000 | 2.9e-5 |
| P T64 β14 linear L1024 | −139.2 | −137.6 | −130.4 | −137.8 | −153.0 | −130.4 | ±0.00000 | −0.09 / −40.1 | 0.0000 | 2.4e-5 |
| R1 rubato 5 sinc256 linear (today's params) | −136.5 | −120.0 | −108.1 | −120.7 | −142.7 | −108.2 | −0.00015 | −0.02 / −106.5 | −127.996 | not exposed |
| R1 rubato 5 sinc256 cubic (defaults) | −135.2 | −131.4 | −127.1 | −133.6 | −142.6 | −136.4 | −0.00002 | −0.04 / −128.1 | −127.992 | not exposed |
| R1 rubato 5 sinc64 cubic | −139.0 | −132.7 | −123.1 | −136.5 | −144.2 | −125.1 | **−14.9** (auto cutoff) | −50.5 / −90.0 | −31.992 | not exposed |
| R2 rubato 5 septic | −144.2 | −53.6 | −11.2 | −28.5 | −147.9 | −11.2 | −2.86 | −4.5 / −5.5 | −4.000 | not exposed |
| R2 rubato 5 cubic | −113.7 | −33.7 | −7.8 | −25.7 | −153.0 | −7.8 | −4.16 | −5.4 / −3.9 | −2.000 | not exposed |

Raw data: `spikes/s3-resampler/results/quality.tsv`, condensed in
`results/quality-summary.md`. The analytic predictions agree; for example, P
T64 β14 Hermite L32 predicts −136.5 dB at 20 kHz, −146.5 at 10 kHz and −148.7
at 1 kHz.

What the table shows:

- **The ratio does not matter.** For the in-house design the results are
  identical (±0.1 dB) at ±100, ±500 and ±2000 ppm, modulated or not. In-band
  THD+N depends only on the filter.
- **The floor is f32.**
  - About −139 dB is the f32 floor of a 64-term accumulation plus output
    rounding. The analytic filter error at 1 kHz is −149 dB.
  - The ratio-1 runs show −153 dB: the output is the input exactly, so only
    the f32 input quantisation of the analytic tone remains.
- **The prototype's stopband sets the 20 kHz figure.** It is −137 dB at
  ≥ 28 kHz for T64 β14. Images of 20 kHz content at 28 kHz alias back next to
  the tone.
- **Content between 20 and 24 kHz lies in the transition band by design.** A
  22 kHz tone produces a −40 dB alias product at about 22 kHz. That is
  ultrasonic, so the requirement ("aliasing well below audibility") holds.
  T96 pushes it to −87 dB if ever wanted.
- **Today's parameters are limited by linear interpolation.** rubato with 256
  taps, 256× oversampling and linear interpolation is limited by the linear
  interpolation between oversampled points, not by its 256 taps. Its −108 dB
  at 20 kHz matches the analytic "linear L = 256" row.

### 4.3 Fixed SRC in the same stage

The run is `quality src` → `results/quality-src.tsv`. The ratio is the
nominal ratio times (1 + 300 ppm·sin), with tones defined at the input rate.

| Case | Design | THD+N 1k / 10k / 20k | Multitone | Passband |
|---|---|---|---|---|
| 44.1 → 48 kHz | T96 β14 Hermite L32, fc = 1 | −137.8 / −137.6 / −135.7 | −137.5 | ±0.00000 |
| 44.1 → 48 kHz | T64 (too short for a 4.1 kHz transition) | −139.1 / −138.4 / **−47.9** | −62.8 | −0.035 |
| 48 → 44.1 kHz | T96 β14, fc = 44.1/48 | −140.2 / −137.7 / −135.9 | −138.5 | −0.0002 |
| 48 → 44.1 kHz | T112 β14, fc = 44.1/48 | −139.6 / −136.9 / −136.2 | −138.1 | ±0.00000 |
| 44.1 → 48 kHz | rubato 5 sinc256 linear | −136.8 / −118.7 / −106.7 | −119.3 | −0.0006 |

**Separate SRC stage: not needed.**

- One stage with the nominal ratio folded into the step gives:
  - one filter pass;
  - one position counter in source frames, which is what `N_play` must count;
  - one ring.
- The table is designed for the `(fs_in, fs_out)` pair at init:
  - cutoff = `min(1, fs_out/fs_in)` relative to the input Nyquist;
  - passband 20 kHz;
  - stopband from `min(fs_in, fs_out) − 20 kHz`.
- Analytic design rules (`analysis/explorer-*.txt`):

  | Rate | Taps (β = 14) | Worst THD+N |
  |---|---|---|
  | 48 kHz, drift only | T = 64 | −136 dB |
  | 44.1 kHz family, or 44.1 ↔ 48 | T = 96 | −135 dB |
  | 88.2 / 96 kHz | T = 32 | −137 dB |

  At 96 kHz, 32 taps cost per second what 64 taps cost at 48 kHz.
- A dedicated fixed-rational stage would only pay off if 44.1 kHz material at
  24 channels became a CPU problem. At 0.43 % of a core (§5) it is not.

## 5. CPU

Measured with `src/bin/bench.rs`:

- one thread pinned to one core, `nice -n 19`;
- best of 5 runs of 4 s of audio;
- N = 256, with ratio 1 + 300 ppm ± 50 ppm updated every call;
- the ring → resampler `memcpy` is inside the timed region.

The table gives ns per output frame (all channels). Today's path (R0) works
in chunks of 1024 input frames and includes its planar deinterleave and
interleave copies, but not the `ArrayQueue` atomics.

| Candidate | 2 ch | 8 ch | 16 ch | 24 ch | ns/frame/ch at 24 ch | **% of one core, 24 ch at 48 kHz** | 24 ch, x86-64-v3 build | Steady-state allocations |
|---|---|---|---|---|---|---|---|---|
| **P T64 Hermite L32** | 30 | 29 | 42 | **61** | 2.56 | **0.29 %** | 54 ns = 0.26 % | 0 |
| P T96 Hermite L32 | 44 | 41 | 61 | 90 | 3.74 | 0.43 % | 79 ns = 0.38 % | 0 |
| P T48 Hermite L32 | 23 | 23 | 33 | 48 | 2.02 | 0.23 % | 42 ns = 0.20 % | 0 |
| P T32 Hermite L32 | 17 | 17 | 24 | 35 | 1.44 | 0.17 % | 29 ns = 0.14 % | 0 |
| P T64 linear L256 / L1024 | 27 | 26 | 39 | 59 | 2.44 | 0.28 % | 52 ns = 0.25 % | 0 |
| R1 rubato 5 sinc256 linear | 44 | 103 | 178 | 270 | 11.24 | 1.29 % | 269 ns = 1.29 % | 0 |
| R1 rubato 5 sinc256 cubic | 65 | 126 | 202 | 297 | 12.36 | 1.42 % | 294 ns = 1.41 % | 0 |
| R1 rubato 5 sinc64 cubic | 37 | 61 | 105 | 137 | 5.71 | 0.66 % | 137 ns = 0.66 % | 0 |
| R2 rubato 5 septic | 18 | 57 | 110 | 162 | 6.77 | 0.78 % | 141 ns = 0.68 % | 0 |
| R2 rubato 5 cubic | 7 | 21 | 39 | 57 | 2.36 | 0.27 % | 51 ns = 0.24 % | 0 |
| **R0 rubato 0.14 SincFixedIn (today)** | 54 | 182 | 357 | **543** | 22.6 | **2.61 %** | 508 ns = 2.44 % | 0 |

Notes:

- **Per-call overhead is negligible.** The cost per frame does not depend on
  N. At 24 ch, P T64 runs at 62 / 61 / 62 ns for N = 64 / 256 / 1024.
- **rubato picks AVX at runtime in both builds**, so its two columns are
  nearly identical. The in-house baseline build is plain SSE2. The repo sets
  no `target-cpu`, so that is what ships.
- **Stereo takes the scalar path** (fewer than 4 channels), so 2 ch costs as
  much as 8 ch. That is 0.15 % of a core, and irrelevant on a PC.
  - If an in-order embedded core ever needs it, a C = 2 specialisation that
    duplicates the kernel `[h0,h0,h1,h1…]` and runs the 4-lane path over
    frame pairs takes about 15 lines.
- **Embedded estimate (aarch64, verified to vectorise)** for T64 at 24 ch:
  - about 384 `fmla.4s` plus about 50 for the kernel per frame;
  - Cortex-A73/A76 class (two 128-bit NEON pipes): about 220 cycles, i.e.
    **≈ 0.5 % of a 2 GHz core**;
  - in-order A53/A55: about 450–900 cycles, i.e. **≈ 1–2.5 %** of a 1.8 GHz
    core;
  - T48 (−118 dB at 20 kHz, −134 dB at 10 kHz) is the cheaper fallback at
    3/4 of the cost.
  - armv7 without NEON would run scalar at about 74 M FMA/s for 24 ch × 64
    taps. Use T = 48 or 32 there.
- **Memory:**
  - table: 32 KiB (T64) or 48 KiB (T96);
  - history: `(T + ⌈N_max·s_max⌉ + 4)·C·4 B`, i.e. 400 KiB for 24 ch at
    N_max = 4096. Rendering internally in sub-blocks of at most 256 frames
    would cap it at about 31 KiB with the same API (Phase 2).

## 6. Recommendation

Adopt the in-house resampler, with this design table:

| `(fs_in, fs_out)` | Taps T | Window | Cutoff (relative to input Nyquist) | Phase table | Group delay (look-ahead) |
|---|---|---|---|---|---|
| equal, ≤ 48 kHz (drift only) | **64** | Kaiser β = **14** | **1.0** (transition 20 → 28 kHz at 48 kHz) | Hermite cubic, **L = 32** segments, f32, DC-normalised rows, 32 KiB | **32 frames** = 0.667 ms at 48 kHz |
| 44.1 kHz family, or 44.1 ↔ 48 | 96 | Kaiser β = 14 | `min(1, fs_out/fs_in)` | Hermite, L = 32, 48 KiB | 48 input frames (1.09 ms at 44.1 kHz) |
| ≥ 88.2 kHz | 32 | Kaiser β = 14 | `min(1, fs_out/fs_in)` | Hermite, L = 32, 16 KiB | 16 input frames |
| constrained CPU option | 48 | Kaiser β = 12 | as above | Hermite, L = 32 | 24 frames |

The group delay of the filter is **0 relative to the reported position**
(linear phase, centred kernel) and **T/2 frames relative to the ring read
cursor**. The servo uses `position()` directly as `N_play`, with no
subtraction.

**Ratio contract.** The resampler takes input frames per output frame:
`ratio = (fs_in/fs_out) · r_ff · (1 + u)`.

- The plan's ±1000 ppm `r_ff` sanity bound and ±500 ppm `u` clamp sit well
  inside the tested range: ±2000 ppm showed no degradation, and construction
  takes a `max_step` for buffer sizing.
- No re-design of the table is needed for ratio changes of that size. The
  cutoff stays at the input Nyquist, and the 2000 ppm shift of the output
  Nyquist is 48 Hz inside a 4 kHz guard band.

## 7. API sketch for Phase 2

```rust
/// Built once from the stream's rates (see the design table).
pub struct ResamplerDesign { taps: usize, segments: usize, beta: f64, cutoff: f64 }
impl ResamplerDesign {
    pub fn for_rates(fs_in: u32, fs_out: u32) -> Self;
}

/// Exact input position: integer frame + Q0.32 fraction.
#[derive(Clone, Copy)]
pub struct Position { pub frame: i64, pub frac: u32 }

pub struct DriftResampler { /* table, kernel scratch, history, Q32.32 pos/step */ }

impl DriftResampler {
    /// Allocates everything: table, history for `max_quantum` frames at
    /// `max_step` input frames per output frame.
    pub fn new(channels: usize, design: &ResamplerDesign, max_quantum: usize, max_step: f64) -> Self;
    /// Silence history; next output frame is at source frame `at`
    /// (start / realign, after the fade-out).
    pub fn reset(&mut self, at: i64);
    /// Plan `n_out` frames with the step ramping linearly to `ratio`
    /// (input frames per output frame). Returns the input frames to supply.
    pub fn prepare(&mut self, n_out: usize, ratio: f64) -> usize;
    /// Interleaved slot for exactly `prepare()`'s frames; the ring reader
    /// memcpys into it (two segments).
    pub fn input_slot(&mut self) -> &mut [f32];
    /// Writes exactly `n_out` interleaved frames.
    pub fn render(&mut self, out: &mut [f32]);
    /// N_play: source position of the next output frame (exact).
    pub fn position(&self) -> Position;
    /// Source frames pulled so far (ring read cursor, in source frames).
    pub fn consumed(&self) -> i64;
    /// Constant look-ahead T/2 (for latency budgeting / minimum L).
    pub fn lookahead_frames(&self) -> usize;
}
```

The output-core callback body, the same for every backend:

```rust
let need = rs.prepare(n, ratio_total);
if !ring.read_exact_into(rs.input_slot()) {
    // underrun: zero-fill, count a discontinuity event, request realign
}
rs.render(out);
servo.update(t_m, rs.position(), d_dev);
```

`prepare` and `render` are split so that the caller handles an underrun
(realign per plan §2.4) before any audio is produced, and so that the ring
read lands directly in the resampler's history.

Carry over to Phase 2:

- The two unit tests:
  - position equals the Q32.32 closed form over mixed N and ratios;
  - ratio 1 is bit-exact.
- The quality harness, run as a CI test on short signals:
  - THD+N < −130 dB at 1 kHz and 10 kHz;
  - passband ±0.001 dB;
  - measured vs reported position < 10⁻⁴ frames.
- The counting-allocator check.
- Sub-block rendering, to bound the history buffer.
- A codegen check that the 4-lane groups still vectorise on aarch64 (§3.3).

## 8. Reproduction

```sh
cd spikes/s3-resampler
cargo test --release                                   # unit tests
nice -n 19 taskset -c 7 cargo run --release --bin quality -- results/quality.tsv
nice -n 19 taskset -c 7 cargo run --release --bin quality -- results/quality-src.tsv src
nice -n 19 taskset -c 9 cargo run --release --bin bench > results/bench-x86-64.tsv
CARGO_TARGET_DIR=target-v3 RUSTFLAGS="-C target-cpu=x86-64-v3" \
  nice -n 19 taskset -c 9 cargo run --release --bin bench > results/bench-x86-64-v3.tsv
python3 analysis/summarize.py results/quality.tsv
OMP_NUM_THREADS=1 python3 analysis/design_explorer.py 48000 20000   # analytic table
```
