import * as THREE from 'three';

export const sourceMaterial = new THREE.MeshStandardMaterial({
  color: 0xff7c4d,
  emissive: 0x64210c,
  transparent: true,
  opacity: 0.7
});
export const sourceGeometry = new THREE.SphereGeometry(0.07, 24, 24);
export const speakerGeometry = new THREE.BoxGeometry(0.08, 0.08, 0.08);
export const speakerMaterial = new THREE.MeshStandardMaterial({
  color: 0x8ec8ff,
  emissive: 0x10253a,
  transparent: true,
  opacity: 0.65
});

export const speakerBaseColor = new THREE.Color(0x8ec8ff);
export const speakerHotColor = new THREE.Color(0xff3030);
export const speakerSelectedColor = new THREE.Color(0x4dff88);
export const sourceHotColor = new THREE.Color(0xff3030);
export const sourceDefaultEmissive = new THREE.Color(0x64210c);
export const sourceNeutralEmissive = new THREE.Color(0x10161d);
export const sourceContributionEmissive = new THREE.Color(0x10311a);
export const sourceSelectedEmissive = new THREE.Color(0x9b7f22);
export const sourceOutlineColor = new THREE.Color(0xd9ecff);
export const sourceOutlineSelectedColor = new THREE.Color(0xffde8a);

export const raycaster = new THREE.Raycaster();
raycaster.params.Line.threshold = 0.08;
export const pointer = new THREE.Vector2();
