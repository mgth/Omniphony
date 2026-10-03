#!/usr/bin/env python3
"""Mux IAMF into Matroska (A_IAMF), following docs/iamf-matroska-mapping.md.

    iamf2mka.py INPUT OUTPUT.mka      INPUT: IAMF in MP4 (fragmented or not)
                                      or a standalone .iamf OBU stream
    iamf2mka.py --extract IN.mka OUT.iamf
                                      the standalone stream back: CodecPrivate
                                      configOBUs, then every Block

The track: CodecID A_IAMF; CodecPrivate = IAConfigurationBox payload
(version 1, leb128 size, configOBUs); one temporal unit per SimpleBlock,
without Temporal Delimiter or Descriptor OBUs, keyframe flag from
is_not_key_frame; CodecDelay = the in-band start trim; SeekPreRoll from
audio_roll_distance; DefaultDuration = one frame; Channels 2 (ignored).

Only the standard library. Holds one cluster at a time in memory; the output
must be seekable (sizes and the seek head are patched at the end).
"""

import argparse
import os
import struct
import sys

# ── IAMF OBUs ────────────────────────────────────────────────────────────────

OBU_CODEC_CONFIG, OBU_AUDIO_ELEMENT, OBU_MIX_PRESENTATION = 0, 1, 2
OBU_PARAMETER_BLOCK, OBU_TEMPORAL_DELIMITER = 3, 4
OBU_METADATA, OBU_SEQUENCE_HEADER = 24, 31
DESCRIPTOR_TYPES = {OBU_SEQUENCE_HEADER, OBU_CODEC_CONFIG, OBU_AUDIO_ELEMENT, OBU_MIX_PRESENTATION}


def is_audio_frame(obu_type):
    return 5 <= obu_type <= 23


def leb128(data, at):
    value = shift = 0
    for i in range(8):
        byte = data[at + i]
        value |= (byte & 0x7F) << shift
        shift += 7
        if not byte & 0x80:
            return value, at + i + 1
    raise ValueError(f"leb128 longer than 8 bytes at {at}")


def leb128_bytes(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        out.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(out)


class Obu:
    __slots__ = ("type", "redundant", "flag", "start", "payload", "end", "trim_end", "trim_start")

    def __init__(self, data, at):
        header = data[at]
        self.type = header >> 3
        self.redundant = bool(header & 0x04)
        # Type-dependent bit: trimming flag (audio frames), is_not_key_frame
        # (temporal delimiter), optional_fields_flag (mix presentation).
        self.flag = bool(header & 0x02)
        extension = bool(header & 0x01)
        size, body = leb128(data, at + 1)
        self.start, self.end = at, body + size
        if self.end > len(data):
            raise ValueError(f"OBU at {at} runs past the end of the data")
        self.trim_end = self.trim_start = 0
        if is_audio_frame(self.type) and self.flag:
            self.trim_end, body = leb128(data, body)
            self.trim_start, body = leb128(data, body)
        if extension:
            ext, body = leb128(data, body)
            body += ext
        self.payload = body


def obus(data, start=0, end=None):
    end = len(data) if end is None else end
    at = start
    while at < end:
        obu = Obu(data, at)
        yield obu
        at = obu.end


class Config:
    """What the Matroska track needs from the descriptors."""

    def __init__(self, config_obus):
        self.config_obus = bytes(config_obus)
        self.sample_rate = None
        self.frame_size = None
        self.roll_distance = 0
        self.bit_depth = None
        self.pre_skip = None
        self.codec = None
        self.substreams = 0
        for obu in obus(config_obus):
            if obu.type == OBU_CODEC_CONFIG and self.codec is None:
                self._codec_config(config_obus, obu.payload)
            elif obu.type == OBU_AUDIO_ELEMENT:
                _, at = leb128(config_obus, obu.payload)  # audio_element_id
                at += 1  # audio_element_type, reserved
                _, at = leb128(config_obus, at)  # codec_config_id
                count, _ = leb128(config_obus, at)
                self.substreams += count
        if self.sample_rate is None or not self.substreams:
            raise ValueError("descriptors without a usable codec config or audio element")

    def _codec_config(self, data, at):
        _, at = leb128(data, at)  # codec_config_id
        self.codec = bytes(data[at:at + 4]).decode("latin1")
        at += 4
        self.frame_size, at = leb128(data, at)
        (self.roll_distance,) = struct.unpack(">h", data[at:at + 2])
        at += 2
        if self.codec == "Opus":
            # RFC 7845 ID header without its magic, big-endian: version,
            # output channel count, pre-skip, input sample rate, ...
            (self.pre_skip,) = struct.unpack(">H", data[at + 2:at + 4])
            self.sample_rate = 48000
        elif self.codec == "ipcm":
            self.bit_depth = data[at + 1]
            (self.sample_rate,) = struct.unpack(">I", data[at + 2:at + 6])
        elif self.codec == "fLaC":
            # First metadata block: 4-byte block header, then STREAMINFO;
            # the sample rate is 20 bits at byte 10, bits per sample next.
            si = at + 4
            bits = int.from_bytes(data[si + 10:si + 14], "big")
            self.sample_rate = bits >> 12
            self.bit_depth = ((bits >> 4) & 0x1F) + 1
        elif self.codec == "mp4a":
            self.sample_rate = aac_sample_rate(data, at)
        else:
            raise ValueError(f"unsupported codec_id {self.codec!r}")


AAC_RATES = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350]


