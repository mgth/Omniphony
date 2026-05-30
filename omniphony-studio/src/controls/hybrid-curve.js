/**
 * Graphical editor for the hybrid backend blend curve.
 *
 * X axis = normalised distance (0 = cube centre, 1 = cube surface).
 * Y axis = blend ratio (0 = 100 % internal backend, 1 = 100 % external backend).
 *
 * Interaction:
 *  - drag a point to move it (the two endpoints are locked on X at 0 and 1,
 *    interior points are clamped between their neighbours);
 *  - double-click empty space to add a point;
 *  - double-click a point to remove it (the endpoints cannot be removed).
 *
 * Curve state lives in `app.renderBackendState.hybrid.curve`. Edits update that
 * state, refresh the renderer status, and push the new points to the renderer
 * over OSC (debounced while dragging).
 */

import { invoke } from '@tauri-apps/api/core';
import { app } from '../state.js';
import { renderVbapStatus } from './vbap.js';

const DEFAULT_CURVE = [[0, 0], [1, 1]];
const PAD = { left: 26, right: 10, top: 10, bottom: 18 };
const HIT_RADIUS = 11; // px

let canvas = null;
let ctx = null;
let readoutEl = null;
let dragIndex = -1;
let sendTimer = null;

function clamp01(value) {
  return Math.min(1, Math.max(0, value));
}

/**
 * Evaluate the blend curve at normalised x — mirror of the renderer's
 * BlendCurve::eval so the displayed curve matches the audio. `smoothing` blends
 * piecewise-linear (0) with a Catmull-Rom spline through the points (1).
 */
function evalCurve(points, smoothing, x) {
  if (!points.length) return 0;
  if (x <= points[0][0]) return points[0][1];
  const last = points.length - 1;
  if (x >= points[last][0]) return points[last][1];
  for (let i = 0; i < last; i += 1) {
    const [x0, y0] = points[i];
    const [x1, y1] = points[i + 1];
    if (x <= x1) {
      const span = Math.max(x1 - x0, 1e-6);
      const t = clamp01((x - x0) / span);
      const linear = y0 + (y1 - y0) * t;
      if (smoothing <= 0) return linear;
      const ym = i > 0 ? points[i - 1][1] : y0;
      const yp = i + 2 <= last ? points[i + 2][1] : y1;
      const m0 = 0.5 * (y1 - ym);
      const m1 = 0.5 * (yp - y0);
      const t2 = t * t;
      const t3 = t2 * t;
      const spline = (2 * t3 - 3 * t2 + 1) * y0
        + (t3 - 2 * t2 + t) * m0
        + (-2 * t3 + 3 * t2) * y1
        + (t3 - t2) * m1;
      return clamp01(linear + (spline - linear) * smoothing);
    }
  }
  return points[last][1];
}

function currentCurve() {
  const curve = app.renderBackendState.hybrid.curve;
  if (Array.isArray(curve) && curve.length >= 2) {
    return curve;
  }
  return DEFAULT_CURVE.map((point) => [point[0], point[1]]);
}

function plotRect() {
  return {
    x: PAD.left,
    y: PAD.top,
    w: canvas.width - PAD.left - PAD.right,
    h: canvas.height - PAD.top - PAD.bottom
  };
}

function toPixel(point) {
  const r = plotRect();
  return {
    x: r.x + clamp01(point[0]) * r.w,
    y: r.y + (1 - clamp01(point[1])) * r.h
  };
}

function eventToData(evt) {
  const rect = canvas.getBoundingClientRect();
  const scaleX = canvas.width / Math.max(1, rect.width);
  const scaleY = canvas.height / Math.max(1, rect.height);
  const px = (evt.clientX - rect.left) * scaleX;
  const py = (evt.clientY - rect.top) * scaleY;
  const r = plotRect();
  return [
    clamp01((px - r.x) / r.w),
    clamp01(1 - (py - r.y) / r.h)
  ];
}

