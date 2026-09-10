# Viewport parity spec — objects (sources)

Scope: `omniphony-studio/src/` files `sources.js`, `scene/labels.js`, `scene/materials.js`, `scene/object-energy-shared.js` (colormap parts), `picking.js`, `mute-solo.js` (visual side), plus the object-appearance parts of `state.js`, `controls/scene-effects-bar.js`, and the room-warp application in `coordinates.js`. All paths below are relative to `omniphony-studio/src/` unless absolute. `line` references are `file:line`.

three.js version: `^0.165.0` (`omniphony-studio/package.json:26`). Renderer defaults apply (no override found anywhere in `src/`): `outputColorSpace = SRGB`, `toneMapping = NoToneMapping`, `setPixelRatio(viewport.dpr)` (`core/render/render-surface-controller.js:24`). All hex colours below are sRGB as written in the JS; three.js converts `Color(hex)` to linear internally for lighting.

Scene frame (scene units, room half-width = 1): `scene.x` = depth (front +), `scene.y` = up, `scene.z` = right. Camera `PerspectiveCamera(65°, aspect, 0.1, 100)` at `(-3.8, 1.1, 0)` looking at `HEAD_PIVOT (0, 0.25, 0)` (`scene/setup.js:20-26`). Background `0x0a0b10`. Lights (`scene/setup.js:234-246`): Ambient `0xffffff` 0.24; Directional `0xfff7ea` 2.35 at `(3.6, 4.8, 1.4)`; Directional `0xb8d4ff` 1.05 at `(-2.8, 1.1, -3.8)`; Hemisphere sky `0xdcecff` ground `0x0d0f14` 0.12; PointLight `0xfff4dc` 0.9 dist 2.2 decay 2 at `(-0.18, 0.42, 0.22)` under `brassempouyAnchor` (head fill).

---

## 1. Object data model (state.js)

| Map / field (`state.js`) | Key | Value | Written by |
|---|---|---|---|
| `sourceMeshes` :15 | id (string) | THREE.Mesh (sphere) | `getSourceMesh` sources.js:981 |
| `sourceLabels` :16 | id | THREE.Sprite (label) | same |
| `sourceOutlines` :17 | id | THREE.LineLoop (ring) | same |
| `sourceLevels` :18 | id | `{peakDbfs, rmsDbfs, bandRmsDbfs: number[]|null}` | `updateSourceLevel` :1212 |
| `sourceLevelLastSeen` :26 | id | `performance.now()` ms | :1224 |
| `sourceGains` :28 | id | `number[]` per speaker (0..1) | `updateSourceGains` :1253 |
| `sourceBandGains` :29 | id | `number[band][speaker]` | `updateSourceBandGains` :1276 |
| `objectMuted` / `objectManualMuted` :34/:38 | id | Set | mute-solo.js, tauri-bridge.js:378 |
| `sourceNames` :39 | id | raw name string | `updateSource` :1122 |
| `sourceTags` :40 | id | `'A'|'B'|other` upper-cased | :1054-1061, `updateSourceTag` :1168 |
| `sourcePositionsRaw` :41 | id | hydrated raw record (§3) incl. `x,y,z` (ADM normalised), `azimuthDeg, elevationDeg, distanceM, coordMode, metadataGainDb, directSpeakerIndex, fixed, label, kind, t` | :1069-1087 |
| `sourceSizes` :44 | id | `{w,d,h}` clamped [0,1] — **not used by any viewport visual**, only the list gauges | `updateSourceSize` :1150 |
| `sourceDirectSpeakerIndices` :45 | id | speaker index | :1089 |
| `sourceTrails` :46 | id | `{positions[], line, lastPointAt, lastRebuildAt}` | trails.js (specified elsewhere in this directory) |
| `sourceEffectiveMarkers` / `sourceEffectiveLines` :47/48 | id | Mesh / Line | §6.5 |
| `sourceBaseColors` :49 | id | THREE.Color (palette cache) | `getObjectBaseColor` :219 |
| `app.selectedSourceId` :408 | — | id string or null | `setSelectedSource` :945 |
| `app.selectedSpeakerIndex` :409 | — | int or null | speakers.js |

Mesh `userData` (sources.js:990-1003): `sourceId`, `baseOpacity` (=0.7, from `sourceMaterial.opacity`), `objectBaseColor`, `objectTrailColor`, `diffuseHalo` (Sprite), `levelScale` (set by `applySourceLevel` :580).

**Kinds**: `sourcePositionsRaw.kind ∈ {'height','phantom', undefined}` and `fixed: true|undefined`. **Neither `kind` nor `fixed` changes any 3D visual**; they only drive the list badge icon (`objectBadge` :142, consumed by speakers.js:1498/1615) and the dominant-speaker readout (speakers.js:1459). The only kind-like effect in the viewport is positional: an object with `directSpeakerIndex` is snapped onto the speaker mesh (§3).

**"Metadata silent"** (`isSourceMetadataSilent` :267): `metadataGainDb <= -128` → every per-object visual is hidden (mesh, outline, halo, label, trail, effective marker/line) regardless of other toggles.

**Test/injected object**: id `OBJECT_TEST_SOURCE_ID` (`object-test-id.js`), pushed through `updateSource` with `coordMode:'cartesian'`, `name: t('objectTest.markerLabel')` (controls/object-test.js:205-215); label code forced to `INJ` (:146). Same visuals as any object.

