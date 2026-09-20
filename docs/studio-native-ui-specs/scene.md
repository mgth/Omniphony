# Viewport parity spec — scene setup, camera, room, axes, head, coordinates, gizmos, hybrid shape

Source tree: `omniphony-studio/src/` (all paths below relative to it unless absolute). three.js **r0.165.0** (`package.json:26`). No file in this scope defines a custom GLSL shader (no `ShaderMaterial`/`RawShaderMaterial`); every material is a stock three material, so §15 gives pipeline-level WGSL notes instead of translations.

## 0. Conventions

| Item | Value | Ref |
|---|---|---|
| ADM / Omniphony frame | x = right, y = front, z = up, normalised cube [-1,1]³ | `coordinates.js:22-39`, `omniphony-geometry/src/lib.rs:86-96` |
| three scene frame | x = depth (front +), y = up, z = right | same |
| ADM → scene | `(sx, sy, sz) = (ay, az, ax)` — crate `adm_to_scene` (`lib.rs:88`) | `coordinates.js:33-39` |
| scene → ADM | `(ax, ay, az) = (sz, sx, sy)` — crate `scene_to_adm` (`lib.rs:94`) | `coordinates.js:25-31` |
| Scene unit | 1 unit = `metersPerUnit` metres = room half-width = layout `radius_m` (`speakers.js:2261,2496`; `state.js:113`) | `coordinates.js:186-196` |
| Colours | hex literals are sRGB (three `ColorManagement.enabled=true`, renderer `outputColorSpace=SRGBColorSpace`, `toneMapping=NoToneMapping` — all r165 defaults, nothing overrides them). Unlit `MeshBasicMaterial`/`LineBasicMaterial` colours therefore appear on screen exactly as the hex value before alpha blending. | — |
| Blending | every `transparent:true` material uses three `NormalBlending` = `src_alpha, one_minus_src_alpha`, non-premultiplied; opaque pass first, then transparent objects sorted by `renderOrder` ascending, then far→near | — |
| Line width | `LineBasicMaterial.linewidth` is ignored by WebGL: all lines are 1 px | `setup.js:282` |
| Canvas | opaque (`alpha:false`), `antialias:true` (MSAA) | `setup.js:83,202-205,211` |

## 1. Render surface, viewport, canvas vs overlays

Files: `scene/setup.js:28-83`, `core/viewport/window-viewport.js`, `core/render/render-surface-controller.js`, `core/render/projection-service.js`, `styles/app.css`.

- The canvas is a **full-window** surface: a `<div id="omniphony-renderer-mount">` `position:fixed; inset:0; z-index:0; overflow:hidden; pointer-events:auto` prepended to `<body>` (`setup.js:46-61`), holding the WebGL canvas. Nothing about the overlay panels changes the surface: panels float **over** it.
- Viewport = `{ width: max(1, window.innerWidth), height: max(1, window.innerHeight), dpr: max(1, devicePixelRatio) }` (`window-viewport.js:4-10`). On `window.resize` (`window-viewport.js:19-25`) → `applyViewport` (`render-surface-controller.js:16-29`): `camera.aspect = w/h`, `updateProjectionMatrix`, `renderer.setPixelRatio(dpr)`, `renderer.setSize(w, h)` (drawing buffer = w·dpr × h·dpr), then `reapplyViewOffset(w, h)` (§2.3). Called once at init through `initRenderSurfaceController({getRenderer, getCamera})` (`render-surface-controller.js:94-104`).
- Overlay panels (`app.css`): left `#overlay` fixed `top:1rem; left:1rem; width: var(--panel-width-left)=min(440px,92vw); height: calc(100vh - 2rem); z-index:2; background rgba(0,0,0,0.65); backdrop-filter blur(8px); border 1px rgba(255,255,255,0.2); radius 12px` (`app.css:40-56`); right `#speakersOverlay` mirror image with `right:1rem; width: var(--panel-width-right)` (`app.css:571-589`). Both are user-resizable (`.panel-resize-handle`, `app.css:77-103`) and collapsible (`.panel-collapsed`, `app.css:60-72`). **The 3D view is never resized by panel changes** (CLAUDE.md rule) — the camera aspect is the full window. Body background is `#0a0b10` (`app.css:13-18`), same as the scene clear colour.
- Pointer → NDC (`projection-service.js:5-10`): `ndc.x = ((clientX - rect.left)/rect.width)*2 - 1`, `ndc.y = -((clientY - rect.top)/rect.height)*2 + 1` with `rect = canvas.getBoundingClientRect()`. Picking unprojects via the camera's projection inverse, so the lens-shift pan (§2.3) is honoured automatically by `raycaster.setFromCamera` (`picking.js:159,254,309`).
- `getRenderSurfaceInvariantState` (`render-surface-controller.js:52-92`) is a diagnostic only (checks mount is `fixed` at 0/0/0/0 and the canvas rect equals the viewport within 1 px). Not a viewport element.
- `rebuildRendererOnExistingCanvas` / `rebuildRendererOnFreshCanvas` / `teardownRenderer` (`setup.js:199-228`) are WebGL context-loss recovery paths; a native renderer has no equivalent. `dragstart` is suppressed on the mount (`setup.js:60`) — WebKitGTK compositor workaround, not needed natively.

## 2. Camera and orbit controls

### 2.1 Camera (`setup.js:20-26`)

| Param | Value |
|---|---|
| Type | perspective, **vertical FOV 65°**, near **0.1**, far **100**, aspect = viewport w/h |
| Up | +Y (three default) |
| Orbit pivot `HEAD_PIVOT` | `(0, 0.25, 0)` scene (0.25 above the listener origin) |
| Initial position | `(-3.8, 1.1, 0.0)` looking at `HEAD_PIVOT` → i.e. behind the listener (−depth), slightly above. In OrbitControls spherical terms about the pivot: radius ≈ 3.894, polar φ ≈ 77.4° from +Y, azimuth θ = atan2(x, z) = −90° |

The spike's camera (`omniphony-studio-egui/src/render/camera.rs`) uses fov 45°, near 0.05, target (0,0,0.1) in a z-up frame — all differ from the above; parity needs fov 65°, near 0.1, far 100, pivot (ADM) `(0, 0, 0.25)`.

### 2.2 OrbitControls (`setup.js:85-105`)

