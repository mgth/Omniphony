# Spike S1 — `own` source-clock mechanics

Phase-0 spike for [resampling-rework-plan.md](../resampling-rework-plan.md) §2.1
(`own` mode). Date: 2026-10-01. Code: `spikes/s1-own-clock/` (throwaway C, see
"Reproducing" at the end).

## 1. Question

In `own` mode, the `omniphony` PipeWire sink (which mpv, VLC and Kodi play into,
including IEC958 passthrough) must be clocked on `CLOCK_MONOTONIC` with absolute
deadlines. The player must see a clean clock that is not the DAC, plus a
constant advertised latency. Which mechanism should do this? For each candidate:

- What clock does a **follower** (the player) see: `spa_io_clock.nsec`,
  `position`, `rate_diff`, `delay`, and `pw_time`?
- Can IEC958 passthrough be negotiated?
- How is the latency advertised?
- How much do cycle start times jitter against the ideal deadlines?

## 2. Answer in one paragraph

**Use option (c).** orender creates its own PipeWire timer driver: a
`support.node.driver` node created through `spa-node-factory`, with
`clock.id = monotonic` and `priority.driver = 0`. The existing `pw_stream`
sink becomes a plain **follower** (no `DRIVER` flag), placed in the same
`node.group`, with `RT_PROCESS` and `node.always-process = true`.

This gives the player a mathematically clean clock:

- `nsec = position·10⁹/rate`, exactly. The fit residual is ≤ 1 ns and the rate
  error is 0.000 ppm.
- `rate_diff = 1`, and deadlines are absolute.

The approach costs no custom realtime code and leaves IEC958 negotiation
unchanged. Unlike (a), a stalled orender cannot delay the player's wakeup.

Option (a) gives the same clock, but only if orender fills
`spa_io_position.clock` itself. orender does not do that today (a0): **the
current driver sink presents a frozen clock**, and mpv's A/V clock then shows a
21 ms sawtooth with zero latency knowledge. Option (b) (null-audio-sink) cannot
carry IEC958 and is rejected.

## 3. Setup

- PipeWire 1.6.9, WirePlumber 0.5.17, kernel 7.2 (`tsc` clocksource).
- Graph settings: `clock.rate 48000`, `clock.allowed-rates [48000]`,
  `quantum 1024` (32–2048).
- Client data loops run `SCHED_FIFO` 83; the daemon data loop runs FIFO 88.
- The production `orender` and the default sink were left untouched.
- Every test node was named `rwspike-s1-*` and nothing was linked to hardware.
- All media was digital silence.
- Players were connected with `node.dont-fallback = true` and
  `node.dont-reconnect = true`.

Programs (C, `libpipewire-0.3`):

- **`s1sink -m a0|a|c|b`**
  - Implements the four variants below.
  - Logs every timer wakeup against its deadline (a0/a).
  - Logs every `process` call with the `spa_io_position.clock` it sees, and the
    bytes received (checked all-zero for PCM).
- **`s1play`**
  - A follower that stands in for mpv's `ao_pipewire`.
  - Plays silence as raw S16 or IEC958 (AC-3 48k, E-AC-3 192k, TrueHD 192k 8ch).
  - Records `pw_stream_get_time_n()` and the raw clock every cycle.
  - Re-computes mpv's `end_time` formula verbatim (`audio/out/ao_pipewire.c`
    on_process): `mp_time + nframes/rate + delay + queued + buffered − (pw_get_nsec − time.now)`.
- **Real mpv**
  - `/usr/bin/mpv` 0.41, run with `--no-config --no-video --vo=null --ao=pipewire --audio-device=pipewire/rwspike-s1-X`.
  - Plays PCM WAV, or `--audio-spdif=ac3,eac3` with AC-3/E-AC-3 files.
  - `mpvpoll.py` reads `audio-pts` over IPC every 20 ms.
  - `t_monotonic − audio_pts` is the instant mpv believes pts 0 is heard. It
    must be constant on a clean clock.
