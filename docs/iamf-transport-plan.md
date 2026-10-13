# IAMF beyond the in-process path: raw OBU transport and an IEC 61937 encapsulation

Status: **built; the audio machine is what is left**. Written 2026-10-07,
implemented 2026-10-12 and 13. Built: the burst format (`spdif::burst`,
`spdif::iamf`), the test framer (`orender wrap-iec61937`), the bridge's
decode of the burst type (harletty-bridge, `bridge-family-iamf::iec61937`),
the sender in the mpv fork (`--ad=iamf-iec61937`, on both tracks and in both
patch sets of mpv-omniphony), and the receiver's carrier input (the sink's
linear PCM, `sync_host::carrier`). Not done: everything that needs the audio
machine and the real link (step 5 of "Order of work", with what to do there).

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

## Inventory: what existed when this was written

As of 2026-10-07, before any of this was built, except for the two rows the
resampling cutover (#830) replaced since, which say what is there now.

| Piece | Where | State |
|---|---|---|
| IEC 61937 parser | `omniphony-renderer/spdif/src/parser.rs` | Codec-agnostic. The data type only decides whether `Pd` counts bits (AC-3, DTS core) or bytes (everything else). A new type needs no parser change. Bursts carry an absolute `start_byte` for the transport timeline. |
| Transport timeline | `omniphony-renderer/src/cli/sync_host/`, [resampling-rework-plan.md](resampling-rework-plan.md) | The sync host counts a burst stream's input time in carrier bytes from the first burst; the carrier byte rate comes from the data type. Raw streams keep the decoded-frame count. |
| Transport ABI | `omniphony-renderer/bridge_api/src/lib.rs` | `RInputTransport { Raw, Iec61937 }` plus `data_type: u8` on every `push_packet`. Adding a data type is not an ABI change. |
| Bridge dispatch | `harletty-bridge/bridge/src/bridge.rs` | `Raw`: codec sniffed on the first chunk (`sniff_raw_codec`: IAMF sequence header at offset 0, TrueHD major sync, DTS, E-AC-3), or forced by `configure("input_codec", …)`, or continued while `iamf.has_sequence()`. Falls back to TrueHD when nothing matches. `Iec61937`: Dolby and DTS by data type, IAMF refused. |
| IAMF family | `harletty-bridge/bridge-family-iamf/src/lib.rs` | Frames OBUs itself (any chunking), collects descriptors up to the first temporal unit, ignores redundant descriptor copies once configured, keeps descriptors across a reset so decoding resumes without a new sequence header. A non-redundant sequence header starts a new sequence. |
| Pipe input | `omniphony-renderer/src/cli/sync_host/reader.rs`, `mod.rs` (`feed_chunk`) | Per chunk: an IEC 61937 sync word anywhere switches to the parser, otherwise the chunk goes as `Raw`. Path from `render.input_pipe`. |
| PipeWire input sink | `omniphony-renderer/src/cli/sync_host/own_sink.rs`, `audio_input/src/pipewire_pods.rs` | Advertises IEC958 formats (`iec958.codecs` = TrueHD, E-AC-3, AC-3, DTS, DTS-HD; 2 or 8 channels) **and** a float PCM alternative (8 channels, 48 kHz). It runs on orender's own timer driver, in a node group of its own ([resampling-rework-plan.md](resampling-rework-plan.md) §6.1). |
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
- orender's input sink and its output stream sit on the same clock on the
  audio machine. The sink has a timer driver of its own for the players that
  write into it; ports of another node linked into it bring that node's
  driver with them, and PipeWire then runs the sink's whole group on it (seen
  locally: see "Receiver, as built"). With the netjack2 manager's ports
  linked in, that is the audio machine's graph driver, the DAC. The servo
  then measures the same clock on both sides and has nothing to steer;
  latency stays at its setpoint
  ([latency-regulation.md](latency-regulation.md)).
- Bursts are dated by `start_byte` on the carrier, which is exactly the
  transport time the sync host already counts for IEC 61937 input.
- Nothing new on the network: no protocol, no return channel, no clock sync
  daemon.

What it demands:

- **A bit-exact path.** The carrier is 16-bit words carried as float samples.
  Exact as long as nothing touches them: no dither on the float-to-S16
  conversion, no channel mixing, no resampling (the link forbids it already),
  no other source summed on the carrier channels, no software volume in mpv
  (a spdif-flagged format bypasses mpv's filter chain, see sender work), **and
  no volume on any sink the words go through**: a PipeWire sink applies its
  volume in software to every channel. A null test over the real link is the
  gate.
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
media time in samples, sender monotonic time. The receiver's
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
| 8-12 | data-type dependent | bits 16-20 of the payload length, whose low 16 bits are `Pd`: up to 2 MiB. A unit of FLAC or LPCM on many channels is longer than the 65 535 bytes `Pd` alone counts. The carrier is not encoded in the burst: the receiver knows it from the negotiated format (PipeWire) or the device (S/PDIF) |
| 13-15 | bitstream number | 0 |

The carrier is 16-bit little-endian words, and a payload byte keeps its
place: byte `i` of the payload is byte `8 + i` of the burst, with no word
swap. A payload of odd length is followed by a zero the length does not count.

Pd is the payload length in bytes, as E-AC-3 and MAT do, continued in Pc
past 16 bits. The payload is the
OBUs of exactly one temporal unit, preceded by redundant descriptor copies
(`obu_redundant_copy = 1`, OBU header bit 2) at a fixed interval, one second
proposed, and right after a seek or a reconnect. A new sequence (track
change, codec config change) is a non-redundant sequence header followed by
fresh descriptors; the family rebuilds its decoder on it. No padding inside
the payload: a zero byte parses as a codec config OBU header. Zero padding
sits after the payload up to the repetition period; the parser reads `Pd`
bytes and resyncs on the next preamble, so the padding is dropped without a
rule.

A burst is a resync point for the receiver: what a truncated burst left
unframed is dropped when the next burst arrives. Temporal delimiter OBUs are
not carried; the burst is the unit.

The repetition period is one temporal unit, in carrier frames. It is
constant per stream because IAMF fixes the frame size per codec config. On a
48 kHz carrier a 20 ms Opus unit is 960 frames; on the classic 2-channel
192 kHz and HBR carriers it is 3840. Bursts start on a carrier frame
boundary.

### Receiver, as built

The burst type, the parser and the bridge arm are steps 1 and 2 of "Order of
work". What takes the carrier in is the renderer's PipeWire sink
(`render.input_mode: pipewire`), on its **linear PCM**: IAMF has no IEC 958
codec PipeWire could negotiate, and a link that only carries linear PCM has
no encoded format at all, so the words arrive as float samples, one word per
sample (`word / 32768`, which a 32-bit float holds exactly).

- **`spdif::pcm`** turns the floats back into words and finds the bursts: a
  burst starts on a frame of the carrier, so its preamble is the first four
  samples of a frame. A sample that is not a 16-bit word is counted.
- **`sync_host::carrier`** is a gate on the sink's linear PCM, in the
  capture's pump (not in the realtime callback). While bursts come, each
  chunk is handed to the host as its words, as an IEC 958 carrier with the
  capture's exact transport count: the host deframes it, places each burst
  by `start_byte` and counts the source's clock in carrier bytes, with no
  change to the host or to the servo. The zeros between two bursts and after
  the last one are carrier time with nothing on it, like a cycle a player
  gave nothing for: a pause shorter than the silence that ends a stream
  (0.5 s) is filled when the next burst shows where it ends, a longer one
  ends the stream, and nothing tells the servo that a source which stopped
  is still sending.
- **On the sink as it is** (audio on the 7.1 map), a carrier is recognised
  and never played as audio. Before this, mpv's carrier pointed at the sink
  was rendered as the audio it is not: −15 dBFS RMS of noise on eight
  speakers, measured into a file. It works with no configuration for a
  carrier of 8 channels at 48 kHz that reaches the sink untouched, and only
  then: at a sink volume other than 100 % the words are no longer words, and
  the samples are audio like any other.
- **A carrier sink** is `render.live_input.carrier_channels: N` (4 to 64;
  option `live_input_carrier_channels`, staged with the live input;
  `--live-input-carrier-channels`). Its linear PCM is `N` unpositioned
  channels (`AUX0`…) at 48 kHz that are **never audio**: what holds no burst
  is dropped, and said once in the log when it is not silence either (a
  carrier that a volume, a mixer or a resampler changed looks like that). It
  puts its own volume and mute back when they are moved, and restores none
  saved by an earlier session. This is the sink for a link: use it on the
  audio machine.
- The sink's linear PCM runs at 48 kHz only, like the link. A track at
  another rate does not cross: mpv's stream is never linked (seen with a
  44.1 kHz track: the stream stays suspended, nothing plays at either end).
- **The count starts at the first burst that gives audio.** A receiver
  that missed the opening burst drops every unit until the next copy of the
  descriptors. Counted from the first burst seen, that second read as
  frames the decoder was holding, and the latency target was raised to it
  for the rest of the stream; the host now starts its count again at each
  IAMF burst until one has given audio.