**Synthetic at-rest bed objects** (controls/virtual-bed.js:449-547): when no `spatial:frame` for 800 ms (`STREAM_IDLE_MS` :424), Studio creates one object per virtual-bed channel via `updateSource(ch.name, {coordMode, x,y,z, azimuthDeg, elevationDeg, distanceM, name, fixed:true, label, gainDb, directSpeakerIndex, _noTrail:true})`. `_noTrail` suppresses trail points (sources.js:1063). These are ids = channel names (e.g. `L`, `Ls`). They are removed as soon as streaming resumes.

---

## 2. Data feed (OSC → Rust `OscEvent` → Tauri event → JS)

Backend parser: `omniphony-studio/src-tauri/src/osc_parser.rs`; forwarding: `src-tauri/src/osc_listener.rs`. High-frequency events are coalesced into one `state:batch` Tauri event (`{events:[{event, payload}]}`) and replayed (`tauri-bridge.js:90-139`), individually-listened events at :174-236, :378-391.

| Viewport need | OSC address (lower-cased, split on `/`) | Args | `OscEvent` variant (osc_parser.rs) | Tauri event / payload | JS entry |
|---|---|---|---|---|---|
| Position | `/omniphony/object/{id}/xyz` or `/aed` (`spherical`, `polar` also accepted) | `[p0,p1,p2, speaker_idx, gain_db, priority, ramp, gen(Long), name(String)]` — 9 args post-`divergence`; index 3 = `direct_speaker_index` (≥0 else None), 4 = `gain_db` as i32, name at index 9/8/7 depending on arg count (:511-533) | `Update{id, position: Position, name}` :163 | `source:update` `{id, position:{x,y,z,coordMode,azimuthDeg,elevationDeg,distanceM,gainDb,generation,directSpeakerIndex,fixed,label,kind,sourceTag,name}}` (osc_listener.rs:2145-2184; backend merges meta/name into the entry so every emit carries the full record) | `updateSource(id, position)` sources.js:1033 |
| Fixed/label/kind | `/omniphony/object/{id}/meta` | `[Int fixed, String label, Long generation, String kind?]`; `kind ∈ {height, phantom}` else inferred from label prefix (`infer_object_kind` :62) | `UpdateMeta{id, fixed, label, generation, kind}` :171 | re-emitted as `source:update` (osc_listener.rs:2187-2223). **Note**: this re-emit omits `"kind"` from the payload (line 2217 lists `fixed,label` only) — the JS then stores `kind: undefined` until the next positional update; harmless for visuals. | same |
| Size | `/omniphony/object/{id}/size` | `[w,d,h, gen]` clamped 0..1 | `UpdateSize{id,size:[f32;3]}` :183 | `source:size {id,size:{w,d,h}}` | `updateSourceSize` (list only) |
| Remove | any address containing `remove|delete|off` | id from arg0 or address | `Remove{id}` :190 | `source:remove {id}` | `removeSource` :1300 |
| Level | `/…/meter/object/{id}` | `[peak, rms, band_rms…]`; peak clamped [-100, +24], rms [-100, 0] (:1080-1095) | `MeterObject{id, peak_dbfs, rms_dbfs, band_rms_dbfs}` :193 | `source:meter {id, meter:{peakDbfs, rmsDbfs, bandRmsDbfs[], peakHoldDbfs}}` (osc_listener.rs:2264-2280) | `updateSourceLevel` :1212 |
| Per-speaker gains | `/…/meter/object/{id}/gains` | `[g0..gN]` clamped 0..1 | `MeterObjectGains{id, gains}` :206 | `source:gains {id, gains[]}` | `updateSourceGains` :1253 |
| Per-band gains | `/…/meter/object/{id}/band/{b}/gains` | `[g0..gN]` clamped 0..1 | `MeterObjectBandGains{id, band, gains}` :209 | `source:band_gains {id, band, gains[]}` | `updateSourceBandGains` :1276 |
| Mute state | `/omniphony/state/object/{id}/mute` | `[0|1]` | `StateObjectMute{id, muted}` :249 | `object:mute {id, muted}` | tauri-bridge.js:378 → `objectMuted` set |
| Source tag (A/B) | `/omniphony/state/object/{id}/source_tag` | `[String]` | `StateObjectSourceTag{id, source_tag}` :251 | `object:source_tag {id, sourceTag}` | `updateSourceTag` :1168 |
| Stream liveness | `/omniphony/spatial/frame` | `[sample_pos, gen, count, coord_format]` | `SpatialFrame` :153 | `spatial:frame` | sets `app.lastSpatialFrameAt` (tauri-bridge.js:248) |
| Initial snapshot | — | — | — | `state:snapshot_ready` → `applyInitState` (init.js:120-140): `sources{}`, `sourceLevels{}`, `objectSpeakerGains{}`, `objectMutes{}` replayed through the same functions | |

Mute/solo commands sent by Studio: Tauri `invoke('control_object_mute', {id: Number(id), muted: 0|1})` (mute-solo.js:183). Non-numeric ids (test object) are handled locally.

---

## 3. Coordinate pipeline (position → scene)

`updateSource` (sources.js:1033-1143):

