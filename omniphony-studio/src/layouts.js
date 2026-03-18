const fs = require('fs');
const path = require('path');
const { sphericalToCartesian, clamp } = require('./oscParser');

const LAYOUTS_DIR = path.join(__dirname, '..', '..', 'layouts');

function cartesianToSpherical(x, y, z) {
  const dist = Math.sqrt((x * x) + (y * y) + (z * z));
  const azimuth = (Math.atan2(z, x) * 180) / Math.PI;
  const elevation = dist > 0 ? (Math.atan2(y, Math.sqrt((x * x) + (z * z))) * 180) / Math.PI : 0;
  return { azimuth, elevation, distance: dist };
}

function normalizeSpeaker(speaker) {
  const spatialize = Number(speaker?.spatialize) === 0 ? 0 : 1;
  const coordModeRaw = String(
    speaker?.coord_mode
    ?? speaker?.coordinate_mode
    ?? speaker?.coordMode
    ?? ((speaker && typeof speaker.x === 'number' && typeof speaker.y === 'number' && typeof speaker.z === 'number')
      ? 'cartesian'
      : 'polar')
  ).toLowerCase();
  const coordMode = coordModeRaw === 'cartesian' ? 'cartesian' : 'polar';
  const delayMs = Math.max(0, Number(speaker?.delay_ms ?? speaker?.delay ?? 0) || 0);
  const id = String(speaker.id || speaker.name || 'spk');

  if (speaker && typeof speaker.x === 'number' && typeof speaker.y === 'number' && typeof speaker.z === 'number') {
    const x = clamp(Number(speaker.x) || 0, -1, 1);
    const y = clamp(Number(speaker.y) || 0, -1, 1);
    const z = clamp(Number(speaker.z) || 0, -1, 1);
    const polar = cartesianToSpherical(x, y, z);
    return {
      id,
      x,
      y,
      z,
      azimuthDeg: Number(speaker.azimuth ?? speaker.az ?? speaker.azimuthDeg ?? polar.azimuth),
      elevationDeg: Number(speaker.elevation ?? speaker.el ?? speaker.elevationDeg ?? polar.elevation),
      distanceM: Math.max(0.01, Number(speaker.distance ?? speaker.dist ?? speaker.distanceM ?? polar.distance) || polar.distance || 1),
      coordMode,
      spatialize,
      delay_ms: delayMs
    };
  }

  const azimuth = Number(speaker.azimuth ?? speaker.az ?? 0);
  const elevation = Number(speaker.elevation ?? speaker.el ?? 0);
  const distance = Number(speaker.distance ?? speaker.dist ?? 1);
  const c = sphericalToCartesian(azimuth, elevation, distance);

  return {
    id,
    x: clamp(c.x, -1, 1),
    y: clamp(c.y, -1, 1),
    z: clamp(c.z, -1, 1),
    azimuthDeg: azimuth,
    elevationDeg: elevation,
    distanceM: Math.max(0.01, distance || 1),
    coordMode,
    spatialize,
    delay_ms: delayMs
  };
}

function parseYamlValue(rawValue) {
  const trimmed = rawValue.trim();
  if (!trimmed) {
    return '';
  }

  if ((trimmed.startsWith('"') && trimmed.endsWith('"')) || (trimmed.startsWith("'") && trimmed.endsWith("'"))) {
    return trimmed.slice(1, -1);
  }

  if (trimmed === 'true') {
    return true;
  }

  if (trimmed === 'false') {
    return false;
  }

  const asNumber = Number(trimmed);
  if (!Number.isNaN(asNumber)) {
    return asNumber;
  }

  return trimmed;
}

function parseYamlLayout(rawText) {
  const lines = rawText.split(/\r?\n/);
  const speakers = [];
  let currentSpeaker = null;
  let inSpeakersBlock = false;
  const topLevel = {};

  lines.forEach((line) => {
    const withoutComment = line.replace(/\s+#.*$/, '');
    const trimmed = withoutComment.trim();

    if (!trimmed) {
      return;
    }

    if (trimmed === 'speakers:') {
      inSpeakersBlock = true;
      return;
    }

    if (!inSpeakersBlock) {
      const separatorIndex = trimmed.indexOf(':');
      if (separatorIndex !== -1) {
        const key = trimmed.slice(0, separatorIndex).trim();
        const value = trimmed.slice(separatorIndex + 1);
        topLevel[key] = parseYamlValue(value);
      }
      return;
    }

    if (trimmed.startsWith('- ')) {
      currentSpeaker = {};
      speakers.push(currentSpeaker);

      const sameLineEntry = trimmed.slice(2).trim();
      if (!sameLineEntry) {
        return;
      }

      const separatorIndex = sameLineEntry.indexOf(':');
      if (separatorIndex === -1) {
        return;
      }

      const key = sameLineEntry.slice(0, separatorIndex).trim();
      const value = sameLineEntry.slice(separatorIndex + 1);
      currentSpeaker[key] = parseYamlValue(value);
      return;
    }

    if (!currentSpeaker) {
      return;
    }

    const separatorIndex = trimmed.indexOf(':');
    if (separatorIndex === -1) {
      return;
    }

    const key = trimmed.slice(0, separatorIndex).trim();
    const value = trimmed.slice(separatorIndex + 1);
    currentSpeaker[key] = parseYamlValue(value);
  });

  return {
    ...topLevel,
    speakers
  };
}

function parseLayoutFile(filePath, extension) {
  const rawText = fs.readFileSync(filePath, 'utf8');
  if (extension === '.json') {
    return JSON.parse(rawText);
  }

  if (extension === '.yaml' || extension === '.yml') {
    return parseYamlLayout(rawText);
  }

  return null;
}

function loadLayouts() {
  if (!fs.existsSync(LAYOUTS_DIR)) {
    return [];
  }

  const layoutFiles = fs
    .readdirSync(LAYOUTS_DIR)
    .filter((file) => ['.json', '.yaml', '.yml'].includes(path.extname(file).toLowerCase()));

  const stemCounts = layoutFiles.reduce((acc, file) => {
    const extension = path.extname(file).toLowerCase();
    const stem = path.basename(file, extension);
    acc[stem] = (acc[stem] || 0) + 1;
    return acc;
  }, {});

  return layoutFiles
    .map((file) => {
      const fullPath = path.join(LAYOUTS_DIR, file);
      const extension = path.extname(file).toLowerCase();
      const stem = path.basename(file, extension);
      const raw = parseLayoutFile(fullPath, extension);
      const speakers = Array.isArray(raw.speakers) ? raw.speakers.map(normalizeSpeaker) : [];
      const hasDuplicateStem = stemCounts[stem] > 1;
      const extensionLabel = extension.replace('.', '');

      return {
        key: hasDuplicateStem ? `${stem}-${extensionLabel}` : stem,
        name: raw.name || (hasDuplicateStem ? `${stem} (${extensionLabel})` : stem),
        radius_m: Math.max(0.01, Number(raw.radius_m) || 1),
        speakers
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

module.exports = {
  loadLayouts,
  normalizeSpeaker
};