- The FIFO and a file needed nothing beyond the bridge arm.

In the Studio, the Audio Input panel has the carrier sink's switch and its
channels in the PipeWire group, for an engine that declares the option, and
its status line says when the sink that runs is a carrier sink. Not done:
nothing says that a carrier is being recognised on the plain sink, beyond
the engine's log.

Checked, with an isolated renderer on a sink of its own and mpv
(`--ad=iamf-iec61937`) as the sender. A render is compared, as a file, with
the offline render of the stream the Matroska file holds, through the same
engine and layout (7.1.4).

| Run | Result |
|---|---|
| Carrier sink of 8 channels, Opus 7.1.4 bed | File byte-identical to the offline render (240 000 frames, 12 channels) |
| Carrier sink of 16 channels, mpv on 16, FLAC 5.1 + 4 objects | Byte-identical |
| The same sink, its volume set to 50 % and muted before the stream, to 30 % during it | Put back each time; byte-identical |
| Carrier sink, volume at 50 % before the sink put it back by itself | Nothing played, the warning logged |
| Plain sink, mpv's 8-channel carrier | Recognised in the cycle of its first burst; 2 880 000 samples identical, silence before and after |
| **Link simulation**: mpv into a null sink of 8 `AUX` channels, its monitor ports linked one to one into the carrier sink's | The sink's group runs on the null sink's driver. Byte-identical |
| mpv's carrier bytes (`--ao=pcm`), the framer's, and the raw stream, each written into the input pipe; the framer's read from a file | Byte-identical, bed and objects |
| Device output (a null sink), target 150 ms, 12 s | Held, error −0.001 ms, floor 46 ms, no realign |
| The link simulation with a device output, eight starts | 150 ms held in all eight (floor 37 to 47 ms, error within 0.001 ms, no realign), the two that lost the opening burst included |
| The same: 45 s of one pass; then five minutes of a 50 s clip looped | 45 s: error 0.000 ms, source and device both +0.02 ppm, floor 46 ms, no realign. Five minutes: target held (floor 42 ms), the same rates, no gap, no underrun at the sender; four realigns over the six loops of the file (see "Found on the way") |
| Pause of 0.3 s; pause of 3 s; seek | 14 336 frames of silence fill the gap; the stream ends and resumes without new descriptors; the cut burst's 128 frames are dropped. Nothing else is reported lost or made up |

The link simulation stands in for the netjack2 manager's ports on the audio
machine, with a null sink's clock in place of the DAC's. It says the
mechanism works on a foreign driver; it says nothing about the real link.

**The first bursts of a stream do not always arrive.** In about one start
in three here (21 of 63), the first 40 ms mpv wrote into PipeWire reached
the sink as silence: the two bursts they held were gone, the third was in
its place. The first is the one that opens the sequence, so nothing decoded
until the next copy of the descriptors, a second later at the sender's
default. With a copy on every unit (`--ad-iamf-iec61937-copy-interval=20`,
141 bytes a unit here) the receiver decoded from the first burst it got in
ten starts of ten, four of which had lost the opening one. Until the sender
leads its first bursts with copies by itself, that is the setting to play
with.

### Sender (mpv fork), as built

`--ad=iamf-iec61937` on an IAMF track (`audio/decode/ad_iamf_iec61937.c`,
branch `feat/iamf-forward`). A decoder of its own rather than a mode of
`ad_orender`: it needs no engine and no liborender.

- One burst per Matroska block. The descriptors from `CodecPrivate` lead the
  first burst and the first after a seek, as they are, which opens a new
  sequence at the receiver; a redundant copy leads one every
  `--ad-iamf-iec61937-copy-interval` (1 s by default).
- The carrier is `--ad-iamf-iec61937-channels` wide (8 by default) at the
  track's own sample rate, so a unit is that many carrier frames and mpv's
  A/V sync works unchanged. **The far end has to run at that rate**: a
  resampler on the way destroys the words, so a 44.1 kHz track does not
  cross a 48 kHz link.
- A unit longer than its period leaves the period empty and says to widen
  the carrier.
- The frames are a compressed format, `AF_FORMAT_S_IAMF`: no filter, volume
  or resampler of mpv's touches them, and the player refuses any output
  that would change the format.
- `ao_pcm` writes the words as they are. `ao_pipewire` presents them as
  unpositioned S16 channels with remixing, resampling, dither and restored
  volume off. **It refuses the default sink and any fallback**: on a
  loudspeaker these words are full-scale noise, so the carrier sink is named
  with `--audio-device=pipewire/<sink>` or nothing plays.
