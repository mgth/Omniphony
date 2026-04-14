import * as THREE from 'three';
import { sourceLabels, speakerLabels, app } from '../state.js';

const labelDebugStats = {
  created: 0,
  textureRebuilds: 0,
  textUpdates: 0,
  textSkips: 0,
  imageLoads: 0,
  recentEvents: []
};

let nextLabelDebugId = 1;

function labelDebugTarget(sprite) {
  if (!sprite?.userData) {
    return null;
  }
  if (sprite.userData.sourceId !== undefined) {
    return `source:${sprite.userData.sourceId}`;
  }
  if (sprite.userData.speakerIndex !== undefined) {
    return `speaker:${sprite.userData.speakerIndex}`;
  }
  return sprite.userData.labelDebugId ? `label:${sprite.userData.labelDebugId}` : null;
}

function pushLabelDebugEvent(type, sprite, extra = {}) {
  const event = {
    t: Date.now(),
    type,
    target: labelDebugTarget(sprite),
    text: sprite?.userData?.labelText ?? null,
    ...extra
  };
  labelDebugStats.recentEvents.push(event);
  if (labelDebugStats.recentEvents.length > 80) {
    labelDebugStats.recentEvents.shift();
  }
}

function attachLabelDebugHandle() {
  if (typeof window === 'undefined') {
    return;
  }
  const existing = window.omniphonyDebug && typeof window.omniphonyDebug === 'object'
    ? window.omniphonyDebug
    : {};
  window.omniphonyDebug = {
    ...existing,
    labelStats: labelDebugStats,
    summarizeLabels() {
      const sourceTargets = new Map();
      const speakerTargets = new Map();
      sourceLabels.forEach((label, id) => {
        sourceTargets.set(`source:${id}`, (sourceTargets.get(`source:${id}`) || 0) + 1);
      });
      speakerLabels.forEach((label, index) => {
        if (!label) {
          return;
        }
        const key = `speaker:${label.userData?.speakerIndex ?? index}`;
        speakerTargets.set(key, (speakerTargets.get(key) || 0) + 1);
      });
      const duplicateSources = Array.from(sourceTargets.entries()).filter(([, count]) => count > 1);
      const duplicateSpeakers = Array.from(speakerTargets.entries()).filter(([, count]) => count > 1);
      return {
        sourceLabelCount: sourceLabels.size,
        speakerLabelCount: speakerLabels.filter(Boolean).length,
        duplicateSources,
        duplicateSpeakers,
        stats: { ...labelDebugStats }
      };
    },
    resetLabelStats() {
      labelDebugStats.created = 0;
      labelDebugStats.textureRebuilds = 0;
      labelDebugStats.textUpdates = 0;
      labelDebugStats.textSkips = 0;
      labelDebugStats.imageLoads = 0;
      labelDebugStats.recentEvents.length = 0;
    }
  };
}

attachLabelDebugHandle();

export function escapeSvgText(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;');
}

export function buildLabelSvgDataUrl(text, color, width, height, isLarge) {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(buildLabelSvgMarkup(text, color, width, height, isLarge))}`;
}

export function buildLabelSvgMarkup(text, color, width, height, isLarge) {
  const lines = String(text ?? '').split('\n');
  const fontSize = isLarge ? 36 : 28;
  const baseFontSize = isLarge ? 24 : 18;
  const lineHeight = isLarge ? 24 : 18;
  let textMarkup = '';

  if (lines.length <= 1) {
    textMarkup = `<text x="50%" y="50%" text-anchor="middle" dominant-baseline="middle" font-size="${fontSize}" font-weight="700">${escapeSvgText(lines[0] || '')}</text>`;
  } else {
    const totalHeight = lineHeight * (lines.length - 1);
    const startY = (height / 2) - (totalHeight / 2);
    textMarkup = lines.map((line, index) => {
      const weight = index === 0 ? 700 : 600;
      const y = startY + (index * lineHeight);
      return `<text x="50%" y="${y}" text-anchor="middle" dominant-baseline="middle" font-size="${baseFontSize}" font-weight="${weight}">${escapeSvgText(line)}</text>`;
    }).join('');
  }

  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}"><g fill="${escapeSvgText(color || '#ffffff')}" font-family="sans-serif">${textMarkup}</g></svg>`;
}

function createLabelCanvas(width, height) {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  return canvas;
}

function createLabelCanvasTexture(canvas) {
  const texture = new THREE.CanvasTexture(canvas);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.SRGBColorSpace;
  return texture;
}

function drawLabelTextToCanvas(sprite, text) {
  const canvas = sprite?.userData?.labelCanvas;
  const ctx = sprite?.userData?.labelCtx;
  if (!canvas || !ctx) {
    return;
  }

  const lines = String(text ?? '').split('\n');
  const isLargeCanvas = canvas.width >= 200;
  const fontSize = isLargeCanvas ? 36 : 28;
  const baseFontSize = isLargeCanvas ? 24 : 18;
  const lineHeight = isLargeCanvas ? 24 : 18;

  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = sprite.userData.labelColor || '#ffffff';

  if (lines.length <= 1) {
    ctx.font = `700 ${fontSize}px sans-serif`;
    ctx.fillText(lines[0] || '', canvas.width / 2, canvas.height / 2);
    return;
  }

  const totalHeight = lineHeight * (lines.length - 1);
  const startY = (canvas.height / 2) - (totalHeight / 2);
  lines.forEach((line, index) => {
    const weight = index === 0 ? 700 : 600;
    const y = startY + (index * lineHeight);
    ctx.font = `${weight} ${baseFontSize}px sans-serif`;
    ctx.fillText(line, canvas.width / 2, y);
  });
}

