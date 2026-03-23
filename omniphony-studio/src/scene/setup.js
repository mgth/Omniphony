import * as THREE from 'three';
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';

// ---------------------------------------------------------------------------
// Mutable scene state
// ---------------------------------------------------------------------------

export const sceneState = { metersPerUnit: 1.0 };

// ---------------------------------------------------------------------------
// Scene, camera, renderer, controls
// ---------------------------------------------------------------------------

export const scene = new THREE.Scene();
scene.background = new THREE.Color(0x0a0b10);

export const camera = new THREE.PerspectiveCamera(65, window.innerWidth / window.innerHeight, 0.1, 100);
camera.position.set(-3.8, 1.1, 0.0);
camera.lookAt(0, 0.25, 0);

export const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setSize(window.innerWidth, window.innerHeight);
document.body.appendChild(renderer.domElement);

export const controls = new OrbitControls(camera, renderer.domElement);
controls.target.set(0, 0.25, 0);
controls.enableDamping = true;
controls.dampingFactor = 0.06;
controls.update();

// ---------------------------------------------------------------------------
// Lights
// ---------------------------------------------------------------------------

const ambient = new THREE.AmbientLight(0xffffff, 0.78);
scene.add(ambient);

const directional = new THREE.DirectionalLight(0xffffff, 1.15);
directional.position.set(3, 4, 2);
scene.add(directional);

const hemisphere = new THREE.HemisphereLight(0xeaf6ff, 0x1a1d24, 0.55);
scene.add(hemisphere);

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

export const roomGroup = new THREE.Group();
scene.add(roomGroup);
export const roomDimensionGroup = new THREE.Group();
scene.add(roomDimensionGroup);
export const brassempouyAnchor = new THREE.Group();
roomGroup.add(brassempouyAnchor);

export const brassempouyFill = new THREE.PointLight(0xfff4dc, 0.9, 2.2, 2);
brassempouyFill.position.set(-0.18, 0.42, 0.22);
brassempouyAnchor.add(brassempouyFill);

// ---------------------------------------------------------------------------
// Room box
// ---------------------------------------------------------------------------

const roomGeometry = new THREE.BoxGeometry(2, 1, 2);
export const room = new THREE.Mesh(
  roomGeometry,
  new THREE.MeshBasicMaterial({ color: 0x4d6eff, transparent: true, opacity: 0.08, depthWrite: false })
);
room.position.y = 0.5;
roomGroup.add(room);

export const roomEdges = new THREE.LineSegments(
  new THREE.EdgesGeometry(roomGeometry),
  new THREE.LineBasicMaterial({ color: 0x6f8dff, linewidth: 2, transparent: true, opacity: 0.45, depthTest: false })
);
roomGroup.add(roomEdges);

// ---------------------------------------------------------------------------
// Room face materials
// ---------------------------------------------------------------------------

export const roomFaceMaterial = new THREE.MeshBasicMaterial({
  color: 0x233047,
  transparent: true,
  opacity: 0.18,
  side: THREE.DoubleSide,
  depthWrite: false,
  depthTest: false,
  polygonOffset: true,
  polygonOffsetFactor: 1,
  polygonOffsetUnits: 1
});

export const screenMaterial = new THREE.MeshBasicMaterial({
  color: 0xffffff,
  transparent: true,
  opacity: 0.18,
  side: THREE.DoubleSide,
  depthWrite: false,
  depthTest: false
});

// ---------------------------------------------------------------------------
// Room faces
// ---------------------------------------------------------------------------

const roomFaceSideGeometry = new THREE.PlaneGeometry(2, 1);
const roomFaceCapGeometry = new THREE.PlaneGeometry(2, 2);
export const roomFaces = {
  posX: new THREE.Mesh(roomFaceSideGeometry, roomFaceMaterial),
  negX: new THREE.Mesh(roomFaceSideGeometry, roomFaceMaterial),
  posY: new THREE.Mesh(roomFaceCapGeometry, roomFaceMaterial),
  negY: new THREE.Mesh(roomFaceCapGeometry, roomFaceMaterial),
  posZ: new THREE.Mesh(roomFaceSideGeometry, roomFaceMaterial),
  negZ: new THREE.Mesh(roomFaceSideGeometry, roomFaceMaterial)
};

roomFaces.posX.rotation.y = -Math.PI / 2;
roomFaces.posX.position.set(1, 0.5, 0);
roomFaces.posX.renderOrder = 1;
roomGroup.add(roomFaces.posX);

roomFaces.negX.rotation.y = Math.PI / 2;
roomFaces.negX.position.set(-1, 0.5, 0);
roomFaces.negX.renderOrder = 1;
roomGroup.add(roomFaces.negX);

