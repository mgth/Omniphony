import * as THREE from 'https://unpkg.com/three@0.165.0/build/three.module.js?module';
import { OrbitControls } from 'https://unpkg.com/three@0.165.0/examples/jsm/controls/OrbitControls.js?module';

const statusEl = document.getElementById('status');
const layoutSelectEl = document.getElementById('layoutSelect');
const speakersListEl = document.getElementById('speakersList');
const objectsListEl = document.getElementById('objectsList');
const speakersSectionEl = document.getElementById('speakersSection');
const objectsSectionEl = document.getElementById('objectsSection');
const roomRatioEl = document.getElementById('roomRatio');
const spreadInfoEl = document.getElementById('spreadInfo');
const loudnessInfoEl = document.getElementById('loudnessInfo');
const latencyInfoEl = document.getElementById('latencyInfo');
const resampleRatioInfoEl = document.getElementById('resampleRatioInfo');
const loudnessToggleEl = document.getElementById('loudnessToggle');
const spreadMinSliderEl = document.getElementById('spreadMinSlider');
const spreadMaxSliderEl = document.getElementById('spreadMaxSlider');
const spreadMinValEl = document.getElementById('spreadMinVal');
const spreadMaxValEl = document.getElementById('spreadMaxVal');
const latencyMeterFillEl = document.getElementById('latencyMeterFill');
const masterGainSliderEl = document.getElementById('masterGainSlider');
const masterGainBoxEl = document.getElementById('masterGainBox');
const masterMeterTextEl = document.getElementById('masterMeterText');
const masterMeterFillEl = document.getElementById('masterMeterFill');
const distanceModelSelectEl = document.getElementById('distanceModelSelect');
const editModeSelectEl = document.getElementById('editModeSelect');
const distanceDiffuseToggleEl = document.getElementById('distanceDiffuseToggle');
const distanceDiffuseThresholdSliderEl = document.getElementById('distanceDiffuseThresholdSlider');
const distanceDiffuseThresholdValEl = document.getElementById('distanceDiffuseThresholdVal');
const distanceDiffuseCurveSliderEl = document.getElementById('distanceDiffuseCurveSlider');
const distanceDiffuseCurveValEl = document.getElementById('distanceDiffuseCurveVal');
const saveConfigBtnEl = document.getElementById('saveConfigBtn');
const configSavedIndicatorEl = document.getElementById('configSavedIndicator');
const trailToggleEl = document.getElementById('trailToggle');
const trailLengthSliderEl = document.getElementById('trailLengthSlider');
const trailLengthValEl = document.getElementById('trailLengthVal');

const scene = new THREE.Scene();
scene.background = new THREE.Color(0x0a0b10);

const camera = new THREE.PerspectiveCamera(65, window.innerWidth / window.innerHeight, 0.1, 100);
camera.position.set(-3.8, 1.1, 0.0);
camera.lookAt(0, 0.25, 0);

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setSize(window.innerWidth, window.innerHeight);
document.body.appendChild(renderer.domElement);

const controls = new OrbitControls(camera, renderer.domElement);
controls.target.set(0, 0.25, 0);
controls.enableDamping = true;
controls.dampingFactor = 0.06;
controls.update();

const ambient = new THREE.AmbientLight(0xffffff, 0.6);
scene.add(ambient);

const directional = new THREE.DirectionalLight(0xffffff, 1);
directional.position.set(3, 4, 2);
scene.add(directional);

const roomGroup = new THREE.Group();
scene.add(roomGroup);

const roomGeometry = new THREE.BoxGeometry(2, 2, 2);
const room = new THREE.Mesh(
  roomGeometry,
  new THREE.MeshBasicMaterial({ color: 0x4d6eff, transparent: true, opacity: 0.08, depthWrite: false })
);
roomGroup.add(room);

const roomEdges = new THREE.LineSegments(
  new THREE.EdgesGeometry(roomGeometry),
  new THREE.LineBasicMaterial({ color: 0x6f8dff, linewidth: 2, transparent: true, opacity: 0.45, depthTest: false })
);
roomGroup.add(roomEdges);

const roomFaceMaterial = new THREE.MeshBasicMaterial({
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

const screenMaterial = new THREE.MeshBasicMaterial({
  color: 0xffffff,
  transparent: true,
  opacity: 0.18,
  side: THREE.DoubleSide,
  depthWrite: false,
  depthTest: false
});

const roomFaceGeometry = new THREE.PlaneGeometry(2, 2);
const roomFaces = {
  posX: new THREE.Mesh(roomFaceGeometry, roomFaceMaterial),
  negX: new THREE.Mesh(roomFaceGeometry, roomFaceMaterial),
  posY: new THREE.Mesh(roomFaceGeometry, roomFaceMaterial),
  negY: new THREE.Mesh(roomFaceGeometry, roomFaceMaterial),
  posZ: new THREE.Mesh(roomFaceGeometry, roomFaceMaterial),
  negZ: new THREE.Mesh(roomFaceGeometry, roomFaceMaterial)
};

roomFaces.posX.rotation.y = -Math.PI / 2;
roomFaces.posX.position.set(1, 0, 0);
roomFaces.posX.renderOrder = 1;
roomGroup.add(roomFaces.posX);

roomFaces.negX.rotation.y = Math.PI / 2;
roomFaces.negX.position.set(-1, 0, 0);
roomFaces.negX.renderOrder = 1;
roomGroup.add(roomFaces.negX);

roomFaces.posY.rotation.x = -Math.PI / 2;
roomFaces.posY.position.set(0, 1, 0);
roomFaces.posY.renderOrder = 1;
roomGroup.add(roomFaces.posY);

roomFaces.negY.rotation.x = Math.PI / 2;
roomFaces.negY.position.set(0, -1, 0);
roomFaces.negY.renderOrder = 1;
roomGroup.add(roomFaces.negY);

roomFaces.posZ.position.set(0, 0, 1);
roomFaces.posZ.renderOrder = 1;
roomGroup.add(roomFaces.posZ);

roomFaces.negZ.rotation.y = Math.PI;
roomFaces.negZ.position.set(0, 0, -1);
roomFaces.negZ.renderOrder = 1;
roomGroup.add(roomFaces.negZ);

const roomFaceDefs = [
  { key: 'posX', mesh: roomFaces.posX, inward: new THREE.Vector3(-1, 0, 0) },
  { key: 'negX', mesh: roomFaces.negX, inward: new THREE.Vector3(1, 0, 0) },
  { key: 'posY', mesh: roomFaces.posY, inward: new THREE.Vector3(0, -1, 0) },
  { key: 'negY', mesh: roomFaces.negY, inward: new THREE.Vector3(0, 1, 0) },
  { key: 'posZ', mesh: roomFaces.posZ, inward: new THREE.Vector3(0, 0, -1) },
  { key: 'negZ', mesh: roomFaces.negZ, inward: new THREE.Vector3(0, 0, 1) }
];

const tempCameraLocal = new THREE.Vector3();
const tempToCamera = new THREE.Vector3();
const tempToCenter = new THREE.Vector3();

const screenGeometry = new THREE.PlaneGeometry(2, 2 * (9 / 16));
const screenMesh = new THREE.Mesh(screenGeometry, screenMaterial);
screenMesh.rotation.y = -Math.PI / 2;
screenMesh.position.set(0.995, 0, 0);
screenMesh.renderOrder = 5;
roomGroup.add(screenMesh);

const roomRatio = { width: 1, length: 2, height: 1 };
const spreadState = { min: null, max: null };
const distanceDiffuseState = { enabled: null, threshold: null, curve: null };
let distanceModel = 'none';
let configSaved = null;
let loudnessEnabled = null;
let loudnessSource = null;
let loudnessGain = null;
let latencyMs = null;
let resampleRatio = null;
let masterGain = 1;

const axes = new THREE.AxesHelper(1.2);
scene.add(axes);

const ringPoints = Array.from({ length: 64 }, (_, i) => {
  const a = (i / 64) * Math.PI * 2;
  return new THREE.Vector3(Math.cos(a), 0, Math.sin(a));
});
const arcPoints = Array.from({ length: 48 }, (_, i) => {
  const t = (i / 47) * Math.PI - Math.PI / 2;
  return new THREE.Vector3(Math.cos(t), Math.sin(t), 0);
});

const ringTickPoints = [];
for (let i = 0; i < 72; i += 1) {
  const a = (i / 72) * Math.PI * 2;
  const inner = 1.0;
  const outer = 1.08;
  ringTickPoints.push(new THREE.Vector3(Math.cos(a) * inner, 0, Math.sin(a) * inner));
  ringTickPoints.push(new THREE.Vector3(Math.cos(a) * outer, 0, Math.sin(a) * outer));
}

const ringMinorTickPoints = [];
for (let i = 0; i < 360; i += 1) {
  const a = (i / 360) * Math.PI * 2;
  const inner = 1.01;
  const outer = 1.05;
  ringMinorTickPoints.push(new THREE.Vector3(Math.cos(a) * inner, 0, Math.sin(a) * inner));
  ringMinorTickPoints.push(new THREE.Vector3(Math.cos(a) * outer, 0, Math.sin(a) * outer));
}

const arcTickPoints = [];
for (let angle = -90; angle <= 90; angle += 5) {
  const t = (angle * Math.PI) / 180;
  const inner = 1.0;
  const outer = 1.08;
  arcTickPoints.push(new THREE.Vector3(Math.cos(t) * inner, Math.sin(t) * inner, 0));
  arcTickPoints.push(new THREE.Vector3(Math.cos(t) * outer, Math.sin(t) * outer, 0));
}

const arcMinorTickPoints = [];
for (let i = 0; i <= 180; i += 1) {
  const t = (i / 180) * Math.PI - Math.PI / 2;
  const inner = 1.01;
  const outer = 1.05;
  arcMinorTickPoints.push(new THREE.Vector3(Math.cos(t) * inner, Math.sin(t) * inner, 0));
  arcMinorTickPoints.push(new THREE.Vector3(Math.cos(t) * outer, Math.sin(t) * outer, 0));
}

const speakerGizmo = {
  ring: new THREE.LineLoop(
    new THREE.BufferGeometry().setFromPoints(ringPoints),
    new THREE.LineBasicMaterial({ color: 0x9ef7ff, transparent: true, opacity: 0.6 })
  ),
  ringTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(ringTickPoints),
    new THREE.LineBasicMaterial({ color: 0x9ef7ff, transparent: true, opacity: 0.5 })
  ),
  ringMinorTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(ringMinorTickPoints),
    new THREE.LineBasicMaterial({ color: 0x9ef7ff, transparent: true, opacity: 0.35 })
  ),
  arc: new THREE.LineLoop(
    new THREE.BufferGeometry().setFromPoints(arcPoints),
    new THREE.LineBasicMaterial({ color: 0xffd27a, transparent: true, opacity: 0.75 })
  ),
  arcTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(arcTickPoints),
    new THREE.LineBasicMaterial({ color: 0xffd27a, transparent: true, opacity: 0.55 })
  ),
  arcMinorTicks: new THREE.LineSegments(
    new THREE.BufferGeometry().setFromPoints(arcMinorTickPoints),
    new THREE.LineBasicMaterial({ color: 0xffd27a, transparent: true, opacity: 0.38 })
  ),
  ringLabels: new THREE.Group(),
  arcLabels: new THREE.Group(),
  ringCurrent: new THREE.Group(),
  arcCurrent: new THREE.Group()
};
speakerGizmo.ring.visible = false;
speakerGizmo.ringTicks.visible = false;
speakerGizmo.ringMinorTicks.visible = false;
speakerGizmo.arc.visible = false;
speakerGizmo.arcTicks.visible = false;
speakerGizmo.arcMinorTicks.visible = false;
scene.add(speakerGizmo.ring);
scene.add(speakerGizmo.ringTicks);
scene.add(speakerGizmo.ringMinorTicks);
scene.add(speakerGizmo.arc);
scene.add(speakerGizmo.arcTicks);
scene.add(speakerGizmo.arcMinorTicks);
scene.add(speakerGizmo.ringLabels);
scene.add(speakerGizmo.arcLabels);
scene.add(speakerGizmo.ringCurrent);
scene.add(speakerGizmo.arcCurrent);