1. **Pin check** (:1038-1053): if `app.channelEditPinId === id` and pin not expired (`channelEditPinUntil` 0 = never, else `performance.now() > until` clears it): set `mesh.position = channelEditPinPos`, label at `(pin.x, pin.y + 0.12, pin.z)`, **return** (no decorations update).
2. **Source tag** (:1054-1061): from `position.sourceTag` or inferred from id prefix `^a[_:]` → `'A'`, `^b[_:]` → `'B'` (:198).
3. **Hydrate** (`hydrateObjectCoordinateState`, coordinates.js:435-466) into `sourcePositionsRaw`:
   - mode = `coordMode` if `'cartesian'|'polar'`, else `'polar'` if any of az/el/dist finite, else `'cartesian'` (:420).
   - cartesian: `x,y,z` clamped [-1,1] (ADM normalised: x right, y front, z up); az/el/dist derived from the **room-warped scene position** (`cartesianToSpherical(normalizedOmniphonyToScenePosition({x,y,z}))`, :443-444), dist floored 0.01. Rust equivalent: `omniphony_geometry::f64::to_spherical` on the scaled position (`omniphony-renderer/omniphony-geometry/src/lib.rs:108`) — note the JS takes atan2(scene.z, scene.x) = atan2(adm.x, adm.y), identical to the crate.
   - polar: `dist = max(0.01, distanceM || 1)`; scene = `sphericalToCartesianDeg(az, el, dist)` (coordinates.js:230: `x = d cos el cos az`, `y = d sin el`, `z = d cos el sin az`, in scene axes) then `scenePositionToNormalizedOmniphony` (:396: inverse depth warp by 28-step bisection, divide y by `roomRatio.height` (y≥0) or `roomRatio.lower` (y<0), divide z by `roomRatio.width`, swizzle back, clamp [-1,1], snap to {-1,0,1} within 1e-5). Rust: `inverse_map_depth`, `inverse_room_scaled_position` (lib.rs:232, :290).
4. **Scene position** (:1088-1101):
   - if `directSpeakerIndex` is an integer and `speakerMeshes[idx]` exists → `mesh.position = speakerMesh.position` (snap onto the speaker cube, whatever its room-warped position is).
   - else `normalizedOmniphonyToScenePosition(raw)` (coordinates.js:341) = `mapRoomPosition(omniphonyToSceneCartesian(raw))`:
     - swizzle (:194): `scene = (adm.y, adm.z, adm.x)` = `omniphony_geometry::adm_to_scene` (lib.rs:88).
     - room warp (`mapRoomPosition` :289): `scene.x' = depthWarpWithRatios(scene.x, roomRatio.length, roomRatio.rear, roomRatio.centerBlend)`; `scene.y' = y ≥ 0 ? y·roomRatio.height : y·roomRatio.lower`; `scene.z' = z·roomRatio.width`. `depthWarpWithRatios` (:267) = `omniphony_geometry::map_depth` (lib.rs:207) exactly (cubic through (0,0) and (±1, ±ratio), origin slope `rear + (front-rear)·blend`). Whole step = `room_scaled_position(adm, [width, length, height], rear, lower, centerBlend)` (lib.rs:269) followed by `adm_to_scene`.
     - `app.roomRatio` defaults `{width:1, length:2, height:1, rear:1, lower:0.5, centerBlend:0.5}` (state.js:107); updated from the renderer's room domain (controls/room-geometry.js `applyRoomRatio`, not in scope). When the ratio changes, room-geometry.js re-runs `updateSourceDecorations` for every object (it imports it at :16) — positions themselves are recomputed on the next `updateSource`; check whether meshes are re-projected immediately (`sourceMeshes`/`sourcePositionsRaw` are imported at room-geometry.js:8, so presumably yes).
5. **Trail point** (:1103-1116, specified in trails_volumes.md), then `updateSourceDecorations(id)` (§6), name/label refresh (:1119-1127), list UI.

Cadence: on every `source:update` (per object per spatial frame, batched per Tauri tick). No interpolation/easing of positions — meshes jump to the new position.

---

## 4. Colours

Materials (`scene/materials.js`):

| Name | Hex | Use |
|---|---|---|
| `sourceMaterial.color` :6 | `#ff7c4d` | default object colour when object colours are off |
| `sourceMaterial.emissive` :7 / `sourceDefaultEmissive` :44 | `#64210c` | default emissive |
| `sourceNeutralEmissive` :45 | `#10161d` | speaker selected, object not contributing |
| `sourceContributionEmissive` :46 | `#10311a` | speaker selected, object contributing |
| `sourceSelectedEmissive` :47 | `#9b7f22` | selected object |
| `sourceHotColor` :43 | `#ff3030` | mixed into selected-outline colour when a speaker is selected |
| `sourceOutlineColor` :48 | `#d9ecff` | ring default |
| `sourceOutlineSelectedColor` :49 | `#ffde8a` | ring when selected |
| `speakerSelectedColor` :42 | `#4dff88` | "contribution" tint lerped into object colour when a speaker is selected |

Per-object base colour `getObjectBaseColor(id)` (sources.js:219-238):
- tag `'A'` → `#ff8b6b`; tag `'B'` → `#62d7c7` (semantic colours, **always** applied even when object colours are off — `objectHasSemanticColor` :240).
- else palette index = `|Number(id)| % 16` if id is an integer string, else `FNV-1a-32(id) % 16` (`hashObjectId` :209: `h=2166136261; for each UTF-16 code unit: h ^= c; h = imul(h, 16777619)`; result `>>> 0`). Palette (:179): `#ff6b6b #4ecdc4 #ffe66d #5dade2 #af7ac5 #f5b041 #58d68d #ec7063 #48c9b0 #f4d03f #5499c7 #a569bd #eb984e #45b39d #7fb3d5 #f1948a`. Cached in `sourceBaseColors`.
- `useObjectColor = app.objectColorsEnabled || objectHasSemanticColor(id)` (:789). When false, `objectColor = sourceMaterial.color` (`#ff7c4d`).
- Trail colour = base colour with `offsetHSL(0, +0.04, +0.08)` (:246) (three.js HSL offset in sRGB).