- **`pw-profiler -J`**
  - Driver-side timing: `signal` (when the driver started the cycle) against
    `clock.nsec`.

Each main measurement ran for ≥ 60 s; the first 2 s were dropped as warm-up.

## 4. Options tried

| | Mechanism | Who fills `spa_io_clock` |
|---|---|---|
| **a0** | `pw_stream`, `media.class=Audio/Sink`, `PW_STREAM_FLAG_DRIVER`. An absolute `CLOCK_MONOTONIC` timerfd on the stream's data loop calls `pw_stream_trigger_process()` | **Nobody.** This is today's orender `PwStream` driver path (orender additionally runs it on the main loop, `node.async=true`) |
| **a** | Same as a0, but the timer callback writes `clock.{nsec=deadline, rate, position, duration, delay=0, rate_diff=1, next_nsec}` into the `SPA_IO_Position` area (from `io_changed`) before triggering. Same pattern as `module-pipe-tunnel` / `gstpipewiresink` | the app |
| **b** | `pw_core_create_object("adapter", factory.name=support.null-audio-sink, node.driver=true)`, plus a capture stream on its monitor (`stream.capture.sink=true`) | PipeWire (`null-audio-sink.c`) |
| **c** | `pw_core_create_object("spa-node-factory", factory.name=support.node.driver, clock.id=monotonic, node.group=G, priority.driver=0)`. The sink `pw_stream` is a follower in `node.group=G`, with `RT_PROCESS` and `node.always-process=true` | PipeWire (`node-driver.c`) |

Other mechanisms looked at in the 1.6 source:

- **`node.group = pipewire.dummy`.** Reusing the system Dummy-Driver is the same
  code as (c). However, that driver is the global fallback target for every
  unassigned node, and it has `priority.driver 200000`.
- **`PW_STREAM_FLAG_TRIGGER` / lazy scheduling.** These are for followers
  asking a driver for cycles; they do not apply here.
- **`pw_filter`.** It gives no IEC958 negotiation convenience.

## 5. What the follower sees

60 s runs: s1play (PCM 48k), with the sink advertising `Latency = 100 ms`
except in (b).

| | a0 (today) | a | b | c |
|---|---|---|---|---|
| `clock.nsec` | **frozen** at the value the scheduler wrote at setup (stale on 2905/2906 cycles; 4 s → 66 s old) | ideal deadline grid | grid of `1024·10⁹/48000` truncated to 21 333 333 ns | `scale(position, 10⁹, rate)` exact |
| `clock.position` / `pw_time.ticks` | **frozen** | +duration per cycle | +duration | +duration (absolute: monotonic·rate) |
| nsec-vs-position fit | n/a | 0.000 ppm, residual ≤ 1 ns | **−0.016 ppm** (integer truncation; 58 µs/h) | 0.000 ppm, residual ≤ 1 ns |
| `rate_diff` | **0.0** | 1.0 | 1.0 | 1.0 |
| `clock.delay` | 0 | 0 | 0 | 0 |
| `clock.name` / `clock.id` | "" / stream node | app-chosen / stream node | `clock.system.monotonic` / null sink | `clock.system.monotonic` / driver node |
| `pw_time.delay` | 4800 (100 ms) | 4800 | 0 (cannot advertise from the stream) | 4800 |
| mpv `end_time` drift vs frames written (s1play model) | **−62 s** over 60 s | std 0.012 µs | −1.0 µs over 60 s | std 0.020 µs |
| Latency mpv believes at write (model) | −3.9 s … −66 s → mpv clamps it to **0** | 121.310 ms, std 0.011 ms | 21.31 ms | 121.315 ms, std 0.008 ms |

The 121.3 ms in (a) and (c) is 100 ms advertised plus the 1024-frame buffer
being written (21.3 ms). mpv's `end_time` refers to the last sample of that
buffer.

Real mpv (`audio-pts` over IPC, 64 s; the residual is after a linear fit and is
dominated by IPC noise, median round-trip time ≈ 70–90 µs):

