# Spike S4 — Sample accounting through capture, decoder bridge and renderer

Phase-0 spike of [the resampling rework plan](../resampling-rework-plan.md)
(§2.2 "Accounting invariant", §4 Phase 0 S4). State read: workflow
`resampling-rework`, Omniphony `8450cbff` (= `main` `8eb253a6` + the audit
docs), bridge = `workflows/integration/harletty-bridge/` (read-only). Paths
below are relative to `omniphony-renderer/` unless stated otherwise.

The question: the new servo measures

```
L_e2e = (N_in − N_play) / f_s + D_dev
```

That only works if every source frame counted in `N_in` either reaches the
resampler (and is counted in `N_play`) or is reported. This document lists
every point between capture and the resampler where that does not hold today,
what each codec does to the count, where the counters should sit, and which
drop points must disappear rather than be reported.

## 0. Summary

- **No input path conserves the count today.** Live PipeWire input drops
  packets and decoded frames silently on full queues (`try_send`). Every path
  drops decoded frames silently while the output writer is missing
  (bootstrap gate, stream-not-ready, rebuild), and the back-pressure push can
  drop a partial frame, which rotates the channel interleaving.
- **The burst figure in the plan is wrong by 16×.** TrueHD is emitted per MAT
  frame: about 24 access units of 40 samples, i.e. 960 frames = **20 ms** per
  `push_packet` (measured 23–25 AUs, `docs/LATENCY_DAC_SAWTOOTH_REPORT.md`
  §"Direct measurement of frames-per-push_packet"). The ~320 ms / 3.1 Hz
  figure is the **beat** between 960-frame decoder bursts and the 1024-frame
  resampler chunk (1/20 ms − 1/21.33 ms = 3.125 Hz), with a 16–18 ms p-p
  amplitude. The simulator should model 20 ms TrueHD bursts plus the
  ingest accumulation window, not 320 ms bursts.
- **Count `N_in` in transport time at the capture point; make ingest
  time-conserving.** Every IEC 61937 span (data burst, pause burst, null
  stuffing, garbage during sync loss) maps to an exact PCM duration. Data
  bursts go to the decoder. Everything else becomes explicit silence of the
  same duration. Decoder output is then audited per burst against the
  burst's nominal length, which catches decoder drops without bridge
  cooperation.
- **Accounting API:** each stage between the two counters owns two
  monotonic per-epoch counters (`inserted`, `dropped`, in source frames),
  bumped once per event, never per sample. The servo reads
  `N_in_eff = N_in + Σinserted − Σdropped`. A non-realtime event queue
  carries `DiscontinuityEvent { epoch, stage, position, delta_frames, reason }`
  for diagnostics. Format, rate, source or writer changes start a new
  **epoch**: counters reset and the start rule applies.
- **Make impossible rather than report:** the live packet and frame queues
  (replace them with SPSC rings sized from `L_max`), sample-granular partial
  pushes, the bootstrap gate, stream-not-ready drops, back-pressure timeouts,
  the pacer and its zero fills, and the recovery trims. **Report** only true
  faults (ring overflow or underrun, decoder shortfall, parser resync) and the
  realigner's own corrections.

## 1. Input paths, capture to `write_samples`

### 1.1 Thread and queue topology (today)

```
                     ┌──────────────── live PipeWire sink (Linux) ─────────────────┐
PipeWire graph ─► pw-live-bridge-<node> thread (main loop, no RT flag)
                  process cb (audio_input/src/pipewire.rs:661)
                   ├─ IEC958: accumulate 4 callbacks (:980-987) ─► SpdifParser
                   │          ─► raw_tx.try_send  (sync_channel 256 pkts, src/cli/decode/live_input.rs:422;
                   │                               audio_input/src/bridge.rs:27-33)
                   │          ─► "bridge-decode" thread (live_bridge.rs:58) ─► bridge.push_packet
                   │          ─► tx.try_send (main channel)            (live_bridge.rs:183)
                   └─ PCM:    build_live_input_frame (allocates) ─► tx.try_send (live_input.rs:531)
                     └──────────────────────────────────────────────────────────────┘
pipe / file ─► decoder thread (decoder_thread.rs:100): poll+read 64 KiB (sys/src/input.rs:560)
               ─► SpdifParser or Raw ─► bridge.push_packet ─► tx.send (blocking) (decoder_thread.rs:361)

main channel: mpsc::sync_channel(clamp(2·latency_target_ms, 512, 8192) messages)
              (session_run.rs:24-27,165-172,276); one message = one decoded frame
              (TrueHD 40 samples, E-AC-3 1536, …); shared by ALL producers.
   ─► main loop process_decoder_messages (session_run.rs:644) ─► DecodeHandler::handle_decoded_frame
   ─► renderer ─► AudioWriter::write_pcm_samples (output.rs:223)
   ─► PipewireWriter::write_samples (audio_output/src/pipewire.rs:494)
       ─► [pacer FIFO (pacer.rs) drained by the capture thread or the pacer-bridge-drain thread]
       ─► ring ArrayQueue<f32> (ring_buffer_io.rs:8, 4 s × 16 ch)
       ─► output callback ─► rubato + output FIFO ─► device
```

