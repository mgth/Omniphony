import * as THREE from 'three';

// Per-speaker "frequency extent" gauge shown in the 3D scene: a small vertical
// bar on a log-frequency axis (20 Hz at the bottom → 20 kHz at the top) whose
// lit segment is the speaker's pass-band. It lets you recognise a speaker's
// crossover role at a glance — low-pass (sub) fills the bottom, high-pass
// (tweeter) the top, band-pass (mid) a floating middle segment, and full-band
// the whole bar. The lit segment is filled with a solid colour, one per band
// actually present in the layout's crossover split, so speakers in different
// bands stand out clearly. Drawn as a billboard sprite so it always reads
// upright regardless of camera orbit, and redrawn only when something it
// depends on changes.

const FMIN = 20;
const FMAX = 20000;
const LOG_MIN = Math.log(FMIN);
const LOG_SPAN = Math.log(FMAX) - LOG_MIN;

// Canvas aspect (W/H) matches the sprite scale aspect so the bar isn't stretched.
const CANVAS_W = 64;
const CANVAS_H = 256;
const SCALE_X = 0.055;
const SCALE_Y = 0.22;

// Reference gridlines (Hz) drawn faintly across the track.
const TICKS_HZ = [100, 1000, 10000];

// Colour used when there is no crossover split at all (every speaker full-band).
const FULL_BAND_COLOR = '#8ec8ff';

function logPos(freq) {
  const f = Math.min(FMAX, Math.max(FMIN, Number(freq) || FMIN));
  return (Math.log(f) - LOG_MIN) / LOG_SPAN; // 0 at FMIN (bottom), 1 at FMAX (top)
}

// One solid, well-separated colour per band index, ordered low→high
// (warm→cool: red for the lowest band, blue for the highest). Shared with the
// per-object band bars in the objects list so both read the same palette.
export function bandColor(index, count) {
  if (count <= 1) return FULL_BAND_COLOR;
  const hue = 8 + (248 * index) / (count - 1); // ~red (low) → ~blue (high)
  return `hsl(${hue.toFixed(0)}, 68%, 56%)`;
}

// Index of the band a speaker belongs to within `edges` (= [0, ...cutoffs,
// Infinity]). A band starts at the speaker's lower cutoff (freqLow, or 0 for a
// full-/low-pass), which uniquely identifies the band in a crossover layout.
// Exported so the speaker cubes can take the same band colour as this gauge.
export function speakerBandIndex(speaker, edges) {
  if (!Array.isArray(edges) || edges.length < 2) return 0;
  const lo = Number(speaker?.freqLow) > 0 ? Number(speaker.freqLow) : 0;
  for (let i = 0; i < edges.length - 1; i += 1) {
    if (Math.abs(edges[i] - lo) < 0.1) return i;
  }
  return 0;
}

function roundRect(ctx, x, y, w, h, r) {
  const radius = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + w, y, x + w, y + h, radius);
  ctx.arcTo(x + w, y + h, x, y + h, radius);
  ctx.arcTo(x, y + h, x, y, radius);
  ctx.arcTo(x, y, x + w, y, radius);
  ctx.closePath();
}

// Pass-band extent of a single speaker, in Hz, plus a cache key.
function passBand(speaker) {
  const low = Number(speaker?.freqLow);
  const high = Number(speaker?.freqHigh);
  const hasLow = Number.isFinite(low) && low > 0;
  const hasHigh = Number.isFinite(high) && high > 0;
  return {
    lo: hasLow ? low : FMIN,
    hi: hasHigh ? high : FMAX,
    key: `${hasLow ? low : 0}|${hasHigh ? high : 0}`
  };
}

function drawBar(ctx, speaker, edges) {
  const W = CANVAS_W;
  const H = CANVAS_H;
  ctx.clearRect(0, 0, W, H);

  const padY = 10;
  const trackW = 22;
  const trackX = (W - trackW) / 2;
  const trackTop = padY;
  const trackH = H - padY * 2;
  const yFor = (freq) => trackTop + (1 - logPos(freq)) * trackH;

  const bandCount = Array.isArray(edges) ? edges.length - 1 : 1;
  const color = bandColor(speakerBandIndex(speaker, edges), bandCount);

  // Dark track base.
  ctx.fillStyle = 'rgba(16, 22, 30, 0.82)';
  roundRect(ctx, trackX, trackTop, trackW, trackH, 8);
  ctx.fill();

  // Lit pass-band segment, solid band colour (clipped to the rounded track).
  const { lo, hi } = passBand(speaker);
  const yHi = yFor(hi); // smaller y (top)
  const yLo = yFor(lo); // larger y (bottom)
  ctx.save();
  roundRect(ctx, trackX, trackTop, trackW, trackH, 8);
  ctx.clip();
  ctx.fillStyle = color;
  ctx.fillRect(trackX + 2, yHi, trackW - 4, Math.max(2, yLo - yHi));
  ctx.restore();

  // Border.
  ctx.lineWidth = 2;
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.28)';
  roundRect(ctx, trackX, trackTop, trackW, trackH, 8);
  ctx.stroke();

  // Faint reference gridlines at the decade frequencies.
  ctx.lineWidth = 1;
  ctx.strokeStyle = 'rgba(255, 255, 255, 0.22)';
  TICKS_HZ.forEach((hz) => {
    const y = yFor(hz);
    ctx.beginPath();
    ctx.moveTo(trackX + 3, y);
    ctx.lineTo(trackX + trackW - 3, y);
    ctx.stroke();
  });
}

export function createSpeakerBandBar() {
  const canvas = document.createElement('canvas');
  canvas.width = CANVAS_W;
  canvas.height = CANVAS_H;
  const ctx = canvas.getContext('2d');
  const texture = new THREE.CanvasTexture(canvas);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.SRGBColorSpace;
  const material = new THREE.SpriteMaterial({
    map: texture,
    transparent: true,
    depthTest: false,
    depthWrite: false,
    toneMapped: false
  });
  const sprite = new THREE.Sprite(material);
  sprite.scale.set(SCALE_X, SCALE_Y, 1);
  sprite.frustumCulled = false;
  sprite.renderOrder = 39;
  sprite.userData.bandCanvas = canvas;
  sprite.userData.bandCtx = ctx;
  sprite.userData.bandTexture = texture;
  sprite.userData.bandKey = null;
  return sprite;
}

// Redraw the gauge only when its appearance would actually change. The fill
// colour depends on the layout's whole crossover split, so `edges` (from
// computeCrossoverBandEdges) is part of the cache key — recolouring all gauges
// when any speaker's cutoffs change the band set.
export function updateSpeakerBandBar(sprite, speaker, edges) {
  if (!sprite?.userData?.bandCtx) return;
  const bandCount = Array.isArray(edges) ? edges.length - 1 : 1;
  const key = `${passBand(speaker).key}#${speakerBandIndex(speaker, edges)}/${bandCount}`;
  if (sprite.userData.bandKey === key) return;
  sprite.userData.bandKey = key;
  drawBar(sprite.userData.bandCtx, speaker, edges);
  sprite.userData.bandTexture.needsUpdate = true;
}

export function disposeSpeakerBandBar(sprite) {
  if (!sprite?.userData) return;
  sprite.userData.bandTexture?.dispose();
  sprite.userData.bandTexture = null;
  sprite.userData.bandCanvas = null;
  sprite.userData.bandCtx = null;
  sprite.material?.dispose();
}
