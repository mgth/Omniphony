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

// ── Incremental demux (live streaming) ────────────────────────────────────

// Push a capture through the demuxer in uneven appends; return its events.
function demuxInChunks(bytes, seed = 7) {
  const demuxer = globalThis.OmniphonyIamfMp4.createDemuxer();
  const events = [];
  let at = 0;
  let x = seed;
  while (at < bytes.length) {
    x = (x * 1103515245 + 12345) % 2 ** 31;
    const n = 1 + (x % 70000);
    events.push(...demuxer.push(bytes.subarray(at, at + n)));
    at += n;
  }
  return events;
}

function concatEvents(events) {
  const parts = events.map((e) => (e.type === 'config' ? e.obus : e.bytes));
  return Buffer.concat(parts.map((p) => Buffer.from(p)));
}

test('the incremental demuxer yields the raw stream as timed units', (t) => {
  const mp4 = vector('test_000220_f.mp4');
  const raw = vector('test_000220.iamf');
  if (!mp4 || !raw) return t.skip('set HARLETTY_IAMF_VECTORS');
  const events = demuxInChunks(new Uint8Array(mp4));
  assert.equal(events[0].type, 'config');
  assert.equal(events.filter((e) => e.type === 'config').length, 1);
  assert.ok(concatEvents(events).equals(raw));
  const units = events.filter((e) => e.type === 'unit');
  assert.equal(units.length, 251);
  // 960-sample Opus frames at 48 kHz, back to back. The first decode time
  // is the Opus pre-skip, 312 samples.
  assert.ok(Math.abs(units[0].pts - 312 / 48000) < 1e-9, `starts at ${units[0].pts}`);
  units.forEach((u, i) => {
    assert.ok(Math.abs(u.pts - units[0].pts - i * 0.02) < 1e-9, `unit ${i} at ${u.pts}`);
  });
  // Every unit lasts a frame but the last, which the container shortens by
  // the stream's 648-sample end trim: 250 × 960 + 312 = 240 312 samples of
  // track time, the 240 000 decoded plus the 312 pre-skipped.
  units.slice(0, -1).forEach((u) => assert.ok(Math.abs(u.duration - 0.02) < 1e-9));
  const last = units.at(-1);
  assert.ok(Math.abs(last.pts + last.duration - (312 + 240312) / 48000) < 1e-9);
});

test('the demuxer agrees with the one-shot conversion on a YouTube capture', (t) => {
  const dir = process.env.OMNIPHONY_IAMF_CAPTURES;
  const path = dir && join(dir, 'iamf-capture-CD7n92qPuq4-0.mp4');
  if (!path || !existsSync(path)) return t.skip('set OMNIPHONY_IAMF_CAPTURES');
  const bytes = new Uint8Array(readFileSync(path));
  const events = demuxInChunks(bytes, 99);
  assert.ok(concatEvents(events).equals(Buffer.from(toIamf(bytes).iamf)));
  const units = events.filter((e) => e.type === 'unit');
  for (let i = 1; i < units.length; i++) {
    const gap = units[i].pts - (units[i - 1].pts + units[i - 1].duration);
    assert.ok(Math.abs(gap) < 1e-6, `gap of ${gap}s before unit ${i}`);
  }
  t.diagnostic(`${units.length} units, ${units[0].pts}s → ${units.at(-1).pts + units.at(-1).duration}s`);
});
