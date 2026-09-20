# Viewport parity spec — speakers, speaker gauges, per-speaker heatmaps, gain-table transport

Scope: `omniphony-studio/src/speakers.js`, `src/scene/speaker-gaintable.js`, `src/scene/speaker-band-bars.js`, `src/scene/speaker-band-select.js`, `src/scene/speaker-solo-volume.js`, `src/scene/discontinuity-volume.js`, plus the OSC/Tauri path in `src-tauri/src/osc_parser.rs` / `osc_listener.rs` / `commands/diag.rs` and the renderer-side encoder (`omniphony-renderer/renderer/src/band_gaintable.rs`, `runtime_control/src/osc.rs`, `orender_engine/src/osc/{dispatch,gaintable}.rs`). All paths below are relative to `omniphony-studio/` unless prefixed with `omniphony-renderer/`. Line numbers are from the integration tree on 2026-09-10.

Helpers that live in other files but that these visuals cannot be reproduced without (`scene/materials.js`, `sources.js` `applySpeakerLevel`/`updateSpeakerColorsFromSelection`, `coordinates.js`, `scene/labels.js`, `scene/gizmos.js`, `scene/energy-volume-core.js`, `scene/object-energy-shared.js`) are quoted with references; their own specs in this directory are authoritative for anything not stated here.

---

## 0. Conventions

### 0.1 Coordinate frames

| Frame | Axes | Where |
|---|---|---|
| Omniphony/ADM normalised | `x` left(−1)/right(+1), `y` rear(−1)/front(+1), `z` down(−1)/up(+1); all clamped to [−1, 1] | layout JSON, gain-table grid, OSC |
| Scene (three.js, Y-up) | `scene.x = adm.y` (depth, +x = front/screen), `scene.y = adm.z` (height), `scene.z = adm.x` (width, +z = right) | `coordinates.js:33-39` (`omniphonyToSceneCartesian`), crate `omniphony_geometry::adm_to_scene` (`omniphony-renderer/omniphony-geometry/src/lib.rs:88`) |

Room-ratio warp (`coordinates.js:106-135`, crate `map_depth` lib.rs:207 / `room_scaled_position` lib.rs:269):

```
mapRoomPosition(raw):                       // raw = scene-swizzled ADM
  x = depthWarp(raw.x, length, rear, centerBlend)
  y = raw.y >= 0 ? raw.y * height : raw.y * lower
  z = raw.z * width
depthWarp(d, f, r, blend):                  // d in [-1,1]
  center = r + (f - r) * blend
  d >= 0: t = d;  a = center - f; b = 2(f - center); return  a t³ + b t² + center t
  d <  0: t = -d; a = center - r; b = 2(r - center); return -(a t³ + b t² + center t)
```

`app.roomRatio = { width:1, length:2, height:1, rear:1, lower:0.5, centerBlend:0.5 }` by default (`state.js:107`). Speaker scene position = `normalizedOmniphonyToScenePosition(speaker)` = `mapRoomPosition(omniphonyToSceneCartesian(speaker))` (`coordinates.js:180-183`). Inverse: `scenePositionToNormalizedOmniphony` (`coordinates.js:235-249`) with a 28-iteration bisection for depth and snapping of |v−{−1,0,1}| < 1e-5 to the cardinal value.

Spherical (scene frame, `coordinates.js:50-76`; crate `to_spherical`/`from_spherical` lib.rs:108/130):
`az = atan2(scene.z, scene.x)` deg (0° = front, +90° = right), `el = atan2(y, √(x²+z²))` deg with exact ±90 when horizontal < 1e-6, `dist = |p|`. `sphericalToCartesianDeg(az, el, d) = (d cos el cos az, d sin el, d cos el sin az)`. `normalizeAngleDeg` wraps to (−180, 180]; `snapAngleDeg(a, step, thr)` rounds to `step` when within `thr`.

Speaker coordinate hydration (`coordinates.js:307-338`): `coordMode === 'cartesian'` → clamp x,y,z to [−1,1], derive az/el/distanceM from the warped scene position; otherwise (`'polar'`, default) → scene = `sphericalToCartesianDeg(az, el, max(0.01, distanceM))`, then x,y,z = inverse-warp (clamped to the room cube). The mesh is always placed at `warp(x, y, z)`, so a polar speaker beyond the room cube is clamped onto it. `distanceM` is in scene units (room half-width = 1), despite the name.

### 0.2 Colour management / lighting context (from `scene/setup.js`, three r0.165)

three r165 has `ColorManagement.enabled = true`: every hex/`setStyle` colour below is **sRGB** and is converted to linear before shading; the default output colour space is sRGB. `RawShaderMaterial` (the volumes) bypasses that: what the fragment shader writes is displayed as-is. Sprite textures are tagged `SRGBColorSpace` and `toneMapped: false`. No tone mapping is configured in `setup.js` (grep: none). Scene background `0x0a0b10` (`setup.js:18`). Lights hitting the `MeshStandardMaterial` speaker cubes (`setup.js:234-245`): AmbientLight `0xffffff` ×0.24, DirectionalLight `0xfff7ea` ×2.35, DirectionalLight `0xb8d4ff` ×1.05 (rim), HemisphereLight sky `0xdcecff` / ground `0x0d0f14` ×0.12 (directions are in setup.js, not read here).

### 0.3 Update cadence (from `app.js:336-365`)

Per animation frame, in order: `controls.update()`, `updateHeadPose()`, `updateRoomFaceVisibility()`, `updateSelectedSpeakerFaceShadows()`, `updateSelectedObjectFaceShadows()`, `decayTrails`, `decayMeters(now)`, `refreshObjectEnergyVolume`, `refreshSpeakerSoloVolume(now)`, `refreshGlobalEnergyVolume`, `refreshDiscontinuityVolume(now)`, then render. Everything else in this spec is event-driven.

---

## 1. Speaker cube (one per layout speaker)

Created in `renderLayout` (`speakers.js:2229-2377`, meshes at 2330-2368); destroyed in `clearSpeakers` (`sources.js:1373-1390`). Index = position in `layout.speakers`; `speakerMeshes[i]`, `speakerLabels[i]`, `speakerBandBars[i]` are parallel arrays (`state.js:53-56`).

### 1.1 Geometry / material

| Item | Value | Ref |
|---|---|---|
| Geometry | `BoxGeometry(0.08, 0.08, 0.08)` (`SPEAKER_BASE_SIZE = 0.08`), cloned per speaker | `materials.js:4,21`; `speakers.js:2331` |
| Material | `MeshStandardMaterial` cloned per speaker: `color 0x8ec8ff`, `emissive 0x10253a`, `transparent: true`, `opacity 0.65`; default roughness/metalness (1.0 / 0.0), depth test+write on, front side, normal blending, renderOrder 0 | `materials.js:22-27` |
| Driver disc (child) | `CircleGeometry(0.08 × 0.36 = 0.0288, 24)`, `MeshBasicMaterial color 0x0c1118, transparent, opacity 0.85`; shared geometry+material; local position `(0, 0, 0.04 + 0.0008)` on the cube's +Z face; `visible = app.speakerFaceListenerEnabled` (default false) | `materials.js:33-38`; `speakers.js:2338-2343, 199-205` |

### 1.2 Position and orientation