Observations that matter for accounting:

- **One channel, several producers.** The pipe decoder thread, the live
  bridge decoder and the live PCM callback all post into the same
  `sync_channel`. The live bridge decoder tags its frames
  `DecodedSource::Bridge` (`live_bridge.rs:157`), the same tag as the pipe
  decoder, and in `Pipewire` mode both are accepted
  (`handler.rs:207-221`). A pipe writer active while the sink is in use
  interleaves two streams with no way to tell them apart. Counters must be
  per source.
- **The pipe reader is the decoder.** `read()` → parse → `push_packet` →
  blocking `send` all run on one thread (`decoder_thread.rs:171-483`). The
  reader therefore blocks on decode time, on a full main channel, and
  (through the handler) on the ring's back-pressure. The default Linux pipe
  buffer is 64 KiB, which is **21 ms** of 8-ch 192 kHz HBR transport
  (TrueHD, DTS-HD MA) and 340 ms of 2-ch 48 kHz (AC-3). For `follow` mode the
  plan requires that the reader never blocks, so the reader must become its
  own thread (§3.4).
- **The live capture thread is not realtime.** The stream is connected
  without `RT_PROCESS` (`audio_input/src/pipewire.rs:1160-1164`), so
  `process` runs on the capture thread's main loop
  (`mainloop.loop_().iterate`, `:1207-1235`). `build_live_input_frame`
  allocates per buffer (`live_input.rs:552-560`). Neither is a realtime
  violation today, but the capture timestamp is "main-loop wake time", not
  the graph cycle time. Use `pw_time.now` (already read in
  `refresh_pw_stream_driver_timing`, `:297-375`) for stamping.
- **`input_clock_us` is already an `N_in`.** The capture callback adds
  `byte_len / (channels · 2) / rate` to a cumulative counter before any
  parsing (`audio_input/src/pipewire.rs:883-896`). That is exactly the
  counting point the new design needs (§3.1).

### 1.2 Drop / insert / trim / variable-delay points

Legend for **Treatment**: **remove**: the new design makes it impossible.
**epoch**: it becomes an epoch boundary (counters reset, start rule).
**report**: kept, but counted as a discontinuity. **n/a**: conserves the
count (delay only).

#### A. Live PipeWire input, IEC958 (bitstream through the `omniphony` sink)

| # | Where | Condition | Frames affected | Today | Treatment |
|---|---|---|---|---|---|
| A1 | `audio_input/src/pipewire.rs:684-846` | No buffer, empty `datas`, no data pointer, zero-size or oversized chunk, `channels == 0` | The callback returns without consuming. An oversized chunk (`byte_len > bytes.len()`, `:816`) is **dropped** with its frames | silent | Count `N_in` only from consumed chunks. Oversized chunk: **report** (`CaptureMalformed`) |
| A2 | `:980-987` accumulate window (`PW_STREAM_ACCUMULATE_CALLBACKS = 4`, `:24`) | Always | None lost. Adds 0–3 callbacks of **variable** delay before parsing | — | **remove** (feed the deframer every callback; the parser already buffers partial bursts) |
| A3 | `:542-621` `param_changed` (2 ch ↔ 8 ch IEC, IEC ↔ PCM, codec carrier change) | Format renegotiation | `accumulate_buf` and the `SpdifParser` buffer are **not cleared**. Stale bytes from the old format are prepended to the new stream: a spurious burst can be decoded (insert) or a partial one lost | silent | **epoch** (clear the deframer, flush or reset the decoder, new carrier rate) |
| A4 | `spdif/src/parser.rs:72-89` | Bytes before a sync word (start, resync after corruption) | Dropped (keeps 3 bytes) | silent | Start: part of the epoch start. Mid-stream: **report** `ParserResync{bytes}` as transport time with no payload, filled with silence (§3.2) |
| A5 | `spdif/src/parser.rs:137-147` | Pause bursts (Pc data type 3), null data (0) and stuffing between bursts | Stuffing is skipped by `WaitingForSync`. Type 0/3 "packets" are handed to the bridge as data | silent | Pause/null: convert to silence of the stated duration (Pause burst `Pd` = gap length) at the deframer, **n/a** for the count |
| A6 | `audio_input/src/bridge.rs:27-33` | `raw_tx.try_send` with the 256-packet queue full (bridge thread slower than real time, or not yet started) | Whole burst: 960 fr (TrueHD MAT), 1536 (AC-3, E-AC-3), 512–4096 (DTS) | `warn!` only | **remove**: an SPSC byte ring capture → decoder sized for `L_max` + 1 burst. Overflow is a fault: **report** + realign |
| A7 | `live_bridge.rs:103-112` | `push_packet` returns `did_reset` or an error | Whatever the bridge dropped or concealed (§2). The reset is only logged; **no `BridgeReset` is posted** on this path (unlike `decoder_thread.rs:327-340`) | logged | Decoder audit per burst (§3.3): **report** `DecoderShortfall`/`DecoderReset` |
| A8 | `live_bridge.rs:173-188` | `tx.try_send` with the main channel full | One decoded frame (40 fr TrueHD, 1536 E-AC-3, …). Happens whenever the handler stalls (writer rebuild flush up to 5 s, stream-ready wait up to 3 s, back-pressure) | silent | **remove** (per-source SPSC frame ring, handler never blocks in `own`/`follow`). Ring overflow: **report** + realign |
| A9 | `live_input.rs:150-164`, `:271-274` | Live latency target changed (Studio) or capture shape changed | The capture thread is **restarted**: new parser, new bridge instance (priming, §2), old bridge's internal look-ahead lost (no flush API). The old `bridge-decode` thread keeps delivering its queued packets while the new one starts, so the two decoders' outputs can interleave | silent | **epoch**. Must not restart capture for a latency change at all (latency is an output-side setting in the new design) |
| A10 | `session_run.rs:511-523` standby | mpv took the OSC port | All queued frames drained (`try_recv` loop) and the writer dropped. Capture keeps counting | — | **epoch** |
| A11 | `audio_input/src/pipewire_client_node.rs:124-177` (`clock_mode: upstream`) | Backend selected for `upstream` | `ingest` is stored but **never called**: no data reaches the decoder | broken | **remove** (the backend goes with the rewrite) |