---

## 5. Toggles / options (defaults, controls, persistence)

All persisted in `localStorage['spatialviz.effective_render_prefs']` as JSON (controls/room-geometry.js:98-131 write, :325-368 read). Panel controls live in the Display panel (`index.html:286-323`); the floating scene-fx bar (`controls/scene-effects-bar.js`) only mirrors the panel checkboxes/select by dispatching `change` (no logic of its own).

| `app.*` (state.js) | Default | Range / values | Control id (index.html) | i18n | Bar button | Effect entry point |
|---|---|---|---|---|---|---|
| `objectsVisible` :555 | `true` | bool | `showObjectsToggle` :289 | `display.showObjects` | `fxObjectsBtn` (+ flyout `fxObjectsMenu`) | `applyObjectsVisibility` sources.js:554; also mirrored to mpv via `invoke('mpv_overlay_set_objects')` and can be **overwritten by the engine** (`overlay:state` → mpvOverlay.js:60) |
| `objectDisplayMode` :562 | `'circle'` | `'circle' \| 'transparent-sphere' \| 'diffuse-sphere'` (persisted key `objectDisplayMode`) | `objectDisplayModeSelect` :297 | `display.objectDisplayMode`, `.circle/.transparentSphere/.diffuseSphere` | flyout items `data-value` | `updateSourceSelectionStyles` (listeners/trails-and-display-listeners.js:139-149) |
| `objectSphereSize` :563 | `0.07` | [0.03, 0.2] step 0.002 (persisted `objectSphereSize`) | `objectSphereSizeSlider` :305 | `display.objectSphereSize` | — | `updateSourceSelectionStyles` + `updateSourceDecorations` for all (:151-168) |
| `objectColorsEnabled` :556 | `false` | bool (persisted `objectColors`) | `objectColorsToggle` :293 | `display.objectColors` | — | `updateSourceSelectionStyles` + trail rebuild (:126-137) |
| `objectLabelsEnabled` :557 | `true` in state, but the checkbox in HTML is unchecked; the persisted value wins at load (:344) | bool (persisted `objectLabels`) | `objectLabelsToggle` :309 | `display.objectLabels` | `fxLabelsBtn` | `updateSourceDecorations` all (:170-181); mirrored to mpv `mpv_overlay_set_labels`; overwritable by `overlay:state` |
| `effectiveRenderEnabled` :551 | `false` | bool (persisted `enabled`) | `effectiveRenderToggle` :320 | `effectiveRender.title` | — | `refreshEffectiveRenderVisibility` (:108-114) → `updateEffectiveRenderDecoration` |
| `heatmapBandIndex` / `heatmapAllBands` :483/484 | `0` / `true` | int / bool | crossover band select (specified elsewhere in this directory) | — | — | selects which band's gains feed the effective-render centroid (§6.5) |
| `showObjectDetails` :558 | `true` | bool | `showObjectDetailsToggle` | `display.showObjectDetails` | — | **pure UI** (list rows) |

Flyout behaviour (scene-effects-bar.js:82-97): picking a mode also turns `showObjectsToggle` on if it was off.

---

## 6. Per-object renderables

All created once per object in `getSourceMesh` (sources.js:981-1031) and added directly to `scene` (not parented to the mesh, except conceptually the halo via `userData`). Removed/disposed in `removeSource` (:1300-1371).

Render-order summary (three.js sorts transparent objects by `renderOrder`, then far→near):

| Element | renderOrder | depthTest | depthWrite | blending |
|---|---|---|---|---|
| sphere mesh | 0 (default) | true | **false** | normal |
| effective line | 11 | true | false | normal |
| effective marker | 12 | true | false | normal |
| diffuse halo sprite | 14 | false | false | **additive** |
| outline ring | 20 | false | false | normal |
| label sprite | 40 | false | false | normal, `alphaTest 0.25` |

### 6.1 Sphere mesh

- Geometry: `SphereGeometry(SOURCE_BASE_RADIUS = 0.07, 24, 24)` (materials.js:3, :20), shared.
- Material: `MeshPhysicalMaterial` cloned per object (materials.js:6-18): `color #ff7c4d`, `emissive #64210c`, `transparent`, `opacity 0.7`, `roughness 0.08`, `metalness 0.04`, `clearcoat 1.0`, `clearcoatRoughness 0.03`, `sheen 0.75`, `sheenRoughness 0.18`, `specularIntensity 1.6`, `reflectivity 0.8`. Per-clone overrides at creation (sources.js:983-989): `color = objectBaseColor`, `emissive = sourceDefaultEmissive`, `opacity = 0`, `depthWrite = false`. No env map (no IBL), so the physical model reduces to the analytic lights of §0 (direct + ambient + hemisphere + point). A Rust port needs: a lit sphere with strong glossy specular (roughness 0.08), a clearcoat lobe and a sheen lobe; if exact parity is not required, a GGX PBR sphere with those parameters is the target.
- Position: §3. Scale: uniform `levelScale × sphereSizeScale` (§7), i.e. visual radius = `objectSphereSize × levelScale`.
- Visibility: `mesh.visible = !metadataSilent` (:792); master switch forces `false` (:538).
- **Colour / opacity / emissive by mode and state** — computed in `updateSourceColorsFromSelection` (:776-853) then `updateSourceSelectionStyles` (:855-916). Let `mix = clamp01(sourceGains[id][selectedSpeakerIndex])` when a speaker is selected, `hasContribution = mix > 1e-6`, `baseOpacity = 0.7`, `C = objectColor`, `G = speakerSelectedColor (#4dff88)`.

