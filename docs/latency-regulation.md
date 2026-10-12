# Latency regulation

How `orender render` holds its output at a latency target. This is the
summary of what runs; the design, its measurements and the slices it was
built in are in [resampling-rework-plan.md](resampling-rework-plan.md).

## The contract

The **latency target** (`latency_target`, ms) is the time between the moment
audio reaches the renderer and the moment it leaves the output device. It is
the same number for every source and every backend, and it includes the
decoder's look-ahead and the render's own delay.

A stream that cannot be played at the target (its decoder holds more audio
than the target leaves room for) is played at the latency it needs instead:
`/state/audio` says so (`latencyTargetHeldMs`), the Studio shows it under the
target, and the configured value is left alone.

## What is measured

Everything is stamped against one monotonic clock.

- **In**: source frames received, counted before decoding, on the transport
  the input arrives in (so a burst that decodes later is already counted).
- **Played**: source frames that have left the resampler towards the device.
- **Device delay**: what the backend reports is still in front of the
  converter.

```
latency = (in − played) / rate + device delay
```

No low-pass, pacer or calibration sits between the measurement and the loop.

## The loop

```
ratio = feed-forward · (1 + u)
u     = Kp·e + Ki·∫e dt          e = latency − target
```

- The **feed-forward** is the rate of the source over the rate of the device,
  each estimated against the monotonic clock. It carries the drift between
  the two clocks (tens of ppm) with almost no noise.
- The **PI** only corrects phase. It is set by one number, the loop bandwidth
  `B`: `ω = 2πB`, `Kp = 2ω`, `Ki = ω²`, integrated per second with the
  measured interval, so it does not depend on the device's cycle. `u` is
  clamped to ±500 ppm.
- A feed-forward more than 1000 ppm from nominal is a clock that is wrong, not
  drift: it is reported as a mismatch and clamped.

One polyphase resampler applies the ratio, on the device's side of a
lock-free ring.

## Start and realign

- **Start**: silence until the latency would equal the target, then the first
  sample on the exact frame.
- **Realign**: when the error passes the realign threshold, on an underrun or
  on a full ring, the output fades out, drops or inserts exactly what puts the
  error back at zero, and fades in. The feed-forward and the integrator are
  kept: they describe drift, not phase. Each realign is counted.

There is nothing else: no near/far bands, no recovery states, no settling
dwell.

## The source's clock

`source_clock` says what clock the source runs on. `auto` takes the input's
own:

| Clock | Input | What the loop does |
|---|---|---|
| `own` | the renderer's PipeWire sink | the sink runs on the renderer's clock: nothing to estimate |
| `follow` | a pipe written at its writer's pace (mpv) | the writer's rate is estimated from when its audio arrives |
| `none` | a file, a writer faster than real time | the device is the master; the writer is held back to its pace |

A source with a clock is never held back: blocking it would replace its clock
with the device's. The renderer falls back to the input's own clock when asked
for one the input cannot have.

## Nothing is lost quietly

Every frame dropped or inserted between the input and the device is an event
with its position and its cause: a gap in the stream is filled with silence
for its exact length, an overlap is trimmed, and what a decoder loses or makes
up is found by an audit of what went in against what came out. The counts are
published with the diagnostic metrics (`frames_lost`, `frames_filled`,
`realigns`, `underruns`).

## Settings

| Option | Default | Meaning |
|---|---|---|
| `latency_target` | — | the latency to hold, ms |
| `loop_bandwidth_hz` | 0 | bandwidth of the loop; 0 takes the source's own (0.02 Hz for the sink, 0.005 Hz for a pipe) |
| `realign_threshold_ms` | 20 | latency error beyond which the output realigns |
| `source_clock` | `auto` | the clock the source runs on (above) |

All four are host options of the standalone renderer
([live-options-registry.md](live-options-registry.md)): a config key under
`render`, a command-line flag, and a control over OSC. In the Studio they are
in the Latency section, the last three in the advanced view.

## What a client sees

- `/state/latency_instant`: the latency measured; `/state/latency_target`:
  the target; `/state/resample_ratio`: the ratio applied.
- The diagnostic metrics (`/state/diag_schema`, `/state/diag_values`): the
  latency error, the feed-forward and the correction in ppm, and the counts
  above.

## Configs written for the regulation this replaced

The keys of the adaptive resampler (`enable_adaptive_resampling`,
`adaptive_resampling_*`) and the shape of the old live input (`live_input`'s
`backend`, `layout`, `clock_mode`, `channels`, `sample_rate`, `map`,
`lfe_mode`) are still read. They do nothing: a run says once which of them it
found, and a save keeps them as they were.
