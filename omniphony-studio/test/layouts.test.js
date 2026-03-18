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

test('normalizeSpeaker preserves coord mode and normalized cartesian coordinates', () => {
  const s = normalizeSpeaker({
    id: 'FL',
    coord_mode: 'cartesian',
    x: 2,
    y: -0.25,
    z: 0.5,
    azimuth: -30,
    elevation: 0,
    distance: 2
  });
  assert.equal(s.coordMode, 'cartesian');
  assert.equal(s.x, 1);
  assert.equal(s.y, -0.25);
  assert.equal(s.z, 0.5);
  assert.equal(s.azimuthDeg, -30);
  assert.equal(s.distanceM, 2);
});

test('loadLayouts returns available layouts', () => {
  const layouts = loadLayouts();
  assert.equal(layouts.length, 8);
  const stereo = layouts.find((l) => l.key === '2.0');
  assert.ok(stereo);
  assert.equal(stereo.speakers.length, 2);
  assert.ok(stereo.radius_m > 0);

  const immersive = layouts.find((l) => l.key === '7.1.4');
  assert.ok(immersive);
  assert.ok(immersive.speakers.length >= 10);

  const layout51 = layouts.find((l) => l.key === '5.1');
  assert.ok(layout51);
  assert.equal(layout51.speakers.length, 6);
});