function pointIndexNear(evt) {
  const rect = canvas.getBoundingClientRect();
  const scaleX = canvas.width / Math.max(1, rect.width);
  const scaleY = canvas.height / Math.max(1, rect.height);
  const px = (evt.clientX - rect.left) * scaleX;
  const py = (evt.clientY - rect.top) * scaleY;
  const curve = currentCurve();
  let best = -1;
  let bestDist = HIT_RADIUS * HIT_RADIUS;
  curve.forEach((point, index) => {
    const pixel = toPixel(point);
    const dx = pixel.x - px;
    const dy = pixel.y - py;
    const dist = dx * dx + dy * dy;
    if (dist <= bestDist) {
      bestDist = dist;
      best = index;
    }
  });
  return best;
}

function commitCurve(points, { immediate = false } = {}) {
  points.sort((a, b) => a[0] - b[0]);
  app.renderBackendState.hybrid.curve = points;
  app.vbapRecomputing = true;
  renderVbapStatus();
  renderHybridCurve();
  scheduleSend(immediate);
}

function scheduleSend(immediate) {
  if (sendTimer) {
    clearTimeout(sendTimer);
    sendTimer = null;
  }
  const send = () => {
    sendTimer = null;
    invoke('control_hybrid_curve', { points: currentCurve() });
  };
  if (immediate) {
    send();
  } else {
    sendTimer = setTimeout(send, 60);
  }
}

function onPointerDown(evt) {
  const index = pointIndexNear(evt);
  if (index >= 0) {
    dragIndex = index;
    canvas.setPointerCapture?.(evt.pointerId);
    evt.preventDefault();
  }
}

function onPointerMove(evt) {
  if (dragIndex < 0) {
    const hovered = pointIndexNear(evt);
    canvas.style.cursor = hovered >= 0 ? 'grab' : 'crosshair';
    return;
  }
  const curve = currentCurve().map((point) => [point[0], point[1]]);
  const [dataX, dataY] = eventToData(evt);
  const isFirst = dragIndex === 0;
  const isLast = dragIndex === curve.length - 1;
  let x = dataX;
  if (isFirst) {
    x = 0;
  } else if (isLast) {
    x = 1;
  } else {
    // Keep interior points strictly between their neighbours.
    const lo = curve[dragIndex - 1][0] + 1e-3;
    const hi = curve[dragIndex + 1][0] - 1e-3;
    x = Math.min(hi, Math.max(lo, dataX));
  }
  curve[dragIndex] = [x, dataY];
  app.renderBackendState.hybrid.curve = curve;
  app.vbapRecomputing = true;
  renderVbapStatus();
  renderHybridCurve();
  updateReadout(curve[dragIndex]);
  scheduleSend(false);
  evt.preventDefault();
}

function onPointerUp(evt) {
  if (dragIndex < 0) return;
  dragIndex = -1;
  canvas.releasePointerCapture?.(evt.pointerId);
  scheduleSend(true);
}

function onDoubleClick(evt) {
  evt.preventDefault();
  const index = pointIndexNear(evt);
  const curve = currentCurve().map((point) => [point[0], point[1]]);
  if (index >= 0) {
    // Remove, unless it is one of the two endpoints.
    if (index === 0 || index === curve.length - 1) {
      return;
    }
    curve.splice(index, 1);
    commitCurve(curve, { immediate: true });
    return;
  }
  const [dataX, dataY] = eventToData(evt);
  // Don't drop a new point on top of an endpoint's X.
  const x = Math.min(1 - 1e-3, Math.max(1e-3, dataX));
  curve.push([x, dataY]);
  commitCurve(curve, { immediate: true });
}

/** Max real distance for the active metric (normalised X is scaled by this). */
function maxDistance() {
  return app.renderBackendState.hybrid?.metric === 'spherical' ? Math.sqrt(3) : 1;
}

function updateReadout(point) {
  if (!readoutEl) return;
  if (!point) {
    readoutEl.textContent = '—';
    return;
  }
  // point[0] is the normalised X; show the real distance under the metric.
  const distance = point[0] * maxDistance();
  readoutEl.textContent = `d=${distance.toFixed(2)} → ratio=${point[1].toFixed(2)}`;
}

