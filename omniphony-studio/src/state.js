/**
 * Shared application state.
 *
 * All Maps/Sets are exported directly (mutable by reference).
 * Primitive values are grouped in the `app` and `dirty` objects so that
 * mutations from any module are visible everywhere.
 */

import * as THREE from 'three';

// ---------------------------------------------------------------------------
// Source / speaker data maps
// ---------------------------------------------------------------------------

export const sourceMeshes = new Map();
export const sourceLabels = new Map();
export const sourceOutlines = new Map();
export const sourceLevels = new Map();
export const speakerLevels = new Map();
export const sourceLevelLastSeen = new Map();
export const speakerLevelLastSeen = new Map();
export const sourceGains = new Map();
export const speakerGainCache = new Map();
export const objectGainCache = new Map();
export const speakerBaseGains = new Map();
export const objectBaseGains = new Map();
export const speakerDelays = new Map();
export const speakerMuted = new Set();
export const objectMuted = new Set();
export const speakerItems = new Map();
export const objectItems = new Map();
export const speakerManualMuted = new Set();
export const objectManualMuted = new Set();
export const sourceNames = new Map();
export const sourcePositionsRaw = new Map();
export const sourceDirectSpeakerIndices = new Map();
export const sourceTrails = new Map();
export const sourceEffectiveMarkers = new Map();
export const sourceEffectiveLines = new Map();
export const sourceBaseColors = new Map();
export const layoutsByKey = new Map();

// Speaker meshes/labels are arrays (indexed by speaker slot)
export const speakerMeshes = [];
export const speakerLabels = [];

// ---------------------------------------------------------------------------
// UI item registries
// ---------------------------------------------------------------------------

export const speakerReorderAnimations = new WeakMap();

// ---------------------------------------------------------------------------
// Dirty flags (UI flush batching)
// ---------------------------------------------------------------------------

export const dirtyObjectMeters = new Set();
export const dirtySpeakerMeters = new Set();
export const dirtyObjectPositions = new Set();
export const dirtyObjectLabels = new Set();

export const dirty = {
  masterMeter: false,
  roomRatio: false,
  spread: false,
  vbapMode: false,
  renderBackend: false,
  vbapCartesian: false,
  vbapPolar: false,
  loudness: false,
  adaptiveResampling: false,
  distanceDiffuse: false,
  distanceModel: false,
  configSaved: false,
  latency: false,
  renderTime: false,
  resample: false,
  audioFormat: false,
  masterGain: false
};

// ---------------------------------------------------------------------------
// Application state (primitive values)
// ---------------------------------------------------------------------------