speakerGizmo.ringCurrentLabel = createSmallLabelSprite('0', '#9ef7ff');
speakerGizmo.arcCurrentLabel = createSmallLabelSprite('0', '#ffd27a');
speakerGizmo.ringCurrentLabel.renderOrder = 5;
speakerGizmo.arcCurrentLabel.renderOrder = 5;
speakerGizmo.ringCurrent.add(speakerGizmo.ringCurrentLabel);
speakerGizmo.arcCurrent.add(speakerGizmo.arcCurrentLabel);

const distanceGizmo = {
  group: new THREE.Group(),
  line: new THREE.Line(
    new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(), new THREE.Vector3()]),
    new THREE.LineBasicMaterial({ color: 0xa8ffbf, transparent: true, opacity: 0.7 })
  ),
  arrowA: new THREE.Mesh(
    new THREE.ConeGeometry(0.02, 0.06, 8),
    new THREE.MeshBasicMaterial({ color: 0xa8ffbf, transparent: true, opacity: 0.7 })
  ),
  arrowB: new THREE.Mesh(
    new THREE.ConeGeometry(0.02, 0.06, 8),
    new THREE.MeshBasicMaterial({ color: 0xa8ffbf, transparent: true, opacity: 0.7 })
  ),
  label: createSmallLabelSprite('0.00', '#7bff6a')
};
distanceGizmo.arrowA.renderOrder = 5;
distanceGizmo.arrowB.renderOrder = 5;
distanceGizmo.label.renderOrder = 5;
distanceGizmo.group.add(distanceGizmo.line);
distanceGizmo.group.add(distanceGizmo.arrowA);
distanceGizmo.group.add(distanceGizmo.arrowB);
distanceGizmo.group.add(distanceGizmo.label);
distanceGizmo.group.visible = false;
scene.add(distanceGizmo.group);

  const ringLabelAngles = Array.from({ length: 24 }, (_, i) => -180 + i * 15);
  const arcLabelAngles = Array.from({ length: 13 }, (_, i) => -90 + i * 15);

ringLabelAngles.forEach((angle) => {
  const sprite = createSmallLabelSprite(`${angle}`);
  speakerGizmo.ringLabels.add(sprite);
});

arcLabelAngles.forEach((angle) => {
  const sprite = createSmallLabelSprite(`${angle}`);
  speakerGizmo.arcLabels.add(sprite);
});

const sourceMeshes = new Map();
const sourceLabels = new Map();
const sourceOutlines = new Map();
const speakerMeshes = [];
const speakerLabels = [];
const sourceLevels = new Map();
const speakerLevels = new Map();
const sourceGains = new Map();
const speakerGainCache = new Map();
const objectGainCache = new Map();
const speakerBaseGains = new Map();
const objectBaseGains = new Map();
const speakerMuted = new Set();
const objectMuted = new Set();
const speakerItems = new Map();
const objectItems = new Map();
const speakerManualMuted = new Set();
const objectManualMuted = new Set();
const sourceNames = new Map();
const sourcePositionsRaw = new Map();
const sourceTrails = new Map();
let trailMaxPoints = 80;
let trailsEnabled = true;

let uiFlushScheduled = false;
const dirtyObjectMeters = new Set();
const dirtySpeakerMeters = new Set();
const dirtyObjectPositions = new Set();
const dirtyObjectLabels = new Set();
let dirtyMasterMeter = false;
let dirtyRoomRatio = false;
let dirtySpread = false;
let dirtyDialogNorm = false;
let dirtyDistanceDiffuse = false;
let dirtyDistanceModel = false;
let dirtyConfigSaved = false;
let dirtyLatency = false;
let dirtyResample = false;
let dirtyMasterGain = false;

let selectedSourceId = null;
let selectedSpeakerIndex = null;
let activeEditMode = 'polar';
let isDraggingSpeaker = false;
let dragMode = null;
let dragAzimuthDeg = 0;
let dragElevationDeg = 0;
let dragDistance = 1;
let dragAzimuthDelta = 1;
let dragElevationDelta = 1;

const sourceMaterial = new THREE.MeshStandardMaterial({
  color: 0xff7c4d,
  emissive: 0x64210c,
  transparent: true,
  opacity: 0.7
});
const sourceGeometry = new THREE.SphereGeometry(0.07, 24, 24);
const speakerGeometry = new THREE.BoxGeometry(0.08, 0.08, 0.08);
const speakerMaterial = new THREE.MeshStandardMaterial({
  color: 0x8ec8ff,
  emissive: 0x10253a,
  transparent: true,
  opacity: 0.65
});

const speakerBaseColor = new THREE.Color(0x8ec8ff);
const speakerHotColor = new THREE.Color(0xff3030);
const sourceDefaultEmissive = new THREE.Color(0x64210c);
const sourceSelectedEmissive = new THREE.Color(0x9b7f22);
const sourceOutlineColor = new THREE.Color(0xd9ecff);
const sourceOutlineSelectedColor = new THREE.Color(0xffde8a);

const layoutsByKey = new Map();
const raycaster = new THREE.Raycaster();
raycaster.params.Line.threshold = 0.08;
const pointer = new THREE.Vector2();
let pointerDownPosition = null;
let draggingPointerId = null;
let currentLayoutKey = null;
let currentLayoutSpeakers = [];

function formatNumber(value, digits = 2) {
  if (typeof value !== 'number' || Number.isNaN(value)) {
    return '—';
  }
  return value.toFixed(digits);
}

function formatPosition(position) {
  if (!position) {
    return 'x:— y:— z:—';
  }
  const x = Number(position.x);
  const y = Number(position.y);
  const z = Number(position.z);
  if (!Number.isFinite(x) || !Number.isFinite(y) || !Number.isFinite(z)) {
    return 'x:— y:— z:—';
  }

  // Speaker config objects carry raw az/el/dist (physical metres).
  // Use them directly to avoid distortion from the scene Cartesian coordinates,
  // which may be scaled for display purposes.
  if (typeof position.azimuthDeg === 'number') {
    const az = position.azimuthDeg;
    const el = position.elevationDeg;
    const r = position.distanceM;
    return `x:${formatNumber(x, 1)} y:${formatNumber(y, 1)} z:${formatNumber(z, 1)} | az:${formatNumber(az, 1)} el:${formatNumber(el, 1)} r:${formatNumber(r, 2)}`;
  }

  const az = (Math.atan2(z, x) * 180) / Math.PI;
  const planar = Math.sqrt(x * x + z * z);
  const el = (Math.atan2(y, planar) * 180) / Math.PI;
  const dist = Math.sqrt(x * x + y * y + z * z);

  return `x:${formatNumber(x, 1)} y:${formatNumber(y, 1)} z:${formatNumber(z, 1)} | az:${formatNumber(az, 1)} el:${formatNumber(el, 1)} r:${formatNumber(dist, 2)}`;
}

function formatLevel(meter) {
  if (!meter) {
    return '— dB';
  }
  return `${formatNumber(meter.rmsDbfs, 1)} dB`;
}

function meterToPercent(meter) {
  const db = typeof meter?.rmsDbfs === 'number' ? meter.rmsDbfs : -100;
  const clamped = Math.min(0, Math.max(-100, db));
  return ((clamped + 100) / 100) * 100;
}

function linearToDb(value) {
  const v = Number(value);
  if (!Number.isFinite(v) || v <= 0) {
    return '-∞ dB';
  }
  return `${(20 * Math.log10(v)).toFixed(1)} dB`;
}

function dbToLinear(db) {
  const v = Number(db);
  if (!Number.isFinite(v)) {
    return 0;
  }
  return Math.pow(10, v / 20);
}

function updateMeterUI(entry, meter) {
  if (!entry) return;
  entry.levelText.textContent = formatLevel(meter);
  entry.meterFill.style.setProperty('--level', `${meterToPercent(meter).toFixed(1)}%`);
}

function scheduleUIFlush() {
  if (uiFlushScheduled) {
    return;
  }
  uiFlushScheduled = true;
  requestAnimationFrame(flushUI);
}

function flushUI() {
  uiFlushScheduled = false;

  dirtyObjectMeters.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    updateMeterUI(entry, sourceLevels.get(id));
  });
  dirtyObjectMeters.clear();

  dirtySpeakerMeters.forEach((id) => {
    const entry = speakerItems.get(id);
    if (!entry) return;
    updateMeterUI(entry, speakerLevels.get(id));
  });
  dirtySpeakerMeters.clear();

  dirtyObjectPositions.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    const pos = sourcePositionsRaw.get(id);
    entry.position.textContent = formatPosition(pos);
  });
  dirtyObjectPositions.clear();

  dirtyObjectLabels.forEach((id) => {
    const entry = objectItems.get(id);
    if (!entry) return;
    entry.label.textContent = getObjectDisplayName(id);
  });
  dirtyObjectLabels.clear();

  if (dirtyMasterMeter) {
    updateMasterMeterUI();
    dirtyMasterMeter = false;
  }

  if (dirtyRoomRatio) {
    renderRoomRatioDisplay();
    dirtyRoomRatio = false;
  }

  if (dirtySpread) {
    renderSpreadDisplay();
    dirtySpread = false;
  }

  if (dirtyDialogNorm) {
    renderDialogNormDisplay();
    dirtyDialogNorm = false;
  }

  if (dirtyDistanceDiffuse) {
    renderDistanceDiffuseUI();
    dirtyDistanceDiffuse = false;
  }

  if (dirtyDistanceModel) {
    renderDistanceModelUI();
    dirtyDistanceModel = false;
  }

  if (dirtyConfigSaved) {
    renderConfigSavedUI();
    dirtyConfigSaved = false;
  }

  if (dirtyLatency) {
    renderLatencyDisplay();
    renderLatencyMeterUI();
    dirtyLatency = false;
  }

  if (dirtyResample) {
    renderResampleRatioDisplay();
    dirtyResample = false;
  }

  if (dirtyMasterGain) {
    renderMasterGainUI();
    dirtyMasterGain = false;
  }
}

function updateItemClasses(entry, isMuted, isDimmed) {
  entry.root.classList.toggle('is-muted', isMuted);
  entry.root.classList.toggle('is-dimmed', isDimmed);
}

function updateSpeakerMeterUI(id) {
  const key = String(id);
  dirtySpeakerMeters.add(key);
  scheduleUIFlush();
}

function updateObjectMeterUI(id) {
  const key = String(id);
  dirtyObjectMeters.add(key);
  scheduleUIFlush();
}

function updateObjectPositionUI(id, position) {
  const key = String(id);
  if (position) {
    sourcePositionsRaw.set(key, position);
  }
  dirtyObjectPositions.add(key);
  scheduleUIFlush();
}

function updateObjectLabelUI(id) {
  const key = String(id);
  dirtyObjectLabels.add(key);
  scheduleUIFlush();
}

function getObjectDisplayName(id) {
  const raw = sourceNames.get(id);
  if (raw && typeof raw === 'string' && raw.trim()) {
    return raw.trim();
  }
  return String(id);
}

function formatObjectLabel(id) {
  const raw = sourceNames.get(id);
  if (raw && typeof raw === 'string') {
    const trimmed = raw.trim();
    const underscoreIndex = trimmed.indexOf('_');
    const cleaned = underscoreIndex >= 0 ? trimmed.slice(underscoreIndex + 1) : trimmed;
    if (cleaned) {
      return cleaned;
    }
  }
  return String(id);
}

function updateSpeakerControlsUI() {
  const soloTarget = getSoloTarget('speaker');
  speakerItems.forEach((entry, id) => {
    const gainValue = getBaseGain(speakerBaseGains, speakerGainCache, id);
    entry.gainSlider.value = String(gainValue);
    entry.gainBox.textContent = linearToDb(gainValue);
    entry.muteBtn.classList.toggle('active', speakerMuted.has(id));
    entry.soloBtn.classList.toggle('active', soloTarget === id);
    updateItemClasses(entry, speakerMuted.has(id), soloTarget && soloTarget !== id);
    entry.root.classList.toggle('is-selected', selectedSpeakerIndex !== null && Number(id) === selectedSpeakerIndex);
  });
}