| Setting | Value | Note |
|---|---|---|
| `target` | `HEAD_PIVOT` | never moved (pan is a lens shift, §2.3) |
| `enablePan` | **false** | |
| `mouseButtons` | LEFT = rotate, MIDDLE = dolly, RIGHT = null (freed for custom pan) | |
| `enableDamping` / `dampingFactor` | **true / 0.06** | default factor is 0.05 |
| `enableZoom` | true (wheel); set false only transiently during Ctrl/Shift-wheel in polar edit (`picking.js:69-83`) and reset true (`speakers.js:1877`, `picking.js:109`) | |
| `enabled` | false during a gizmo drag (`picking.js:231`), true again on release (`picking.js:375`); the custom pan also checks it (`setup.js:162`) | |
| `minDistance/maxDistance` | 0 / ∞ (default) | no dolly clamp |
| `minPolarAngle/maxPolarAngle` | 0 / π (default), `makeSafe` clamps φ to [1e-6, π−1e-6] | can look from straight above/below |
| `minAzimuthAngle/maxAzimuthAngle` | −∞ / ∞ (default) | |
| `rotateSpeed/zoomSpeed` | 1 / 1 (default) | |
| keys | `listenToKeyEvents` never called → arrow keys inert | |
| touches | ONE = rotate, TWO = dolly (pan disabled) | |
| `autoRotate`, `zoomToCursor` | false | |

OrbitControls maths to reproduce (three r165 `OrbitControls.js`):
- Rotate (left drag, `:694-704`): per pointer move `Δθ −= 2π·dx/clientHeight·rotateSpeed`, `Δφ −= 2π·dy/clientHeight` (**both use the element height**), accumulated into `sphericalDelta`.
- Dolly (`:512-516,612-631,748-758`): `f = 0.95^(zoomSpeed·|deltaY·0.01|)`; wheel up (deltaY<0) → `scale *= f` (closer), wheel down → `scale /= f`. Middle-drag: dy>0 → `scale /= f`, dy<0 → `scale *= f`, with `f = 0.95^(|dy·0.01|)`.
- Per frame `controls.update()` (`app.js:334`; `OrbitControls.js:196-300`): `offset = cam.pos − target` → spherical (three convention: `x = r sinφ sinθ, y = r cosφ, z = r sinφ cosθ`); `θ += Δθ·0.06; φ += Δφ·0.06`; clamp φ; `r = r·scale` then `scale = 1`; `cam.pos = target + offset`; `cam.lookAt(target)`; `Δθ *= 0.94; Δφ *= 0.94`. Damping therefore runs every frame until the deltas decay.

### 2.3 Head-pivot pan = lens shift (`setup.js:107-197`)

Right-button drag on the mount (capture phase, pointer capture, context menu suppressed): `panX −= dx; panY −= dy` in CSS px (`:134-139`), then `camera.setViewOffset(W, H, panX, panY, W, H)` (`:121-127`; cleared when both are 0). With `view.width == fullWidth` three's `updateProjectionMatrix` becomes a pure frustum shift: `left += panX·width/W`, `top −= panY·height/H`. Equivalent clip-space form for the Rust side:

```
x_ndc' = x_ndc − 2·panX/W      (= x_ndc + 2·Σdx/W  → image follows the cursor 1:1 in pixels)
y_ndc' = y_ndc + 2·panY/H      (= y_ndc − 2·Σdy/H)
```
i.e. `P' = T · P` with `T = translate(−2·panX/W, +2·panY/H, 0)` applied after the perspective matrix. The pivot stays at its panned screen position while rotating. Re-applied on resize (`render-surface-controller.js:28`). `resetPan()` (`setup.js:142-146`) exists but has **no caller** in `src/` (unused). Picking must use the shifted projection (§1).

## 3. Render-order / depth ledger (this scope)

| Element | renderOrder | depthTest | depthWrite | side | blending |
|---|---|---|---|---|---|
| room box fill | 0 | on | off | front | normal α 0.08 |
| room faces (6 planes) | 1 | **off** | off | double | normal α 0.18 |
| face shadows (speaker + object) | 3 | off | off | double | normal α var |
| hybrid iso shape | 3 | on | off | double | normal α 0.14 |
| screen plane | 5 | off | off | double | normal α 0.18 |
| polar gizmo ring/arc/ticks | 0 (default) | on | on | — | normal α |
| ring/arc current-value labels, distance gizmo arrows/label | 5 | off (sprites) / on (cones) | | | |
| VBAP face grids | 6 | off | off | — | normal α 0.42 |
| axes lines / cones / labels | 30 / 31 / 32 | off | off (lines: on) | — | normal |
| room dimension guide lines / labels | 30 / 31 | off | on / off | — | normal α 0.85 |
| labels (all sprites) | 40 unless overridden | off | off | — | normal, `alphaTest 0.25` |

Note `roomFaceMaterial.polygonOffset` (`setup.js:297-299`) has no effect because depthTest is off.

## 4. Lights (`setup.js:234-261`)

three r165 uses physically-based light units by default (`useLegacyLights=false`). Only lit materials are affected: in this scope only the glTF head (§9). Outside this scope, `scene/materials.js:6` `sourceMaterial` (`MeshPhysicalMaterial`), `:22` `speakerMaterial` (`MeshStandardMaterial`) and `sources.js:321` (`MeshStandardMaterial`) are lit; everything else here is unlit.

| Light | Colour | Intensity | Position / extra | Parent |
|---|---|---|---|---|
| Ambient | `#ffffff` | 0.24 | — | scene |
| Directional (key) | `#fff7ea` | 2.35 | pos `(3.6, 4.8, 1.4)`, target origin → direction = normalize(−3.6, −4.8, −1.4) | scene |
| Directional (rim) | `#b8d4ff` | 1.05 | pos `(−2.8, 1.1, −3.8)` → dir normalize(2.8, −1.1, 3.8) | scene |
| Hemisphere | sky `#dcecff`, ground `#0d0f14` | 0.12 | up = +Y | scene |
| Point `brassempouyFill` | `#fff4dc` | 0.9 | pos `(−0.18, 0.42, 0.22)`, `distance 2.2`, `decay 2` (inverse-square, cut at 2.2) | `brassempouyAnchor` (world-aligned; **not** rotated by head pose) |

Scene background / clear colour: `#0a0b10` (`setup.js:18`). No fog.

## 5. Scene graph (`setup.js:252-266`)

```
scene
├─ roomGroup (identity; `applyRoomRatioToScene` forces scale 1,1,1)        room-geometry.js:973
│  ├─ room (box fill), roomEdges, roomFaces.{posX,negX,posY,negY,posZ,negZ}, screenMesh
│  ├─ vbapCartesianFaceGrids.* (gizmos.js:26-30)
│  └─ brassempouyAnchor (0,0,0)
│     ├─ brassempouyFill (PointLight)
│     └─ headPoseGroup (quaternion = head pose)  └─ glTF head model
├─ roomDimensionGroup (visible = app.roomGeometryExpanded)  └─ 7 guide groups (line + label)
├─ axes group
├─ speakerGizmo.* , distanceGizmo.group, cartesianGizmo.group, selected*Shadows.* (gizmos.js)
└─ hybrid iso mesh (hybrid-distance.js)
```

