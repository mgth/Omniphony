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

  globalThis.OmniphonyIamfMp4 = { boxes, configObus, toIamf };
})();
