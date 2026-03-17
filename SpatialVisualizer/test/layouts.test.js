const test = require('node:test');
const assert = require('node:assert/strict');
const { loadLayouts, normalizeSpeaker } = require('../src/layouts');

test('normalizeSpeaker supports spherical coordinates', () => {
  const s = normalizeSpeaker({ id: 'L', azimuth: 0, elevation: 0, distance: 1 });
  assert.equal(s.id, 'L');
  assert.equal(s.x, 1);
  assert.equal(s.y, 0);
  assert.equal(s.z, 0);
});

test('loadLayouts returns available layouts', () => {
  const layouts = loadLayouts();
  assert.equal(layouts.length, 8);
  const stereo = layouts.find((l) => l.key === 'stereo');
  assert.ok(stereo);
  assert.equal(stereo.speakers.length, 2);

  const immersive = layouts.find((l) => l.key === '7.1.4');
  assert.ok(immersive);
  assert.ok(immersive.speakers.length >= 10);

  const json51 = layouts.find((l) => l.key === '5.1-json');
  const yaml51 = layouts.find((l) => l.key === '5.1-yaml');
  assert.ok(json51);
  assert.ok(yaml51);
});
