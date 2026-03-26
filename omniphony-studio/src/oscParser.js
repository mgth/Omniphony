function unwrapArg(arg) {
  return arg && typeof arg === 'object' && Object.prototype.hasOwnProperty.call(arg, 'value')
    ? arg.value
    : arg;
}

function toNumber(value) {
  const n = Number(value);
  return Number.isFinite(n) ? n : null;
}

function clamp(value, min, max) {
  return Math.min(max, Math.max(min, value));
}

function sphericalToCartesian(azimuthDeg, elevationDeg, distance) {
  const az = (azimuthDeg * Math.PI) / 180;
  const el = (elevationDeg * Math.PI) / 180;
  const d = distance;

  const x = d * Math.cos(el) * Math.cos(az);
  const y = d * Math.sin(el);
  const z = d * Math.cos(el) * Math.sin(az);

  return { x, y, z };
}

function omniphonySpeakerToSceneCartesian(azimuthDeg, elevationDeg, distance) {
  const az = (azimuthDeg * Math.PI) / 180;
  const el = (elevationDeg * Math.PI) / 180;
  const d = distance;

  return {
    // omniphony convention: 0 front, +90 left, +elevation up
    x: d * Math.cos(el) * Math.cos(az),
    y: d * Math.sin(el),
    z: d * Math.cos(el) * Math.sin(az)
  };
}

function getOmniphonyCoordinateFormat(context) {
  return context && Number(context.omniphonyCoordinateFormat) === 1 ? 1 : 0;
}

function findIdInAddress(parts) {
  const anchors = ['source', 'sources', 'object', 'obj', 'track', 'channel'];
  const reserved = new Set(['position', 'pos', 'xyz', 'aed', 'spherical', 'polar', 'angles', 'remove', 'delete', 'off']);

  for (let i = 0; i < parts.length - 1; i += 1) {
    if (anchors.includes(parts[i])) {
      const candidate = parts[i + 1];
      if (!reserved.has(candidate)) {
        return candidate;
      }
    }
  }

  return null;
}

function parseOmniphonyConfigMessage(parts, args) {
  if (!parts.includes('omniphony') || !parts.includes('config')) {
    return null;
  }

  if (parts.length === 3 && parts[2] === 'speakers') {
    const count = toNumber(args[0]);
    if (count === null) {
      return null;
    }

    return {
      type: 'config:speakers:count',
      count: Math.max(0, Math.floor(count))
    };
  }

  if (parts.length === 4 && parts[2] === 'speaker') {
    const index = toNumber(parts[3]);
    if (index === null || index < 0) {
      return null;
    }

    const [nameRaw, azRaw, elRaw, distanceRaw, spatializeRaw] = args;
    const azimuth = toNumber(azRaw);
    const elevation = toNumber(elRaw);
    const distance = toNumber(distanceRaw);
    if (azimuth === null || elevation === null || distance === null) {
      return null;
    }

    const position = omniphonySpeakerToSceneCartesian(azimuth, elevation, distance);
    const spatialize = toNumber(spatializeRaw);

    return {
      type: 'config:speaker',
      index: Math.floor(index),
      name: String(nameRaw ?? `spk-${index}`),
      azimuthDeg: azimuth,
      elevationDeg: elevation,
      distanceM: distance,
      spatialize: spatialize === null ? 1 : spatialize !== 0 ? 1 : 0,
      position: {
        x: position.x,
        y: position.y,
        z: position.z
      }
    };
  }

  return null;
}