| Mode | No speaker selected | Speaker selected, contributing | Speaker selected, not contributing |
|---|---|---|---|
| `circle` | color `C`; opacity **0.0** (sphere invisible, only ring shows) | color `lerp(C, G, min(0.68, 0.22 + 0.42·mix))`; opacity `max(0.7·(0.35 + 0.55·mix), 0.24)` | color `C`; opacity 0.0 |
| `transparent-sphere` | color `C`; opacity `max(0.7·0.82, 0.58) = 0.58` | color as above; opacity `max(0.7·(0.82 + 0.36·mix), 0.68)` | color `C`; opacity 0.58 |
| `diffuse-sphere` | color `C`; opacity 0.06 | color as above; opacity `max(0.07 + 0.08·mix, 0.07)` | color `C`; opacity 0.05 |

Emissive (:866-881), applied after the colour pass:

| Condition | emissive | emissiveIntensity |
|---|---|---|
| `circle`, selected object | `#9b7f22` | 1.0 |
| `circle`, speaker selected, contributing / not | `#10311a` / `#10161d` | 1.0 |
| `circle`, otherwise | `#64210c` | 1.0 |
| `transparent-sphere` | `material.color × (selected ? 0.72 : 0.42)` | selected ? 1.45 : 1.0 |
| `diffuse-sphere` | `material.color × (selected ? 0.46 : 0.26)` | selected ? 1.0 : 0.65 |

(For the sphere modes the emissive is derived from the *already tinted* colour, so the contribution tint propagates.)

### 6.2 Outline ring (circle mode only)

- `createSourceOutline` (:297-317): `LineLoop` of 64 points on the unit circle in local XY (`(cos a, sin a, 0)`), `LineBasicMaterial{color #d9ecff, transparent, opacity 0.98, depthTest false, depthWrite false}`, `renderOrder 20`. WebGL line width = 1 px (three.js ignores `linewidth`).
- Billboard: every frame `outline.quaternion.copy(camera.quaternion)` (app.js:352-354) — the ring plane faces the camera.
- Position = mesh position (:507). Scale (uniform) = `0.07 × max(0.5, levelScale) × sphereSizeScale × 1.08` (:504-508) → radius 8 % larger than the sphere.
- Visible iff `!metadataSilent && mode === 'circle'` (:506, :839, :903).
- Colour/opacity (:837-850, then :891-904 override):

| State | colour | opacity |
|---|---|---|
| no speaker selected, not selected object | `useObjectColor ? C : #d9ecff` | 0.98 |
| speaker selected, mix ≤ 1e-6 | `(useObjectColor ? C : #d9ecff)` (lerp to G by 0) | 0.15 |
| speaker selected, mix > 1e-6 | `lerp(base, G, 0.65·mix)` | `0.25 + 0.73·mix` |
| selected object, no speaker selected | `#ffde8a` | 1.0 |
| selected object, speaker selected | `lerp(#ff3030, #ffde8a, 0.55)` | 1.0 |

### 6.3 Diffuse halo sprite (diffuse-sphere mode only)

- Texture (:348-370): 128×128 canvas, radial gradient centred, radius 64 px, stops (t → white alpha): `0.0→1.0, 0.12→0.96, 0.34→0.54, 0.64→0.14, 0.86→0.03, 1.0→0.0`, sRGB colour space. Rust: generate the same 128² RGBA (or R8) texture procedurally with piecewise-linear alpha over `r = dist/64px`.
- `SpriteMaterial{map, color #ffffff, transparent, opacity 0, depthTest false, depthWrite false, blending Additive, toneMapped false}` (:373-382), `renderOrder 14`, camera-facing quad (three.js Sprite: always faces camera, world-space size, perspective attenuated).
- Position = mesh position (:517). Scale (quad side, world units) = `0.26 × mesh.scale.x × 2.15 = 0.559 × levelScale × sphereSizeScale` (:518) — the initial `0.24` (:386) is overwritten on the first decoration pass.
- Visible iff `!metadataSilent && mode === 'diffuse-sphere'` (:516, :885).
- Colour = `C`, lerped to `G` by `min(0.55, 0.2 + 0.4·mix)` when a speaker is selected and contributing (:826-829).
- Opacity (:830-834, :886-888): no speaker selected → 0.4; speaker selected → contributing `0.34 + 0.34·mix`, else 0.2; selected object → `max(current, 0.7)`. Other modes → 0.

### 6.4 Label sprite

