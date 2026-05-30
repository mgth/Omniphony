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
export const sourcePeaks = new Map(); // { value: number, expires: number }
export const speakerPeaks = new Map(); // { value: number, expires: number }
export let masterPeak = { value: 0, expires: 0 };
export const sourceLevelLastSeen = new Map();
export const speakerLevelLastSeen = new Map();
export const sourceGains = new Map();
export const sourceBandGains = new Map();
export const speakerGainCache = new Map();
export const speakerBaseGains = new Map();
export const speakerDelays = new Map();
export const speakerMuted = new Set();
export const objectMuted = new Set();
export const speakerItems = new Map();
export const objectItems = new Map();
export const speakerManualMuted = new Set();
export const objectManualMuted = new Set();
export const sourceNames = new Map();
export const sourceTags = new Map();
export const sourcePositionsRaw = new Map();
/// Per-object 3-D extent (w, d, h) received via `/omniphony/object/{id}/size`.
/// Each entry is `{ w: number, d: number, h: number }` in [0, 1].
export const sourceSizes = new Map();
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
  barycenter: false,
  experimentalDistance: false,
  hybrid: false,
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
  drcUI: false,
  masterGain: false
};

// ---------------------------------------------------------------------------
// Application state (primitive values)
// ---------------------------------------------------------------------------

