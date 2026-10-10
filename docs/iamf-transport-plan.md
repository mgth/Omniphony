# IAMF beyond the in-process path: raw OBU transport and an IEC 61937 encapsulation

Status: **proposal**, nothing built. Written 2026-10-07, clocking section
added the same day.

## Problem

IAMF reaches the engine one way only: in-process. mpv's `ad_orender`
demuxes an `A_IAMF` track, hands the descriptor OBUs from `CodecPrivate` to
liborender, then one temporal unit per Matroska block (see
[iamf-matroska-mapping.md](iamf-matroska-mapping.md)). The decoder is the
bridge's IAMF family, on the `Raw` transport.

TrueHD, E-AC-3 and DTS have a second life as bitstreams: IEC 61937 bursts
over S/PDIF, HDMI, a PipeWire IEC958 sink, or the named pipe orender reads in
`--continuous` mode. IAMF has none of this. The bridge refuses it on the IEC
61937 transport on purpose (`bridge/src/bridge.rs`, "IAMF has no IEC 61937
data type: it only comes raw"), and no sender exists outside `ad_orender`.

The target topology is a player machine (mpv, video) and an audio machine
(orender, DAC), today joined by a netjack2 link carrying rendered PCM. The
goal is to move the renderer to the audio machine and carry the IAMF
bitstream instead of PCM, **with a clock at least as good as today's**: one
master clock (the audio machine's DAC), no drift, A/V sync unchanged in mpv.
That requirement, not the byte format, decides the transport.

Two transports:

- **Transport A, raw OBU stream.** The IAMF standalone bitstream (descriptors,
  then temporal units) on `RInputTransport::Raw`, over any byte pipe. Nothing
  is invented, but the stream has no clock of its own: it needs a
  timestamped framing and a return channel to meet the requirement.
- **Transport B, IEC 61937 encapsulation.** A private data type on a carrier
  shaped like PCM. The carrier *is* a clock: a burst's position in the PCM
  stream is its time, and a synchronous link (netjack2, S/PDIF, HDMI) carries
  the clock with the bytes. Never recognised by third-party equipment.

IAMF has no IEC 61937 data type assigned by the IEC as of this writing (MPEG-H
and AC-4 got parts 13 and 14; nothing for IAMF). Transport B is a private
extension and must be documented as such.

## Inventory: what exists today

| Piece | Where | State |
|---|---|---|
| IEC 61937 parser | `omniphony-renderer/spdif/src/parser.rs` | Codec-agnostic. The data type only decides whether `Pd` counts bits (AC-3, DTS core) or bytes (everything else). A new type needs no parser change. Bursts carry an absolute `start_byte` for the transport timeline. |
| Transport timeline | `omniphony-renderer/src/cli/sync_host/`, [resampling-rework-plan.md](resampling-rework-plan.md) | The sync host counts a burst stream's input time in carrier bytes from the first burst; the carrier byte rate comes from the data type. Raw streams keep the decoded-frame count. |
| Transport ABI | `omniphony-renderer/bridge_api/src/lib.rs` | `RInputTransport { Raw, Iec61937 }` plus `data_type: u8` on every `push_packet`. Adding a data type is not an ABI change. |
| Bridge dispatch | `harletty-bridge/bridge/src/bridge.rs` | `Raw`: codec sniffed on the first chunk (`sniff_raw_codec`: IAMF sequence header at offset 0, TrueHD major sync, DTS, E-AC-3), or forced by `configure("input_codec", …)`, or continued while `iamf.has_sequence()`. Falls back to TrueHD when nothing matches. `Iec61937`: Dolby and DTS by data type, IAMF refused. |
| IAMF family | `harletty-bridge/bridge-family-iamf/src/lib.rs` | Frames OBUs itself (any chunking), collects descriptors up to the first temporal unit, ignores redundant descriptor copies once configured, keeps descriptors across a reset so decoding resumes without a new sequence header. A non-redundant sequence header starts a new sequence. |
| Pipe input | `omniphony-renderer/src/cli/decode/decoder_thread.rs` | Per chunk: an IEC 61937 sync word anywhere switches to the parser, otherwise the chunk goes as `Raw`. Path from `render.input_pipe`. |
| PipeWire input sink | `omniphony-renderer/audio_input/src/pipewire.rs`, `pipewire_pods.rs` | Advertises IEC958 formats (`iec958.codecs` = TrueHD, E-AC-3, AC-3, DTS, DTS-HD; 2 or 8 channels) **and** a float PCM alternative. Rate and channel count are re-read from the negotiated format. |
| Engine config | `omniphony-renderer/orender_engine/src/engine.rs` | `input_codec` forwarded to the bridge as the forced raw codec. |
| mpv sender | `mpv/audio/decode/ad_orender.c`, `ad_spdif.c`, `ao_pcm.c` | In-process only: descriptors from `CodecPrivate` (IAConfigurationBox payload), re-sent after every engine reset; one temporal unit per `process()`. `ad_spdif` wraps AAC, AC-3, DTS, E-AC-3, MP3 and TrueHD through libavformat's `spdif` muxer; spdif-flagged frames bypass the filter chain. `ao_pcm` has a timed mode whose reported delay is `--ao-pcm-latency`. |
| Link between the machines | PipeWire `netjack2-driver` on the player, `netjack2-manager` on the audio machine | 16 float channels, uncompressed. The manager's cycle clocks the player's graph: every stream into `nj2-sink` runs on the audio machine's clock. |

## Clocking

What "a correct clock" means here:

1. The audio machine's DAC is the only master. Everything upstream follows it,
   or is disciplined to it with a bounded buffer.
2. mpv's A/V sync keeps working as it does with a local device: its audio
   output reports a position that is the DAC's position, less a known delay.
3. No drift to correct between the renderer's input and its output.

Three ways to get there, from best to worst.

### Option 1: the carrier clock over the synchronous link (recommended)

Transport B inside the existing netjack2 link. The IEC 61937 bursts travel as
16-bit words inside PCM channels of `nj2-sink`; the manager on the audio
machine hands those channels to orender's PipeWire input sink.

What this buys:

- One clock domain end to end. netjack2 already slaves the player's PipeWire
  graph to the audio machine's cycle, so mpv's audio output position *is* the
  DAC clock, as it is today for rendered PCM. No resampling anywhere.
- orender's input sink and its output stream sit on the same graph on the
  audio machine. The latency controller has nothing to track; latency stays
  at its setpoint ([latency-regulation.md](latency-regulation.md)).
- Bursts are dated by `start_byte` on the carrier, which is exactly the
  transport time the sync host already counts for IEC 61937 input.
- Nothing new on the network: no protocol, no return channel, no clock sync
  daemon.

What it demands:

- **A bit-exact path.** The carrier is 16-bit words carried as float samples.
  Exact as long as nothing touches them: no dither on the float-to-S16
  conversion, no channel mixing, no resampling (the link forbids it already),
  no other source summed on the carrier channels, no software volume in mpv
  (a spdif-flagged format bypasses mpv's filter chain, see sender work). A
  null test over the real link is the gate.
- **Dedicated channels.** Today the 16 channels carry rendered PCM and are
  shared with other sources on FL to RR. The carrier needs its own: either
  split the link (8 PCM for the rest of the desktop, 8 carrier for the
  bitstream) or widen it to 32 channels. The carrier byte rate is channels
  × rate × 2:

  | Carrier channels at 48 kHz | Byte rate | Fits |
  |---|---|---|
  | 2 | 1.5 Mbit/s | Opus, AAC |
  | 8 | 6.1 Mbit/s | wide Opus, AAC, most FLAC |
  | 16 | 12.3 Mbit/s | FLAC |
  | 24 | 18.4 Mbit/s | PCM 24-bit, 14 channels |

  A unit must fit its repetition period on the chosen carrier (period bytes
  = unit samples × channels × 2 at the carrier rate, minus the 8-byte
  header); a larger unit needs more channels.
- **Lip sync.** mpv sees the link latency through its PipeWire output. It
  does not see orender's decode, render and output latency on the audio
  machine, which is a setpoint and therefore stable: a fixed `--audio-delay`
  set once, later replaced by the engine's `measured_latency_ms` telemetry
  over OSC applied by the Lua script.

### Option 2: a timestamped raw stream with a return channel

Transport A, with a clock bolted on. Each unit gets a header: magic, length,
media time in samples, sender monotonic time. The receiver's sync-play
resampler disciplines the DAC-domain output to the sender's rate, as it does
for a pipe fed by a timed writer. The receiver reports the DAC position back
(OSC telemetry or the same socket), and mpv's sender reports it as the audio
output delay so video follows the remote DAC.

This is an RTP-like protocol plus a network audio output in mpv. It needs
either a clock sync between the machines (chrony, PTP) for absolute lip sync
or a measured round trip, and it keeps two clock domains with a resampler
between them. Correct, not better than today. Worth building only for a link
that has no synchronous carrier: Wi-Fi, a remote machine outside the
netjack2 graph.

### Option 3: an untimed raw stream paced by backpressure

The named pipe fed by an untimed writer. No clock at all: the writer runs as
fast as the pipe accepts, video free-runs, and a TCP hop turns the pipe into
an unbounded buffer. Rejected; kept here so nobody rebuilds it.

## Transport B: IEC 61937 encapsulation (private)

### Burst format

Pa and Pb as IEC 61937-1 defines them. Pc:

| Bits | Field | Value |
|---|---|---|
| 0-4 | data type | `IEC61937_IAMF_PRIVATE`, proposed `0x1D`; see the open question below |
| 5-6 | subdata type | 0 = temporal unit burst. 1 reserved for a descriptors-only burst if inline copies prove too costly; the first version uses 0 only |
| 7 | error flag | 0 |
| 8-12 | data-type dependent | 0. The carrier is known to the receiver from the negotiated format (PipeWire) or the device (S/PDIF); it is not encoded in the burst |
| 13-15 | bitstream number | 0 |

Pd is the payload length in bytes, as E-AC-3 and MAT do. The payload is the
OBUs of exactly one temporal unit, preceded by redundant descriptor copies
(`obu_redundant_copy = 1`, OBU header bit 2) at a fixed interval, one second
proposed, and right after a seek or a reconnect. A new sequence (track
change, codec config change) is a non-redundant sequence header followed by
fresh descriptors; the family rebuilds its decoder on it. No padding inside
the payload: a zero byte parses as a codec config OBU header. Zero padding
sits after the payload up to the repetition period; the parser reads `Pd`
bytes and resyncs on the next preamble, so the padding is dropped without a
rule.

The repetition period is one temporal unit, in carrier frames. It is
constant per stream because IAMF fixes the frame size per codec config. On a
48 kHz carrier a 20 ms Opus unit is 960 frames; on the classic 2-channel
192 kHz and HBR carriers it is 3840. Bursts start on a carrier frame
boundary.

### Receiver work

1. `spdif`: a named constant for the data type and a round-trip test. The
   byte-count default already applies.
2. Bridge: in the `Iec61937` arm, route the data type to `push_iamf`, with
   the mid-stream lock described under transport A. Keep the "unsupported
   data type" counter for everything else. No ABI change.
3. PipeWire input sink: a **PCM-carrier mode**. The sink already offers a
   float PCM alternative; in this mode the float samples are converted back
   to 16-bit words and fed to the parser instead of being treated as audio.
   The node properties must pin dither off and channel mixing off. The
   carrier byte rate for the transport timeline is the negotiated channels
   × rate × 2, passed to the sync host instead of being derived from the
   data type.
4. Sync host: accept a carrier byte rate stated by the input instead of
   looked up from the data type (one more arm, the IAMF data type).
5. ALSA S/PDIF devices and the FIFO need nothing beyond the bridge arm.

### Sender work (mpv fork)

- A `forward` host decoder in `ad_orender`
  (`--ad-orender-host-decoder=forward`) that frames the bursts itself:
  descriptors from `CodecPrivate` as redundant copies on the schedule above,
  one temporal unit per burst, zero padding to the period, carrier geometry
  from an option (`--ad-orender-forward-channels`, rate = the graph rate).
  The frames come out spdif-flagged (a new `AF_FORMAT_S_IAMF`), so mpv's
  filter chain never touches them, and their pseudo-sample count is real time
  on the carrier, so mpv's A/V sync works unchanged.
- `ao_pipewire`: a fork option to present a spdif-flagged stream as plain S16
  PCM with N channels, so a PCM sink (`nj2-sink`) accepts it; today an
  spdif format is mapped to an IEC958 codec and PipeWire refuses to route it
  to a PCM sink.
- Flush on pause, stop and seek; a seek into another sequence writes a
  non-redundant header.
- Not recommended: a libavformat `spdifenc` patch. FFmpeg has no codec id
  for IAMF (it is a container there, elements are Opus, AAC, FLAC or PCM), so
  the muxer would need a private codec id through the whole chain.
- For tests: a Rust framer in the orender CLI (`orender wrap-iec61937`) that
  turns a raw OBU file into a burst stream on a given carrier, used by the
  null test and the parser round-trip.

### Where transport B does not work

- HDMI through a TV or an AV receiver: EDID has no IAMF descriptor, so
  Windows, macOS and Android TV never offer the passthrough, and an eARC hop
  mutes an unknown type. Only a direct link into a capture input or an S/PDIF
  input of the Omniphony machine, from a sender we control.
- Any third-party decoder.

## Transport A: raw OBU stream

Kept for the links that have no synchronous carrier, and for the test
harness (feeding a raw file through the FIFO costs nothing).

### Stream definition

The stream is an IA sequence as IAMF §3 defines it: sequence header and
descriptors, then temporal units, with the same redundant-copy schedule and
new-sequence rule as the burst payload above. Temporal delimiter OBUs are
optional; the family frames OBUs on its own. Chunk boundaries are arbitrary.

### Receiver work (serves both transports)

1. **Mid-stream lock.** Today a raw stream is sniffed on the first chunk only,
   at offset 0. A receiver that joins mid-unit, or resumes after a reset
   before the next copy, must not fall through to the TrueHD default. Change
   `resolve_raw_codec`: when `input_codec = iamf` is forced, or IAMF was seen
   in this session, scan the chunk for a sequence header OBU and drop bytes up
   to it. Bound the scan to the chunk.
2. **Forced codec surface.** Confirm `render.input_codec` is exposed by the
   options registry (CLI flag, config key, Studio) and document `iamf` as a
   value. The bridge side already exists.
3. **Tests.** Bridge: join mid-stream and lock on the next redundant copy;
   seek signalled by a new sequence; reset then continuation without
   descriptors. Host: the `host_parity` null test (PR #705) extended with a
   FIFO-fed raw IAMF run against the in-process decode, byte-identical.

### Sender work

Only with the timestamped framing of clocking option 2; the untimed variant
is rejected. The same `forward` decoder gains a `raw` flavour: header per
unit, local silence of the unit's duration to keep mpv's clock, position
reports from the receiver turned into the reported output delay.

## Acceptance

- Null tests: in-process, raw over the FIFO, IEC 61937 over the FIFO, and
  IEC 61937 over the real netjack2 link decode to identical PCM on an anonymous
  IAMF fixture. The last one is the bit-exactness gate for option 1.
- Clock: over the link, orender's latency stays at its setpoint for an hour
  with no resampler activity, and mpv reports a stable A/V offset.
- Resync: join mid-stream, seek, reconnect, track switch, each locking within
  one descriptor interval.
- Steady state: no allocation per burst beyond the parser's existing buffers;
  the family already frames OBUs without copying units.
- Listening: the user listens live on the remote scenario before any merge.

## Order of work

1. Bridge: IAMF arm on the `Iec61937` transport, mid-stream lock, forced
   codec, tests. Bridge PR, no ABI change. Serves every transport.
2. orender: PCM-carrier mode of the PipeWire input sink, stated carrier byte
   rate in the sync host, data-type constant in `spdif`, `wrap-iec61937`
   test framer, FIFO null tests.
3. mpv fork: `forward` decoder with the IEC 61937 framer, `AF_FORMAT_S_IAMF`,
   `ao_pipewire` PCM presentation option. Fork PR plus both patch sets.
4. Audio machine: orender installed there with an `iamf` bridge, local config
   and layout; netjack2 channel plan (split or widen); link null test; fixed
   `--audio-delay`; listening.
5. Transport A with timestamps and a return channel: only if a link without a
   synchronous carrier becomes a target.

Branch names: `feat/iec61937-iamf` (bridge, orender), `feat/iamf-forward`
(mpv fork). No brand names.

## Open questions

- The data type value. `0x17` to `0x1F` are reserved in the FFmpeg table but
  the IEC has assigned some of them since (parts 13 and 14). Check IEC
  61937-1 edition 3 table 2 before freezing the constant; keep it in one
  place so it can move.
- Bit-exactness of 16-bit words through float channels on the real link:
  expected exact (symmetric scale, no dither), proven only by the link null
  test.
- The channel plan on the link: split 8 + 8, or widen to 32. Decided by the
  largest unit the user plays (FLAC beds need more than 8).
- Lip-sync delay: fixed first, telemetry later; whether the input sink should
  also publish its latency into the audio machine's graph.
- Descriptor interval: one second, or every N units; cost is a few hundred
  bytes per copy either way.