* Position: `warp(x, y, z)` (§0.1), set on creation (2332-2333) and on every edit (`updateSpeakerVisualsFromState`, 1057-1094).
* Orientation (`applySpeakerOrientation`, 199-217): identity unless `speakerFaceListenerEnabled`. When enabled and `|p|² ≥ 1e-8`: quaternion from `Matrix4.lookAt(eye = (0,0,0), target = p, up = (0,1,0))`, i.e. three.js semantics: `z = normalize(eye − target) = −p̂`, `x = normalize(up × z)`, `y = z × x` (if `|x| ≈ 0`, three.js nudges `z.x` (or `z.z` when `|up.z| = 1`) by 1e-4 and recomputes). Effect: the cube's local +Z (the driver face) points from the speaker toward the listener while local +Y stays world-up (no roll on elevated speakers). Cube on the origin → identity.

### 1.3 Scale (live level meter)

`applySpeakerLevel(mesh, meter)` (`sources.js:587-590`), applied on creation (2367), on every `speaker:meter` (2383-2397), on meter decay ticks (2439-2455) and when the size slider moves (`trails-and-display-listeners.js:245-248`):

```
dbfsToScale(dbfs, lo, hi) = lo + ((clamp(dbfs ?? -100, -100, 0) + 100) / 100) * (hi - lo)   // sources.js:568-572
sizeScale = clamp(app.speakerSize, 0.04, 0.2) / 0.08                                        // sources.js:263-265
mesh.scale = uniform( dbfsToScale(meter.rmsDbfs, 0.65, 2.2) * sizeScale )
```

Idle (no meter or −100 dBFS): 0.65 → 0.052 scene units wide; 0 dBFS: 2.2 → 0.176. The driver child inherits the scale. Uses **RMS**, not peak.

### 1.4 Colour

`updateSpeakerColorsFromSelection` (`sources.js:918-943`), called on selection changes, list re-render, layout render, spatialize edits and every `source:gains` update for the selected object (`sources.js:1268-1270`):

```
base  = mesh.userData.baseColor ?? 0x8ec8ff           // band colour, §1.6
mix   = clamp(sourceGains.get(selectedSourceId)?.[index] ?? 0, 0, 1)
color = lerp(base, 0xff3030, mix)                     // THREE.Color.lerp: per-channel in linear working space
if index === selectedSpeakerIndex: color = 0x4dff88   // selection overrides everything
```

`sourceGains` comes from `/omniphony/meter/object/{id}/gains [f g0 … gN-1]` → `OscEvent::MeterObjectGains { id, gains }` (each clamped 0..1, `osc_parser.rs:1076-1080`) → Tauri `source:gains` `{ id, gains }` (batched, §8.3).

### 1.5 Opacity

| Situation | Opacity | Ref |
|---|---|---|
| Base | `getSpeakerBaseOpacity`: `spatialize === 0 ? 0.3 : 0.65` (non-spatialised / direct feeds such as an LFE output) | `coordinates.js:409-411`; `speakers.js:2334-2336` |
| Binaural output (ghosted) | `base × 0.18`; labels 0.3 | `speakers.js:993-1012`; trigger `controls/binaural.js:423-429` when renderer state `outputMode === 'binaural'` |
| An object is selected | `mix ≤ 1e-6 ? min(base, 0.08) : base` — speakers the selected object does not feed fade to 0.08 | `sources.js:933-939` |
| No selected object | `base` | `sources.js:934-937` |

As coded: `updateSpeakerColorsFromSelection` writes `baseOpacity` without the ghost factor, so a ghosted speaker returns to full opacity on the next selection/gains update (`setSpeakersGhosted` early-returns when the state is unchanged). Port faithfully or fix deliberately.

### 1.6 Band base colour (crossover layouts)

`applySpeakerBandBaseColor` (`speakers.js:1046-1055`) stores `mesh.userData.baseColor = Color.setStyle(bandColor(speakerBandIndex(speaker, edges), bandCount))`, refreshed in `renderLayout` (2366) and every `renderSpeakersList` (1586):

```
edges   = crossoverBandEdges(app.currentLayoutCutoffs) = [0, ...cutoffs, Infinity]      // crossover-bands.js:20-25
bandCount = edges.length - 1
speakerBandIndex(spk, edges): lo = spk.freqLow > 0 ? spk.freqLow : 0; first i with |edges[i] - lo| < 0.1, else 0   // speaker-band-bars.js:49-56
bandColor(i, n): n <= 1 → '#8ec8ff' ; else hsl( round(8 + 248·i/(n-1)), 68%, 56% )      // speaker-band-bars.js:39-43 (red low → blue high)
```

`currentLayoutCutoffs` = `layout.crossoverCutoffs` (backend-derived interior cutoffs in Hz, `layouts.rs:58-77`, `speakers.js:278-280`), also refreshed by `speaker:spatialize` / `speaker:freq_low` / `speaker:freq_high` events which carry `crossoverCutoffs` (`osc_listener.rs:2479-2569`).

### 1.7 State summary

| State | 3D effect |
|---|---|
| selected (`app.selectedSpeakerIndex === i`) | colour `0x4dff88`; face shadows (§4); gizmo when armed (§5); list row highlight |
| hovered | none (no hover feedback on speakers) |
| muted / solo (`speakerMuted`, `getSoloTarget('speaker')`) | **none in 3D** — list buttons/classes + OSC only (`mute-solo.js`) |
| metering | uniform scale from RMS (§1.3) |
| selected object feeds it | lerp toward `0xff3030` by gain; unfed → opacity 0.08 (§1.4-1.5) |
| `spatialize === 0` | opacity 0.3 |
| binaural output | opacity ×0.18 |
| crossover band | base colour by band (§1.6) |
| "LFE" / height speakers | no dedicated visual: an LFE is just a speaker with `spatialize 0` (opacity 0.3) and/or the lowest band colour; height is only position |

---

## 2. Speaker label sprite

`createLabelSprite(text)` (`labels.js:203-205` → `createLabelSpriteBase(256, 96, 0.42, 0.16, '#ffffff', text)`, 173-201):

| Item | Value |
|---|---|
| Content | canvas 256×96, transparent background, text centred, `font: 700 36px sans-serif`, fill `#ffffff` (multi-line: 24px, line height 24, first line 700 others 600 — speakers are single-line) (`labels.js:139-170`) |
| Texture | `CanvasTexture`, Linear min/mag, no mipmaps, `SRGBColorSpace` (130-137) |
| Material | `SpriteMaterial { map, transparent, alphaTest 0.25, depthTest false, depthWrite false, toneMapped false }` |
| Sprite | scale `(0.42, 0.16, 1)` **world units** (size attenuates with distance, always camera-facing, centre 0.5/0.5), `frustumCulled false`, `renderOrder 40` |
| Position | speaker scene position + `(0, +0.12, 0)` (`speakers.js:2352, 1074`) |
| Text | `String(speaker.id ?? index)`; re-set on layout edits and `updateSpeakerLabelsFromSelection` (`labels.js:315-324`, no style change on selection) |
| Visible | `app.speakerLabelsEnabled` (default **false**) |
| Ghosted | `material.opacity = 0.3` |

The sprite is a pick target (§9). Note the egui spike renders labels as egui text; parity only needs: world-anchored, camera-facing, ~0.42×0.16 scene units, bold white, alpha-tested edges.

---

## 3. Speaker frequency-extent gauge ("band bar") — `scene/speaker-band-bars.js`

A billboard sprite per speaker showing its pass-band on a vertical log-frequency track.

