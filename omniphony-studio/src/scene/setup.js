import * as THREE from 'three';
import { OrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
import { getWindowViewport } from '../core/viewport/window-viewport.js';

// ---------------------------------------------------------------------------
// Mutable scene state
// ---------------------------------------------------------------------------

export const sceneState = { metersPerUnit: 1.0 };

// ---------------------------------------------------------------------------
// Scene, camera, renderer, controls
// ---------------------------------------------------------------------------

const initialViewport = getWindowViewport();

export const scene = new THREE.Scene();
scene.background = new THREE.Color(0x0a0b10);

export const camera = new THREE.PerspectiveCamera(65, initialViewport.width / initialViewport.height, 0.1, 100);
// Rotation pivot / listener centre. The orbit controls always rotate and zoom
// around this point; panning is applied as a separate parallel camera offset
// (see updateOrbitControls / setupHeadPivotPan) so it never moves the pivot.
export const HEAD_PIVOT = new THREE.Vector3(0, 0.25, 0);
camera.position.set(-3.8, 1.1, 0.0);
camera.lookAt(HEAD_PIVOT);

function configureRenderer(nextRenderer) {
  const viewport = getWindowViewport();
  nextRenderer.setSize(viewport.width, viewport.height);
  nextRenderer.domElement.dataset.omniphonyRenderer = 'true';
  attachRendererCanvas(nextRenderer.domElement);
  return nextRenderer;
}

const RENDERER_MOUNT_ID = 'omniphony-renderer-mount';

function getRendererMount() {
  let mount = document.getElementById(RENDERER_MOUNT_ID);
  if (mount) {
    return mount;
  }
  document.querySelectorAll('body > canvas[data-omniphony-renderer="true"]').forEach((canvas) => {
    canvas.remove();
  });
  mount = document.createElement('div');
  mount.id = RENDERER_MOUNT_ID;
  mount.style.position = 'fixed';
  mount.style.inset = '0';
  mount.style.zIndex = '0';
  mount.style.pointerEvents = 'auto';
  mount.style.overflow = 'hidden';
  // Block native HTML5 drag starting inside the 3D canvas. On WebKitGTK an
  // orbit-drag could otherwise kick off a native drag-and-drop whose drag
  // image is a compositor snapshot; dropping it aliases the sprite textures
  // with the backdrop and corrupts the labels (see
  // docs/webgl-compositor-aliasing.md). Scoped to the mount, so the speaker
  // list's own drag-to-reorder is unaffected; the mount persists across canvas
  // rebuilds so one listener is enough.
  mount.addEventListener('dragstart', (event) => event.preventDefault());
  document.body.prepend(mount);
  return mount;
}

function attachRendererCanvas(canvas) {
  const mount = getRendererMount();
  const staleChildren = Array.from(mount.children).filter((child) => child !== canvas);
  staleChildren.forEach((child) => child.remove());
  if (canvas.parentNode !== mount) {
    mount.replaceChildren(canvas);
  }
}

function disposeRendererInstance(currentRenderer) {
  try {
    currentRenderer.forceContextLoss?.();
  } catch (_error) {
    // Ignore explicit context-loss failures during teardown.
  }
  currentRenderer.dispose();
}

export let renderer = configureRenderer(new THREE.WebGLRenderer({ antialias: true }));

function createControls(domElement) {
  const nextControls = new OrbitControls(camera, domElement);
  nextControls.target.copy(HEAD_PIVOT);
  // Rotation + zoom always orbit the head. OrbitControls' own pan would move the
  // target (and thus the rotation pivot) to the view centre, so it stays off;
  // panning is handled separately (setupHeadPivotPan) as a parallel camera
  // offset that leaves the pivot on the head. The right mouse button is freed
  // for that custom pan.
  nextControls.enablePan = false;
  nextControls.mouseButtons = {
    LEFT: THREE.MOUSE.ROTATE,
    MIDDLE: THREE.MOUSE.DOLLY,
    RIGHT: null,
  };
  nextControls.enableDamping = true;
  nextControls.dampingFactor = 0.06;
  nextControls.update();
  return nextControls;
}

export let controls = createControls(renderer.domElement);

// ── Head-pivot panning ─────────────────────────────────────────────────────
// OrbitControls always orbits AND lookAt-centres its target (the head), so it
// alone can only rotate around the screen centre. To rotate around a panned
// (off-centre) head, panning is done as a lens shift: an off-centre principal
// point via camera.setViewOffset(). OrbitControls still centres the head in the
// frustum; the view offset moves where that frustum centre lands on screen.
// The head therefore stays pinned at its panned screen position through
// rotation, and the scene turns around it. Tracked in CSS pixels (1:1 drag).
let panX = 0;
let panY = 0;

/** Apply the current pan as the camera's view offset for the given surface
 *  size. Exposed so the resize path can re-apply it (the offset is relative to
 *  the full surface size). */
export function reapplyViewOffset(width, height) {
  if (panX === 0 && panY === 0) {
    camera.clearViewOffset();
  } else {
    camera.setViewOffset(width, height, panX, panY, width, height);
  }
}

function applyViewOffsetNow() {
  const viewport = getWindowViewport();
  reapplyViewOffset(viewport.width, viewport.height);
}

function addPan(dx, dy) {
  // Negative frustum offset shifts the image so the scene follows the cursor.
  panX -= dx;
  panY -= dy;
  applyViewOffsetNow();
}

/** Reset the pan so the head returns to the centre of the view. */
export function resetPan() {
  panX = 0;
  panY = 0;
  applyViewOffsetNow();
}

/** Wire right-drag panning on the renderer mount. The mount persists across
 *  canvas rebuilds, so this is called once. Move/up are on `window` so a drag
 *  that leaves the canvas still tracks. */
export function setupHeadPivotPan() {
  const mount = getRendererMount();
  let panning = false;
  let lastX = 0;
  let lastY = 0;
  // Capture phase + stopPropagation so OrbitControls (which listens on the
  // canvas child and would otherwise grab the right-button pointer) never sees
  // the drag; we own the pointer for its whole lifetime via setPointerCapture.
  mount.addEventListener(
    'pointerdown',
    (event) => {
      if (event.button !== 2 || !controls.enabled) return;
      event.stopPropagation();
      event.preventDefault();
      panning = true;
      lastX = event.clientX;
      lastY = event.clientY;
      mount.setPointerCapture?.(event.pointerId);
    },
    true,
  );
  mount.addEventListener(
    'pointermove',
    (event) => {
      if (!panning) return;
      addPan(event.clientX - lastX, event.clientY - lastY);
      lastX = event.clientX;
      lastY = event.clientY;
    },
    true,
  );
  const endPan = (event) => {
    if (!panning) return;
    panning = false;
    try {
      mount.releasePointerCapture?.(event.pointerId);
    } catch (_error) {
      // pointer already released
    }
  };
  mount.addEventListener('pointerup', endPan, true);
  mount.addEventListener('pointercancel', endPan, true);
  // Suppress the context menu so right-drag panning doesn't pop it up.
  mount.addEventListener('contextmenu', (event) => event.preventDefault());
}

setupHeadPivotPan();

export function rebuildRendererOnExistingCanvas() {
  const canvas = renderer.domElement;
  disposeRendererInstance(renderer);
  renderer = configureRenderer(new THREE.WebGLRenderer({
    antialias: true,
    canvas
  }));
  return renderer;
}

export function rebuildRendererOnFreshCanvas() {
  const previousRenderer = renderer;
  const nextRenderer = configureRenderer(new THREE.WebGLRenderer({ antialias: true }));
  controls.dispose();
  disposeRendererInstance(previousRenderer);
  renderer = nextRenderer;
  controls = createControls(renderer.domElement);
  // The camera (and its view offset) persist across the rebuild; re-apply the
  // pan view offset in case the surface size changed.
  applyViewOffsetNow();
  return renderer;
}

export function teardownRenderer(removeCanvas = false) {
  controls.dispose();
  disposeRendererInstance(renderer);
  if (removeCanvas) {
    renderer.domElement.remove();
  }
}

// ---------------------------------------------------------------------------
// Lights
// ---------------------------------------------------------------------------

const ambient = new THREE.AmbientLight(0xffffff, 0.24);
scene.add(ambient);

const directional = new THREE.DirectionalLight(0xfff7ea, 2.35);
directional.position.set(3.6, 4.8, 1.4);
scene.add(directional);

const rimDirectional = new THREE.DirectionalLight(0xb8d4ff, 1.05);
rimDirectional.position.set(-2.8, 1.1, -3.8);
scene.add(rimDirectional);

const hemisphere = new THREE.HemisphereLight(0xdcecff, 0x0d0f14, 0.12);
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

// Rotation carrier for the head model: head-tracking rotates this group (see
// scene/head-pose.js) so the fill light above stays world-aligned.
export const headPoseGroup = new THREE.Group();
brassempouyAnchor.add(headPoseGroup);

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
export const brassempouyAssetUrl = new URL('../../../omniphony-studio-egui/assets/la_dame_de_brassempouy_centered.glb', import.meta.url);
