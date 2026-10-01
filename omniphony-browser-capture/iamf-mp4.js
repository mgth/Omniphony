// IAMF in ISO-BMFF (MP4) → raw IAMF OBU stream, as orender's bridge reads it.
//
// An IAMF track carries its descriptor OBUs in the sample entry's `iacb` box
// (IAMF §6.2.3) and one temporal unit per sample in `mdat`. The raw stream
// is therefore the descriptors followed by the sample data, in order. A
// capture is a byte stream of top-level boxes (ftyp, moov, then moof/mdat
// pairs, possibly a later moov when the player re-initialises); every moov
// whose descriptors differ from the last ones emitted starts a new IA
// sequence in the output.
//
// Classic script: loaded in the page by the extension and imported by the
// Node tests; it only defines `globalThis.OmniphonyIamfMp4`.

(() => {
  'use strict';

  // Boxes whose payload is only child boxes.
  const CONTAINERS = new Set(['moov', 'trak', 'mdia', 'minf', 'stbl', 'edts', 'dinf', 'mvex']);
  // Bytes before the child boxes, by box type: stsd is a full box with an
  // entry count, an audio sample entry has 28 bytes of fixed fields.
  const CHILD_OFFSET = { stsd: 8, iamf: 28 };

  function fourcc(bytes, at) {
    return String.fromCharCode(bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]);
  }

  // The boxes in bytes[start, end): { type, start, payload, end }. Stops at a
  // box that runs past `end` (an incomplete capture tail).
  function* boxes(bytes, start = 0, end = bytes.length) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    let at = start;
    while (at + 8 <= end) {
      let size = view.getUint32(at);
      const type = fourcc(bytes, at + 4);
      let header = 8;
      if (size === 1) {
        if (at + 16 > end) return;
        size = Number(view.getBigUint64(at + 8));
        header = 16;
      } else if (size === 0) {
        size = end - at;
      }
      if (size < header || at + size > end) return;
      yield { type, start: at, payload: at + header, end: at + size };
      at += size;
    }
  }

  // The descriptor OBUs of the first IAMF sample entry under `moov`, or null.
  function configObus(bytes, moov) {
    const search = (start, end) => {
      for (const box of boxes(bytes, start, end)) {
        if (box.type === 'iacb') return iacbObus(bytes, box);
        if (CONTAINERS.has(box.type) || box.type in CHILD_OFFSET) {
          const found = search(box.payload + (CHILD_OFFSET[box.type] || 0), box.end);
          if (found) return found;
        }
      }
      return null;
    };
    return search(moov.payload, moov.end);
  }

  // IAConfigurationBox: configurationVersion (u8), configOBUs_size (leb128),
  // configOBUs.
  function iacbObus(bytes, box) {
    let at = box.payload + 1;
    let size = 0;
    for (let shift = 0; shift < 56; shift += 7) {
      const byte = bytes[at++];
      size += (byte & 0x7f) * 2 ** shift;
      if (!(byte & 0x80)) break;
    }
    return at + size <= box.end ? bytes.subarray(at, at + size) : null;
  }

  function sameBytes(a, b) {
    if (!a || !b || a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
    return true;
  }

  // Convert a captured MP4 byte stream to a raw IAMF stream. Returns
  // { iamf: Uint8Array, sequences, samplesBytes }.
  function toIamf(bytes) {
    const parts = [];
    let last = null;
    let sequences = 0;
    let samplesBytes = 0;
    // A non-fragmented file may put its mdat before its moov: the first
    // descriptors open the stream wherever they sit.
    for (const box of boxes(bytes)) {
      if (box.type !== 'moov') continue;
      last = configObus(bytes, box);
      if (last) {
        parts.push(last);
        sequences++;
        break;
      }
    }
    for (const box of boxes(bytes)) {
      if (box.type === 'moov') {
        const obus = configObus(bytes, box);
        if (obus && !sameBytes(obus, last)) {
          parts.push(obus);
          last = obus;
          sequences++;
        }
      } else if (box.type === 'mdat' && last) {
        parts.push(bytes.subarray(box.payload, box.end));
        samplesBytes += box.end - box.payload;
      }
    }
    const total = parts.reduce((n, p) => n + p.length, 0);
    const iamf = new Uint8Array(total);
    let at = 0;
    for (const p of parts) {
      iamf.set(p, at);
      at += p.length;
    }
    return { iamf, sequences, samplesBytes };
  }

  // ── Incremental demux: what live streaming needs ──────────────────────
  //
  // The capture above is converted once, at the end. Streaming live needs
  // each temporal unit as soon as it is appended, with its presentation time,
  // so that it can be sent just before the video reaches it.

  // The first box of `type` under bytes[start, end), searched through the
  // container boxes.
  function findBox(bytes, start, end, type) {
    for (const box of boxes(bytes, start, end)) {
      if (box.type === type) return box;
      if (CONTAINERS.has(box.type)) {
        const found = findBox(bytes, box.payload, box.end, type);
        if (found) return found;
      }
    }
    return null;
  }

  // The init segment's timescale (mdhd) and fragment defaults (trex).
  function trackInfo(bytes, moov) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const mdhd = findBox(bytes, moov.payload, moov.end, 'mdhd');
    const trex = findBox(bytes, moov.payload, moov.end, 'trex');
    if (!mdhd) return null;
    const version = bytes[mdhd.payload];
    const timescale = view.getUint32(mdhd.payload + (version === 1 ? 20 : 12));
    return {
      timescale,
      // trex: version/flags, track_ID, sample_description_index, duration, size.
      defaultDuration: trex ? view.getUint32(trex.payload + 12) : 0,
      defaultSize: trex ? view.getUint32(trex.payload + 16) : 0,
    };
  }

  // A moof's first track fragment: decode time of its first sample, the
  // offset of the sample data from the moof's start, and each sample's
  // duration and size. Null when the data offset is absolute
  // (base_data_offset), which streaming segments never use.
  function parseMoof(bytes, moof, track) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const traf = findBox(bytes, moof.payload, moof.end, 'traf');
    if (!traf) return null;
    let defaultDuration = track.defaultDuration;
    let defaultSize = track.defaultSize;
    let baseTime = 0;
    let trun = null;
    for (const box of boxes(bytes, traf.payload, traf.end)) {
      const flags = view.getUint32(box.payload) & 0xffffff;
      if (box.type === 'tfhd') {
        if (flags & 0x1) return null;
        let at = box.payload + 8;
        if (flags & 0x2) at += 4;
        if (flags & 0x8) defaultDuration = view.getUint32((at += 4) - 4);
        if (flags & 0x10) defaultSize = view.getUint32((at += 4) - 4);
      } else if (box.type === 'tfdt') {
        baseTime =
          bytes[box.payload] === 1
            ? Number(view.getBigUint64(box.payload + 4))
            : view.getUint32(box.payload + 4);
      } else if (box.type === 'trun' && !trun) {
        trun = { box, flags };
      }
    }
    if (!trun) return null;
    const { box, flags } = trun;
    const count = view.getUint32(box.payload + 4);
    let at = box.payload + 8;
    let dataOffset = 0;
    if (flags & 0x1) dataOffset = view.getInt32((at += 4) - 4);
    if (flags & 0x4) at += 4;
    const samples = [];
    for (let i = 0; i < count; i++) {
      let duration = defaultDuration;
      let size = defaultSize;
      if (flags & 0x100) duration = view.getUint32((at += 4) - 4);
      if (flags & 0x200) size = view.getUint32((at += 4) - 4);
      if (flags & 0x400) at += 4;
      if (flags & 0x800) at += 4;
      samples.push({ duration, size });
    }
    return { baseTime, dataOffset, samples };
  }

  // Feed appended bytes in order; get back
  //   { type: 'config', obus }                 when the descriptors change,
  //   { type: 'unit', pts, duration, bytes }   per temporal unit (seconds).
  // `timestampOffset` is the SourceBuffer's at the time of the append, which
  // MSE adds to every presentation time.
  function createDemuxer() {
    let pending = new Uint8Array(0);
    let track = null;
    let config = null;
    return {
      push(bytes, timestampOffset = 0) {
        const all = new Uint8Array(pending.length + bytes.length);
        all.set(pending, 0);
        all.set(bytes, pending.length);
        const events = [];
        let consumed = 0;
        let moof = null;
        for (const box of boxes(all)) {
          if (box.type === 'moov') {
            track = trackInfo(all, box);
            const obus = configObus(all, box);
            if (obus && !sameBytes(obus, config)) {
              config = obus.slice();
              events.push({ type: 'config', obus: config });
            }
          } else if (box.type === 'moof') {
            moof = box;
            continue; // consumed with its mdat
          } else if (box.type === 'mdat' && moof && track) {
            const frag = parseMoof(all, moof, track);
            if (frag) {
              let at = moof.start + frag.dataOffset;
              let time = frag.baseTime;
              for (const s of frag.samples) {
                if (at + s.size > box.end) break;
                events.push({
                  type: 'unit',
                  pts: time / track.timescale + timestampOffset,
                  duration: s.duration / track.timescale,
                  bytes: all.slice(at, at + s.size),
                });
                at += s.size;
                time += s.duration;
              }
            }
          }
          moof = null;
          consumed = box.end;
        }
        // Keep an unpaired moof (its mdat not appended yet) and any
        // incomplete box for the next append.
        pending = all.slice(moof ? moof.start : consumed);
        return events;
      },
    };
  }

  globalThis.OmniphonyIamfMp4 = { boxes, configObus, toIamf, createDemuxer };
})();
