const test = require('node:test');
const assert = require('node:assert/strict');

const { parseOscMessage, sphericalToCartesian, clamp } = require('../src/oscParser');

function msg(address, args) {
  return { address, args };
}

test('clamp clamps values', () => {
  assert.equal(clamp(2, -1, 1), 1);
  assert.equal(clamp(-2, -1, 1), -1);
  assert.equal(clamp(0.2, -1, 1), 0.2);
});

test('sphericalToCartesian converts degrees to xyz', () => {
  const { x, y, z } = sphericalToCartesian(0, 0, 1);
  assert.ok(Math.abs(x - 1) < 1e-6);
  assert.ok(Math.abs(y - 0) < 1e-6);
  assert.ok(Math.abs(z - 0) < 1e-6);
});

test('parses cartesian with id in args', () => {
  const parsed = parseOscMessage(msg('/source/position', ['7', 0.2, -0.1, 0.4]));
  assert.deepEqual(parsed, {
    type: 'update',
    id: '7',
    position: { x: 0.2, y: -0.1, z: 0.4 }
  });
});

test('returns null for malformed or insufficient args', () => {
  assert.equal(parseOscMessage(msg('/source/position', ['x', 'y'])), null);
  assert.equal(parseOscMessage(msg('/source/position', [1, 2])), null);
  assert.equal(parseOscMessage(msg('/source/position', [])), null);
});

test('parses cartesian with id in address', () => {
  const parsed = parseOscMessage(msg('/source/5/position', [0.2, 0.1, -0.4]));
  assert.deepEqual(parsed, {
    type: 'update',
    id: '5',
    position: { x: 0.2, y: 0.1, z: -0.4 }
  });
});

test('clamps cartesian positions to [-1, 1]', () => {
  const parsed = parseOscMessage(msg('/source/1/position', [2, -3, 0.5]));
  assert.deepEqual(parsed, {
    type: 'update',
    id: '1',
    position: { x: 1, y: -1, z: 0.5 }
  });
});

test('parses spherical aed with id in address', () => {
  const parsed = parseOscMessage(msg('/source/9/aed', [90, 0, 1]));
  assert.equal(parsed.type, 'update');
  assert.equal(parsed.id, '9');
  assert.ok(Math.abs(parsed.position.x - 0) < 1e-6);
  assert.ok(Math.abs(parsed.position.y - 0) < 1e-6);
  assert.ok(Math.abs(parsed.position.z - 1) < 1e-6);
});

test('parses remove even with reserved keywords in address', () => {
  const parsed = parseOscMessage(msg('/object/remove', ['99']));
  assert.deepEqual(parsed, { type: 'remove', id: '99' });
});

test('parses remove with id in args', () => {
  const parsed = parseOscMessage(msg('/source/remove', [12]));
  assert.deepEqual(parsed, { type: 'remove', id: '12' });
});

test('parses remove with id in address', () => {
  const parsed = parseOscMessage(msg('/source/12/remove', []));
  assert.deepEqual(parsed, { type: 'remove', id: '12' });
});

test('parses meter and gains', () => {
  const meter = parseOscMessage(msg('/omniphony/meter/object/3', [-2, -10]));
  assert.deepEqual(meter, {
    type: 'meter:object',
    id: '3',
    peakDbfs: -2,
    rmsDbfs: -10
  });

  const gains = parseOscMessage(msg('/omniphony/meter/object/3/gains', [1.2, 0.5, -1]));
  assert.deepEqual(gains, {
    type: 'meter:object:gains',
    id: '3',
    gains: [1, 0.5, 0]
  });
});

test('clamps meter values to [-100, 0]', () => {
  const meter = parseOscMessage(msg('/omniphony/meter/speaker/2', [5, -200]));
  assert.deepEqual(meter, {
    type: 'meter:speaker',
    id: '2',
    peakDbfs: 0,
    rmsDbfs: -100
  });
});

