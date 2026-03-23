import * as THREE from 'three';
import { scene } from './setup.js';
import { createSmallLabelSprite } from './labels.js';

export function createSceneAxes() {
  const group = new THREE.Group();
  const axisGap = 0.3;
  const axisExtent = 0.58;
  const arrowRadius = 0.02;
  const arrowLength = 0.075;
  const labelOffset = 0.12;
  const specs = [
    { axis: 'y', color: 0xff6b6b, dir: new THREE.Vector3(1, 0, 0), rot: new THREE.Euler(0, 0, -Math.PI / 2) },
    { axis: 'z', color: 0x7fff7f, dir: new THREE.Vector3(0, 1, 0), rot: new THREE.Euler(0, 0, 0) },
    { axis: 'x', color: 0x6bb8ff, dir: new THREE.Vector3(0, 0, 1), rot: new THREE.Euler(Math.PI / 2, 0, 0) }
  ];

  specs.forEach(({ axis, color, dir, rot }) => {
    const material = new THREE.LineBasicMaterial({
      color,
      transparent: true,
      opacity: 0.85,
      depthTest: false
    });
    const positive = new THREE.Line(
      new THREE.BufferGeometry().setFromPoints([
        dir.clone().multiplyScalar(axisGap),
        dir.clone().multiplyScalar(axisExtent)
      ]),
      material.clone()
    );
    const negative = new THREE.Line(
      new THREE.BufferGeometry().setFromPoints([
        dir.clone().multiplyScalar(-axisGap),
        dir.clone().multiplyScalar(-axisExtent)
      ]),
      material.clone()
    );
    positive.renderOrder = 30;
    negative.renderOrder = 30;
    group.add(positive);
    group.add(negative);

    const arrow = new THREE.Mesh(
      new THREE.ConeGeometry(arrowRadius, arrowLength, 14),
      new THREE.MeshBasicMaterial({ color, transparent: true, opacity: 0.92, depthTest: false })
    );
    arrow.position.copy(dir.clone().multiplyScalar(axisExtent + arrowLength * 0.45));
    arrow.rotation.copy(rot);
    arrow.renderOrder = 31;
    group.add(arrow);

    const label = createSmallLabelSprite(axis.toUpperCase(), `#${color.toString(16).padStart(6, '0')}`);
    label.position.copy(dir.clone().multiplyScalar(axisExtent + arrowLength + labelOffset));
    label.renderOrder = 32;
    group.add(label);
  });

  return group;
}

const axes = createSceneAxes();
scene.add(axes);

export { axes };