function updateObjectControlsUI() {
  const soloTarget = getSoloTarget('object');
  objectItems.forEach((entry, id) => {
    const gainValue = getBaseGain(objectBaseGains, objectGainCache, id);
    entry.gainSlider.value = String(gainValue);
    entry.gainBox.textContent = linearToDb(gainValue);
    entry.muteBtn.classList.toggle('active', objectMuted.has(id));
    entry.soloBtn.classList.toggle('active', soloTarget === id);
    updateItemClasses(entry, objectMuted.has(id), soloTarget && soloTarget !== id);
    entry.root.classList.toggle('is-selected', selectedSourceId === id);
  });
}

function createSpeakerItem(id, speaker) {
  const root = document.createElement('div');
  root.className = 'info-item speaker-item';
  root.addEventListener('click', () => {
    setSelectedSource(null);
    setSelectedSpeaker(Number(id));
  });

  const idStrip = document.createElement('div');
  idStrip.className = 'id-strip';
  const idText = document.createElement('span');
  idStrip.appendChild(idText);

  const content = document.createElement('div');
  content.className = 'speaker-content';

  const position = document.createElement('div');
  content.appendChild(position);

  const level = document.createElement('div');
  level.className = 'meter-row';

  const levelText = document.createElement('div');
  level.appendChild(levelText);

  const meterBar = document.createElement('div');
  meterBar.className = 'meter-bar';
  const meterFill = document.createElement('div');
  meterFill.className = 'meter-fill';
  meterBar.appendChild(meterFill);
  level.appendChild(meterBar);
  content.appendChild(level);

  const controls = document.createElement('div');
  controls.className = 'control-row';

  const gainSlider = document.createElement('input');
  gainSlider.type = 'range';
  gainSlider.min = '0';
  gainSlider.max = '2';
  gainSlider.step = '0.01';
  gainSlider.className = 'gain-slider';
  gainSlider.addEventListener('input', () => {
    speakerBaseGains.set(id, Number(gainSlider.value));
    applyGroupGains('speaker');
  });
  gainSlider.addEventListener('dblclick', () => {
    gainSlider.value = '1';
    speakerBaseGains.set(id, 1);
    applyGroupGains('speaker');
    updateSpeakerControlsUI();
  });
  controls.appendChild(gainSlider);

  const gainBox = document.createElement('div');
  gainBox.className = 'gain-box';
  gainBox.textContent = '0.0 dB';
  controls.appendChild(gainBox);

  const muteBtn = document.createElement('button');
  muteBtn.type = 'button';
  muteBtn.className = 'toggle-btn';
  muteBtn.textContent = 'M';
  muteBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleMute('speaker', id);
  });
  controls.appendChild(muteBtn);

  const soloBtn = document.createElement('button');
  soloBtn.type = 'button';
  soloBtn.className = 'toggle-btn';
  soloBtn.textContent = 'S';
  soloBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleSolo('speaker', id);
  });
  controls.appendChild(soloBtn);

  content.appendChild(controls);
  root.appendChild(content);
  root.appendChild(idStrip);

  return { root, label: idText, position, levelText, meterFill, gainSlider, gainBox, muteBtn, soloBtn };
}

function updateSpeakerItem(entry, id, speaker) {
  const soloTarget = getSoloTarget('speaker');
  entry.label.textContent = String(speaker.id ?? id);
  entry.position.textContent = formatPosition(speaker);
  const gainValue = getBaseGain(speakerBaseGains, speakerGainCache, id);
  entry.gainSlider.value = String(gainValue);
  entry.gainBox.textContent = linearToDb(gainValue);
  entry.muteBtn.classList.toggle('active', speakerMuted.has(id));
  entry.soloBtn.classList.toggle('active', soloTarget === id);
  updateItemClasses(entry, speakerMuted.has(id), soloTarget && soloTarget !== id);
  entry.root.classList.toggle('is-selected', selectedSpeakerIndex !== null && Number(id) === selectedSpeakerIndex);
  updateMeterUI(entry, speakerLevels.get(id));
}

function createObjectItem(id) {
  const root = document.createElement('div');
  root.className = 'info-item object-item';
  root.addEventListener('click', () => {
    setSelectedSource(id);
  });

  const idStrip = document.createElement('div');
  idStrip.className = 'id-strip flip';
  const idText = document.createElement('span');
  idText.textContent = String(id);
  idStrip.appendChild(idText);
  root.appendChild(idStrip);

  const content = document.createElement('div');
  content.className = 'object-content';

  const position = document.createElement('div');
  content.appendChild(position);

  const level = document.createElement('div');
  level.className = 'meter-row';

  const levelText = document.createElement('div');
  level.appendChild(levelText);

  const meterBar = document.createElement('div');
  meterBar.className = 'meter-bar';
  const meterFill = document.createElement('div');
  meterFill.className = 'meter-fill';
  meterBar.appendChild(meterFill);
  level.appendChild(meterBar);
  content.appendChild(level);

  const controls = document.createElement('div');
  controls.className = 'control-row';

  const gainSlider = document.createElement('input');
  gainSlider.type = 'range';
  gainSlider.min = '0';
  gainSlider.max = '2';
  gainSlider.step = '0.01';
  gainSlider.className = 'gain-slider';
  gainSlider.addEventListener('input', () => {
    objectBaseGains.set(id, Number(gainSlider.value));
    applyGroupGains('object');
  });
  gainSlider.addEventListener('dblclick', () => {
    gainSlider.value = '1';
    objectBaseGains.set(id, 1);
    applyGroupGains('object');
    updateObjectControlsUI();
  });
  controls.appendChild(gainSlider);

  const gainBox = document.createElement('div');
  gainBox.className = 'gain-box';
  gainBox.textContent = '0.0 dB';
  controls.appendChild(gainBox);

  const muteBtn = document.createElement('button');
  muteBtn.type = 'button';
  muteBtn.className = 'toggle-btn';
  muteBtn.textContent = 'M';
  muteBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleMute('object', id);
  });
  controls.appendChild(muteBtn);

  const soloBtn = document.createElement('button');
  soloBtn.type = 'button';
  soloBtn.className = 'toggle-btn';
  soloBtn.textContent = 'S';
  soloBtn.addEventListener('click', (event) => {
    event.preventDefault();
    toggleSolo('object', id);
  });
  controls.appendChild(soloBtn);

  content.appendChild(controls);
  root.appendChild(content);

  return { root, label: idText, position, levelText, meterFill, gainSlider, gainBox, muteBtn, soloBtn };
}

function updateObjectItem(entry, id, position, name) {
  const soloTarget = getSoloTarget('object');
  if (name) {
    sourceNames.set(id, name);
  }
  entry.label.textContent = getObjectDisplayName(id);
  entry.position.textContent = formatPosition(position);
  const gainValue = getBaseGain(objectBaseGains, objectGainCache, id);
  entry.gainSlider.value = String(gainValue);
  entry.gainBox.textContent = linearToDb(gainValue);
  entry.muteBtn.classList.toggle('active', objectMuted.has(id));
  entry.soloBtn.classList.toggle('active', soloTarget === id);
  updateItemClasses(entry, objectMuted.has(id), soloTarget && soloTarget !== id);
  entry.root.classList.toggle('is-selected', selectedSourceId === id);
  updateMeterUI(entry, sourceLevels.get(id));
}

function renderSpeakersList() {
  if (!speakersListEl) return;

  if (!currentLayoutSpeakers.length) {
    speakersListEl.textContent = 'Aucun speaker.';
    speakerItems.clear();
    updateSectionProportions();
    return;
  }

  speakersListEl.textContent = '';
  const activeIds = new Set();
  currentLayoutSpeakers.forEach((speaker, index) => {
    const id = String(index);
    activeIds.add(id);
    let entry = speakerItems.get(id);
    if (!entry) {
      entry = createSpeakerItem(id, speaker);
      speakerItems.set(id, entry);
    }
    updateSpeakerItem(entry, id, speaker);
    speakersListEl.appendChild(entry.root);
  });
  speakerItems.forEach((entry, id) => {
    if (!activeIds.has(id)) {
      entry.root.remove();
      speakerItems.delete(id);
    }
  });
  updateSectionProportions();
}

function renderObjectsList() {
  if (!objectsListEl) return;

  const ids = [...sourceMeshes.keys()].sort((a, b) => {
    const aNum = Number(a);
    const bNum = Number(b);
    const aIsNum = Number.isFinite(aNum);
    const bIsNum = Number.isFinite(bNum);
    if (aIsNum && bIsNum) {
      return aNum - bNum;
    }
    if (aIsNum) {
      return -1;
    }
    if (bIsNum) {
      return 1;
    }
    return String(a).localeCompare(String(b));
  });
  if (!ids.length) {
    objectsListEl.textContent = 'Aucun objet.';
    objectItems.clear();
    updateSectionProportions();
    return;
  }

  objectsListEl.textContent = '';
  const activeIds = new Set();
  ids.forEach((id) => {
    const mesh = sourceMeshes.get(id);
    if (!mesh) return;
    const key = String(id);
    activeIds.add(key);
    let entry = objectItems.get(key);
    if (!entry) {
      entry = createObjectItem(key);
      objectItems.set(key, entry);
    }
    const raw = sourcePositionsRaw.get(key) || mesh.position;
    updateObjectItem(entry, key, raw, sourceNames.get(key));
    objectsListEl.appendChild(entry.root);
  });
  objectItems.forEach((entry, id) => {
    if (!activeIds.has(id)) {
      entry.root.remove();
      objectItems.delete(id);
    }
  });
  updateSectionProportions();
}

function refreshOverlayLists() {
  renderSpeakersList();
  renderObjectsList();
  updateSectionProportions();
}

function getBaseGain(map, cache, id) {
  if (map.has(id)) {
    return map.get(id);
  }
  if (cache.has(id)) {
    return cache.get(id);
  }
  return 1;
}

function getSpeakerIds() {
  return currentLayoutSpeakers.map((_, index) => String(index));
}

function getObjectIds() {
  return [...sourceMeshes.keys()].map((id) => String(id));
}

function sendObjectGain(id, gain) {
  if (ws.readyState !== WebSocket.OPEN) {
    return;
  }
  ws.send(
    JSON.stringify({
      type: 'control:object:gain',
      id,
      gain
    })
  );
}

function sendSpeakerGain(id, gain) {
  if (ws.readyState !== WebSocket.OPEN) {
    return;
  }
  ws.send(
    JSON.stringify({
      type: 'control:speaker:gain',
      id,
      gain
    })
  );
}

function getSoloTarget(group) {
  const ids = group === 'speaker' ? getSpeakerIds() : getObjectIds();
  const mutedSet = group === 'speaker' ? speakerMuted : objectMuted;
  if (ids.length <= 1) {
    return null;
  }

  const unmuted = ids.filter((id) => !mutedSet.has(id));
  if (unmuted.length !== 1) {
    return null;
  }

  const target = unmuted[0];
  const othersMuted = ids.every((id) => id === target || mutedSet.has(id));
  return othersMuted ? target : null;
}

function areAllOthersMuted(group, id) {
  const ids = group === 'speaker' ? getSpeakerIds() : getObjectIds();
  const mutedSet = group === 'speaker' ? speakerMuted : objectMuted;
  return ids.every((other) => other === id || mutedSet.has(other));
}

function sendObjectMute(id, muted) {
  if (ws.readyState !== WebSocket.OPEN) {
    return;
  }
  ws.send(
    JSON.stringify({
      type: 'control:object:mute',
      id,
      muted: muted ? 1 : 0
    })
  );
}

function sendSpeakerMute(id, muted) {
  if (ws.readyState !== WebSocket.OPEN) {
    return;
  }
  ws.send(
    JSON.stringify({
      type: 'control:speaker:mute',
      id,
      muted: muted ? 1 : 0
    })
  );
}

