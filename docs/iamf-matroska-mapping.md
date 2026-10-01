# AOM IAMF codec mapping in Matroska — draft

**Status:** draft, written in the Omniphony project as a proposal for
[matroska-specification#940](https://github.com/ietf-wg-cellar/matroska-specification/issues/940).
Not endorsed by AOMedia or the CELLAR working group. Open questions are
marked **[Open]**. Version 0.1, 2026-10-01.

This document specifies the storage of [IAMF](#iamf-specification) (AOMedia
Immersive Audio Model and Formats) in [Matroska](#matroska-specifications)
audio tracks. It follows the structure of the Matroska AV1 mapping (`codec/av1.md`), the
other OBU-based AOMedia format, and stays as close as possible to IAMF's own
[ISO-BMFF encapsulation](#iamf-specification) (IAMF "ISO-BMFF IAMF Encapsulation"), so that a stream can
move between the two containers without touching its OBUs.

Elements in square brackets __[]__ refer to syntax elements of the
[IAMF specification](#iamf-specification).

# Terms

## Block
A Matroska element storing one frame; also a `SimpleBlock` outside a
`BlockGroup`. "Block" in this document means both.

## Descriptors
The OBUs that configure an IA Sequence, in this order: one
`IA Sequence Header OBU`, all `Codec Config OBUs`, all `Audio Element OBUs`,
all `Mix Presentation OBUs`, optionally `Metadata OBUs` after the sequence
header and before the first mix presentation, and possibly `Reserved OBUs`
(IAMF "Descriptor OBUs"). They are what the ISO-BMFF `IAConfigurationBox` carries as
__[configOBUs]__.

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
start within that duration (IAMF "Timing Model"). An IAMF `Temporal Unit` may be
preceded by a `Temporal Delimiter OBU`, whose __[is_not_key_frame]__ flag
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
`IAConfigurationBox` (IAMF "IA Configuration Box"), i.e. the box without its size and type:

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
* Bytes after `configOBUs` are reserved for future versions and **MUST** be
  ignored by readers.

Reusing the box payload rather than bare OBUs keeps a version byte for
future extension and makes conversion from ISO-BMFF a copy.

**[Open]** The AV1 mapping prefixes its `CodecPrivate` with a 4-byte record
whose fields duplicate the sequence header (profile, level) for readers that
do not parse OBUs. The IAMF equivalent would be __[primary_profile]__ and
__[additional_profile]__, both already at fixed offsets in the sequence
header (`configOBUs[6]` and `configOBUs[7]` when the header's size fits in one
leb128 byte). This draft does not duplicate them.

## SamplingFrequency
EBML Path: `\Segment\Tracks\TrackEntry\Audio\SamplingFrequency` | Mandatory: Yes

The `SamplingFrequency` **MUST** be the sample rate IAMF uses for computing
offsets with the track's codec (IAMF "Codec Specific"): 48000 for `Opus`; the
__[samplingFrequencyIndex]__ rate for `mp4a`; the `STREAMINFO` sample rate for
`fLaC`; __[sample_rate]__ for `ipcm`. When the Descriptors carry two
`Codec Config OBUs`, it **MUST** be that of the first one.

**[Open]** IAMF profiles allow two codec configurations in one IA Sequence;
whether they may differ in sample rate in a Matroska track needs settling.

## Channels
EBML Path: `\Segment\Tracks\TrackEntry\Audio\Channels` | Mandatory: Yes

An IA Sequence has no single channel count: its audio elements are rendered
to whatever layout the playback system has. Matroska does not allow 0 (the
value ISO-BMFF uses), so the `Channels` **SHOULD** be 2 and **MUST** be ignored
by readers. This mirrors IAMF's own convention for the codec headers it
embeds (Opus __[Output Channel Count]__ and AAC __[channelConfiguration]__ are
set to 2 and ignored).

**[Open]** Alternatives: the channel count of the first mix presentation's
highest loudspeaker layout, or the total number of coded channels. Neither is
what a player outputs.

# Codec delay, trimming and seeking

## CodecDelay
EBML Path: `\Segment\Tracks\TrackEntry\CodecDelay` | Mandatory: Yes (default 0)

IAMF carries its trimming in-band: `Audio Frame OBUs` signal
__[num_samples_to_trim_at_start]__ and __[num_samples_to_trim_at_end]__, and an
IAMF decoder discards those samples itself. This is exactly what `CodecDelay`
describes ("the number of codec samples that will be discarded by the
decoder").

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

`DiscardPadding` asks the player to drop samples the decoder already drops
from the in-band end trim. To avoid trimming twice, `DiscardPadding`
**SHOULD NOT** be used in an IAMF track; the end trim stays in the
`Audio Frame OBUs`. A reader that finds one **MUST NOT** apply it on top of the
decoder's own trimming.

**[Open]** ISO-BMFF signals the same trims a second time in the edit list and
lets either the decoder or the player apply them (IAMF "Handling Trimming Information"). A Matroska
reader could likewise be told to configure its IAMF decoder not to trim and
apply `CodecDelay`/`DiscardPadding` itself. This draft keeps one owner, the
decoder, which is how IAMF decoders behave by default.

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
  __[num_samples_per_frame]__ samples, trimmed samples included: trimming is
  the decoder's (see [DiscardPadding](#discardpadding)). Muxers **SHOULD**
  set the `Segment` `Duration` to the presentation duration, after trims.

Lacing **MAY** be used; each laced frame is then one Temporal Unit.

## Keyframes
A Temporal Unit is a key frame unless IAMF marks it otherwise
(__[is_not_key_frame]__ in its `Temporal Delimiter OBU`, used when a
`Parameter Block OBU` spans several units).

* A `SimpleBlock` **MUST** be marked as a keyframe if and only if its Temporal
  Unit is a key frame.
* A `Block` inside a `BlockGroup` **MUST** carry a `ReferenceBlock` if its
  Temporal Unit is not a key frame, referencing the `Block` that holds the
  `Parameter Block OBUs` it depends on.

Codec pre-roll (`SeekPreRoll`) is independent of this flag, as for every audio
codec in Matroska: a keyframe is a point the IAMF parameter state can restart
from, not one the codec state can.

## DefaultDuration
EBML Path: `\Segment\Tracks\TrackEntry\DefaultDuration`

The `DefaultDuration` **SHOULD** be set to
`num_samples_per_frame * 1,000,000,000 / SamplingFrequency` (20,000,000 for
20 ms Opus frames).

# Segment restrictions

The Descriptors of an IA Sequence are fixed for its whole duration, and a
Matroska `CodecPrivate` for the whole `Segment`. A track **MUST** therefore
hold a single IA Sequence: an IAMF configuration change (IAMF "IAMF Configuration Changes"), which
starts a new IA Sequence with new non-redundant Descriptors, **MUST** start a
new `Segment` (for example a linked or chained segment).

**[Open]** ISO-BMFF allows several sample entries in one track for this.
Matroska could allow a `Block` that starts a new IA Sequence to carry the new
non-redundant Descriptors in-band (keyframe, `Sequence Header OBU` first),
which a decoder handles natively. This draft forbids it for simplicity,
like the AV1 mapping forbids sequence header changes.

# Track language

An IA Sequence may mix audio elements in several languages. As ISO-BMFF
recommends, the `Language` (or `LanguageBCP47`) **SHOULD** be `mul` when
several languages are present and `und` when none applies.

# Cue considerations

Only `Blocks` marked as keyframes (or `BlockGroups` without `ReferenceBlock`)
**SHOULD** be referenced in the `Cues`. A seek lands `SeekPreRoll` before its
target and decodes forward, as for Opus.

# Encryption

**[Open]** Not defined in this draft. ISO-BMFF protects IAMF with full-sample
`cenc` or whole-block `cbcs`; a Matroska equivalent would follow the WebM
encryption scheme with whole-`Block` encryption, keeping OBU headers in the
clear if partial encryption is wanted.

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
| edit list end trim | nothing: kept in-band |
| `roll` sample group | `SeekPreRoll` |
| non-sync samples (`stss`, `sample_is_non_sync_sample`) | `SimpleBlock` keyframe flag cleared / `ReferenceBlock` |
| `mdhd` language `mul`/`und` | `Language` `mul`/`und` |
| several sample entries | several `Segments` (**[Open]**) |

# Implementation notes (informative, Omniphony)

These are not part of the mapping. In the Omniphony stack:

* **Reading:** mpv demuxes Matroska itself (`demux_mkv`), not through
  FFmpeg, so mpv-omniphony needs only a `A_IAMF` → `iamf` codec entry: the
  `Blocks` then reach `ad_orender` unchanged, with the `CodecPrivate`'s
  `configOBUs` passed first. harletty-bridge's IAMF pipeline
  (harletty/harletty-bridge#79) already accepts exactly that: Descriptors,
  then Temporal Units; a reset (seek) keeps the configuration.
* **Writing:** neither mkvmerge nor FFmpeg can produce such a track today
  (FFmpeg exposes IAMF as one stream per substream plus a stream group, not
  as one track). `scripts/iamf2mka.py` writes it from IAMF in ISO-BMFF
  (fragmented or not) or from a standalone `.iamf`, and `--extract` reads it
  back to a standalone stream. Checked on the libiamf vectors and a YouTube
  capture: `mkvinfo` reports no error; from ISO-BMFF and from standalone
  streams without Temporal Delimiters the round trip is byte-identical; with
  delimiters only those are dropped, and the decoded PCM is identical.
* **Test material:** the libiamf conformance vectors (AOMediaCodec/libiamf
  `tests/`, standalone `.iamf` and ISO-BMFF forms with reference renders)
  convert directly.

# Referenced documents

## IAMF specification
Immersive Audio Model and Formats, AOMedia, https://aomediacodec.github.io/iamf/
(quoted section titles refer to the August 2026 draft).

## Matroska specifications
Matroska Media Container Format Specification (RFC 9559) and the Matroska
codec mappings, https://github.com/ietf-wg-cellar/matroska-specification.

## Related mappings
Matroska Opus mapping (`A_OPUS`: `CodecDelay` from pre-skip, `SeekPreRoll`
80 ms) and AV1 mapping (`codec/av1.md`: OBUs in `CodecPrivate` and
`Blocks`).