def aac_sample_rate(data, at):
    # DecoderConfigDescriptor (tag 4): objectTypeIndication, streamType...,
    # then DecoderSpecificInfo (tag 5) = AudioSpecificConfig.
    def descriptor(at):
        tag = data[at]
        at += 1
        size = 0
        for _ in range(4):
            byte = data[at]
            at += 1
            size = (size << 7) | (byte & 0x7F)
            if not byte & 0x80:
                break
        return tag, at, size

    tag, body, _ = descriptor(at)
    if tag != 4:
        raise ValueError("AAC decoder config without DecoderConfigDescriptor")
    tag, asc, _ = descriptor(body + 13)
    if tag != 5:
        raise ValueError("AAC decoder config without AudioSpecificConfig")
    index = ((data[asc] & 0x07) << 1) | (data[asc + 1] >> 7)
    return AAC_RATES[index]


class Unit:
    __slots__ = ("data", "keyframe", "trim_start", "trim_end")

    def __init__(self, data, keyframe, trim_start, trim_end):
        self.data, self.keyframe = data, keyframe
        self.trim_start, self.trim_end = trim_start, trim_end


def unit_from_obus(data, start, end, keyframe):
    """A Block's data: the unit's OBUs without delimiters or descriptors."""
    parts = []
    trim_start = trim_end = None
    for obu in obus(data, start, end):
        if obu.type == OBU_TEMPORAL_DELIMITER or obu.type in DESCRIPTOR_TYPES:
            continue
        if is_audio_frame(obu.type) and trim_start is None:
            trim_start, trim_end = obu.trim_start, obu.trim_end
        parts.append(data[obu.start:obu.end])
    return Unit(b"".join(parts), keyframe, trim_start or 0, trim_end or 0)


# ── Inputs ───────────────────────────────────────────────────────────────────


def read_standalone(data):
    """A standalone IA Sequence → (Config, units)."""
    it = obus(data)
    first = next(it)
    if first.type != OBU_SEQUENCE_HEADER:
        raise ValueError("a standalone stream must start with its sequence header")
    at = first.end
    for obu in obus(data, at):
        if obu.type not in DESCRIPTOR_TYPES and obu.type != OBU_METADATA and not 25 <= obu.type <= 30:
            break
        at = obu.end
    config = Config(data[:at])
    units = []
    start = at
    frames = 0
    keyframe = True
    pending_td = False
    for obu in obus(data, at):
        if obu.type == OBU_SEQUENCE_HEADER and not obu.redundant:
            raise ValueError("configuration change: the mapping holds one IA Sequence per Segment")
        if obu.type == OBU_TEMPORAL_DELIMITER:
            if frames:
                raise ValueError(f"temporal delimiter inside a temporal unit at {obu.start}")
            keyframe = not obu.flag
            pending_td = True
            continue
        if is_audio_frame(obu.type):
            frames += 1
            if frames == config.substreams:
                units.append(unit_from_obus(data, start, obu.end, keyframe))
                start, frames, keyframe = obu.end, 0, True
                pending_td = False
    if frames or pending_td:
        raise ValueError("stream ends inside a temporal unit")
    return config, units


