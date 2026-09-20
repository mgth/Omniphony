# Viewport parity spec: object trails + ray-marched energy volumes

Source tree: `omniphony-studio/src/` (all JS paths below are relative to it unless prefixed). Rust paths are relative to ``. three.js version is `0.165.0` (`package.json:26`), `ColorManagement.enabled = true` (three default), `renderer.outputColorSpace` left at its default (`SRGBColorSpace`); no `setPixelRatio` call anywhere, so 1 canvas pixel = 1 CSS pixel.

Files covered: `trails.js`, `scene/energy-volume-core.js`, `scene/global-energy-volume.js`, `scene/object-energy-volume.js`, `scene/object-energy-shared.js`, `scene/gradient-editor.js`, `controls/scene-effects-bar.js`, `listeners/trails-and-display-listeners.js` (there is no `controls/trails-and-display-listeners.js`). Supporting reads: `sources.js`, `coordinates.js`, `state.js`, `app.js`, `controls/room-geometry.js`, `scene/speaker-gaintable.js`, `scene/speaker-solo-volume.js`, `scene/discontinuity-volume.js`, `mpvOverlay.js`, `index.html`, `i18n/en.json`, `src-tauri/src/osc_parser.rs`, `src-tauri/src/osc_listener.rs`, `src-tauri/src/commands/diag.rs`, `src-tauri/src/commands/mpv_overlay.rs`, `omniphony-renderer/renderer/src/band_gaintable.rs`, `omniphony-renderer/renderer/src/live_params.rs`, `omniphony-renderer/runtime_control/src/osc.rs`, `omniphony-renderer/omniphony-geometry/src/lib.rs`.

---

## 0. Shared context

### 0.1 Coordinate frames (`coordinates.js`)

| Name | Axes | Notes |
|---|---|---|
| Omniphony normalised ("raw") | x = width (left/right), y = depth (rear −1 … front +1), z = height (−1 … +1) | Each in [−1, 1]. This is what OSC carries and what the gain tables are gridded on. |
| Scene (three.js) | x = depth, y = height (up), z = width | `omniphonyToSceneCartesian(p) = {x: p.y, y: p.z, z: p.x}` (`coordinates.js:33-39`). |

Room-ratio warp (`coordinates.js:106-133`), `app.roomRatio` default `{ width: 1, length: 2, height: 1, rear: 1, lower: 0.5, centerBlend: 0.5 }` (`state.js:107`):

```
depthWarpWithRatios(rawDepth, f=length, r=rear, blend=centerBlend):
  d = clamp(rawDepth, -1, 1); center = r + (f - r) * clamp(blend, 0, 1)
  d >= 0: t = d;  a = center - f; b = 2(f - center); return  a t^3 + b t^2 + center t
  d <  0: t = -d; a = center - r; b = 2(r - center); return -(a t^3 + b t^2 + center t)
mapRoomDepth(rawX)            = depthWarpWithRatios(rawX, length, rear, centerBlend)        // mapRoomDepth(1)=length, mapRoomDepth(-1)=-rear
mapRoomPosition(rawScene)     = { x: mapRoomDepth(rawScene.x),
                                  y: rawScene.y >= 0 ? rawScene.y*height : rawScene.y*lower,
                                  z: rawScene.z*width }
normalizedOmniphonyToScenePosition(omni) = mapRoomPosition(omniphonyToSceneCartesian(omni))   // coordinates.js:180-183
inverseMapRoomDepth(sx)       = 28-iteration bisection of depthWarpWithRatios on [0,1] (sx>=0, target clamped to [0,front]) or [-1,0] (sx<0, target clamped to [-rear,0])   // coordinates.js:147-176
```

`hydrateObjectCoordinateState(p)` (`coordinates.js:274-305`): if cartesian mode, clamps x/y/z to [−1,1] and derives az/el/dist from the scene position; if polar mode, derives x/y/z from az/el/dist via `sphericalToCartesianDeg` + `scenePositionToNormalizedOmniphony`. Idempotent; the trail code calls it a second time on a copy.

Camera (`scene/setup.js:20-25`): `PerspectiveCamera(65°, aspect, near 0.1, far 100)`, initial position `(-3.8, 1.1, 0)`, looking at `HEAD_PIVOT = (0, 0.25, 0)`. Scene background `0x0a0b10`.

### 0.2 three.js blend/depth conventions used here (three r165 `WebGLState.js:599-635`)

| three material flags | GL state | wgpu equivalent |
|---|---|---|
| `NormalBlending`, `premultipliedAlpha:false` | `blendFuncSeparate(SRC_ALPHA, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA)`, `FUNC_ADD` | color: `SrcAlpha`/`OneMinusSrcAlpha` Add; alpha: `One`/`OneMinusSrcAlpha` Add |
| `NormalBlending`, `premultipliedAlpha:true` | `blendFuncSeparate(ONE, ONE_MINUS_SRC_ALPHA, ONE, ONE_MINUS_SRC_ALPHA)`, `FUNC_ADD` | color: `One`/`OneMinusSrcAlpha` Add; alpha: `One`/`OneMinusSrcAlpha` Add (`BlendState::PREMULTIPLIED_ALPHA_BLENDING`) |
| `depthTest:false` | `gl.disable(DEPTH_TEST)` | `DepthStencilState.depth_compare = Always` (or no depth attachment) |
| `depthWrite:false` | `gl.depthMask(false)` | `depth_write_enabled = false` |
| `side: BackSide` | `cullFace(FRONT)` | `cull_mode = Some(Face::Front)` |

three.js draws opaque objects first, then transparent objects sorted by `renderOrder` ascending, then by view-space depth far→near (default `sortObjects = true`). Every element in this spec is `transparent: true`, so their relative order is given by `renderOrder`:

| renderOrder | Element | File |
|---|---|---|
| 1 | room faces | `scene/setup.js:328-352` |
| 3 | gizmo shadows, hybrid-distance mesh | `scene/gizmos.js`, `scene/hybrid-distance.js` |
| 5 | screen mesh, gizmo labels/arrows | `scene/setup.js:386`, `scene/gizmos.js` |
| 6 | grid | `scene/gizmos.js:28` |
| 11 / 12 | effective-render line / marker | `sources.js:342/329` |
| 14 | source diffuse halo | `sources.js:384` |
| **15** | **trails (both modes)** | `trails.js:55,72` |
| 20 | source outline | `sources.js:315` |
| **22** | **energy volumes (all four providers)** | `scene/energy-volume-core.js:121,165` |
| 30–32 | axes, room-geometry lines/labels | `scene/axes.js`, `controls/room-geometry.js` |
| 39 / 40 | speaker band-bar sprites / label sprites | `scene/speaker-band-bars.js:151`, `scene/labels.js:187` |

Both trails and volumes have `depthTest:false` and `depthWrite:false`, and `frustumCulled = false`.

### 0.3 Colour-space caveat (important for parity)

`new THREE.Color(hex)` converts the sRGB hex to **linear-sRGB** floats (ColorManagement enabled). Built-in materials (the `LineBasicMaterial` used by the line trail) run `#include <colorspace_fragment>` (`meshbasic.glsl.js:110`), i.e. linear → sRGB OETF on output. Custom `ShaderMaterial`/`RawShaderMaterial` shaders (the diffuse trail points, the volume) do **not** include that chunk, so whatever they write is stored raw into the sRGB canvas. Consequences:
- Diffuse trail: vertex colours are linear-space floats written raw → on screen the trail is darker/more saturated than the palette hex (e.g. `#ff7c4d` → linear `(1.0, 0.198, 0.074)` displayed as if sRGB).
- Line trail: same linear floats but OETF-encoded on output → displayed at the palette hue/lightness (times the 0.2–1.0 ramp and 0.6 opacity).
- Volume: colormap values (pure 0/1 primaries mostly) written raw; treat the shader output as already-sRGB-encoded. Native port: render these three into an sRGB-format swapchain **without** applying an OETF for the custom shaders, and **with** one for the line trail, to match.

---

## 1. Object trails (`trails.js`, append path in `sources.js`)

### 1.1 What it looks like

One renderable per object (`sourceTrails: Map<id, {positions, line, lastPointAt, lastRebuildAt}>`, `state.js:46`, created in `sources.js:997-1024`). Two mutually exclusive modes selected by `app.trailRenderMode` (`'diffuse'` default, or `'line'`, `state.js:463`); switching mode disposes and recreates every renderable (`listeners/trails-and-display-listeners.js:253-275`, also `trails.js:270-290`).