## 6. Room box

### 6.1 Room bounds and ratios → geometry (`room-geometry.js:950-997`, `setup.js:393-400`)

State: `app.roomRatio = { width, length, height, rear, lower, centerBlend }` (defaults `1, 2, 1, 1, 0.5, 0.5`, `state.js:107`), `app.metersPerUnit` (default 1.0, `state.js:113`).

```
xMax = max(0.001, length)      xMin = −max(0.001, rear)
yMax = max(0.001, height)      yMin = −max(0.001, lower||0.5)
halfZ = max(0.001, width)      depthHalfX = (xMax − xMin)/2
xCenter = (xMin + xMax)/2      yCenter = (yMin + yMax)/2      totalHeight = yMax − yMin
roomBounds = { xMin, xMax, yMin, yMax, zMin: −halfZ, zMax: halfZ }        (:966-971)
```
So with defaults the box spans x ∈ [−1, 2], y ∈ [−0.5, 1], z ∈ [−1, 1]. `setup.js:393-400` seeds `roomBounds` with `xMax=1` and the initial box geometry (2×1×2 at y=0.5) does not match the defaults; `init.js:148-152` always calls `applyRoomRatio`/`applyRoomRatioToScene` at boot so the seed is never displayed. Metres: width_m = `width·mpu·2`, front_m = `length·mpu`, rear_m = `rear·mpu`, height_m = `height·mpu`, lower_m = `lower·mpu` (`:771-776`); inverse from typed metres: `mpu = max(0.01, width_m/2)`, ratios = metres / mpu, `width` always 1 (`:694-722`).

Data sources:
- `get_state` (`app.js:284`) and Tauri event `state:snapshot_ready` (`tauri-bridge.js:141`) payload `roomRatio: { width, length, height, rear, lower, centerBlend, scaleM }` (`src-tauri/src/app_state.rs:53-65`, `RoomRatio`; `scaleM` default 1.0) — fed on the Rust side by `OscEvent::StateRenderer { value }` (`/omniphony/state/renderer`, JSON, key `roomRatio`, `osc_listener.rs:126-152,629-630`). `applyRoomRatio` (`room-geometry.js:1018-1073`) copies it, sets `metersPerUnit = scaleM` if > 0, then repositions every source/speaker mesh through `normalizedOmniphonyToScenePosition`.
- Local edits send `control_room_ratio {width,length,height}`, `control_room_ratio_rear {value}`, `control_room_ratio_lower {value}`, `control_room_ratio_center_blend {value}`, `control_layout_radius_m {value}` (`room-geometry.js:599-603`). Editing is blocked while `app.renderBackendState.frozenRoomRatio` (`state.js:581-583`). While typing, `previewRoomGeometryScene` (`:1006-1012`) redraws box+guides from uncommitted values without refitting the screen.

### 6.2 Elements (`setup.js:272-353`, scaled in `room-geometry.js:975-991`)

| Element | Geometry | Material | Transform |
|---|---|---|---|
| `room` fill | `BoxGeometry(2,1,2)` | `MeshBasicMaterial #4d6eff, transparent, opacity 0.08, depthWrite false` (depthTest on), renderOrder 0 | `scale (depthHalfX, totalHeight, halfZ)`, `position (xCenter, yCenter, 0)` |
| `roomEdges` | `EdgesGeometry(box)` = 12 edges, `LineSegments` | `LineBasicMaterial #6f8dff, opacity 0.45, depthTest false` (linewidth 2 ignored → 1 px) | same as fill |
| `roomFaces.posX` (front wall, +depth) | `PlaneGeometry(2,1)`, `rotation.y = −π/2` | `roomFaceMaterial` shared: `#233047, opacity 0.18, DoubleSide, depthWrite false, depthTest false`, renderOrder 1 | `position (xMax, yCenter, 0)`, `scale (halfZ, totalHeight, 1)` |
| `negX` (rear wall) | plane 2×1, `rotation.y = +π/2` | same | `(xMin, yCenter, 0)`, `scale (halfZ, totalHeight, 1)` |
| `posY` (ceiling) | `PlaneGeometry(2,2)`, `rotation.x = −π/2` | same | `(xCenter, yMax, 0)`, `scale (depthHalfX, halfZ, 1)` |
| `negY` (floor) | plane 2×2, `rotation.x = +π/2` | same | `(xCenter, yMin, 0)`, `scale (depthHalfX, halfZ, 1)` |
| `posZ` (right wall) | plane 2×1, no rotation | same | `(xCenter, yCenter, halfZ)`, `scale (depthHalfX, totalHeight, 1)` |
| `negZ` (left wall) | plane 2×1, `rotation.y = π` | same | `(xCenter, yCenter, −halfZ)`, `scale (depthHalfX, totalHeight, 1)` |

There is **no floor grid**; the only grid is the optional VBAP face grid (§11.1). The "Grid" checkbox (`display.grid`, help text `help.display.grid`: "Draw the reference grid / room box…") only toggles those face grids; the box/faces/edges are always drawn.

### 6.3 Per-frame face culling (`speakers.js:1884-1912`, `setup.js:355-362`)

Each frame: `camLocal = roomGroup.worldToLocal(camera.position)`; for each face with inward normal `n` (`posX: (−1,0,0)`, `negX: (1,0,0)`, `posY: (0,−1,0)`, `negY: (0,1,0)`, `posZ: (0,0,−1)`, `negZ: (0,0,1)`): `visible = dot(n, camLocal − face.position) > 0`. Result: the walls on the **far** side of the camera are drawn, near walls (between camera and room) are hidden; from inside the room all six are drawn. `syncVbapCartesianFaceGridVisibility` follows (§11.1). `screenMaterial.opacity = isInside ? 0.18 : 0.18` (`:1910`) is a no-op.

### 6.4 Screen plane (`setup.js:376-431`)

