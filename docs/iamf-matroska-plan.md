# IAMF in Matroska — implementation plan for mapping v0.2

Status: plan, 2026-10-12. Written to be executed by a separate session.
Companion to `docs/iamf-matroska-mapping.md` (v0.1, PR mgth/Omniphony#641)
and `scripts/iamf2mka.py`. Nothing in this plan has been listened to on
speakers; every "verified" below means a file comparison.

## 1. Context the executor needs

Upstream state (checked 2026-10-12):

- [matroska-specification#940](https://github.com/ietf-wg-cellar/matroska-specification/issues/940)
  is still open with no draft; our PR #641 is the only cross-reference.
  `draft-ietf-cellar-codec` (-20, -21 pending) is in IESG evaluation; new
  codecs are deferred to the "Matroska v4 Update 1" document, which has no
  date. Once the codec RFC is out, the Matroska Codec IDs registry becomes an
  IANA "Expert Review" registry: a stable public document is enough to
  register `A_IAMF` without waiting for Update 1. That is the realistic route.
- MKVToolNix: one open request, no reply
  ([mbunkus/mkvtoolnix#6148](https://codeberg.org/mbunkus/mkvtoolnix/issues/6148)).
  mkvmerge passes an `A_IAMF` track through untouched (checked with 101).
- FFmpeg: no IAMF in `matroska.c`/`matroskadec.c`/`matroskaenc.c`.
- IAMF spec is at v2.0.0 (2026-08-24), libiamf v2.0.0, iamf-tools v3.0.0.

Our implementation lives in three repos:

| Role | Where | Identity / remote |
|---|---|---|
| Writer (encoder output) | `harlettizer/crates/hz-iamf/src/{stream,matroska}.rs`, `harlettizer/crates/hz-cli/src/iamf_objects.rs` | harletty account, `harletty/harlettizer`; worktrees are `harlettizer-<name>/` beside `harlettizer/` |
| Muxer / extractor (tool) | `workflows/integration/omniphony/scripts/iamf2mka.py` | mgth, `mgth/Omniphony`, PRs with `GH_TOKEN=$(gh auth token --user mgth)` |
| Mapping text | `workflows/integration/omniphony/docs/iamf-matroska-mapping.md` | same |
| Reader | mpv fork `workflows/integration/mpv/demux/demux_mkv.c`, `audio/decode/ad_orender.c` | `mgth/mpv`, branches `orender` **and** `orender-master`; then `mpv-omniphony` patch sets (`scripts/regenerate-patches.sh`, `scripts/regenerate-patches-master.sh`) |
| Decoder | `workflows/integration/harletty-bridge/bridge-family-iamf` (iamf-rs) | harletty, `harletty/harletty-bridge` |

Test material: libiamf vectors in `dumps/eclipsa/aomedia-libiamf-tests/`
(`test_000220.iamf`, `test_000220_s.mp4`, the fMP4 form, and the reference
renders `test_000220_rendered_id_42_sub_mix_0_layout_{0,1}.wav`;
`test_000082` LPCM, `test_000006` temporal delimiters, `test_000116` reserved
OBUs). Tron's IAMF export (AtmosFer) is the real-content case.

Rules that apply throughout: everything in English; branch names generic
(no codec brand names); never `gh pr merge --auto`; never claim a listening
result; rebuild harlettizer with `--features opus` after merging (AtmosFer's
config points at `harlettizer/target/release/harlettizer`); rebuild the
bridge and the engine together if the bridge changes; any mpv fork commit
merged on one branch is carried to the other and both patch sets are
regenerated, or the ranker's FEL mpv misses it.

## 2. Decisions (fixed; do not reopen while implementing)

- **D1 — every Block is a key frame.** The encoder restates every object's
  parameter block in every temporal unit instead of letting a standstill
  block span up to ten seconds. This matches the ISO-BMFF rule that every IA
  Sample is a sync sample, makes Matroska tooling that treats audio blocks as
  independent (mkvmerge `--split`, players that ignore the key flag on
  audio) safe, and removes the ten-second cap that only existed because of
  the decoder drift bug. The mapping keeps the `ReferenceBlock` rule as the
  escape hatch for converted streams, says Blocks **SHOULD** be key frames,
  and forbids lacing.
- **D2 — the container restates the in-band trims.** `CodecDelay` carries
  the start trim (already the case) and `DiscardPadding` on the block(s)
  that carry an end trim restates it. The OBUs are authoritative; a player
  applies exactly one of the two (decoder-side trimming or container-side),
  never both. Our reader (mpv + orender) keeps decoder-side trimming and
  ignores `DiscardPadding` for `iamf`.
- **D3 — unchanged:** single track per IA Sequence (argued with IAMF v2.0's
  own "IA Track" single-track statement); `Channels` = 2, reworded to
  "**MUST** be 2; readers **MUST NOT** rely on it"; `CodecPrivate` = the
  `iacb` box payload (AV1 precedent); one IA Sequence per Segment, with
  `CodecState` named and explained as not used.
- **D4 — text facts to settle from IAMF v2.0:** two Codec Config OBUs must
  share frame size and output sample rate and one of them must be `ipcm`,
  so `SamplingFrequency` is unambiguous; OBU size is capped at 2 MB
  (2^21 bytes) by the v2.0 profiles.
- **D5 — scope:** Matroska only, no WebM claim. No FFmpeg or mkvmerge work in
  this plan (phase 6 lists it as an option for the user to decide).

## 3. Phases

Each phase is one PR per repo. Order matters: phase 1 is a bug fix in
shipped files, phases 2–3 freeze the format, phase 4 produces the samples
from the frozen format.

### Phase 1 — harlettizer: one parameter block per object per unit

Bug being fixed: `longest_still` in `hz-cli/src/iamf_objects.rs` lets a
standstill block cover `⌊(10 s · rate − 1) / frame⌋` units; the Matroska
writer flags every block as a key frame (`matroska.rs`, `block()`) and the
standalone writer never sets `is_not_key_frame` in its temporal delimiters
(`stream.rs`, `write_unit`). A seek into such a span puts the object at its
default position until the next block, up to ten seconds. Files exported by
AtmosFer so far carry this; they need re-exporting after the fix.

Changes:

1. `iamf_objects.rs`: in the standstill branch (around the
   `.min(longest_still(frame, sample_rate))` call), cover exactly one unit:
   `subblocks[0].duration = frame`, `covered_until = unit_index + 1`.
   Remove `longest_still` and `EXACT_SECONDS` with their doc comment (the
   drift they worked around is fixed in iamf-rs since harletty/iamf-rs#1).
   Check the `Placement::Folded` branch (`cluster_subblocks`) already emits
   per-unit blocks; if a folded block can span, cap it the same way.
2. The codec flush: `stream.rs::push` writes a trailing silent unit
   (`write_silent_unit`) with no parameter blocks, covered today by the
   `2 * frame` block appended on `last`. Under D1 that silent unit would be a
   non-key unit. Give it its own block instead: let `write_silent_unit` take
   the per-object `Step(end)` subblocks of duration `frame`, and drop the
   `2 * frame` special case in `iamf_objects.rs`. Invariant to enforce with
   a debug assertion in `write_unit`: every unit carries exactly one block
   per animated object element, and no subblock sum exceeds `frame`.
3. Tests in `iamf_objects.rs`: replace
   `a_standstill_block_stays_inside_what_a_decoder_times_exactly` and
   `a_standstill_knows_when_it_ends` with one test that parses the written
   OBUs and checks the invariant over a sequence with a long standstill, a
   move, and the flush unit. Keep the existing byte-exact tests passing.
4. Size: report `account.blocks_written` and the output size on Tron before
   and after; expect roughly 15–20 bytes per object per unit (about 10 kB/s
   for 13 objects). Note the numbers in the PR.

Verification:

- `cargo test -p hz-iamf -p hz-cli` (never `cargo fmt --all` on this repo).
- Full Tron re-encode (`iamf --objects`, same options AtmosFer uses) then
  decode with the integration `harletty --codec iamf decode` (built
  `--features iamf`): the audio stays byte-exact against `vf.atmos.audio`,
  and the position changes stay at lag 0 on the 256-sample grid (the
  comparison scripts were in a session scratchpad; the reference slices are
  in `dumps/harlettizer/iamf-timing/`; rewrite `lag.py` if needed — it
  compares the decoded position events to the master's events by sample).
- Seek test (new): cut the standalone `.iamf` at a unit boundary inside a
  former standstill span, keep the descriptors in front, decode, and check
  the object is at the right position from the first decoded unit. Before
  the fix this shows the default position.
- `cargo run -- iamf --out x.mka` then `mkvinfo x.mka`: no error, all blocks
  key frames, one cue per cluster.

PR: `harletty/harlettizer`, branch `fix/iamf-parameter-block-per-unit`.
After merge: fast-forward `harlettizer/` and rebuild
`cargo build --release --features opus`.

### Phase 2 — end trim in the container (`DiscardPadding`)

2a. harlettizer `hz-iamf/src/matroska.rs`:

- `block()` takes the unit's end trim in samples. When it is 0, write the
  `SimpleBlock` as today. When it is not, write a `BlockGroup` (`0xA0`)
  holding a `Block` (`0xA1`, same 4-byte header: track vint, 16-bit offset,
  flags 0) and `DiscardPadding` (`0x75A2`, signed integer,
  `end_trim · 1 000 000 000 / rate` ns). Update the module doc (lines
  12–15) which currently says no `DiscardPadding` is written.
- `stream.rs::write_unit` already knows `end`; pass it through the Matroska
  framing path.
- Tests: extend the structural test (around line 537) so the last block is
  a `BlockGroup` whose `DiscardPadding` equals the trim and every other
  block is a `SimpleBlock`; a sequence with no end trim writes no
  `BlockGroup`.

2b. Omniphony `scripts/iamf2mka.py`:

- `mux()`: a `Unit` with `trim_end > 0` is written as a `BlockGroup` +
  `DiscardPadding`, as above. Cues stay on key frames only.
- `extract()`: handle `BlockGroup` (`0xA0`) → `Block` (`0xA1`) the same way
  as `SimpleBlock` (skip the track vint and 3 header bytes). Today it
  silently drops `BlockGroup`s.
- Round trips on the four libiamf vectors (standalone, MP4, fMP4) must stay
  byte-identical to v0.1 behaviour except for the dropped temporal
  delimiters; `mkvinfo` reports no error; `mkvmerge -o y.mka x.mka` keeps
  `DiscardPadding` (check with `mkvinfo -v`).

2c. mpv fork `demux/demux_mkv.c`:

- Where `discardpadding` is applied to the packet (around line 3117), skip
  it when the track codec is `iamf`: the decoder trims in band. Keep the
  `codec_delay` subtraction (line 3039) and the `SeekPreRoll` handling
  (lines 2147–2152). Comment why.
- Commit on `orender`, cherry-pick to `orender-master`, PRs for both, then
  `mpv-omniphony`: regenerate both patch sets, one PR. Build the
  integration mpv (`meson compile -C _build`) and the ranker's FEL build.

2d. Prove the trimming chain (not proven today — the v0.1 null test compared
orender to orender):

- Play `test_000220.mka` (Opus, 312-sample pre-skip) with the fork,
  `--ao=pcm --audio-channels=<layout of the reference wav>`, and compare
  sample alignment against
  `test_000220_rendered_id_42_sub_mix_0_layout_N.wav` (the libiamf reference
  for the matching layout). Alignment within the renderer's known delay
  proves the decoder drops the trimmed samples; a 312-sample offset proves
  it does not, and the fix then belongs in `bridge-family-iamf`/iamf-rs, not
  in the container.
- Check the first audio frame's pts in mpv (`--msg-level=ad_orender=debug`,
  or `--ao=pcm` with `--start` and a visual marker): with decoder-side
  trimming the first kept sample must land at pts 0 after mpv's
  `codec_delay` subtraction. If it lands `CodecDelay` early, shift the
  packet-to-engine pts mapping in `ad_orender.c` (`engine_call` /
  `out_pts_us`) by the start trim read from the first audio frame OBU.
  Record the outcome in the PR either way.

### Phase 3 — mapping text v0.2 (`docs/iamf-matroska-mapping.md`)

Edits, in document order:

1. Header: version 0.2, date, and a short "Registry entry" block in the
   codec-spec house style: `Codec ID: A_IAMF`, `Codec Name: AOMedia
   Immersive Audio Model and Formats`, `Description:` (two sentences),
   `Initialization:` (the `iacb` payload), `Citation:` (IAMF v2.0.0 URL).
2. `CodecPrivate`: keep; drop the **[Open]** about duplicating the profile
   bytes (fixed offsets, not needed). Add: a `configOBUs_size` that does not
   match the element length **MUST** be rejected; the 2 MB OBU cap applies.
3. `SamplingFrequency`: replace the **[Open]** with the v2.0 rule (D4).
4. `Channels`: "**MUST** be 2; readers **MUST NOT** rely on it", keep the
   IAMF-headers rationale, drop the alternatives paragraph.
5. `CodecDelay` / new `DiscardPadding` section per D2: the container
   restates the in-band trims; `DiscardPadding` **SHOULD** be written on the
   blocks whose Audio Frame OBUs carry `num_samples_to_trim_at_end`; the
   OBUs are authoritative; a player **MUST** apply exactly one of the two.
   Replace the **[Open]** with this rule. Block duration still includes the
   trimmed samples (Opus precedent).
6. Timestamps: add that readers reconstruct continuity from decoded sample
   counts and `DefaultDuration`, since frame durations such as 1024 samples
   at 48 kHz (21.333 ms) do not fall on `TimestampScale` ticks.
7. Keyframes: Blocks **SHOULD** be key frames and muxers **SHOULD** restate
   parameter state per temporal unit; the `ReferenceBlock` rule stays for
   streams that cannot be restated. Lacing **MUST NOT** be used (one key
   flag per block). Note the ISO-BMFF text's own contradiction on `stss`
   (SHALL NOT be present vs SHOULD be present) and that our rule matches the
   IA Sample Format section.
8. Segment restrictions: name `CodecState` as the Matroska mechanism for a
   changed initialization and say why it is not used (AV1 precedent, no
   known reader support); keep "new Segment".
9. New "Security considerations" section (the spec repo keeps
   `codec_security.md`; the IANA entry expects one): bounded allocations on
   `obu_size`/`configOBUs_size`, the 2 MB cap, reserved OBUs skipped not
   trusted, substream count from the descriptors bounding what a block may
   contain, and the FFmpeg IAMF parser CVEs as precedent.
10. Implementation notes: update to phases 1–2 (every block key, end trim
    restated, mpv ignores `DiscardPadding`, trimming chain result from 2d).
11. Drop WebM anywhere it is implied; "Matroska" only.

Also update the `A_IAMF` comment in `demux_mkv.c` if its wording no longer
matches. PR: `mgth/Omniphony`, branch `docs/iamf-matroska-mapping-v0-2`,
together with the 2b script change or stacked on it.

### Phase 4 — sample files

With the frozen format, convert the libiamf vectors (`test_000220` in its
three forms, `test_000082`, `test_000006`, `test_000116`) and one short
`harlettizer iamf --objects` clip (no film material, no film name) with
`iamf2mka.py` / harlettizer. For each: `mkvinfo` clean, `mkvmerge` remux
preserves `CodecPrivate`, `CodecDelay`, `SeekPreRoll`, `DiscardPadding`;
`--extract` round trip; fork mpv `--ao=pcm` decode equals the orender
offline render. Publish where the user decides (release asset or a small
dedicated repo); the mapping's implementation notes link to it.

### Phase 5 — upstream

Only after phases 1–4: a comment in matroska-specification#940 pointing at
the v0.2 text, the samples, and the mpv/harlettizer implementations, with a
sentence on each design choice the group is likely to question (single
track, trims, `Channels`). Expect "no third-party support" and "wait for
Update 1 / the registry" as answers; the registry route needs the codec RFC
first. Note: the IESG has raised AI-generated input to Expert Review
registries as a concern (CELLAR minutes 2026-09-22); keep the submission
short and factual.

### Phase 6 — optional, user decision

An FFmpeg `matroskadec.c` arm mapping `A_IAMF` to the existing IAMF reader
(`libavformat/iamf_reader.c`): the one change that makes "supported
somewhere" true outside our forks. Substantial; only worth it if the user
wants to push the registration.

## 4. Checklist per repo

- harlettizer: `cargo test -p hz-iamf -p hz-cli`; Tron re-encode and decode
  comparison; `mkvinfo`; rebuild `--features opus`; re-export existing IAMF
  `.mka` files (seek bug).
- Omniphony: `python3 -I scripts/iamf2mka.py` on the four vectors and
  `--extract` round trips; docs reviewed against the v2.0 spec text quoted
  in this plan.
- mpv fork: both branches, both patch sets, integration `_build` and the
  ranker FEL build; `--ao=pcm` decode of `test_000220.mka` compared to the
  libiamf reference wav.
- Nothing has been listened to; say so in every PR.