#### B. Live PipeWire input, linear PCM (sink negotiated `Raw`)

| # | Where | Condition | Frames affected | Today | Treatment |
|---|---|---|---|---|---|
| B1 | `live_input.rs:524-529` | Negotiated channels ≠ 8 | Whole buffer dropped, `warn!` per buffer | silent-ish | **epoch** with a refused format (fail negotiation instead) |
| B2 | `live_input.rs:531` | `tx.try_send` on a full main channel | One quantum (whatever PipeWire delivered) | silent | **remove** (same as A8) |
| B3 | `audio_input/src/pipewire.rs:930-952` | PCM heartbeat re-trigger (`FIXME(pcm-clock)`) | No loss, but delivery is measured at 0.005× then 7.7× real time: variable delay and overflow pressure upstream of B2 | — | Fixed by S1 (`own` driver clocked on M) |
| B4 | `handler.rs:207-221` + `should_accept_source` | Source not accepted for the active mode | Frame discarded (no idle-feed holdoff either) | silent | Per-source pipelines. A mode switch is an **epoch** |

#### C. Pipe input (`/tmp/orender.pipe`, mpv `--ao=pcm`, "pipe bridge" mode)

| # | Where | Condition | Frames affected | Today | Treatment |
|---|---|---|---|---|---|
| C1 | `sys/src/input.rs:470-482` + `drain_fd` `:318-368` | `drain_pipe` (default on, `!args.no_drain_pipe`) at every (re)open | All bytes buffered in the FIFO are discarded | logged | **epoch** start (count from the first byte kept) |
| C2 | `decoder_thread.rs:197-216` | First chunk: transport detection. Later: raw → IEC switch | Raw → IEC switch resets the parser (`spdif_parser.reset()`), losing buffered bytes | logged | **epoch** |
| C3 | `decoder_thread.rs:171-178` | SIGHUP reload / restart-from-config / shutdown **mid-chunk** | The remaining packets of the 64 KiB chunk are dropped, then the input is reopened (C1 drains again) | silent | **epoch** |
| C4 | `decoder_thread.rs:327-340` | `did_reset` | A `BridgeReset` is posted (spatial reset only, audio untouched). Frames dropped or concealed inside the bridge are invisible | silent | Decoder audit (§3.3) |
| C5 | `decoder_thread.rs:361-383` | Blocking `send` on a full main channel | No loss. Blocks the **reader**: variable delay, and back-pressure on mpv once the 64 KiB pipe buffer fills (21 ms at HBR) | `warn!` > 5 ms | **remove** for `follow` (reader thread with its own ring). Keep blocking for `none` |
| C6 | `decoder_thread.rs:493-520` | EOF in `continuous` mode | `StreamEnd`, then `bridge.reset()`: the decoder's held look-ahead is discarded (no flush/drain call in `bridge_api`). 100 ms sleep, then reopen (→ C1) | silent | **epoch**. Add a bridge flush (§2.4) so the tail plays |
| C7 | `session_run.rs:437-474` `handle_stream_end` | `StreamEnd` | `finalize()` waits for the ring to empty (≤ 5 s, stall 500 ms, then **discards the rest**, `ring_buffer_io.rs:88-132`). Then `*handler = DecodeHandler::default()` drops the writer, and `Drop` pops whatever is left (`audio_output/src/pipewire.rs:796-806`) | silent | **epoch** (end: drain to the device, then stop) |
| C8 | `session_run.rs:805-869` pacer-bridge-drain thread | `use_output_pacing` in Bridge mode | Drains `emitted_us × out_rate` frames per decoded packet (fractional carry). Pre-roll and underrun **insert zeros**. A flush discards the FIFO | diag atomics | **remove** (pacer deleted) |
| C9 | `session_run.rs:281-284` | Drain-token channel is unbounded | Memory grows if the drain thread stalls; tokens from the idle feed and both decoders mix | — | **remove** |

#### D. File input (and any faster-than-real-time writer: `none` mode)

Same chain as C, with blocking `send` and blocking back-pressure, so it is
count-conserving in steady state except for C2, C4, C6–C8 and the shared
points E1–E9 below. `--output-backend file` without `--continuous` skips live
input entirely (`session_run.rs:956-991`).