| Item | Value | Ref |
|---|---|---|
| Canvas | 64×256 px | 20-21 |
| Sprite scale | `(0.055, 0.22, 1)` world units (aspect = canvas aspect) | 22-23, 149 |
| Material | `SpriteMaterial { map, transparent, depthTest false, depthWrite false, toneMapped false }`; texture Linear, no mipmaps, sRGB | 136-147 |
| renderOrder / culling | 39 / `frustumCulled false` | 150-151 |
| Position | speaker scene position + `(+0.11, 0, 0)` — i.e. offset along scene X (the **depth** axis, toward the front), `SPEAKER_BAND_BAR_OFFSET = 0.11` | `speakers.js:185, 1081, 2359` |
| Visible | `app.speakerBandBarsEnabled` (default **false**) | `speakers.js:2358` |
| Redraw | only when cache key `"${freqLow|0}|${freqHigh|0}#${bandIndex}/${bandCount}"` changes | 163-171 |

Canvas drawing (`drawBar`, 82-129), all in px on the 64×256 canvas:

```
W=64 H=256 padY=10 trackW=22 trackX=(W-trackW)/2=21 trackTop=10 trackH=236
logPos(f) = (ln(clamp(f,20,20000)) - ln 20) / (ln 20000 - ln 20)        // 0 bottom … 1 top
yFor(f)   = trackTop + (1 - logPos(f)) * trackH
lo = freqLow  > 0 ? freqLow  : 20 ; hi = freqHigh > 0 ? freqHigh : 20000     // passBand, 70-80
1. track:  roundRect(trackX, trackTop, trackW, trackH, r=8) fill rgba(16,22,30,0.82)
2. lit:    clip to the same rounded rect; fillRect(trackX+2, yFor(hi), trackW-4, max(2, yFor(lo)-yFor(hi))) with bandColor(bandIndex, bandCount) (§1.6)
3. border: stroke same rounded rect, lineWidth 2, rgba(255,255,255,0.28)
4. ticks:  for hz in [100, 1000, 10000]: line (trackX+3, yFor(hz)) → (trackX+trackW-3, yFor(hz)), lineWidth 1, rgba(255,255,255,0.22)
```

Colours are canvas (sRGB) values. The sprite is a pick target (§9). Everything else in the file (`bandColor`, `speakerBandIndex`) is shared with the list UI.

---

## 4. Selection face shadows (`speakers.js:1914-1999`, objects created in `gizmos.js:299-355`)

Six discs projected onto the room walls for the selected speaker (`selectedSpeakerShadows`) and, with identical maths, for the selected object (`selectedObjectShadows`, driven from `updateSelectedObjectFaceShadows` in the same file). Updated **every frame**.

| Item | Value |
|---|---|
| Geometry | `CircleGeometry(1, 24)` |
| Material | `MeshBasicMaterial { color 0x000000, transparent, opacity (set per frame), side DoubleSide, depthWrite false, depthTest false }`, `renderOrder 3` |
| Orientation | posX `rot.y = +π/2`, negX `rot.y = −π/2`, posY `rot.x = −π/2`, negY `rot.x = +π/2`, posZ `rot.y = π`, negZ identity (`gizmos.js:322-327`) |

Per frame, with `p` = selected mesh position, `roomBounds = {xMin,xMax,yMin,yMax,zMin,zMax}` (default `−1,1,−0.5,1,−1,1`, `setup.js:393-400`, mutated by the room-ratio code), `eps = 0.01`, `baseRadius = 0.08`, `span_a = max(1e-6, aMax − aMin)`, `c = clamp(p, bounds)`:

```
for each face (example posX): pos = (xMax - eps, c.y, c.z); dist = |xMax - p.x|; maxDist = spanX
  t = maxDist > 1e-6 ? clamp(1 - dist/maxDist, 0.08, 1) : 1
  visible = true; scale = uniform(0.08 * (0.7 + 0.6 t)); opacity = 0.06 + 0.18 t
negX: (xMin+eps, c.y, c.z) dist |xMin-p.x|; posY/negY on y with spanY; posZ/negZ on z with spanZ
```

All six hidden when nothing is selected. Blending normal (black at ≤ 0.24 alpha → a soft dark spot that grows/darkens as the speaker nears the wall).

---

## 5. Edit gizmos driven from `speakers.js` (`updateSpeakerGizmo`, 1717-1853; objects built in `gizmos.js:166-401`)

Event-driven (selection change, any position edit, drag frames, layout render, editor listeners) — **not** per frame; the cartesian gizmo's camera-distance scale is therefore stale until the next such event.

Target (`resolveEditTarget`, 1699-1715): the selected speaker; else the selected object if it is a *virtual* bed channel (`canonicalChannelName(name)` and `channelPlacement(name) === 'virtual'`). `polarActive = activeEditMode === 'polar' && target && polarEditArmed`; `cartesianActive = activeEditMode === 'cartesian' && target && cartesianEditArmed`. Defaults: `activeEditMode 'polar'`, both armed flags false (`state.js:414-416`); armed by the editor's gizmo buttons (`speakerEditPolarGizmoBtn` / `speakerEditCartesianGizmoBtn`, `speaker-editor-listeners.js`, not traced).

### 5.1 Polar gizmo (`polarActive`)

With `az, el, dist = cartesianToSpherical(mesh.position)`; `app.dragDistance = max(0.01, dist)` = `d`; `azRad = az·π/180`:

| Object | Geometry (unit) | Material | Transform |
|---|---|---|---|
| `ring` | `LineLoop` 64 points on unit circle in XZ | `LineBasicMaterial 0x9ef7ff, opacity 0.6` | pos 0, scale `(d, 1, d)` |
| `ringTicks` | `LineSegments` 72 radial ticks r 1.0→1.08 every 5° | `0x9ef7ff`, 0.5 | same; visible `!isDraggingSpeaker || dragAzimuthDelta > 0.1` |
| `ringMinorTicks` | 360 ticks r 1.01→1.05 every 1° | `0x9ef7ff`, 0.35 | same; visible `isDraggingSpeaker && 0 ≤ dragAzimuthDelta ≤ 0.1` |
| `ringLabels` | 24 small sprites, text `-180…165` step 15 | `createSmallLabelSprite` (128×64 canvas, scale (0.25,0.12), 28px bold, colour `#d9ecff`, renderOrder 40) | group scale `(d,1,d)`; child i at `(cos a·1.1, 0.02, sin a·1.1)`, a = angle_i |
| `ringCurrentLabel` | small sprite, text `az.toFixed(1)`, colour `#9ef7ff`, renderOrder 5 | | in `ringCurrent` group (scale (d,1,d)) at `(cos r·1.24, 0.04, sin r·1.24)`, r = normalizeAngleDeg(az) |
| `arc` | `LineLoop` 48 points, `t = i/47·π − π/2`, `(cos t, sin t, 0)` (a half circle in XY; LineLoop closes it with a vertical chord) | `0xffd27a`, 0.75 | pos 0, scale `(d,d,d)`, `rotation.y = −azRad` |
| `arcTicks` | ticks every 5° from −90..90, r 1.0→1.08 | `0xffd27a`, 0.55 | same; visible `!dragging || dragElevationDelta > 0.1` |
| `arcMinorTicks` | ticks every 1°, r 1.01→1.05 | `0xffd27a`, 0.38 | same; visible `dragging && 0 ≤ dragElevationDelta ≤ 0.1` |
| `arcLabels` | 13 small sprites `-90…90` step 15 | as ring labels | group scale (d,d,d), rot.y −azRad; child at `(cos a·1.1, sin a·1.1, 0)` |
| `arcCurrentLabel` | text `el.toFixed(1)`, colour `#ffd27a`, renderOrder 5 | | in `arcCurrent` (scale d, rot −azRad) at `(cos e·1.24, sin e·1.24, 0)` |
| `distanceGizmo.line` | `Line` origin → speaker position (geometry rewritten) | `0xa8ffbf`, 0.7 | world |
| `distanceGizmo.arrowA/B` | `ConeGeometry(0.02, 0.06, 8)` | `MeshBasicMaterial 0xa8ffbf`, 0.7, renderOrder 5 | A at `dir·0.1` oriented `quat(unitY → dir)`; B at `p − dir·0.1` oriented `quat(unitY → −dir)`; `dir = p̂` (or +X if `|p| ≤ 1e-6`) |
| `distanceGizmo.label` | small sprite, text `|p|.toFixed(2)`, colour `#7bff6a`, renderOrder 5 | | at `p/2 + (0, 0.08, 0)` |

