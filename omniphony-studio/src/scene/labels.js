import * as THREE from 'three';
import { speakerLabels, app } from '../state.js';

export function createLabelSpriteBase(width, height, scaleX, scaleY, color, text) {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const texture = new THREE.CanvasTexture(canvas);
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.generateMipmaps = false;
  texture.colorSpace = THREE.SRGBColorSpace;
  const material = new THREE.SpriteMaterial({ map: texture, transparent: true, depthTest: false });
  const sprite = new THREE.Sprite(material);
  sprite.scale.set(scaleX, scaleY, 1);
  sprite.userData.labelCanvas = canvas;
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
  if (!sprite?.userData?.labelTexture || !sprite.userData.labelCanvas) {
    return;
  }
  const nextText = String(text ?? '');
  if (sprite.userData.labelText === nextText) {
    return;
  }
  const canvas = sprite.userData.labelCanvas;
  const width = canvas.width;
  const height = canvas.height;
  const color = sprite.userData.labelColor || '#ffffff';
  const isLargeCanvas = width >= 200;
  const ctx = canvas.getContext('2d');
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = color;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  const lines = nextText.split('\n');
  if (lines.length <= 1) {
    const fontSize = isLargeCanvas ? 36 : 28;
    ctx.font = `700 ${fontSize}px sans-serif`;
    ctx.fillText(lines[0] || '', width / 2, height / 2);
  } else {
    const fontSize = isLargeCanvas ? 24 : 18;
    const lineHeight = isLargeCanvas ? 24 : 18;
    const totalHeight = lineHeight * (lines.length - 1);
    const startY = height / 2 - totalHeight / 2;
    lines.forEach((line, index) => {
      const weight = index === 0 ? '700' : '600';
      ctx.font = `${weight} ${fontSize}px sans-serif`;
      ctx.fillText(line, width / 2, startY + index * lineHeight);
    });
  }
  sprite.userData.labelTexture.needsUpdate = true;
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