#### E. Shared handler → writer → ring (all paths)

| # | Where | Condition | Frames affected | Today | Treatment |
|---|---|---|---|---|---|
| E1 | `sample_write.rs:213` (`if self.output.audio_writer.is_some()`) | No writer: bootstrap gate, init failure, after invalidation and before rebuild | **Every decoded frame is dropped** | silent | **remove**: the ring exists before the first frame. Writer and device lifecycles are separate (§3.4) |
| E2 | `writer_lifecycle.rs:98-121` bootstrap gate | PipeWire backend with a renderer | First **8 frames** (channel content) or **3 frames** after `bed_indices` is known (objects) are dropped. For TrueHD that is 120–320 samples; for E-AC-3 it is 12–37 ms | silent | **remove** |
| E3 | `audio_output/src/pipewire.rs:435-444` | Writer construction waits ≤ 3 s for `stream_ready` | No loss here, but the handler blocks: live A8/B2 drops pile up | — | **remove** (no blocking on the frame path) |
| E4 | `audio_output/src/pipewire.rs:503-507` | `stream_ready == false` (stream not yet streaming, or PipeWire thread exited) | Whole write dropped | `trace!` | **remove**; the device state belongs to the output core |
| E5 | `ring_buffer_io.rs:16-55` `push_samples_with_backpressure` (10 ms sleeps, 200 waits) | Ring (or pacer FIFO) at the cap for > 2 s | Remainder of the write **dropped at sample granularity**. Can split a frame and **rotate the channel interleaving** for the rest of the stream | `warn!` | **remove** (frame-granular ring; `none`: condvar wait without timeout; `own`/`follow`: never block) |
| E6 | `ring_buffer_io.rs:62-80` `push_samples_drop_overflow` (`disable_backpressure`) | Ring above `max_buffer_fill` | Overflow dropped **per sample** (same rotation risk; the consumer also pops per sample, so the ring length is not a multiple of the channel count at arbitrary instants) | silent | **remove**; overflow is a fault: **report** + realign |
| E7 | `handler.rs:492-511`, `:634-651` (`is_new_segment`, output width change) and `output_runtime_sync.rs:94-237` (backend, device, rate, adaptive toggle, latency target) | Writer invalidated | `flush()` waits for the ring to drain (≤ 5 s / 500 ms stall), then discards the rest; the writer is dropped (pacer FIFO dropped too, `state.rs:270-280`); the next frames hit E1–E4 | silent | **epoch** for a format or device change. A **latency-target change** must become a realign, not a writer rebuild |
| E8 | `handler.rs:346-349` + `orender_engine/src/render.rs:58-71` `follow_stream_rate` | Decoded sample rate changes **without** `is_new_segment` | Renderer DSP rebuilt; the **writer keeps its old input rate** (no rebuild on rate change), so the ratio is wrong until the next invalidation | silent | **epoch** keyed on `(fs, channel layout, source)` |
| E9 | `sample_write.rs:307-330` (`BedPlanKind::Silence`), `:299-305` (host passthrough) | No render mapping for the labels / passthrough | Silence of the same length / decoded PCM: count conserved | — | **n/a** |

#### F. Fabricated input (idle feed)

| # | Where | Condition | Frames affected | Today | Treatment |
|---|---|---|---|---|---|
| F1 | `session_run.rs:584-642`, `idle_feed.rs:94-136` | Speaker/object test armed, no accepted real frame for 250 ms | **Inserts** 2-ch silence at the last stream rate, clocked on `Instant` (M), ≤ 250 ms per chunk (excess wall time dropped, `MAX_CHUNK`). Posts drain tokens (C8) | logged | A distinct **source** with its own epoch. Better: the output core plays silence with no source, and the renderer runs from the output side for tests (decide in Phase 4) |
| F2 | Same, real input resumes | Holdoff 250 ms | Fabricated frames already in the ring delay the real stream by up to 250 ms + ring | — | Source switch = **epoch** |

#### G. Output side, after the ring (deleted by the rewrite, listed for completeness)

These sit on the `N_play` side of the boundary. They are replaced by the
output core's realigner, which reports its own events.

| # | Where | Effect |
|---|---|---|
| G1 | `audio_input/src/pipewire.rs:897-919` + `pacer.rs:110-148` | Live pacer drain: drain count is `in_subframes · out_rate · out_channels / rate_hz` (integer truncation, not frame-aligned, no fractional carry). Pre-roll and underrun push zeros. `ring.push` failures are ignored (silent drop). Flush discards the FIFO |
| G2 | `audio_output/src/pipewire.rs:1531-1700` (resampled path) | Reacquire: `resampler.reset()` + `fifo.reset()` (`resampler_fifo.rs:66-72`) discard up to one 1024-frame input chunk plus the output FIFO and the filter state. Low-recover trim and muted consume: `discard_samples`. Hard-recover-high: discard plan. Muted output: zeros while consuming |
| G3 | `audio_output/src/pipewire.rs:1830-1990` (direct path) | Same via `discard_ring_samples` (`adaptive_runtime.rs:355-365`), counted in `recovery_discard_total`, the only counted drop in the tree |
| G4 | `audio_output/src/pipewire.rs:796-806` | `Drop` pops the whole ring |
| G5 | `audio_output/src/cpal_output.rs:854-875`, `:693-759` | Same back-pressure and drop-overflow as E5/E6, plus zero fills |