function applyGroupGains(group) {
  const isSpeaker = group === 'speaker';
  const ids = isSpeaker ? getSpeakerIds() : getObjectIds();
  const baseMap = isSpeaker ? speakerBaseGains : objectBaseGains;
  const cache = isSpeaker ? speakerGainCache : objectGainCache;
  const mutedSet = isSpeaker ? speakerMuted : objectMuted;

  ids.forEach((id) => {
    const baseGain = getBaseGain(baseMap, cache, id);
    const muted = mutedSet.has(id);
    const effectiveGain = muted ? 0 : baseGain;
    if (isSpeaker) {
      sendSpeakerGain(id, effectiveGain);
    } else {
      sendObjectGain(id, effectiveGain);
    }
  });
}

function renderRoomRatioDisplay() {
  if (!roomRatioEl) return;
  roomRatioEl.textContent = `room_ratio: ${formatNumber(roomRatio.width, 2)} ${formatNumber(roomRatio.length, 2)} ${formatNumber(roomRatio.height, 2)}`;
}

function updateRoomRatioDisplay() {
  dirtyRoomRatio = true;
  scheduleUIFlush();
}

function renderSpreadDisplay() {
  if (!spreadInfoEl) return;
  const minDeg = spreadState.min === null ? null : spreadState.min * 180.0;
  const maxDeg = spreadState.max === null ? null : spreadState.max * 180.0;
  const minText = minDeg === null ? '—' : formatNumber(minDeg, 0);
  const maxText = maxDeg === null ? '—' : formatNumber(maxDeg, 0);
  spreadInfoEl.textContent = `spread: ${minText}° / ${maxText}°`;
  if (spreadMinSliderEl) {
    const value = minDeg === null ? 0 : minDeg;
    spreadMinSliderEl.value = String(value);
  }
  if (spreadMaxSliderEl) {
    const value = maxDeg === null ? 180 : maxDeg;
    spreadMaxSliderEl.value = String(value);
  }
  if (spreadMinValEl) {
    spreadMinValEl.textContent = minDeg === null ? '—' : `${formatNumber(minDeg, 0)}°`;
  }
  if (spreadMaxValEl) {
    spreadMaxValEl.textContent = maxDeg === null ? '—' : `${formatNumber(maxDeg, 0)}°`;
  }
}

function updateSpreadDisplay() {
  dirtySpread = true;
  scheduleUIFlush();
}

function renderDialogNormDisplay() {
  if (!loudnessInfoEl) return;
  const enabledText = loudnessEnabled === null ? '—' : loudnessEnabled ? 'on' : 'off';
  const levelText = loudnessSource === null ? '—' : `${formatNumber(loudnessSource, 0)} dBFS`;
  const gainText =
    loudnessGain === null
      ? '—'
      : `${formatNumber(loudnessGain, 2)} (${linearToDb(loudnessGain)})`;
  loudnessInfoEl.textContent = `source loudness: ${levelText} | correction: ${gainText} | ${enabledText}`;
  if (loudnessToggleEl) {
    loudnessToggleEl.checked = loudnessEnabled === true;
  }
}

function updateDialogNormDisplay() {
  dirtyDialogNorm = true;
  scheduleUIFlush();
}

function renderDistanceDiffuseUI() {
  if (distanceDiffuseToggleEl) {
    distanceDiffuseToggleEl.checked = distanceDiffuseState.enabled === true;
  }
  if (distanceDiffuseThresholdSliderEl && distanceDiffuseState.threshold !== null) {
    distanceDiffuseThresholdSliderEl.value = String(distanceDiffuseState.threshold);
  }
  if (distanceDiffuseThresholdValEl) {
    const v = distanceDiffuseState.threshold === null ? '—' : formatNumber(distanceDiffuseState.threshold, 2);
    distanceDiffuseThresholdValEl.textContent = v;
  }
  if (distanceDiffuseCurveSliderEl && distanceDiffuseState.curve !== null) {
    distanceDiffuseCurveSliderEl.value = String(distanceDiffuseState.curve);
  }
  if (distanceDiffuseCurveValEl) {
    const v = distanceDiffuseState.curve === null ? '—' : formatNumber(distanceDiffuseState.curve, 2);
    distanceDiffuseCurveValEl.textContent = v;
  }
}

function updateDistanceDiffuseUI() {
  dirtyDistanceDiffuse = true;
  scheduleUIFlush();
}

function renderConfigSavedUI() {
  if (!configSavedIndicatorEl) return;
  if (configSaved === null) {
    configSavedIndicatorEl.textContent = '—';
    configSavedIndicatorEl.style.color = '#d9ecff';
  } else if (configSaved) {
    configSavedIndicatorEl.textContent = 'Sauvegardé';
    configSavedIndicatorEl.style.color = '#52e2a2';
  } else {
    configSavedIndicatorEl.textContent = 'Modifié (non sauvegardé)';
    configSavedIndicatorEl.style.color = '#ffd56a';
  }
}

function updateConfigSavedUI() {
  dirtyConfigSaved = true;
  scheduleUIFlush();
}

function renderLatencyDisplay() {
  if (!latencyInfoEl) return;
  latencyInfoEl.textContent = latencyMs === null
    ? 'latency: —'
    : `latency: ${formatNumber(latencyMs, 0)} ms`;
}

function updateLatencyDisplay() {
  dirtyLatency = true;
  scheduleUIFlush();
}

function renderResampleRatioDisplay() {
  if (!resampleRatioInfoEl) return;
  if (resampleRatio === null) {
    resampleRatioInfoEl.textContent = 'resample: —';
    return;
  }
  // Express as ppm deviation from nominal (1.0)
  const ppm = Math.round((resampleRatio - 1.0) * 1e6);
  const sign = ppm >= 0 ? '+' : '';
  resampleRatioInfoEl.textContent = `resample: ${sign}${ppm} ppm`;
}

function updateResampleRatioDisplay() {
  dirtyResample = true;
  scheduleUIFlush();
}

function renderLatencyMeterUI() {
  if (!latencyMeterFillEl) return;
  if (latencyMs === null) {
    latencyMeterFillEl.style.setProperty('--level', '0%');
    return;
  }
  const value = Math.max(0, Number(latencyMs));
  const maxMs = 2000;
  const percent = Math.min(100, (value / maxMs) * 100);
  latencyMeterFillEl.style.setProperty('--level', `${percent.toFixed(1)}%`);
}

function updateLatencyMeterUI() {
  dirtyLatency = true;
  scheduleUIFlush();
}

function renderMasterGainUI() {
  if (masterGainSliderEl) {
    masterGainSliderEl.value = String(masterGain);
  }
  if (masterGainBoxEl) {
    masterGainBoxEl.textContent = linearToDb(masterGain);
  }
}

function updateMasterGainUI() {
  dirtyMasterGain = true;
  scheduleUIFlush();
}

function renderDistanceModelUI() {
  if (!distanceModelSelectEl) return;
  distanceModelSelectEl.value = ['none', 'linear', 'quadratic', 'inverse-square'].includes(distanceModel)
    ? distanceModel
    : 'none';
}

function updateDistanceModelUI() {
  dirtyDistanceModel = true;
  scheduleUIFlush();
}

function getAverageSpeakerRmsDb() {
  const levels = speakerMeshes.length
    ? speakerMeshes.map((_, index) => speakerLevels.get(String(index)))
    : [];
  const valid = levels.filter((meter) => meter && typeof meter.rmsDbfs === 'number');
  if (valid.length === 0) {
    return null;
  }
  const sumLinear = valid.reduce((acc, meter) => acc + dbToLinear(meter.rmsDbfs), 0);
  const avgLinear = sumLinear / valid.length;
  const avgDb = 20 * Math.log10(Math.max(avgLinear, 1e-6));
  return Math.max(-100, Math.min(0, avgDb));
}

function updateMasterMeterUI() {
  if (!masterMeterTextEl || !masterMeterFillEl) return;
  const avgDb = getAverageSpeakerRmsDb();
  if (avgDb === null) {
    masterMeterTextEl.textContent = '— dB';
    masterMeterFillEl.style.setProperty('--level', '0%');
    return;
  }
  masterMeterTextEl.textContent = `${formatNumber(avgDb, 1)} dB`;
  const percent = ((avgDb + 100) / 100) * 100;
  masterMeterFillEl.style.setProperty('--level', `${percent.toFixed(1)}%`);
}

function applyRoomRatioToScene() {
  const maxDim = Math.max(roomRatio.width, roomRatio.length, roomRatio.height, 1e-6);
  const sx = (roomRatio.length / maxDim) * 2;
  const sy = (roomRatio.height / maxDim) * 2;
  const sz = (roomRatio.width / maxDim) * 2;
  roomGroup.scale.set(sx, sy, sz);
}

function cartesianToSpherical(position) {
  const x = Number(position.x) || 0;
  const y = Number(position.y) || 0;
  const z = Number(position.z) || 0;
  const dist = Math.sqrt(x * x + y * y + z * z);
  const az = (Math.atan2(z, x) * 180) / Math.PI;
  const el = dist > 0 ? (Math.atan2(y, Math.sqrt(x * x + z * z)) * 180) / Math.PI : 0;
  return { az, el, dist };
}

function sphericalToCartesianDeg(az, el, dist) {
  const azRad = (az * Math.PI) / 180;
  const elRad = (el * Math.PI) / 180;
  const x = dist * Math.cos(elRad) * Math.cos(azRad);
  const y = dist * Math.sin(elRad);
  const z = dist * Math.cos(elRad) * Math.sin(azRad);
  return { x, y, z };
}

function normalizeAngleDeg(angle) {
  let a = angle;
  while (a > 180) a -= 360;
  while (a < -180) a += 360;
  return a;
}

function snapAngleDeg(angle, step, threshold) {
  const snapped = Math.round(angle / step) * step;
  return Math.abs(angle - snapped) <= threshold ? snapped : angle;
}