- The same decoder is on the fork's `orender-master` branch, and both patch
  sets of mpv-omniphony carry it. Not done, and not planned: a libavformat
  `spdifenc` patch (FFmpeg has no codec id for IAMF, it is a container
  there).
- **The carrier's channels have no position.** On a sink with more ports
  than the carrier has channels they are linked to the first ones: on a
  null sink shaped like the netjack2 sink (`FL`…`RR`, `AUX0`…`AUX7`), mpv's
  eight channels went to `FL`…`RR`. See step 5.

Checked: through `ao_pcm` the output is byte-identical to
`orender wrap-iec61937` on the same content (Opus, FLAC and object
encodings; 2, 8, 16 and 24 channels; FLAC units of 67 kB, past what `Pd`
alone counts). Through PipeWire into a null sink and back from its monitor
(S16 to float to S16), 600 bursts of 600 came back intact on their period.
That is the local graph, not the netjack2 link.

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

1. **Mid-stream lock.** Needed nothing: the host's probing router finds the
   sequence header wherever it sits.
2. **Forced codec surface.** Not done, and not needed by anything built
   here: a raw IAMF stream is recognised by its sequence header.
3. **Tests.** The bridge's are in `bridge/tests/iamf_carrier.rs`. The
   FIFO-fed runs were made by hand (see "Receiver, as built"); they are not
   in the test suite, which has no IAMF fixture of its own to feed them.

### Sender work

Only with the timestamped framing of clocking option 2; the untimed variant
is rejected. The same `forward` decoder gains a `raw` flavour: header per
unit, local silence of the unit's duration to keep mpv's clock, position
reports from the receiver turned into the reported output delay.

## Acceptance