`createLabelSprite(text)` = `createLabelSpriteBase(256, 96, 0.42, 0.16, '#ffffff', text)` (scene/labels.js:203-205, :173-201):
- Canvas 256×96 px, `CanvasTexture` with `LinearFilter` min/mag, no mipmaps, sRGB (:130-137).
- Drawing (`drawLabelTextToCanvas` :139-171): clear; `textAlign center`, `textBaseline middle`, `fillStyle labelColor (#ffffff)`. Single line (the object case): font `700 36px sans-serif` when `canvas.width >= 200` (true for 256) → drawn at `(128, 48)`. Multi-line (`\n`): `24px`, line height 24, first line weight 700 others 600, vertically centred block. (The SVG builder :101-121 is a legacy twin with the same numbers; not used at runtime.) **No background box** — text only, transparent elsewhere.
- Sprite: `SpriteMaterial{map, transparent, alphaTest 0.25, depthTest false, depthWrite false, toneMapped false}`; scale `(0.42, 0.16, 1)` world units; `frustumCulled false`; `renderOrder 40` (:176-187). Camera-facing, perspective-attenuated (default `sizeAttenuation`), anchor centre (default). Effective glyph height ≈ `36/96 × 0.16 = 0.06` scene units.
- Text = `formatObjectLabel(id)` = `objectBadge(id).code` (:175-177, :142-173): display name = `sourceNames[id]` trimmed (fallback id) with `^[av][_:-]` and `^obj[_:-]` stripped (:114-121); then code rules: `INJ` for the test object; `Ambience_X`→`X`; `Height_X_synth`→`X`; `Diffuse_X`→`X`; `Phantom_A_B`→`A·B`; `Phantom_A`→`A`; `DirectH_X`→`X↑`; `Direct_X`→`X`; else strip up to the first `_` (`Foo_Bar`→`Bar`), else the name. Not translated. Redrawn only when the text changes (`setLabelSpriteText` :295-313).
- Position: `updateSourceDecorations` sets the label **exactly at the mesh centre** (:500) — the text overlaps the sphere/ring. Exceptions: the pinned-OSC path (:1049) and the drag helpers (picking.js:205, :361) place it at `y + 0.12`, but `commitTargetScene` immediately calls `updateSourceDecorations` (:216) which resets it to the centre. Net: centred, except transiently during pin-hold with incoming OSC packets. (Inconsistency in the JS; a port should pick one — centre is what is seen almost always.)
- Visible iff `app.objectLabelsEnabled && !metadataSilent` (:499); creation default `app.objectLabelsEnabled` (:1013).
- `updateSpeakerLabelsFromSelection` (:315-324) is speaker-side (sets speaker label text = speaker id), listed here only because it lives in labels.js.
- `createSmallLabelSprite` (:207-209): 128×64 canvas, scale `(0.25, 0.12)`, colour `#d9ecff`, font `700 28px` (small canvas branch) — used by gizmos/room dimension guides (specified elsewhere in this directory).

### 6.5 Effective-render marker + line (toggle `effectiveRenderEnabled`)

Shows where the object is *actually* rendered (gain²-weighted speaker centroid).
- Position (`computeEffectiveRenderPosition` :394-427): `gains = sourceBandGains[id][heatmapBandIndex]` if present and non-empty, else `sourceGains[id]`; `P = Σ gain_i² · speakerMesh_i.position / Σ gain_i²` over `gain_i > 0` with an existing speaker mesh; null if no gains or `Σ ≤ 1e-9`. Uses **scene** positions of speakers. Note it always uses `heatmapBandIndex` (ignores `heatmapAllBands`).
- Marker (`createEffectiveRenderMarker` :319-331): `SphereGeometry(0.04, 18, 18)`, `MeshStandardMaterial{color #7ce7ff, emissive #0a2834, transparent, opacity 0.34, depthWrite false}`, `renderOrder 12`. Scale (uniform) = `max(0.035, mesh.scale.x × 0.12)` (:454) → effective radius `0.04 × that` (≈ 0.005 at unit scale — very small; this is what the code does). Selected object: opacity 0.68, emissive `#10566c`; else 0.34 / `#0a2834` (:458-459).
- Line (`createEffectiveRenderLine` :333-344): 2-point `Line` from mesh position to `P`, `LineBasicMaterial{color #7ce7ff, transparent, opacity 0.22, depthWrite false}`, `renderOrder 11`; opacity 0.44 when selected (:470); hidden if `|P − mesh| ≤ 0.01` (:463).
- Both hidden when toggle off, metadata-silent, or no centroid (:437-450). Updated on: every `updateSourceDecorations` (position/level events), `updateSourceGains`, `updateSourceBandGains`, selection changes (:906), toggle.

### 6.6 Trail

Created via `createTrailRenderable()` (trails.js, specified in trails_volumes.md). Object-side hooks only: `trail.line.visible = app.trailsEnabled && !metadataSilent` (:512), points appended in `updateSource` with `trailRmsDbfs` and `trailColor = captureTrailPointColor(mesh)` = `userData.objectTrailColor` (trails.js:109-119), skipped when `_noTrail`. `decayTrails` (:1189-1210): every ≥120 ms drop points older than `app.trailPointTtlMs` (7000 ms default) and rebuild.

### 6.7 Selected-object face shadows (adjacent; `scene/gizmos.js:299-347`, `speakers.js:1958-1999`)

Six discs `CircleGeometry(1, 24)` + `MeshBasicMaterial{color #000000, transparent, opacity 0.18, DoubleSide, depthWrite false, depthTest false}`, one per room face, rotated to lie on the face. Per frame (app.js:342) for the selected object at `p`: for each face, disc placed at the face plane (`±bound ∓ 0.01`) with the other two coords clamped to the room bounds; `t = clamp(1 − dist/span, 0.08, 1)`; scale `0.08 × (0.7 + 0.6·t)`; opacity `0.06 + 0.18·t`. Hidden when no object selected. (Speaker equivalent exists for the selected speaker.)

---

## 7. Level → size, decay