function updateSpeakerGizmo() {
  if (activeEditMode !== 'polar' || selectedSpeakerIndex === null) {
    speakerGizmo.ring.visible = false;
    speakerGizmo.ringTicks.visible = false;
    speakerGizmo.ringMinorTicks.visible = false;
    speakerGizmo.arc.visible = false;
    speakerGizmo.arcTicks.visible = false;
    speakerGizmo.arcMinorTicks.visible = false;
    speakerGizmo.ringLabels.visible = false;
    speakerGizmo.arcLabels.visible = false;
    speakerGizmo.ringCurrent.visible = false;
    speakerGizmo.arcCurrent.visible = false;
    distanceGizmo.group.visible = false;
    return;
  }

  const mesh = speakerMeshes[selectedSpeakerIndex];
  if (!mesh) {
    speakerGizmo.ring.visible = false;
    speakerGizmo.ringTicks.visible = false;
    speakerGizmo.ringMinorTicks.visible = false;
    speakerGizmo.arc.visible = false;
    speakerGizmo.arcTicks.visible = false;
    speakerGizmo.arcMinorTicks.visible = false;
    speakerGizmo.ringLabels.visible = false;
    speakerGizmo.arcLabels.visible = false;
    speakerGizmo.ringCurrent.visible = false;
    speakerGizmo.arcCurrent.visible = false;
    distanceGizmo.group.visible = false;
    return;
  }

  const { az, el, dist } = cartesianToSpherical(mesh.position);
  dragAzimuthDeg = az;
  dragElevationDeg = el;
  dragDistance = Math.max(0.01, dist);

  speakerGizmo.ring.visible = true;
  speakerGizmo.ringTicks.visible = !isDraggingSpeaker || dragAzimuthDelta > 0.1;
  speakerGizmo.ringMinorTicks.visible = isDraggingSpeaker && dragAzimuthDelta >= 0 && dragAzimuthDelta <= 0.1;
  speakerGizmo.arc.visible = true;
  speakerGizmo.arcTicks.visible = !isDraggingSpeaker || dragElevationDelta > 0.1;
  speakerGizmo.arcMinorTicks.visible = isDraggingSpeaker && dragElevationDelta >= 0 && dragElevationDelta <= 0.1;
  speakerGizmo.ringLabels.visible = true;
  speakerGizmo.arcLabels.visible = true;
  speakerGizmo.ringCurrent.visible = true;
  speakerGizmo.arcCurrent.visible = true;
  distanceGizmo.group.visible = true;

  speakerGizmo.ring.position.set(0, 0, 0);
  speakerGizmo.ring.scale.set(dragDistance, 1, dragDistance);
  speakerGizmo.ringTicks.position.set(0, 0, 0);
  speakerGizmo.ringTicks.scale.set(dragDistance, 1, dragDistance);
  speakerGizmo.ringMinorTicks.position.set(0, 0, 0);
  speakerGizmo.ringMinorTicks.scale.set(dragDistance, 1, dragDistance);
  speakerGizmo.ringLabels.position.set(0, 0, 0);
  speakerGizmo.ringLabels.scale.set(dragDistance, 1, dragDistance);
  speakerGizmo.ringCurrent.position.set(0, 0, 0);
  speakerGizmo.ringCurrent.scale.set(dragDistance, 1, dragDistance);

  const azRad = (az * Math.PI) / 180;
  speakerGizmo.arc.position.set(0, 0, 0);
  speakerGizmo.arc.scale.set(dragDistance, dragDistance, dragDistance);
  speakerGizmo.arc.rotation.set(0, -azRad, 0);
  speakerGizmo.arcTicks.position.set(0, 0, 0);
  speakerGizmo.arcTicks.scale.set(dragDistance, dragDistance, dragDistance);
  speakerGizmo.arcTicks.rotation.set(0, -azRad, 0);
  speakerGizmo.arcMinorTicks.position.set(0, 0, 0);
  speakerGizmo.arcMinorTicks.scale.set(dragDistance, dragDistance, dragDistance);
  speakerGizmo.arcMinorTicks.rotation.set(0, -azRad, 0);
  speakerGizmo.arcLabels.position.set(0, 0, 0);
  speakerGizmo.arcLabels.scale.set(dragDistance, dragDistance, dragDistance);
  speakerGizmo.arcLabels.rotation.set(0, -azRad, 0);
  speakerGizmo.arcCurrent.position.set(0, 0, 0);
  speakerGizmo.arcCurrent.scale.set(dragDistance, dragDistance, dragDistance);
  speakerGizmo.arcCurrent.rotation.set(0, -azRad, 0);

  ringLabelAngles.forEach((angle, idx) => {
    const sprite = speakerGizmo.ringLabels.children[idx];
    const rad = (angle * Math.PI) / 180;
    const r = 1.1;
    sprite.position.set(Math.cos(rad) * r, 0.02, Math.sin(rad) * r);
  });

  arcLabelAngles.forEach((angle, idx) => {
    const sprite = speakerGizmo.arcLabels.children[idx];
    const rad = (angle * Math.PI) / 180;
    const r = 1.1;
    sprite.position.set(Math.cos(rad) * r, Math.sin(rad) * r, 0);
  });

  const ringAngle = normalizeAngleDeg(dragAzimuthDeg);
  const ringRad = (ringAngle * Math.PI) / 180;
  speakerGizmo.ringCurrentLabel.position.set(Math.cos(ringRad) * 1.24, 0.04, Math.sin(ringRad) * 1.24);
  setLabelSpriteText(speakerGizmo.ringCurrentLabel, `${ringAngle.toFixed(1)}`);

  const arcAngle = dragElevationDeg;
  const arcRad = (arcAngle * Math.PI) / 180;
  speakerGizmo.arcCurrentLabel.position.set(Math.cos(arcRad) * 1.24, Math.sin(arcRad) * 1.24, 0);
  setLabelSpriteText(speakerGizmo.arcCurrentLabel, `${arcAngle.toFixed(1)}`);

  const speakerPos = mesh.position.clone();
  const dir = speakerPos.length() > 1e-6 ? speakerPos.clone().normalize() : new THREE.Vector3(1, 0, 0);
  const lineGeom = distanceGizmo.line.geometry;
  lineGeom.setFromPoints([new THREE.Vector3(0, 0, 0), speakerPos.clone()]);
  lineGeom.attributes.position.needsUpdate = true;

  const arrowOffset = 0.1;
  distanceGizmo.arrowA.position.copy(dir.clone().multiplyScalar(arrowOffset));
  distanceGizmo.arrowB.position.copy(speakerPos.clone().add(dir.clone().multiplyScalar(-arrowOffset)));

  const up = new THREE.Vector3(0, 1, 0);
  const quat = new THREE.Quaternion().setFromUnitVectors(up, dir);
  distanceGizmo.arrowA.quaternion.copy(quat);
  const quatB = new THREE.Quaternion().setFromUnitVectors(up, dir.clone().negate());
  distanceGizmo.arrowB.quaternion.copy(quatB);

  const mid = speakerPos.clone().multiplyScalar(0.5);
  distanceGizmo.label.position.set(mid.x, mid.y + 0.08, mid.z);
  setLabelSpriteText(distanceGizmo.label, `${speakerPos.length().toFixed(2)}`);
}

function setSelectedSpeaker(index) {
  selectedSpeakerIndex = index;
  updateSpeakerGizmo();
  updateSpeakerControlsUI();
  updateControlsForEditMode();
}

function updateControlsForEditMode() {
  controls.enableZoom = true;
}

function updateRoomFaceVisibility() {
  tempCameraLocal.copy(camera.position);
  roomGroup.worldToLocal(tempCameraLocal);
  roomFaceDefs.forEach((entry) => {
    const facePos = entry.mesh.position;
    tempToCamera.set(
      tempCameraLocal.x - facePos.x,
      tempCameraLocal.y - facePos.y,
      tempCameraLocal.z - facePos.z
    );
    tempToCenter.set(-facePos.x, -facePos.y, -facePos.z);
    const camSide = entry.inward.dot(tempToCamera);
    entry.mesh.visible = camSide > 0;
  });

  const screenFace = roomFaceDefs.find((entry) => entry.key === 'posX');
  if (screenFace) {
    const facePos = screenFace.mesh.position;
    tempToCamera.set(
      tempCameraLocal.x - facePos.x,
      tempCameraLocal.y - facePos.y,
      tempCameraLocal.z - facePos.z
    );
    const camSide = screenFace.inward.dot(tempToCamera);
    const isInside = camSide > 0;
    screenMaterial.opacity = isInside ? 0.18 : 0.18;
  }
}

function toggleMute(group, id) {
  const mutedSet = group === 'speaker' ? speakerMuted : objectMuted;
  const manualMutedSet = group === 'speaker' ? speakerManualMuted : objectManualMuted;
  if (mutedSet.has(id)) {
    mutedSet.delete(id);
    manualMutedSet.delete(id);
  } else {
    mutedSet.add(id);
    manualMutedSet.add(id);
  }
  if (group === 'speaker') {
    sendSpeakerMute(id, speakerMuted.has(id));
    updateSpeakerControlsUI();
  } else {
    sendObjectMute(id, objectMuted.has(id));
    updateObjectControlsUI();
  }
}

function toggleSolo(group, id) {
  const isSpeaker = group === 'speaker';
  const ids = isSpeaker ? getSpeakerIds() : getObjectIds();
  const mutedSet = isSpeaker ? speakerMuted : objectMuted;
  const manualMutedSet = isSpeaker ? speakerManualMuted : objectManualMuted;
  const currentSolo = getSoloTarget(group);

  if (currentSolo && currentSolo !== id) {
    mutedSet.add(currentSolo);
    manualMutedSet.add(currentSolo);
    mutedSet.delete(id);
    manualMutedSet.delete(id);
    if (isSpeaker) {
      sendSpeakerMute(currentSolo, true);
      sendSpeakerMute(id, false);
      updateSpeakerControlsUI();
    } else {
      sendObjectMute(currentSolo, true);
      sendObjectMute(id, false);
      updateObjectControlsUI();
      setSelectedSource(id);
    }
    return;
  }

  if (currentSolo === id) {
    ids.forEach((other) => {
      if (other === id) {
        return;
      }
      mutedSet.delete(other);
      manualMutedSet.delete(other);
      if (isSpeaker) {
        sendSpeakerMute(other, false);
      } else {
        sendObjectMute(other, false);
      }
    });
    if (isSpeaker) {
      updateSpeakerControlsUI();
    } else {
      updateObjectControlsUI();
    }
    return;
  }

  ids.forEach((other) => {
    if (other === id) {
      return;
    }
    if (!mutedSet.has(other)) {
      mutedSet.add(other);
      if (isSpeaker) {
        sendSpeakerMute(other, true);
      } else {
        sendObjectMute(other, true);
      }
    }
  });

  if (!isSpeaker) {
    setSelectedSource(id);
  }

  if (isSpeaker) {
    updateSpeakerControlsUI();
  } else {
    updateObjectControlsUI();
  }
}

function updateSectionProportions() {
  if (speakersSectionEl) {
    speakersSectionEl.style.flex = '1 1 0%';
  }
  if (objectsSectionEl) {
    objectsSectionEl.style.flex = '1 1 0%';
  }
}

function createLabelSprite(text) {
  const canvas = document.createElement('canvas');
  canvas.width = 256;
  canvas.height = 96;
  const ctx = canvas.getContext('2d');
  const texture = new THREE.CanvasTexture(canvas);
  texture.minFilter = THREE.LinearFilter;
  const material = new THREE.SpriteMaterial({ map: texture, transparent: true, depthTest: false });
  const sprite = new THREE.Sprite(material);
  sprite.scale.set(0.42, 0.16, 1);
  sprite.userData.labelCanvas = canvas;
  sprite.userData.labelCtx = ctx;
  sprite.userData.labelTexture = texture;
  sprite.userData.labelText = '';
  sprite.userData.labelColor = '#ffffff';
  setLabelSpriteText(sprite, text);
  return sprite;
}

function createSmallLabelSprite(text, color = '#d9ecff') {
  const canvas = document.createElement('canvas');
  canvas.width = 128;
  canvas.height = 64;
  const ctx = canvas.getContext('2d');

  const texture = new THREE.CanvasTexture(canvas);
  texture.minFilter = THREE.LinearFilter;
  const material = new THREE.SpriteMaterial({ map: texture, transparent: true, depthTest: false });
  const sprite = new THREE.Sprite(material);
  sprite.scale.set(0.25, 0.12, 1);
  sprite.userData.labelCanvas = canvas;
  sprite.userData.labelCtx = ctx;
  sprite.userData.labelTexture = texture;
  sprite.userData.labelText = '';
  sprite.userData.labelColor = color;
  setLabelSpriteText(sprite, text);
  return sprite;
}

function setLabelSpriteText(sprite, text) {
  if (!sprite?.userData?.labelCanvas || !sprite.userData.labelCtx) {
    return;
  }
  const nextText = String(text ?? '');
  if (sprite.userData.labelText === nextText) {
    return;
  }

  const canvas = sprite.userData.labelCanvas;
  const ctx = sprite.userData.labelCtx;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.font = canvas.width >= 200 ? 'bold 36px sans-serif' : 'bold 28px sans-serif';
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = sprite.userData.labelColor || '#ffffff';
  ctx.fillText(nextText, canvas.width / 2, canvas.height / 2);
  sprite.userData.labelText = nextText;
  if (sprite.userData.labelTexture) {
    sprite.userData.labelTexture.needsUpdate = true;
  }
}

function createTrailLine() {
  const posArr = new Float32Array(300 * 3); // pre-allocated for max slider value
  const colArr = new Float32Array(300 * 3);
  const geometry = new THREE.BufferGeometry();
  const posAttr = new THREE.BufferAttribute(posArr, 3);
  const colAttr = new THREE.BufferAttribute(colArr, 3);
  posAttr.setUsage(THREE.DynamicDrawUsage);
  colAttr.setUsage(THREE.DynamicDrawUsage);
  geometry.setAttribute('position', posAttr);
  geometry.setAttribute('color', colAttr);
  geometry.setDrawRange(0, 0);
  const material = new THREE.LineBasicMaterial({
    vertexColors: true,
    transparent: true,
    opacity: 0.75,
    depthTest: false,
    depthWrite: false
  });
  const line = new THREE.Line(geometry, material);
  line.renderOrder = 15;
  line.frustumCulled = false;
  return line;
}