- Null tests: in-process, raw over the FIFO, IEC 61937 over the FIFO, and
  IEC 61937 over the real netjack2 link decode to identical PCM on an anonymous
  IAMF fixture. The last one is the bit-exactness gate for option 1.
  **Done but for the last**: raw and bursts over the FIFO, bursts from a
  file, and bursts over a local PipeWire graph (directly, and through
  another node's ports) all give the offline render byte for byte. The real
  link is step 5.
- Clock: over the link, orender's latency stays at its setpoint for an hour
  with no resampler activity, and mpv reports a stable A/V offset.
  **Locally only**, on the link simulation: the target held for 45 s of one
  pass and for five minutes of a looped clip, the source and the device at
  the same rate (two null sinks' clocks), realigns at loops of the file
  only. The real link, and the hour, are step 5.
- Resync: join mid-stream, seek, reconnect, track switch, each locking within
  one descriptor interval. **Done** for the late join (bridge tests), the
  seek, the pause and the stop and start again (live, see "Receiver, as
  built"). A track switch was not played.
- Steady state: no allocation per burst beyond the parser's existing buffers;
  the family already frames OBUs without copying units. **Not met by the
  sink's capture, by its design**: its pump allocates one buffer per graph
  cycle for any format, and one more for the words while a carrier plays.
  Neither is in the realtime callback, which only copies into a ring.
- Listening: the user listens live on the remote scenario before any merge.
  **Not possible before step 5.** What is merged before it plays nothing new
  on a setup that sends no carrier, and stops one being played as noise.

## Order of work

1. **Done.** Bridge: the IAMF arm on the `Iec61937` transport, a burst as a
   resync point, late join on a redundant copy, tests
   (harletty/harletty-bridge#148). No ABI change. The mid-stream lock of the
   raw transport needed nothing: the host's probing router already finds the
   sequence header wherever it sits.
2. **Done.** orender: the data-type constant, the burst writer and the
   extended length in the parser (`spdif::burst`), the sequence cutter and
   the carrier period (`spdif::iamf`), the `wrap-iec61937` test framer
   (#831). The null test lives in the bridge
   (`bridge/tests/iamf_carrier.rs`): every libiamf conformance vector the
   framer can carry, 233 of them, decodes through the parser and the burst
   type to the frames of its raw decode.
3. **Done.** orender: the carrier input, on the sync host of the resampling
   cutover (#830). See "Receiver, as built". What the cutover had put in
   question answered itself: the carrier is the sink's linear PCM, so no new
   format; the gate hands it to the host as an IEC 958 carrier, so the
   transport count is the capture's own; and the clock is whichever driver
   the ports linked into the sink bring, the DAC's on the audio machine.
4. **Done.** mpv: `--ad=iamf-iec61937`, `AF_FORMAT_S_IAMF`, `ao_pcm` and
   `ao_pipewire`, on the fork's `orender` and `orender-master` branches
   (mgth/mpv#31 to #33) and in both patch sets of mpv-omniphony (#87).
5. **Audio machine. Not done: it needs the machine, the link and ears.**
   In order:
   1. *The channel plan.* The link has 16 channels today, all of them
      rendered PCM, and its sink on the player has **one volume for all 16**
      (at 36 % when this was written): a carrier on some of its channels is
      scaled with the rest and is no carrier any more. Either the link's
      sink stays at 100 % for good and the level is set elsewhere, or the
      carrier gets a netjack2 link of its own (a second driver on the
      player, a second manager on the audio machine), whose sink nothing
      else uses. The second also answers "split or widen", and keeps the
      carrier's words off channels that reach the DAC.
   2. *The sender's ports.* mpv's carrier channels have no position: on a
      sink with other ports before the carrier's, link them by hand
      (`pw-link`), or give the carrier a sink of exactly its width (the
      second link above). Tagging mpv's channels `AUX0`… would do it on a
      shared sink; that is a change to the fork, not made. Play with
      `--ad-iamf-iec61937-copy-interval=20` (see "Receiver, as built": the
      start of a stream).
   3. *The receiver.* orender on the audio machine with the `iamf` bridge,
      its layout, `render.input_mode: pipewire` and
      `render.live_input.carrier_channels` at the carrier's width; the
      manager's carrier ports linked one to one to the sink's
      `playback_AUX<n>` (the link's loader script is where the existing
      links are made), **and to nothing else**: on the DAC they are noise.
      Check with `pw-top` or `pw-dump` that the sink's node runs on the
      DAC's driver once linked.
   4. *The link null test.* The receiver writing to a file with the channels
      passed through (`--output-backend file --channel-render-mode host`),
      mpv playing a short stream through the link, the file compared with
      the offline decode of the same stream. The receiver's log says so when
      the samples it gets are not words.
   5. *The clock.* An hour of playback: the latency held, no realign, the
      source and device rates equal.
   6. *Lip sync.* mpv sees the link's latency, not the renderer's: a fixed
      `--audio-delay` of the renderer's latency target, by ear and by a
      sync test clip; the telemetry later.
   7. Listening.
6. Transport A with timestamps and a return channel: only if a link without a
   synchronous carrier becomes a target.

Found on the way:

- On the IAMF v2.0 vector `test_000850` the burst path matched the reference
  rendering while the raw path did not (one frame of 24 with the stream's
  temporal delimiters; a mix-gain step placed by how the bytes were
  chunked). Fixed in the bridge (harletty/harletty-bridge#147).
- The first 40 ms of a stream mpv starts on a PipeWire sink are lost about
  one start in three, the opening burst with them (see "Receiver, as
  built"). The receiver recovers at the next copy of the descriptors and
  keeps its latency; the sender should not let that take a second. To do in
  the fork: lead the first bursts after a start or a seek with a redundant
  copy, whatever the copy interval.
- With a looped file each pass reports its codec's trimmed samples as a
  decoder shortfall (960 frames for the Opus stream played here: the
  pre-skip and the end padding). That is the count being right: the
  carrier's time for a unit is the whole unit. Taken in one step it is
  20 ms, the realign threshold: four of six loops realigned in a
  five-minute run. Nothing realigned within a pass.

Branch names: `feat/iec61937-iamf` (bridge, orender), `feat/iamf-forward`
(mpv fork), `feat/carrier-input` (orender, step 3). No brand names.

## Open questions

- The data type value, `0x1D`. IEC 61937-2 is at edition 3.1 (2021, amended
  2026); its contents list some thirty formats, the last ones Auro-Cx, AVSA
  and AVS3-P3, and no IAMF. Edition 3 also turned on an *extended* data type in
  `Pe`, which is how more formats than five bits count are told apart. Its
  table of values is not public and was not read: that `0x1B` to `0x1E` are
  free is what this project could establish, not what the standard was seen
  to say. The constant is stated once on each side (`spdif::burst`,
  `bridge-family-iamf`, the mpv decoder) so it can move.
- Bit-exactness of 16-bit words through float channels on the real link:
  exact on a local graph, through a sample-format conversion and through
  another node's ports; on the link itself, proven only by the link null
  test.
- The channel plan on the link: see step 5. A carrier of 8 channels holds
  Opus, AAC and most FLAC (the short synthetic FLAC stream with four objects
  played here on 16 would fit 8); what the user plays decides.
- Lip-sync delay: fixed first, telemetry later; whether the input sink should
  also publish its latency into the audio machine's graph.
- Descriptor interval: one second, or every N units; cost is a few hundred
  bytes per copy either way.
