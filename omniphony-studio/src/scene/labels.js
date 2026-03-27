import * as THREE from 'three';
import { speakerLabels, app } from '../state.js';

export function escapeSvgText(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;');
}

export function buildLabelSvgDataUrl(text, color, width, height, isLarge) {
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

  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}"><g fill="${escapeSvgText(color || '#ffffff')}" font-family="sans-serif">${textMarkup}</g></svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

export function createLabelSpriteBase(width, height, scaleX, scaleY, color, text) {
  const image = new Image();
  const texture = new THREE.Texture(image);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.SRGBColorSpace;
  const material = new THREE.SpriteMaterial({ map: texture, transparent: true, depthTest: false });
  const sprite = new THREE.Sprite(material);
  sprite.scale.set(scaleX, scaleY, 1);
  sprite.userData.labelImage = image;
  sprite.userData.labelTexture = texture;
  sprite.userData.labelText = '';
  sprite.userData.labelColor = color;
  sprite.userData.labelWidth = width;
  sprite.userData.labelHeight = height;
  setLabelSpriteText(sprite, text);
  return sprite;
}

export function createLabelSprite(text) {
  return createLabelSpriteBase(256, 96, 0.42, 0.16, '#ffffff', text);
}

export function createSmallLabelSprite(text, color = '#d9ecff') {
  return createLabelSpriteBase(128, 64, 0.25, 0.12, color, text);
}

export function setLabelSpriteText(sprite, text) {
  if (!sprite?.userData?.labelTexture || !sprite.userData.labelImage) {
    return;
  }
  const nextText = String(text ?? '');
  if (sprite.userData.labelText === nextText) {
    return;
  }
  const width = Number(sprite.userData.labelWidth) || 128;
  const height = Number(sprite.userData.labelHeight) || 64;
  const isLargeCanvas = width >= 200;
  const image = sprite.userData.labelImage;
  image.onload = () => {
    if (sprite.userData.labelImage === image && sprite.userData.labelTexture) {
      sprite.userData.labelTexture.needsUpdate = true;
    }
  };
  image.src = buildLabelSvgDataUrl(
    nextText,
    sprite.userData.labelColor || '#ffffff',
    width,
    height,
    isLargeCanvas
  );
  sprite.userData.labelText = nextText;
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