## 2. Per-codec frame relationship (decoder bridge)

`HB` = `workflows/integration/harletty-bridge/`. Measured offline with a
throwaway harness. It loads the prebuilt `libharletty_bridge.so` (30/09) the
way the host does, feeds IEC 61937 bursts through the host's own
`spdif::SpdifParser` and calls `push_packet`. Inputs were 5 s slices of the
`dumps/` samples, wrapped with ffmpeg's `-f spdif` encoder (DTS-HD at
`-dtshd_rate 768000`). The generated media were deleted afterwards.

| Codec (IEC type) | Burst period (carrier frames) | PCM per burst (measured) | Ratio fixed? | Constant hold in the bridge | Priming at start | EOF / flush | Batching | Errors (measured) |
|---|---|---|---|---|---|---|---|---|
| AC-3 (0x01) | 1536 @ 48 k × 2 | 1536 | 1:1 | **1 AU (1536)**: the core is parked in case an E-AC-3 dependent follows (`HB/bridge/src/bridge.rs:546-556`) | Only the hold: burst 1 → 0 frames | Last AU never emitted | 1 frame per burst | Failed core → 1536 of silence (`bridge.rs:557-580`); count kept |
| E-AC-3 (0x15), plain or with a non-JOC dependent | 6144 @ 192 k × 2 (short frames aggregated to 1536) | 1536 | 1536:6144 | **1 AU**: the independent frame is held until the next AU shows whether dependents follow (`bridge.rs:582-611`) | None | Last AU lost | 1 per burst | See the JOC row |
| E-AC-3 + JOC | 6144 @ 192 k | 1536 | yes | **0** (`bridge.rs:530-534, 612-618`). Objects lag the core by 577 samples inside the decoder; the core is delayed with zero padding and the count is kept (`HB/eac3/src/eac3dec/pcm.rs:13-29, 430-458`). A JOC ↔ non-JOC switch makes the bed jump 577 samples with no count change | None | none held | 1 per burst | Decode error: `did_reset` **and** `error_message` (even non-strict, `bridge.rs:643-650`). Measured: 1 frame lost. The embedded engine then discards the whole packet (`engine.rs:864-866`). A 2nd+ stand-in silence frame in one push is **dropped** (`bridge.rs:636-641`). Orphan dependents are dropped (`:515-524`) |
| TrueHD / MLP (0x16, MAT) | 61440 B = 3840 frames @ 192 k × 8 = 20 ms | **960 @ 48 k** (24 × 40; 1920 @ 96 k) | 960:3840 (fs/192 k); ±40 possible if an AU straddles bursts (not observed) | **0**: an AU is emitted once complete (`truehd_pipeline.rs:300-311`) | **AUs are dropped until the first major sync** (every ~128 AUs ≈ 107 ms here). A mid-stream join lost 3520 samples | none held | One 40-sample frame per AU, ~24 per IEC push | **Parse error: silent resync to the next major sync**, with no flag and no concealment (`truehd_pipeline.rs:108-133`). Measured: 1 corrupt burst lost 3160 samples; a dropped burst lost 960 + 2240. Decode errors drop the AU (`:218-225`). Duplicate AUs at seamless branches are discarded (`:298-305`). Bad MAT start → `did_reset` (`bridge.rs:756-772`) |
| DTS core (0x0B/0C/0D) | 512/1024/2048 @ 48 k × 2 | 512 | 1:1 | **1 frame**: waits for 4 more bytes to rule out an extension substream (`HB/bridge/src/dts_pipeline.rs:199-201`) | burst 1 → 0 | Last frame lost | 1 per burst | Decode error: **frame dropped silently** (`dts_pipeline.rs:313-324`) |
| DTS-HD HRA / MA / DTS:X / IMAX (0x11) | 4 × core samples @ 192 k × 8 (2048 for a 512 core) | 512 @ 48 k, 1024 @ 96 k | fs/192 k | 0 for HRA and DTS:X | **MA without X: Auro-detection hold** of up to 8192 samples with no valid block, 16384 once blocks validate (`HB/bridge/src/auro_pipeline.rs:26-31,196-237`). Measured: 8 bursts → 0, then 9216 samples at once, then 1:1 (a one-off backlog). **PBR/XLL priming: samples lost**, not delayed (`dts_pipeline.rs:261`, `HB/dca/src/hd.rs:259-277`) | an undecided Auro backlog is lost | 1 per burst | Corrupt burst: 1 frame lost, no flag (`dts_pipeline.rs:262-300`). Wrong-length frame dropped while the position advances (`:404-409`) |
| Auro-3D (unfolded from a DTS-HD MA carrier) | as DTS-HD | 512 steady | long-run 1:1 | **Constant lag = detected block size, 1000 samples on two titles** (`auro_pipeline.rs:291`, `HB/auro/src/unfold.rs:22-23,134-160`) + the decision hold above | none dropped | **Tail lost**: `Unfolder::finish()` is never called | variable (`ready()` per push) | Unclaimed samples play the carrier with silent heights |
| LPCM | n/a | — | 1:1 | 0 | 0 | — | — | PipeWire PCM bypasses the bridge (`audio_input/src/pipewire.rs:930`). On a raw pipe an unknown sync falls back to TrueHD (`bridge.rs:483-499`): harletty has no PCM path |