- `dbfsToScale(dbfs, min, max)` (:568): `n = (clamp(dbfs, −100, 0) + 100)/100`; `min + n·(max − min)`. Objects: `(0.5, 2.4)` on `rmsDbfs` (:579) → `levelScale ∈ [0.5, 2.4]`; speakers use `(0.65, 2.2)`.
- `applySourceLevel` (:578-585): stores `userData.levelScale`; **applies `mesh.scale = levelScale × sphereSizeScale` only when no speaker is selected**; with a speaker selected the scale is re-applied by `updateSourceColorsFromSelection` (:813/:822), i.e. on gain/selection events, not on meter events. Then `updateSourceDecorations` (ring/halo follow the level immediately in both cases via `levelScale`).
- Missing meter → `rmsDbfs` defaults −100 → scale 0.5.
- Decay (speakers.js:2410-2437, per frame): if no meter received for ≥ `METER_DECAY_START_MS = 250` ms, `peak/rms −= 45 dB/s × dt` down to −100, re-applying `applySourceLevel`. Constants state.js:625-626.

---

## 8. Update cadence

| Trigger | Work |
|---|---|
| `source:update` (per object, per spatial frame) | hydrate, position, trail point, `updateSourceDecorations` (label/ring/halo/effective positions + scales), label text |
| `source:meter` | `sourceLevels`, `applySourceLevel` → scale + decorations |
| `source:gains` / `source:band_gains` | contribution tint (if a speaker is selected: full `updateSourceSelectionStyles`), effective marker |
| selection change (`setSelectedSource`, `setSelectedSpeaker`) | `updateSourceSelectionStyles` for all objects |
| toggles | see §5 |
| every frame (app.js:336-365) | `decayTrails`, `decayMeters`, `enforceObjectsVisibilityIfHidden` (:533-549: forces every per-object visual invisible while `objectsVisible === false`), ring billboard quaternions, selected-object face shadows |

---

## 9. Interaction (picking.js)

- Pointer NDC from canvas rect (`core/render/projection-service.js:5-10`). `Raycaster` with `params.Line.threshold = 0.08` (materials.js:52).
- **Click** = pointerup within 6 px of pointerdown (:34). Targets (`getPickableSceneTargets` :116-128): visible source labels, meshes, outlines; then speaker meshes, labels, band bars. Speakers win first (`pickSpeakerFromIntersects` :130-155); then the first hit whose `userData.sourceId` is set (mesh or label; the outline has no `sourceId` so a ring-only hit does not select — :168) → `setSelectedSource(id)`, `setSelectedSpeaker(null)`. Empty hit → deselect both (:39-41). Sprites (labels) are picked on their full 0.42×0.16 quad; sphere on its geometry (even when opacity 0 in circle mode — the mesh is still visible/pickable).
- **Hover**: none (no hover feedback, no cursor change in these files).
- **Drag** (edit gizmos, `beginSpeakerDrag` :248-301): only when `resolveEditTarget()` (speakers.js:1699-1715) returns a target: selected speaker, or selected object whose name is a canonical channel with placement `'virtual'` (spatialize on). Polar mode (`activeEditMode === 'polar' && polarEditArmed`): hit ring/arc of `speakerGizmo`; cartesian mode: hit `cartesianGizmo` x/y/z handles. Drag math :303-364 (azimuth from ground-plane hit, `atan2(z, x)`, snap 1°/0.5° or 5°/2.5° by radial delta; elevation from the vertical plane through the azimuth; cartesian by ray-axis projection). Each move → `commitTargetScene` (:197-225): set mesh position, keep `channelEditPinPos` in sync, `updateSourceDecorations`, `updateSpeakerGizmo`, preview the editor; on release (:366-390) send `applyChannelPolar(name, az, el, max(0.01, dist))` and keep the pin for 600 ms.
- **Wheel** (:59-84): polar mode + armed + Ctrl/Shift → distance `±0.05` (`±0.01` with Shift) clamped [0.2, 2.0]; channels send OSC per tick, speakers do not.
- Orbit: `OrbitControls` left = rotate, middle = dolly, right = custom pan as lens shift (scene/setup.js:85-197), damping 0.06; disabled while dragging a gizmo.

---

## 10. Mute / solo (visual side)

No direct 3D visual for muted objects in these files (no dimming, colour, or hiding). Effects on the viewport:
- `objectMuted` excludes the object from the energy field (`collectActiveObjects`, object-energy-shared.js:283).
- Solo (`toggleSolo`, mute-solo.js:224-293) mutes all others and **selects** the solo'd object (`setSelectedSource`), which drives the selected visuals (§6). `setSelectedSource` while a solo is active re-targets the solo to the newly selected object (sources.js:947-960).
- Meter bar scale for the list: −60…+6 dBFS (`dbToMeterPercent`, mute-solo.js:52-59) — UI only, but reused for the "contribution percent" readouts.

---

## 11. Energy colormap (object-energy-shared.js, shared with the object field volume)

- `SILENT_RMS_DBFS = −100`; `objectEnergyLinear(rms) = rms ≤ −100 or non-finite ? 0 : 10^(rms/10)` (:171-177).
- `collectActiveObjects()` (:280-309): for each `sourcePositionsRaw` entry not in `objectMuted`, energy from `rmsDbfs` or, when `!heatmapAllBands` and ≥2 band meters, `bandRmsDbfs[min(heatmapBandIndex, n−1)]` (:268-275); emits `{x, y, z, energy}` in **ADM normalised** coordinates (not scene, not room-warped).
- `OBJECT_ENERGY_COLORMAPS = ['heatmap','blueWhite','whiteRed','red','custom']` (:51), index = GLSL `uColormap`. `MAX_CUSTOM_STOPS = 8`. `app.objectEnergyColormap` default `'blueWhite'` (state.js:504); custom stops default blue(0)→green(0.5)→red(1) (:510-519).
- `HEATMAP_STOPS` (:35-41): `(0.00: 0,0,1) (0.25: 0,1,1) (0.48: 0,1,0) (0.70: 1,1,0) (1.00: 1,0,0)`, linear RGB interpolation.
- `VOLUME_GAMMA_RANGE` (:159-162): accumulate `[1, 10]` step 0.1 default 2.5; mip `[0.2, 3]` step 0.05 default 0.8.