| Run | slope of `t − audio_pts` | \|residual\| p50 / p99 / max |
|---|---|---|
| a0, PCM | +0.47 ppm | **5352 / 10 562 / 10 681 µs** (21.3 ms sawtooth) |
| a, PCM | +0.020 ppm | 2.5 / 16.7 / 61 µs |
| a, AC-3 passthrough | +0.021 ppm | 2.6 / 16.8 / 35 µs |
| b, PCM | −0.003 ppm | 2.6 / 17.1 / 30 µs |
| c, PCM | −0.008 ppm | 2.5 / 14.4 / 60 µs |
| c, AC-3 passthrough | −0.003 ppm | 2.5 / 16.0 / 42 µs |
| c, E-AC-3 passthrough (192 kHz) | −0.002 ppm | 2.5 / 15.8 / 61 µs |

**Today's driver sink (a0) gives mpv no usable clock.** `time.now` never moves,
so mpv's `end_time` lies in the past. `buffer.c` clamps `driver_delay` to 0, and
`audio-pts` then follows mpv's own write bursts.

## 6. Jitter of cycle starts against ideal deadlines

All values are in µs. Runs lasted ≥ 55 s, about 2600–3300 cycles at 21.33 ms.

| | a0 | a | b | c |
|---|---|---|---|---|
| Driver timer wake − deadline (sink log), mean / p50 / p99 / p99.9 / max | 10.5 / 10.4 / 21.5 / 110 / 235 | 10.9 / 10.2 / 21.2 / 193 / 224 | (in daemon) | (in daemon) |
| Driver `signal` − `clock.nsec` (pw-profiler), p50 / p99 / p99.9 / max | — | 14.7 / 28.3 / 159 / 243 | — | **3.2 / 16.6 / 152 / 231** |
| Player wake − `clock.nsec`, mean / p50 / p99 / p99.9 / max | (nsec frozen) | 23.5 / 22.8 / 39.3 / 205 / 238 | 25.1 / 24.1 / 44.4 / 193 / 279 | 18.0 / 17.2 / 30.6 / 84 / 261 |
| Sink `process` − `clock.nsec`, mean / p99 / max | (frozen) | 32.7 / 54.9 / 253 | 27.6 / 46.2 / 283 | 36.7 / 57.9 / 282 |
| Sink period std | 20.5 | 17.1 | 13.3 | 12.1 |

No timer ever reported more than one expiration, and no xrun was counted.
Wakeup jitter is tens of µs: three orders of magnitude below anything that
matters for a 20 ms-scale servo. The **reported** clock (`nsec`) carries no
jitter at all in a, b and c, because it is the ideal deadline and not a
measured wakeup.

**Robustness probe** (`-S`: the sink's `process` sleeps 30 ms every 250
cycles, 30 s run):

| | a | c |
|---|---|---|
| Player wake − `clock.nsec`, p99 / p99.9 / max | 44.7 µs / **8 731 µs / 8 753 µs** | 35.2 µs / 181 µs / 189 µs |
| Clock seen by the player | clean (fit residual ≤ 1 ns) | clean |

In (a) the timer shares orender's data thread, so an orender stall delays the
**player's** cycle. In (c) the driver lives in the daemon: the player stays on
time, and only the sink's own `process` runs late.

## 7. Latency advertisement

The sink advertised 100 ms (mode c; same results in a):