export function createLabelSpriteBase(width, height, scaleX, scaleY, color, text) {
  const canvas = createLabelCanvas(width, height);
  const texture = createLabelCanvasTexture(canvas);
  const material = new THREE.SpriteMaterial({
    map: texture,
    transparent: true,
    alphaTest: 0.25,
    depthTest: false,
    depthWrite: false,
    toneMapped: false
  });
  const sprite = new THREE.Sprite(material);
  sprite.scale.set(scaleX, scaleY, 1);
  sprite.frustumCulled = false;
  sprite.renderOrder = 40;
  sprite.userData.labelDebugId = nextLabelDebugId++;
  sprite.userData.labelCanvas = canvas;
  sprite.userData.labelCtx = canvas.getContext('2d');
  sprite.userData.labelTexture = texture;
  sprite.userData.labelText = '';
  sprite.userData.labelColor = color;
  sprite.userData.labelWidth = width;
  sprite.userData.labelHeight = height;
  sprite.userData.labelDisposed = false;
  labelDebugStats.created += 1;
  pushLabelDebugEvent('create', sprite, { width, height });
  setLabelSpriteText(sprite, text);
  return sprite;
}

export function createLabelSprite(text) {
  return createLabelSpriteBase(256, 96, 0.42, 0.16, '#ffffff', text);
}

export function createSmallLabelSprite(text, color = '#d9ecff') {
  return createLabelSpriteBase(128, 64, 0.25, 0.12, color, text);
}

export function createLabelTexture() {
  const canvas = createLabelCanvas(128, 64);
  return createLabelCanvasTexture(canvas);
}

export function createLabelTextureFromImageSource(imageSource) {
  if (imageSource instanceof HTMLCanvasElement) {
    return createLabelCanvasTexture(imageSource);
  }
  return createLabelCanvasTexture(createLabelCanvas(128, 64));
}

export function rebuildLabelSpriteTexture(sprite) {
  if (!sprite?.material || sprite.userData?.labelDisposed) {
    return;
  }
  labelDebugStats.textureRebuilds += 1;
  pushLabelDebugEvent('rebuild-texture', sprite);
  disposeLabelTextureResources(sprite);
  const canvas = sprite.userData.labelCanvas
    ?? createLabelCanvas(
      Number(sprite.userData.labelWidth) || 128,
      Number(sprite.userData.labelHeight) || 64
    );
  const ctx = sprite.userData.labelCtx ?? canvas.getContext('2d');
  sprite.userData.labelCanvas = canvas;
  sprite.userData.labelCtx = ctx;
  const texture = createLabelCanvasTexture(canvas);
  sprite.userData.labelTexture = texture;
  sprite.material.map = texture;
  sprite.material.needsUpdate = true;
  drawLabelTextToCanvas(sprite, sprite.userData.labelText ?? '');
  texture.needsUpdate = true;
}

function disposeLabelImageAsset(_asset) {
  // Labels now render directly to canvas; there is no external image asset.
}

function disposeLabelCanvasResources(sprite) {
  if (!sprite?.userData) {
    return;
  }
  sprite.userData.labelCtx = null;
  sprite.userData.labelCanvas = null;
}

function disposeLabelTextureResources(sprite) {
  if (!sprite?.userData) {
    return;
  }
  const oldTexture = sprite.userData.labelTexture;
  sprite.userData.labelTexture = null;
  if (sprite.material?.map === oldTexture) {
    sprite.material.map = null;
    sprite.material.needsUpdate = true;
  }
  if (oldTexture) {
    oldTexture.dispose();
  }
  disposeLabelImageAsset(null);
}

function adoptLabelTexture(sprite, texture, asset) {
  const previousTexture = sprite.userData.labelTexture;
  sprite.userData.labelTexture = texture;
  sprite.material.map = texture;
  sprite.material.needsUpdate = true;
  if (previousTexture && previousTexture !== texture) {
    previousTexture.dispose();
  }
  disposeLabelImageAsset(asset);
}

export function disposeLabelSprite(sprite) {
  if (!sprite?.material || !sprite.userData) {
    return;
  }
  sprite.userData.labelDisposed = true;
  disposeLabelTextureResources(sprite);
  disposeLabelCanvasResources(sprite);
  sprite.material.dispose();
}

export function setLabelSpriteText(sprite, text) {
  if (!sprite?.material || !sprite.userData || sprite.userData.labelDisposed) {
    return;
  }
  const nextText = String(text ?? '');
  if (sprite.userData.labelText === nextText) {
    labelDebugStats.textSkips += 1;
    return;
  }
  labelDebugStats.textUpdates += 1;
  pushLabelDebugEvent('set-text', sprite, { nextText });
  sprite.userData.labelText = nextText;
  drawLabelTextToCanvas(sprite, nextText);
  if (sprite.userData.labelTexture) {
    labelDebugStats.imageLoads += 1;
    pushLabelDebugEvent('image-load', sprite);
    sprite.userData.labelTexture.needsUpdate = true;
  }
}

export function updateSpeakerLabelsFromSelection() {
  speakerLabels.forEach((label, index) => {
    const speaker = app.currentLayoutSpeakers[index];
    if (!label || !speaker) {
      return;
    }
    const speakerName = String(speaker.id || index);
    setLabelSpriteText(label, speakerName);
  });
}