GLSL (`HEATMAP_GLSL` :116-153) → WGSL (same names; `uColormap: i32`, `uCustomStopCount: i32`, `uCustomStops: array<vec4<f32>, 8>` as `(pos, r, g, b)`):

```wgsl
const MAX_CUSTOM_STOPS: i32 = 8;

fn customStopsColor(t: f32) -> vec3<f32> {
  let n = uniforms.uCustomStopCount;
  if (n <= 0) { return vec3<f32>(t); }
  if (t <= uniforms.uCustomStops[0].x) { return uniforms.uCustomStops[0].yzw; }
  for (var i: i32 = 0; i + 1 < MAX_CUSTOM_STOPS; i = i + 1) {
    if (i + 1 >= n) { break; }
    let a = uniforms.uCustomStops[i];
    let b = uniforms.uCustomStops[i + 1];
    if (t <= b.x) {
      let f = select(0.0, (t - a.x) / (b.x - a.x), b.x > a.x);
      return mix(a.yzw, b.yzw, f);
    }
  }
  return uniforms.uCustomStops[n - 1].yzw;
}

fn heatmapColor(value: f32) -> vec3<f32> {
  let t = clamp(value, 0.0, 1.0);
  if (uniforms.uColormap == 4) { return customStopsColor(t); }              // custom stops
  if (uniforms.uColormap == 3) { return vec3<f32>(1.0, 0.0, 0.0); }         // red: alpha-only
  if (uniforms.uColormap == 2) { return vec3<f32>(1.0, 1.0 - t, 1.0 - t); } // white -> red
  if (uniforms.uColormap == 1) { return vec3<f32>(t, t, 1.0); }             // blue -> white
  var c: vec3<f32>;                                                         // heatmap rainbow
  if (t < 0.25) {
    c = mix(vec3<f32>(0.0, 0.0, 1.0), vec3<f32>(0.0, 1.0, 1.0), (t - 0.00) / 0.25);
  } else if (t < 0.48) {
    c = mix(vec3<f32>(0.0, 1.0, 1.0), vec3<f32>(0.0, 1.0, 0.0), (t - 0.25) / 0.23);
  } else if (t < 0.70) {
    c = mix(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 1.0, 0.0), (t - 0.48) / 0.22);
  } else {
    c = mix(vec3<f32>(1.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), (t - 0.70) / 0.30);
  }
  return c;
}
```

CPU twin `objectEnergyColor(colormap, value, target, customStops)` (:96-111) is identical (alpha always encodes the value, handled by the caller). The volume shader that consumes this (blending, gamma, ray-march) is in `scene/energy-volume-core.js` / `object-energy-volume.js` (specified elsewhere in this directory). `makeCellIndexer`/`signaturesEqual` (:192-257) are gain-table plumbing (specified elsewhere in this directory).

---

## 12. Objects master switch

`app.objectsVisible === false` → `enforceObjectsVisibilityIfHidden` (:533-549) every frame sets `visible=false` on all meshes, halos, labels, outlines, trails, effective markers/lines. Turning back on runs `updateSourceColorsFromSelection`, `updateSourceSelectionStyles`, `updateSourceDecorations` for all (:554-562). It does not touch `objectLabelsEnabled`/`trailsEnabled`.

---

## 13. Not viewport (pure UI) — schedule later

- sources.js: `objectBadge` type (list icon), `getObjectUiAccent`/`applyObjectItemColor` (:273-291, row accent `--object-accent`, id strip `#edf5ff`), contribution helpers and band bars (:603-770), `sourceCallbacks.*UI`, list scroll-into-view (:969-973), `updateSourceSize` (list gauges).
- mute-solo.js: everything except §10 (meter bars, peak cursor, `formatLevel`, gain sends, `toggleMute`/`toggleSolo` state logic and Tauri invokes).
- scene/labels.js: `window.omniphonyDebug.labelStats` debug handle (:4-86).
- state.js: all non-object fields; `dirty*` sets are UI-flush batching (flush.js).
- controls/scene-effects-bar.js: entire file is DOM (buttons, flyouts, `aria-*`), no rendering.
- coordinates.js: `formatNumber`, `decomposePosition`, `formatPosition` (:505-564) — text readouts.

---

## 14. Unclear / noteworthy from the code

1. Effective-render marker radius ends up ≈ 0.005 scene units (§6.5) — faithful to the code, possibly unintended.
2. Label y-offset inconsistency (§6.4): centred by decorations vs `+0.12` in pin/drag paths.
3. `UpdateMeta` re-emit omits `kind` (osc_listener.rs:2217) — no visual impact.
4. `objectLabelsEnabled` default `true` in state vs unchecked checkbox in HTML; persisted value normally decides.
5. Whether `sourceMeshes` positions are re-projected immediately on a room-ratio change (room-geometry.js) was not verified here (see scene.md).
6. In `circle` mode the sphere is invisible (opacity 0) yet still raycast-pickable; the ring itself is not a selection target.