`PlaneGeometry(2, 1.125)` (16:9), `screenMaterial`: `#ffffff, opacity 0.18, DoubleSide, depthWrite false, depthTest false`, renderOrder 5, `rotation.y = −π/2` (plane normal → −X, i.e. facing the listener), always visible (no toggle). `fitScreenToUpperHalf()` (`:406-429`, run at init and after each committed ratio change with `refit=true`):
```
availW = max(0.01, zMax − zMin); availH = max(0.01, yMax − yMin)
h = 1; w = h·16/9
if h > availH: h = availH; w = h·16/9
if w > 2:      w = 2;      h = w·9/16
if w > availW: w = availW; h = w·9/16
scale = (w/2, h/1.125, 1)
position = (xMax − 0.005, yMin + availH/2, (zMin+zMax)/2)     ← vertically centred in the room despite the name
```

## 7. Room dimension guides (`room-geometry.js:830-944`)

Visible only when `app.roomGeometryExpanded` (`:822,943`; toggled by `#roomGeometryToggleBtn`, title i18n `room.title`, `listeners/room-geometry-listeners.js:20-23`; forced false at boot `app.js:246`; not persisted). Each guide = `LineSegments` (`LineBasicMaterial color, opacity 0.85, depthTest false`, renderOrder 30) + `createSmallLabelSprite('')` (renderOrder 31). Geometry per guide (`:863-875`): segment `start→end` plus two end ticks `±tick` where `tick = normalize(tickDir)·0.04`; label at `mid + tick·2.2` (0.088 along tickDir); text = metres, 2 decimals, suffix `m`. With `yTop = yMax + 0.06`, `off = 0.08`:

| Guide | Colour | start | end | tickDir | Label |
|---|---|---|---|---|---|
| width | `#88c7ff` | `(xMax+off, yTop, zMin)` | `(xMax+off, yTop, zMax)` | +X | `width·mpu·2` |
| front | `#a0ffd1` | `(0, yTop, zMax+off)` | `(xMax, yTop, zMax+off)` | +Z | `length·mpu` |
| rear | `#ffd08a` | `(xMin, yTop, zMax+off)` | `(0, yTop, zMax+off)` | +Z | `rear·mpu` |
| total | `#b8b8ff` | `(xMin, yTop, zMin−off)` | `(xMax, yTop, zMin−off)` | +Z | `(length+rear)·mpu` |
| height | `#ff9ed8` | `(xMax+off, 0, zMax+off)` | `(xMax+off, yMax, zMax+off)` | +X | `height·mpu` |
| lower | `#ff7a7a` | `(xMax+off, yMin, zMax+off)` | `(xMax+off, 0, zMax+off)` | +X | `lower·mpu` |
| totalHeight | `#ffb3e6` | `(xMax+off, yMin, zMin−off)` | `(xMax+off, yMax, zMin−off)` | +X | `(height+lower)·mpu` |

Rebuilt on every committed ratio change and on every keystroke preview (uses preview ratios/mpu when given).

## 8. Axes triad (`scene/axes.js`)

Constants (`:7-11`): `axisGap 0.3`, `axisExtent 0.58`, `arrowRadius 0.02`, `arrowLength 0.075`, `labelOffset 0.12`. Origin at scene (0,0,0), no parent transform. For each spec (`:12-16`):

| Label | Colour | Scene direction `dir` | Cone Euler | Meaning |
|---|---|---|---|---|
| **Y** | `#ff6b6b` (red) | `(1,0,0)` = scene X (depth/front) | `(0,0,−π/2)` | ADM y |
| **Z** | `#7fff7f` (green) | `(0,1,0)` = up | `(0,0,0)` | ADM z |
| **X** | `#6bb8ff` (blue) | `(0,0,1)` = scene Z (right) | `(π/2,0,0)` | ADM x |

Per axis: two `Line`s (`LineBasicMaterial color, opacity 0.85, depthTest false`, renderOrder 30): positive from `dir·0.3` to `dir·0.58`, negative from `dir·(−0.3)` to `dir·(−0.58)` (gap around the head); one `ConeGeometry(r 0.02, h 0.075, 14 segs)` (`MeshBasicMaterial color, opacity 0.92, depthTest false`, renderOrder 31) at `dir·(0.58 + 0.075·0.45) = dir·0.61375`, rotated so its +Y apex points along `dir` — **positive end only**; one small label sprite (§8.1) at `dir·(0.58+0.075+0.12) = dir·0.775`, renderOrder 32, colour = axis colour. Always visible, no toggle.

### 8.1 Small label sprite (used by axes, guides, gizmos) — `scene/labels.js:173-209`
`createSmallLabelSprite(text, color='#d9ecff')`: 128×64 canvas texture (`LinearFilter`, no mipmaps, sRGB), text centred, `700 28px sans-serif`, fill = colour; `SpriteMaterial { transparent, alphaTest 0.25, depthTest false, depthWrite false, toneMapped false }`; sprite world scale `(0.25, 0.12)`, `sizeAttenuation` true (perspective-scaled billboard), `frustumCulled false`, renderOrder 40 unless overridden. Multi-line text: 18 px, weight 700 first line / 600 others, line height 18 (`:139-171`). Large variant `createLabelSprite` = 256×96, scale (0.42, 0.16), 36 px. (Labels as a system belong to another scope; only the parameters used here are listed.)

## 9. Listener head (Dame de Brassempouy) and head pose

Model: `assets/la_dame_de_brassempouy_centered.glb` (1.35 MB, `setup.js:439`), loaded with `GLTFLoader` (`app.js:192-232`). Per mesh: shadows off, `frustumCulled false`, `material.roughness = min(0.92, roughness||0.92)`, `metalness = 0` (glTF `MeshStandardMaterial`, lit by §4). Uniform scale so the bounding-box max dimension = **0.34** scene units (`BRASSEMPOUY_TARGET_MAX_DIMENSION`, `setup.js:438`); `model.rotation.y = −π/2` (glTF forward → scene +X front); added to `headPoseGroup` at the origin. The asset is pre-centred; no positional offset is applied (the 0.25 orbit pivot height is independent). No other listener-position/orientation marker exists in this scope (the triad §8 doubles as the orientation reference; speaker "face listener" aiming is in `speakers.js`, out of scope).