function parseOmniphonyObjectPosition(parts, args, context) {
  if (!parts.includes('omniphony') || !parts.includes('object')) {
    return null;
  }
  const explicitCartesian = parts.includes('xyz');
  const explicitPolar = parts.includes('aed') || parts.includes('spherical') || parts.includes('polar');
  if (!explicitCartesian && !explicitPolar) {
    return null;
  }

  const idFromAddress = findIdInAddress(parts);
  if (!idFromAddress) {
    return null;
  }

  const x = toNumber(args[0]);
  const y = toNumber(args[1]);
  const z = toNumber(args[2]);
  if (x === null || y === null || z === null) {
    return null;
  }

  const nameRaw = args[7];
  const name = typeof nameRaw === 'string' && nameRaw.trim() ? nameRaw.trim() : null;

  const coordinateFormat = explicitCartesian ? 0 : explicitPolar ? 1 : getOmniphonyCoordinateFormat(context);

  const result = {
    type: 'update',
    id: String(idFromAddress),
    position: {
      x: coordinateFormat === 0 ? clamp(x, -1, 1) : 0,
      y: coordinateFormat === 0 ? clamp(y, -1, 1) : 0,
      z: coordinateFormat === 0 ? clamp(z, -1, 1) : 0,
      coordMode: coordinateFormat === 0 ? 'cartesian' : 'polar',
      azimuthDeg: coordinateFormat === 1 ? x : undefined,
      elevationDeg: coordinateFormat === 1 ? y : undefined,
      distanceM: coordinateFormat === 1 ? Math.max(0, z) : undefined
    }
  };
  if (name !== null) result.name = name;
  return result;
}

function parseOmniphonySpatialFrame(parts, args, context) {
  if (parts.length !== 3 || parts[0] !== 'omniphony' || parts[1] !== 'spatial' || parts[2] !== 'frame') {
    return null;
  }

  const samplePos = toNumber(args[0]);
  const objectCount = toNumber(args[1]);
  if (samplePos === null || objectCount === null) {
    return null;
  }

  const coordinateFormat = toNumber(args[2]) === 1 ? 1 : 0;
  if (context) {
    context.omniphonyCoordinateFormat = coordinateFormat;
  }

  return {
    type: 'spatial:frame',
    samplePos: Math.floor(samplePos),
    objectCount: Math.max(0, Math.floor(objectCount)),
    coordinateFormat
  };
}