export const app = {
  // Room geometry
  roomRatio: { width: 1, length: 2, height: 1, rear: 1, lower: 0.5, centerBlend: 0.5 },
  roomMasterAxis: 'width',
  roomAxisDrivers: {
    width: 'size',
    length: 'size',
    height: 'size',
    rear: 'size',
    lower: 'size'
  },
  roomGeometryExpanded: false,
  roomGeometryBaselineKey: '',
  roomGeometryApplyTimer: null,
  metersPerUnit: 1.0,

  // VBAP
  vbapCartesianState: { xSize: null, ySize: null, zSize: null, zNegSize: 0 },
  vbapPolarState: { azimuthResolution: null, elevationResolution: null, distanceRes: null, distanceMax: null },
  evaluationModeState: { selection: null, effective: null },
  renderBackendState: {
    selection: null,
    effective: null,
    effectiveLabel: null,
    capabilities: null,
    allowedEvaluationModes: [],
    frozenRoomRatio: false,
    frozenSpeakers: false,
    restoreBackendAvailable: false
  },
  vbapPositionInterpolation: null,
  vbapAllowNegativeZ: null,
  vbapRecomputing: null,
  vbapCartesianFaceGridEnabled: false,

  // Spread
  spreadState: { min: null, max: null, fromDistance: null, distanceRange: null, distanceCurve: null },

  // Distance diffuse
  distanceDiffuseState: { enabled: null, threshold: null, curve: null },
  distanceModel: 'none',

  // Master
  masterGain: null,

  // Loudness
  loudnessEnabled: null,
  loudnessSource: null,
  loudnessGain: null,

  // Config
  configSaved: null,

  // Adaptive resampling
  adaptiveResamplingEnabled: false,
  adaptiveResamplingPaused: false,
  adaptiveResamplingEnableFarMode: true,
  adaptiveResamplingForceSilenceInFarMode: false,
  adaptiveResamplingHardRecoverHighInFarMode: true,
  adaptiveResamplingHardRecoverLowInFarMode: false,
  adaptiveResamplingFarModeReturnFadeInMs: 0,
  adaptiveResamplingKpNear: 10.0,
  adaptiveResamplingKi: 50.0,
  adaptiveResamplingIntegralDischargeRatio: 0.25,
  adaptiveResamplingMaxAdjust: 0.01,
  adaptiveResamplingNearFarThresholdMs: 120,
  adaptiveResamplingUpdateIntervalCallbacks: 10,
  adaptiveResamplingBand: null,
  adaptiveResamplingState: null,

  // Latency & performance
  latencyMs: null,
  latencyInstantMs: null,
  latencyControlMs: null,
  latencyTargetMs: null,
  latencyRequestedMs: null,
  decodeTimeMs: null,
  decodeTimeWindow: [],
  renderTimeMs: null,
  renderTimeWindow: [],
  writeTimeMs: null,
  writeTimeWindow: [],
  frameDurationMs: null,
  latencyRawWindow: [],
  resampleRatio: null,
  latencyTargetApplyTimer: null,

  // Audio
  audioSampleRate: null,
  rampMode: 'sample',
  audioOutputDevice: null,
  audioOutputDeviceEffective: null,
  audioOutputDevices: [],
  orenderInputPipe: null,
  audioSampleFormat: null,
  audioError: null,
  inputMode: 'pipe_bridge',
  inputActiveMode: 'pipe_bridge',
  inputApplyPending: false,
  inputBackend: null,
  inputChannels: null,
  inputSampleRate: null,
  inputStreamFormat: null,
  inputError: null,
  liveInput: {
    backend: 'pipewire',
    node: '',
    description: '',
    layout: '',
    clockMode: 'dac',
    channels: 8,
    sampleRate: 48000,
    format: 'f32',
    map: '7.1-fixed',
    lfeMode: 'object'
  },

  // OSC
  oscMeteringEnabled: false,
  oscSnapshotReady: false,
  oscStatusState: 'initializing',
  oscConfigAutoOpenTimer: null,
  oscLaunchPending: false,
  oscConfiguredOrenderPath: '',
  oscConfigBaselineKey: '',
  orenderServiceInstalled: false,
  orenderServiceRunning: false,
  orenderServiceManager: null,
  orenderServicePending: false,

  // Editing state
  audioOutputDeviceEditing: false,
  audioSampleRateEditing: false,
  latencyTargetEditing: false,
  latencyTargetDirty: false,
  adaptiveKpNearEditing: false,
  adaptiveKpNearDirty: false,
  adaptiveKiEditing: false,
  adaptiveKiDirty: false,
  adaptiveIntegralDischargeRatioEditing: false,
  adaptiveIntegralDischargeRatioDirty: false,
  adaptiveMaxAdjustEditing: false,
  adaptiveMaxAdjustDirty: false,
  adaptiveNearFarThresholdEditing: false,
  adaptiveNearFarThresholdDirty: false,
  adaptiveUpdateIntervalCallbacksEditing: false,
  adaptiveUpdateIntervalCallbacksDirty: false,
  adaptiveFarFadeInMsEditing: false,
  adaptiveFarFadeInMsDirty: false,
  telemetryGaugesOpen: false,
  audioOutputSectionOpen: false,
  inputSectionOpen: false,
  rendererSectionOpen: false,
  displaySectionOpen: false,

  // Selection & drag
  selectedSourceId: null,
  selectedSpeakerIndex: null,
  draggedSpeakerIndex: null,
  draggedSpeakerInitialIndex: null,
  draggedSpeakerDidDrop: false,
  draggedSpeakerRoot: null,
  polarEditArmed: false,
  cartesianEditArmed: false,
  activeEditMode: 'polar',
  isDraggingSpeaker: false,
  dragMode: null,
  dragAxis: null,
  dragAxisOrigin: new THREE.Vector3(),
  dragAxisDirection: new THREE.Vector3(1, 0, 0),
  dragSpeakerStartPosition: new THREE.Vector3(),
  dragAxisStartT: 0,
  dragAzimuthDeg: 0,
  dragElevationDeg: 0,
  dragDistance: 1,
  dragAzimuthDelta: 1,
  dragElevationDelta: 1,
  pointerDownPosition: null,
  draggingPointerId: null,

  // Trail
  trailsEnabled: true,
  trailRenderMode: 'diffuse',
  trailPointTtlMs: 7000,
  speakerHeatmapSlicesEnabled: true,
  speakerHeatmapVolumeEnabled: false,
  speakerHeatmapSampleCount: 3072,
  speakerHeatmapMaxSphereSize: 0.062,
  effectiveRenderEnabled: false,
  objectColorsEnabled: false,
  lastTrailDecayAt: 0,

  // Layout
  currentLayoutKey: null,
  currentLayoutSpeakers: [],

  // UI flush
  uiFlushScheduled: false,

  // Meter decay
  lastMeterDecayAt: 0
};

export function isRoomRatioFrozen() {
  return app.renderBackendState.frozenRoomRatio === true;
}

export function isSpeakerLayoutFrozen() {
  return app.renderBackendState.frozenSpeakers === true;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

export const METER_DECAY_START_MS = 250;
export const METER_DECAY_DB_PER_SEC = 45;
export const DEFAULT_SAMPLE_RATE_HZ = 48000;
export const LATENCY_RAW_WINDOW_MS = 4000;
export const RENDER_TIME_WINDOW_MS = 5000;
export const AUDIO_SAMPLE_RATE_PRESETS = [0, 32000, 44100, 48000, 88200, 96000, 176400, 192000];
export const isLinux = typeof navigator !== 'undefined' && navigator.userAgent.toLowerCase().includes('linux');