All hidden when `!polarActive` (1724-1735).

### 5.2 Cartesian gizmo (`cartesianActive`)

`cartesianGizmo.group` at the mesh position, uniform scale `max(0.2, |camera − mesh| · 0.08)` (1847-1851). Children (`gizmos.js:361-401`): three `Line`s from origin to 0.45 on X/Y/Z (`LineBasicMaterial` `0xff6b6b` / `0x7fff7f` / `0x6bb8ff`, opacity 0.85) and three `SphereGeometry(0.045, 16, 16)` handles at 0.45 (`MeshBasicMaterial` same colours, opacity 0.95), `userData.axis = 'x'|'y'|'z'`. Hidden otherwise.

---

## 6. Room face visibility (`updateRoomFaceVisibility`, `speakers.js:1884-1912`) — per frame

For each `roomFaceDefs` entry `{ key, mesh, inward }` (`setup.js:355-361`: posX inward (−1,0,0), negX (1,0,0), posY (0,−1,0), negY (0,1,0), posZ (0,0,−1), negZ (0,0,1)): `camLocal = roomGroup.worldToLocal(camera.position)`; `mesh.visible = inward · (camLocal − mesh.position) > 0` — a wall is drawn only when the camera is on its inner side (the far walls show, the near ones are culled). Then `syncVbapCartesianFaceGridVisibility()` (grids follow their wall, `gizmos.js:54-59`). `screenMaterial.opacity = 0.18` in both branches (dead conditional, 1900-1911).

---

## 7. Level metering, decay, clip

* `/omniphony/meter/speaker/{index} [f peak_dbfs, f rms_dbfs]` → `OscEvent::MeterSpeaker { id: "{index}", peak_dbfs: clamp(−100, +24), rms_dbfs: clamp(−100, 0) }` (`osc_parser.rs:1063-1110`) → Tauri `speaker:meter` `{ id, meter: { peakDbfs, rmsDbfs, peakHoldDbfs } }` (`osc_listener.rs:2336-2349`), **batched** (§8.3). `peakHoldDbfs` (1 s hold, 120 dB/s decay, `peak_hold.rs`) is used only by the list meter.
* JS `updateSpeakerLevel(index, meter)` (`speakers.js:2383-2397`): stores `{peakDbfs, rmsDbfs}` in `speakerLevels`, `speakerLevelLastSeen = performance.now()`, re-applies the cube scale (§1.3), dirties the list meter.
* `decayMeters(now)` per frame (2410-2461): for every speaker whose last message is older than `METER_DECAY_START_MS = 250` ms, `peak, rms −= 45 dB/s · dt` (floor −100) (`state.js:625-626`), re-applying the cube scale. Same for objects.
* Clip: `/omniphony/state/clip [i speaker]` → `OscEvent::StateClip` → Tauri `clip:detected { speaker }` → `flashSpeakerClip` (779-804): **DOM only** (list name chip `.clip-flash` for 1 s). No 3D element.
* `/omniphony/meter/master` → `updateMasterLevel` (2401-2408): DOM only.

---

## 8. Gain-table transport (per-speaker / global fields) — `scene/speaker-gaintable.js` + Rust

### 8.1 OSC contract (`omniphony-renderer/runtime_control/src/osc_contract.rs:133-137, 396-402`)

| Direction | Address | Args | Meaning |
|---|---|---|---|
| Studio → renderer | `/omniphony/control/debug/speaker_gaintable/subscribe` | `[i have_version, i speaker_index]` | subscribe for one field; `have_version` = version already cached (0 = none, sent as `max(0)`); `speaker_index ≥ 0` = that speaker's slice, `−1` = global energy, `−2` = gain discontinuity, `−3` = centroid jump; any other negative is treated as `−1` by the renderer (`dispatch.rs:297-334`). Additive per client (one target per display). Tauri cmd `subscribe_speaker_gaintable(have_version, speaker_index)` (`commands/diag.rs:50-68`). |
| Studio → renderer | `/omniphony/control/debug/speaker_gaintable/unsubscribe` | none | last consumer released; client keeps its cache (`diag.rs:72-80`) |
| Studio → renderer | `/omniphony/control/debug/speaker_gaintable/nack` | `[i version, i idx…]` (≤ 256 indices per datagram) | re-request missing chunks (`osc_listener.rs:1526-1546`) |
| renderer → Studio | `/omniphony/state/debug/speaker_gaintable/meta` | `[s json]` `{"version":u32,"total_len":usize,"chunk_count":usize,"chunk_bytes":1024}` | starts a transfer (`osc.rs:213-225`) |
| renderer → Studio | `/omniphony/state/debug/speaker_gaintable/chunk` | `[b blob]` | `blob = version:u32 LE ‖ chunk_index:u32 LE ‖ artifact[idx·1024 .. min((idx+1)·1024, len)]` (`osc.rs:226-237`) |
| renderer → Studio | `/omniphony/state/debug/speaker_gaintable/uptodate` | `[i version]` | client already holds the current version (`dispatch.rs:1044-1052`) |
| renderer → Studio | `/omniphony/state/debug/speaker_gaintable/unavailable` | `[s json]` `{"reason":"no precomputed gain table for the active backend"}` | no table for the active backend (`dispatch.rs:1060-1069`) |

Parsed into `OscEvent::StateDebugSpeakerGaintableMeta { value }`, `…Chunk { bytes }`, `…Unavailable { value }`, `…Uptodate { version: i32 }` (`osc_parser.rs:290-297, 824-838`).

`version` = low 31 bits of Rust `DefaultHasher` over the serialized artifact bytes (`osc.rs:176-180`) — opaque; equal bytes ⇒ equal version; a topology rebuild changes it. Subscribe reply: `have_version == version` → `uptodate`; else `meta` + all chunks (`dispatch.rs:1033-1071`). While subscribed, every topology rebuild re-pushes each target (`recompute.rs:169-183`). A NACK with a matching version resends only the listed chunks, without `meta`; a mismatched version triggers a full resend with `meta` (`osc.rs:197-210`).

### 8.2 Reassembly (Studio Rust, `osc_listener.rs:1402-1572`) — to reimplement in the egui OSC listener

* Transfers keyed by `version` in a map (max 6 in flight, oldest evicted). `meta` (re)starts that version's transfer with `chunk_count`.
* `chunk`: needs ≥ 8 bytes; route by the embedded version (unknown version → dropped); store `bytes[8..]` at `chunk_index`; when `chunks.len() == chunk_count`, concatenate in index order, remove the entry, decode (§8.3).
* Stall repair: a transfer with no activity for 120 ms gets a NACK for its missing indices (max 12 rounds, then abandoned); checked from the receive loop (`osc_listener.rs:1059-1063`).