def boxes(data, start=0, end=None):
    end = len(data) if end is None else end
    at = start
    while at + 8 <= end:
        size, kind = struct.unpack(">I4s", data[at:at + 8])
        header = 8
        if size == 1:
            (size,) = struct.unpack(">Q", data[at + 8:at + 16])
            header = 16
        elif size == 0:
            size = end - at
        if size < header or at + size > end:
            return
        yield kind.decode("latin1"), at, at + header, at + size
        at += size


CONTAINERS = {"moov", "trak", "mdia", "minf", "stbl", "edts", "dinf", "mvex", "moof", "traf"}


def find(data, start, end, kind):
    for k, s, p, e in boxes(data, start, end):
        if k == kind:
            return s, p, e
        if k in CONTAINERS:
            found = find(data, p, e, kind)
            if found:
                return found
    return None


def read_mp4(data):
    """IAMF in ISO-BMFF → (Config, units)."""
    moov = find(data, 0, len(data), "moov")
    if not moov:
        raise ValueError("no moov box")
    stsd = find(data, moov[1], moov[2], "stsd")
    entry = next((b for b in boxes(data, stsd[1] + 8, stsd[2]) if b[0] == "iamf"), None)
    if not entry:
        raise ValueError("no iamf sample entry")
    iacb = next((b for b in boxes(data, entry[2] + 28, entry[3]) if b[0] == "iacb"), None)
    if not iacb or data[iacb[2]] != 1:
        raise ValueError("no IAConfigurationBox version 1")
    size, at = leb128(data, iacb[2] + 1)
    config = Config(data[at:at + size])

    samples = []  # (offset, size, sync)
    stbl = find(data, moov[1], moov[2], "stbl")
    stsz = find(data, stbl[1], stbl[2], "stsz")
    if stsz:
        samples += plain_samples(data, stbl, stsz)
    trex_size = trex_default(data, moov)
    for kind, s, p, e in boxes(data):
        if kind == "moof":
            # A fragment that puts its samples in a 'roll' sample group marks
            # them non-sync for the codec's pre-roll, not because the IAMF
            # unit is not a key frame (IAMF wants every IA Sample sync).
            # Matroska carries that pre-roll as SeekPreRoll instead.
            roll = any(kind == "sgpd" and data[bp + 4:bp + 8] == b"roll"
                       for kind, _, bp, _ in boxes(data, *find(data, p, e, "traf")[1:]))
            samples += [(o, n, sync or roll) for o, n, sync in fragment_samples(data, s, p, e, trex_size)]
    units = []
    for offset, length, sync in samples:
        units.append(unit_from_obus(data, offset, offset + length, sync))
    return config, units


def plain_samples(data, stbl, stsz):
    _, p, _ = stsz
    fixed, count = struct.unpack(">II", data[p + 4:p + 12])
    sizes = [fixed] * count if fixed else list(struct.unpack(f">{count}I", data[p + 12:p + 12 + 4 * count]))
    if not count:
        return []
    stco = find(data, stbl[1], stbl[2], "stco")
    if stco:
        (n,) = struct.unpack(">I", data[stco[1] + 4:stco[1] + 8])
        offsets = struct.unpack(f">{n}I", data[stco[1] + 8:stco[1] + 8 + 4 * n])
    else:
        co64 = find(data, stbl[1], stbl[2], "co64")
        (n,) = struct.unpack(">I", data[co64[1] + 4:co64[1] + 8])
        offsets = struct.unpack(f">{n}Q", data[co64[1] + 8:co64[1] + 8 + 8 * n])
    stsc = find(data, stbl[1], stbl[2], "stsc")
    (n_entries,) = struct.unpack(">I", data[stsc[1] + 4:stsc[1] + 8])
    entries = [struct.unpack(">III", data[stsc[1] + 8 + 12 * i:stsc[1] + 20 + 12 * i]) for i in range(n_entries)]
    stss = find(data, stbl[1], stbl[2], "stss")
    sync = None
    if stss:
        (n,) = struct.unpack(">I", data[stss[1] + 4:stss[1] + 8])
        sync = set(struct.unpack(f">{n}I", data[stss[1] + 8:stss[1] + 8 + 4 * n]))
    out = []
    index = 0
    for chunk, offset in enumerate(offsets, start=1):
        per_chunk = next(spc for first, spc, _ in reversed(entries) if first <= chunk)
        for _ in range(per_chunk):
            if index >= count:
                break
            out.append((offset, sizes[index], sync is None or index + 1 in sync))
            offset += sizes[index]
            index += 1
    return out