Consequences for the design:

- **Steady state is a fixed ratio for every codec.** Source frames =
  carrier frames / carrier rate × the frame's `sampling_frequency`. Account
  in time, or per `f_s` (96 kHz TrueHD and DTS-HD exist).
- **Constant holds** (count stays conserved, only an offset): AC-3 and plain
  E-AC-3 one AU (1536), DTS core one frame (512), Auro 1000 samples, TrueHD
  and DTS-HD HRA/X 0, plus partial-burst buffering in the deframer. They sit
  inside `N_in − N_play`, as intended.
- **Transient holds:** DTS-HD MA without X releases up to 16384 samples
  (≈ 340 ms) in a single push after start. The ring, and `L` at startup, must
  absorb it, or the start rule must wait until the backlog is released.
- **Silent losses, with no flag:** TrueHD resync to the next major sync (up
  to ~107 ms per error, and at start and after any reset), DTS frame drops,
  DTS PBR priming, and the duplicate stand-in-silence filter. These are
  exactly the cases the §3.3 audit exists for. Only E-AC-3 errors raise
  `did_reset`.
- **TrueHD batch = one MAT frame (960 frames ≈ 20 ms), not 320 ms.** The
  3.1 Hz / ~320 ms sawtooth seen in the latency trace is the beat between
  960-frame bursts and the 1024-frame resampler chunk
  (`docs/LATENCY_DAC_SAWTOOTH_REPORT.md`). Cumulative counters do not show it.
- **No flush:** every hold above is lost at EOF and on `reset()`.
- **bridge_api today** (`bridge_api/src/lib.rs`): `push_packet → RPushResult
  {frames, error_message, did_reset}` (226-236, 293-298); `RDecodedFrame
  {sampling_frequency, sample_count, channel_count, pcm, is_new_segment}`
  (201-220); `is_new_segment` is set only by TrueHD on a substream-info
  change. `RMetadataFrame.sample_pos` (186) exists only on metadata frames.
  It is per codec family, is not reset by `reset()` (`bridge.rs:851-856`),
  and runs ahead of the emitted PCM on drops, so it is unusable as a counter.
  There is no flush, no latency query, no consumed count and no PTS. New
  methods can be added compatibly after the 0.4 prefix with default bodies
  (`lib.rs:356-392`).
- **The host parser discards most of the burst header.** It keeps only Pc
  bits 0-4 (`spdif/src/parser.rs:96`), dropping the error flag and the
  DTS type IV subtype (which gives the repetition period). Pause (0x03) and
  null bursts reach the bridge, which only logs them (`bridge.rs:837-847`).
  §3.2 needs the parser to keep Pc and Pd.

## 3. Counting points and accounting API

### 3.1 Where `N_in` is counted

Count in **transport time** at the capture point, in units that convert
exactly to source PCM frames:

| Source | Counting point | Unit | Conversion to source frames |
|---|---|---|---|
| `own`, IEC958 | PipeWire `process`, right after `dequeue_buffer`, before any parsing (where `input_clock_us_cumulative` is today, `audio_input/src/pipewire.rs:887-896`) | carrier frames `byte_len / (channels · 2)` at carrier rate `R_c` (48 k for AC-3 and DTS core, 192 k for E-AC-3, TrueHD and DTS-HD) | `× f_s / R_c`, exact rational (4 for TrueHD and E-AC-3 at 48 kHz, 2 for 96 kHz content, 1 for AC-3 and DTS core). `f_s` is known after the first decoded burst: keep the raw carrier count and convert at read time |
| `own`, PCM | Same callback | frames at the negotiated rate | 1:1 |
| `follow`, pipe, IEC 61937 | **Reader thread**, after each `read()`, before parsing (stamped `CLOCK_MONOTONIC`) | bytes | `bytes / B`, where `B = burst_spacing_bytes / nominal_pcm_frames(burst)` is learned once per epoch from two consecutive preambles. Integer for every type: AC-3 6144 B / 1536 = 4; E-AC-3 24576 / 1536 = 16; TrueHD MAT 61440 / 960 = 64; DTS-HD at 192 k 8 ch = 64. The pipe carries no format metadata, so `B` cannot come from anywhere else |
| `follow`, pipe, raw elementary stream | Reader thread | parsed codec frames (VBR bytes) | Σ nominal frame lengths from the codec frame headers. Coarse timing (one codec frame). Raw pipe input is in practice a `none` source |
| `none` (file, `cat`) | Decoder output | decoded frames | Back-pressure paces the producer; arrival time carries no clock information, and the servo holds the ratio nominal |

The count must start at the epoch start, after `drain_pipe` (C1) and after
the deframer has locked (A4/C2).

### 3.2 Make ingest time-conserving

With `N_in` in transport time, the decoded stream must cover the same time
span, minus a constant decoder delay. The ingest stage (deframer) therefore:

1. passes data bursts to the decoder and records each burst's **nominal PCM
   length** (from data type and header: §2);
2. turns everything that is not a data burst into **explicit silence of the
   same duration**: IEC pause bursts (their `Pd` states the gap), null data,
   stuffing beyond the expected repetition period, and garbage between
   preambles after a sync loss. This is not a discontinuity: source time
   passed and the output plays it as silence;
3. on a format change (A3, carrier rate, data type family), ends the epoch.

A source that stops sending entirely (mpv paused with the sink in `own`
mode) gives no transport frames at all. `N_in` stops, the ring drains, and
the result is an underrun: realign, as the plan already says.

### 3.3 Decoder audit (detect bridge drops without bridge cooperation)

Keep a per-epoch backlog `B_dec = Σ nominal(burst) − Σ emitted frames`. In
steady state it oscillates in `[B_min, B_min + batch]` (TrueHD: one MAT frame
= 960, plus the ±1 AU jitter seen in the measurements; E-AC-3: 0). A decoder
that skips a frame (CRC error, missing dependent substream, reset) shifts
`B_dec` up permanently by that frame's length. A decoder that conceals emits
the nominal length and does not move it. Detection rule: track `min(B_dec)`
over a window of a few bursts. A step outside `[B_min, B_min + batch]` emits
`DecoderShortfall{frames}` (or `DecoderSurplus`). `did_reset` emits
`DecoderReset` with the measured step after the next window. This works with
today's `bridge_api`. A bridge-side count (§3.6) would make it exact and
immediate.

### 3.4 Where `N_play` is counted, and the constant terms

- `N_play` = the resampler's exact fractional input position (frames popped
  from the ring into the resampler, minus its pending input, minus its group
  delay), per the plan. It is in source frames at `f_s`, the same unit as
  `N_in`. Output-side mutes or fades that consume frames while playing zeros
  conserve the count.
- **Renderer DSP latency does not show in the count.** `render_frame` is
  count-preserving (N in → N out) with an internal delay line, so the content
  of ring frame *i* is input frame *i − d*. The acoustic latency is therefore

  ```
  L_acoustic = (N_in_eff − N_play)/f_s + D_dev + (d_dsp + d_gen)/f_s
  ```

  - `d_dsp = SpatialRenderer::output_latency_samples()`
    (`renderer/src/spatial_renderer/mod.rs:1302`): FIR crossover
    `kernel_delay + block − 1` (`renderer/src/crossover/fir.rs:232`), BRIR
    stage `BRIR_BLOCK − 1` (`renderer/src/binaural/brir_stage.rs:354`).
  - `d_gen` is **not reported today**. The spectral phantom extraction
    (`orender_engine/src/phantom_extract.rs:20`, one 1024-sample FFT frame)
    and the DirAC object generator (`orender_engine/src/object_gen.rs:1135`,
    FIFO primed with one FFT frame of zeros) add a fixed latency on the
    rendered path that `output_latency_samples()` does not include. Sink
    latency advertisement (`sample_write.rs:524-529`) misses it too.
  - The servo regulates `e = L_e2e − (L − (d_dsp + d_gen)/f_s)`. When `d`
    changes live (crossover engine switch, binaural mode toggle, phantom
    method change), emit `LatencyStep{delta = Δd}`. The realigner then drops
    (Δd > 0) or inserts (Δd < 0) Δd frames, with fades, so `L` holds. `d` is
    already cached per rendered frame (`last_output_latency`), so the step is
    exact and attached to a frame position.
- **Decoder look-ahead is inside the count.** Frames the bridge holds back
  are counted in `N_in` and not yet in `N_play`, so they are part of the
  measured `L_e2e`. That is correct (they delay the sound) and constant per
  codec (§2). `L` must exceed `max(B_dec) + ring jitter + quantum + margin`.

### 3.5 Accounting API

Pure `sync` core types (no I/O), used by every stage between the two counters:

```rust
/// Bumps on a change of (source, f_s, channel layout, carrier) and on every
/// start/stop. Counters and the ring position restart at 0; the realigner
/// applies the start rule.
pub struct Epoch(pub u32);

/// One per stage (deframer, decoder audit, frame ring producer, writer,
/// realigner), owned by the stage's single thread. Two monotonic counters
/// in source frames, updated once per event (never per sample). Read by the
/// output core with Relaxed loads, together with `N_in`, under the epoch
/// check.
pub struct StageCounters {
    pub epoch: AtomicU32,
    pub inserted: AtomicU64,
    pub dropped: AtomicU64,
}

pub enum DiscontinuityReason {
    // Faults, reported:
    CaptureOverflow, ParserResync, DecoderShortfall, DecoderSurplus,
    DecoderReset, RingOverflow, RingUnderrun,
    // Servo actions, reported by the realigner itself:
    StartAlign, Realign, LatencyStep,
    // Informational, count-conserving:
    PauseFill,
}

pub struct DiscontinuityEvent {
    pub epoch: u32,
    pub stage: StageId,
    /// Source-frame position at the stage's input where it happened.
    pub position: u64,
    /// +inserted / −dropped, in source frames (whole frames only).
    pub delta_frames: i64,
    pub reason: DiscontinuityReason,
}
```