function parseOmniphonyStateMessage(parts, args) {
  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'latency') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:latency',
      value
    };
  }
  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'latency_instant') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:latency:instant',
      value
    };
  }
  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'latency_target') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:latency:target',
      value
    };
  }
  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'latency_target_requested') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:latency:requested',
      value
    };
  }

  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'resample_ratio') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:resample_ratio',
      value
    };
  }
  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'adaptive_resampling') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:adaptive_resampling',
      enabled: value !== 0
    };
  }
  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'adaptive_resampling') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    if (parts[3] === 'kp_near') {
      return { type: 'state:adaptive_resampling:kp_near', value };
    }
    if (parts[3] === 'kp_far') {
      return { type: 'state:adaptive_resampling:kp_far', value };
    }
    if (parts[3] === 'ki') {
      return { type: 'state:adaptive_resampling:ki', value };
    }
    if (parts[3] === 'max_adjust') {
      return { type: 'state:adaptive_resampling:max_adjust', value };
    }
    if (parts[3] === 'near_far_threshold_ms') {
      return { type: 'state:adaptive_resampling:near_far_threshold_ms', value };
    }
    return null;
  }

  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'gain') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:master:gain',
      value
    };
  }

  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'loudness') {
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: 'state:loudness',
      enabled: value !== 0
    };
  }

  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'loudness') {
    const kind = parts[3];
    if (!['source', 'gain'].includes(kind)) {
      return null;
    }
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    return {
      type: kind === 'source' ? 'state:loudness:source' : 'state:loudness:gain',
      value
    };
  }

  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'spread') {
    const kind = parts[3];
    if (!['min', 'max', 'from_distance', 'distance_range', 'distance_curve'].includes(kind)) {
      return null;
    }
    const value = toNumber(args[0]);
    if (value === null) {
      return null;
    }
    if (kind === 'from_distance') {
      return { type: 'state:spread:from_distance', enabled: value !== 0 };
    }
    if (kind === 'distance_range') {
      return { type: 'state:spread:distance_range', value };
    }
    if (kind === 'distance_curve') {
      return { type: 'state:spread:distance_curve', value };
    }
    return {
      type: kind === 'min' ? 'state:spread:min' : 'state:spread:max',
      value
    };
  }

  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'distance_model') {
    const value = String(args[0]?.value ?? args[0] ?? '').trim().toLowerCase();
    if (!['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
      return null;
    }
    return {
      type: 'state:distance_model',
      value
    };
  }

  if (parts.length === 5 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[4] === 'gain') {
    const kind = parts[2];
    if (!['object', 'speaker'].includes(kind)) {
      return null;
    }

    const index = toNumber(parts[3]);
    if (index === null || index < 0) {
      return null;
    }

    const gain = toNumber(args[0]);
    if (gain === null) {
      return null;
    }

    return {
      type: kind === 'speaker' ? 'state:speaker:gain' : 'state:object:gain',
      id: String(Math.floor(index)),
      gain: clamp(gain, 0, 2)
    };
  }

  if (parts.length === 3 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'room_ratio') {
    const width = toNumber(args[0]);
    const length = toNumber(args[1]);
    const height = toNumber(args[2]);
    if (width === null || length === null || height === null) {
      return null;
    }

    return {
      type: 'state:room_ratio',
      width,
      length,
      height
    };
  }

  if (parts.length === 5 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[4] === 'mute') {
    const kind = parts[2];
    if (!['object', 'speaker'].includes(kind)) {
      return null;
    }

    const index = toNumber(parts[3]);
    if (index === null || index < 0) {
      return null;
    }

    const mutedRaw = toNumber(args[0]);
    if (mutedRaw === null) {
      return null;
    }

    return {
      type: kind === 'speaker' ? 'state:speaker:mute' : 'state:object:mute',
      id: String(Math.floor(index)),
      muted: mutedRaw !== 0
    };
  }

  if (parts.length === 5 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'speaker' && parts[4] === 'spatialize') {
    const index = toNumber(parts[3]);
    if (index === null || index < 0) {
      return null;
    }
    const spatializeRaw = toNumber(args[0]);
    if (spatializeRaw === null) {
      return null;
    }
    return {
      type: 'state:speaker:spatialize',
      id: String(Math.floor(index)),
      spatialize: spatializeRaw !== 0 ? 1 : 0
    };
  }

  if (parts.length === 5 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'speaker' && parts[4] === 'name') {
    const index = toNumber(parts[3]);
    if (index === null || index < 0) {
      return null;
    }
    const name = String(args[0] ?? '').trim();
    if (!name) {
      return null;
    }
    return {
      type: 'state:speaker:name',
      id: String(Math.floor(index)),
      name
    };
  }

  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'distance_diffuse') {
    const key = parts[3];
    if (key === 'enabled') {
      const value = toNumber(args[0]);
      if (value === null) return null;
      return { type: 'state:distance_diffuse:enabled', enabled: value !== 0 };
    }
    if (key === 'threshold' || key === 'curve') {
      const value = toNumber(args[0]);
      if (value === null) return null;
      return { type: `state:distance_diffuse:${key}`, value };
    }
  }

  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'vbap' && parts[3] === 'table_mode') {
    const value = typeof args[0] === 'string' ? args[0].trim().toLowerCase() : '';
    if (!['auto', 'polar', 'cartesian'].includes(value)) {
      return null;
    }
    return { type: 'state:vbap:table_mode', value };
  }

  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'vbap' && parts[3] === 'effective_mode') {
    const value = typeof args[0] === 'string' ? args[0].trim().toLowerCase() : '';
    if (!['polar', 'cartesian'].includes(value)) {
      return null;
    }
    return { type: 'state:vbap:effective_mode', value };
  }

  if (parts.length === 4 && parts[0] === 'omniphony' && parts[1] === 'state' && parts[2] === 'config' && parts[3] === 'saved') {
    const value = toNumber(args[0]);
    if (value === null) return null;
    return { type: 'state:config:saved', saved: value !== 0 };
  }

  return null;
}


function parseObjectGainsMessage(parts, args) {
  const meterIndex = parts.indexOf('meter');
  if (meterIndex === -1 || parts.length <= meterIndex + 3) {
    return null;
  }

  const meterKind = parts[meterIndex + 1];
  const meterId = parts[meterIndex + 2];
  const suffix = parts[meterIndex + 3];

  if (meterKind !== 'object' || suffix !== 'gains' || !meterId) {
    return null;
  }

  const gains = args
    .map((value) => clamp(toNumber(value) ?? 0, 0, 1));

  return {
    type: 'meter:object:gains',
    id: String(meterId),
    gains
  };
}