function rebuildTrailGeometry(id) {
  const trail = sourceTrails.get(id);
  if (!trail) return;
  const count = trail.positions.length;
  if (count < 2) {
    trail.line.geometry.setDrawRange(0, 0);
    return;
  }
  const mesh = sourceMeshes.get(id);
  const r = mesh ? mesh.material.color.r : 0.8;
  const g = mesh ? mesh.material.color.g : 0.4;
  const b = mesh ? mesh.material.color.b : 0.2;
  const posAttr = trail.line.geometry.getAttribute('position');
  const colAttr = trail.line.geometry.getAttribute('color');
  for (let i = 0; i < count; i++) {
    const raw = trail.positions[i];
    posAttr.array[i * 3] = raw.x * roomRatio.length;
    posAttr.array[i * 3 + 1] = raw.y * roomRatio.height;
    posAttr.array[i * 3 + 2] = raw.z * roomRatio.width;
    const t = count > 1 ? i / (count - 1) : 1; // 0=oldest (dark), 1=newest (full colour)
    colAttr.array[i * 3] = r * t;
    colAttr.array[i * 3 + 1] = g * t;
    colAttr.array[i * 3 + 2] = b * t;
  }
  posAttr.needsUpdate = true;
  colAttr.needsUpdate = true;
  trail.line.geometry.setDrawRange(0, count);
}

function createSourceOutline() {
  const points = [];
  const segments = 64;
  for (let i = 0; i < segments; i += 1) {
    const a = (i / segments) * Math.PI * 2;
    points.push(new THREE.Vector3(Math.cos(a), Math.sin(a), 0));
  }

  const geometry = new THREE.BufferGeometry().setFromPoints(points);
  const material = new THREE.LineBasicMaterial({
    color: sourceOutlineColor.clone(),
    transparent: true,
    opacity: 0.98,
    depthTest: false,
    depthWrite: false
  });

  const outline = new THREE.LineLoop(geometry, material);
  outline.renderOrder = 20;
  return outline;
}

function updateSourceDecorations(id) {
  const mesh = sourceMeshes.get(id);
  const label = sourceLabels.get(id);
  const outline = sourceOutlines.get(id);

  if (!mesh) {
    return;
  }

  if (label) {
    label.position.set(mesh.position.x, mesh.position.y, mesh.position.z);
  }

  if (outline) {
    const radius = 0.07 * mesh.scale.x * 1.08;
    outline.position.set(mesh.position.x, mesh.position.y, mesh.position.z);
    outline.scale.setScalar(radius);
  }
}

function dbfsToScale(dbfs, minScale, maxScale) {
  const clamped = Math.min(0, Math.max(-100, Number(dbfs ?? -100)));
  const normalized = (clamped + 100) / 100;
  return minScale + normalized * (maxScale - minScale);
}

function gainToMix(gain) {
  return Math.min(1, Math.max(0, Number(gain ?? 0)));
}

function applySourceLevel(id, mesh, meter) {
  const scale = dbfsToScale(meter?.rmsDbfs, 0.5, 2.4);
  mesh.scale.setScalar(scale);
  updateSourceDecorations(id);
}

function applySpeakerLevel(mesh, meter) {
  const scale = dbfsToScale(meter?.rmsDbfs, 0.65, 2.2);
  mesh.scale.setScalar(scale);
}

function getSelectedSourceGains() {
  if (!selectedSourceId) {
    return null;
  }
  return sourceGains.get(selectedSourceId) || null;
}

function updateSourceSelectionStyles() {
  sourceMeshes.forEach((mesh, id) => {
    const isSelected = id === selectedSourceId;
    mesh.material.emissive.copy(isSelected ? sourceSelectedEmissive : sourceDefaultEmissive);

    const outline = sourceOutlines.get(id);
    if (outline) {
      outline.material.color.copy(isSelected ? sourceOutlineSelectedColor : sourceOutlineColor);
      outline.material.opacity = isSelected ? 1 : 0.98;
    }
  });
}

function updateSpeakerColorsFromSelection() {
  const gains = getSelectedSourceGains();

  speakerMeshes.forEach((mesh, index) => {
    const mix = gainToMix(gains?.[index]);
    mesh.material.color.copy(speakerBaseColor).lerp(speakerHotColor, mix);

    const baseOpacity = Number(mesh.userData.baseOpacity ?? 0.65);
    if (!selectedSourceId) {
      mesh.material.opacity = baseOpacity;
      return;
    }

    mesh.material.opacity = mix <= 1e-6 ? Math.min(baseOpacity, 0.08) : baseOpacity;
  });
}

function setSelectedSource(id) {
  selectedSourceId = id;
  updateSourceSelectionStyles();
  updateSpeakerColorsFromSelection();
  updateObjectControlsUI();
}

function getSourceMesh(id) {
  if (!sourceMeshes.has(id)) {
    const mesh = new THREE.Mesh(sourceGeometry, sourceMaterial.clone());
    mesh.material.color.setHSL(Math.random(), 0.8, 0.6);
    mesh.material.emissive.copy(sourceDefaultEmissive);
    mesh.material.opacity = 0.0;
    mesh.material.depthWrite = false;
    mesh.userData.sourceId = id;

    const outline = createSourceOutline();
    const trailLine = createTrailLine();
    trailLine.visible = trailsEnabled;
    scene.add(mesh);
    scene.add(outline);
    scene.add(trailLine);

    const label = createLabelSprite(formatObjectLabel(id));
    label.userData.sourceId = id;
    scene.add(label);

    sourceMeshes.set(id, mesh);
    sourceLabels.set(id, label);
    sourceOutlines.set(id, outline);
    sourceTrails.set(id, { positions: [], line: trailLine });
    applySourceLevel(id, mesh, sourceLevels.get(id));
    updateSourceSelectionStyles();
  }
  return sourceMeshes.get(id);
}

function updateSource(id, position) {
  const mesh = getSourceMesh(id);
  const raw = {
    x: Number(position.x) || 0,
    y: Number(position.y) || 0,
    z: Number(position.z) || 0
  };
  sourcePositionsRaw.set(String(id), raw);
  const scaled = {
    x: raw.x * roomRatio.length,
    y: raw.y * roomRatio.height,
    z: raw.z * roomRatio.width
  };
  mesh.position.set(scaled.x, scaled.y, scaled.z);

  const trail = sourceTrails.get(id);
  if (trail) {
    trail.positions.push(raw);
    if (trail.positions.length > trailMaxPoints) {
      trail.positions.shift();
    }
    if (trailsEnabled) {
      rebuildTrailGeometry(id);
    }
  }

  updateSourceDecorations(id);
  if (position && typeof position.name === 'string' && position.name.trim()) {
    sourceNames.set(String(id), position.name.trim());
  }
  const label = sourceLabels.get(id);
  if (label) {
    setLabelSpriteText(label, formatObjectLabel(String(id)));
  }
  const key = String(id);
  if (!objectItems.has(key)) {
    renderObjectsList();
  } else {
    updateObjectPositionUI(key, raw);
    updateObjectLabelUI(key);
  }
}

function updateSourceLevel(id, meter) {
  sourceLevels.set(id, meter);
  const mesh = sourceMeshes.get(id);
  if (mesh) {
    applySourceLevel(id, mesh, meter);
  }
  updateObjectMeterUI(String(id));
}

function normalizeGainsPayload(payload) {
  if (Array.isArray(payload)) {
    return payload;
  }
  if (payload && Array.isArray(payload.gains)) {
    return payload.gains;
  }
  return [];
}

function updateSourceGains(id, gainsPayload) {
  sourceGains.set(id, normalizeGainsPayload(gainsPayload));
  if (selectedSourceId === id) {
    updateSpeakerColorsFromSelection();
  }
}

function removeSource(id) {
  const mesh = sourceMeshes.get(id);
  if (!mesh) return;
  const label = sourceLabels.get(id);
  scene.remove(mesh);
  if (label) {
    scene.remove(label);
    label.material.map.dispose();
    label.material.dispose();
  }
  const outline = sourceOutlines.get(id);
  if (outline) {
    scene.remove(outline);
    outline.geometry.dispose();
    outline.material.dispose();
  }
  const trail = sourceTrails.get(id);
  if (trail) {
    scene.remove(trail.line);
    trail.line.geometry.dispose();
    trail.line.material.dispose();
    sourceTrails.delete(id);
  }
  mesh.geometry.dispose();
  mesh.material.dispose();
  sourceMeshes.delete(id);
  sourceLabels.delete(id);
  sourceLevels.delete(id);
  sourceGains.delete(id);
  sourceOutlines.delete(id);
  sourceNames.delete(String(id));
  dirtyObjectMeters.delete(String(id));
  dirtyObjectPositions.delete(String(id));
  dirtyObjectLabels.delete(String(id));

  if (selectedSourceId === id) {
    setSelectedSource(null);
  }
  objectMuted.delete(String(id));
  objectManualMuted.delete(String(id));
  objectBaseGains.delete(String(id));
  const entry = objectItems.get(String(id));
  if (entry) {
    entry.root.remove();
    objectItems.delete(String(id));
  }
  updateObjectControlsUI();
  updateSectionProportions();
}

function clearSpeakers() {
  speakerMeshes.forEach((mesh) => {
    scene.remove(mesh);
    mesh.geometry.dispose();
    mesh.material.dispose();
  });
  speakerLabels.forEach((label) => {
    scene.remove(label);
    label.material.map.dispose();
    label.material.dispose();
  });
  speakerMeshes.length = 0;
  speakerLabels.length = 0;
}

function renderLayout(key) {
  clearSpeakers();
  const layout = layoutsByKey.get(key);
  if (!layout) {
    currentLayoutKey = null;
    currentLayoutSpeakers = [];
    renderSpeakersList();
    selectedSpeakerIndex = null;
    updateSpeakerGizmo();
    updateControlsForEditMode();
    return;
  }

  currentLayoutKey = key;
  currentLayoutSpeakers = Array.isArray(layout.speakers) ? layout.speakers : [];
  selectedSpeakerIndex = null;
  updateSpeakerGizmo();
  updateControlsForEditMode();
  const speakerIds = getSpeakerIds();
  speakerMuted.forEach((id) => {
    if (!speakerIds.includes(id)) {
      speakerMuted.delete(id);
    }
  });
  speakerManualMuted.forEach((id) => {
    if (!speakerIds.includes(id)) {
      speakerManualMuted.delete(id);
    }
  });
  speakerBaseGains.forEach((_, id) => {
    if (!speakerIds.includes(id)) {
      speakerBaseGains.delete(id);
    }
  });

  layout.speakers.forEach((speaker, index) => {
    const mesh = new THREE.Mesh(speakerGeometry.clone(), speakerMaterial.clone());
    mesh.position.set(speaker.x, speaker.y, speaker.z);
    const baseOpacity = speaker.spatialize === 0 ? 0.3 : 0.65;
    mesh.userData.baseOpacity = baseOpacity;
    mesh.material.opacity = baseOpacity;
    scene.add(mesh);
    speakerMeshes.push(mesh);

    const label = createLabelSprite(String(speaker.id || index));
    label.position.set(speaker.x, speaker.y + 0.12, speaker.z);
    scene.add(label);
    speakerLabels.push(label);

    applySpeakerLevel(mesh, speakerLevels.get(String(index)));
  });

  updateSpeakerColorsFromSelection();
  refreshOverlayLists();
}

function updateSpeakerLevel(index, meter) {
  speakerLevels.set(String(index), meter);
  const mesh = speakerMeshes[index];
  if (mesh) {
    applySpeakerLevel(mesh, meter);
  }
  updateSpeakerMeterUI(String(index));
  dirtyMasterMeter = true;
  scheduleUIFlush();
}