**Diffuse mode** (`trails.js:18-58`): a `THREE.Points` cloud of soft round sprites, dense along the path (segments are subdivided), growing in size, brightness and alpha from the oldest point (small, dim, ~transparent) to the newest (large, full colour, alpha 0.25), scaled overall by the object's current loudness. Normal alpha blending, no depth test/write, `renderOrder 15`.

**Line mode** (`trails.js:60-75`): `THREE.LineSegments`, 1 px (WebGL line width is fixed), per-vertex colour ramped from 20 % (oldest) to 100 % (newest) of the object trail colour, `LineBasicMaterial{ vertexColors:true, transparent:true, opacity:0.6, depthTest:false, depthWrite:false }`, `renderOrder 15`. Segments spanning a "teleport" are omitted so the polyline visibly breaks.

### 1.2 Data model and sources

Each trail point is a record pushed in `sources.js:1103-1112` (in `updateSource`, i.e. on every `source:update` Tauri event that passes the append throttle):

| Field | Type | Origin |
|---|---|---|
| `x, y, z` | f64 in [−1,1] (Omniphony normalised, clamped by `hydrateObjectCoordinateState`) | `source:update` payload `position.{x,y,z}` ← `OscEvent::Update{ position: Position{..} }` (`osc_parser.rs:120-140`, parsed at `osc_parser.rs:452-560` from `/omniphony/object/<id>/xyz` or `/aed`, args `[p0, p1, p2, speaker_idx, gain_db, priority, ramp, gen, name]`) |
| `coordMode, azimuthDeg, elevationDeg, distanceM` | derived | same payload / hydration |
| `metadataGainDb` | i32 or undefined | `position.gainDb` (`Position.gain_db`, arg index 4) |
| `directSpeakerIndex` | u32 or null | `position.directSpeakerIndex` (arg index 3, ≥0) |
| `fixed, label, kind` | contract fields | `position.{fixed,label,kind}` |
| `t` | ms (`performance.now()`) | append time; drives TTL decay |
| `trailRmsDbfs` | f64 or undefined | `sourceLevels.get(id).rmsDbfs` at append time ← `source:meter` event ← `OscEvent::MeterObject{ rms_dbfs }` (`osc_parser.rs:193-203`, address `/omniphony/meter/object/<id>`, args `[peak, rms, band0, band1, …]`, `osc_parser.rs:1082-1101`; emitted at `osc_listener.rs:2255-2283` as `{id, meter:{peakDbfs, rmsDbfs, bandRmsDbfs, peakHoldDbfs}}`) |
| `trailColor` | `[r,g,b]` linear floats | `captureTrailPointColor(mesh)` (`trails.js:109-119`) = `mesh.userData.objectTrailColor` |