def trex_default(data, moov):
    trex = find(data, moov[1], moov[2], "trex")
    return struct.unpack(">I", data[trex[1] + 16:trex[1] + 20])[0] if trex else 0


def fragment_samples(data, moof_start, p, e, default_size):
    traf = find(data, p, e, "traf")
    out = []
    flags_default = 0
    for kind, s, bp, be in boxes(data, traf[1], traf[2]):
        flags = int.from_bytes(data[bp + 1:bp + 4], "big")
        if kind == "tfhd":
            if flags & 0x1:
                raise ValueError("tfhd base_data_offset is not supported")
            at = bp + 8
            if flags & 0x2:
                at += 4
            if flags & 0x8:
                at += 4
            if flags & 0x10:
                (default_size,) = struct.unpack(">I", data[at:at + 4])
                at += 4
            if flags & 0x20:
                (flags_default,) = struct.unpack(">I", data[at:at + 4])
        elif kind == "trun":
            (count,) = struct.unpack(">I", data[bp + 4:bp + 8])
            at = bp + 8
            offset = moof_start
            if flags & 0x1:
                (rel,) = struct.unpack(">i", data[at:at + 4])
                offset += rel
                at += 4
            first_flags = None
            if flags & 0x4:
                (first_flags,) = struct.unpack(">I", data[at:at + 4])
                at += 4
            for i in range(count):
                size = default_size
                sample_flags = first_flags if (i == 0 and first_flags is not None) else flags_default
                if flags & 0x100:
                    at += 4
                if flags & 0x200:
                    (size,) = struct.unpack(">I", data[at:at + 4])
                    at += 4
                if flags & 0x400:
                    (sample_flags,) = struct.unpack(">I", data[at:at + 4])
                    at += 4
                if flags & 0x800:
                    at += 4
                # sample_is_non_sync_sample
                out.append((offset, size, not sample_flags & 0x10000))
                offset += size
    return out


# ── Matroska writer ──────────────────────────────────────────────────────────

EBML, EBML_VERSION, EBML_READ_VERSION = 0x1A45DFA3, 0x4286, 0x42F7
EBML_MAX_ID, EBML_MAX_SIZE, DOCTYPE = 0x42F2, 0x42F3, 0x4282
DOCTYPE_VERSION, DOCTYPE_READ_VERSION = 0x4287, 0x4285
SEGMENT, SEEK_HEAD, SEEK, SEEK_ID, SEEK_POSITION = 0x18538067, 0x114D9B74, 0x4DBB, 0x53AB, 0x53AC
INFO, TIMESTAMP_SCALE, DURATION, MUXING_APP, WRITING_APP = 0x1549A966, 0x2AD7B1, 0x4489, 0x4D80, 0x5741
TRACKS, TRACK_ENTRY, TRACK_NUMBER, TRACK_UID, TRACK_TYPE = 0x1654AE6B, 0xAE, 0xD7, 0x73C5, 0x83
FLAG_LACING, LANGUAGE, CODEC_ID, CODEC_PRIVATE = 0x9C, 0x22B59C, 0x86, 0x63A2
CODEC_DELAY, SEEK_PRE_ROLL, DEFAULT_DURATION = 0x56AA, 0x56BB, 0x23E383
AUDIO, SAMPLING_FREQUENCY, CHANNELS, BIT_DEPTH = 0xE1, 0xB5, 0x9F, 0x6264
CLUSTER, CLUSTER_TIMESTAMP, SIMPLE_BLOCK = 0x1F43B675, 0xE7, 0xA3
CUES, CUE_POINT, CUE_TIME, CUE_TRACK_POSITIONS = 0x1C53BB6B, 0xBB, 0xB3, 0xB7
CUE_TRACK, CUE_CLUSTER_POSITION, VOID = 0xF7, 0xF1, 0xEC