Head pose (`scene/head-pose.js`):
- Data: (a) full state path — `payload.binaural` (`init.js:96` → `setHeadPoseTarget`; from `get_state`/`state:snapshot_ready`; Tauri `AppState.binaural` is the renderer domain's `binaural` JSON passthrough, `app_state.rs:424`, `osc_listener.rs:608-609`), fields used: `outputMode` (`'speaker' | 'binaural'`), `headPose: {w,x,y,z}` (world→head quaternion in ADM axes); (b) fast path — OSC `/omniphony/state/head_pose f f f f` = `(w, x, y, z)` → **`OscEvent::StateHeadPose { w, x, y, z }`** (`osc_parser.rs:267-268,758-763`) → Tauri event `binaural:head_pose` `{w,x,y,z}` coalesced into `state:batch` at ~60 Hz, latest wins (`osc_listener.rs:1758-1759,2585-2592`) → `setHeadPoseQuat` (`tauri-bridge.js:123`).
- Gate (`head-pose.js:33-46`): `trackingActive = outputMode === 'binaural' && pose numeric`; fast-path quats are ignored unless active (`:52-55`); when inactive the target is identity (head eases back to neutral).
- Mapping (`:45,54`): `q_scene = normalize( (x: −pose.y, y: −pose.z, z: −pose.x, w: pose.w) )` — conjugate (head-in-world) then the cyclic ADM→scene permutation (det +1, so a quaternion maps by permuting its vector part).
- Easing (`:26,58-61`): every rendered frame `headPoseGroup.quaternion = slerp(current, target, 0.4)`.

## 10. Coordinate maths (`coordinates.js`) ↔ `omniphony-geometry`

Golden vectors: `scripts/golden/geometry.json` (generated by `cargo run -p omniphony-geometry --example dump_golden_vectors`), replayed by `scripts/test-math.mjs` with tolerance 1e-6; cases: `toSpherical`, `fromSpherical`, `mapDepth` (+inverse), `normalizeDeg`, `snapDeg`, `roomScaledPosition` (+inverse). A Rust port can consume the same JSON directly.

| JS (`coordinates.js`) | Definition | Crate mirror (`lib.rs`) |
|---|---|---|
| `omniphonyToSceneCartesian` (:33) | `(ay, az, ax)` | `adm_to_scene` :88 |
| `sceneToOmniphonyCartesian` (:25) | `(sz, sx, sy)` | `scene_to_adm` :94 |
| `cartesianToSpherical(scene)` (:50-67) | `dist = ‖p‖`, `horiz = √(x²+z²)`, `az = atan2(z, x)°`, `el = horiz<1e-6 ? sign(y)·90 : atan2(y, horiz)°` | `to_spherical(ax,ay,az)` :108 (`az = atan2(x, y)` in ADM — identical after swizzle) |
| `sphericalToCartesianDeg(az, el, d)` (:69-76) | scene `x = d cos el cos az`, `y = d sin el`, `z = d cos el sin az` | `from_spherical` :130 (ADM `(h sin az, h cos az, d sin el)`) |
| `normalizeAngleDeg` (:82-87) | wrap to [−180,180] by repeated ±360 | `normalize_deg` :152 |
| `snapAngleDeg(a, step, thr)` (:89-92) | `s = round(a/step)·step; |a−s| ≤ thr ? s : a` | `snap_deg` :182 |
| `depthWarpWithRatios(d, f, r, blend=0.5)` (:106-122) | `d = clamp(d,−1,1); c = r + (f−r)·clamp(blend)`; `d≥0: t=d, a=c−f, b=2(f−c), y = a t³ + b t² + c t`; `d<0: t=−d, a=c−r, b=2(r−c), y = −(a t³ + b t² + c t)` | `map_depth` :207 |
| `mapRoomDepth(x)` (:124) | `depthWarpWithRatios(x, length, rear, centerBlend)` | — |
| `mapRoomPosition(scene)` (:128-135) | `x → mapRoomDepth(x)`, `y → y≥0 ? y·height : y·lower`, `z → z·width` | (`room_scaled_position` :269 in ADM order) |
| `normalizedOmniphonyToScenePosition(adm)` (:180) | `mapRoomPosition(adm_to_scene(adm))` | `adm_to_scene(room_scaled_position(p, [width,length,height], rear, lower, blend))` |
| `inverseMapRoomDepth(m)` (:147-174) | front/rear floored at 0.001; 28-step bisection on [0,1] or [−1,0] of the warp toward `clamp(m, 0, front)` / `clamp(m, −rear, 0)` | `inverse_map_depth` :232 (identical iteration count) |
| `scenePositionToNormalizedOmniphony(scene)` (:235-249) | `x → inverseMapRoomDepth`, `y → y/(height or lower, floored 0.001)`, `z → z/width`; swizzle to ADM; clamp [−1,1]; snap each component to {−1,0,1} if within 1e-5 (`GRID_SNAP_TOLERANCE`, :225-233) | `inverse_room_scaled_position` :290 (floors at `MIN_ROOM_RATIO` 0.01, **no** cardinal snap) |
| `metersPerUnit()` (:194) | `max(0.001, app.metersPerUnit||1)` | — |
| `normalizedToMeters(adm)` (:199-203) | `scene_to_adm(normalizedOmniphonyToScenePosition(adm)·mpu)` (ADM-axis metres) | — |
| `metersToSceneUnits(m)` (:207-211) | `adm_to_scene(m)/mpu` | — |
| `hydrateObjectCoordinateState` / `hydrateSpeakerCoordinateState` (:274-338) | cartesian mode: clamp xyz, `scene = normalizedOmniphonyToScenePosition`, `az/el/dist = cartesianToSpherical(scene)` (dist floored 0.01); polar mode: `scene = sphericalToCartesianDeg`, `xyz = scenePositionToNormalizedOmniphony(scene)` | `hydrate_from_cartesian` :480 / `hydrate_from_spherical` :491 operate on **raw ADM without room scaling** — the JS versions go through the room warp, so az/el/dist differ whenever ratios ≠ 1 (not covered by the golden vectors). Flagging, not resolving. |
| `getObjectCoordMode` (:259) | explicit `coordMode`, else polar if any of `azimuthDeg/elevationDeg/distanceM` finite, else cartesian | — |
| `getSpeakerCoordMode` (:255) | `'cartesian'` iff `coordMode` lower-cases to it, else `'polar'` | — |
| `getSpeakerBaseOpacity` (:409) | `spatialize===0 ? 0.3 : 0.65` (visual param consumed by speaker meshes, out of scope) | — |

`decomposePosition`/`formatPosition`/`formatNumber` (:14-19, :344-403) are text formatting only (ADM `az = atan2(x,y)`, `el = atan2(z, √(x²+y²))`).

## 11. Gizmos (`scene/gizmos.js`, driven by `speakers.js:1717-1853` and `picking.js`)

### 11.1 VBAP cartesian face grids (`gizmos.js:11-154`)
Six `LineSegments` (one per room face) sharing `LineBasicMaterial #66d8ff, opacity 0.42, depthWrite false, depthTest false`, renderOrder 6, parented to `roomGroup`. Visible iff `app.vbapCartesianFaceGridEnabled && geometry non-empty && roomFaces[key].visible` (`:54-59`, re-evaluated every frame from §6.3). Rebuilt (`:61-154`) when the toggle changes, sizes change, or the room ratio changes (`room-geometry.js:995`); hidden if any of `xSize, ySize, zSize` is non-finite or < 2.

Node positions (sizes are interval counts; nodes = n+1, mirrors renderer `live_params.rs`):
```
xs (scene x, depth)  = axisValues(−1, 1, ySize+1) mapped by mapRoomPosition({x}).x   (depth warp; ADM y ← ySize)
ys (scene y, height) = [axisValues(yMin, 0, zNegSize+1) without its last] ++ axisValues(0, yMax, zSize+1)   (ADM z)
zs (scene z, width)  = axisValues(zMin, zMax, xSize+1)                                                     (ADM x ← xSize)
axisValues(min,max,n) = min + (max−min)·i/(n−1), i∈[0,n)        (crate evenly_spaced_axis :322; ys mirrors cartesian_z_axis :343)
```
Lines: `posX/negX` at `x = xMax/xMin`: for each y a z-span line, for each z a y-span line; `posY/negY` at `y = yMax/yMin`: per x a z-span, per z an x-span; `posZ/negZ` at `z = zMax/zMin`: per x a y-span, per y an x-span (`:120-145`).
Data: `app.vbapCartesianState.{xSize,ySize,zSize,zNegSize}` from `payload.vbapCartesian` (`init.js:156-165`; renderer-domain JSON `vbapCartesian` → `app_state.rs:112-121`), Tauri events `render_evaluation:cartesian:{x_size,y_size,z_size,z_neg_size}` `{value}` (`tauri-bridge.js:507-527`), and panel inputs (`renderer-panel-listeners.js:97-138`). Toggle: checkbox `#vbapCartesianGridToggleBtn` (`index.html:278`, label i18n `display.grid`), mirrored by scene-fx bar button `fxGridBtn` (`scene-effects-bar.js:17`); default **false** (`state.js:148`); **not persisted**.

### 11.2 Polar gizmo: ring + arc + ticks + labels (`gizmos.js:166-263, 407-418`; layout `speakers.js:1748-1819`)
Shown when `app.activeEditMode === 'polar' && app.polarEditArmed && resolveEditTarget() !== null`. Target (`speakers.js:1699-1715`): selected speaker mesh, or the selected source mesh if it is a **virtual-bed channel** (`canonicalChannelName(name) && channelPlacement(name) === 'virtual'`). All parts are children of `scene` at the origin (the listener), rebuilt per call of `updateSpeakerGizmo` (on selection/edit-mode change and every drag move).

Let `(az, el, dist) = cartesianToSpherical(mesh.position)`, `D = max(0.01, dist)` (stored in `app.dragAzimuthDeg/dragElevationDeg/dragDistance`), `azRad = az·π/180`.

| Part | Geometry (unit) | Material | Transform |
|---|---|---|---|
| `ring` | `LineLoop`, 64 pts `(cos a, 0, sin a)` | `#9ef7ff` α 0.6 | `scale (D, 1, D)` |
| `ringTicks` (5°) | 72 segments radial 1.00→1.08 in XZ | `#9ef7ff` α 0.5 | `scale (D,1,D)`; visible `!dragging || dragAzimuthDelta > 0.1` |
| `ringMinorTicks` (1°) | 360 segments 1.01→1.05 | `#9ef7ff` α 0.35 | visible `dragging && 0 ≤ dragAzimuthDelta ≤ 0.1` |
| `ringLabels` | 24 small sprites `−180,−165,…,165` (default colour `#d9ecff`) at `(cos a·1.1, 0.02, sin a·1.1)` | | `scale (D,1,D)` |
| `ringCurrentLabel` | sprite `#9ef7ff`, text `az.toFixed(1)` (normalised), renderOrder 5, at `(cos·1.24, 0.04, sin·1.24)` | | group `scale (D,1,D)` |
| `arc` | `LineLoop`, 48 pts `(cos t, sin t, 0)`, `t ∈ [−π/2, π/2]` (loop closes with a chord) | `#ffd27a` α 0.75 | `scale (D,D,D)`, `rotation.y = −azRad` (arc lies in the vertical plane through the azimuth) |
| `arcTicks` (5°) | segments 1.00→1.08 for `−90..90` step 5 | `#ffd27a` α 0.55 | same; visible `!dragging || dragElevationDelta > 0.1` |
| `arcMinorTicks` (1°) | 181 segments 1.01→1.05 | `#ffd27a` α 0.38 | visible `dragging && 0 ≤ dragElevationDelta ≤ 0.1` |
| `arcLabels` | 13 sprites `−90..90` step 15 at `(cos·1.1, sin·1.1, 0)` | | `scale (D,D,D)`, `rotation.y = −azRad` |
| `arcCurrentLabel` | sprite `#ffd27a`, `el.toFixed(1)`, at `(cos·1.24, sin·1.24, 0)` | | same |

All line materials: `transparent:true`, depthTest/depthWrite default (on), renderOrder 0.

### 11.3 Distance gizmo (`gizmos.js:269-293`; `speakers.js:1820-1838`)
Visible with the polar gizmo. `line` origin→`mesh.position` (`#a8ffbf` α 0.7); two cones `ConeGeometry(0.02, 0.06, 8)` (`MeshBasicMaterial #a8ffbf α 0.7`, renderOrder 5): `arrowA` at `dir·0.1` oriented `+Y → dir`, `arrowB` at `pos − dir·0.1` oriented `+Y → −dir` (`dir = normalize(pos)` or `(1,0,0)` if ‖pos‖ < 1e-6; orientation via shortest-arc quaternion `setFromUnitVectors`); label sprite (`#7bff6a`, renderOrder 5) at `pos/2 + (0, 0.08, 0)` with text `‖pos‖.toFixed(2)` (scene units).

### 11.4 Cartesian gizmo (`gizmos.js:361-401`; `speakers.js:1841-1852`)
Shown when `activeEditMode === 'cartesian' && cartesianEditArmed && target`. Group at `mesh.position`, uniform `scale = max(0.2, distance(camera, mesh)·0.08)` (screen-size-ish, recomputed on `updateSpeakerGizmo` — **not per frame**, so it lags camera moves until the next selection/drag update). Three lines from origin to `0.45` along local X/Y/Z (`#ff6b6b / #7fff7f / #6bb8ff`, α 0.85 — same colours as the triad, i.e. red = scene X/depth, green = up, blue = scene Z/right) and three handle spheres `SphereGeometry(0.045, 16, 16)` (`MeshBasicMaterial`, α 0.95) at the line ends, `userData.axis = 'x'|'y'|'z'` (scene axes).

### 11.5 Face shadows (`gizmos.js:299-355`; `speakers.js:1914-1999`)
Two sets of six `CircleGeometry(1, 24)` discs (`MeshBasicMaterial #000000, DoubleSide, depthWrite false, depthTest false`, renderOrder 3), one set for the selected speaker (`speakerMeshes[selectedSpeakerIndex]`), one for the selected source (`sourceMeshes.get(selectedSourceId)`) — selecting one deselects the other, so at most one set is visible. Rotations: `posX: rot.y=+π/2`, `negX: −π/2`, `posY: rot.x=−π/2`, `negY: +π/2`, `posZ: rot.y=π`, `negZ: 0`. Updated **every frame** (`app.js:336-337`): with `p = mesh.position`, `c = clamp(p, roomBounds)`, `eps = 0.01`, `span_k = max(1e-6, kMax − kMin)`:
```
posX: at (xMax−eps, c.y, c.z), dist = |xMax − p.x|, maxDist = spanX      (negX: xMin+eps; posY/negY on y; posZ/negZ on z)
t = maxDist > 1e-6 ? clamp(1 − dist/maxDist, 0.08, 1) : 1
scale = 0.08·(0.7 + 0.6·t)      opacity = 0.06 + 0.18·t
```

## 12. Hybrid iso-distance shape (`scene/hybrid-distance.js`; trigger `controls/hybrid-curve.js:262-285`)

Shown iff `app.renderBackendState.selection === 'hybrid'` and a blend-curve point is selected (UI state in `hybrid-curve.js`). Spec: `shape = hybrid.metric === 'spherical' ? 'sphere' : 'cube'`, `radius = curve[selected][0] · (spherical ? √3 : 1)` (ADM distance; hidden if ≤ 1e-4). Geometry: `SphereGeometry(1, 40, 28)` or `BoxGeometry(2,2,2)`; base vertices are kept as ADM coordinates and re-deformed on every spec or room-ratio change (`:68-100`):
```
for each vertex v (ADM unit shape): o = v·radius
scene.x = depthWarpWithRatios(o.y, length, rear, centerBlend)
scene.y = o.z ≥ 0 ? o.z·height : o.z·lower
scene.z = o.x·width
```
Material `MeshBasicMaterial #ffd166, transparent, opacity 0.14, depthWrite false, DoubleSide`, renderOrder 3, `frustumCulled false`. Data: `renderBackendState.selection`, `renderBackendState.hybrid.metric` (renderer domain `renderBackendState`, `osc_listener.rs:147`), the curve/selected index are Studio-local UI state.

## 13. Interaction (`picking.js`, `input.js:55-65`, `scene/materials.js:51-53`)

Raycaster: `params.Line.threshold = 0.08` (world units, divided by mean object scale internally). Pointer events are bound on the canvas (`:99-104`); wheel with `capture:true, passive:false`.

| Input | Behaviour | Ref |
|---|---|---|
| Left press | record `pointerDownPosition`; if an edit gizmo is armed and the ray hits `ring`/`arc` (polar) or a handle sphere (cartesian) → start drag, `controls.enabled=false`, pointer captured by id | `:15-20, 248-301` |
| Left release with ≤ 6 px movement | raycast pickables (`sourceLabels, sourceMeshes, sourceOutlines, speakerMeshes, speakerLabels, speakerBandBars`, visible only): first speaker hit → `setSelectedSpeaker(idx)`; first object with `userData.sourceId` → `setSelectedSource(id)`; nothing → deselect both (also clears `polarEditArmed/cartesianEditArmed`, `speakers.js:1857-1858`) | `:22-43, 116-192` |
| Azimuth drag (ring) | ray ∩ plane `y=0` → `az = normalize(atan2(hit.z, hit.x)°)`; `delta = (√(hit.x²+hit.z²) − D)/D` → `dragAzimuthDelta`; snap: `0≤delta≤0.1` → `snapAngleDeg(az, 1, 0.5)`; `delta>0.1` → `snapAngleDeg(az, 5, 2.5)`; `delta<0` (inside the ring) → no snap | `:311-325` |
| Elevation drag (arc) | plane through origin with normal `cross((cos az, 0, sin az), up)`; `el = clamp(atan2(hit.y, √(hit.x²+hit.z²))°, −90, 90)`; `delta = (‖hit‖ − D)/D` → same snapping | `:326-344` |
| Polar drag result | `pos = sphericalToCartesianDeg(az, el, D)`; mesh (and label at `+0.12` y) moved; `commitTargetScene(send=false)` | `:358-363` |
| Cartesian drag (handle) | `t = projectRayOntoAxis(rayO, rayD, axisOrigin=mesh.pos, axisDir)` = parameter of the closest point on the axis line to the ray (`input.js:55-65`: `w0 = O_axis − O_ray; b = a·d; den = 1 − b²; t = (b·(d·w0) − a·w0)/den`, 0 if `|den|<1e-6`); `pos = start + axisDir·(t − t0)`; commit `send=false` | `:281-296, 345-356` |
| Release / cancel / leave | `controls.enabled=true`; `commitTargetScene(final, send=true)`; channels keep an editor pin for 600 ms so stale stream packets do not snap back | `:366-390` |
| Ctrl/Shift + wheel (polar armed) | `D ← clamp(D − sign(deltaY)·step, 0.2, 2.0)`, `step = Shift ? 0.01 : 0.05`; commit with `send = (target.kind === 'channel')`; orbit zoom suppressed for that event | `:59-84` |
| Right drag | lens-shift pan (§2.3) | `setup.js:151-195` |
| Left drag (no gizmo hit) / middle drag / wheel | OrbitControls rotate / dolly / dolly | §2.2 |
| Escape / other keys | no effect on the 3D view (only modals/flyouts/curve editor) | `modal-and-toggle-listeners.js:317`, `scene-effects-bar.js:158`, `hybrid-curve.js:249` |
| Cursor | no cursor changes in this scope | — |

Commit path `commitTargetScene(target, x, y, z, send)` (`:197-225`):
- **speaker** → `applySpeakerSceneCartesianEdit(index, x, y, z, send)` (`speakers.js:1096-1134`; no-op when `renderBackendState.frozenSpeakers`): `xyz_adm = scenePositionToNormalizedOmniphony(scene)`, `az/el/dist = cartesianToSpherical(scene)` (dist in scene units, i.e. room-scaled), stored on the speaker; if `send`: Tauri `control_layout_config { payload: { speakerEdits: [{ id: index, coordMode, x, y, z }] } }` when `coordMode==='cartesian'`, else `{ id, coordMode:'polar', azimuth, elevation, distance }` — only the block matching the mode is sent — followed by `control_layout_config_apply` (`speakers.js:142-159`).
- **channel** (virtual bed) → mesh/label moved, `channelEditPinPos` updated, `updateSourceDecorations`, `updateSpeakerGizmo`, `previewChannelEditorFromScene`; if `send`: `applyChannelPolar(name, az, el, max(0.01, dist))` (`controls/virtual-bed.js:321-335`) → `commitChannel` → Tauri `control_virtual_bed { value: JSON.stringify(app.virtualBed) }` (`:268-279`, whole bed payload).

Edit-mode state (`state.js:414-416`, defaults `polarEditArmed=false, cartesianEditArmed=false, activeEditMode='polar'`), toggled by the "3D Edit" buttons `#speakerEditPolarGizmoBtn` / `#speakerEditCartesianGizmoBtn` (`index.html:847,870`; `listeners/speaker-editor-listeners.js:55-66,98-110`) and `#channelEditPolarGizmoBtn` / `#channelEditCartesianGizmoBtn` (`index.html:746,769`; `listeners/channel-editor-listeners.js:142-162`), i18n `speaker.edit3d`; arming one disarms the other. `#editModeSelect` is read (`speaker-editor-listeners.js:17,46-52`) but has no markup in `index.html` (apparently dead). Not persisted.

## 14. Update cadence

| What | When |
|---|---|
| `controls.update()` (damping), `updateHeadPose()` (slerp 0.4), `updateRoomFaceVisibility()` (+ grid visibility), speaker/object face shadows | every animation frame (`app.js:332-359`) |
| Camera aspect / size / pixel ratio / view offset | `window.resize` |
| Pan view offset | each right-drag `pointermove` |
| Polar/cartesian/distance gizmo layout | `updateSpeakerGizmo` on selection change, arm toggle, every drag move, wheel-distance, `applySpeakerSceneCartesianEdit` |
| Room box / faces / edges / screen / guides / grid / hybrid re-deform | `applyRoomRatioToScene` — on state snapshot, committed edit, and keystroke preview |
| Head pose target | 10 Hz full state + ~30 Hz OSC fast channel (batched ≤ 60 Hz) |
| VBAP grid geometry | toggle, size events, room ratio |

## 15. Shaders

No custom GLSL in this scope. Required native pipelines (all colour = sRGB hex, alpha = opacity, normal blending unless noted):
1. **Unlit triangle** (`MeshBasicMaterial`): uniforms `color: vec3, opacity: f32`, per-object model matrix; variants: depthTest on/off, depthWrite off, cull none (`DoubleSide`) — used by room fill, faces, screen, shadows, hybrid shape, cones, handle spheres. Fragment: `out = vec4(color, opacity)`.
2. **1 px line list / line loop** (`LineBasicMaterial`): same uniforms; depthTest on (gizmo) or off (room edges, axes, guides, grids).
3. **Billboard sprite with texture, `alphaTest 0.25`, depth off** for the small labels (world size 0.25×0.12 at unit distance, perspective-attenuated). Text rasterisation: 700 28px sans-serif on 128×64, centred.
4. **Lit PBR** for the glTF head only (metalness 0, roughness ≤ 0.92) under the §4 light rig (three r165 `MeshStandardMaterial` = Cook-Torrance GGX + Lambert, physical light units).
5. Projection: `perspective(fov_y = 65°, aspect, 0.1, 100)` then the clip-space translation of §2.3.

## 16. Defaults and toggles

| Element | Default | Toggle / control | Persisted |
|---|---|---|---|
| Room box fill, edges, faces, screen | always on | none | — |
| Face culling by camera side | always | none | — |
| Room dimension guides | off (`app.js:246`) | `#roomGeometryToggleBtn` (▸/▾, i18n `room.title`) → `app.roomGeometryExpanded` | no |
| Axes triad | always on | none | — |
| Head model + pose | model always; pose only when `binaural.outputMode === 'binaural'` (select `#outputModeSelect`, out of scope) | — | renderer state |
| VBAP face grid | off | `#vbapCartesianGridToggleBtn` (`display.grid`) / `fxGridBtn` | no |
| Polar / cartesian gizmo | off | "3D Edit" buttons (§13) | no |
| Face shadows | on whenever a speaker/source is selected | selection | — |
| Hybrid iso shape | off | hybrid backend + selected curve point | no |
| Pan offset | 0 | right drag; `resetPan` unused | no |
| `app.roomRatio`, `metersPerUnit` | `{1,2,1,1,0.5,0.5}`, 1.0 | Room Geometry panel inputs; renderer state | renderer config |

## 17. Not viewport elements (pure UI / plumbing in these files) — schedule separately

- `coordinates.js`: `formatNumber`, `decomposePosition`, `formatPosition` (text formatting for editors/tooltips).
- `controls/room-geometry.js`: everything except §6.1/§7 — panel inputs & summary (`renderRoomRatioDisplay`, `renderRoomGeometrySummary`, `computeRoomGeometryFromInputs`, `applyRoomGeometryNow`, `scheduleRoomGeometryApply`, baseline/dirty tracking, frozen-state handling `refreshRoomGeometryInputState`), collapse state `setRoomGeometryExpanded` (DOM part), and unrelated display-prefs persistence: trails (`persistTrailPrefs/loadTrailPrefs`, localStorage `spatialviz.trail_prefs`) and effective-render/energy-volume prefs (`spatialviz.effective_render_prefs`), gradient stop sanitising.
- `scene/gizmos.js:156-160` `renderVbapCartesianGridToggle` (checkbox sync).
- `core/render/render-surface-controller.js:35-92` rect/invariant diagnostics; `scene/setup.js:199-228` WebGL context rebuild; `scene/labels.js` debug stats (`window.omniphonyDebug`).
- `app.js` HMR reload hooks (`:369-380`).

## 18. Unclear / not derivable from the code

- The glTF's own origin vs. the 0.25 pivot: the asset is "centered" but its actual bounds/centre are only known at load time (`brassempouyBounds` is computed but not used for positioning).
- Whether the JS hydrate (room-scaled az/el/dist) or the crate hydrate (raw ADM) is the intended contract — they disagree for non-unit ratios (§10).
- `#editModeSelect` has no markup; treated as dead.
- `screenMaterial.opacity` inside/outside branch is identical (no visible state change).
