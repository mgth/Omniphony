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
// Engine-reported master output meter { peakDbfs, rmsDbfs }, or null until the
// first /omniphony/meter/master is received. Live ESM binding read by master.js.
export let masterLevel = null;
export function setMasterLevel(meter) {
  masterLevel = meter;
}
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
// Per-speaker crossover frequency-extent gauges (3D billboard sprites).
export const speakerBandBars = [];

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
  masterGain: false,
  autoGain: false,
  autoGainCeiling: false
};

// ---------------------------------------------------------------------------
// Application state (primitive values)
// ---------------------------------------------------------------------------

export const app = {
  producerCapabilities: null,
  producerSession: null,

  // Room geometry — metres-only model (see computeRoomGeometryFromInputs() in
  // controls/room-geometry.js): Width is the implicit reference that pins the
  // scale (radius_m = Width/2). roomRatio holds the renderer-facing ratios.
  roomRatio: { width: 1, length: 2, height: 1, rear: 1, lower: 0.5, centerBlend: 0.5 },
  roomGeometryExpanded: false,
  roomGeometryBaselineKey: '',
  roomGeometryApplyTimer: null,
  // Room scale (m/u). Restored from the renderer's room domain (roomRatio.scaleM)
  // in applyRoomRatio — no flag needed, the room domain round-trips reliably.
  metersPerUnit: 1.0,

  // VBAP
  vbapCartesianState: { xSize: null, ySize: null, zSize: null, zNegSize: 0 },
  vbapPolarState: { azimuthResolution: null, elevationResolution: null, distanceRes: null, distanceMax: null },
  evaluationModeState: { selection: null, effective: null },
  // Number of object-size intervals precomputed (0 = single table).
  objectSizeIntervals: 0,
  renderBackendState: {
    selection: null,
    effective: null,
    effectiveLabel: null,
    capabilities: null,
    allowedEvaluationModes: [],
    frozenRoomRatio: false,
    frozenSpeakers: false,
    restoreBackendAvailable: false,
    availableBackends: [],
    backendParamValuesById: {},
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

  // Distance diffuse
  distanceDiffuseState: { enabled: null, threshold: null, curve: null, metric: 'spherical' },
  distanceModel: 'none',
  distanceModelMetric: 'spherical',

  // Master
  masterGain: null,
  autoGain: null,
  autoGainCeilingDb: -1.0,

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
  channelRenderMode: 'spatial',
  // Where the 4.x/5.x surround pair (Ls/Rs) of a 2D source is placed: 'side' or
  // 'back'. Only affects sources without dedicated back channels.
  surroundPlacement: 'side',
  // Bed→height object generator (2D upmix) id: 'none' (off), 'copy_up', 'pad'.
  // Synthesizes height objects from channel content on a height-capable layout.
  objectGeneratorId: 'none',
  // Declared bed→height generator schema, published by the renderer on
  // /omniphony/state/object_generators: [{id,label,i18nKey,requiresHeightLayer,
  // params:[{key,label,i18nKey,min,max,step,default,unit}]}]. Studio builds the
  // selector + parameter sliders from this.
  objectGenerators: [],
  // Live param overrides for the active generator (key → value).
  objectGeneratorParams: {},
  // Whether the active output layout has top speakers; when false the 2D-upmix
  // generators are a no-op and the selector is greyed out. Assume yes until the
  // renderer reports otherwise.
  objectGeneratorLayoutHasHeight: true,
  // Phantom-source extraction pre-stage: extracts correlated content from channel
  // pairs as discrete objects at their real panned position, before the height
  // lift. Off by default.
  phantomEnabled: false,
  // Declared phantom-extraction param schema, published on /omniphony/state/phantom
  // as [{key,label,i18nKey,min,max,step,default,unit}]. Studio builds the sliders.
  phantomSchema: [],
  // Live param overrides for the phantom stage (key → value).
  phantomParams: {},
  // How output channels map to device ports: 'by_index' (positionless — port N =
  // layout speaker N) or 'by_name' (positional). Default 'by_index'.
  outputChannelMapping: 'by_index',
  // Speaker names that can't be routed by position in by_name mode (reported by
  // the renderer for the active backend); shown as a warning. Empty when none.
  outputChannelMappingUnroutable: [],
  // Parametrable virtual bed for 2D sources (a SpeakerLayout-shaped object, or
  // null = built-in canonical poses). Edited by the virtual-bed editor.
  virtualBed: null,
  // Declared live options passthrough (registry RFC phase 1): the renderer's
  // `options` snapshot block, keyed by canonical snake_case option key. The
  // camelCase fields above stay the UI's consumers until the data-option
  // binder (phase 2) reads this instead.
  options: {},
  // One-shot guard: once we've materialised the canonical bed into the
  // renderer/config (when none was saved), don't push it again this session.
  virtualBedMaterialized: false,
  audioOutputDevice: null,
  audioOutputDeviceEffective: null,
  audioOutputDevices: [],
  // Output backend selection: 'device' (PipeWire/ASIO) or 'file' (FIFO/stdout
  // capture/stream). Drives the device-vs-file rows in the audio panel.
  audioOutputBackend: 'device',
  audioOutputFile: '-',
  // Remembered named-pipe/file path, so toggling stdout↔pipe restores it.
  audioOutputPipePath: '',
  audioOutputFileFormat: 'raw_f32',
  audioOutputFileEditing: false,
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
  // Config YAML the connected renderer actually loaded (null = built-in
  // defaults). Surfaced in About to diagnose CLI-vs-host config mismatches.
  renderConfigPath: null,
  // Whether that path actually loaded: 'loaded' | 'missing' | 'parse_error' |
  // null (no path → defaults by design). A non-loaded value = the renderer is
  // on defaults despite having a config path.
  renderConfigStatus: null,
  // Build fingerprint of the connected renderer (git-describe + build time).
  // Lets About expose a liborender-vs-orender version skew.
  renderVersion: null,
  // C-ABI version ("major.minor") of the liborender shim hosting the engine.
  // Null when the engine is linked as a Rust crate (the CLI — no C ABI).
  renderAbi: null,
  // Non-empty when the renderer came up degraded (decoder bridge missing) —
  // drives a red banner under the OSC status. Cleared when a healthy renderer
  // reports an empty value.
  renderBridgeError: null,
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
  // Safety timer so a launch that never reaches 'connected' (orender failed to
  // come up) doesn't leave the connection buttons disabled forever.
  oscLaunchPendingTimer: null,
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
  // Last bridge-class input error that auto-opened the Audio Input section;
  // reset on disconnect so the auto-open fires once per connection, not on
  // every state snapshot while the error persists.
  lastAutoOpenedInputError: null,
  rendererSectionOpen: false,
  displaySectionOpen: false,
  drcSectionOpen: false,
  twoDSourcesSectionOpen: false,
  autoGainSectionOpen: false,

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
  // Display-only coord mode for the channel editor (which table the radios
  // highlight). The virtual bed is always stored/sent as polar — see
  // controls/virtual-bed.js — so this never changes what reaches the renderer.
  channelEditCoordMode: 'cartesian',
  // Timestamp (performance.now) of the last spatial:frame. Used to tell an
  // actively-streaming/seeking session from a truly idle one, so the synthetic
  // at-rest bed objects don't double the live objects during playback.
  lastSpatialFrameAt: 0,
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

  // The current gizmo edit target (speaker or virtual-bed channel object),
  // resolved at drag start so update/end commit to the right model.
  dragEditTarget: null,

  // Virtual-bed channel edit (a 2D-source marker edited via the 3D gizmo).
  // While set, updateSource skips repositioning this id so the live OSC stream
  // doesn't fight the gizmo drag; the new position is sent on release.
  isDraggingVirtualBed: false,
  draggingVirtualBedSourceId: null,
  draggingVirtualBedChannel: null,

  // Editor-authoritative pin for a channel object: while its id is pinned,
  // updateSource holds the mesh at `channelEditPinPos` and ignores the live OSC
  // stream — during the drag (channelEditPinUntil = 0, no expiry) and through a
  // short settle window after release (a future timestamp) so in-flight stream
  // packets carrying the pre-edit position can't flash the object back before the
  // renderer applies the new bed.
  channelEditPinId: null,
  channelEditPinPos: null,
  channelEditPinUntil: 0,

  // Trail
  trailsEnabled: true,
  trailRenderMode: 'diffuse',
  trailPointTtlMs: 7000,
  // Max XYZ displacement (normalised Omniphony units) between two consecutive
  // trail points before the connecting segment is considered a teleport and
  // skipped. Same value drives the 3D view and the mpv overlay.
  trailTeleportThreshold: 0.5,
  // Per-speaker local raymarched volume (gain²); drives speaker-solo-volume.js.
  speakerHeatmapVolumeEnabled: false,
  // Own gradient for the speaker heatmap volume (differentiated from the object
  // field). Colour gradient: 'heatmap' | 'blueWhite' | 'whiteRed' | 'red'.
  speakerHeatmapVolumeColormap: 'heatmap',
  // Crossover band selected for effective-render / dominant-speaker readout, and
  // for the per-speaker heatmap volume (single band). `speakerHeatmapAllBands`
  // overrides it for the heatmap with the level-weighted, frequency-coloured
  // "all bands" composite (effective-render still uses the numeric index).
  speakerHeatmapBandIndex: 0,
  speakerHeatmapAllBands: false,
  // Object energy field (client-side theoretical field, ray-marched 3D volume).
  objectEnergyHeatmapEnabled: false,
  // Colour gradient: 'heatmap' | 'blueWhite' | 'whiteRed' | 'red'.
  objectEnergyColormap: 'blueWhite',
  // Independent user-editable gradients backing the 'custom' colormap — one for the
  // object field, one for the speaker heatmap (they do NOT share settings). Stops
  // are { pos, r, g, b } in [0,1], kept sorted by `pos` (2..8 stops).
  // `speakerCustomGradientVersion` bumps on every speaker-gradient edit so the
  // static speaker volume's rebuild guard re-runs (see speaker-solo-volume.js).
  objectCustomGradientStops: [
    { pos: 0.0, r: 0.0, g: 0.0, b: 1.0 },
    { pos: 0.5, r: 0.0, g: 1.0, b: 0.0 },
    { pos: 1.0, r: 1.0, g: 0.0, b: 0.0 },
  ],
  speakerCustomGradientStops: [
    { pos: 0.0, r: 0.0, g: 0.0, b: 1.0 },
    { pos: 0.5, r: 0.0, g: 1.0, b: 0.0 },
    { pos: 1.0, r: 1.0, g: 0.0, b: 0.0 },
  ],
  speakerCustomGradientVersion: 0,
  // Both projections (accumulate front-to-back + peak/MIP) are computed together
  // and blended by `objectEnergyVolumeMix` (0 = pure accumulate, 1 = pure peak).
  // They have different alpha semantics (one sample vs the whole ray), so each
  // keeps its own γ — a single value isn't comparable between them.
  objectEnergyVolumeMix: 0.6,
  objectEnergyVolumeGammaAccumulate: 4,
  objectEnergyVolumeGammaMip: 3,
  objectEnergyHeatmapResolution: 64,
  objectEnergyHeatmapFalloffRadius: 0.5,
  objectEnergyHeatmapOpacity: 1,
  // Min interval (ms) between full energy-volume rebuilds — i.e. how often the
  // n³ RGBA-float 3D texture is re-uploaded to the GPU. Lower = more fluid but
  // more GPU upload traffic (which inflates WebView2 memory on Windows); higher
  // = lighter. Shared by both volume providers (object field + speaker solo).
  // Default 160 ms (~6 Hz) keeps the upload churn bounded with no visible-quality
  // loss (the volume still renders every frame). User-adjustable in Display.
  volumeRefreshMs: 160,
  // Volume sampling: false = crisp cells (NearestFilter), true = trilinear
  // gradient between each cell's 8 corner texels (LinearFilter). Shared by both
  // volumes. Needs the OES_texture_float_linear WebGL2 extension.
  volumeSmoothInterpolation: false,
  // Drives the mpv overlay's depth-plane count (not the Studio 3D view).
  objectEnergyHeatmapBandCount: 12,
  lastObjectEnergyHeatmapAt: 0,
  // The mix/γ/resolution/opacity above are shared between the object field and the
  // per-speaker heatmap volume (which renders the selected speaker's gain field
  // from the local table as gain² — see speaker-solo-volume.js).
  lastSpeakerSoloVolumeAt: 0,
  speakerSize: 0.08,
  effectiveRenderEnabled: false,
  // Display-only master switch: when false, objects + their labels + trails are
  // hidden in the 3D view and on the mpv overlay, without touching
  // objectLabelsEnabled / trailsEnabled (they return as set when shown again).
  objectsVisible: true,
  objectColorsEnabled: false,
  objectLabelsEnabled: true,
  showObjectDetails: true,
  speakerLabelsEnabled: false,
  speakerBandBarsEnabled: false,
  speakerFaceListenerEnabled: false,
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
