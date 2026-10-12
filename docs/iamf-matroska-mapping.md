# AOM IAMF codec mapping in Matroska — draft

**Status:** draft, written in the Omniphony project as a proposal for
[matroska-specification#940](https://github.com/ietf-wg-cellar/matroska-specification/issues/940).
Not endorsed by AOMedia or the CELLAR working group. Version 0.2,
2026-10-12 (v0.1: 2026-10-01). Changes since v0.1 are listed at the end.

This document specifies the storage of [IAMF](#iamf-specification) (AOMedia
Immersive Audio Model and Formats) in [Matroska](#matroska-specifications)
audio tracks. It follows the structure of the Matroska AV1 mapping
(`codec/av1.md`), the other OBU-based AOMedia format, and stays as close as
possible to IAMF's own [ISO-BMFF encapsulation](#iamf-specification) (IAMF
"ISO-BMFF IAMF Encapsulation"), so that a stream can move between the two
containers without touching its OBUs. It defines a Matroska mapping only; no
WebM profile is claimed.

Elements in square brackets __[]__ refer to syntax elements of the
[IAMF specification](#iamf-specification).

## Registry entry

In the style of the Matroska codec registry:

* **Codec ID:** `A_IAMF`
* **Codec Name:** AOMedia Immersive Audio Model and Formats
* **Description:** An IAMF IA Sequence stored as one audio track: its
  Descriptors in the `CodecPrivate`, one Temporal Unit per `Block`. The
  track carries audio elements that the player renders to its own
  loudspeaker layout or to headphones, so the usual channel count does not
  describe it.
* **Initialization:** the payload of the ISO-BMFF `IAConfigurationBox`
  (`configurationVersion`, `configOBUs_size`, `configOBUs`); see
  [CodecPrivate](#codecprivate).
* **Citation:** Immersive Audio Model and Formats, version 2.0.0, AOMedia,
  https://aomediacodec.github.io/iamf/v2.0.0.html

# Terms

## Block
A Matroska element storing one frame; also a `SimpleBlock` outside a
`BlockGroup`. "Block" in this document means both.

## Descriptors
The OBUs that configure an IA Sequence, in this order: one
`IA Sequence Header OBU`, all `Codec Config OBUs`, all `Audio Element OBUs`,
all `Mix Presentation OBUs`, optionally `Metadata OBUs` after the sequence
header and before the first mix presentation, and possibly `Reserved OBUs`
(IAMF "Descriptor OBUs"). They are what the ISO-BMFF `IAConfigurationBox`
carries as __[configOBUs]__.

## IA Sequence
Descriptors followed by the IA Data they configure (IAMF "IA Sequence").

## OBU
Open Bitstream Unit: an IAMF OBU header (__[obu_type]__,
__[obu_redundant_copy]__, a type-dependent flag, __[obu_extension_flag]__,
__[obu_size]__ as leb128, optional trimming and extension fields) followed by
its payload. Unlike AV1, every IAMF OBU carries its size.

## Temporal Unit
All `Audio Frame OBUs` sharing a decode start time and duration, one per
coded audio substream, with the non-redundant `Parameter Block OBUs` that
start within that duration (IAMF "Timing Model"). An IAMF `Temporal Unit` may
be preceded by a `Temporal Delimiter OBU`, whose __[is_not_key_frame]__ flag
marks a unit that is not a key frame.

# Mandatory TrackEntry elements

## TrackType
EBML Path: `\Segment\Tracks\TrackEntry\TrackType` | Mandatory: Yes

The `TrackType` **MUST** be `audio` (2).

## CodecID
EBML Path: `\Segment\Tracks\TrackEntry\CodecID` | Mandatory: Yes

The `CodecID` **MUST** be the ASCII string `A_IAMF`.

## CodecPrivate
EBML Path: `\Segment\Tracks\TrackEntry\CodecPrivate` | Mandatory: Yes

The `CodecPrivate` **MUST** contain the payload of the ISO-BMFF
`IAConfigurationBox` (IAMF "IA Configuration Box"), i.e. the box without its
size and type:

```c
unsigned int (8) configurationVersion;  // 1
leb128()         configOBUs_size;
unsigned int (8 x configOBUs_size) configOBUs;
```

* `configurationVersion` **MUST** be 1. A reader **MUST** reject a track whose
  `configurationVersion` it does not know.
* `configOBUs` **MUST** be the complete Descriptors of the IA Sequence stored
  in the track, with the same constraints as in ISO-BMFF: sequence header
  first, then codec configs, audio elements, mix presentations; `Metadata
  OBUs` and `Reserved OBUs` where IAMF allows them. Every OBU in it **MUST**
  have __[obu_redundant_copy]__ set to 0.
* `configOBUs_size` **MUST** equal the number of bytes from the end of the
  size field to the end of the `CodecPrivate`. A reader **MUST** reject a
  `CodecPrivate` whose `configOBUs_size` does not match the element's length;
  it **MUST NOT** read past the element.
* The IAMF profiles bound every OBU at 2 MiB (__[obu_size]__ at most
  2^21 bytes); that bound applies to each OBU in `configOBUs` as it does to
  the OBUs in `Blocks`.

Reusing the box payload rather than bare OBUs keeps a version byte for
future extension and makes conversion from ISO-BMFF a copy. The profile bytes
(__[primary_profile]__, __[additional_profile]__) sit at fixed offsets in the
sequence header, so they are not duplicated in front of the OBUs as the AV1
mapping does for its sequence header fields.

## SamplingFrequency
EBML Path: `\Segment\Tracks\TrackEntry\Audio\SamplingFrequency` | Mandatory: Yes

The `SamplingFrequency` **MUST** be the sample rate IAMF uses for computing
offsets with the track's codec (IAMF "Codec Specific"): 48000 for `Opus`; the
__[samplingFrequencyIndex]__ rate for `mp4a`; the `STREAMINFO` sample rate for
`fLaC`; __[sample_rate]__ for `ipcm`.

An IA Sequence may carry two `Codec Config OBUs`. IAMF v2.0 requires them to
state the same __[num_samples_per_frame]__ and the same output sample rate,
and one of them to be `ipcm`; the `SamplingFrequency` is therefore
unambiguous, and it is that common rate.

## Channels
EBML Path: `\Segment\Tracks\TrackEntry\Audio\Channels` | Mandatory: Yes

An IA Sequence has no single channel count: its audio elements are rendered
to whatever layout the playback system has. Matroska does not allow 0 (the
value ISO-BMFF uses), so the `Channels` **MUST** be 2, and readers **MUST
NOT** rely on it for anything — not to size buffers, not to choose an output
layout. This mirrors IAMF's own convention for the codec headers it embeds
(Opus __[Output Channel Count]__ and AAC __[channelConfiguration]__ are set
to 2 and ignored).

# Codec delay, trimming and seeking

IAMF carries its trimming in-band: `Audio Frame OBUs` signal
__[num_samples_to_trim_at_start]__ and __[num_samples_to_trim_at_end]__, and
an IAMF decoder discards those samples itself. The OBUs are authoritative.
The container restates the same trims in the elements Matroska has for them,
`CodecDelay` for the start and `DiscardPadding` for the end, so that a player
that does not decode the track (a remuxer, an editor, a player that
passes the track to an external decoder) still knows the presentation
timeline. A player **MUST** apply exactly one of the two: either it lets the
decoder trim and ignores `DiscardPadding` (while still subtracting
`CodecDelay` from the timestamps, as Matroska requires of every track), or
it configures the decoder not to trim and applies `CodecDelay` and
`DiscardPadding` itself. Applying both trims twice.

## CodecDelay
EBML Path: `\Segment\Tracks\TrackEntry\CodecDelay` | Mandatory: Yes (default 0)

* The `CodecDelay` **MUST** be the total number of samples the stream trims at
  its start (the sum of __[num_samples_to_trim_at_start]__ over the leading
  `Audio Frame OBUs` of one substream), converted to Matroska Ticks at the
  `SamplingFrequency`. For `Opus` this equals the __[Pre-skip]__ of the codec
  config, as in the Matroska Opus mapping:

        CodecDelay = trimmed_start_samples * 1,000,000,000 / SamplingFrequency

* `Block` timestamps are decode times, as in ISO-BMFF: the first `Block` of a
  stream has timestamp 0. The decoder discards the start-trimmed samples, and
  subtracting `CodecDelay` (as Matroska requires) puts the first kept sample
  at 0, exactly as for Opus.

## DiscardPadding
EBML Path: `\Segment\Cluster\BlockGroup\DiscardPadding`

* A `Block` whose `Audio Frame OBUs` carry a non-zero
  __[num_samples_to_trim_at_end]__ **SHOULD** be written in a `BlockGroup`
  with a `DiscardPadding` equal to that trim, converted to nanoseconds at
  the `SamplingFrequency`:

        DiscardPadding = trimmed_end_samples * 1,000,000,000 / SamplingFrequency

  In a conforming IA Sequence only the last `Temporal Unit` trims its end.
* A `DiscardPadding` **MUST NOT** state anything other than the OBUs' own
  end trim. A reader that decodes the track **MUST NOT** apply it on top of
  the decoder's trimming (see above).
* The `Block` duration still includes the trimmed samples
  (__[num_samples_per_frame]__), as for Opus; muxers **SHOULD** set the
  `Segment` `Duration` to the presentation duration, after both trims.

## SeekPreRoll
EBML Path: `\Segment\Tracks\TrackEntry\SeekPreRoll` | Mandatory: Yes (default 0)

The `SeekPreRoll` **MUST** be the pre-roll IAMF requires after a discontinuity:

    SeekPreRoll = -audio_roll_distance * num_samples_per_frame * 1,000,000,000 / SamplingFrequency

with __[audio_roll_distance]__ and __[num_samples_per_frame]__ from the
`Codec Config OBU`. That gives 80 ms for `Opus` with 20 ms frames
(R = ⌈3840 / 960⌉ = 4 frames, the value the Matroska Opus mapping
recommends), one frame for `mp4a`, and 0 for `fLaC` and `ipcm`. With two
codec configurations, the larger value applies.

# Block data

Each `Block` **MUST** contain exactly one Temporal Unit, as one ISO-BMFF
`IA Sample` does (IAMF "IA Sample Format"):

* The OBUs **MUST** be stored complete (header with __[obu_size]__, then
  payload), back to back, in the order IAMF "IA Data OBUs" requires.
* `Temporal Delimiter OBUs` **MUST NOT** be stored; their only information,
  __[is_not_key_frame]__, moves to the `Block`'s keyframe signalling.
* Descriptor OBUs (`IA Sequence Header`, `Codec Config`, `Audio Element`,
  `Mix Presentation`) **MUST NOT** be stored in `Blocks`, redundant or not: the
  `CodecPrivate` already provides random access to them.
* `Metadata OBUs` **MAY** be stored where IAMF allows them in a Temporal Unit.
  `Reserved OBUs` **MAY** be present and **MUST** be ignored by readers that do
  not understand them.
* The `Block` duration is the Temporal Unit's decode duration,
  __[num_samples_per_frame]__ samples, trimmed samples included.

Lacing **MUST NOT** be used: a `Block` has one keyframe flag, and every
Temporal Unit needs its own (see [Keyframes](#keyframes)).

## Timestamps
`Block` timestamps are the Temporal Unit's decode time in Matroska Ticks,
rounded to the nearest tick. Frame durations such as 1024 samples at 48 kHz
(21.333 ms) do not fall on `TimestampScale` ticks, so consecutive timestamps
jitter by up to half a tick. Readers **SHOULD** reconstruct the sample-exact
timeline from the decoded sample counts (every unit is
__[num_samples_per_frame]__ samples, less the in-band trims) or from
`DefaultDuration`, and use the `Block` timestamps only to place the first
unit after a seek or a discontinuity.

## Keyframes
A Temporal Unit is a key frame when a decoder that starts from it has every
parameter at its right value: every parameter (mix gains, demixing,
reconstruction gains, object positions) is either at its definition's
default or stated by a `Parameter Block OBU` that starts in that unit. IAMF
lets a `Parameter Block OBU` span several units, in which case the units it
covers after the first are not key frames and IAMF marks them
(__[is_not_key_frame]__ in the `Temporal Delimiter OBU`).

* Muxers **SHOULD** write every `Block` as a key frame, and encoders
  **SHOULD** restate parameter state in every Temporal Unit so that they
  can. This is what IAMF's own ISO-BMFF encapsulation asks of IA Samples (its
  "IA Sample Format" section has every IA Sample a sync sample; its text on
  the `stss` box is contradictory, saying both that the box shall not be
  present and that it should be, and this mapping follows the IA Sample
  Format section). It makes Matroska tooling that treats audio `Blocks` as
  independent — splitters, players that ignore the keyframe flag on audio —
  safe, and a seek lands with every parameter right.
* A `SimpleBlock` **MUST** be marked as a keyframe if and only if its Temporal
  Unit is a key frame.
* A `Block` inside a `BlockGroup` **MUST** carry a `ReferenceBlock` if its
  Temporal Unit is not a key frame, referencing the `Block` that holds the
  `Parameter Block OBUs` it depends on. This is the escape hatch for streams
  converted from elsewhere that cannot be restated.

Codec pre-roll (`SeekPreRoll`) is independent of this flag, as for every audio
codec in Matroska: a keyframe is a point the IAMF parameter state can restart
from, not one the codec state can.

## DefaultDuration
EBML Path: `\Segment\Tracks\TrackEntry\DefaultDuration`

The `DefaultDuration` **SHOULD** be set to
`num_samples_per_frame * 1,000,000,000 / SamplingFrequency` (20,000,000 for
20 ms Opus frames), rounded to the nearest nanosecond when the division is
not exact.

# Segment restrictions

The Descriptors of an IA Sequence are fixed for its whole duration, and a
Matroska `CodecPrivate` for the whole `Segment`. A track **MUST** therefore
hold a single IA Sequence: an IAMF configuration change (IAMF "IAMF
Configuration Changes"), which starts a new IA Sequence with new
non-redundant Descriptors, **MUST** start a new `Segment` (for example a
linked or chained segment).

Matroska has `CodecState` (`\Segment\Cluster\BlockGroup\CodecState`) for a
change of initialization data mid-`Segment`, which could carry the new
Descriptors on the `Block` that starts the new IA Sequence. This mapping does
not use it: the AV1 mapping forbids a sequence header change in the same way,
and no reader of this mapping supports `CodecState`. ISO-BMFF's several
sample entries per track are mapped to several `Segments`.

# Track language

An IA Sequence may mix audio elements in several languages. As ISO-BMFF
recommends, the `Language` (or `LanguageBCP47`) **SHOULD** be `mul` when
several languages are present and `und` when none applies.

# Cue considerations

Only `Blocks` marked as keyframes (or `BlockGroups` without `ReferenceBlock`)
**SHOULD** be referenced in the `Cues`. A seek lands `SeekPreRoll` before its
target and decodes forward, as for Opus.

# Security considerations

An IAMF track is a sequence of length-prefixed units whose structure is
described by the `CodecPrivate`, and a reader allocates by what it reads:

* Every OBU carries __[obu_size]__, and `CodecPrivate` carries
  `configOBUs_size`. A reader **MUST** check each against the bytes actually
  present (the `Block` or the element) before allocating or reading, and
  **MUST** honour the 2 MiB bound the IAMF profiles put on an OBU rather than
  trust a larger stated size.
* The Descriptors bound what a `Block` may contain: the number of coded
  substreams (and so of `Audio Frame OBUs` a Temporal Unit holds) and the
  parameter ids a `Parameter Block OBU` may name follow from the
  `Audio Element` and `Mix Presentation OBUs`. A `Block` with more frames,
  frames for substreams the Descriptors do not declare, or blocks for
  parameters they do not define **SHOULD** be rejected or truncated, not
  used to grow a decoder's state.
* `Reserved OBUs` and unknown `obu_type` values **MUST** be skipped by their
  stated size and never interpreted.
* A `DiscardPadding` or `CodecDelay` larger than the `Block` or the stream
  **SHOULD** be clamped to what is there.

Implementations of IAMF parsers have had out-of-bounds reads and writes from
trusting stated sizes (FFmpeg's IAMF demuxer and parser carried several
CVE-numbered fixes in 2024 and 2025); the checks above are what they
address.

# Encryption

Not defined in this draft. ISO-BMFF protects IAMF with full-sample `cenc` or
whole-block `cbcs`; a Matroska equivalent would encrypt whole `Blocks`,
keeping OBU headers in the clear if partial encryption is wanted.

# Reconstructing a standalone IA Sequence (informative)

A reader that feeds an IAMF decoder a standalone OBU stream (IAMF "Standalone
IAMF Representation") produces:

1. the `configOBUs` from the `CodecPrivate`;
2. then each `Block`'s data in order. If the decoder wants `Temporal Delimiter
   OBUs`, one is prepended to every Temporal Unit, with __[is_not_key_frame]__
   set to 1 for `Blocks` not marked as keyframes.

After a seek, a reader passes `configOBUs` again (or keeps the decoder's
configuration) and then the `Blocks` from the pre-roll point.

# Converting from ISO-BMFF (informative)

| ISO-BMFF | Matroska |
|---|---|
| `iamf` sample entry, `iacb` payload | `CodecID` `A_IAMF`, `CodecPrivate` = `iacb` payload |
| `channelcount` = 0, `samplerate` = 0 | `Channels` = 2 (ignored), `SamplingFrequency` from the codec config |
| one `IA Sample` | one `Block`, data unchanged |
| sample duration (`stts`/`trun`) | `Block` timestamp delta / `DefaultDuration` |
| edit list start trim | `CodecDelay` (the same samples, trimmed in-band) |
| edit list end trim | `DiscardPadding` (the same samples, trimmed in-band) |
| `roll` sample group | `SeekPreRoll` |
| non-sync samples (`stss`, `sample_is_non_sync_sample`) | `SimpleBlock` keyframe flag cleared / `ReferenceBlock` |
| `mdhd` language `mul`/`und` | `Language` `mul`/`und` |
| several sample entries | several `Segments` |

# Implementation notes (informative, Omniphony)

These are not part of the mapping. In the Omniphony stack, as of v0.2:

* **Reading:** mpv demuxes Matroska itself (`demux_mkv`), not through
  FFmpeg. The mpv fork maps `A_IAMF` to the codec `iamf`, uses `SeekPreRoll`
  as the seek pre-roll, subtracts `CodecDelay` from the timestamps and
  ignores `DiscardPadding` for this codec (the decoder trims in band), and
  has `ad_orender` feed the `CodecPrivate`'s `configOBUs` to the engine
  before the first `Block` and after every reset; harletty-bridge's IAMF
  family decodes the `Blocks` as they are. Trimming chain, checked on the
  libiamf vector `test_000220` (Opus, 312-sample pre-skip, 648-sample end
  trim): played to a file, the output has exactly the reference render's
  240 000 frames and lines up with it at lag 0, so the decoder drops the
  trimmed samples and the first kept sample lands at 0.
* **Writing:** neither mkvmerge nor FFmpeg can produce such a track today
  (FFmpeg exposes IAMF as one stream per substream plus a stream group, not
  as one track). Two writers follow this mapping: `scripts/iamf2mka.py`
  writes it from IAMF in ISO-BMFF (fragmented or not) or from a standalone
  `.iamf`, and `--extract` reads it back to a standalone stream; the
  harlettizer encoder writes it directly (`iamf --out x.mka`), restating
  every object's position in every Temporal Unit so that every `Block` is a
  key frame. Both write the end trim as `DiscardPadding`. Checked on the
  libiamf vectors: `mkvinfo` reports no error; the `--extract` round trip is
  byte-identical to the standalone stream but for the dropped Temporal
  Delimiters; mkvmerge 102 carries `CodecPrivate`, `CodecDelay`,
  `SeekPreRoll` and `DiscardPadding` through a remux untouched.
* **Test material:** the libiamf conformance vectors (AOMediaCodec/libiamf
  `tests/`, standalone `.iamf` and ISO-BMFF forms with reference renders)
  convert directly.

# Changes since v0.1

* Registry entry added; the document claims a Matroska mapping only.
* `CodecPrivate`: a mismatched `configOBUs_size` is rejected; the 2 MiB OBU
  bound applies; the profile bytes are not duplicated (was open).
* `SamplingFrequency`: the two-codec-config case settled from IAMF v2.0
  (was open).
* `Channels`: **MUST** be 2 and **MUST NOT** be relied on (was **SHOULD**,
  with alternatives open).
* Trimming: the container restates the in-band trims, `DiscardPadding` on
  the blocks that trim their end; a player applies exactly one of the two
  (v0.1 said `DiscardPadding` **SHOULD NOT** be written).
* Timestamps: readers reconstruct continuity from sample counts.
* Keyframes: `Blocks` **SHOULD** be key frames and encoders **SHOULD**
  restate parameter state per unit; lacing **MUST NOT** be used (was
  **MAY**); the ISO-BMFF `stss` contradiction noted.
* Segment restrictions: `CodecState` named and set aside.
* Security considerations added.
* Implementation notes updated to the current readers and writers.

# Referenced documents

## IAMF specification
Immersive Audio Model and Formats, version 2.0.0 (2026-08-24), AOMedia,
https://aomediacodec.github.io/iamf/v2.0.0.html (quoted section titles refer
to that version).

## Matroska specifications
Matroska Media Container Format Specification (RFC 9559) and the Matroska
codec mappings, https://github.com/ietf-wg-cellar/matroska-specification.

## Related mappings
Matroska Opus mapping (`A_OPUS`: `CodecDelay` from pre-skip, `SeekPreRoll`
80 ms) and AV1 mapping (`codec/av1.md`: OBUs in `CodecPrivate` and
`Blocks`).
