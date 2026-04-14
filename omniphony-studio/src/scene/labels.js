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

export function createLabelSpriteBase(width, height, scaleX, scaleY, color, text) {
  const texture = createLabelTexture();
  const material = new THREE.SpriteMaterial({
    map: texture,
    transparent: false,
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
  sprite.userData.labelTexture = texture;
  sprite.userData.labelImage = texture.image;
  sprite.userData.labelImageAsset = texture.image;
  sprite.userData.labelText = '';
  sprite.userData.labelColor = color;
  sprite.userData.labelWidth = width;
  sprite.userData.labelHeight = height;
  sprite.userData.labelRequestId = 0;
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
  return createLabelTextureFromImageSource(null);
}

export function createLabelTextureFromImageSource(imageSource) {
  const image = imageSource ?? new Image();
  const texture = new THREE.Texture(image);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.SRGBColorSpace;
  return texture;
}

export function rebuildLabelSpriteTexture(sprite) {
  if (!sprite?.material || sprite.userData?.labelDisposed) {
    return;
  }
  labelDebugStats.textureRebuilds += 1;
  pushLabelDebugEvent('rebuild-texture', sprite);
  sprite.userData.labelRequestId = (Number(sprite.userData.labelRequestId) || 0) + 1;
  disposeLabelTextureResources(sprite);
  const nextText = String(sprite.userData?.labelText ?? '');
  sprite.userData.labelText = '';
  setLabelSpriteText(sprite, nextText);
}

function disposeLabelImageAsset(asset) {
  if (asset && typeof asset.close === 'function') {
    asset.close();
  }
}

function disposeLabelTextureResources(sprite) {
  if (!sprite?.userData) {
    return;
  }
  const oldTexture = sprite.userData.labelTexture;
  const oldAsset = sprite.userData.labelImageAsset;
  sprite.userData.labelTexture = null;
  sprite.userData.labelImage = null;
  sprite.userData.labelImageAsset = null;
  if (sprite.material?.map === oldTexture) {
    sprite.material.map = null;
    sprite.material.needsUpdate = true;
  }
  if (oldTexture) {
    oldTexture.dispose();
  }
  disposeLabelImageAsset(oldAsset);
}

function adoptLabelTexture(sprite, texture, asset) {
  const previousTexture = sprite.userData.labelTexture;
  const previousAsset = sprite.userData.labelImageAsset;
  sprite.userData.labelTexture = texture;
  sprite.userData.labelImage = texture.image;
  sprite.userData.labelImageAsset = asset;
  sprite.material.map = texture;
  sprite.material.needsUpdate = true;
  if (previousTexture && previousTexture !== texture) {
    previousTexture.dispose();
  }
  if (previousAsset && previousAsset !== asset) {
    disposeLabelImageAsset(previousAsset);
  }
}

export function disposeLabelSprite(sprite) {
  if (!sprite?.material || !sprite.userData) {
    return;
  }
  sprite.userData.labelDisposed = true;
  sprite.userData.labelRequestId = (Number(sprite.userData.labelRequestId) || 0) + 1;
  disposeLabelTextureResources(sprite);
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
  const width = Number(sprite.userData.labelWidth) || 128;
  const height = Number(sprite.userData.labelHeight) || 64;
  const isLargeCanvas = width >= 200;
  const requestId = (Number(sprite.userData.labelRequestId) || 0) + 1;
  const nextTexture = createLabelTexture();
  const image = nextTexture.image;
  sprite.userData.labelRequestId = requestId;
  sprite.userData.labelText = nextText;
  image.onload = () => {
    const currentRequestId = Number(sprite.userData?.labelRequestId) || 0;
    if (sprite.userData?.labelDisposed || currentRequestId !== requestId) {
      nextTexture.dispose();
      disposeLabelImageAsset(image);
      return;
    }
    labelDebugStats.imageLoads += 1;
    pushLabelDebugEvent('image-load', sprite);
    nextTexture.needsUpdate = true;
    adoptLabelTexture(sprite, nextTexture, image);
  };
  image.onerror = () => {
    const currentRequestId = Number(sprite.userData?.labelRequestId) || 0;
    if (sprite.userData?.labelDisposed || currentRequestId !== requestId) {
      nextTexture.dispose();
      disposeLabelImageAsset(image);
      return;
    }
    nextTexture.dispose();
    pushLabelDebugEvent('image-load-error', sprite, { message: 'label image load failed' });
  };
  image.src = buildLabelSvgDataUrl(
    nextText,
    sprite.userData.labelColor || '#ffffff',
    width,
    height,
    isLargeCanvas
  );
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
