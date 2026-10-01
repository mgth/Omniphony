// node --test omniphony-browser-capture/test/convert.test.mjs
//
// Converts the libiamf conformance vector test_000220 from its fragmented and
// its plain MP4 form and compares with the raw .iamf of the same stream.
// Needs HARLETTY_IAMF_VECTORS (the libiamf `tests/` directory, with the
// `_f.mp4`/`_s.mp4` files); skipped without it.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import '../iamf-mp4.js';

const { toIamf } = globalThis.OmniphonyIamfMp4;
const dir = process.env.HARLETTY_IAMF_VECTORS;
const vector = (name) => (dir && existsSync(join(dir, name)) ? readFileSync(join(dir, name)) : null);

for (const form of ['_f.mp4', '_s.mp4']) {
  test(`test_000220${form} converts to the raw stream`, (t) => {
    const mp4 = vector(`test_000220${form}`);
    const raw = vector('test_000220.iamf');
    if (!mp4 || !raw) return t.skip('set HARLETTY_IAMF_VECTORS');
    const { iamf, sequences } = toIamf(new Uint8Array(mp4));
    assert.equal(sequences, 1);
    assert.ok(Buffer.from(iamf).equals(raw), `converted ${iamf.length} bytes, raw ${raw.length}`);
  });
}

test('a re-initialised capture repeats the descriptors only when they change', (t) => {
  const mp4 = vector('test_000220_f.mp4');
  const raw = vector('test_000220.iamf');
  if (!mp4 || !raw) return t.skip('set HARLETTY_IAMF_VECTORS');
  // The player appending the same init segment again (a SABR format
  // re-announce) must not start a new sequence; the media keeps flowing.
  const bytes = new Uint8Array(mp4);
  const firstMoof = [...globalThis.OmniphonyIamfMp4.boxes(bytes)].find((b) => b.type === 'moof');
  const init = bytes.subarray(0, firstMoof.start);
  const twice = new Uint8Array(init.length + bytes.length);
  twice.set(bytes, 0);
  twice.set(init, bytes.length);
  const { iamf, sequences } = toIamf(twice);
  assert.equal(sequences, 1);
  assert.ok(Buffer.from(iamf).equals(raw), `converted ${iamf.length} bytes, raw ${raw.length}`);
});

test('an incomplete tail box is left out', () => {
  // A capture saved mid-append: a box header announcing more bytes than
  // were captured.
  const truncated = new Uint8Array([0, 0, 0, 64, 0x6d, 0x64, 0x61, 0x74, 1, 2, 3]);
  const { iamf, sequences } = toIamf(truncated);
  assert.equal(sequences, 0);
  assert.equal(iamf.length, 0);
});