roomFaces.posY.rotation.x = -Math.PI / 2;
roomFaces.posY.position.set(0, 1, 0);
roomFaces.posY.renderOrder = 1;
roomGroup.add(roomFaces.posY);

roomFaces.negY.rotation.x = Math.PI / 2;
roomFaces.negY.position.set(0, 0, 0);
roomFaces.negY.renderOrder = 1;
roomGroup.add(roomFaces.negY);

roomFaces.posZ.position.set(0, 0.5, 1);
roomFaces.posZ.renderOrder = 1;
roomGroup.add(roomFaces.posZ);

roomFaces.negZ.rotation.y = Math.PI;
roomFaces.negZ.position.set(0, 0.5, -1);
roomFaces.negZ.renderOrder = 1;
roomGroup.add(roomFaces.negZ);

export const roomFaceDefs = [
  { key: 'posX', mesh: roomFaces.posX, inward: new THREE.Vector3(-1, 0, 0) },
  { key: 'negX', mesh: roomFaces.negX, inward: new THREE.Vector3(1, 0, 0) },
  { key: 'posY', mesh: roomFaces.posY, inward: new THREE.Vector3(0, -1, 0) },
  { key: 'negY', mesh: roomFaces.negY, inward: new THREE.Vector3(0, 1, 0) },
  { key: 'posZ', mesh: roomFaces.posZ, inward: new THREE.Vector3(0, 0, -1) },
  { key: 'negZ', mesh: roomFaces.negZ, inward: new THREE.Vector3(0, 0, 1) }
];

// ---------------------------------------------------------------------------
// Temp vectors (reused every frame for face-transparency sorting)
// ---------------------------------------------------------------------------

export const tempCameraLocal = new THREE.Vector3();
export const tempToCamera = new THREE.Vector3();
export const tempToCenter = new THREE.Vector3();

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

export const SCREEN_ASPECT = 16 / 9;
export const SCREEN_BASE_WIDTH = 2;
export const SCREEN_BASE_HEIGHT = 2 * (9 / 16);
export const SCREEN_MAX_WIDTH = 2;
export const SCREEN_MAX_HEIGHT_UPPER_HALF = 1;

const screenGeometry = new THREE.PlaneGeometry(SCREEN_BASE_WIDTH, SCREEN_BASE_HEIGHT);
export const screenMesh = new THREE.Mesh(screenGeometry, screenMaterial);
screenMesh.rotation.y = -Math.PI / 2;
screenMesh.position.set(0.995, 0.5, 0);
screenMesh.renderOrder = 5;
roomGroup.add(screenMesh);

// ---------------------------------------------------------------------------
// Room bounds (mutable — updated when room ratio changes)
// ---------------------------------------------------------------------------

export const roomBounds = {
  xMin: -1,
  xMax: 1,
  yMin: -0.5,
  yMax: 1,
  zMin: -1,
  zMax: 1
};

// ---------------------------------------------------------------------------
// fitScreenToUpperHalf
// ---------------------------------------------------------------------------

export function fitScreenToUpperHalf() {
  const availableWidth = Math.max(0.01, roomBounds.zMax - roomBounds.zMin);
  const availableHeight = Math.max(0.01, roomBounds.yMax - roomBounds.yMin);
  let height = SCREEN_MAX_HEIGHT_UPPER_HALF;
  let width = height * SCREEN_ASPECT;
  if (height > availableHeight) {
    height = availableHeight;
    width = height * SCREEN_ASPECT;
  }
  if (width > SCREEN_MAX_WIDTH) {
    width = SCREEN_MAX_WIDTH;
    height = width / SCREEN_ASPECT;
  }
  if (width > availableWidth) {
    width = availableWidth;
    height = width / SCREEN_ASPECT;
  }
  screenMesh.scale.set(width / SCREEN_BASE_WIDTH, height / SCREEN_BASE_HEIGHT, 1);
  screenMesh.position.set(
    roomBounds.xMax - 0.005,
    roomBounds.yMin + (availableHeight * 0.5),
    (roomBounds.zMin + roomBounds.zMax) * 0.5
  );
}

fitScreenToUpperHalf();
roomDimensionGroup.visible = false;

// ---------------------------------------------------------------------------
// Brassempouy model constants (loading happens in main app)
// ---------------------------------------------------------------------------

export const BRASSEMPOUY_TARGET_MAX_DIMENSION = 0.34;
export const brassempouyAssetUrl = new URL('../assets/la_dame_de_brassempouy_centered.glb', import.meta.url);