test('parses omniphony config messages', () => {
  const count = parseOscMessage(msg('/omniphony/config/speakers', [4]));
  assert.deepEqual(count, { type: 'config:speakers:count', count: 4 });

  const speaker = parseOscMessage(msg('/omniphony/config/speaker/2', ['L', 30, 10, 1.5, 0]));
  assert.equal(speaker.type, 'config:speaker');
  assert.equal(speaker.index, 2);
  assert.equal(speaker.name, 'L');
  assert.equal(speaker.azimuthDeg, 30);
  assert.equal(speaker.elevationDeg, 10);
  assert.equal(speaker.distanceM, 1.5);
  assert.equal(speaker.spatialize, 0);
  assert.ok(typeof speaker.position.x === 'number');
});

test('parses omniphony object xyz mapping', () => {
  const parsed = parseOscMessage(msg('/omniphony/object/7/xyz', [0.2, 0.3, 0.4]));
  assert.deepEqual(parsed, {
    type: 'update',
    id: '7',
    position: { x: 0.2, y: 0.3, z: 0.4, coordMode: 'cartesian', azimuthDeg: undefined, elevationDeg: undefined, distanceM: undefined }
  });
});

test('parses omniphony spatial frame and preserves explicit xyz decoding', () => {
  const ctx = { omniphonyCoordinateFormat: 0 };
  const frame = parseOscMessage(msg('/omniphony/spatial/frame', [1024, 3, 1]), ctx);
  assert.deepEqual(frame, {
    type: 'spatial:frame',
    samplePos: 1024,
    objectCount: 3,
    coordinateFormat: 1
  });
  assert.equal(ctx.omniphonyCoordinateFormat, 1);

  const parsed = parseOscMessage(msg('/omniphony/object/7/xyz', [0.2, 0.3, 0.4]), ctx);
  assert.equal(parsed.type, 'update');
  assert.equal(parsed.id, '7');
  assert.deepEqual(parsed.position, { x: 0.2, y: 0.3, z: 0.4, coordMode: 'cartesian', azimuthDeg: undefined, elevationDeg: undefined, distanceM: undefined });
});

test('parses omniphony object aed in polar mode', () => {
  const ctx = { omniphonyCoordinateFormat: 0 };
  const frame = parseOscMessage(msg('/omniphony/spatial/frame', [1024, 3, 1]), ctx);
  assert.equal(frame.coordinateFormat, 1);

  const parsed = parseOscMessage(msg('/omniphony/object/7/aed', [90, 0, 1]), ctx);
  assert.deepEqual(parsed, {
    type: 'update',
    id: '7',
    position: { x: 0, y: 0, z: 0, coordMode: 'polar', azimuthDeg: 90, elevationDeg: 0, distanceM: 1 }
  });
});

test('parses omniphony state messages', () => {
  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/latency', [12.5])),
    { type: 'state:latency', value: 12.5 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/resample_ratio', [0.999])),
    { type: 'state:resample_ratio', value: 0.999 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/gain', [1.2])),
    { type: 'state:master:gain', value: 1.2 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/loudness', [1])),
    { type: 'state:loudness', enabled: true }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/loudness/source', [-24])),
    { type: 'state:loudness:source', value: -24 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/loudness/gain', [0.8])),
    { type: 'state:loudness:gain', value: 0.8 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/spread/min', [0.2])),
    { type: 'state:spread:min', value: 0.2 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/spread/max', [0.9])),
    { type: 'state:spread:max', value: 0.9 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/room_ratio', [1, 2, 3])),
    { type: 'state:room_ratio', width: 1, length: 2, height: 3 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/object/5/gain', [1.5])),
    { type: 'state:object:gain', id: '5', gain: 1.5 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/object/5/gain', [3])),
    { type: 'state:object:gain', id: '5', gain: 2 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/speaker/6/gain', [0.7])),
    { type: 'state:speaker:gain', id: '6', gain: 0.7 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/object/5/mute', [1])),
    { type: 'state:object:mute', id: '5', muted: true }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/speaker/6/mute', [0])),
    { type: 'state:speaker:mute', id: '6', muted: false }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/distance_diffuse/enabled', [1])),
    { type: 'state:distance_diffuse:enabled', enabled: true }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/distance_diffuse/threshold', [0.6])),
    { type: 'state:distance_diffuse:threshold', value: 0.6 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/distance_diffuse/curve', [1.2])),
    { type: 'state:distance_diffuse:curve', value: 1.2 }
  );

  assert.deepEqual(
    parseOscMessage(msg('/omniphony/state/config/saved', [1])),
    { type: 'state:config:saved', saved: true }
  );
});