export const app = {
  producerCapabilities: null,
  producerSession: null,

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
    restoreBackendAvailable: false,
    barycenter: {
      localize: null
    },
    experimentalDistance: {
      distanceFloor: null,
      minActiveSpeakers: null,
      maxActiveSpeakers: null,
      positionErrorFloor: null,
      positionErrorNearestScale: null,
      positionErrorSpanScale: null
    },
    hybrid: {
      externalBackend: null,
      internalBackend: null,
      curve: null,
      curveSmoothing: 0,
      metric: 'chebyshev'
    }
  },
  // Which inner backend's parameter tab is shown while the hybrid backend is active.
  hybridParamTab: null,
  vbapPositionInterpolation: null,
  vbapAllowNegativeZ: null,
  vbapRecomputing: null,
  recomputeError: null,
  saveRequested: false,
  saveError: null,
  vbapCartesianFaceGridEnabled: false,

  // Spread
  spreadState: { min: null, max: null, fromDistance: null, distanceRange: null, distanceCurve: null, sizeToSpreadMode: 'max' },

  // Distance diffuse
  distanceDiffuseState: { enabled: null, threshold: null, curve: null, metric: 'spherical' },
  distanceModel: 'none',
  distanceModelMetric: 'spherical',

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
  adaptiveResamplingHighRecoverEntryMarginMs: 120,
  adaptiveResamplingUpdateIntervalCallbacks: 10,
  adaptiveResamplingLowRecoverSettleStableMs: 200,
  adaptiveResamplingLowRecoverEntryMarginMs: 18,
  adaptiveResamplingLowRecoverExitMarginMs: 6,
  adaptiveResamplingLowRecoverSettleMarginMs: 6,
  adaptiveResamplingLowRecoverRefillDeltaAlpha: 0.5,
  adaptiveResamplingControlSmoothingCutoffHz: 0.5,
  adaptiveResamplingControlSmoothingOrder: 1,
  adaptiveResamplingUsePreBridgeClock: false,
  adaptiveResamplingUseOutputPacing: false,
  adaptiveResamplingDisableBackpressure: false,
  adaptiveResamplingBand: null,
  adaptiveResamplingState: null,

  // Latency & performance
  latencyMs: null,
  latencyInstantMs: null,
  latencyControlMs: null,
  latencySmoothedMs: null,
  latencyDownstreamMs: null,
  latencyTargetMs: null,
  latencyRequestedMs: null,
  // Generic diagnostic-metric registry pushed by the renderer.
  //   diagSchema: { items: [{name, label, group, unit}, ...] } — list of
  //     metrics the renderer exposes (refreshed when new ones register).
  //   diagValues: { name: value, ... } — current values, updated each
  //     meter-bundle tick.
  // The generic diag plot polls these to render any user-selected subset.
  diagSchema: null,
  diagValues: null,
  decodeTimeMs: null,
  decodeTimeWindow: [],
  renderTimeMs: null,
  renderTimeWindow: [],
  crossoverTimeMs: null,
  crossoverTimeWindow: [],
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
  inputModeDirty: false,
  inputActiveMode: 'pipe_bridge',
  inputApplyPending: false,
  inputApplyAwaitingAck: false,
  inputBackend: null,
  inputChannels: null,
  inputSampleRate: null,
  inputNode: null,
  inputDescription: null,
  inputStreamFormat: null,
  inputError: null,
  drcMode: null,
  supportedDrcModes: [],
  drcGain: 1.0,
  drcWeight: 1.0,
  renderBridgePath: null,
  liveInput: {
    backend: 'pipewire',
    node: '',
    description: '',
    layout: '',
    clockMode: 'dac',
    channels: 2,
    sampleRate: 192000,
    format: 'f32',
    map: '7.1-fixed',
    lfeMode: 'object'
  },
  liveInputClockModeDirty: false,

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
  adaptiveHighRecoverEntryMarginEditing: false,
  adaptiveHighRecoverEntryMarginDirty: false,
  adaptiveUpdateIntervalCallbacksEditing: false,
  adaptiveUpdateIntervalCallbacksDirty: false,
  adaptiveFarFadeInMsEditing: false,
  adaptiveFarFadeInMsDirty: false,
  adaptiveLowRecoverSettleStableMsEditing: false,
  adaptiveLowRecoverSettleStableMsDirty: false,
  adaptiveLowRecoverEntryMarginMsEditing: false,
  adaptiveLowRecoverEntryMarginMsDirty: false,
  adaptiveLowRecoverExitMarginMsEditing: false,
  adaptiveLowRecoverExitMarginMsDirty: false,
  adaptiveLowRecoverSettleMarginMsEditing: false,
  adaptiveLowRecoverSettleMarginMsDirty: false,
  adaptiveLowRecoverRefillDeltaAlphaEditing: false,
  adaptiveLowRecoverRefillDeltaAlphaDirty: false,
  adaptiveControlSmoothingAlphaEditing: false,
  adaptiveControlSmoothingAlphaDirty: false,
  telemetryGaugesOpen: false,
  audioOutputSectionOpen: false,
  inputSectionOpen: false,
  rendererSectionOpen: false,
  displaySectionOpen: false,
  drcSectionOpen: false,

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
  // Max XYZ displacement (normalised Omniphony units) between two consecutive
  // trail points before the connecting segment is considered a teleport and
  // skipped. Same value drives the 3D view and the mpv overlay.
  trailTeleportThreshold: 0.5,
  speakerHeatmapSlicesEnabled: true,
  speakerHeatmapVolumeEnabled: false,
  speakerHeatmapBandIndex: 0,
  speakerHeatmapSampleCount: 3072,
  speakerHeatmapMaxSphereSize: 0.062,
  speakerSize: 0.08,
  effectiveRenderEnabled: false,
  objectColorsEnabled: false,
  objectLabelsEnabled: true,
  showObjectDetails: true,
  speakerLabelsEnabled: false,
  objectDisplayMode: 'circle',
  objectSphereSize: 0.07,
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

export function hasProducerDomain(domain) {
  const domains = app.producerCapabilities?.domains;
  return Array.isArray(domains) && domains.includes(domain);
}

export function hasControlConfig(key) {
  const cfg = app.producerCapabilities?.controlConfig;
  return Array.isArray(cfg) && cfg.includes(key);
}

// Renderer flavour from the capability handshake: "standalone" (the CLI/service,
// which owns audio output, the resampler and input) or "embedded" (liborender in
// mpv, where mpv owns those). Defaults to "standalone" before the handshake.
export function producerVariant() {
  return app.producerCapabilities?.variant || 'standalone';
}

// Optional host hint sent alongside the variant ("cli" / "mpv"), or null.
export function producerHost() {
  return app.producerCapabilities?.host || null;
}

export function isEmbeddedProducer() {
  return producerVariant() === 'embedded';
}

export function supportsRealtimeKey(key) {
  const realtime = app.producerCapabilities?.realtime;
  return Array.isArray(realtime) && realtime.includes(key);
}

export function usesNumericSpatialPlaceholders() {
  return (app.producerCapabilities?.producer || 'renderer') === 'renderer';
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