### 8.3 Artifact byte format ("OBGT", written by `band_gaintable.rs:232-295`, read by `osc_listener.rs:1604-1661`)

```
offset  size  field
0       4     magic  b"OBGT"
4       1     container version = 1        (Studio does not check it)
5       3     reserved, 0
8       4     meta_len     u32 LE
12      4     payload_len  u32 LE
16      meta_len      metadata, UTF-8 JSON (below)
16+meta_len  payload_len  zlib stream (flate2 ZlibEncoder, Compression::default = level 6; RFC 1950 framing)
```

Metadata JSON (`band_gaintable.rs:242-263`):

```json
{ "domain": "cartesian_bands",
  "speaker_index": <i64: ≥0 speaker | -1 energy | -2 gain discontinuity | -3 centroid jump>,
  "x_count": nx, "y_count": ny, "z_count": nz, "band_count": nb,
  "bands": [ { "low_hz": f32, "high_hz": f32 | null }, … ]   // null = open-ended top band
}
```

Inflated payload = little-endian `f32` only, in this order (`band_gaintable.rs:265-279`):

```
x_positions[nx]  y_positions[ny]  z_positions[nz]
band 0: values[cells]   band 1: values[cells] …   (cells = nx·ny·nz)
cell index = xi + nx·(yi + ny·zi)          // xi fastest (live_params.rs:2218-2220, band_gaintable.rs:214)
```

Grid axes (`live_params.rs:2166-2190`, `omniphony-geometry/src/lib.rs:322-352`): `x_positions = evenly_spaced_axis(x_size, −1, 1)` (**ADM x**, width), `y_positions = evenly_spaced_axis(y_size, −1, 1)` (ADM y, depth), `z_positions = cartesian_z_axis(z_size, z_neg_size)` = `z_neg_size` nodes at `−1 + i/z_neg_size` (covering [−1, 0)) followed by `z_size` evenly spaced nodes on [0, 1] — **not** symmetric, which is why the positions are shipped and must be used for the height lookup. Counts are node counts (already `interval + 1`); use the shipped positions, not the `render_evaluation` state events.

Value semantics per `speaker_index`:

| target | per cell per band | Ref |
|---|---|---|
| ≥ 0 | that speaker's linear VBAP gain in the band-restricted topology (0 if the speaker is not in the band; bands with < 3 speakers get a uniform `1/√n` fill for their speakers) | `band_gaintable.rs:79-84`, `live_params.rs:2198-2240` |
| −1 | `√Σᵢ gᵢ²` over all speakers (1.0 = unit energy) | 93-102 |
| −2 | max over the 6 grid neighbours of the L2 distance between energy-normalised gain vectors; 0 = same configuration, √2 = disjoint sets, ≤ 2; cells with `√Σg² ≤ 1e-6` and pairs touching them contribute 0 | 109-135, 180-228 |
| −3 | max over the 6 neighbours of the distance (room units, [−1,1]³) the `g²`-weighted speaker-position centroid moves | 141-172 |

Legacy container `"OEVL"` (`osc_listener.rs:1663-1744`, cartesian/polar with `speaker_count`) is decoded to JSON arrays but is **not** consumed by any speaker display; ignore for parity.

### 8.4 Delivery to JS and decoding (`osc_listener.rs:2699-2726`; `speaker-gaintable.js:161-203`)

Tauri events (immediate, not batched): `speaker_gaintable` with payload
`{ version, domain: "cartesian_bands", speakerIndex: i64, xCount, yCount, zCount, bandCount, bands: [{ lowHz: f64, highHz: f64|null }], dataB64: base64(inflated payload) }`;
`speaker_gaintable:unavailable` `{ reason }`; `speaker_gaintable:uptodate` `{ version }` (ignored by JS, `tauri-bridge.js:220-222`).

JS (`setSpeakerGainTable`): requires `domain === 'cartesian_bands'`, `nx,ny,nz,nb ≥ 1`, buffer ≥ `(nx+ny+nz+nb·cells)·4` bytes; slices `Float32Array` views (host endianness, i.e. assumes little-endian) in the order above; caches `{ nx, ny, nz, speakerIndex, bands: [{lowHz, highHz, gains}], xPositions, yPositions, zPositions }` under `speakerIndex` and records `versions[speakerIndex] = version`.

### 8.5 Subscription / cache semantics (`speaker-gaintable.js:36-150`; `trails-and-display-listeners.js:317-435`)

* Consumers: `speakerSoloVolume` → target `selectedSpeakerIndex` (or 0 when none), `globalEnergyVolume` → −1, `discontinuityVolume` → −2 (`mode 'gain'`) / −3 (`'centroid'`).
* `acquireGainTable(id)` on toggle-on (and at boot for persisted-on toggles): subscribe every target in use with its held version; start a 5 s heartbeat that re-subscribes (repair path). `releaseGainTable(id)`: drop that target's cached table/version if no other consumer uses it; when no consumer remains stop the heartbeat and send `unsubscribe`.
* `refreshGaintableSubscription()` on speaker selection change (`speakers.js:272`), discontinuity-mode change, global toggle.
* Cell lookup for the volumes (`makeCellIndexer`, `object-energy-shared.js:235-257`), inputs are Omniphony-normalised `(ow = width/ADM x, od = depth/ADM y, oh = height/ADM z)`:
  `xi = clamp(round((ow+1)/2·(nx−1)), 0, nx−1)`, `yi` likewise from `od`, `zi = argminᵢ |zPositions[i] − oh|`, `index = xi + nx·(yi + ny·zi)`.

---

## 9. Interaction (`picking.js`, called through `speakers.js` API)

* Pick targets: all visible speaker cubes, speaker labels and band bars (plus object meshes/labels/outlines) via `Raycaster.intersectObjects(…, recursive=false)` (`picking.js:116-128`; `raycaster.params.Line.threshold = 0.08`, `materials.js:51-52`). Click = pointerdown/up within 6 px (`picking.js:34`). First hit that is a speaker mesh, label or band bar → `setSelectedSource(null); setSelectedSpeaker(idx)` (130-155). Click on nothing → deselect both. No hover feedback, no cursor change is set anywhere in these files.
* `setSelectedSpeaker(index)` (`speakers.js:1855-1874`): `index === null` disarms both gizmo modes; updates colours, gizmo, list, scrolls the row into view.
* Drag (only when the matching gizmo is armed; `OrbitControls.enabled = false` during the drag, `picking.js:231`):
  * polar: pointerdown must hit `speakerGizmo.ring` (→ azimuth) or `speakerGizmo.arc` (→ elevation) (256-267). Azimuth: intersect the ray with plane `y = 0`, `az = normalize(atan2(hit.z, hit.x))`, `delta = (√(hit.x²+hit.z²) − d)/d`; `0 ≤ delta ≤ 0.1` → snap 1° (thr 0.5°) and show minor ticks; `delta > 0.1` → snap 5° (thr 2.5°) (311-325). Elevation: plane containing the Y axis and the azimuth direction (normal = `dir × up`), `el = clamp(atan2(hit.y, √(x²+z²)), −90, 90)`, same snapping against `|hit|` (326-344). New position = `sphericalToCartesianDeg(az, el, d)`; mesh + label (`+0.12` y) moved immediately; committed through `applySpeakerSceneCartesianEdit(index, x, y, z, sendOsc=false)` each move and `sendOsc=true` on pointerup (366-390).
  * cartesian: pointerdown must hit a handle sphere; the drag projects the ray onto the axis line through the start position (`projectRayOntoAxis`, `input.js`) and moves by `Δt` along that axis (269-298, 345-356).
  * wheel with Ctrl or Shift while polar-armed: `dragDistance ± 0.05` (Shift: 0.01) in [0.2, 2.0] (59-84); speakers are not sent per tick.
