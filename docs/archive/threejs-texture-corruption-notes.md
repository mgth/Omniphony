# Three.js Texture Corruption Notes

## Problem summary

Intermittent rendering corruption still occurs in `omniphony-studio`:

- text sprites in the 3D view are sometimes replaced by what looks like a copy of the 3D framebuffer/view itself
- diffuse trail particles can become square and opaque instead of soft round sprites
- the issue is not reliably reproducible, but it eventually appears during normal use

The failure mode strongly suggests a texture/resource corruption path affecting UI/text/trail textures rather than ordinary geometry.

## Observed symptom clusters

### Labels

- speaker/object text stops looking like text
- the sprite shows unrelated image content, often resembling the scene itself
- the corruption is persistent once it appears

### Trails

- diffuse trail particles lose their soft alpha falloff
- particles become visible square quads
- opacity handling looks broken or bypassed

## Relevant history

### 2026-03-23: modular refactor

Commit: `509e5fd`  
Message: `Split monolithic app.js (9166 lines) into 27 focused ES modules`

After the split, the relevant modules were:

- `omniphony-studio/src/scene/labels.js`
- `omniphony-studio/src/trails.js`

At this point:

- labels used `THREE.Texture(Image)` fed from an SVG data URL
- diffuse trails used a `CanvasTexture` point sprite

This is important because the later regression did not originate in the modular split itself. The split already contained the SVG-image label path.

### 2026-03-24: regression reintroduced

Commit: `4fd41aa`  
Message: `Continue adaptive resampling and runtime control updates`

This commit touched both files again and reintroduced the more fragile texture paths:

- `scene/labels.js` went back to `document.createElement('canvas')` + `THREE.CanvasTexture`
- `trails.js` used a generated point texture again, this time through `THREE.DataTexture`

This is the first clear historical point where the hardened label path disappeared.

### 2026-03-27: hardening attempt

Commit: `7bc8430`  
Message: `Fix label corruption and add object display colors`

This commit applied the following mitigation:

- labels: replaced `CanvasTexture(canvas)` with `Texture(Image)` loaded from SVG data URLs
- trails: removed external point textures entirely for diffuse trails and computed the round alpha mask directly in the fragment shader using `gl_PointCoord`

The intent was:

- avoid canvas-backed label textures
- avoid canvas/data textures for diffuse particles
- keep only plain image textures for text and procedural shader masking for diffuse trails

## What has been tried

### Attempt 1: canvas label textures

Implementation shape:

- `document.createElement('canvas')`
- `new THREE.CanvasTexture(canvas)`
- text drawn with `CanvasRenderingContext2D`
- `texture.needsUpdate = true`

Result:

- bug present
- when corruption happens, text sprites can display unrelated image content

### Attempt 2: SVG image label textures

Implementation shape:

- build SVG markup from text
- encode as `data:image/svg+xml`
- set `image.src = dataUrl`
- use `new THREE.Texture(image)`
- update on `image.onload`

Result:

- this was intended as the hardened path
- it did not eliminate the bug completely in real usage
- however, the historical regression path is still significant because it removed one known-risk texture type from labels

### Attempt 3: textured diffuse trail particles

Implementation shape:

- generated sprite texture
- seen historically with both `CanvasTexture` and `DataTexture`
- fragment shader sampled `texture2D(pointTexture, gl_PointCoord)`

Result:

- trail particles could become square and opaque

### Attempt 4: procedural diffuse trail particles

Implementation shape:

- no external texture for diffuse particles
- alpha mask derived directly from `gl_PointCoord` in the fragment shader

Result:

- intended to remove one more texture-corruption surface
- did not definitively solve the full issue

## Current code state

As of the current working tree:

- labels are still using the SVG `Image` -> `THREE.Texture` path in `omniphony-studio/src/scene/labels.js`
- diffuse trails are still using the procedural `gl_PointCoord` mask path in `omniphony-studio/src/trails.js`

So the bug persisting today means the current remaining trigger is likely elsewhere than the original obvious `CanvasTexture` label path.