function parseMeterMessage(parts, args) {
  const meterIndex = parts.indexOf('meter');
  if (meterIndex === -1 || parts.length <= meterIndex + 2) {
    return null;
  }

  const meterKind = parts[meterIndex + 1];
  const meterId = parts[meterIndex + 2];
  const [peakRaw, rmsRaw] = args;
  const peakDbfs = clamp(toNumber(peakRaw) ?? -100, -100, 0);
  const rmsDbfs = clamp(toNumber(rmsRaw) ?? -100, -100, 0);

  if (!['object', 'speaker'].includes(meterKind) || !meterId) {
    return null;
  }

  return {
    type: meterKind === 'object' ? 'meter:object' : 'meter:speaker',
    id: String(meterId),
    peakDbfs,
    rmsDbfs
  };
}


function mapCartesianByAddress(parts, position) {
  const isOmniphonyObjectXyz = parts.includes('omniphony') && parts.includes('object') && parts.includes('xyz');
  if (!isOmniphonyObjectXyz) {
    return position;
  }

  // omniphony/object/{id}/xyz uses x=right, y=front, z=up.
  // Our scene convention is x=front, y=up, z=right.
  return {
    x: position.y,
    y: position.z,
    z: position.x
  };
}

function parseOscMessage(oscMsg, context = null) {
  const address = String(oscMsg.address || '');
  const parts = address.split('/').filter(Boolean).map((p) => p.toLowerCase());
  const args = (oscMsg.args || []).map(unwrapArg);

  const parsedOmniphonyConfig = parseOmniphonyConfigMessage(parts, args);
  if (parsedOmniphonyConfig) {
    return parsedOmniphonyConfig;
  }

  const parsedOmniphonyObjectPosition = parseOmniphonyObjectPosition(parts, args, context);
  if (parsedOmniphonyObjectPosition) {
    return parsedOmniphonyObjectPosition;
  }

  const parsedOmniphonySpatialFrame = parseOmniphonySpatialFrame(parts, args, context);
  if (parsedOmniphonySpatialFrame) {
    return parsedOmniphonySpatialFrame;
  }

  const parsedOmniphonyState = parseOmniphonyStateMessage(parts, args);
  if (parsedOmniphonyState) {
    return parsedOmniphonyState;
  }

  if (parts.includes('meter')) {
    const parsedGains = parseObjectGainsMessage(parts, args);
    if (parsedGains) {
      return parsedGains;
    }

    return parseMeterMessage(parts, args);
  }

  const isRemove = parts.some((p) => ['remove', 'delete', 'off'].includes(p));
  if (isRemove) {
    const idFromAddress = findIdInAddress(parts);
    const idFromArg = args.length > 0 ? String(args[0]) : null;
    const id = idFromArg || idFromAddress;
    return id ? { type: 'remove', id } : null;
  }

  const idFromAddress = findIdInAddress(parts);

  const hasSphericalHint = parts.some((p) => ['aed', 'spherical', 'polar', 'angles'].includes(p));
  const hasPositionHint = parts.some((p) => ['position', 'xyz', 'pos'].includes(p));

  let id = idFromAddress;
  let numericArgs = args.map(toNumber).filter((n) => n !== null);

  if (!id && args.length >= 4) {
    id = String(args[0]);
    numericArgs = args.slice(1).map(toNumber).filter((n) => n !== null);
  }

  if (!id || numericArgs.length < 3) {
    return null;
  }

  let position;
  if (hasSphericalHint) {
    const [azimuth, elevation, distance] = numericArgs;
    position = sphericalToCartesian(azimuth, elevation, distance);
  } else if (hasPositionHint || numericArgs.length >= 3) {
    const [x, y, z] = numericArgs;
    position = { x, y, z };
  }

  if (!position) {
    return null;
  }

  const mappedPosition = mapCartesianByAddress(parts, position);

  return {
    type: 'update',
    id,
    position: {
      x: clamp(mappedPosition.x, -1, 1),
      y: clamp(mappedPosition.y, -1, 1),
      z: clamp(mappedPosition.z, -1, 1)
    }
  };
}

module.exports = {
  parseOscMessage,
  sphericalToCartesian,
  clamp
};