| Method | `pw_time.delay` seen by the player | mpv-believed latency |
|---|---|---|
| nothing | 0 | 21.3 ms (one quantum) |
| **`SPA_PARAM_Latency`, `direction = INPUT`, `min_ns = max_ns = L`**, via `pw_stream_update_params` | **4800 frames = 100.000 ms, std 0** | 121.31 ms |
| `SPA_PARAM_ProcessLatency{ns = L}` on the `pw_stream` | **0**: pw_stream stores it but never adds it to port latency | 21.3 ms |
| Latency param, sink **without `RT_PROCESS`** (`node.loop.class=main`, `node.async=true`, today's orender) | 5824 = 4800 + **1024**: an async follower adds one quantum | 142.65 ms |
| Same without `RT_PROCESS`, but the sink is the driver (a) | 4800 | 121.31 ms |

The Latency param reaches mpv exactly and constantly; `pw_time.delay` is
derived from port latency, not from `clock.delay`, which `copy_position()`
zeroes. Also, `PW_KEY_NODE_LATENCY` (`node.latency`) is only a quantum request
to the scheduler; it is not an advertised latency.

## 8. IEC958 passthrough

| Player format | a | b | c |
|---|---|---|---|
| s1play AC-3 @ 48k | ✅ | ❌ "defined target not found" | ✅ |
| s1play E-AC-3 @ 192k | ✅ | ❌ | ✅ |
| s1play TrueHD @ 192k, 8 ch | ✅ | ❌ | ✅ |
| mpv `--audio-spdif` AC-3 | ✅ (`AO: 48000Hz stereo spdif-ac3`) | ❌ (mpv retries in a loop) | ✅ |
| mpv `--audio-spdif` E-AC-3 | — | — | ✅ (`AO: 192000Hz stereo spdif-eac3`) |

- **b** cannot work. `null-audio-sink` only enumerates raw `F32`/`F32P`, so
  there is no passthrough port config.
- **a and c** are identical here, because the IEC958 sink is the same
  `pw_stream` in both. Two sink-side conditions are needed for a 4× carrier to
  flow in real time:
  1. **The graph rate must be the carrier rate.** With
     `clock.allowed-rates=[48000]`, the scheduler kept the group at
     1/48000 × 1024 even with `node.rate=1/192000`. An E-AC-3 player was then
     fed 1024 carrier frames per 21.3 ms: **¼ real time**. Setting
     `node.force-rate = <negotiated IEC958 rate>` dynamically fixes it: in
     `param_changed(Format)`, call `pw_stream_update_properties`, and use
     `0` for PCM or for a 48k carrier. The group then ran at 1/192000 × 4096.
     A sequence of E-AC-3 192k → AC-3 48k → PCM → TrueHD 192k players on one
     sink instance renegotiated correctly in both a and c. Each switch costs
     one or two cycles of gap, which is a discontinuity: realign.
     - This removes the old "force-rate makes 48 kHz carriers unreachable"
       problem noted in `pipewire_pods.rs`. The force follows the format; it
       is not pinned.
  2. **A `Buffers` param sized for a full quantum** (8192 frames × 16 bytes).
     Without it, the player's buffers held 2048 frames, so only half a
     4096-frame quantum arrived per cycle.
  - Measured afterwards:
    - E-AC-3: 16 384 bytes/cycle (4096 frames × 4 B).
    - TrueHD: 65 536 bytes/cycle (4096 × 16 B).

## 9. Other checks

- **Quantum changes.** A player requesting `node.latency=256/48000` moved the
  group to 256 frames. Both (a) and (c) followed: the clock stayed on one grid
  (fit residual ≤ 1 ns) and the latency mpv believes was 105.3 ms.
- **No player.** With `node.always-process=true`, the sink keeps cycling on
  the M clock in (a) and (c), so orender keeps a continuous `own` timeline
  when nothing plays.
- **No hardware link.** Every node stayed unlinked from hardware, and none
  became the default sink.
- **Cleanup.** All nodes disappeared with their client:
  `pw-cli ls Node | grep rwspike-s1` returned nothing after every run. In (c),
  the driver node is owned by orender's client connection, so a crash removes
  it.

## 10. Recommendation and rationale

**Adopt (c) for `own` mode.** Keep (a) as the documented fallback, only for a
PipeWire without `spa-node-factory` (it is loaded by the default
`pipewire.conf`).

1. **Clock.** It is the same ideal clock as (a), with `position` tied
   absolutely to `CLOCK_MONOTONIC`. As a result S ≡ M holds exactly and
   orender needs no estimation (plan §2.1). The resulting rates are:
   - `r_S = 1`;
   - `N_in(t)` is known in closed form from `clock.position` and
     `clock.nsec`.
2. **No realtime code of our own.** There is no timer, no clock writes into
   shared memory (unsafe in Rust), and no re-anchoring on quantum or rate
   changes. PipeWire's `node-driver.c` already does absolute deadlines
   (`next = scale(position + duration)`, `TFD_TIMER_ABSTIME`) and restarts
   cleanly on rate switches.
3. **Isolation.** An orender hiccup cannot delay the player (§6 stall probe).
   The driver wakes with lower jitter (p50 3 µs against 15 µs) from the
   daemon's FIFO-88 thread.
4. **IEC958 and latency.** These are unchanged from the current sink: same
   `pw_stream`, same formats, Latency param on the input port.
5. **Embedded fit.** It adds no thread and no per-cycle work in orender.

(a) is acceptable but strictly more code, and more fragile. (b) is out (no
IEC958, an extra hop, −0.016 ppm truncation, latency not advertisable from our
stream). The current a0 path must not be kept even transitionally: it is the
reason mpv sees no sink clock today.

## 11. Risks and unknowns

- **Driver merging.** When a group contains two drivers, the higher
  `priority.driver` wins. If a user links something that is also linked to a
  hardware node into the sink's group (e.g. a loopback or filter chain), the
  sink would follow the DAC instead of M. Mitigations:
  - **Detect** it in `process`: `clock.id` ≠ our driver's id, or
    `rate_diff` ≠ 1, or `clock.name` ≠ `clock.system.monotonic`. Then fall back
    to a `follow`-style estimation from `rate_diff` and `nsec`, and surface a
    warning.
  - Use `priority.driver = 0` on purpose. A positive value would make our
    driver the fallback target for unrelated unassigned nodes
    (`module-scheduler-v1.c` `context_recalc_graph`).
- **Group name collisions.** Use a unique `node.group` (e.g.
  `orender.own-clock.<pid>`).
- **Async placement.** The sink must use `RT_PROCESS`. Without it, PipeWire
  marks the node async, which adds one quantum to the latency players see and
  shifts orender's data one cycle late. The Rust crate runs `process` on the
  data thread with `RT_PROCESS`: its state must be thread-safe and lock-free
  (SPSC ring plus atomics, as the plan already requires).
- **Rate switches** produce a 1–2-cycle gap. Treat a Format change, or
  `SPA_IO_CLOCK_FLAG_DISCONT` or `XRUN_RECOVER`, as a realign event.
- **Global overrides.** `clock.force-quantum` or `clock.force-rate` set by
  another app still apply to our group. The clock stays clean, but quantum or
  rate change, and so must be read per cycle.
- **Not tested:**
  - VLC and Kodi (Kodi's native PipeWire sink is expected to behave like mpv;
    pipewire-pulse clients get latency through the same `pw_time.delay`);
  - long runs under heavy CPU load;
  - DTS-HD MA (8 ch 192k is the same path as TrueHD);
  - `ProcessLatency` on an adapter node for (b).
- **mpv clock model.** mpv's believed latency equals advertised `L` plus the
  current buffer. orender must therefore define a sample's source time as
  `clock.nsec + (p − clock.position)/rate`. It must not use the wall time of
  its own `process` call, and it must advertise exactly `L`, with no quantum
  subtracted.

## 12. API recipe for Phase 4 (`own` mode, option c)

**1. Driver node** (main thread, once per sink lifetime; keep the proxy alive;
dropping it or disconnecting removes the node):

```c
props = { "factory.name"    = "support.node.driver",
          "node.name"       = "<unique, e.g. orender-own-clock>",
          "node.description"= "orender own clock",
          "node.group"      = G,          /* unique, e.g. "orender.own-clock.<pid>" */
          "priority.driver" = "0",
          "clock.id"        = "monotonic",
          "node.freewheel"  = "false" };
proxy = pw_core_create_object(core, "spa-node-factory",
                              PW_TYPE_INTERFACE_Node, PW_VERSION_NODE, &props, 0);
```

In Rust: `core.create_object::<pw::node::Node>("spa-node-factory", &properties!{…})`.

**2. Sink stream.** Keep today's props: `media.class=Audio/Sink`,
`iec958.codecs`, `resample.disable`, `audio.channels/position`, `node.latency`,
`node.rate`. Add or change:

- `node.group = G`;
- `node.always-process = true`;
- flags: `MAP_BUFFERS | RT_PROCESS`, **without** `DRIVER`. Drop
  `AUTOCONNECT`, which is meaningless for a sink; the spike ran without it.

**3. Connect params** (`SPA_DIRECTION_INPUT`):

- one `EnumFormat` per IEC958 codec/rate (AC-3 48k, E-AC-3 192k, TrueHD 192k,
  DTS 48k, DTS-HD 192k), plus the raw alternative;
- `Buffers`: `buffers 2..16`, `blocks 1`, size ≥ quantum-limit (8192) × max
  stride (16 B for 8-ch IEC958);
- then, once: `SPA_PARAM_Latency{direction=INPUT, min_ns=max_ns=L}` through
  `pw_stream_update_params`. Republish only when the configured `L` changes,
  never from a measurement.

**4. `param_changed(SPA_PARAM_Format)`:**

- if the format is IEC958 at rate `R ≠ 48000`, set `node.force-rate = R`;
  otherwise set `"0"`;
- use `pw_stream_update_properties`. In Rust this is
  `pw_sys::pw_stream_update_properties(stream.as_raw_ptr(), dict)`, because the
  crate has no wrapper;
- signal a discontinuity to the sync core.

**5. `io_changed(SPA_IO_Position)`:** store the area pointer in an atomic.

**6. `process` (RT thread):**

- read `clock.nsec`, `clock.position`, `clock.duration`, `clock.rate`,
  `clock.id`, `clock.flags`;
- `N_in`: the first frame of this cycle's buffer has source time `clock.nsec`;
  frame `j` has `clock.nsec + j·10⁹/rate`;
- `r_S = 1` while `clock.id` = driver id and `rate_diff = 1`;
- realign on `DISCONT`, `XRUN_RECOVER`, a `clock.id` change or a Format
  change;
- dequeue, copy into the SPSC ring, requeue. No allocation, no lock.

**Fallback (a)**, if `spa-node-factory` is missing:

- create the stream with `DRIVER | RT_PROCESS`;
- `loop = pw_stream_get_data_loop(stream)`; on that loop (via
  `pw_loop_invoke`), call `pw_loop_add_timer` and arm it with
  `pw_loop_update_timer(loop, t, &abs, NULL, true)`;
- in the callback:
  - take `q = clock.target_duration` and `R = clock.target_rate.denom`;
  - re-anchor the grid if `R` changed;
  - write `clock.nsec = deadline`, `rate = 1/R`, `position = pos`,
    `duration = q`, `delay = 0`, `rate_diff = 1.0`, and
    `next_nsec = anchor + (pos + q − anchor_pos)·10⁹/R`;
  - re-arm at `next_nsec`;
  - call `pw_stream_trigger_process`;
- this needs raw `spa_io_position` writes (`unsafe`) and `pw_sys` for the data
  loop.

## Reproducing

```
cd spikes/s1-own-clock && make
./run.sh c 72 64 OUT -k port -l 100            # sink mode c + s1play, 100 ms advertised
./run.sh a 14 9 OUT -F -- -c eac3 -r 192000    # IEC958 E-AC-3 with dynamic force-rate
./runmpv.sh c 70 OUT silent.wav -k port -l 100 # real mpv, audio-pts over IPC
```

Sink flags: `-m a0|a|c|b`, `-k none|port|process -l <ms>` (latency
advertisement), `-F` (force-rate follows the format), `-N` (no `RT_PROCESS`),
`-S` (stall probe). Raw logs of this run: session scratchpad (not kept).