export function renderHybridCurve() {
  if (!canvas || !ctx) return;
  const r = plotRect();
  ctx.clearRect(0, 0, canvas.width, canvas.height);

  // Grid.
  ctx.strokeStyle = 'rgba(255,255,255,0.10)';
  ctx.lineWidth = 1;
  ctx.beginPath();
  for (let i = 0; i <= 4; i += 1) {
    const gx = r.x + (i / 4) * r.w;
    ctx.moveTo(gx, r.y);
    ctx.lineTo(gx, r.y + r.h);
    const gy = r.y + (i / 4) * r.h;
    ctx.moveTo(r.x, gy);
    ctx.lineTo(r.x + r.w, gy);
  }
  ctx.stroke();

  // Axis labels.
  ctx.fillStyle = 'rgba(255,255,255,0.5)';
  ctx.font = '10px sans-serif';
  ctx.textBaseline = 'middle';
  ctx.fillText('1', 4, r.y);
  ctx.fillText('0', 4, r.y + r.h);
  ctx.textBaseline = 'top';
  ctx.fillText('center', r.x, r.y + r.h + 4);

  // Distance reference markers. The X axis is normalised [0, 1] = [centre, max].
  // Chebyshev's max is the cube surface (1.0). Spherical's max is the cube
  // diagonal √3, so mark the real distances 1.0 (axis face) and √2 (horizontal
  // square diagonal) along the way, with the right edge labelled √3.
  const metric = app.renderBackendState.hybrid?.metric === 'spherical' ? 'spherical' : 'chebyshev';
  if (metric === 'spherical') {
    const sqrt3 = Math.sqrt(3);
    const markers = [
      { norm: 1 / sqrt3, label: '1.0' },
      { norm: Math.SQRT2 / sqrt3, label: '√2' }
    ];
    ctx.save();
    ctx.strokeStyle = 'rgba(159,220,255,0.35)';
    ctx.setLineDash([3, 3]);
    ctx.lineWidth = 1;
    markers.forEach((marker) => {
      const mx = r.x + marker.norm * r.w;
      ctx.beginPath();
      ctx.moveTo(mx, r.y);
      ctx.lineTo(mx, r.y + r.h);
      ctx.stroke();
      ctx.fillText(marker.label, mx - 6, r.y + r.h + 4);
    });
    ctx.restore();
    ctx.fillText('√3', r.x + r.w - 14, r.y + r.h + 4);
  } else {
    ctx.fillText('surface', r.x + r.w - 36, r.y + r.h + 4);
  }

  const curve = currentCurve();
  const smoothing = Math.min(1, Math.max(0, app.renderBackendState.hybrid?.curveSmoothing || 0));

  // Curve (sampled so the spline smoothing is visible).
  ctx.strokeStyle = '#9fdcff';
  ctx.lineWidth = 2;
  ctx.beginPath();
  const samples = 96;
  for (let s = 0; s <= samples; s += 1) {
    const x = s / samples;
    const pixel = toPixel([x, evalCurve(curve, smoothing, x)]);
    if (s === 0) {
      ctx.moveTo(pixel.x, pixel.y);
    } else {
      ctx.lineTo(pixel.x, pixel.y);
    }
  }
  ctx.stroke();

  // Control points.
  curve.forEach((point, index) => {
    const pixel = toPixel(point);
    const endpoint = index === 0 || index === curve.length - 1;
    ctx.beginPath();
    ctx.arc(pixel.x, pixel.y, endpoint ? 4 : 5, 0, Math.PI * 2);
    ctx.fillStyle = endpoint ? '#5fb0d6' : '#ffffff';
    ctx.fill();
  });
}

export function setupHybridCurveEditor() {
  canvas = document.getElementById('hybridCurveCanvas');
  readoutEl = document.getElementById('hybridCurveReadout');
  if (!canvas) {
    ctx = null;
    return;
  }
  ctx = canvas.getContext('2d');
  canvas.addEventListener('pointerdown', onPointerDown);
  canvas.addEventListener('pointermove', onPointerMove);
  canvas.addEventListener('pointerup', onPointerUp);
  canvas.addEventListener('pointercancel', onPointerUp);
  canvas.addEventListener('dblclick', onDoubleClick);
  renderHybridCurve();
}