- Servo input: `N_in_eff = N_in + Σ_stages (inserted − dropped)`, read at the
  output callback with the same epoch. A mismatched epoch means "between
  epochs": output silence and wait for the start rule.
- Events go to a bounded SPSC queue per stage, drained by a non-realtime
  thread for logs, OSC and the Studio plot (realign count, underruns). A full
  event queue loses events, never counter updates, because the counters are
  the source of truth.
- Invariant checks (tests and the simulator): per stage,
  `out = in + inserted − dropped` at every event boundary. Only whole frames
  may be dropped or inserted, which rules out E5/E6-style channel rotation by
  construction.

### 3.6 Optional bridge_api extension

Additive and versioned. Not needed for correctness thanks to §3.3, but it
makes the audit exact:

- `RPushResult.consumed_nominal_frames` (the PCM length the packet stands
  for), `dropped_frames`, `concealed_frames`;
- `FormatBridge::decoder_delay_frames()` (constant look-ahead) and
  `FormatBridge::flush() -> RPushResult` (emit the held tail at EOF or before
  a reset, fixing C6);
- `RDecodedFrame.is_new_segment` stays the epoch trigger.

### 3.7 Remove vs report, by drop point

| Make impossible (by design) | Becomes an epoch boundary | Report as discontinuity |
|---|---|---|
| A2 accumulate window; A6 packet `try_send` (→ SPSC byte ring); A8/B2 frame `try_send` (→ per-source SPSC frame ring); A11 client-node stub; C5 reader blocked by decode (→ reader thread, `follow`); C8/C9/G1 pacer; E1–E4 writer-absent drops, bootstrap gate, stream-ready wait; E5/E6 sample-granular partial pushes and timeouts; G2–G4 recovery trims and discards (→ realigner); shared multi-producer channel | A3 format renegotiation; A9 capture restart (and stop restarting on latency change); A10/standby; B1/B4 refused format or mode switch; C1 drain on (re)open; C2 transport switch; C3 reload; C6/C7 EOF and stream end; E7 segment, width or device change; E8 rate change; F1/F2 idle-feed source switch | A1 malformed chunk; A4 mid-stream resync; A7/C4 decoder shortfall/reset (audit); ring overflow (`own`/`follow` only; it replaces A6/A8/B2/E6); ring underrun; realigner start, realign and `LatencyStep` |

Queue sizing for the "impossible" ones: capacity ≥ `L_max` plus one
decoder batch plus one capture quantum, in frames, allocated once per
epoch. In `own`/`follow` a full ring is a fault (§2.1 of the plan), never
a pacing mechanism. In `none`, the producer waits on a condvar signalled by
the output core, with no timeout other than shutdown.

## 4. Open risks

1. **Silent bridge losses** (§2): TrueHD resync drops up to ~107 ms with no
   flag, and DTS drops frames with no flag. The §3.3 audit must catch them,
   or the bridge must gain a `dropped_frames` field (§3.6). The audit
   assumes nominal burst lengths are knowable: TrueHD 960/1920 per MAT
   frame, DTS from the core header, E-AC-3 1536.
2. **Plan correction:** §2.2 and §5 of the plan quote TrueHD bursts of
   ~320 ms. Use 20 ms MAT-frame batches plus ±1 AU jitter. The minimum `L`
   for TrueHD is driven by one MAT frame, the ingest window (A2, removed) and
   the capture quantum, not by 320 ms. The worst steady-state hold is
   E-AC-3/AC-3 (1 AU = 32 ms, plus one burst of transport). The worst
   transient is the DTS-HD MA Auro-detection backlog (≤ 16384 samples at
   start).
3. **Pipe carrier is unknown.** `B` (bytes per PCM frame) must be learned
   from burst spacing, which needs two preambles before the first `N_in`
   sample. mpv `--ao=pcm` may also prepend a WAV header (`--ao-pcm-waveheader`),
   which the deframer must skip as part of the epoch start.
4. **Silent pause semantics.** If a source stops sending with the sink in
   `own` mode, there are no transport frames, so an underrun follows. If it
   sends zeros or pause bursts instead, §3.2 turns them into silence and `L`
   holds. Both must be in the simulator.
5. **Unreported DSP latency** (`d_gen`: spectral phantom extraction and the
   DirAC generator, 1024 samples each) breaks `L` by 21 ms when those modes
   are toggled. Expose it through `output_latency_samples()` or a sibling
   accessor.
6. **Same-tag sources** (live bridge frames tagged `Bridge`, A/B4) can
   interleave two streams today. Per-source pipelines are a prerequisite for
   per-source counters.
7. **No bridge flush:** EOF and reset lose the decoder tail (C6). It is
   harmless for accounting once these are epochs, but audible at the end of
   files.
8. **E-AC-3 JOC ↔ non-JOC switches** shift the bed by 577 samples with no
   count change: an audible step that accounting cannot see. Treat it as a
   `LatencyStep` if the bridge ever reports it.
9. Nothing here was measured live through PipeWire. The bridge figures come
   from offline pushes of ffmpeg-wrapped IEC streams; the renderer citations
   are from code at `8450cbff`.