* OSC out on commit (`speakers.js:1096-1134`): `control_layout_config { speakerEdits: [{ id, coordMode, x,y,z | azimuth,elevation,distance }] }` (only the block matching the speaker's `coordMode`) then `control_layout_config_apply`. The renderer echoes the layout (`/omniphony/state/layout` → `layouts:update`) which re-renders.

---

## 10. Per-speaker heatmap volume — `scene/speaker-solo-volume.js`

Ray-marched volume of the selected speaker's gain field from the cached OBGT slice (§8). Static field: rebuilt only when its signature changes.

* Shown iff `app.speakerHeatmapVolumeEnabled && table && Number.isInteger(selectedSpeakerIndex) && selectedSpeakerIndex ≥ 0 && table.speakerIndex === selectedSpeakerIndex` (61-76); otherwise `volume.hide()`.
* Signature (83-98): `[table identity, speaker, heatmapAllBands, heatmapBandIndex, speakerHeatmapVolumeColormap, speakerCustomGradientVersion, volumeSmoothInterpolation, objectEnergyHeatmapResolution, objectEnergyHeatmapOpacity, objectEnergyVolumeMix, gammaAccumulate, gammaMip, roomRatio.height/lower/width/rear/length]`. Rebuild additionally throttled to `app.volumeRefreshMs` (default 160 ms; fallback `VOLUME_REBUILD_INTERVAL_MS = 160`) since `app.lastSpeakerSoloVolumeAt` (100-106).
* Common volume params (112-120): `resolution = objectEnergyHeatmapResolution` (64), `opacity = objectEnergyHeatmapOpacity` (1), `mix = objectEnergyVolumeMix` (0.6), `gammaAccumulate = clamp(objectEnergyVolumeGammaAccumulate, 1.0, 10.0)` (state default 4), `gammaMip = clamp(objectEnergyVolumeGammaMip, 0.2, 3.0)` (default 3), `customStops = speakerCustomGradientStops`, `smooth = volumeSmoothInterpolation` (false).
* Single band (`!heatmapAllBands || nbands ≤ 1`, 165-175): `bandIndex = clamp(round(heatmapBandIndex), 0, nbands−1)`; `sampleEnergy(ow, od, oh) = g²`, `g = bands[bandIndex].gains[cellIndex(ow, od, oh)]`; `colormap = index of speakerHeatmapVolumeColormap` in `['heatmap','blueWhite','whiteRed','red','custom']`; level normalised by the field's own max (`maxLevel` unset).
* All bands (`heatmapAllBands && nbands > 1`, 122-163): precoloured path. Per band `bandRGB[b] = objectEnergyColor(colormap, bandLogT(band))` (§11.2 gradient at the band's log-frequency position) where
  `bandLogT: lo = max(20, lowHz); hi = min(20000, highHz > 0 ? highHz : 20000); f = √(lo · max(lo, hi)); t = clamp((ln clamp(f,20,20000) − ln 20)/(ln 20000 − ln 20), 0, 1)` (51-59).
  Per cell: `lvl_b = g_b²`, `sumW = Σ lvl_b`, `rgb = Σ lvl_b·bandRGB[b] / sumW`, `alpha = sumW` (0,0,0,0 when `sumW = 0`). Level normalised by the max `alpha`.
* Default `heatmapAllBands = true`, `heatmapBandIndex = 0` (`state.js:483-484`); the selector is `#heatmapBandSelect` (§13).

---

## 11. Discontinuity volume — `scene/discontinuity-volume.js`

* Shown iff `app.discontinuityHeatmapEnabled && table` where `table = tables[−2 | −3]` by `discontinuityHeatmapMode` (233-238); hidden otherwise.
* Signature (243-258): `[table, scale, heatmapBandIndex, heatmapAllBands, smooth, resolution, opacity, mix, γacc, γmip, roomRatio…]`; throttle `volumeRefreshMs` via `app.lastDiscontinuityVolumeAt`.
* `scale = clamp(discontinuityHeatmapScale, 0.05, 2)` (default 0.5, `DISCONTINUITY_SCALE_*`, 212-231).
* Field: `jumps = bands[clamp(round(heatmapBandIndex), 0, nb−1)].gains`; when `heatmapAllBands && nb > 1`: per cell `max_b gains_b` (279-291).
* Precoloured, **absolute** normalisation `maxLevel: 1` (303): per cell `t = clamp(jump/scale, 0, 1)`, `rgb = (1, 0.65 − 0.45 t, 0.05)` (amber → red), `alpha = t` (304-312). Uses the shared volume core with `resolution/opacity/mix/γ/smooth` as §10 (no colormap).

---

## 11.x Shared volume core contract (`scene/energy-volume-core.js`) — required by §10-11

Owned by the object-energy spec; restated here because both speaker volumes are nothing but `EnergyVolume.update(...)` calls.

### Box and texel mapping (`update`, 216-321)

```
n = clamp(round(resolution), 8, 64)                       // texture n³, RGBA float32
height = max(1e-3, roomRatio.height); lower = max(1e-3, roomRatio.lower); width = max(1e-3, roomRatio.width)
xMin = mapRoomDepth(-1); xMax = mapRoomDepth(1); yMin = -lower; yMax = height; zMin = -width; zMax = width   // scene-space box
texel (i = depth/x, j = height/y, k = width/z), data index = i + n·(j + n·k)   (i fastest)
od[i] = inverseMapRoomDepth(xMin + (i+0.5)/n·(xMax−xMin))
oh[j] = sy >= 0 ? sy/height : sy/lower,  sy = yMin + (j+0.5)/n·(yMax−yMin)
ow[k] = (zMin + (k+0.5)/n·(zMax−zMin)) / width
scalar mode:  texel = (sampleEnergy(ow,od,oh), 0, 0, 0);  maxEnergy = max
precoloured:  texel = sampleColor(ow,od,oh) → (r,g,b,level); maxEnergy = max level
uInvMax = 1 / (maxLevel > 0 ? maxLevel : maxEnergy)  (0 if both 0)
uOpacity = clamp(opacity, 0.05, 1); uMix = clamp(mix, 0, 1); uSteps = clamp(round(2n), 32, 384); uStepNorm = 64 / uSteps
mesh: unit BoxGeometry positioned at box centre, scaled to (xMax−xMin, yMax−yMin, zMax−zMin)
```

Material (`RawShaderMaterial`, GLSL3, 136-166): `transparent`, `premultipliedAlpha: true` (blend `src ONE, dst ONE_MINUS_SRC_ALPHA` for both RGB and A), `depthWrite false`, `depthTest false`, `side BackSide` (cull front faces → the ray starts from the back face so it works with the camera inside the box), `toneMapped false`, `renderOrder 22`, `frustumCulled false`. Sampler: Nearest, or Linear when `smooth` and `OES_texture_float_linear` is available (222-227). Texture `unpackAlignment 1`.

### Shaders (GLSL at 35-115 + `HEATMAP_GLSL` in `object-energy-shared.js:116-153`) — WGSL translation, same names/maths

```wgsl
struct VolumeUniforms {
  box_min: vec3<f32>,  _p0: f32,          // uBoxMin
  box_max: vec3<f32>,  _p1: f32,          // uBoxMax
  camera_position: vec3<f32>, _p2: f32,   // cameraPosition (world)
  inv_max: f32,            // uInvMax
  opacity: f32,            // uOpacity
  gamma_accumulate: f32,   // uGammaAccumulate
  gamma_mip: f32,          // uGammaMip
  step_norm: f32,          // uStepNorm
  mix_amount: f32,         // uMix
  colormap: i32,           // uColormap 0 heatmap,1 blueWhite,2 whiteRed,3 red,4 custom
  steps: i32,              // uSteps
  precolored: i32,         // uPrecolored 0 scalar(.r) / 1 rgba
  custom_stop_count: i32,  // uCustomStopCount
  _p3: vec2<f32>,
  custom_stops: array<vec4<f32>, 8>,  // uCustomStops (pos, r, g, b)
};
@group(0) @binding(0) var<uniform> u: VolumeUniforms;
@group(0) @binding(1) var volume_tex: texture_3d<f32>;   // rgba32float, n³
@group(0) @binding(2) var volume_samp: sampler;          // nearest | linear, clamp-to-edge

struct VsOut { @builtin(position) clip: vec4<f32>, @location(0) world_pos: vec3<f32> };
@vertex fn vs_main(@location(0) position: vec3<f32>) -> VsOut {   // modelMatrix, viewMatrix, projectionMatrix
  var o: VsOut; let world = model * vec4(position, 1.0); o.world_pos = world.xyz; o.clip = proj * view * world; return o;
}

fn custom_stops_color(t: f32) -> vec3<f32> {
  let n = u.custom_stop_count;
  if (n <= 0) { return vec3(t); }
  if (t <= u.custom_stops[0].x) { return u.custom_stops[0].yzw; }
  for (var i = 0; i + 1 < 8; i++) {
    if (i + 1 >= n) { break; }
    let a = u.custom_stops[i]; let b = u.custom_stops[i + 1];
    if (t <= b.x) { let f = select(0.0, (t - a.x) / (b.x - a.x), b.x > a.x); return mix(a.yzw, b.yzw, f); }
  }
  return u.custom_stops[n - 1].yzw;
}
fn heatmap_color(value: f32) -> vec3<f32> {
  let t = clamp(value, 0.0, 1.0);
  if (u.colormap == 4) { return custom_stops_color(t); }
  if (u.colormap == 3) { return vec3(1.0, 0.0, 0.0); }
  if (u.colormap == 2) { return vec3(1.0, 1.0 - t, 1.0 - t); }
  if (u.colormap == 1) { return vec3(t, t, 1.0); }
  if (t < 0.25) { return mix(vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 1.0), (t - 0.00) / 0.25); }
  if (t < 0.48) { return mix(vec3(0.0, 1.0, 1.0), vec3(0.0, 1.0, 0.0), (t - 0.25) / 0.23); }
  if (t < 0.70) { return mix(vec3(0.0, 1.0, 0.0), vec3(1.0, 1.0, 0.0), (t - 0.48) / 0.22); }
  return mix(vec3(1.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), (t - 0.70) / 0.30);
}

@fragment fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
  let ro = u.camera_position;
  let rd = normalize(in.world_pos - ro);
  let invd = 1.0 / rd;
  let ta = (u.box_min - ro) * invd;
  let tb = (u.box_max - ro) * invd;
  let tmin = min(ta, tb); let tmax = max(ta, tb);
  var t_near = max(max(tmin.x, tmin.y), tmin.z);
  let t_far = min(min(tmax.x, tmax.y), tmax.z);
  t_near = max(t_near, 0.0);
  if (t_far <= t_near) { discard; }
  let box_size = u.box_max - u.box_min;
  let dt = (t_far - t_near) / f32(u.steps);
  var acc = vec4<f32>(0.0);
  var e_max = 0.0; var e_max_col = vec3<f32>(0.0);
  for (var s = 0; s < 512; s++) {
    if (s >= u.steps) { break; }
    let t = t_near + (f32(s) + 0.5) * dt;
    let p = ro + rd * t;
    let uvw = (p - u.box_min) / box_size;                 // x=depth, y=height, z=width
    let tx = textureSampleLevel(volume_tex, volume_samp, uvw, 0.0);
    let e = clamp(select(tx.r, tx.a, u.precolored == 1) * u.inv_max, 0.0, 1.0);
    let col = select(heatmap_color(e), tx.rgb, u.precolored == 1);
    if (e > e_max) { e_max = e; e_max_col = col; }
    if (e > 0.004) {
      let a = clamp(pow(e, u.gamma_accumulate) * u.opacity * u.step_norm, 0.0, 1.0);
      acc = vec4(acc.rgb + (1.0 - acc.a) * col * a, acc.a + (1.0 - acc.a) * a);
      if (u.mix_amount < 0.001 && acc.a > 0.98) { break; }
    }
  }
  let a_mip = clamp(pow(e_max, u.gamma_mip) * u.opacity, 0.0, 1.0);
  let peak = vec4(e_max_col * a_mip, a_mip);
  let result = mix(acc, peak, u.mix_amount);
  if (result.a <= 0.0) { discard; }
  return result;                                            // premultiplied alpha
}
```

### 11.2 `objectEnergyColor` (CPU, `object-energy-shared.js:96-111`) — used by §10 all-bands

Same ramps as `heatmap_color`: `'red'` → (1,0,0); `'blueWhite'` → (t,t,1); `'whiteRed'` → (1,1−t,1−t); `'custom'` → piecewise-linear over `stops[{pos,r,g,b}]` (greyscale `t` if none); default `'heatmap'` → stops `(0: 0,0,1) (0.25: 0,1,1) (0.48: 0,1,0) (0.70: 1,1,0) (1: 1,0,0)` (35-41). Colours are used raw (no colour management) in the RawShaderMaterial.

---

## 12. Data sources summary

| Data | Source | Path to JS | Consumers here |
|---|---|---|---|
| Layout speakers `{id, x, y, z, azimuthDeg, elevationDeg, distanceM, coordMode:'polar'|'cartesian', spatialize:0|1, delay_ms, freqLow?, freqHigh?}`, `radius_m`, `crossoverCutoffs[]` | `/omniphony/state/layout [s json]` → `OscEvent::StateLayout` (`osc_parser.rs:779`) → `apply_layout_domain_state` → Tauri `layouts:update { layouts: [Layout], selectedLayoutKey }` (`osc_listener.rs:2620-2623`; `layouts.rs:7-77`) | `hydrateLayoutSelect` → `renderLayout`/`patchCurrentLayout` (`speakers.js:2513-2557, 2229-2377, 2486-2511`) |
| Per-speaker live edits | `/omniphony/state/speaker/{i}/gain|delay|mute|spatialize|name|freq_low|freq_high` → `OscEvent::StateSpeaker*` (`osc_parser.rs:1007-1058`) → Tauri `speaker:gain {id,gain}`, `speaker:delay {id,delayMs}`, `speaker:mute {id,muted 0|1}`, `speaker:spatialize {id,spatialize,crossoverCutoffs}`, `speaker:name {id,name}`, `speaker:freq_low {id,freq_low,crossoverCutoffs}`, `speaker:freq_high {…}` (`osc_listener.rs:2399-2569`) | handlers in `tauri-bridge.js` (not in scope) → `setSpeakerSpatializeLocal` (1014-1030), `updateSpeakerVisualsFromState`, `renderSpeakersList` |
| Speaker meter | `/omniphony/meter/speaker/{i}` → `MeterSpeaker` → `speaker:meter` (batched) | §7 |
| Selected object's per-speaker gains | `/omniphony/meter/object/{id}/gains` → `MeterObjectGains` → `source:gains` (batched) | §1.4 |
| Per-band gains | `/omniphony/meter/object/{id}/band/{b}/gains` → `MeterObjectBandGains` → `source:band_gains {id, band, gains}` | list UI only (`getObjectDominantSpeakerText`, `updateSpeakerBandBars`) |
| Clip | `/omniphony/state/clip` → `clip:detected` | DOM only |
| Gain tables | §8 | §10, §11 |
| Room ratio / bounds | not in these files (`controls/room-geometry.js`, `setup.js`) | §0.1, §4, §11.x |
| Binaural ghosting | renderer binaural state (`controls/binaural.js:423-429`) | §1.5 |

Batching (`osc_listener.rs:1756-1768, 2001-2025`): `speaker:meter`, `source:gains`, `source:band_gains`, `source:meter`, `master:meter`, `ear:meter`, `binaural:head_pose`, `meter:drc_gain` are coalesced per `(event|id[|band])` and flushed every 16 ms as one Tauri event `state:batch { events: [{ event, payload }] }`; JS dispatches them identically to the direct events (`tauri-bridge.js:98-115`).

---

## 13. Defaults, toggles, persistence

All persisted in `localStorage['spatialviz.effective_render_prefs']` (`controls/room-geometry.js:26, 98-135`, restore 327-395), written by `persistEffectiveRenderPrefs()` on every change.

| State (`app.*`) | Default | Control id (`index.html`) | i18n key | Persist key | Effect |
|---|---|---|---|---|---|
| `speakerLabelsEnabled` | false | `#speakerLabelsToggle` (checkbox) | `display.speakerLabels` | `speakerLabels` | label sprites `visible` (§2) |
| `speakerBandBarsEnabled` | false | `#speakerBandBarsToggle` | `display.speakerBands` | `speakerBands` | band bars `visible` (§3) |
| `speakerFaceListenerEnabled` | false | `#speakerFaceListenerToggle` | `display.speakerFaceListener` | `speakerFaceListener` | orientation + driver disc (§1.2) |
| `speakerSize` | 0.08, range [0.04, 0.2] step 0.002 | `#speakerSizeSlider` + `#speakerSizeVal` | `display.speakerSize` | `speakerSize` | cube scale factor (§1.3) |
| `speakerHeatmapVolumeEnabled` | false | `#speakerHeatmapVolumeToggle` | (label has only `help.heatmap.speakerVolume`; text "Heatmap volume") | same | §10 + gain-table consumer |
| `speakerHeatmapVolumeColormap` | `'heatmap'` | `#speakerHeatmapVolumeColormap` (select: heatmap, blueWhite, whiteRed, red, custom) | `heatmap.objectEnergy.colormap*` | same | §10 |
| `speakerCustomGradientStops` / `speakerCustomGradientVersion` | 3 stops blue/green/red | `#speakerGradientEditor` (shown when colormap = custom) | — | `speakerCustomGradientStops` | §10 custom ramp |
| `heatmapBandIndex` / `heatmapAllBands` | 0 / true | `#heatmapBandSelect` (options `0..n-1`, `'all'` for multi-band; `speaker-band-select.js`) + floating band cursor over the 3D view (`controls/band-cursor.js`, DOM) | `heatmap.crossoverBand`, `heatmap.bandFull`, `heatmap.bandAll` | same | §10, §11 |
| `discontinuityHeatmapEnabled` | false | `#discontinuityHeatmapToggle` | `heatmap.discontinuity.toggle` | same | §11 |
| `discontinuityHeatmapMode` | `'gain'` | `#discontinuityHeatmapMode` (gain / centroid) | `heatmap.discontinuity.mode*` | same | target −2/−3 |
| `discontinuityHeatmapScale` | 0.5, [0.05, 2] step 0.05 | `#discontinuityHeatmapScale` (number) | `heatmap.discontinuity.scale` | same | §11 |
| shared volume params `objectEnergyHeatmapResolution` 64, `objectEnergyHeatmapOpacity` 1, `objectEnergyVolumeMix` 0.6, `objectEnergyVolumeGammaAccumulate` 4, `objectEnergyVolumeGammaMip` 3, `volumeRefreshMs` 160, `volumeSmoothInterpolation` false | (`state.js:525-541`) | object-energy controls (other spec) | | | §10-11 |
| edit mode `activeEditMode 'polar'`, `polarEditArmed false`, `cartesianEditArmed false` | | `#speakerEditPolarGizmoBtn`, `#speakerEditCartesianGizmoBtn` | | not persisted | §5 |

---

## 14. Not viewport (pure UI) — for later scheduling

From `speakers.js`:
* Speakers list rows: `createSpeakerItem` / `updateSpeakerItem` (586-912) — name chip, position thumbnail SVG (`positionIconMarkup`, 849-877; height→hue `240·(1−t)`), filter glyph SVG + cutoff text (806-897), dB readout, meter bar + peak-hold cursor (`mute-solo.js:90-112`), M/S buttons, per-band contribution bars (`updateSpeakerBandBars`, 921-983), drag-and-drop reorder with FLIP animation (593-657, 2081-2223), clip flash (769-804).
* Objects list rows: `createObjectItem` / `updateObjectItem` / `applyObjectIdentity` / `applyObjectPositionIcon` (1304-1546), grouping/sort in `renderObjectsList` (1601-1672), dominant-speaker text (542-569).
* Speaker editor panel `renderSpeakerEditor` (1159-1298) and all DOM getters (230-264); coord-mode switch (468-486); add/remove/move/reorder (2021-2162); delay↔distance tools (373-462); export/replace-layout serialisation (296-367); layout `<select>` hydration (2513-2557); `updateSectionProportions` (2001-2010); `updateSpeakerControlsUI` / `updateObjectControlsUI` (492-536); `updateControlsForEditMode` (1876-1878, sets `controls.enableZoom = true`).
* `scene/speaker-band-select.js`: entirely UI (the `#heatmapBandSelect` options + `renderBandCursor` DOM overlay).
* `scene/speaker-gaintable.js`: transport/cache only (no drawing) — but required by §10-11.
* `scene/speaker-band-bars.js`: `bandColor`/`speakerBandIndex` are also used by the list UI.

---

## 15. Unclear / not verified in this pass

* `apply_layout_domain_state` (renderer JSON of `/omniphony/state/layout` → `Layout`) and the JS handlers for `speaker:*` events (`tauri-bridge.js`) were not read; the field set above is the serialized `Layout` struct.
* `roomBounds` mutation and `roomGroup` transform (room-geometry / setup) are assumed to track `roomRatio`; not traced.
* `renderBandCursor` (`controls/band-cursor.js`) is a DOM overlay on the 3D view; whether the egui port needs it is a UI decision.
* The renderer's `uptodate` reply is ignored by JS; `unavailable` only logs. Behaviour to keep: when unavailable, the volumes simply stay hidden (no table).
* `MeshStandardMaterial` lighting parity depends on the light directions in `setup.js:234-260` (not quoted here).