TIMESTAMP_SCALE_NS = 1_000_000  # 1 ms ticks for Block timestamps
CLUSTER_MS = 5000


def ebml_id(value):
    return value.to_bytes((value.bit_length() + 7) // 8, "big")


def ebml_size(n, width=None):
    if width is None:
        width = 1
        while n >= (1 << (7 * width)) - 1:
            width += 1
    return ((1 << (7 * width)) | n).to_bytes(width, "big")


def element(eid, payload):
    return ebml_id(eid) + ebml_size(len(payload)) + payload


def uint(eid, value):
    return element(eid, value.to_bytes(max(1, (value.bit_length() + 7) // 8), "big"))


def flt(eid, value):
    return element(eid, struct.pack(">d", value))


def text(eid, value):
    return element(eid, value.encode())


def mux(config, units, out_path):
    sr = config.sample_rate
    frame_ns = config.frame_size * 1_000_000_000 // sr
    # CodecDelay: the in-band start trim (samples the decoder drops).
    trimmed = 0
    for unit in units:
        trimmed += unit.trim_start
        if unit.trim_start < config.frame_size:
            break
    codec_delay = trimmed * 1_000_000_000 // sr
    seek_preroll = -config.roll_distance * config.frame_size * 1_000_000_000 // sr
    total = len(units) * config.frame_size
    end_trim = units[-1].trim_end if units else 0
    duration_ms = (total - trimmed - end_trim) * 1000 / sr

    codec_private = b"\x01" + leb128_bytes(len(config.config_obus)) + config.config_obus
    audio = flt(SAMPLING_FREQUENCY, float(sr)) + uint(CHANNELS, 2)
    if config.bit_depth:
        audio += uint(BIT_DEPTH, config.bit_depth)
    track = element(TRACK_ENTRY, b"".join([
        uint(TRACK_NUMBER, 1),
        uint(TRACK_UID, int.from_bytes(os.urandom(7), "big") | 1),
        uint(TRACK_TYPE, 2),
        uint(FLAG_LACING, 0),
        text(LANGUAGE, "und"),
        text(CODEC_ID, "A_IAMF"),
        element(CODEC_PRIVATE, codec_private),
        uint(CODEC_DELAY, codec_delay),
        uint(SEEK_PRE_ROLL, seek_preroll),
        uint(DEFAULT_DURATION, frame_ns),
        element(AUDIO, audio),
    ]))
    tracks = element(TRACKS, track)
    info = element(INFO, uint(TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS) + flt(DURATION, duration_ms)
                   + text(MUXING_APP, "omniphony iamf2mka") + text(WRITING_APP, "omniphony iamf2mka"))

    header = element(EBML, b"".join([
        uint(EBML_VERSION, 1), uint(EBML_READ_VERSION, 1), uint(EBML_MAX_ID, 4), uint(EBML_MAX_SIZE, 8),
        text(DOCTYPE, "matroska"), uint(DOCTYPE_VERSION, 4), uint(DOCTYPE_READ_VERSION, 2),
    ]))
    with open(out_path, "wb") as f:
        f.write(header)
        f.write(ebml_id(SEGMENT) + ebml_size(0, 8))  # size patched at the end
        segment_data = f.tell()
        seek_head_at = f.tell()
        f.write(element(VOID, bytes(80)))  # room for the seek head
        info_at = f.tell() - segment_data
        f.write(info)
        tracks_at = f.tell() - segment_data
        f.write(tracks)

        cues = []
        cluster = []
        cluster_ms = None
        for index, unit in enumerate(units):
            ms = round(index * config.frame_size * 1000 / sr)
            # Clusters start on keyframes, the only blocks the Cues point at;
            # a run of non-keyframes still gets cut before the 16-bit block
            # timestamp could overflow.
            age = None if cluster_ms is None else ms - cluster_ms
            if age is None or (unit.keyframe and age >= CLUSTER_MS) or age >= 30_000:
                if cluster:
                    f.write(element(CLUSTER, b"".join(cluster)))
                cluster_ms = ms
                if unit.keyframe:
                    cues.append((ms, f.tell() - segment_data))
                cluster = [uint(CLUSTER_TIMESTAMP, ms)]
            block = b"\x81" + struct.pack(">h", ms - cluster_ms) + bytes([0x80 if unit.keyframe else 0]) + unit.data
            cluster.append(element(SIMPLE_BLOCK, block))
        if cluster:
            f.write(element(CLUSTER, b"".join(cluster)))

        cues_at = f.tell() - segment_data
        f.write(element(CUES, b"".join(
            element(CUE_POINT, uint(CUE_TIME, ms) + element(CUE_TRACK_POSITIONS,
                    uint(CUE_TRACK, 1) + uint(CUE_CLUSTER_POSITION, pos)))
            for ms, pos in cues)))
        end = f.tell()

        seek_head = element(SEEK_HEAD, b"".join(
            element(SEEK, element(SEEK_ID, ebml_id(eid)) + uint(SEEK_POSITION, pos))
            for eid, pos in ((INFO, info_at), (TRACKS, tracks_at), (CUES, cues_at))))
        room = 80 + 2  # the Void element written above: id + 1-byte size + 80
        if len(seek_head) + 2 > room:
            raise RuntimeError("seek head larger than the room reserved for it")
        f.seek(seek_head_at)
        f.write(seek_head + element(VOID, bytes(room - len(seek_head) - 2)))
        f.seek(segment_data - 8)
        f.write(ebml_size(end - segment_data, 8))
    return {
        "units": len(units), "sample_rate": sr, "codec": config.codec,
        "codec_delay_ns": codec_delay, "seek_preroll_ns": seek_preroll,
        "default_duration_ns": frame_ns, "duration_s": duration_ms / 1000,
        "non_keyframes": sum(1 for u in units if not u.keyframe), "cue_points": len(cues),
    }


# ── Matroska reader (for --extract) ─────────────────────────────────────────


def read_vint(data, at, mask_marker=True):
    first = data[at]
    width = 1
    while width <= 8 and not first & (0x80 >> (width - 1)):
        width += 1
    value = int.from_bytes(data[at:at + width], "big")
    if mask_marker:
        value &= (1 << (7 * width)) - 1
    return value, at + width


def elements(data, start, end):
    at = start
    while at < end:
        eid, at2 = read_vint(data, at, mask_marker=False)
        size, body = read_vint(data, at2)
        if size == (1 << (7 * (body - at2))) - 1:  # unknown size
            size = end - body
        yield eid, body, min(body + size, end)
        at = body + size


def extract(mka):
    data = open(mka, "rb").read()
    out = bytearray()
    for eid, body, end in elements(data, 0, len(data)):
        if eid != SEGMENT:
            continue
        for sid, sbody, send in elements(data, body, end):
            if sid == TRACKS:
                for _, tbody, tend in elements(data, sbody, send):
                    fields = {i: data[b:e] for i, b, e in elements(data, tbody, tend)}
                    if fields.get(CODEC_ID) != b"A_IAMF":
                        raise ValueError("not an A_IAMF track")
                    private = fields[CODEC_PRIVATE]
                    if private[0] != 1:
                        raise ValueError("unknown CodecPrivate version")
                    size, at = leb128(private, 1)
                    out += private[at:at + size]
            elif sid == CLUSTER:
                for cid, cbody, cend in elements(data, sbody, send):
                    if cid == SIMPLE_BLOCK:
                        _, at = read_vint(data, cbody)
                        out += data[at + 3:cend]
    return bytes(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--extract", action="store_true", help="read an A_IAMF .mka back to a standalone .iamf")
    ap.add_argument("input")
    ap.add_argument("output")
    args = ap.parse_args()
    if args.extract:
        open(args.output, "wb").write(extract(args.input))
        return
    data = open(args.input, "rb").read()
    is_mp4 = len(data) >= 8 and data[4:8] in (b"ftyp", b"styp", b"moov")
    config, units = read_mp4(data) if is_mp4 else read_standalone(data)
    if not units:
        sys.exit("no temporal units")
    stats = mux(config, units, args.output)
    print(", ".join(f"{k}={v}" for k, v in stats.items()))


if __name__ == "__main__":
    main()