`source:update` / `source:meter` are coalesced by the Rust side into `state:batch` at ~60 Hz (`osc_listener.rs:1751-1765`, PR #88), so the JS sees at most one update per object per ~16 ms.

`position._noTrail === true` skips the append (only set by `controls/virtual-bed.js:467`, the virtual-bed channel editor).

### 1.3 Sampling cadence, ring buffer, TTL (`trails.js:10-14, 294-366`)

| Constant | Value | Meaning |
|---|---|---|
| `TRAIL_MIN_POINT_INTERVAL_MS` | 70 | `shouldAppendTrailPoint`: a new point is appended only if ≥ 70 ms since `trail.lastPointAt` (first point always appended) |
| `TRAIL_MAX_POINTS_PER_OBJECT` | 240 | `recordTrailPoint`: after append, drop the oldest so `positions.length ≤ 240` (`splice(0, overflow)`) |
| `TRAIL_MIN_REBUILD_INTERVAL_MS` | 70 | `shouldRebuildTrailGeometry`: geometry is rebuilt at most every 70 ms per trail (and only if `app.trailsEnabled`) |
| `app.trailPointTtlMs` | 7000 default; `setTrailPointTtlMs` clamps ≥ 500; slider 1.0–20.0 s step 0.5 | Point lifetime |
| decay pass | every 120 ms (`app.lastTrailDecayAt`) from the animation loop (`app.js:344` → `decayTrails`) | Filters `positions` to `p.t >= now − trailPointTtlMs`; points without numeric `t` are dropped; rebuilds geometry if anything was removed; toggles CSS class `has-active-trail` on the object list row (UI) |
| `TRAIL_SILENT_RMS_DBFS` | −100 | diffuse mode only: points with `trailRmsDbfs ≤ −100` are skipped at rebuild |
| `TRAIL_SILENT_GAIN_DB` | −128 | diffuse mode only: points with `metadataGainDb ≤ −128` are skipped at rebuild |

Note: `sources.js:1189-1210` contains an identical duplicate `decayTrails`; the one actually wired in `app.js:38,344` is `trails.js`'s. On `state:reset`-type events `tauri-bridge.js:251-255` clears every trail's `positions` and geometry.

Positions are a plain JS array (not a true ring buffer): push + splice-from-front. A native port can use a fixed 240-slot ring per object.

### 1.4 Position mapping (`trails.js:85-95`)

```
mapTrailRawToScene(raw):
  if raw.directSpeakerIndex is a valid index into speakerMeshes → return that speaker mesh's current scene position (the trail point snaps to the speaker)
  else → hydrateObjectCoordinateState(copy of raw) → normalizedOmniphonyToScenePosition → Vector3
```
Mapping happens at rebuild time (so room-ratio changes re-map old points).

### 1.5 Colour (`trails.js:97-119`, `sources.js:216-247`)

- Per-point colour = `raw.trailColor` clamped to [0,1] per channel, else the fallback = `mesh.userData.objectTrailColor` (else `mesh.material.color`, else `0xcc6640`; and `0xff7c4d` if the mesh has no material colour).
- `objectTrailColor = getObjectTrailColor(id) = getObjectBaseColor(id).offsetHSL(0, +0.04, +0.08)` (`sources.js:245-247`; HSL offset is applied in three's working space = linear-sRGB, s and l clamped to [0,1]).
- `getObjectBaseColor(id)` (`sources.js:224-243`): source tag `A` → `#ff8b6b`; tag `B` → `#62d7c7`; otherwise palette `OBJECT_COLOR_PALETTE[idx]` with `idx = |numericId| % 16` for numeric ids, else FNV-1a 32-bit hash of the id string `% 16`. Palette (`sources.js:176-193`): `#ff6b6b #4ecdc4 #ffe66d #5dade2 #af7ac5 #f5b041 #58d68d #ec7063 #48c9b0 #f4d03f #5499c7 #a569bd #eb984e #45b39d #7fb3d5 #f1948a`. The `objectColorsEnabled` toggle does not change the trail colour (it only affects the object mesh/outline); the listener rebuilds trails on that toggle anyway (`listeners/trails-and-display-listeners.js:126-137`).
- Colour is captured at append time; a later tag change updates `objectTrailColor` (`sources.js:1180-1185`) for subsequent points only.

### 1.6 Teleport detection (`trails.js:138-146`)

`isTrailTeleport(prev, curr)`: true if `app.trailTeleportThreshold > 0` and `(dx²+dy²+dz²) > thr²` on the **raw normalised** x/y/z. Default `0.5` (`state.js:468`), slider 0.05–2.0 step 0.05 (`index.html:348`), clamped to [0.05, 2.0]. Evaluated at rebuild time on consecutive *kept* points (in diffuse mode, on the audible-filtered list).

### 1.7 Diffuse mode geometry (`trails.js:179-241`)

Inputs: `mappedPositions[]` (scene), `pointColors[]`, `sourceScale = max(0, mesh.userData.levelScale)` where `levelScale = dbfsToScale(rmsDbfs, 0.5, 2.4) = 0.5 + ((clamp(rms,−100,0)+100)/100)·1.9` (`sources.js:568-580`), i.e. in [0.5, 2.4] from the object's *current* meter.

```
if count < 2 → empty geometry
loudnessFactor = sourceScale ^ 1.8                                   // [0.287 … 4.83]
expanded = []
for i in 0..count:
  expanded.push({ pos: P[i], col: C[i], t: i/(count-1) })
  if i == count-1 or isTrailTeleport(raw[i], raw[i+1]) → continue    // gap across a teleport
  dist = |P[i+1] - P[i]| (scene units)
  sub = clamp(ceil(dist / 0.06), 2, 10)
  for step in 1..sub:  f = step/sub
    expanded.push({ pos: lerp(P[i],P[i+1],f), col: lerp(C[i],C[i+1],f), t: (i+f)/(count-1) })
for each expanded point:
  glow  = 0.18 + 0.82 t
  color = col * glow
  size  = (6 + 20 t) * loudnessFactor                                 // pixels @ reference distance, see shader
  alpha = 0.05 + 0.2 t²
```
Attributes: `position` vec3, `color` vec3, `size` f32, `alpha` f32. Up to 240 raw points → ≤ ~2400 sprites per object. Geometry is disposed and recreated on every rebuild (native: reuse a buffer, orphan on resize).

Material (`trails.js:19-52`): `ShaderMaterial{ transparent:true, depthTest:false, depthWrite:false, blending:NormalBlending }` (non-premultiplied). Object matrix is identity (positions are scene-space), so `modelViewMatrix = viewMatrix`.

GLSL (verbatim):

```glsl
// vertex
attribute vec3 color; attribute float size; attribute float alpha;
varying vec3 vColor; varying float vAlpha;
void main() {
  vColor = color; vAlpha = alpha;
  vec4 mvPosition = modelViewMatrix * vec4(position, 1.0);
  gl_PointSize = clamp(size * (110.0 / max(0.1, -mvPosition.z)), 0.4, 44.0);
  gl_Position = projectionMatrix * mvPosition;
}
// fragment
varying vec3 vColor; varying float vAlpha;
void main() {
  vec2 centered = (gl_PointCoord - vec2(0.5)) * 2.0;
  float radius = length(centered);
  float alphaMask = 1.0 - smoothstep(0.25, 1.0, radius);
  float alpha = alphaMask * vAlpha;
  if (alpha <= 0.001) discard;
  gl_FragColor = vec4(vColor, alpha);
}
```
`gl_PointSize` is in canvas pixels (no DPR scaling in this app). The sprite is a screen-aligned square of that side; the radial mask makes `gl_PointCoord` orientation irrelevant.

WGSL translation (wgpu has no point sprites: draw 4-vertex triangle strips, one instance per point, expanding in clip space by the same pixel size):

```wgsl
struct Camera {
  view: mat4x4<f32>,          // = three modelViewMatrix (model is identity)
  proj: mat4x4<f32>,
  viewport_px: vec2<f32>,     // canvas size in pixels (DPR 1 in the JS app)
  _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> cam: Camera;

struct PointIn {              // per-instance attributes, step mode Instance
  @location(0) position: vec3<f32>,
  @location(1) color: vec3<f32>,
  @location(2) size: f32,
  @location(3) alpha: f32,
};
struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) vColor: vec3<f32>,
  @location(1) vAlpha: f32,
  @location(2) pointCoord: vec2<f32>,   // stands in for gl_PointCoord in [0,1]^2
};

@vertex
fn vs_main(p: PointIn, @builtin(vertex_index) vi: u32) -> VsOut {
  let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));      // strip: (0,0),(1,0),(0,1),(1,1)
  let mvPosition = cam.view * vec4<f32>(p.position, 1.0);
  let pointSize = clamp(p.size * (110.0 / max(0.1, -mvPosition.z)), 0.4, 44.0);  // pixels
  var clip = cam.proj * mvPosition;
  let offsetNdc = (corner - vec2<f32>(0.5)) * pointSize * 2.0 / cam.viewport_px;
  clip = vec4<f32>(clip.xy + offsetNdc * clip.w, clip.zw);
  var o: VsOut;
  o.clip = clip; o.vColor = p.color; o.vAlpha = p.alpha; o.pointCoord = corner;
  return o;
}

@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
  let centered = (i.pointCoord - vec2<f32>(0.5)) * 2.0;
  let radius = length(centered);
  let alphaMask = 1.0 - smoothstep(0.25, 1.0, radius);
  let alpha = alphaMask * i.vAlpha;
  if (alpha <= 0.001) { discard; }
  return vec4<f32>(i.vColor, alpha);       // written raw, no OETF (see §0.3)
}
```
Pipeline: `TriangleStrip`, 4 vertices × N instances, `cull_mode: None`, blend color `SrcAlpha/OneMinusSrcAlpha`, alpha `One/OneMinusSrcAlpha`, `depth_compare: Always`, `depth_write_enabled: false`. Draw after everything with renderOrder < 15 (objects' halos) and before outlines/volumes.

### 1.8 Line mode geometry (`trails.js:148-177`)

```
segCount = count - 1; for i in 0..segCount:
  if isTrailTeleport(raw[i], raw[i+1]) → skip segment
  t1 = i/(count-1); t2 = (i+1)/(count-1)
  push P[i], P[i+1]; push C[i]*(0.2+0.8 t1), C[i+1]*(0.2+0.8 t2)
```
Attributes `position` vec3, `color` vec3 (2 vertices per kept segment; `LineSegments`, so a gap costs nothing). No audibility filter in line mode (silent points are kept).

Material: `LineBasicMaterial{ vertexColors:true, transparent:true, opacity:0.6, depthTest:false, depthWrite:false }` → three's `meshbasic` program: `diffuseColor = vec4(vec3(1)*vColor, 0.6)`, then `colorspace_fragment` (linear→sRGB). WGSL equivalent:

```wgsl
struct VIn { @location(0) position: vec3<f32>, @location(1) color: vec3<f32> };
struct VOut { @builtin(position) clip: vec4<f32>, @location(0) vColor: vec3<f32> };
@vertex fn vs_main(v: VIn) -> VOut {
  var o: VOut; o.clip = cam.proj * cam.view * vec4<f32>(v.position, 1.0); o.vColor = v.color; return o;
}
fn linearToSrgb(c: vec3<f32>) -> vec3<f32> {            // three.js LinearTransferOETF
  let lo = c * 12.92;
  let hi = pow(c, vec3<f32>(1.0 / 2.4)) * 1.055 - 0.055;
  return select(hi, lo, c <= vec3<f32>(0.0031308));
}
@fragment fn fs_main(i: VOut) -> @location(0) vec4<f32> {
  return vec4<f32>(linearToSrgb(i.vColor), 0.6);
}
```
Pipeline: `LineList`, 1 px, same blend/depth as §1.7.

### 1.9 Visibility rules

`trail.line.visible = app.trailsEnabled && !isSourceMetadataSilent(id)` where metadata-silent = `metadataGainDb ≤ −128` (`sources.js:485-513, 266-270`); forced off for every object while `app.objectsVisible === false` (`sources.js:538-548`, enforced each frame). A trail with < 2 points has empty geometry. Trails are not affected by selection/hover/mute/solo/kind (the metering level only enters through `loudnessFactor` in diffuse mode).

### 1.10 Interaction

None: trails are not picking targets and have no hover/drag behaviour.

### 1.11 Defaults, toggles, persistence

| Control (id, `index.html`) | i18n key | State field | Default / range | Effect |
|---|---|---|---|---|
| `trailToggle` checkbox (l.331) | `trail.title` / `trail.show` | `app.trailsEnabled` | true | sets visibility on every trail, rebuilds when enabling |
| `trailModeSelect` (l.337) options `diffuse`/`line` | `trail.mode`, `trail.mode.diffuse`, `trail.mode.line` | `app.trailRenderMode` | `'diffuse'` | recreates every renderable |
| `trailTtlSlider` (l.344) | `trail.duration` | `app.trailPointTtlMs` | 7.0 s; 1.0–20.0 step 0.5; clamp ≥ 500 ms | TTL |
| `trailTeleportSlider` (l.348) | `trail.teleport` | `app.trailTeleportThreshold` | 0.50; 0.05–2.0 step 0.05 | rebuilds all trails |
| Scene-FX bar `fxTrailsBtn` (+ flyout `fxTrailsMenu` items `diffuse`/`line`) | `sceneFx.trails` | — | mirrors `trailToggle` / `trailModeSelect` by dispatching `change` | see §7 |

Persistence: `localStorage['spatialviz.trail_prefs'] = {enabled, mode, duration_ms, teleport_threshold}` (`controls/room-geometry.js:25, 84-96`, loaded at `301-323`). Every change is also pushed to the renderer via Tauri `mpv_overlay_set_trail_prefs` → OSC `/omniphony/control/overlay/trails [i enabled, i ttl_ms, s mode, f teleport]` (`commands/mpv_overlay.rs:11-32`). Conversely the renderer republishes its persisted overlay prefs as `OscEvent::StateOverlay{json}` (`osc_parser.rs:764`, address `/omniphony/state/overlay`) → Tauri `overlay:state`, and `mpvOverlay.js:73-85` adopts `trailsEnabled / trailTtlMs / trailMode / trailTeleportThreshold` into `app.*` and re-syncs the switches (`listeners/trails-and-display-listeners.js:358-371`). So the renderer's copy wins whenever it arrives.

---

## 2. Ray-marched energy volume core (`scene/energy-volume-core.js`, `scene/object-energy-shared.js`)

### 2.1 Overview

`class EnergyVolume`: one `THREE.Group` (`renderOrder 22`, hidden by default) holding one unit `BoxGeometry(1,1,1)` mesh with a `RawShaderMaterial` (GLSL3) that ray-marches a `Data3DTexture`. Four independent instances exist, one per provider; each is refreshed from the animation loop every frame (`app.js:346-349`) but only *rebuilds* (re-samples the n³ field + re-uploads the texture) under the throttle in §2.9:

| Provider | File | Field | Colour path | Level normalisation |
|---|---|---|---|---|
| Object energy field | `scene/object-energy-volume.js` | client-side inverse-square from live object levels | scalar + `uColormap` (`app.objectEnergyColormap`) | self (1/max) |
| Global energy deviation | `scene/global-energy-volume.js` | renderer gain table target −1 (`√Σg²` per cell per band) | precoloured red/blue | absolute (`maxLevel: 1`) |
| Speaker heatmap volume | `scene/speaker-solo-volume.js` (not assigned; summarised) | gain table for `selectedSpeakerIndex`: `g²` (single band) or level-weighted band-colour mix (all bands) | scalar+`uColormap` (`app.speakerHeatmapVolumeColormap`) or precoloured | self |
| Discontinuity | `scene/discontinuity-volume.js` (not assigned; summarised) | gain table targets −2/−3 | precoloured amber `(1, 0.65−0.45t, 0.05)`, α=t | absolute |

### 2.2 Geometry, placement, material state (`energy-volume-core.js:117-167, 312-320`)

- Mesh: `BoxGeometry(1,1,1)` (three: 24 vertices / 36 indices, centred at origin). `mesh.position = box centre`, `mesh.scale = box size`, so `modelMatrix = T(centre)·S(size)`.
- Box (scene space) = the depth-warped room: `xMin = mapRoomDepth(-1) = -rear`, `xMax = mapRoomDepth(1) = length`, `yMin = -lower`, `yMax = height`, `zMin = -width`, `zMax = width` (each ratio clamped ≥ 1e−3; defaults → x∈[−1,2], y∈[−0.5,1], z∈[−1,1]).
- `RawShaderMaterial{ glslVersion: GLSL3, transparent:true, premultipliedAlpha:true, depthWrite:false, depthTest:false, side: BackSide, toneMapped:false }` → blend `One/OneMinusSrcAlpha` (both), depth off, **front faces culled** (the back faces are rasterised so the volume still draws when the camera is inside the box; the shader recomputes the ray/box intersection from the camera anyway).
- `frustumCulled = false`, `renderOrder 22` (mesh and group).

Uniforms:

| Uniform | Type | Set to |
|---|---|---|
| `modelMatrix, viewMatrix, projectionMatrix, cameraPosition` | mat4/vec3 | three built-ins (world-space camera position) |
| `uVolume` | sampler3D | the n³ RGBA float texture |
| `uBoxMin`, `uBoxMax` | vec3 | box above |
| `uInvMax` | float | `1/effectiveMax` where `effectiveMax = maxLevel` if given (>0) else the field's own max; 0 if max ≤ 0 |
| `uOpacity` | float | `clamp(opacity, 0.05, 1.0)` (fallback 0.55 if NaN) |
| `uGammaAccumulate` | float | provider passes `clampVolumeGamma('accumulate', app.objectEnergyVolumeGammaAccumulate)` → [1.0, 10.0] |
| `uGammaMip` | float | `clampVolumeGamma('mip', app.objectEnergyVolumeGammaMip)` → [0.2, 3.0] |
| `uStepNorm` | float | `REF_STEPS / uSteps` with `REF_STEPS = 64` |
| `uMix` | float | `clamp(mix, 0, 1)` (0 = pure front-to-back accumulate, 1 = pure peak/MIP) |
| `uColormap` | int | `colormap | 0` (index into `OBJECT_ENERGY_COLORMAPS`; providers that use `sampleColor` leave it 0 and set `uPrecolored = 1`) |
| `uSteps` | int | `clamp(round(n*2), 32, 384)` |
| `uPrecolored` | int | 1 if the provider supplied `sampleColor`, else 0 |
| `uCustomStops[8]` | vec4 (pos, r, g, b) | from `customStops` (≤ `MAX_CUSTOM_STOPS = 8`) |
| `uCustomStopCount` | int | number uploaded, 0 if none |

### 2.3 Texture (`energy-volume-core.js:169-189, 220-227, 256-289`)

- `Data3DTexture(Float32Array(n·n·n·4), n, n, n)`, `RGBAFormat`, `FloatType` (WebGL2 `RGBA32F`), `unpackAlignment 1`, wrap = three default `ClampToEdgeWrapping`, `min/magFilter = NearestFilter` (crisp cells) or `LinearFilter` when `app.volumeSmoothInterpolation` **and** the `OES_texture_float_linear` extension exists (else silently stays nearest). wgpu: `Rgba32Float` is filterable only with `Features::FLOAT32_FILTERABLE`; otherwise use nearest, or store `Rgba16Float`.
- `n = clamp(round(resolution), 8, 64)` (default 64; slider 8–64 step 2). Texture is recreated only when `n` changes.
- Memory layout: `idx = i + n*(j + n*k)`, i = depth slice (texture X = scene X), j = height (texture Y = scene Y), k = width (texture Z = scene Z); 4 floats per texel; i is the fastest index → `write_texture` with `bytes_per_row = n*16`, `rows_per_image = n`. Whole texture re-uploaded on every rebuild (≈ 4 MB at n=64).
- Sampling coordinate in the shader: `uvw = (p − uBoxMin)/(uBoxMax − uBoxMin)` — uniform in scene space; only the depth axis is non-linear in Omniphony space, handled when filling.
- Scalar mode: `.r = energy`, `.gba = 0`. Precoloured mode: `.rgb = colour`, `.a = level`.

Per-slice Omniphony coordinates (cell centres):
```
depthByI[i]  = inverseMapRoomDepth(xMin + ((i+0.5)/n)(xMax−xMin))          // Omniphony y (depth)
heightByJ[j] = sy>=0 ? sy/height : sy/lower,  sy = yMin + ((j+0.5)/n)(yMax−yMin)   // Omniphony z
widthByK[k]  = sz/width,                      sz = zMin + ((k+0.5)/n)(zMax−zMin)   // Omniphony x
fill: for k { ow=widthByK[k]; for j { oh=heightByJ[j]; for i { od=depthByI[i];
        scalar: data[o]=sampleEnergy(ow,od,oh)   |  precoloured: sampleColor(ow,od,oh,out[4]) } } }
track maxEnergy (of .r or .a)
```
Provider callbacks receive `(ow = width, od = depth, oh = height)` in Omniphony normalised units.

### 2.4 Volume shaders (verbatim GLSL, `energy-volume-core.js:35-115` + `HEATMAP_GLSL` from `object-energy-shared.js:116-153`, injected where `${HEATMAP_GLSL}` appears)

```glsl
// ---- vertex (GLSL ES 3.00, RawShaderMaterial) ----
precision highp float;
in vec3 position;
uniform mat4 modelMatrix; uniform mat4 viewMatrix; uniform mat4 projectionMatrix;
out vec3 vWorldPos;
void main() {
  vec4 world = modelMatrix * vec4(position, 1.0);
  vWorldPos = world.xyz;
  gl_Position = projectionMatrix * viewMatrix * world;
}

// ---- fragment ----
precision highp float;
precision highp sampler3D;
in vec3 vWorldPos;
uniform vec3 cameraPosition;
uniform sampler3D uVolume;
uniform vec3 uBoxMin; uniform vec3 uBoxMax;
uniform float uInvMax; uniform float uOpacity;
uniform float uGammaAccumulate; uniform float uGammaMip;
uniform float uStepNorm; uniform float uMix;
uniform int uColormap; uniform int uSteps;
uniform int uPrecolored;              // 0 = scalar in .r + colormap; 1 = .rgb colour, .a level
uniform vec4 uCustomStops[8];         // (pos, r, g, b)
uniform int uCustomStopCount;
out vec4 outColor;

vec3 customStopsColor(float t) {
  int n = uCustomStopCount;
  if (n <= 0) { return vec3(t); }
  if (t <= uCustomStops[0].x) { return uCustomStops[0].yzw; }
  for (int i = 0; i + 1 < 8; i++) {
    if (i + 1 >= n) { break; }
    vec4 a = uCustomStops[i]; vec4 b = uCustomStops[i + 1];
    if (t <= b.x) { float f = b.x > a.x ? (t - a.x) / (b.x - a.x) : 0.0; return mix(a.yzw, b.yzw, f); }
  }
  return uCustomStops[n - 1].yzw;
}
vec3 heatmapColor(float value) {
  float t = clamp(value, 0.0, 1.0);
  if (uColormap == 4) { return customStopsColor(t); }
  if (uColormap == 3) { return vec3(1.0, 0.0, 0.0); }
  if (uColormap == 2) { return vec3(1.0, 1.0 - t, 1.0 - t); }
  if (uColormap == 1) { return vec3(t, t, 1.0); }
  vec3 c;
  if (t < 0.25)      { c = mix(vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), (t - 0.00) / 0.25); }
  else if (t < 0.48) { c = mix(vec3(0.0, 1.0, 1.0), vec3(0.0, 1.0, 0.0), (t - 0.25) / 0.23); }
  else if (t < 0.70) { c = mix(vec3(0.0, 1.0, 0.0), vec3(1.0, 1.0, 0.0), (t - 0.48) / 0.22); }
  else               { c = mix(vec3(1.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), (t - 0.70) / 0.30); }
  return c;
}

void main() {
  vec3 ro = cameraPosition;
  vec3 rd = normalize(vWorldPos - ro);
  vec3 invd = 1.0 / rd;
  vec3 ta = (uBoxMin - ro) * invd;
  vec3 tb = (uBoxMax - ro) * invd;
  vec3 tmin = min(ta, tb);
  vec3 tmax = max(ta, tb);
  float tNear = max(max(tmin.x, tmin.y), tmin.z);
  float tFar  = min(min(tmax.x, tmax.y), tmax.z);
  tNear = max(tNear, 0.0);
  if (tFar <= tNear) { discard; }

  vec3 boxSize = uBoxMax - uBoxMin;
  float dt = (tFar - tNear) / float(uSteps);
  vec4 acc = vec4(0.0);
  float eMax = 0.0;
  vec3 eMaxCol = vec3(0.0);
  for (int s = 0; s < 512; s++) {
    if (s >= uSteps) break;
    float t = tNear + (float(s) + 0.5) * dt;
    vec3 p = ro + rd * t;
    vec3 uvw = (p - uBoxMin) / boxSize;
    vec4 tx = texture(uVolume, uvw);
    float e = clamp((uPrecolored == 1 ? tx.a : tx.r) * uInvMax, 0.0, 1.0);
    vec3 col = uPrecolored == 1 ? tx.rgb : heatmapColor(e);
    if (e > eMax) { eMax = e; eMaxCol = col; }
    if (e > 0.004) {
      float a = clamp(pow(e, uGammaAccumulate) * uOpacity * uStepNorm, 0.0, 1.0);
      acc.rgb += (1.0 - acc.a) * col * a;
      acc.a   += (1.0 - acc.a) * a;
      if (uMix < 0.001 && acc.a > 0.98) break;
    }
  }
  float aMip = clamp(pow(eMax, uGammaMip) * uOpacity, 0.0, 1.0);
  vec4 peak = vec4(eMaxCol * aMip, aMip);
  vec4 result = mix(acc, peak, uMix);
  if (result.a <= 0.0) { discard; }
  outColor = result;          // premultiplied RGBA
}
```

### 2.5 WGSL translation

```wgsl
struct VolumeUniforms {
  modelMatrix: mat4x4<f32>,
  viewMatrix: mat4x4<f32>,
  projectionMatrix: mat4x4<f32>,
  cameraPosition: vec3<f32>,  uInvMax: f32,
  uBoxMin: vec3<f32>,         uOpacity: f32,
  uBoxMax: vec3<f32>,         uGammaAccumulate: f32,
  uGammaMip: f32, uStepNorm: f32, uMix: f32, uColormap: i32,
  uSteps: i32, uPrecolored: i32, uCustomStopCount: i32, _pad0: i32,
  uCustomStops: array<vec4<f32>, 8>,          // (pos, r, g, b)
};
@group(0) @binding(0) var<uniform> u: VolumeUniforms;
@group(0) @binding(1) var uVolume: texture_3d<f32>;   // Rgba32Float (or Rgba16Float), n^3, no mips
@group(0) @binding(2) var uVolumeSampler: sampler;    // Nearest (crisp) or Linear (smooth); ClampToEdge

struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) vWorldPos: vec3<f32> };

@vertex
fn vs_main(@location(0) position: vec3<f32>) -> VsOut {   // unit cube, centred at origin
  let world = u.modelMatrix * vec4<f32>(position, 1.0);
  var o: VsOut;
  o.vWorldPos = world.xyz;
  o.clip = u.projectionMatrix * u.viewMatrix * world;
  return o;
}

fn customStopsColor(t: f32) -> vec3<f32> {
  let n = u.uCustomStopCount;
  if (n <= 0) { return vec3<f32>(t); }
  if (t <= u.uCustomStops[0].x) { return u.uCustomStops[0].yzw; }
  for (var i: i32 = 0; i + 1 < 8; i++) {
    if (i + 1 >= n) { break; }
    let a = u.uCustomStops[i];
    let b = u.uCustomStops[i + 1];
    if (t <= b.x) {
      let f = select(0.0, (t - a.x) / (b.x - a.x), b.x > a.x);
      return mix(a.yzw, b.yzw, f);
    }
  }
  return u.uCustomStops[n - 1].yzw;
}

fn heatmapColor(value: f32) -> vec3<f32> {
  let t = clamp(value, 0.0, 1.0);
  if (u.uColormap == 4) { return customStopsColor(t); }
  if (u.uColormap == 3) { return vec3<f32>(1.0, 0.0, 0.0); }
  if (u.uColormap == 2) { return vec3<f32>(1.0, 1.0 - t, 1.0 - t); }
  if (u.uColormap == 1) { return vec3<f32>(t, t, 1.0); }
  var c: vec3<f32>;
  if (t < 0.25)      { c = mix(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 1.0), (t - 0.00) / 0.25); }
  else if (t < 0.48) { c = mix(vec3<f32>(0.0, 1.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), (t - 0.25) / 0.23); }
  else if (t < 0.70) { c = mix(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 1.0, 0.0), (t - 0.48) / 0.22); }
  else               { c = mix(vec3<f32>(1.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), (t - 0.70) / 0.30); }
  return c;
}

@fragment
fn fs_main(i: VsOut) -> @location(0) vec4<f32> {
  let ro = u.cameraPosition;
  let rd = normalize(i.vWorldPos - ro);
  let invd = 1.0 / rd;                                  // IEEE inf on axis-aligned rays, same as GLSL
  let ta = (u.uBoxMin - ro) * invd;
  let tb = (u.uBoxMax - ro) * invd;
  let tmin = min(ta, tb);
  let tmax = max(ta, tb);
  var tNear = max(max(tmin.x, tmin.y), tmin.z);
  let tFar  = min(min(tmax.x, tmax.y), tmax.z);
  tNear = max(tNear, 0.0);
  if (tFar <= tNear) { discard; }

  let boxSize = u.uBoxMax - u.uBoxMin;
  let dt = (tFar - tNear) / f32(u.uSteps);
  var acc = vec4<f32>(0.0);
  var eMax = 0.0;
  var eMaxCol = vec3<f32>(0.0);
  for (var s: i32 = 0; s < 512; s++) {
    if (s >= u.uSteps) { break; }
    let t = tNear + (f32(s) + 0.5) * dt;
    let p = ro + rd * t;
    let uvw = (p - u.uBoxMin) / boxSize;
    // textureSampleLevel: no derivatives, legal in non-uniform control flow (the early break)
    let tx = textureSampleLevel(uVolume, uVolumeSampler, uvw, 0.0);
    let e = clamp(select(tx.r, tx.a, u.uPrecolored == 1) * u.uInvMax, 0.0, 1.0);
    let col = select(heatmapColor(e), tx.rgb, u.uPrecolored == 1);
    if (e > eMax) { eMax = e; eMaxCol = col; }
    if (e > 0.004) {
      let a = clamp(pow(e, u.uGammaAccumulate) * u.uOpacity * u.uStepNorm, 0.0, 1.0);
      acc = vec4<f32>(acc.rgb + (1.0 - acc.a) * col * a, acc.a + (1.0 - acc.a) * a);
      if (u.uMix < 0.001 && acc.a > 0.98) { break; }
    }
  }
  let aMip = clamp(pow(eMax, u.uGammaMip) * u.uOpacity, 0.0, 1.0);
  let peak = vec4<f32>(eMaxCol * aMip, aMip);
  let result = mix(acc, peak, u.uMix);
  if (result.a <= 0.0) { discard; }
  return result;                                        // premultiplied; written raw (no OETF), see §0.3
}
```
Pipeline: `TriangleList` (36 indices of a unit cube), `cull_mode: Some(Face::Front)` (three `BackSide`), blend `PREMULTIPLIED_ALPHA_BLENDING` (color `One/OneMinusSrcAlpha`, alpha `One/OneMinusSrcAlpha`, Add), `depth_compare: Always`, `depth_write_enabled: false`. `pow(0, γ)` is never hit for `e > 0.004`; `pow(eMax=0, γ)` = 0 as in GLSL. Note `heatmapColor` is called with a `select` in WGSL, so both branches are evaluated; harmless.

### 2.6 Shared parameters (`state.js:521-542`, `object-energy-shared.js:155-168`)

| `app.*` field | Default | Range (UI) | Control id | i18n |
|---|---|---|---|---|
| `objectEnergyVolumeMix` | 0.6 | 0–1 step 0.01 | `objectEnergyVolumeMixSlider` | `heatmap.objectEnergy.mix` |
| `objectEnergyVolumeGammaAccumulate` | 4 | 1–10 step 0.1 (`VOLUME_GAMMA_RANGE.accumulate`, default 2.5 if NaN) | `objectEnergyVolumeGammaAccumulateSlider` | `heatmap.objectEnergy.gammaAccumulate` |
| `objectEnergyVolumeGammaMip` | 3 | 0.2–3 step 0.05 (`VOLUME_GAMMA_RANGE.mip`, default 0.8 if NaN) | `objectEnergyVolumeGammaMipSlider` | `heatmap.objectEnergy.gammaMip` |
| `objectEnergyHeatmapResolution` | 64 | 8–64 step 2 | `objectEnergyHeatmapResolutionSlider` | `heatmap.objectEnergy.resolution` |
| `objectEnergyHeatmapOpacity` | 1 | 0.05–1.0 step 0.05 | `objectEnergyHeatmapOpacitySlider` | `heatmap.objectEnergy.opacity` |
| `volumeRefreshMs` | 160 | 40–500 step 10 | `volumeRefreshSlider` | `heatmap.objectEnergy.refresh` |
| `volumeSmoothInterpolation` | false | switch | `volumeSmoothToggle` | `heatmap.smooth` |
| `heatmapBandIndex` / `heatmapAllBands` | 0 / true | select `heatmapBandSelect` (values `"0".."n-1"` or `"all"`; `"all"` only offered for multi-band layouts) | `heatmapBandSelect` | `heatmap.crossoverBand`, `heatmap.bandAll`, `heatmap.bandFull`, `heatmap.bandIndex` |

All of these are shared by the four providers ("Common parameters" block, `index.html:462-492`). Persisted in `localStorage['spatialviz.effective_render_prefs']` (`controls/room-geometry.js:98-137`), together with every heatmap toggle/colormap/scale and both custom-gradient stop lists.

### 2.7 Colormaps (`object-energy-shared.js:30-111`)

`OBJECT_ENERGY_COLORMAPS = ['heatmap', 'blueWhite', 'whiteRed', 'red', 'custom']` → `uColormap` 0..4 (`colormapIndex`, unknown → 0). Value `t ∈ [0,1]` = normalised level; alpha is always carried separately by the ray-march (so `'red'` shows energy through alpha only).

| Index | Name | Control points (t → RGB), linear interpolation in RGB |
|---|---|---|
| 0 | `heatmap` (`HEATMAP_STOPS`) | 0.00 → (0,0,1) blue; 0.25 → (0,1,1) cyan; 0.48 → (0,1,0) green; 0.70 → (1,1,0) yellow; 1.00 → (1,0,0) red |
| 1 | `blueWhite` | RGB = (t, t, 1) — 0 → (0,0,1), 1 → (1,1,1) |
| 2 | `whiteRed` | RGB = (1, 1−t, 1−t) — 0 → (1,1,1), 1 → (1,0,0) |
| 3 | `red` | constant (1,0,0) |
| 4 | `custom` | user stops `{pos, r, g, b}` sorted by pos, 2..8 stops; below first stop → first colour, above last → last colour; no stops → grey (t,t,t). Defaults for **both** `app.objectCustomGradientStops` and `app.speakerCustomGradientStops`: `[{0.0,(0,0,1)}, {0.5,(0,1,0)}, {1.0,(1,0,0)}]` (`state.js:510-519`). |

The JS `objectEnergyColor()` (CPU side, used by the speaker all-bands precolour path) and the GLSL `heatmapColor()` are kept identical. The same index is sent to the mpv overlay (`mpv_overlay_set_heatmap_colormap`).

### 2.8 Providers' contract

`volume.update({ resolution, opacity, mix, gammaAccumulate, gammaMip, colormap?, customStops?, smooth, sampleEnergy? | sampleColor?, maxLevel? })` (`energy-volume-core.js:216`). `hide()` just sets `group.visible=false`; `dispose()` frees the texture.

### 2.9 Update cadence — the "energy-volume upload throttle" (PR #88, commit `ee61c66a`)

- Every provider's `refresh*(now)` is called each animation frame (`app.js:346-349`). The **render** happens every frame (the box mesh is always drawn while visible); the **rebuild** (n³ CPU sampling + full texture upload + uniform update) is throttled: `if (now − app.last<Provider>At < refreshMs) return`, with `refreshMs = app.volumeRefreshMs > 0 ? app.volumeRefreshMs : VOLUME_REBUILD_INTERVAL_MS (160)` (`object-energy-shared.js:28`, `object-energy-volume.js:35-39`, `global-energy-volume.js:92-97`). PR #88 moved this from the 70 ms trail cadence to 160 ms (~6 Hz) because 64³×RGBA32F ≈ 4 MB per upload ≈ 56 MB/s at 70 ms ballooned the WebView2 GPU process on Windows; the follow-up commit made it the `volumeRefreshMs` slider (40–500 ms).
- UI changes reset the timestamp (`app.last…At = 0`) so the next frame rebuilds immediately (`listeners/trails-and-display-listeners.js:480-492`).
- The three **static** (gain-table) providers additionally keep a build signature (`signaturesEqual`, `object-energy-shared.js:192-198`) of `[table identity, scale, heatmapBandIndex, heatmapAllBands, smooth, resolution, opacity, mix, γacc, γmip, roomRatio.height/lower/width/rear/length (+ colormap & gradient version for the speaker one)]` and skip the rebuild entirely while it is unchanged (the volume simply stays up). The object field has no signature (live levels change every tick).

---

## 3. Object energy field provider (`scene/object-energy-volume.js`, `object-energy-shared.js:170-309`)

- Enabled by `app.objectEnergyHeatmapEnabled` (default false; `objectEnergyHeatmapToggle`, i18n `heatmap.objectEnergy`; scene-FX `fxFieldBtn`, `sceneFx.energyField`). Hidden when disabled or when no object is active.
- `collectActiveObjects()` refills a reusable array of `{x, y, z, energy}` from `sourcePositionsRaw` (Omniphony normalised, the hydrated record of §1.2) for every object that is not in `objectMuted` and has finite x/y/z and `energy > 0`, where:
  - `rms = objectFieldRms(level)`: if `app.heatmapAllBands` or `level.bandRmsDbfs` has < 2 entries → `level.rmsDbfs`; else `bandRmsDbfs[clamp(round(heatmapBandIndex), 0, len−1)]`. (`sourceLevels` entry = `{peakDbfs, rmsDbfs, bandRmsDbfs|null}` from `source:meter`, `sources.js:1212-1223`.)
  - `energy = objectEnergyLinear(rms) = 10^(rms/10)`, 0 if not finite or `rms ≤ SILENT_RMS_DBFS (−100)`.
- Field: `r0 = clamp(app.objectEnergyHeatmapFalloffRadius, 0.01, 1.0)` (state default 0.5; slider `objectEnergyHeatmapRadiusSlider` 0.02–0.5 step 0.01, i18n `heatmap.objectEnergy.radius`);
  `sampleEnergy(ow, od, oh) = Σ_objects energy_o / ((ow−x_o)² + (od−y_o)² + (oh−z_o)² + r0²)` — Omniphony units on both sides (x=width, y=depth, z=height).
- `update({ …common, colormap: colormapIndex(app.objectEnergyColormap) /* default 'blueWhite' = 1 */, customStops: app.objectCustomGradientStops, sampleEnergy })`, self-normalised (`uInvMax = 1/max`).
- Cost: n³ × objects per rebuild on the CPU (64³ = 262 144 cells).
- The colormap and enable state are mirrored to the mpv overlay (`setMpvOverlayHeatmapEnabled`, `mpv_overlay_set_heatmap_colormap`, `setMpvOverlayHeatmapCustomStops`) — not a viewport concern.

---

## 4. Global energy deviation provider (`scene/global-energy-volume.js`)

- Enabled by `app.globalEnergyHeatmapEnabled` (default false; `globalEnergyHeatmapToggle`, i18n `heatmap.globalEnergy`), scale `app.globalEnergyHeatmapScaleDb` (default 6; number input `globalEnergyHeatmapScale` 1–40 step 1, clamped to [1,40], i18n `heatmap.globalEnergy.scale`). Enabling calls `acquireGainTable('globalEnergyVolume')` (§5.6), disabling `releaseGainTable`.
- Data: `getGlobalEnergyTable()` = decoded OBGT table cached under `speakerIndex = −1`; hidden until received.
- Amplitudes: `bands[bandIndex].gains` (Float32Array, one per cell) with `bandIndex = clamp(round(heatmapBandIndex), 0, nbands−1)`; if `heatmapAllBands && nbands > 1`: `amp[i] = sqrt(Σ_b gains_b[i]²)` (bands partition the spectrum; powers add).
- Per cell (precoloured): `amp = amplitudes[cellIndex(ow, od, oh)]` (§5.5);
  `t = amp > 1e−6 ? clamp(20·log10(amp)/scaleDb, −1, 1) : −1`;
  `t ≥ 0` → RGB (1, 0, 0) (excess, red); `t < 0` → RGB (0, 0.35, 1) (deficit, blue); `level = |t|` (0 dB → transparent). `maxLevel: 1` so `uInvMax = 1`.
- Rebuild only when the signature (§2.9) changes and the refresh throttle allows.

(Siblings, for completeness: the speaker volume uses `g²` of `bands[band].gains[cell]` for the selected speaker's table with `uColormap = app.speakerHeatmapVolumeColormap` (default `'heatmap'`) and `app.speakerCustomGradientStops`, or in all-bands mode a level-weighted average of per-band gradient colours placed at each band's log-frequency centre `t = (ln √(lo·hi) − ln 20)/(ln 20000 − ln 20)` with `level = Σ g²`; the discontinuity volume uses table −2/−3 with amber precolour `(1, 0.65−0.45t, 0.05, t)`, `t = clamp(jump/scale, 0, 1)`, all-bands = per-cell max over bands.)

---

## 5. Gain-table transport (renderer → Studio), traced end to end

### 5.1 Renderer serialisation: "OBGT" container (`omniphony-renderer/renderer/src/band_gaintable.rs:232-297`)

```
offset  size  content
0       4     magic "OBGT"
4       1     version = 1
5       3     reserved (0)
8       4     meta_len   u32 LE
12      4     payload_len u32 LE
16      meta_len     metadata JSON (UTF-8)
16+meta_len  payload_len  zlib stream (flate2 ZlibEncoder, Compression::default())
```
Metadata JSON: `{"domain":"cartesian_bands","speaker_index":<i64>,"x_count":nx,"y_count":ny,"z_count":nz,"band_count":nb,"bands":[{"low_hz":f32,"high_hz":f32|null}, …]}`. `speaker_index` ≥ 0 = one speaker's slice; `−1` = `GLOBAL_ENERGY_INDEX`; `−2` = `GAIN_DISCONTINUITY_INDEX`; `−3` = `CENTROID_JUMP_INDEX` (`band_gaintable.rs:40-48`).

Inflated payload (all `f32` little-endian, no padding):
```
x_positions[nx]  y_positions[ny]  z_positions[nz]
band 0: values[nx*ny*nz]   band 1: values[…]  …  band nb-1
```
Cell index (`live_params.rs:2210-2214`): `cell = xi + nx*(yi + ny*zi)` (xi fastest). Value per (band, cell): speaker slice `gains[cell*speaker_count + speaker]`; energy field `sqrt(Σ_s g_s²)` (`band_gaintable.rs:93-102`); discontinuity fields per module docs.

Axes (`live_params.rs:2166-2190`, `omniphony-geometry/src/lib.rs:322-354`): `x_positions = evenly_spaced_axis(x_size.max(2), −1, 1)`, `y_positions = evenly_spaced_axis(y_size.max(2), −1, 1)` (node counts; the live parameter is an interval count converted as `x_size.max(1)+1`, `live_params.rs:1410`); `z_positions = cartesian_z_axis(z_size.max(2), z_neg_size)` = `z_neg_size` nodes at `−1 + i/z_neg_size` (i = 0..z_neg_size, covering [−1,0)) followed by `evenly_spaced_axis(z_size, 0, 1)` — **not** symmetric, hence the client must use the shipped `zPositions` (nearest-node lookup) rather than recompute.

### 5.2 Chunking over OSC (`omniphony-renderer/runtime_control/src/osc.rs:172-235`, addresses in `runtime_control/src/osc_contract.rs:395-400`)

- `version = DefaultHasher(bytes) & 0x7fff_ffff` (stable per serialised table; a topology rebuild changes it).
- `GAINTABLE_CHUNK_BYTES = 1024`; `chunk_count = ceil(len/1024)` (≥1).
- Meta: `/omniphony/state/debug/speaker_gaintable/meta`, 1 string arg: `{"version":u32,"total_len":usize,"chunk_count":usize,"chunk_bytes":1024}`.
- Chunks: `/omniphony/state/debug/speaker_gaintable/chunk`, 1 blob arg: `[version u32 LE][chunk_index u32 LE][≤1024 bytes of the OBGT container]`.
- Also `/omniphony/state/debug/speaker_gaintable/unavailable` (string JSON reason) and `/omniphony/state/debug/speaker_gaintable/uptodate` (int version).
- Control (client → renderer): `/omniphony/control/debug/speaker_gaintable/subscribe [i have_version(≥0), i speaker_index]` (`commands/diag.rs:51-68`; `speaker_index` may be −1/−2/−3), `/omniphony/control/debug/speaker_gaintable/unsubscribe` (no args), `/omniphony/control/debug/speaker_gaintable/nack [i version, i idx…]` (`osc_listener.rs:1526-1546`, ≤ 256 indices per datagram).

### 5.3 Studio Rust side (`osc_parser.rs`, `osc_listener.rs`)

- Parsing (`osc_parser.rs:824-838`, address split into lowercase parts; `parts.len()==5 && parts[2]=="debug" && parts[3]=="speaker_gaintable"`): `OscEvent::StateDebugSpeakerGaintableMeta{value: String}`, `::StateDebugSpeakerGaintableChunk{bytes: Vec<u8>}` (blob arg, `unwrap_blob`), `::StateDebugSpeakerGaintableUnavailable{value}`, `::StateDebugSpeakerGaintableUptodate{version: i32}` (`osc_parser.rs:290-297`).
- Reassembly (`osc_listener.rs:1402-1567`): per-version `BTreeMap<u32, GainTableAsm{chunk_count, chunks: BTreeMap<u32, Vec<u8>>, …}>` (max 6 in flight, oldest evicted); a chunk is routed by its embedded version; when `chunks.len() == chunk_count` they are concatenated in index order and decoded. NACK repair: after 120 ms without activity, missing indices are re-requested, up to 12 rounds, then abandoned. The UDP receive buffer is enlarged for the burst (`osc_listener.rs:932`).
- Decode (`decode_evaluation_artifact` → `decode_band_gaintable`, `osc_listener.rs:1604-1660`): validates magic, reads meta/payload lengths, `flate2::read::ZlibDecoder` inflates, reads dims and bands from the metadata, and emits Tauri event **`speaker_gaintable`** with `{version, domain:"cartesian_bands", speakerIndex:i64, xCount, yCount, zCount, bandCount, bands:[{lowHz, highHz|null}], dataB64: base64(inflated bytes)}` (`osc_listener.rs:2704-2709`). Legacy "OEVL" artifacts (full multi-speaker cartesian/polar tables) are also decoded (`1662-1735`) but no current JS consumer uses them. `unavailable`/`uptodate` are forwarded as `speaker_gaintable:unavailable` / `speaker_gaintable:uptodate` (JS only logs the former).

### 5.4 JS decode (`scene/speaker-gaintable.js:152-203`, `tauri-bridge.js:206-210`)

`setSpeakerGainTable(payload)`: base64 → `ArrayBuffer`; requires `byteLength ≥ (nx+ny+nz+nb·cells)·4`; builds zero-copy views `xPositions = Float32Array(buf, 0, nx)`, `yPositions`, `zPositions`, then per band `gains = Float32Array(buf, off, cells)`; stores `tables[speakerIndex] = {nx, ny, nz, speakerIndex, bands:[{lowHz, highHz, gains}], xPositions, yPositions, zPositions}` and `versions[speakerIndex] = version`. (A native port can skip base64 and keep the `Vec<f32>` from the inflater.)

### 5.5 Cell lookup used by the volume samplers (`object-energy-shared.js:235-257`)

```
makeCellIndexer(table) → (ow, od, oh) ↦ cell:
  xi = clamp(round((ow+1)/2 · (nx−1)), 0, nx−1)                  // width axis is regular
  yi = clamp(round((od+1)/2 · (ny−1)), 0, ny−1)                  // depth axis is regular
  zi = zPositions ? argmin_i |zPositions[i] − oh| : clamp(round((oh+1)/2·(nz−1)), 0, nz−1)   // memoised on oh
  cell = xi + nx·(yi + ny·zi)
```
i.e. nearest-node sampling of the table at each volume texel centre; the volume's own n is independent of the table's nx/ny/nz.

### 5.6 Subscription lifecycle (`scene/speaker-gaintable.js:62-150`)

`acquireGainTable(consumerId)` / `releaseGainTable(consumerId)` with consumers `speakerSoloVolume` → target `selectedSpeakerIndex` (or 0), `globalEnergyVolume` → −1, `discontinuityVolume` → −2 (`'gain'`) or −3 (`'centroid'`). Each acquire (and speaker/mode change via `refreshGaintableSubscription`) sends one `subscribe_speaker_gaintable{haveVersion, speakerIndex}` per distinct target; a 5 s heartbeat repeats it (repair path — renderer answers `uptodate` if versions match, else re-pushes). Releasing the last consumer of a target drops its cached table; releasing the last consumer overall stops the heartbeat and sends `unsubscribe`. Persisted-on toggles re-acquire at startup (`listeners/trails-and-display-listeners.js:320, 376, 409`).

---

## 6. `scene/gradient-editor.js` — 2D DOM UI, not a viewport element

Pure HTML/CSS widget (no three.js): a 20 px CSS `linear-gradient` bar with pentagon "pin" handles (14×18 px, `clip-path` polygon) under it, one per stop; drag a pin to move `stop.pos` (clamped [0,1], list re-sorted), click a pin to open a popover with an inline HSV picker (132×84 saturation/value square + 12 px hue strip), a live swatch, delete (disabled at ≤ 2 stops) and close; double-click the bar adds a stop (≤ `MAX_CUSTOM_STOPS = 8`) coloured by sampling the current gradient. Two independent targets: `'object'` edits `app.objectCustomGradientStops` (mounted in `#objectGradientEditor`), `'speaker'` edits `app.speakerCustomGradientStops` (`#speakerGradientEditor`), each shown only while its colormap select is `'custom'` (`listeners/trails-and-display-listeners.js:336-341`). Edits bump `app.speakerCustomGradientVersion` (speaker) and call `onChange(target)` → object: immediate volume rebuild + mpv overlay stops push; speaker: `lastSpeakerSoloVolumeAt = 0`; both persist. Output to the viewport = the `customStops` arrays consumed as `uCustomStops` (§2.7). i18n: `heatmap.gradient.addHint`, `heatmap.gradient.removeStop`, `heatmap.gradient.close`.

---

## 7. `controls/scene-effects-bar.js` — floating quick-toggle bar (UI only)

`#sceneEffectBar` pinned over the bottom of the 3D view (`index.html:955-1003`). Each button just toggles its source checkbox and dispatches `change` (so all panel handlers run) and mirrors `.active`/`aria-pressed`:

| Button | Source control | i18n title | Flyout |
|---|---|---|---|
| `fxGridBtn` | `vbapCartesianGridToggleBtn` | `sceneFx.grid` | — |
| `fxObjectsBtn` | `showObjectsToggle` | `sceneFx.objects` | `fxObjectsMenu` → `objectDisplayModeSelect` (`circle` / `transparent-sphere` / `diffuse-sphere`) |
| `fxLabelsBtn` | `objectLabelsToggle` | `sceneFx.labels` | — |
| `fxTrailsBtn` | `trailToggle` | `sceneFx.trails` | `fxTrailsMenu` → `trailModeSelect` (`diffuse` / `line`); picking a mode also enables the layer |
| `fxFieldBtn` | `objectEnergyHeatmapToggle` | `sceneFx.energyField` | — |
| `fxHeatmapBtn` | `speakerHeatmapVolumeToggle` | `sceneFx.heatmap` | — |
| `fxMpvBtn` | `mpvOverlayToggle` | `sceneFx.mpvOverlay` | — |

Flyouts open from the caret (`.fx-caret`) or right-click; close on outside click / Escape. No rendering logic. Icons are inline SVG (per the project rule: icons + i18n in `title`).

---

## 8. `listeners/trails-and-display-listeners.js` — what it wires (summary)

Everything in §1.11, §2.6, §3, §4 plus (outside this spec's viewport scope): `effectiveRenderToggle`, `showObjectsToggle`, `objectColorsToggle`, `objectDisplayModeSelect`, `objectSphereSizeSlider` (0.03–0.2, default 0.07), `objectLabelsToggle`, `showObjectDetailsToggle`/`objectDetailsToggleBtn`, `speakerLabelsToggle`, `speakerBandBarsToggle`, `speakerFaceListenerToggle`, `speakerSizeSlider` (0.04–0.2, default 0.08), `localeSelect`, `mpvOverlayToggle`, `speakerHeatmapVolumeToggle`/`speakerHeatmapVolumeColormap`, `discontinuityHeatmapToggle`/`Mode`/`Scale` (0.05–2, default 0.5), `heatmapBandSelect` (`setupBandCursor` = floating band cursor over the 3D view, `controls/band-cursor.js`). Every handler ends with `persistEffectiveRenderPrefs()` or `persistTrailPrefs()`; several also `invoke('mpv_overlay_set_*')`.

---

## 9. Non-viewport items in the assigned files (schedule separately)

- `scene/gradient-editor.js` entirely (DOM widget).
- `controls/scene-effects-bar.js` entirely (DOM toolbar + flyouts).
- `listeners/trails-and-display-listeners.js`: all DOM binding, localStorage persistence, mpv-overlay OSC mirroring, `onOverlayState` UI resync, gain-table consumer acquire/release.
- `trails.js:309-312` and `decayTrails`'s `has-active-trail` CSS class toggling on the object list.
- `scene/speaker-gaintable.js`: base64 decoding, Tauri `invoke` subscription plumbing, heartbeat timer.
- `index.html` sections `#trailSection`, `#heatmapsSection`, `#trailInfoModal`, `#heatmapInfoModal`.

## 10. Uncertainties / things the code does not settle

- The sign convention of Omniphony x (which side is +width) is not stated in the files read; the port should take it from `omniphony-geometry` (`adm_to_scene`).
- Three's `Data3DTexture` default `wrapR/S/T = ClampToEdge`; not set explicitly in the JS, assumed default.
- `gl_PointSize` max is implementation-defined in WebGL; the clamp to 44 px is assumed to always be honoured.
- In §5.5 the fallback `zi` formula (when `zPositions` is absent) can never be hit with the current decoder (positions are always shipped); listed for completeness.
- `speaker_gaintable:uptodate` is deliberately unhandled in JS; a native port only needs it to avoid re-requesting.