function applyRoomRatio(nextRatio) {
  roomRatio.width = Number(nextRatio.width) || 1;
  roomRatio.length = Number(nextRatio.length) || 1;
  roomRatio.height = Number(nextRatio.height) || 1;
  updateRoomRatioDisplay();
  applyRoomRatioToScene();

  sourceMeshes.forEach((mesh, id) => {
    const raw = sourcePositionsRaw.get(String(id));
    if (!raw) return;
    mesh.position.set(raw.x * roomRatio.length, raw.y * roomRatio.height, raw.z * roomRatio.width);
    updateSourceDecorations(id);
    rebuildTrailGeometry(id);
  });

  speakerMeshes.forEach((mesh, index) => {
    const speaker = currentLayoutSpeakers[index];
    if (!speaker) return;
    mesh.position.set(speaker.x, speaker.y, speaker.z);
    const label = speakerLabels[index];
    if (label) {
      label.position.set(speaker.x, speaker.y + 0.12, speaker.z);
    }
  });
}

function hydrateLayoutSelect(layouts, selectedLayoutKey) {
  layoutsByKey.clear();
  layoutSelectEl.innerHTML = '';

  layouts.forEach((layout) => {
    layoutsByKey.set(layout.key, layout);

    const option = document.createElement('option');
    option.value = layout.key;
    option.textContent = layout.name;
    layoutSelectEl.appendChild(option);
  });

  if (selectedLayoutKey && layoutsByKey.has(selectedLayoutKey)) {
    layoutSelectEl.value = selectedLayoutKey;
    renderLayout(selectedLayoutKey);
  } else {
    currentLayoutKey = null;
    currentLayoutSpeakers = [];
    renderSpeakersList();
  }

  layoutSelectEl.disabled = layouts.length === 0;
}

function pointerEventToNdc(event) {
  const rect = renderer.domElement.getBoundingClientRect();
  pointer.x = ((event.clientX - rect.left) / rect.width) * 2 - 1;
  pointer.y = -((event.clientY - rect.top) / rect.height) * 2 + 1;
}

function selectSourceFromPointer(event) {
  pointerEventToNdc(event);

  raycaster.setFromCamera(pointer, camera);
  const hitTargets = [...sourceLabels.values(), ...sourceMeshes.values()];
  const intersects = raycaster.intersectObjects(hitTargets, false);

  if (intersects.length > 0) {
    const selectedId = intersects[0].object?.userData?.sourceId || null;
    setSelectedSource(selectedId);
    return true;
  }

  return false;
}

function selectSpeakerFromPointer(event) {
  pointerEventToNdc(event);
  raycaster.setFromCamera(pointer, camera);
  const hitTargets = [...speakerMeshes, ...speakerLabels];
  const intersects = raycaster.intersectObjects(hitTargets, false);
  if (intersects.length > 0) {
    const idx = speakerMeshes.indexOf(intersects[0].object);
    if (idx >= 0) {
      setSelectedSource(null);
      setSelectedSpeaker(idx);
      return true;
    }
    const labelIdx = speakerLabels.indexOf(intersects[0].object);
    if (labelIdx >= 0) {
      setSelectedSource(null);
      setSelectedSpeaker(labelIdx);
      return true;
    }
  }
  return false;
}

function beginSpeakerDrag(event) {
  if (activeEditMode !== 'polar' || selectedSpeakerIndex === null) {
    return false;
  }
  pointerEventToNdc(event);
  raycaster.setFromCamera(pointer, camera);
  const gizmoHits = raycaster.intersectObjects([speakerGizmo.ring, speakerGizmo.arc], false);
  if (gizmoHits.length === 0) {
    return false;
  }
  const hit = gizmoHits[0].object;
  dragMode = hit === speakerGizmo.ring ? 'azimuth' : 'elevation';
  isDraggingSpeaker = true;
  draggingPointerId = event.pointerId;
  dragAzimuthDelta = 1;
  dragElevationDelta = 1;
  controls.enabled = false;
  return true;
}

function updateSpeakerDrag(event) {
  if (!isDraggingSpeaker || selectedSpeakerIndex === null) {
    return;
  }
  pointerEventToNdc(event);
  raycaster.setFromCamera(pointer, camera);

  if (dragMode === 'azimuth') {
    const plane = new THREE.Plane(new THREE.Vector3(0, 1, 0), 0);
    const hitPoint = new THREE.Vector3();
    if (raycaster.ray.intersectPlane(plane, hitPoint)) {
      dragAzimuthDeg = (Math.atan2(hitPoint.z, hitPoint.x) * 180) / Math.PI;
      dragAzimuthDeg = normalizeAngleDeg(dragAzimuthDeg);
      const radial = Math.sqrt(hitPoint.x * hitPoint.x + hitPoint.z * hitPoint.z);
      const delta = (radial - dragDistance) / dragDistance;
      dragAzimuthDelta = delta;
      if (delta >= 0 && delta <= 0.1) {
        dragAzimuthDeg = snapAngleDeg(dragAzimuthDeg, 1, 0.5);
      } else if (delta > 0.1) {
        dragAzimuthDeg = snapAngleDeg(dragAzimuthDeg, 5, 2.5);
      }
    }
  } else if (dragMode === 'elevation') {
    const azRad = (dragAzimuthDeg * Math.PI) / 180;
    const dir = new THREE.Vector3(Math.cos(azRad), 0, Math.sin(azRad));
    const normal = new THREE.Vector3().crossVectors(dir, new THREE.Vector3(0, 1, 0)).normalize();
    const plane = new THREE.Plane().setFromNormalAndCoplanarPoint(normal, new THREE.Vector3(0, 0, 0));
    const hitPoint = new THREE.Vector3();
    if (raycaster.ray.intersectPlane(plane, hitPoint)) {
      const planar = Math.sqrt(hitPoint.x * hitPoint.x + hitPoint.z * hitPoint.z);
      dragElevationDeg = (Math.atan2(hitPoint.y, planar) * 180) / Math.PI;
      dragElevationDeg = Math.max(-90, Math.min(90, dragElevationDeg));
      const radius = Math.sqrt(hitPoint.x * hitPoint.x + hitPoint.y * hitPoint.y + hitPoint.z * hitPoint.z);
      const delta = (radius - dragDistance) / dragDistance;
      dragElevationDelta = delta;
      if (delta >= 0 && delta <= 0.1) {
        dragElevationDeg = snapAngleDeg(dragElevationDeg, 1, 0.5);
      } else if (delta > 0.1) {
        dragElevationDeg = snapAngleDeg(dragElevationDeg, 5, 2.5);
      }
    }
  }

  const pos = sphericalToCartesianDeg(dragAzimuthDeg, dragElevationDeg, dragDistance);
  const mesh = speakerMeshes[selectedSpeakerIndex];
  if (mesh) {
    mesh.position.set(pos.x, pos.y, pos.z);
  }
  const label = speakerLabels[selectedSpeakerIndex];
  if (label) {
    label.position.set(pos.x, pos.y + 0.12, pos.z);
  }
  const speaker = currentLayoutSpeakers[selectedSpeakerIndex];
  if (speaker) {
    speaker.x = pos.x;
    speaker.y = pos.y;
    speaker.z = pos.z;
    const entry = speakerItems.get(String(selectedSpeakerIndex));
    if (entry) {
      entry.position.textContent = formatPosition(speaker);
    }
  }
  updateSpeakerGizmo();
}

function endSpeakerDrag() {
  if (!isDraggingSpeaker || selectedSpeakerIndex === null) {
    return;
  }
  isDraggingSpeaker = false;
  dragMode = null;
  draggingPointerId = null;
  controls.enabled = true;

  if (ws.readyState === WebSocket.OPEN && selectedSpeakerIndex !== null) {
    const idx = selectedSpeakerIndex;
    ws.send(JSON.stringify({ type: 'control:speaker:az', id: idx, value: dragAzimuthDeg }));
    ws.send(JSON.stringify({ type: 'control:speaker:el', id: idx, value: dragElevationDeg }));
    ws.send(JSON.stringify({ type: 'control:speaker:distance', id: idx, value: dragDistance }));
    ws.send(JSON.stringify({ type: 'control:speakers:apply' }));
  }
}

renderer.domElement.addEventListener('pointerdown', (event) => {
  pointerDownPosition = { x: event.clientX, y: event.clientY };
  if (beginSpeakerDrag(event)) {
    pointerDownPosition = null;
    return;
  }
});

renderer.domElement.addEventListener('pointerup', (event) => {
  if (isDraggingSpeaker && event.pointerId === draggingPointerId) {
    endSpeakerDrag();
  }
  if (!pointerDownPosition) {
    return;
  }

  const dx = event.clientX - pointerDownPosition.x;
  const dy = event.clientY - pointerDownPosition.y;
  pointerDownPosition = null;

  if (Math.hypot(dx, dy) <= 6) {
    const hitSpeaker = selectSpeakerFromPointer(event);
    if (hitSpeaker) {
      return;
    }
    const hitSource = selectSourceFromPointer(event);
    if (hitSource) {
      setSelectedSpeaker(null);
      updateControlsForEditMode();
      return;
    }
    setSelectedSource(null);
    setSelectedSpeaker(null);
    updateControlsForEditMode();
  }
});

renderer.domElement.addEventListener('pointermove', (event) => {
  if (isDraggingSpeaker && event.pointerId === draggingPointerId) {
    updateSpeakerDrag(event);
  }
});

renderer.domElement.addEventListener('pointercancel', () => {
  endSpeakerDrag();
});

renderer.domElement.addEventListener('pointerleave', () => {
  endSpeakerDrag();
});

renderer.domElement.addEventListener('wheel', (event) => {
  if (activeEditMode !== 'polar' || selectedSpeakerIndex === null) {
    return;
  }
  if (!event.ctrlKey && !event.shiftKey) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  const prevZoom = controls.enableZoom;
  controls.enableZoom = false;

  const delta = -Math.sign(event.deltaY);
  const step = event.shiftKey ? 0.01 : 0.05;
  const next = Math.min(2.0, Math.max(0.2, dragDistance + delta * step));
  if (next === dragDistance) {
    return;
  }
  dragDistance = next;
  const pos = sphericalToCartesianDeg(dragAzimuthDeg, dragElevationDeg, dragDistance);
  const mesh = speakerMeshes[selectedSpeakerIndex];
  if (mesh) {
    mesh.position.set(pos.x, pos.y, pos.z);
  }
  const label = speakerLabels[selectedSpeakerIndex];
  if (label) {
    label.position.set(pos.x, pos.y + 0.12, pos.z);
  }
  const speaker = currentLayoutSpeakers[selectedSpeakerIndex];
  if (speaker) {
    speaker.x = pos.x;
    speaker.y = pos.y;
    speaker.z = pos.z;
    speaker.distanceM = dragDistance;
    const entry = speakerItems.get(String(selectedSpeakerIndex));
    if (entry) {
      entry.position.textContent = formatPosition(speaker);
    }
  }
  updateSpeakerGizmo();
  controls.enableZoom = prevZoom;
}, { passive: false, capture: true });

const wsProtocol = location.protocol === 'https:' ? 'wss' : 'ws';
const ws = new WebSocket(`${wsProtocol}://${location.host}`);

if (masterGainSliderEl) {
  masterGainSliderEl.addEventListener('input', () => {
    const value = Number(masterGainSliderEl.value);
    if (!Number.isFinite(value)) {
      return;
    }
    masterGain = value;
    updateMasterGainUI();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: 'control:master:gain',
          gain: masterGain
        })
      );
    }
  });

  masterGainSliderEl.addEventListener('dblclick', () => {
    masterGain = 1;
    updateMasterGainUI();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: 'control:master:gain',
          gain: masterGain
        })
      );
    }
  });
}

if (loudnessToggleEl) {
  loudnessToggleEl.addEventListener('change', () => {
    const enabled = loudnessToggleEl.checked ? 1 : 0;
    loudnessEnabled = enabled === 1;
    updateDialogNormDisplay();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: 'control:loudness',
          enable: enabled
        })
      );
    }
  });
}