## Most likely remaining hypotheses

### GPU/WebGL context issue

Possible mechanism:

- context loss or partial context restoration
- stale texture handles or framebuffer state reused incorrectly
- texture contents appearing replaced by unrelated render output

Why this still fits:

- the visual result resembles resource aliasing or stale GPU state more than a simple CPU-side text rendering mistake
- the problem is intermittent and hard to reproduce deterministically

### Three.js / browser / driver interaction

Possible mechanism:

- a renderer bug triggered by a specific sequence of texture updates, sprite use, transparency, or Tauri/WebView GPU behavior
- platform-specific instability in the embedded WebView graphics stack

Why this still fits:

- the failure is long-running and sporadic
- the corruption affects visual resources that are otherwise logically unrelated

### Context-loss handling is missing or incomplete

Possible mechanism:

- the app does not explicitly monitor `webglcontextlost` / `webglcontextrestored`
- after a context event, some scene resources may not be recreated the way the app expects

Why this still fits:

- this was already identified as a worthwhile next diagnostic step
- it matches the "comes back eventually" report pattern

## Facts we do know

- the bug is real and longstanding
- it affects both text sprites and diffuse trail visuals
- there is no reliable reproduction recipe yet
- a regression clearly happened on 2026-03-24 when the hardened label path was replaced by `CanvasTexture`
- the 2026-03-27 hardening reduced obvious risk factors but did not fully eliminate the issue

## New finding: DOM panel composition can trigger it

A much stronger trigger was identified during later debugging:

- opening/closing certain collapsible DOM panels above the Three.js canvas in Tauri/WebView can trigger the corruption very reliably
- `Room geometry` and `OSC config` were both observed to do this with the old `display: none` <-> `display: flex` pattern
- replacing those toggles with a non-destructive hide/show strategy based on:
  - `max-height`
  - `opacity`
  - `overflow: hidden`
  - `pointer-events: none`
  significantly reduced or removed the immediate trigger

This is important because it shifts part of the suspicion away from "labels/trails only" toward:

- WebView compositing
- DOM/canvas stacking behavior
- or a Tauri/WebKit/WebGL interaction triggered by abrupt layout/composition changes above the canvas

In other words, the bug may still manifest in Three.js resources, but a reliable trigger can be a DOM visibility pattern rather than a purely Three.js-side resource update.

## Practical rule for the Studio frontend

For collapsible panels rendered over the 3D canvas:

- avoid `display: none` <-> `display: flex/block/grid` toggles when possible
- prefer keeping the panel mounted and switching visibility with `max-height`, `opacity`, `overflow`, and `pointer-events`

This should be treated as a defensive workaround for the current Tauri/WebView rendering bug, even if the ultimate root cause is lower in the stack.

## Recommended next diagnostics

1. Add logging for `webglcontextlost` and `webglcontextrestored`.
2. Record renderer / platform info when the app starts:
   - WebGL version
   - renderer string
   - vendor string
   - whether running under Tauri on the affected machine
3. When corruption is observed, capture:
   - OS
   - GPU
   - whether trails were in `diffuse` or `line`
   - whether the corrupted text was speaker labels, object labels, or both
   - whether the issue appeared after resize, sleep/wake, monitor changes, or long runtime
4. When investigating UI triggers, test whether the panel uses `display:none/flex` toggles over the canvas and convert it first before changing more Three.js code.
5. If needed, add a temporary debug action to forcibly rebuild all label textures and trail materials after corruption to see whether the problem is recoverable without full reload.

## Files historically involved

- `omniphony-studio/src/scene/labels.js`
- `omniphony-studio/src/trails.js`
- `omniphony-studio/src/sources.js`
- `omniphony-studio/src/app.original.js`

## Key commits

- `509e5fd` `Split monolithic app.js (9166 lines) into 27 focused ES modules`
- `4fd41aa` `Continue adaptive resampling and runtime control updates`
- `7bc8430` `Fix label corruption and add object display colors`