if (spreadMinSliderEl) {
  spreadMinSliderEl.addEventListener('input', () => {
    const valueDeg = Number(spreadMinSliderEl.value);
    if (!Number.isFinite(valueDeg)) {
      return;
    }
    const valueNorm = Math.max(0, Math.min(180, valueDeg)) / 180.0;
    const maxValue = spreadState.max === null ? 1 : spreadState.max;
    spreadState.min = Math.min(valueNorm, maxValue);
    spreadMinSliderEl.value = String((spreadState.min ?? 0) * 180.0);
    updateSpreadDisplay();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: 'control:spread:min',
          value: spreadState.min
        })
      );
    }
  });
}

if (spreadMaxSliderEl) {
  spreadMaxSliderEl.addEventListener('input', () => {
    const valueDeg = Number(spreadMaxSliderEl.value);
    if (!Number.isFinite(valueDeg)) {
      return;
    }
    const valueNorm = Math.max(0, Math.min(180, valueDeg)) / 180.0;
    const minValue = spreadState.min === null ? 0 : spreadState.min;
    spreadState.max = Math.max(valueNorm, minValue);
    spreadMaxSliderEl.value = String((spreadState.max ?? 1) * 180.0);
    updateSpreadDisplay();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(
        JSON.stringify({
          type: 'control:spread:max',
          value: spreadState.max
        })
      );
    }
  });
}

if (distanceModelSelectEl) {
  distanceModelSelectEl.addEventListener('change', () => {
    const value = String(distanceModelSelectEl.value || '').trim().toLowerCase();
    if (!['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
      return;
    }
    distanceModel = value;
    updateDistanceModelUI();
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'control:distance_model', value }));
    }
  });
}

if (distanceDiffuseToggleEl) {
  distanceDiffuseToggleEl.addEventListener('change', () => {
    const enabled = distanceDiffuseToggleEl.checked;
    distanceDiffuseState.enabled = enabled;
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'control:distance_diffuse:enabled', enable: enabled ? 1 : 0 }));
    }
  });
}

if (distanceDiffuseThresholdSliderEl) {
  distanceDiffuseThresholdSliderEl.addEventListener('input', () => {
    const value = Number(distanceDiffuseThresholdSliderEl.value);
    if (!Number.isFinite(value)) return;
    distanceDiffuseState.threshold = value;
    if (distanceDiffuseThresholdValEl) distanceDiffuseThresholdValEl.textContent = formatNumber(value, 2);
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'control:distance_diffuse:threshold', value }));
    }
  });
}

if (distanceDiffuseCurveSliderEl) {
  distanceDiffuseCurveSliderEl.addEventListener('input', () => {
    const value = Number(distanceDiffuseCurveSliderEl.value);
    if (!Number.isFinite(value)) return;
    distanceDiffuseState.curve = value;
    if (distanceDiffuseCurveValEl) distanceDiffuseCurveValEl.textContent = formatNumber(value, 2);
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'control:distance_diffuse:curve', value }));
    }
  });
}

if (saveConfigBtnEl) {
  saveConfigBtnEl.addEventListener('click', () => {
    if (ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: 'control:save_config' }));
    }
  });
}

if (editModeSelectEl) {
  editModeSelectEl.addEventListener('change', () => {
    activeEditMode = editModeSelectEl.value;
    updateSpeakerGizmo();
    updateControlsForEditMode();
  });
}

if (trailToggleEl) {
  trailToggleEl.addEventListener('change', () => {
    trailsEnabled = trailToggleEl.checked;
    sourceTrails.forEach((trail, id) => {
      trail.line.visible = trailsEnabled;
      if (trailsEnabled) {
        rebuildTrailGeometry(id);
      }
    });
  });
}

if (trailLengthSliderEl) {
  trailLengthSliderEl.addEventListener('input', () => {
    trailMaxPoints = Number(trailLengthSliderEl.value);
    if (trailLengthValEl) trailLengthValEl.textContent = String(trailMaxPoints);
    sourceTrails.forEach((trail, id) => {
      if (trail.positions.length > trailMaxPoints) {
        trail.positions.splice(0, trail.positions.length - trailMaxPoints);
      }
      rebuildTrailGeometry(id);
    });
  });
}

ws.onopen = () => {
  statusEl.textContent = 'connecté';
};

ws.onclose = () => {
  statusEl.textContent = 'déconnecté';
};

layoutSelectEl.addEventListener('change', () => {
  if (ws.readyState !== WebSocket.OPEN) {
    return;
  }

  ws.send(
    JSON.stringify({
      type: 'layout:select',
      key: layoutSelectEl.value
    })
  );
});

ws.onmessage = (event) => {
  const payload = JSON.parse(event.data);

  if (payload.type === 'state:init') {
    speakerMuted.clear();
    objectMuted.clear();
    speakerManualMuted.clear();
    objectManualMuted.clear();

    Object.entries(payload.sources).forEach(([id, position]) => {
      updateSource(id, position);
    });
    Object.entries(payload.sourceLevels || {}).forEach(([id, meter]) => {
      updateSourceLevel(id, meter);
    });
    Object.entries(payload.speakerLevels || {}).forEach(([index, meter]) => {
      updateSpeakerLevel(Number(index), meter);
    });
    Object.entries(payload.objectSpeakerGains || {}).forEach(([id, gains]) => {
      updateSourceGains(id, gains);
    });
    Object.entries(payload.objectGains || {}).forEach(([id, gain]) => {
      objectGainCache.set(String(id), Number(gain));
    });
    Object.entries(payload.speakerGains || {}).forEach(([id, gain]) => {
      speakerGainCache.set(String(id), Number(gain));
    });
    Object.entries(payload.objectMutes || {}).forEach(([id, muted]) => {
      const key = String(id);
      if (Number(muted)) {
        objectMuted.add(key);
      }
    });
    Object.entries(payload.speakerMutes || {}).forEach(([id, muted]) => {
      const key = String(id);
      if (Number(muted)) {
        speakerMuted.add(key);
      }
    });

    if (payload.roomRatio) {
      applyRoomRatio(payload.roomRatio);
    } else {
      updateRoomRatioDisplay();
      applyRoomRatioToScene();
    }
    if (payload.spread) {
      if (typeof payload.spread.min === 'number') {
        spreadState.min = payload.spread.min;
      }
      if (typeof payload.spread.max === 'number') {
        spreadState.max = payload.spread.max;
      }
    }
    updateSpreadDisplay();
    if (typeof payload.loudness === 'number') {
      loudnessEnabled = payload.loudness !== 0;
    }
    if (typeof payload.loudnessSource === 'number') {
      loudnessSource = payload.loudnessSource;
    }
    if (typeof payload.loudnessGain === 'number') {
      loudnessGain = payload.loudnessGain;
    }
    updateDialogNormDisplay();
    if (typeof payload.masterGain === 'number') {
      masterGain = payload.masterGain;
    }
    updateMasterGainUI();
    if (typeof payload.distanceModel === 'string') {
      const value = payload.distanceModel.trim().toLowerCase();
      if (['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
        distanceModel = value;
      }
    }
    updateDistanceModelUI();
    if (payload.distanceDiffuse) {
      if (typeof payload.distanceDiffuse.enabled === 'boolean') {
        distanceDiffuseState.enabled = payload.distanceDiffuse.enabled;
      }
      if (typeof payload.distanceDiffuse.threshold === 'number') {
        distanceDiffuseState.threshold = payload.distanceDiffuse.threshold;
      }
      if (typeof payload.distanceDiffuse.curve === 'number') {
        distanceDiffuseState.curve = payload.distanceDiffuse.curve;
      }
    }
    updateDistanceDiffuseUI();
    if (typeof payload.configSaved === 'number') {
      configSaved = payload.configSaved !== 0;
    }
    updateConfigSavedUI();
    if (typeof payload.latencyMs === 'number') {
      latencyMs = payload.latencyMs;
    }
    if (typeof payload.resampleRatio === 'number') {
      resampleRatio = payload.resampleRatio;
    }
    updateLatencyDisplay();
    updateLatencyMeterUI();
    updateResampleRatioDisplay();
    updateMasterMeterUI();

    hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
    refreshOverlayLists();
  }

  if (payload.type === 'layouts:update') {
    hydrateLayoutSelect(payload.layouts || [], payload.selectedLayoutKey);
  }

  if (payload.type === 'layout:selected') {
    if (payload.key && layoutsByKey.has(payload.key)) {
      layoutSelectEl.value = payload.key;
      renderLayout(payload.key);
    }
  }

  if (payload.type === 'source:update') {
    updateSource(payload.id, payload.position);
  }

  if (payload.type === 'source:meter') {
    updateSourceLevel(payload.id, payload.meter);
  }

  if (payload.type === 'source:gains') {
    updateSourceGains(payload.id, payload.gains);
  }

  if (payload.type === 'speaker:meter') {
    updateSpeakerLevel(Number(payload.id), payload.meter);
  }

  if (payload.type === 'object:gain') {
    objectGainCache.set(String(payload.id), Number(payload.gain));
    updateObjectControlsUI();
  }

  if (payload.type === 'speaker:gain') {
    speakerGainCache.set(String(payload.id), Number(payload.gain));
    updateSpeakerControlsUI();
  }

  if (payload.type === 'object:mute') {
    const key = String(payload.id);
    if (Number(payload.muted)) {
      objectMuted.add(key);
    } else {
      objectMuted.delete(key);
      objectManualMuted.delete(key);
    }
    updateObjectControlsUI();
  }

  if (payload.type === 'speaker:mute') {
    const key = String(payload.id);
    if (Number(payload.muted)) {
      speakerMuted.add(key);
    } else {
      speakerMuted.delete(key);
      speakerManualMuted.delete(key);
    }
    updateSpeakerControlsUI();
  }

  if (payload.type === 'room_ratio') {
    if (payload.roomRatio) {
      applyRoomRatio(payload.roomRatio);
    }
  }

  if (payload.type === 'spread:min') {
    spreadState.min = Number(payload.value);
    updateSpreadDisplay();
  }

  if (payload.type === 'spread:max') {
    spreadState.max = Number(payload.value);
    updateSpreadDisplay();
  }

  if (payload.type === 'loudness') {
    loudnessEnabled = Number(payload.enabled) !== 0;
    updateDialogNormDisplay();
  }

  if (payload.type === 'loudness:source') {
    loudnessSource = Number(payload.value);
    updateDialogNormDisplay();
  }

  if (payload.type === 'loudness:gain') {
    loudnessGain = Number(payload.value);
    updateDialogNormDisplay();
  }

  if (payload.type === 'master:gain') {
    masterGain = Number(payload.value);
    updateMasterGainUI();
  }

  if (payload.type === 'distance_model') {
    const value = String(payload?.value ?? '').trim().toLowerCase();
    if (['none', 'linear', 'quadratic', 'inverse-square'].includes(value)) {
      distanceModel = value;
      updateDistanceModelUI();
    }
  }

  if (payload.type === 'distance_diffuse:enabled') {
    distanceDiffuseState.enabled = payload.enabled === true;
    updateDistanceDiffuseUI();
  }

  if (payload.type === 'distance_diffuse:threshold') {
    distanceDiffuseState.threshold = Number(payload.value);
    updateDistanceDiffuseUI();
  }

  if (payload.type === 'distance_diffuse:curve') {
    distanceDiffuseState.curve = Number(payload.value);
    updateDistanceDiffuseUI();
  }

  if (payload.type === 'config:saved') {
    configSaved = payload.saved !== 0;
    updateConfigSavedUI();
  }

  if (payload.type === 'latency') {
    latencyMs = Number(payload.value);
    updateLatencyDisplay();
    updateLatencyMeterUI();
  }

  if (payload.type === 'resample_ratio') {
    resampleRatio = Number(payload.value);
    updateResampleRatioDisplay();
  }

  if (payload.type === 'source:remove') {
    removeSource(payload.id);
  }
};

function animate() {
  requestAnimationFrame(animate);
  controls.update();
  updateRoomFaceVisibility();

  sourceOutlines.forEach((outline) => {
    outline.quaternion.copy(camera.quaternion);
  });

  renderer.render(scene, camera);
}

animate();

window.addEventListener('resize', () => {
  camera.aspect = window.innerWidth / window.innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(window.innerWidth, window.innerHeight);
});
