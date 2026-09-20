# Omniphony Studio — LEFT overlay panel specification (phase 2, egui port)

Scope: everything inside `#overlay` of the web Studio (`omniphony-studio/src/index.html`
lines 41–773) plus the shared machinery those panels rely on (`state.js`, `flush.js`,
`i18n.js`, `controls/inline-help.js`, `runtime-connection.js`, `options-binder.js`).
All paths below are relative to
`omniphony-studio/`
(`src/…` = web frontend, `src-tauri/src/…` = Tauri host). Line numbers are from the
`feat/studio-egui-panels` worktree at the time of writing.

Reading conventions used in this document:

* **Label key** = the `data-i18n` key applied to the visible text (`i18n.js:142–145`, `textContent = t(key)`).
  **Help key** = the `data-help-i18n` key that makes the label clickable and opens an inline help
  panel (see §0.5). **Title key** = `data-i18n-title` (tooltip). **HTML key** = `data-i18n-html`
  (`innerHTML = t(key)`). The English strings are quoted from `src/i18n/en.json`.
* "Optimistic" = the JS writes the new value into local state *before* the renderer echoes it.
  "Echo-driven" = nothing is written locally; the UI is re-populated from the next state
  snapshot / event.
* "Runtime-locked" = disabled while `app.oscStatusState !== 'connected'` by the generic sweep
  in `runtime-connection.js` (§0.3). Almost every control in `#overlay` is runtime-locked; the
  exemptions are listed explicitly.
* All `invoke('…')` names are Tauri commands in `src-tauri/src/commands/*.rs`; the OSC address each
  one emits is given where known.
* Every `type="checkbox"` inside `.switch-row`, `.inline-toggle` or `.control-row` is styled as a
  **34×18 px pill switch** (`app.css:1056–1098`): track `rgba(255,255,255,0.18)`, knob 12 px
  `#d9ecff`, checked track `rgba(80,200,120,0.45)`, knob slides 16 px. There are no visible
  square checkboxes anywhere in the panel (project rule: switches, not checkboxes).

---

## 0. Cross-cutting machinery

### 0.1 The overlay container (`index.html:41–774`, `app.css:40–275`)

```
#overlay                       position:fixed; top/left 1rem; height calc(100vh - 2rem); width var(--panel-width-left)
  #aboutOpenArea               brand row (§1.1)              flex 0 0 auto
  #profileRow                  profile picker (§1.2)         flex 0 0 auto
  #profileNameRow              inline name editor, hidden    flex 0 0 auto
  #overlayScroll               THE scrolling column          flex 1 1 auto; overflow-y:auto; padding-right var(--overlay-scroll-inset)=1.35rem
    #updatePanelRoot           §2
    #oscPanelRoot              §3
    #inputPanelRoot            §4  (mounted into #audioInputPanelMount by ui/input-panel.js)
    #twoDSourcesPanelRoot      §5  (.info-section.section-collapsed)
    #roomGeometryPanelRoot     §6  (.info-section.section-collapsed)
    #displayPanelRoot          §7
    #drcPanelRoot              §8
    #objectsPanelRoot          §9.1–9.3
  #objectTestEditSection       §9.5 pinned BELOW the scroll, display:none by default, max-height 45vh, own scroll
  #channelEditSection          §9.4 pinned BELOW the scroll, display:none by default, max-height 45vh, own scroll
```

* Overlay visual: `background rgba(0,0,0,0.65)`, `backdrop-filter blur(8px)`, 1 px border
  `rgba(255,255,255,0.2)`, radius 12 px, padding `0.75rem 1rem`, base font 14 px, gap 0.4 rem.
* The two pinned editors are mutually exclusive: the channel editor shows when the selected
  object is a fixed (bed) channel; the injection editor shows when the selected object is the
  injected test source (`speakers.js:525–528`). Neither ever changes the outer overlay
  geometry (project rule): they cap at 45 vh and scroll internally.
* Collapse/resize (`ui/side-panels.js`): a hamburger button `#leftPanelCollapseBtn`
  (`.panel-side-collapse-btn-left`, top-right of the panel, title "Toggle controls") collapses
  the whole panel to a strip; a 10 px drag handle on the right edge resizes it (double-click
  resets). Width/collapsed state persist in localStorage (owned by
  `ui/layout/overlay-layout-state.js`, not read for this spec). Every layout change emits
  `omniphony:overlay-layout-changed`, which `visual-recovery.js` uses to rebuild WebGL
  resources — a three.js concern with no egui equivalent (out of scope).

### 0.2 Section accordions (`modals.js:194–349`, `room-geometry.js:800–824`, `app.css:349–403, 1383–1398`)

Every collapsible section follows the same pattern:

| Section | root | content | toggle button | state field | summary element shown while collapsed |
|---|---|---|---|---|---|
| Fixed-channel sources | `#twoDSourcesPanelRoot` | `#twoDSourcesBody` (`display:grid`/`none`) | `#twoDSourcesToggleBtn` | `app.twoDSourcesSectionOpen` | `#twoDSourcesSummary` |
| Room geometry | `#roomGeometryPanelRoot` | `#roomGeometryForm` (`.open`) | `#roomGeometryToggleBtn` | `app.roomGeometryExpanded` | `#roomGeometryHeaderSummary` |
| Display | `#displaySection` | `#displaySectionContent` (`.conditional-params.open`) | `#displaySectionToggleBtn` | `app.displaySectionOpen` | — |
| DRC | `#drcSection` | `#drcSectionContent` | `#drcSectionToggleBtn` | `app.drcSectionOpen` | `#drcSummary` |
| Audio input | `#audioInputSection` | `#inputSectionContent` | `#inputSectionToggleBtn` | `app.inputSectionOpen` | `#inputSummary` |

* Toggle button glyph: `▸` (U+25B8) collapsed, `▾` (U+25BE) open. Button class
  `.panel-toggle-btn` (min-width 1.65 rem, height 1.2 rem, font 10 px inside `#overlay`).
* Root gets class `section-collapsed` while collapsed (tighter margins only: cosmetic).
* `.conditional-params` = `max-height:0; opacity:0; pointer-events:none` until `.open`
  (`max-height:none`). `#twoDSourcesBody` uses `display` instead, and is additionally capped at
  `max-height:min(52vh,34rem)` with internal scroll (`app.css:407–413`).
* All sections start **collapsed** at boot (`app.js:246–254`). `collapseRuntimeSections()`
  (`modals.js:342`) collapses telemetry/display/drc/audio-output/input/renderer.
* The `.panel-header` layout is: `.panel-header-main` (title `.info-title.panel-title`, 11 px,
  + optional `.panel-summary`, 10 px `#9eb4c8`, single line ellipsised) on the left, then
  optional `i` info button (`.info-icon-btn`, 17 px circle) and the toggle on the right.
* Section toggles for input/room/display are in `PANEL_TOGGLE_IDS` of the runtime lock: they are
  **disabled while disconnected** (`runtime-connection.js:34–41, 69–74`). The twoD/DRC toggles are
  NOT in that list, so they are locked like every other button (same net effect).

### 0.3 Runtime connection lock (`runtime-connection.js`)

`syncRuntimeConnectionLock()` runs on every OSC status render and on every snapshot. When
`app.oscStatusState !== 'connected'` it sets `disabled=true` on **every** `button/input/select/
textarea` inside `#overlay` and `#speakersOverlay`, remembering the element's previous disabled
state in `data-runtime-lock-prev-disabled` so it can be restored exactly on reconnect.
Exempt ids (stay usable offline):

* OSC: `oscConfigToggleBtn oscHostInput oscRxPortInput oscListenPortInput oscBridgePathInput
  oscBridgeBrowseBtn oscConfigApplyBtn oscServiceBtn oscRestartServiceBtn oscRestartPipewireBtn
  oscLaunchRendererBtn oscInfoBtn oscMeteringToggle mpvOrenderToggle`
* Panel toggles: `inputSectionToggleBtn roomGeometryToggleBtn displaySectionToggleBtn
  audioOutputSectionToggleBtn telemetryGaugesToggleBtn rendererSectionToggleBtn` (these are
  *forced* disabled offline, see above)
* `leftPanelCollapseBtn rightPanelCollapseBtn`, `rendererTabRendererBtn rendererTabBinauralBtn`.

Consequence for the port: the update toggle, profile picker, locale select, display switches,
etc. are all greyed while no renderer answers, even though many of them are purely local. Port
this rule as-is unless told otherwise (it is a deliberate "one gate" design).

### 0.4 Producer-capability visibility classes (`init.js:76–91`, `app.css:2702–2720`)

Set on `<body>` from `app.producerCapabilities` (handshake payload; `null` before the first one):

| class | condition | effect on the left overlay |
|---|---|---|
| `cap-no-audio` | capabilities known && no `audio` domain | hides audio-output rows (right panel) |
| `cap-no-resampler` | known && no `adaptive_resampling` controlConfig | hides `#latencySection` (right panel) |
| `cap-embedded` | known && `variant === 'embedded'` && status `connected` | `#oscPanelRoot .osc-config-actions { display:none !important }` (Connect/service/launch buttons hidden). Re-evaluated on every status change so the buttons return on disconnect. |

Helpers in `state.js:589–618`: `hasProducerDomain(d)`, `hasControlConfig(k)`,
`producerVariant()` (`'standalone'` default), `producerHost()` (`'cli'`/`'mpv'`/null),
`isEmbeddedProducer()`, `supportsRealtimeKey(k)`.

### 0.5 i18n (`i18n.js`)

* Locale preference stored in localStorage key **`spatialviz.locale`**; values `auto|en|fr|de|ja|es|it|pt-BR|zh-CN`.
  `auto` resolves via `navigator.languages` (prefix match, `pt-br`/`zh-cn` exact-prefix), default `en`.
* Non-English tables are `{...en, ...xx}` so every key falls back to English; `t(key)` returns
  the key itself when unknown (`i18n.js:106–108`). `tf(key, {a:…})` substitutes `{a}` placeholders
  (missing → empty string).
* `applyStaticTranslations()` (`i18n.js:124–164`) walks the DOM once per locale change and applies:
  `data-i18n` → `textContent`; `data-i18n-title` → `title`; `data-i18n-html` → `innerHTML`;
  `data-i18n-placeholder` → `placeholder`; `data-i18n-aria-label` → `aria-label`. It also
  re-labels `#localeSelect` options as `"English / Native"` (or just the word when identical,
  e.g. "Auto", "English") from `LOCALE_OPTION_SPECS` (`i18n.js:27–37`), and sets
  `document.documentElement.lang`.
* `setLocale(v)` persists, re-applies static translations, then fires `onLocaleChange`
  listeners. `app.js:170–190` re-renders every dynamic readout (OSC status, room geometry,
  loudness, lists, …); `speaker-band-select.js:88` and `object-test.js:1549` register their own.
* Anything built in JS (object rows, generated param labels, status lines) is **not** covered by
  `data-i18n` and must be re-rendered on locale change explicitly.

### 0.6 Inline help affordance (`controls/inline-help.js`, wired once at boot by `bootstrap-ui.js:14`)

* Any element with `data-help-i18n="<key>"` whose translation exists (`t(key) !== key`) becomes a
  help trigger: `role=button`, `cursor:pointer`, **dotted underline** `underline dotted
  rgba(217,236,255,0.4)`, offset 2 px (`inline-help.js:57–61`).
* Clicking it toggles a help panel inserted **right after the row** — the row being
  `closest(data-help-anchor selector)` if given, else `closest('.control-row, .inline-toggle')`,
  else the parent. Panel look (`makeHelpPanel`, lines 36–50): class `generated-param-help`,
  11 px, line-height 1.35, colour `#d9ecff`, background `rgba(120,200,255,0.10)`, border
  `1px solid rgba(120,200,255,0.30)`, radius 6 px, padding `0.3rem 0.45rem`, margin-top 0.15 rem.
* Exactly one help panel is open at a time across the whole UI; opening another closes it; any
  click outside closes it (`closeInlineHelp`, document click listener).
* Idempotent (`data-help-wired`), so re-running after re-render is safe.
* Modal-style "i" buttons: `bindModalOpenClose` (`listeners/modal-and-toggle-listeners.js:48–80`)
  **hides** the `i` button and instead makes the section/parameter *name* the trigger (same
  dotted-underline affordance via `markClickableName`) whenever a name can be resolved
  (`resolveModalTrigger`: explicit `triggerSelector`, else the `label/span` inside
  `.title-with-info`, else the `.panel-title/.info-title` inside `.panel-header`). Modals are
  `.info-modal` overlays (`app.css:1341–1381`: fixed full-screen scrim `rgba(0,0,0,0.45)`, card
  `min(460px, 100vw-2rem)`, background `rgba(14,16,22,0.96)`), closed by their Close button, a
  click on the scrim, or **Escape** (all modals at once, `modal-and-toggle-listeners.js:317–339`).

### 0.7 `state.js` — the `app` object fields read by the left overlay

Only fields used by §1–§9 are listed (`state.js:100–579`). Defaults in parentheses.

Connection / renderer identity: `oscStatusState ('initializing')` ∈ initializing|connected|reconnecting|error;
`oscSnapshotReady (false)`; `oscMeteringEnabled (false)`; `oscConfigBaselineKey ('')`;
`oscConfigAutoOpenTimer`, `oscLaunchPending (false)`, `oscLaunchPendingTimer`,
`oscConfiguredOrenderPath ('')`; `orenderServiceInstalled/Running (false)`,
`orenderServiceManager (null)`, `orenderServicePending (false)`; `producerCapabilities (null)`,
`producerSession (null)`; `renderBridgePath, renderConfigPath, renderConfigStatus,
renderVersion, renderExecutable, expectedOrenderPath, renderAbi, renderBridgeError (all null)`;
`orenderInputPipe (null)`.

Input: `inputMode ('pipe_bridge')`, `inputModeDirty`, `inputActiveMode ('pipe_bridge')`,
`inputApplyPending`, `inputApplyAwaitingAck`, `inputBackend/Channels/SampleRate/Node/Description/
StreamFormat/Error (null)`, `liveInput {backend:'pipewire', node:'', description:'', layout:'',
clockMode:'dac', channels:2, sampleRate:192000, map:'7.1-fixed', lfeMode:'object'}`,
`liveInputClockModeDirty`, `lastAutoOpenedInputError (null)`.

Fixed-channel sources: `options ({})` + `optionsSchema ([])` (read via `getLiveOption`),
`objectGenerators ([])`, `objectGeneratorParams ({})`, `objectGeneratorLayoutHasHeight (true)`,
`phantomSchema ([])`, `phantomParams ({})`, `fixedChannelCatalog ([])`,
`fixedChannelProcessing ({stream:'idle', labels:[], phantom:'no_stream', height:'no_stream'})`,
`virtualBed (null)`, `virtualBedMaterialized (false)`.

Room: `roomRatio {width:1,length:2,height:1,rear:1,lower:0.5,centerBlend:0.5}`,
`roomGeometryExpanded (false)`, `roomGeometryBaselineKey ('')`, `roomGeometryApplyTimer`,
`metersPerUnit (1.0)`; `renderBackendState.frozenRoomRatio` via `isRoomRatioFrozen()`;
`currentLayoutKey`, `currentLayoutSpeakers ([])`, `currentLayoutCutoffs ([])`, `layoutsByKey` (Map).

Display prefs (all persisted, §7.13): `trailsEnabled (true)`, `trailRenderMode ('diffuse')`,
`trailPointTtlMs (7000)`, `trailTeleportThreshold (0.5)`, `objectsVisible (true)`,
`objectColorsEnabled (false)`, `objectLabelsEnabled (true)`, `showObjectDetails (true)`,
`objectDisplayMode ('circle')`, `objectSphereSize (0.07)`, `effectiveRenderEnabled (false)`,
`speakerLabelsEnabled (false)`, `speakerBandBarsEnabled (false)`, `speakerFaceListenerEnabled (false)`,
`speakerSize (0.08)`, `speakerHeatmapVolumeEnabled (false)`, `speakerHeatmapVolumeColormap ('heatmap')`,
`heatmapBandIndex (0)`, `heatmapAllBands (true)`, `globalEnergyHeatmapEnabled (false)`,
`globalEnergyHeatmapScaleDb (6)`, `discontinuityHeatmapEnabled (false)`,
`discontinuityHeatmapMode ('gain')`, `discontinuityHeatmapScale (0.5)`,
`objectEnergyHeatmapEnabled (false)`, `objectEnergyColormap ('blueWhite')`,
`objectCustomGradientStops`/`speakerCustomGradientStops` (3 stops blue→green→red),
`objectEnergyVolumeMix (0.6)`, `objectEnergyVolumeGammaAccumulate (4)`,
`objectEnergyVolumeGammaMip (3)`, `objectEnergyHeatmapResolution (64)`,
`objectEnergyHeatmapFalloffRadius (0.5)`, `objectEnergyHeatmapOpacity (1)`, `volumeRefreshMs (160)`,
`volumeSmoothInterpolation (false)`, `objectEnergyHeatmapBandCount (12)`, `vbapCartesianFaceGridEnabled (false)`.

DRC / loudness: `drcMode (null)`, `supportedDrcModes ([])`, `drcGain (1.0)`, `drcWeight (1.0)`,
`loudnessEnabled/Source/Gain (null)`.

Objects: `selectedSourceId (null)`, `selectedSpeakerIndex (null)`, `channelEditCoordMode
('cartesian')`, `cartesianEditArmed/polarEditArmed (false)`, `activeEditMode ('polar')`,
`isDraggingVirtualBed`, `channelEditPinId/Pos/Until`, `lastSpatialFrameAt (0)`.
Exported Maps/Sets: `sourceMeshes, sourceNames, sourceTags, sourcePositionsRaw, sourceSizes,
sourceLevels, sourceLevelLastSeen, sourceGains, sourceBandGains, sourceTrails,
sourceDirectSpeakerIndices, objectMuted, objectManualMuted, objectItems, speakerItems` and
`masterLevel` (live binding).

Constants: `METER_DECAY_START_MS = 250`, `METER_DECAY_DB_PER_SEC = 45`,
`AUDIO_SAMPLE_RATE_PRESETS`, `isLinux` (navigator UA sniff).

`getLiveOption(key)` (`state.js:638–643`): `app.options[key]` if defined, else the `default` of
the matching entry in `app.optionsSchema`, else `undefined`. Callers must treat `undefined` as
"pre-connect, keep the baked HTML default".

### 0.8 `flush.js` — dirty flags and what they repaint

`scheduleUIFlush()` coalesces into one `requestAnimationFrame(flushUI)`. `flushUI` (lines 80–234)
processes, in order:

| dirty set/flag | repaint | left-overlay element affected |
|---|---|---|
| `dirtyObjectMeters` (ids) | `updateMeterUI(entry, sourceLevels.get(id))` + `updateObjectContributionUI` | object rows: dB text, meter fill, peak cursor, contribution overlay, band bars |
| `dirtySpeakerMeters` | speaker rows (right panel) | — |
| `dirtyObjectPositions` | `axisElems[axis].textContent = "axis:value"` from `decomposePosition`, then `applyObjectPositionIcon` | object row coords + thumbnail |
| `dirtyObjectLabels` | `applyObjectIdentity(entry,id)` | object row badge/icon |
| `dirty.masterMeter` | `updateMasterMeterUI` | right panel |
| `dirty.roomRatio` | `renderRoomRatioDisplay()` **then `onRoomRatioChanged()`** (object-test faces rebuild) | §6, §9.5 |
| `dirty.vbapMode`, `renderBackend`, `hybrid`, `vbapCartesian`, `vbapPolar` | renderer panel | — |
| `dirty.loudness` | `renderLoudnessDisplay()` | §8 loudness info + toggle + DRC summary |
| `dirty.adaptiveResampling`, `distanceDiffuse`, `distanceModel`, `configSaved`, `latency`, `renderTime`, `resample` | right panel / footer | — |
| `dirty.audioFormat` | `renderAudioFormatDisplay()` → its block at `audio.js:175–194` re-runs `reflectBoundOptions()`, `updateTwoDSourcesSummary()`, `updateObjectGeneratorUI()`, `updatePhantomUI()`, `updateFixedChannelProcessingUI()`, `syncVirtualBedObjects()`, `renderChannelEditor()` | **§5 entirely, §9.4** |
| `dirty.drcUI` | `renderDrcUI()` | §8 mode select, weight, gauge visibility, summary |
| `dirty.autoGain`, `autoGainCeiling`, `masterGain` | right panel | — |

Helper wrappers: `updateObjectMeterUI(id)`, `updateObjectPositionUI(id, pos)` (also stores
`sourcePositionsRaw`), `updateObjectLabelUI(id)`, `updateObjectSizeUI(id)` (direct, no batching:
sets the three `.object-size-fill` widths to `clamp01(v)*100` with `toFixed(1)` %),
`updateItemClasses(entry, isMuted, isDimmed)` → toggles `is-muted` / `is-dimmed` on the row.

### 0.9 Live-options binder (`options-binder.js`) — used by §5

Markup declares `data-option="<snake_key>"` on a `<button data-option-value>`, a `<select>`
(optionally `data-option-empty="none"` meaning "an empty/null value maps to this option"), a
`type=number` input, or a checkbox (optionally `data-option-on/off` for enum-valued switches).
`bindOptionControls()` (called from `audio-panel-listeners.js:574`) wires:

* button click → `setOption(key, data-option-value)`; select change → `setOption(key, value)`;
  number change → `setOption(key, Number)`; checkbox change → bool (or on/off enum).
* `setOption` is **optimistic**: `app.options[key] = value`, runs `AFTER_SET[key]`
  (`object_generator_id` → clears `app.objectGeneratorParams`), `invoke('control_option',
  {key, value})`, `reflectBoundOptions()`, sets `dirty.audioFormat`, schedules a flush.
* Host `control_option` (`commands/engine.rs:70–92`) lower-cases the key and sends
  `/omniphony/control/option [key(string), value]` where a JS string → OSC string
  (lower-cased/trimmed), bool → int 0/1, number → float. Renderer validates against its registry
  and echoes canonical values in the `options` snapshot block.
* `reflectBoundOptions()` applies `getLiveOption(key)` to each bound control: button → toggle
  class `active` when `String(v) === data-option-value`; select → `value` (skipped while focused;
  `''`/`null` maps to `data-option-empty`); number → value (skipped while focused); checkbox →
  checked. `undefined` leaves the baked HTML default.
* The `options:schema` Tauri event (`tauri-bridge.js:771–778`) fills `app.optionsSchema` from a
  JSON string `[{key,kind,values?,default,flags,i18nKey,helpI18nKey?}]` and re-reflects.

---

## 1. Brand / About and Config profiles

### 1.1 Brand row `#aboutOpenArea` (`index.html:42–48`, `app.css:277–310`)

| element | content | keys | behaviour |
|---|---|---|---|
| `#aboutOpenArea` | `role=button`, `tabindex=0`, `cursor:pointer` | title key `about.open` ("About") | click, Enter or Space → `setAboutModalOpen(true)` (`modal-and-toggle-listeners.js:233–243`) |
| `.app-brand-logo` | `omniphony-logo.svg`, 1.8 rem square, radius 0.45 rem, glow `0 0 10px rgba(100,210,255,0.2)` | — | cosmetic |
| `<strong>` | "Omniphony Studio" | `app.title` | 1 rem |
| `.app-brand-subtitle` | "Spatial Control" | `app.subtitle` | 0.7 rem, uppercase, letter-spacing 0.12 em, `rgba(217,236,255,0.7)` |

### 1.1b About modal `#aboutModal` (`index.html:1108–1136`; populated by `app.js:300–319`, `controls/config.js:55–100`)

Opened by the brand row **and** by `#aboutBtn` (§3.1). Contents, in order:

| element | source | format |
|---|---|---|
| logo + `#aboutName` + "About" (`about.title`) | `get_about_info().name` ("Omniphony Studio") | heading |
| `#aboutDescription` | `info.description` (fallback text key `about.descriptionFallback`) | paragraph |
| `about.version` "Version" → `#aboutVersion` | `info.version` = `CARGO_PKG_VERSION` | |
| `about.license` "License" → `#aboutLicense` | `info.license` = "GPL-3.0-or-later" | |
| `about.repository` "Repository" → `#aboutRepositoryLink` | `info.repository` (note: host serialises `repository_url`; JS reads `info.repository`, so in practice the baked href `https://github.com/mgth/Omniphony` stays) | external link |
| `about.rendererVersion` "Renderer" → `#aboutRendererVersion` | `app.renderVersion` (+ `" · ABI " + app.renderAbi` when present); `—` when unknown; tooltip = text + `\n` + `app.renderExecutable` | `updateAboutRendererVersion` |
| `about.configPath` "Config" → `#aboutConfigPath` | `app.renderConfigPath`; if `renderConfigStatus` is `missing`/`parse_error` → `"<path> — " + t('about.configMissing'|'about.configParseError')` in **red `#ff7676`**; no path but connected → `t('about.configDefaults')` in **amber `#ffb347`**; else `—` | `updateAboutConfigPath` |
| `#aboutCloseBtn` | `common.close` | closes |

Host: `get_about_info` (`commands/app.rs:56–64`) returns `{name, version, license, repository_url, description}`.
`maybeCheck(info.version)` (§2) is called from the same handler. The renderer fields arrive from
the snapshot and from events `render:version`, `render:executable`, `render:abi`,
`render:config_path`, `render:config_status` (`tauri-bridge.js:435–459`).

### 1.2 Config-profile picker `#profileRow` / `#profileNameRow` (`index.html:52–63`, `controls/profiles.js`, host `commands/profiles.rs`)

Pinned above the scroll, so showing the name editor never moves the viewport.

| control | type | key(s) | enabled rule | on change |
|---|---|---|---|---|
| `#profileSelect` | select, `flex:1` | title `profiles.selectTitle` ("Config profile") | `disabled` when the option list is empty (`profiles.js:179`); also runtime-locked | `control_profile_switch {value:name}` → OSC `/omniphony/control/profile/switch <name>`; **no optimistic apply** (`applying` guard suppresses echo-triggered change events) |
| `#profileCreateBtn` | `.panel-toggle-btn` "+" | title `profiles.create` ("New profile") | always enabled (except runtime lock) | opens name editor (empty); on submit: if name already exists → `control_profile_switch`; else `control_profile_create {value}` **then** `control_profile_switch {value}` (two messages, in order) |
| `#profileRenameBtn` | "✎" | title `profiles.rename` | disabled unless `activeProfile` is a non-empty string | opens editor pre-filled with the active name; submit: ignored if unchanged or name taken; else `control_profile_rename {old, new}` → OSC `/omniphony/control/profile/rename [old, new]` |
| `#profileDeleteBtn` | "✕" | title `profiles.delete` | disabled unless active exists **and** `profileNames.length > 1` (renderer refuses to delete the last one) | `window.confirm(tf('profiles.confirmDelete', {name, keep}))` — "Delete profile \"{name}\"?\n\nStudio will switch to \"{keep}\" first, then remove \"{name}\". This cannot be undone." → `control_profile_switch {keep}` then `control_profile_delete {name}` (`keep` = first other name) |
| `#profileNameRow` | hidden (`display:none`) row: `#profileNameInput` (text, placeholder key `profiles.namePlaceholder` "Profile name", font 0.75 rem), `#profileNameOkBtn` "✓" (title `profiles.confirmName`), `#profileNameCancelBtn` "✕" (title `common.cancel`) | | shown (`display:flex`) with focus+select-all on create/rename | Enter/✓ = submit (trimmed, empty → no-op), Escape/✕ = cancel |

State source: snapshot fields `activeProfile: Option<String>` and `profileNames: Vec<String>`
(`app_state.rs`, mirrored from OSC `/omniphony/state/profiles`). `applyProfilesState(payload)`
(`profiles.js:151–186`) is called from `applyInitState` on every snapshot (≈10 Hz): it rebuilds
the option list **only if the list actually changed** (otherwise an open dropdown would close),
sets `select.value = active`, and re-evaluates button enablement. Disabled buttons render at
`opacity:0.45` (`app.css:201–204`).

---

## 2. Update panel `#updatePanelRoot` (`index.html:65–74`, `controls/updates.js`)

| control | type | keys | default | behaviour |
|---|---|---|---|---|
| `#updateCheckToggle` | switch in `.switch-row` (13 px) | label `updates.checkLabel` "Check for updates on startup", help `help.updates.check` (anchor `.switch-row`) | off | on change: persist; if turned on → `maybeCheck(currentVersion)`; if off → hide banner |
| `#updateAvailableBanner` | hidden banner | text `#updateAvailableText` = `tf('updates.available',{version: tag})` "Update available: {version}"; link `#updateAvailableLink` text `updates.linkText` "View on GitHub", href = release `html_url` or `https://github.com/mgth/Omniphony/releases` | hidden | shown only when a fetched/cached release tag is strictly newer than the Studio version |

Banner style: padding `0.55rem 0.65rem`, 14 px, weight 600, colour `#d9ecff`, background
`rgba(88,160,255,0.15)`, border `1px solid rgba(88,160,255,0.38)`, radius 7 px; link `#9cc6ff` underlined.

Logic (`updates.js`): localStorage keys **`omniphony.updateCheck.enabled`** (`'1'|'0'`),
**`omniphony.updateCheck.lastCheck`** (ms epoch), **`omniphony.updateCheck.result`** (JSON
`{tag, htmlUrl}`). Check at most once per 24 h; inside the window the banner is rendered from the
cached result. Fetch = `GET https://api.github.com/repos/mgth/Omniphony/releases`
(`Accept: application/vnd.github+json`), pick the highest non-draft, non-prerelease tag matching
`^v(\d+)\.(\d+)\.(\d+)$` (dev tags `v0.x.y.nnn` and `liborender-v*` are excluded). Compare
major/minor/patch numerically. Failures → `pushLog('warn', tf('updates.checkFailed',{error}))`,
nothing shown. Note: the frontend does the HTTP itself (no Tauri command); the egui host will
need its own HTTP client or a host command.

---

## 3. OSC panel `#oscPanelRoot` (`index.html:75–127`, `controls/osc.js`)

### 3.1 Status line (`index.html:76–83`, `renderOscStatus` `osc.js:48–175`)

Row: left = `<span class="osc-status-dot" id="oscStatusDot">` + `osc.label` "OSC" + ": " +
`#status`; right = three icon buttons.

| element | content / rule |
|---|---|
| `#oscStatusDot` | 7 px circle; colour by `app.oscStatusState`: initializing `#89a3ff`, connected `#52e2a2`, reconnecting `#ffb347`, error `#ff5d5d`, other `#7f8a99` |
| `#status` | `t('status.' + state)` ("Initializing...", "Connected", "Reconnecting...", "Error"); when connected and capabilities known, append `" · " + flavour` where flavour = `producerHost()||producerVariant()` for embedded (→ "mpv"), `"service"` when `app.orenderServiceRunning`, else `producerHost()||producerVariant()` (→ "cli") |
| `#aboutBtn` | `.info-icon-btn` "?" title `about.open` → opens About modal |
| `#oscInfoBtn` | `.info-icon-btn` "i" title `osc.infoButton` ("OSC info") — **hidden** by `bindModalOpenClose` because `triggerSelector: '#oscPanelRoot [data-i18n="osc.label"]'` makes the word "OSC" the (dotted-underlined) trigger of `#oscInfoModal` (title `osc.infoTitle`, body HTML key `osc.infoBody`, Close `common.close`) |
| `#oscConfigToggleBtn` | `.panel-toggle-btn` glyph `⚙` (U+2699) closed / `✕` (U+2715) open, title `osc.configTitle` ("OSC Configuration"); click toggles `#oscConfigForm.open`; opening calls `loadOscConfigIntoPanel()` |

Status-state transitions (`setOscStatus`, `osc.js:287–342`): any state ≠ connected clears
`oscSnapshotReady` and re-arms the input auto-open; leaving connected clears the launch-pending
flag + timer and **clears `sourceNames`** (object names must be re-learned from the next
producer); `connected` → cancel the auto-open timer; if a launch was pending, close the config
form; if embedded → close the config form; `initializing` → open form immediately;
`reconnecting` → if previous was initializing, a launch was pending, or this is a fresh
disconnect → `scheduleOscConfigAutoOpen()` (opens the form after **3000 ms** if still not
connected); `error` → open form, clear launch pending. Every change logs
`log.oscStatus` "OSC status: {status}". Source of state: snapshot `oscStatus` and event
`osc:status {status}`; `orender:autostart {status:'failed'}` forces `error`.

### 3.2 Banners (`index.html:84–92`)

| element | visible when | content | style |
|---|---|---|---|
| `#oscConnectingHint` | state ∈ {reconnecting, initializing} **and** no bridge error | HTML key `status.connectingHint`: "No renderer connected. Either run **orender** to create a SPDIF audio input, or launch [**mpv-omniphony**](https://github.com/mgth/mpv-omniphony/releases), which embeds its own renderer." | red text `#ff6b6b` on `rgba(255,107,107,0.10)`, border `rgba(255,107,107,0.42)`, 14 px/600, radius 7 px |
| `#bridgeErrorBanner` | `app.renderBridgeError` non-empty (snapshot `renderBridgeError`, event `render:bridge_error`) | title `status.bridgeErrorTitle` "Decoder bridge not found — spatial audio disabled" + `#bridgeErrorDetail` = raw error text (monospace 12 px, pre-wrap) | white on `rgba(220,38,38,0.92)`, border `#ff8080`, 14 px/700 |
| `#foreignRendererBanner` | `rendererIsForeign() === true` (`config.js:126–132`: not embedded, both `renderExecutable` and `expectedOrenderPath` known and different) | title `status.foreignRendererTitle` "Connected to a renderer this Studio did not start" + `#foreignRendererDetail` = `tf('status.foreignRendererDetail',{running, expected})` ("Controls it does not implement are dropped silently.\nrunning:  {running}\nexpected: {expected}") | dark text `#1a1200` on amber `rgba(245,158,11,0.92)`, border `#ffd27f` |

`expectedOrenderPath` is fetched once at boot via `invoke('expected_orender_path')`
(`app.js:324–331`, host `commands/orender.rs:250`). The "renderer is local" check
(`renderer_is_local`, `commands/app.rs:49–53`, loopback test on the configured host) is **not**
used by the OSC panel; it is only consulted by the renderer-panel file-param Browse button
(`controls/vbap.js:26`) and is out of this spec's scope.

### 3.3 Config form `#oscConfigForm` (`index.html:93–126`)

Collapsed by default (`max-height:0; opacity:0; pointer-events:none`); `.open` gives
`max-height:var(--panel-open-max-height)` with internal scroll (`app.css:1119–1138`).

| control | type / range | label key | help key | loaded from (`get_osc_config` → `OscConfig`, `config.rs:5–29`) | dirty-tracked |
|---|---|---|---|---|---|
| `#oscHostInput` | text, default `127.0.0.1` | `osc.host` "Host" | `help.osc.host` | `cfg.host` | yes |
| `#oscRxPortInput` | number 1…65535, default 9000 | `osc.omniphonyPort` "Omniphony Port" | `help.osc.omniphonyPort` | `cfg.osc_rx_port` | yes |
| `#oscListenPortInput` | number 0…65535, default 0 | `osc.listenPort` "Listen Port" | `help.osc.listenPort` | `cfg.osc_port` | yes |
| `#autoStartRendererToggle` | switch (13 px row) | `osc.autoStartRenderer` "Auto-start local renderer" | `help.osc.autoStartRenderer` (anchor `.switch-row`) | `cfg.auto_start_renderer` (default true) | yes |
| `#keepRendererAliveToggle` | switch | `osc.keepRendererAlive` "Keep renderer alive on quit" | `help.osc.keepRendererAlive` | `cfg.keep_renderer_alive_on_quit` (default false) | yes |
| `#mpvOrenderToggle` | switch | `osc.mpvOrender` "Activate in mpv config" | `help.osc.mpvOrender` | `mpv_orender_status()` | **no** (applies immediately, §3.4) |
| `#mpvOrenderNote` | `.mpv-orender-note` status line (12 px, `rgba(217,236,255,0.62)`; `.is-conflict` → `#ffbf66`) | — | — | — | — |
| `.osc-config-actions` row | 5 buttons (§3.5) | | | | |

`readOscConfigForm()` (`osc.js:256–271`) clamps ports (`rx` 1–65535 fallback 9000, listen 0–65535
fallback 0), host trimmed with fallback `127.0.0.1`, and also includes
`osc_metering_enabled` from `#oscMeteringToggle` (which physically lives in the Objects section,
§9.2). `oscConfigStateKey()` = JSON of that object; `app.oscConfigBaselineKey` is set when the
form is loaded and after a successful save. `renderOscConfigApplyButton()`: Apply is enabled only
when `key !== baseline && !oscLaunchPending && !orenderServicePending`; disabled look = opacity 0.45.
Every `input`/`change` on the six tracked controls re-evaluates it (`osc.js:511–518`).

`#oscConfigApplyBtn` (`.ui-btn.ui-btn-success`, text `osc.connect` "Connect"): if enabled →
`invoke('save_osc_config', {config})` (host: persists, preserves server-only fields, re-arms the
auto-start watchdog, sends `SetMeteringEnabled` and `Reconnect{host, rx_port, listen_port}`),
then locally: `app.oscMeteringEnabled = config.osc_metering_enabled`, baseline = key, log
`log.oscConfigSaved` ("OSC configuration applied. Reconnecting..."), `setOscStatus('reconnecting')`,
close the form. On failure: log `log.oscConfigFailed` "OSC configuration failed: {error}".

`loadOscConfigIntoPanel()` also sets `dirty.audioFormat` and then calls
`refreshOrenderServiceStatus()` (`invoke('get_orender_service_status')` →
`{installed, running, manager}`; `manager` = `"systemd-user"` on Linux).

### 3.4 mpv config switch (`osc.js:678–736`, host `commands/mpv_config.rs`)

* `refreshMpvOrenderToggle()` runs at module load and every time the config form is opened
  (the file can change outside Studio). Host `mpv_orender_status()` → `{path, exists, state,
  conflictLine?, conflictText?}`, `state ∈ enabled|disabled|absent|conflict`.
* Render: `checked = state === 'enabled'`; `disabled = state === 'conflict'`; note text:
  conflict → `tf('osc.mpvOrenderConflict',{line, text})` ("Line {line} already sets a decoder:
  {text}. Remove it to use this switch.") with class `is-conflict`; else
  `tf('osc.mpvOrenderPath',{path})` ("Managed in {path}"); note visible whenever a status exists.
  A failed status call shows the error message in the note (conflict colour).
* On change: disable the switch, `invoke('mpv_orender_set',{enabled})`; success → re-render from
  the returned status and log `log.mpvOrenderEnabled`/`Disabled` ("orender enabled in {path}" /
  "orender commented out in {path}"); failure → log `log.mpvOrenderFailed` and re-fetch the
  status (switch snaps back). Host writes/updates `ad=orender` inside a
  `# >>> omniphony (managed) >>> … # <<< omniphony (managed) <<<` block in the global section of
  `mpv.conf` (`$MPV_HOME` / `$XDG_CONFIG_HOME/mpv` / `~/.config/mpv`; `%APPDATA%\mpv` on Windows),
  refusing when a foreign `ad=` line exists.
* Exempt from the runtime lock (works offline by design).

### 3.5 Action buttons `.osc-config-actions` (`index.html:119–125`, `renderOscStatus` + listeners `osc.js:520–601`)

Hidden wholesale under `body.cap-embedded`. All disabled while `oscLaunchPending ||
orenderServicePending` (opacity 0.6 / 0.45, cursor default). Order left→right:

| button | class | text / title | enabled | action |
|---|---|---|---|---|
| `#oscConfigApplyBtn` | `ui-btn ui-btn-success` | `osc.connect` | dirty (§3.3) | save + reconnect |
| `#oscServiceBtn` | `ui-btn` | text `osc.service.uninstall` "Uninstall service" if `orenderServiceInstalled` else `osc.service.install` "Install service"; background/border red-tinted `rgba(255,96,96,0.18)/0.38` when installed, neutral `rgba(255,255,255,0.08)/0.18` otherwise; title = `(installShort|uninstallShort) + ' ' + serviceNoun + ' (' + manager + ')'` e.g. "Install service (systemd-user)" | not pending | installed → `uninstall_orender_service`; else `install_orender_service {host, oscRxPort, oscPort, oscMeteringEnabled, orenderPath: app.oscConfiguredOrenderPath||null, logLevel}` (payload from the form). Both set `orenderServicePending` around the call, log, then refresh status; install also re-loads the config form (auto-start gets disabled backend-side) |
| `#oscRestartServiceBtn` | `ui-btn` | text baked "Restart service" (no i18n on the text); title `osc.service.restart` when installed else `osc.service.installFirst` ("Install service first") | installed && not pending | `restart_orender_service` |
| `#oscRestartPipewireBtn` | `ui-btn` | baked "Restart PipeWire"; title `osc.pipewire.restartTitle` ("Restart PipeWire and WirePlumber") / `osc.pipewire.linuxOnly` | **display:none unless `isLinux`**; enabled when Linux && not pending | `restart_pipewire_services` |
| `#oscLaunchRendererBtn` | `ui-btn ui-btn-accent` | `running = installed ? orenderServiceRunning : (state === 'connected')`; text: installed → `osc.service.stop`/`osc.service.start` ("Stop service"/"Start service"), else `osc.orender.stop`/`osc.orender.launch` ("Stop orender"/"Launch orender"); colours: running → red tint bg `rgba(255,96,96,0.18)`, border `0.38`, text `#ffe2e2`; else blue tint `rgba(88,160,255,0.18)/0.38`, text `#d9ecff` | not pending | installed → `stop_orender_service` / `start_orender_service` (pending flag around call, then refresh); else connected → `invoke('stop_orender')` (OSC `/omniphony/control/quit`, suppresses the auto-start watchdog); else `launchOrenderFromPanel()` |

`launchOrenderFromPanel(pathOverride)` (`osc.js:348–396`): payload `{host, oscRxPort, oscPort,
oscMeteringEnabled, orenderPath: override||app.oscConfiguredOrenderPath||null, logLevel:
normalizeLogLevel(logState.backendLogLevel)}`; sets `oscLaunchPending = true` and a **12 000 ms**
safety timer that clears it (buttons come back even if orender never connects);
`invoke('launch_orender')` → logs `"orender launched: <command>"`. If the error contains
`"orender binary not found"` → open the form, `invoke('pick_orender_path')` (native file dialog),
store the pick in `app.oscConfiguredOrenderPath` and retry. Other failures →
`"Failed to launch orender: …"` in the log.

### 3.6 OSC info modal

`#oscInfoModal` (`index.html:1094–1106`): title `osc.infoTitle`, body `osc.infoBody` (HTML),
close `#oscInfoCloseBtn`. See §0.6 for the trigger rule.

---

## 4. Audio Input panel (`ui/input-panel.js` markup, `controls/input.js`, `listeners/input-panel-listeners.js`, host `commands/input.rs`, `commands/render.rs:499–520`)

Mounted into `#audioInputPanelMount` as `#inputPanelRoot > .info-section#audioInputSection`.
Header via `panelHeader()` (`ui-primitives.js:1–19`): title `section.audioInput` "Audio Input"
(relabelled to `section.decoderBridge` "Decoder bridge" on an embedded producer), summary
`#inputSummary` (hidden while open), toggle `#inputSectionToggleBtn`. The panel title is the
click trigger of `#inputInfoModal` (`triggerSelector: '#audioInputSection .panel-title'`; title
`input.infoTitle`, body `input.infoBody`).

Content `#inputSectionContent > .input-panel-shell`, in DOM order:

| # | element | type / values | label key | help key | value source | on change |
|---|---|---|---|---|---|---|
| 1 | `#inputStatusInfo` | status text (`.input-panel-status`) | — | — | see below | — |
| 1b | `#inputInfoBtn` | `i` button, title `input.infoButton` — hidden, replaced by the title trigger | | | | |
| 2 | `#inputModeSelect` | select `pipe_bridge` / `pipewire_bridge` | `input.mode` "Mode"; options `input.mode.pipe_bridge` "Pipe bridge", `input.mode.pipewire_bridge` "PipeWire bridge" | `help.input.mode` | `app.inputMode` (snapshot `inputMode`, only adopted when `!inputModeDirty`) ; `disabled = !hasProducerDomain('input')` | `app.inputMode = v; inputModeDirty = true`; entering `pipewire_bridge` resets `liveInput.channels=2, sampleRate=192000`; `updateInputControlUI(); sendInputConfig()` (no apply) |
| 3 | `#inputBridgeFields` subtitle | `input.bridgeInput` "Bridge Input" (hidden when embedded) | | | shown when `embedded || hasInputDomain` | |
| 4 | `#oscBridgePathInput` + `#oscBridgeBrowseBtn` | text (placeholder key `input.autoDetect` "Auto-detect") + `.ui-btn` `input.browse` "Browse" | `input.bridgeBinary` "Bridge" | `help.input.bridge` | `app.renderBridgePath` (not rewritten while focused); class `input-panel-danger` (red border) when bridge-missing | change → `app.renderBridgePath = v||null; control_render_bridge_path {value}` (OSC `/omniphony/control/render/bridge_path`). Browse → `pick_bridge_path` then dispatches `change`. Both exempt from the runtime lock |
| 5 | `#oscBridgePathStatus` | inline red status (`.input-panel-inline-status`, `#ff7d7d`) | — | — | text `"Bridge path missing"` (hard-coded English) when `app.inputError` matches `/bridge path missing|no bridge plugin found|render\.bridge_path/i` and `renderBridgePath` is empty; else hidden | |
| 6 | `#pipeStatus` | text, placeholder `input.autoDetect` | `input.pipe` "Pipe" | `help.input.pipe` | `app.orenderInputPipe` (not rewritten while focused); row visible only when `hasInputDomain && mode === 'pipe_bridge'` | change → `persistInputPipeNow()`: `app.orenderInputPipe = v||null; control_render_input_pipe {value}` |
| 7 | `#inputLiveFields` subtitle | `input.liveSource` "Live Source" | | | block visible only when `hasInputDomain && mode === 'pipewire_bridge'` (opacity 0.55 otherwise, but hidden anyway) | |
| 8 | `#inputBackendSelect` | `pipewire`/`asio` (`input.backend.*`) | `input.backend` | `help.input.backend` | **row permanently hidden and control disabled** (`input.js:226–249`, legacy PCM mode) | change → `liveInput.backend`, `sendInputConfig()` |
| 9 | `#inputNodeInput` | text, placeholder `omniphony` | `input.node` "Node" | `help.input.node` | `liveInput.node || inputNode`; enabled iff `hasInputDomain && pipewire_bridge` | change → `liveInput.node`, `sendInputConfig()` |
| 10 | `#inputDescriptionInput` | text, placeholder `Omniphony Bridge Input` | `input.description` | `help.input.description` | `liveInput.description || inputDescription` | same pattern |
| 11 | `#inputClockModeSelect` (+ `#inputClockInfoBtn` → `#inputClockInfoModal`, title `input.clockInfoTitle`, body `input.clockInfoBody`) | `dac` "DAC" / `pipewire` "PipeWire" / `upstream` "Upstream (advanced)" (`input.clock.*`; the baked option text says "(advanced)", the en.json string is "Upstream") | `input.clock` "Clock" | — (modal instead) | `liveInput.clockMode` (adopted from snapshot only when `!liveInputClockModeDirty`) | change → `liveInput.clockMode = v; liveInputClockModeDirty = true` **(not sent until Apply)** |
| 12 | `#inputLayoutInput` (readonly, placeholder `input.noImportedLayout`) + `#inputLayoutBrowseBtn` `input.import` "Import" | | `input.layout` | `help.input.layout` | **hidden & disabled** (legacy) | Import → `pick_import_layout_path` → `import_input_layout_from_path {path}` → `liveInput.layout`, `sendInputConfig()`, logs `log.layoutImportRequested/Imported/Failed` |
| 13 | `#inputChannelsInput` number min 1 step 1 (default 2) / `#inputSampleRateInput` number min 1 (default 192000) | | `input.channels` / `audio.sampleRate` | `help.input.channels` / `help.input.sampleRate` (anchor `.input-panel-inline-grid`) | **hidden & disabled** | change → `max(1, round(v))`, `sendInputConfig()` |
| 14 | `#inputMapSelect` (`7.1-fixed` only, `input.map.sevenOneFixed`) / `#inputLfeModeSelect` (`object`/`direct`/`drop`, `input.lfe.*`, + `#inputLfeInfoBtn` → `#inputLfeInfoModal`) | | `input.map` / `input.lfe` | `help.input.map` / modal | **hidden & disabled** | `sendInputConfig()` |
| 15 | `#inputApplyBtn` | `.ui-btn.ui-btn-primary` | text `input.apply` "Apply", or `input.applyPending` "Apply pending..." while `showApplyPending` | — | hidden when embedded | see Apply below |

Effective visible layout today: **Mode**, **Bridge (+Browse)**, then either **Pipe** (pipe_bridge)
or **Node / Description / Clock** (pipewire_bridge), then **Apply**. Rows 8, 12, 13, 14 are dead
markup kept for a future rework — port them as hidden or omit.

Status line `#inputStatusInfo` = `tf('input.status.bridge', {requested, active, pipe, sync})`
("requested {requested} • active {active} • pipe {pipe} • {sync}") + (pipewire_bridge ?
`tf('input.status.clock',{clock})` " • clock {clock}" : "") + (inputError ?
`tf('input.status.error',{error})` " • error: {error}" : ""), where `requested/active` are the
localised mode labels, `pipe = app.orenderInputPipe || '—'`, `sync = t('input.sync.pending')`
"pending apply" when `showApplyPending` (= mode ≠ pipe_bridge && `inputApplyPending`) else
`t('input.sync.synced')` "synced".

Header summary `#inputSummary`: embedded → `renderBridgePath || t('input.autoDetect')`;
pipewire_bridge → `input.summary.pipewireBridge` "{requested} • active {active} • {clock} clock";
else `input.summary.bridge` "{requested} • active {active} • pipe".

`sendInputConfig({apply})` (`input.js:88–94`): `control_input_config {payload}` where payload =
`{mode, liveInput:{backend, node, description, layout, clockMode, channels, sampleRate, map,
lfeMode}}` (nulls for empty strings; defaults 2 / 192000 / '7.1-fixed' / 'object' / 'dac') →
OSC `/omniphony/control/config/input <json>`; with `apply` also `control_input_config_apply`
(`/omniphony/control/config/input/apply`).

**Apply button** (`input-panel-listeners.js:177–210`): `needsBridgeBootstrap = mode === 'pipe_bridge'
|| (mode === 'pipewire_bridge' && activeMode !== 'pipewire_bridge')`. If so: clear pending flags,
then sequentially `control_render_bridge_path {value: bridge field}` → `control_input_live_clock_mode
{value: clockMode}` (OSC `/omniphony/control/input/live/clock_mode`) → `control_save_config` →
`control_reload_config` (OSC `/omniphony/control/save_config`, `/reload_config`). Otherwise:
`inputApplyPending = inputApplyAwaitingAck = true`, `control_input_live_clock_mode` then
`sendInputConfig({apply:true})`; on error both flags reset. Snapshot `inputApplyPending` (0/1)
resolves the ack: while `awaitingAck`, only a `1` is accepted (`init.js:558–568`).

Auto-open: on every snapshot, if `app.inputError` matches `/bridge path missing|no bridge plugin
found|render\.bridge_path|duplicate PipeWire sink/i` and differs from `lastAutoOpenedInputError`,
the section is opened (`setInputSectionOpen(true)`); re-armed on disconnect (`init.js:694–704`).

Events: `render:bridge_path {value}` and `state:input_pipe {value}` re-render the panel
(`tauri-bridge.js:430–433, 817–821`).

---

## 5. Fixed-channel sources `#twoDSourcesPanelRoot` (`index.html:129–189`; render `controls/audio.js:386–740`; listeners `audio-panel-listeners.js:574–582`; binder §0.9)

Header: title `section.twoDSources` "Fixed-channel sources" with help `help.twoDSources`; summary
`#twoDSourcesSummary` (shown while collapsed) = `"<placement> · <synthesis>"` where placement =
`t('twoDSources.surroundBack')` if `getLiveOption('surround_placement') === 'back'` else
`twoDSources.surroundSide`, synthesis = `twoDSources.summary.syntheticEnabled` "Fixed + synthetic
objects" if `synthetic_objects_enabled` else `twoDSources.summary.fixedOnly` "Fixed channels
only" (`updateTwoDSourcesSummary`). Toggle `#twoDSourcesToggleBtn` (title `section.twoDSources`).

Body `#twoDSourcesBody` (grid, gap 0.35 rem, internal scroll, DOM order):

| # | element | type | keys | value source / reflection | on change |
|---|---|---|---|---|---|
| 1 | `#fixedChannelActivity` | note, 10 px, opacity 0.8 | text: `fixedChannelProcessing.stream === 'fixed'` → `twoDSources.stream.fixed` "Current source: fixed channels rendered by Omniphony"; `'objects'` → `twoDSources.stream.objects`; else `twoDSources.stream.idle` "No active stream — settings remain editable" | snapshot `fixedChannelProcessing` | — |
| 2 | `#surroundPlacementRow`: label + two `.toggle-btn` `#surroundPlacementSide` / `#surroundPlacementBack` | button pair `data-option="surround_placement"` values `side` / `back` | label `twoDSources.surroundLabel` "Rear channels (4.x/5.x)"; buttons `twoDSources.surroundSide` "Side" / `twoDSources.surroundBack` "Back" | `.active` on the button whose value matches `getLiveOption('surround_placement')` (HTML default: Side active) | binder → `control_option('surround_placement','side'|'back')` |
| 3 | `#syntheticObjectsRow` switch `#syntheticObjectsToggle` | checkbox `data-option="synthetic_objects_enabled"` (13 px `.switch-row`) | label `twoDSources.syntheticObjectsLabel` "Synthetic objects", help `help.syntheticObjects` (anchor `.switch-row`) | checked = `!!getLiveOption(...)` | binder → `control_option(key, bool)` (host sends int 0/1) |
| 4 | `#syntheticObjectsStatus` | note 10 px, padding-left 0.5 rem | `twoDSources.syntheticConfigured` "Configured synthesis; current activity is shown below" when enabled, else `twoDSources.fixedOnly` "Fixed channels only — no synthetic objects" | | |
| 5 | `#objectGeneratorRow`: label, `#objectGeneratorNoHeightNote`, `#objectGeneratorSelect` | select `data-option="object_generator_id" data-option-empty="none"`; baked options `none` "Off" (`twoDSources.objectGenNone`), `copy_up` "Direct copy" (`objectGenCopyUp`), `pad` "Ambience (PAD)" (`objectGenPad`), `dirac` "Diffuse field (DirAC)" (`objectGenDirac`) — **replaced** by the schema list when `objectGenerators:schema` arrives (`rebuildObjectGeneratorControls`: "Off" + one option per `{id,label,i18nKey}` using `t(i18nKey)` if it resolves else `label`) | label `twoDSources.objectGeneratorLabel` "Generate height objects", help `help.objectGenerator` (anchor `#objectGeneratorRow`) | value = `getLiveOption('object_generator_id') || 'none'`; never disabled | binder → `control_option`; `AFTER_SET` clears `objectGeneratorParams` |
| 5b | `#objectGeneratorNoHeightNote` | inline note 10 px, opacity 0.7, nowrap; baked text `twoDSources.objectGenNoHeight` "No top speakers" | shown (`display:inline`) only when `effectiveHeightReason()` is neither `active` nor `off`; text = `processingReason(reason)` (see table below) | | |
| 6 | `#objectGenParamsRow` | column of generated sliders (see 5.1) | | visible iff the active generator's schema has ≥1 param | |
| 7 | `#phantomExtractRow` label + `#phantomExtractModeSelect` | select `data-option="phantom_extract_mode"`: `off` "Off" (`twoDSources.phantomOff`), `broadband` "Broadband", `spectral` "Spectral" | label `twoDSources.phantomLabel` "Phantom extraction", help `help.phantomExtract` (anchor `.switch-row` — resolves to the parent row) | value = `getLiveOption(...)` | binder → `control_option` |
| 8 | `#phantomStatus` | note 10 px | `processingReason(effectivePhantomReason())` | | |
| 9 | `#phantomParamsRow` | generated sliders/switches (5.2) | | visible iff mode ≠ `off` && schema non-empty | |
| 10 | `#virtualBedActions` → `#virtualBedResetBtn` | `.ui-btn.ui-btn-compact`, right-aligned | `virtualBed.reset` "Reset channel layout" | always shown | `window.confirm(t('confirm.resetVirtualBed'))` → `resetVirtualBed()` (§9.4.4) |

Reason strings (`PROCESSING_REASON_KEYS`, `audio.js:399–412`): `active` → `twoDSources.status.active`
"Active for the current stream"; `off` → "Disabled"; `master_off` → "Configured, inactive:
synthetic objects are disabled"; `no_stream` → "Configured, waiting for a stream";
`object_stream` → "Configured, inactive: the stream already carries objects";
`input_has_height` → "…already carries height channels"; `output_has_no_height` → "…the output
layout has no height speaker"; `insufficient_channels` → "…the source has too few compatible
channels"; unknown → `no_stream`. `effectivePhantomReason()`: mode `off` → `off`; master switch
off → `master_off`; else `fixedChannelProcessing.phantom || 'no_stream'`.
`effectiveHeightReason()`: generator `none` → `off`; master off → `master_off`; else
`fixedChannelProcessing.height || 'no_stream'`.

### 5.1 Generator parameter sliders (`buildParamSliders`, `audio.js:502–575`)

Schema: `app.objectGenerators = [{id, label, i18nKey, requiresHeightLayer, params:[{key, label,
i18nKey, min, max, step, default, unit}]}]` (event `objectGenerators:schema`, JSON string).
For each param of the **active** generator, one row `<label style="display:flex;gap:0.5rem;
font-size:11px">`:

* name `<span>` (min-width 96 px) = `t(i18nKey)` if it resolves else `label` (known keys:
  `twoDSources.padStrength` "Ambience strength", `padHpf` "Bass cutoff", `padGain` "Height
  level", `padCenterAmount` "Center to height", `padCenterHpf` "Center bass cutoff",
  `diracAmount` "Diffuse level", `diracBias` "Diffuse bias", `diracHpf` "Bass cutoff");
* `<input type=range min max step>` (`flex:1`), `data-param-key`;
* value `<span>` (48 px, right-aligned, tabular) = `fmtParamValue`: `step ≥ 1` → integer;
  `step ≥ 0.1` → `toFixed(1)`; else `toFixed(2)`; + `" " + unit` if a unit is declared.
* Initial value = `app.objectGeneratorParams[key]` if not null, else `default`.
* `input` event (live, every drag tick) → update the value text and
  `applyObjectGeneratorParamNow(key, v)`: optimistic write into `app.objectGeneratorParams`,
  `invoke('control_object_generator_param',{key, value})` → OSC
  `/omniphony/control/object_generator/param [key, float]` (renderer clamps).
* Rows are rebuilt only when the active generator id changes (`builtParamGenId`); otherwise a
  refresh only rewrites values (skipping the slider that has focus). No help affordance.

### 5.2 Phantom parameter controls (`buildPhantomParamSliders`, `audio.js:594–740`)

Schema `app.phantomSchema = [{key, label, i18nKey, min, max, step, default, unit}]` (event
`phantom:schema`). Same slider row as 5.1, except a **binary** param (`min 0, max 1, step 1`)
renders as a `.switch-row` (11 px) with a pill switch (`checked = value >= 0.5`, change →
`applyPhantomParamNow(key, 1|0)`). Sends `control_phantom_extract_param {key, value}` → OSC
`/omniphony/control/phantom_extract/param`. Known labels: `twoDSources.phantomStrength`
"Extraction", `phantomPasses` "Passes", `phantomLift` "Lift", `phantomCenter` "Relocalize center",
`phantomSides` "Relocalize sides", `phantomHeights` "Extract heights", `phantomHeightSplit`
"Height split".
Method gating (`applyPhantomParamGate`): keys `passes, center, sides` are broadband-only; keys
`heights, height_split` are spectral-only. A gated row stays **enabled** but is drawn at
`opacity:0.7` with tooltip `twoDSources.phantomBroadbandOnly` "Broadband method only" /
`phantomSpectralOnly` "Spectral method only". Gating is re-applied on every refresh (the mode can
change without a rebuild).

### 5.3 Virtual-bed objects (side effect of this section; `controls/virtual-bed.js:413–547`)

When no spatial frame has arrived for **800 ms** (`STREAM_IDLE_MS`), Studio removes all live
(non-synthetic) objects except the injection source and creates one **synthetic editor object per
bed channel** (`syncVirtualBedObjects`) so the Objects list (§9.3) shows L/R/C/LFE/… at rest with
`fixed:true`, `label = name`, `gainDb`, `directSpeakerIndex` for direct channels, and `_noTrail`.
While streaming (recent `spatial:frame` or `source:update`), the synthetic ones are removed and
live objects take over. The channel set = renderer catalogue (`fixedChannelCatalog`
`[{label, x, y, z, spatialize, aliases[]}]`) else `FALLBACK_BED` (24 canonical labels,
`virtual-bed.js:36–61`), overridden per channel by the configured `app.virtualBed.speakers`.

---

## 6. Room geometry `#roomGeometryPanelRoot` (`index.html:190–242`, `controls/room-geometry.js`, `listeners/room-geometry-listeners.js`, host `commands/speakers.rs:12–70`)

Header: title `room.title` "Room Geometry" (click trigger of `#roomGeometryInfoModal`, title
`room.infoTitle`, body `room.infoBody`; the `#roomGeometryInfoBtn` is hidden), summary
`#roomGeometryHeaderSummary` (shown while collapsed):
`"m/u {mpu} • X {W}m • Y {front+rear}m • Z {height+lower}m"`, all `formatNumber(v,2)`
(`renderRoomGeometrySummary`, line 644–647). Toggle `#roomGeometryToggleBtn` (title `room.title`);
expanding also makes the 3D dimension guides visible (`roomDimensionGroup.visible`, out of scope).
`#roomGeometrySummary` (`.room-geometry-summary` with `#roomGeometrySummarySize`, label
`room.summary.size` "Size") is **always hidden** by `setRoomGeometryExpanded` (line 816–818) —
dead markup; its text would be `"X: …m | Y+: …m | Y-: …m | Z+: …m | Z-: …m"`.

### 6.1 Model (`computeRoomGeometryFromInputs`, lines 676–722)

Five metre inputs; Width is the reference: `mpu = max(0.01, width/2)`; ratios sent to the
renderer are `width:1`, `length = front/mpu`, `rear = rear/mpu`, `height = height/mpu`,
`lower = lower/mpu` (fallbacks 1,1,1,0.5; every input `max(0.01, n)`, NaN → current value).
Display is the inverse: `width = roomRatio.width*mpu*2`, others `ratio*mpu`, `formatNumber(v,2)`.

### 6.2 Form `#roomGeometryForm` (3-column grid X | Y | Z, `index.html:209–241`)

| control | type | label key / help key | default | note |
|---|---|---|---|---|
| column headers "X" "Y" "Z" | 12 px, opacity 0.8, right-aligned | — | | cosmetic |
| `#roomDimWidthInput` | number min 0.01 step 0.01, `.delay-input` right-aligned | `room.axis.width` "width" / `help.room.width` (anchor `#roomGeometryForm`) | 2.00 | X column |
| "m/u" label + `#roomMpuValue` | readout, 11 px `#d9ecff` opacity 0.85 | (label baked "m/u", `room.mpu` exists but is not applied) | `—` → `formatNumber(mpu,2)` | X column, below width |
| `#roomDimLengthInput` | number | `room.axis.length` "front" / `help.room.front` | 2.00 | Y column |
| `#roomDimRearInput` | number, baked title "Rear depth (m)" | `room.axis.rear` "rear" / `help.room.rear` | 1.00 | Y column |
| `#roomDimHeightInput` | number | `room.axis.height` "height" / `help.room.height` | 1.00 | Z column |
| `#roomDimLowerInput` | number | `room.axis.lower` "lower" / `help.room.lower` | 0.50 | Z column |
| `#roomCenterBlendRow`: `#roomRatioCenterBlendSlider` + `#roomRatioCenterBlendValue` | range 0–100 step 1 (`.gain-slider`), value text `"{b}/{100-b}"` (default "50/50"), 11 px `#d9ecff`, min-width 3.5 rem; title `room.centerBlend.resetTitle` "Double click to reset 50/50" | `room.centerBlend` "Y center blend" / `help.room.centerBlend` (anchor `#roomCenterBlendRow`) | 50 | **row hidden when `|length − rear| < 1e-6`** (`updateCenterBlendVisibility`) |

Editable state (`setRoomFieldEditable`, lines 734–763): when `isRoomRatioFrozen()`
(`renderBackendState.frozenRoomRatio`) all five inputs become read-only
(`readOnly`, `tabIndex -1`, `pointer-events:none`, class `derived-field`, transparent
background/border, text `rgba(223,232,243,0.88)`) and the slider is disabled; otherwise inputs are
`rgba(255,255,255,0.08)` bg, `1px solid rgba(255,255,255,0.2)` border, `#dfe8f3` text.
`#roomGeometryCancelBtn` is referenced by the JS but does not exist in the markup (dead).

### 6.3 Interaction (`room-geometry-listeners.js`)

* Dimension inputs: `input` (typing) → `previewRoomGeometryScene()` — live-updates the 3D box,
  guides, summary, m/u readout and blend-row visibility from the typed values **without** rewriting
  the fields or sending anything; `change` (blur) or **Enter** (blur → change) →
  `normalizeRoomGeometryInputDisplays()` (rewrite every field as `toFixed(2)`),
  `updateRoomGeometryLivePreview()`, `applyRoomGeometryNow()`.
* Blend slider: `input` → re-render "b/100−b", preview, `scheduleRoomGeometryApply()` (debounced
  **120 ms**); `change` → apply now; **double-click** on the slider or on the value text → reset to
  0.5 and apply now. All ignored when frozen.
* `applyRoomGeometryNow()` (lines 579–607): `app.metersPerUnit = mpu`; also writes
  `layout.radius_m = mpu` into the current layout entry; `applyRoomRatio({width,length,height,
  rear,lower,centerBlend})` (optimistic: updates `app.roomRatio`, sets `dirty.roomRatio`,
  repositions every source/speaker mesh); then five commands in this order:
  `control_layout_radius_m {value:mpu}` (JSON `/omniphony/control/config/layout {"radiusM":v}`),
  `control_room_ratio_center_blend {value}` (`/omniphony/control/room_ratio_center_blend` float,
  clamped 0..1), `control_room_ratio {width,length,height}` (`/omniphony/control/room_ratio`
  3 floats, each `max(0.01)`), `control_room_ratio_rear {value}`, `control_room_ratio_lower {value}`;
  then `renderSpeakerEditor()`, normalise displays, set the dirty baseline.
* Renderer echo: snapshot `roomRatio {width,length,height,rear,lower,centerBlend,scaleM}` →
  `applyRoomRatio` adopts `scaleM` as `metersPerUnit` when > 0 (line 1023–1026). The
  `dirty.roomRatio` flush calls `renderRoomRatioDisplay()` (rewrites all fields, blend, mpu,
  summaries, editable state, baseline) **and** `onRoomRatioChanged()` for §9.5.
* `roomGeometryStateKey()` / `app.roomGeometryBaselineKey` track dirtiness (values rounded to
  1e-6) but only feed the non-existent Cancel button.

---

## 7. Display `#displayPanelRoot > #displaySection` (`index.html:243–498`, listeners `listeners/trails-and-display-listeners.js`, prefs `controls/room-geometry.js:84–462`)

Header: title `section.display` "Display", toggle `#displaySectionToggleBtn` (no summary). Content
is one bordered card (`border 1px solid rgba(255,255,255,0.08)`, radius 8 px, bg
`rgba(255,255,255,0.03)`, grid gap 0.3 rem). Sub-groups: each "section" below is a
`display:grid` block; sub-cards are `margin-left:1rem; padding:0.3rem 0.4rem; bg
rgba(255,255,255,0.03); radius 6px`. Rows are `.inline-toggle` (label left, switch right) or
`.control-row` with `grid-template-columns:auto 1fr` (label left, control right). Slider value
readouts are a `<span>` inside the label.

Every control here is **local display state**, persisted to localStorage (§7.13) and — for the
subset marked "mirror" — also pushed to the renderer's mpv overlay via OSC. None of them are
echo-driven except the overlay mirror (§7.3).

### 7.1 Language

| control | keys | values | behaviour |
|---|---|---|---|
| `#localeSelect` | label `app.language` "Language", help `help.display.language` | `auto, en, fr, de, ja, es, it, pt-BR, zh-CN`; option text "English / Native" (see §0.5) | change → `setLocale(value)`; value restored from `spatialviz.locale` at every `applyStaticTranslations` |

### 7.2 mpv overlay `#mpvOverlaySection`

| control | keys | default | behaviour |
|---|---|---|---|
| `#mpvOverlayToggle` | `mpvOverlay.title` "mpv overlay", help `help.display.mpvOverlay` | `overlay.enabled` module state (initial `true`) | change → `setMpvOverlayEnabled(checked)` → `mpv_overlay_set_active {enabled}` (OSC `/omniphony/control/overlay/enabled` int). No localStorage: orender persists it. Re-synced from `overlay:state` events (§7.3) |

### 7.3 Engine-owned overlay state (`mpvOverlay.js`)

Event `overlay:state` (`/omniphony/state/overlay`) carries `{enabled, objectsVisible,
labelsEnabled, heatmapEnabled, heatmapBands (1..12), trailsEnabled, trailTtlMs (≥500),
trailMode ('diffuse'|'line'), trailTeleportThreshold}`. `applyOverlayState` adopts each field into
`app.*` and, **only if something changed**, notifies listeners; the display listener
(`trails-and-display-listeners.js:358–371`) then re-checks `#trailToggle`, `#showObjectsToggle`,
`#objectLabelsToggle`, `#objectEnergyHeatmapToggle`, `#trailModeSelect`, calls
`applyObjectsVisibility()` and forces an energy-volume refresh. So an mpv keybind moves these
Studio switches. `syncMpvOverlayPrefs()` (called at the end of every `applyInitState`) pushes
Studio's current objects-visible, heatmap enabled/bands/colormap/custom stops, labels and trail
prefs to the renderer so persisted Studio prefs apply after a (re)connect.

### 7.4 Grid `#gridDisplaySection`

| control | keys | default | behaviour |
|---|---|---|---|
| `#vbapCartesianGridToggleBtn` | `display.grid` "Grid", help `help.display.grid` | `app.vbapCartesianFaceGridEnabled` (false) | listener lives in `listeners/renderer-panel-listeners.js:145–150`: sets the flag and calls `updateVbapCartesianFaceGrid()` (3D face grid; `scene/gizmos.js:157` reflects state back). Not persisted. |

### 7.5 Object appearance `#objectAppearanceSection` (sub-card always open)

| control | type | keys | default | on change (all persist via `persistEffectiveRenderPrefs`) |
|---|---|---|---|---|
| `#showObjectsToggle` | switch | `display.showObjects` "Show objects" / `help.display.showObjects` | `objectsVisible` true | `applyObjectsVisibility()`; **mirror** `mpv_overlay_set_objects {visible}` (`/omniphony/control/overlay/objects`) |
| `#objectColorsToggle` | switch | `display.objectColors` / `help.display.objectColors` | false | `updateSourceSelectionStyles()`, rebuild trails, `refreshOverlayLists()` (row accent colours, §9.3) |
| `#objectDisplayModeSelect` | select `circle` "Circle" / `transparent-sphere` "Transparent sphere" / `diffuse-sphere` "Diffuse sphere" (`display.objectDisplayMode.*`) | label `display.objectDisplayMode` "Object appearance" / `help.display.objectDisplayMode` | `'circle'` | `updateSourceSelectionStyles()` |
| `#objectSphereSizeSlider` + `#objectSphereSizeVal` | range 0.03–0.2 step 0.002; readout `toFixed(3)` ("0.070") | `display.objectSphereSize` "Object sphere size" / `help.display.objectSphereSize` | 0.07 | `input` (live): clamp 0.03..0.2, restyle + decorations |
| `#objectLabelsToggle` | switch | `display.objectLabels` / `help.display.objectLabels` | `objectLabelsEnabled` true | `updateSourceDecorations` for all; **mirror** `mpv_overlay_set_labels {enabled}` (`/omniphony/control/overlay/labels`) |
| `#showObjectDetailsToggle` | switch | `display.showObjectDetails` / `help.display.showObjectDetails` | true | `applyObjectDetailsVisibility(v)`: sets `app.showObjectDetails`, syncs `#objectDetailsToggleBtn` (§9.1), toggles `body.hide-object-details` (hides `.object-head` = coords + dominant-speaker line in every object row, tighter row padding) |
| `#effectiveRenderSection`: `#effectiveRenderToggle` + `#effectiveRenderInfoBtn` | switch; `i` button hidden, the "Effective render" text opens `#effectiveRenderInfoModal` (title `effectiveRender.infoTitle`, body `effectiveRender.infoBody`) | `effectiveRender.title` "Effective render" | `effectiveRenderEnabled` false | `refreshEffectiveRenderVisibility()` (3D marker + line per object) |

### 7.6 Trails `#trailSection`

| control | type | keys | default | on change (persist `persistTrailPrefs`; **mirror** `pushMpvOverlayTrailPrefs(enabled, ttlMs, mode, teleport)` → `mpv_overlay_set_trail_prefs` → OSC `/omniphony/control/overlay/trails [int enabled, int ttl_ms, string mode, float threshold]`) |
|---|---|---|---|---|
| `#trailToggle` (+ `#trailInfoBtn` hidden; "Trails" text opens `#trailInfoModal` `trail.infoTitle`/`trail.infoBody`) | switch | `trail.title` "Trails" | `trailsEnabled` true (HTML `checked`) | show/hide every trail line, rebuild geometry |
| `#trailModeSelect` | select `diffuse` "Diffuse" / `line` "Line" (`trail.mode.*`) | `trail.mode` "Mode" / `help.trail.mode` | `'diffuse'` | recreate every trail renderable |
| `#trailTtlSlider` + `#trailTtlVal` | range 1.0–20.0 step 0.5; readout `"{s.toFixed(1)}s"` ("7.0s") | `trail.duration` "Duration" / `help.trail.duration` | 7.0 s (`trailPointTtlMs = 7000`, floor 500 ms) | `input`: `trailPointTtlMs = max(500, s*1000)` |
| `#trailTeleportSlider` + `#trailTeleportVal` | range 0.05–2.0 step 0.05; readout `toFixed(2)` ("0.50") | `trail.teleport` "Teleport threshold" / `help.trail.teleport` | 0.5 | `input`: clamp 0.05..2.0, rebuild all trails |

Note `#trailSectionContent` has class `conditional-params open` in markup and is never toggled:
the trail sub-card is always expanded regardless of the trail switch.

### 7.7 Speakers display `#speakersDisplaySection` (title `display.speakers` "Speakers", no switch)

| control | type | keys | default | on change (persist) |
|---|---|---|---|---|
| `#speakerLabelsToggle` | switch | `display.speakerLabels` "Speaker labels" / `help.display.speakerLabels` | false | toggle 3D label sprites |
| `#speakerBandBarsToggle` | switch | `display.speakerBands` "Frequency bands" / `help.display.speakerBands` | false | toggle 3D band gauges |
| `#speakerFaceListenerToggle` | switch | `display.speakerFaceListener` "Aim at listener" / `help.display.speakerFaceListener` | false | `refreshSpeakerOrientations()` |
| `#speakerSizeSlider` + `#speakerSizeVal` | range 0.04–0.2 step 0.002, readout `toFixed(3)` ("0.080") | `display.speakerSize` "Speaker size" / `help.display.speakerSize` | 0.08 | `input`: clamp, rescale meshes |

### 7.8 Heatmaps `#heatmapsSection` (title `display.heatmaps` "Heatmaps"; `#heatmapInfoBtn` hidden, title text opens `#heatmapInfoModal` `heatmap.infoTitle`/`heatmap.infoBody`)

The block starts with the **shared band selector**, then four sub-cards.

**Band selector** `#heatmapBandSelect` (label `heatmap.crossoverBand` "Crossover band", help
`help.heatmap.crossoverBand`): options built by `syncCrossoverBandSelects()`
(`scene/speaker-band-select.js`) from `crossoverBandLabels(app.currentLayoutCutoffs,
{includeSingleBand:true})`: single band → one option value `"0"` text `heatmap.bandFull` "Full
band"; multi-band → one option per band (`< 100 Hz`, `100-1k Hz`, `>= 4k Hz`; `formatHz`: ≥1000 →
`k` with one decimal unless round) **plus** `value="all"` text `heatmap.bandAll` "All bands".
Shown value = `'all'` if `app.heatmapAllBands && labels.length > 1` else
`min(maxIndex, heatmapBandIndex)`; state is deliberately never clamped. Change → `heatmapAllBands =
(v==='all')`, else `heatmapBandIndex = round(v)`; invalidates all four volumes
(`last*At = 0`), re-syncs the select and the floating band cursor over the 3D view
(`controls/band-cursor.js`, which clicks through to this select), `refreshOverlayLists()`
(dominant-speaker readouts §9.3), `refreshEffectiveRenderVisibility()`, persist. Rebuilt on locale
change. Defaults: `heatmapBandIndex 0`, `heatmapAllBands true`.

**Speakers sub-card** (title `heatmap.speakers` "Speakers"):

| control | keys | default | on change |
|---|---|---|---|
| `#speakerHeatmapVolumeToggle` | label baked "Heatmap volume" (no `data-i18n`), help `help.heatmap.speakerVolume` | false | acquire/release gain-table subscription (`speakerSoloVolume` consumer), reset throttle, persist |
| `#speakerHeatmapVolumeColormap` | select `heatmap` (selected) / `blueWhite` / `whiteRed` / `red` / `custom` — texts `heatmap.objectEnergy.colormapHeatmap` "Heatmap", `…BlueWhite` "Blue → White", `…WhiteRed` "White → Red", `…Red` "Red (alpha only)", `…Custom` "Custom"; label `heatmap.objectEnergy.colormap` "Gradient", help `help.heatmap.colormap` | `'heatmap'` | store, reset throttle, show `#speakerGradientEditor` iff `custom`, persist |
| `#speakerGradientEditor` | `.gradient-editor` container, hidden unless custom | | **gradient editor = later phase**; entry point: `registerGradientEditor(el, 'speaker')` (`scene/gradient-editor.js:387`), edits `app.speakerCustomGradientStops` (2..8 stops `{pos,r,g,b}`), bumps `speakerCustomGradientVersion`, persists. Strings: `heatmap.gradient.addHint` "Double-click to add a stop", `heatmap.gradient.removeStop`, `heatmap.gradient.close` |

**Total energy sub-card** (title `heatmap.global` "Total energy"):

| control | keys | default / range | on change |
|---|---|---|---|
| `#globalEnergyHeatmapToggle` | `heatmap.globalEnergy` "Energy deviation" / `help.heatmap.globalEnergy` | false | acquire/release gain table (`globalEnergyVolume`), `refreshGaintableSubscription()`, persist |
| `#globalEnergyHeatmapScale` | number `.delay-input` min 1 max 40 step 1; label `heatmap.globalEnergy.scale` "Scale (±dB)" / `help.heatmap.globalEnergyScale` | 6 | `change`: `round`, clamp 1..40 (NaN → 6), write back, persist |

**Usage breaks sub-card** (title `heatmap.discontinuity` "Usage breaks"):

| control | keys | default / range | on change |
|---|---|---|---|
| `#discontinuityHeatmapToggle` | `heatmap.discontinuity.toggle` "Discontinuity" / `help.heatmap.discontinuity` | false | acquire/release gain table (`discontinuityVolume`), persist |
| `#discontinuityHeatmapMode` | select `gain` "Speaker configuration" / `centroid` "Energy centroid" (`heatmap.discontinuity.modeGain/Centroid`); label `heatmap.discontinuity.mode` "Metric" / `help.heatmap.discontinuityMode` | `'gain'` | store, `refreshGaintableSubscription()`, persist |
| `#discontinuityHeatmapScale` | number min 0.05 max 2 step 0.05; label `heatmap.discontinuity.scale` "Full scale" / `help.heatmap.discontinuityScale` | 0.5 | `change`: clamp 0.05..2 (≤0/NaN → 0.5), write back, persist |

**Objects sub-card** (title `heatmap.objects` "Objects"):

| control | keys | default / range | on change |
|---|---|---|---|
| `#objectEnergyHeatmapToggle` | `heatmap.objectEnergy` "Objects energy field" / `help.heatmap.objectEnergy` | false | refresh volume now; **mirror** `mpv_overlay_set_heatmap_enabled {enabled}`; persist |
| `#objectEnergyColormap` | same option set as above, default `blueWhite` selected; label `heatmap.objectEnergy.colormap` / `help.heatmap.colormap` | `'blueWhite'` | refresh; **mirror** `mpv_overlay_set_heatmap_colormap {colormap: index}` where index = position in `['heatmap','blueWhite','whiteRed','red','custom']`; if custom also `mpv_overlay_set_heatmap_custom_stops {stops: flat [pos,r,g,b,…]}`; show `#objectGradientEditor` iff custom; persist |
| `#objectGradientEditor` | hidden unless custom; entry `registerGradientEditor(el,'object')` — later phase | | edits `app.objectCustomGradientStops`; on change refresh + mirror stops + persist |
| `#objectEnergyHeatmapRadiusSlider` + `#objectEnergyHeatmapRadiusVal` | range 0.02–0.5 step 0.01, readout `toFixed(2)`; label `heatmap.objectEnergy.radius` "Falloff radius" / `help.heatmap.radius` | 0.5 | `input`: clamp (NaN → 0.12), refresh, persist |

**Common parameters sub-card** (title `heatmap.common` "Common parameters"; shared by the object
field and the speaker volume):

| control | keys | default / range | readout | on change |
|---|---|---|---|---|
| `#volumeSmoothToggle` | `heatmap.smooth` "Smooth gradient" / `help.heatmap.smooth` | false | — | refresh both volumes, persist |
| `#objectEnergyVolumeMixSlider` | `heatmap.objectEnergy.mix` "Accumulate ↔ Peak" / `help.heatmap.mix` | 0.6, range 0–1 step 0.01 | `toFixed(2)` | `input`: clamp 0..1 (NaN → 0), refresh shared, persist |
| `#objectEnergyVolumeGammaAccumulateSlider` | `heatmap.objectEnergy.gammaAccumulate` "Accumulate γ" / `help.heatmap.gammaAccumulate` | 4, range 1–10 step 0.1 (`VOLUME_GAMMA_RANGE.accumulate`, module default 2.5 used only for NaN) | `toFixed(1)` | `clampVolumeGamma('accumulate', v)`, refresh, persist |
| `#objectEnergyVolumeGammaMipSlider` | `heatmap.objectEnergy.gammaMip` "Peak γ" / `help.heatmap.gammaMip` | 3, range 0.2–3 step 0.05 (module default 0.8 for NaN) | `toFixed(2)` | `clampVolumeGamma('mip', v)`, refresh, persist |
| `#objectEnergyHeatmapResolutionSlider` | `heatmap.objectEnergy.resolution` "Field resolution" / `help.heatmap.resolution` | 64, range 8–64 step 2 | integer | `round`, clamp 8..64 (NaN → 24), refresh, persist |
| `#volumeRefreshSlider` | `heatmap.objectEnergy.refresh` "Refresh interval" / `help.heatmap.refresh` | 160, range 40–500 step 10 | `"{n} ms"` | `round`, clamp 40..500 (NaN → 160), persist (no immediate refresh) |
| `#objectEnergyHeatmapOpacitySlider` | `heatmap.objectEnergy.opacity` "Field opacity" / `help.heatmap.opacity` | 1.0, range 0.05–1.0 step 0.05 | `toFixed(2)` | clamp 0.05..1 (NaN → 0.55), refresh shared, persist |

The gain-table subscription (`scene/speaker-gaintable.js` `acquireGainTable/releaseGainTable/
refreshGaintableSubscription`) and the volume renderers are 3D-view concerns (later phase); the
panel only needs to call the equivalents. Persisted-on toggles re-acquire at startup
(`trails-and-display-listeners.js:320, 376, 409`).

### 7.9 Scene effects bar (`controls/scene-effects-bar.js`) — related, out of this panel

A floating icon bar over the 3D view mirrors `#vbapCartesianGridToggleBtn`, `#showObjectsToggle`
(with a flyout driving `#objectDisplayModeSelect`), `#objectLabelsToggle`, `#trailToggle`
(flyout → `#trailModeSelect`), `#objectEnergyHeatmapToggle`, `#speakerHeatmapVolumeToggle`,
`#mpvOverlayToggle` by dispatching `change` on those controls. The egui port must expose the
same setters so both surfaces stay in sync.

### 7.13 Preference persistence (localStorage)

| key | writer | contents | load-time clamps |
|---|---|---|---|
| `spatialviz.locale` | `i18n.js` | locale preference string | normalised to known locales |
| `spatialviz.trail_prefs` | `persistTrailPrefs` (`room-geometry.js:84–96`) | `{enabled, mode:'line'|'diffuse', duration_ms, teleport_threshold}` | `enabled = Boolean(parsed.enabled)` (**a missing key loads as false**, unlike the HTML default true); duration `max(500)`; teleport clamp 0.05..2.0 |
| `spatialviz.effective_render_prefs` | `persistEffectiveRenderPrefs` (lines 98–138) | `{enabled, objectsVisible, objectColors, objectDisplayMode, objectSphereSize, objectLabels, showObjectDetails, speakerLabels, speakerBands, speakerFaceListener, speakerSize, speakerHeatmapVolumeEnabled, speakerHeatmapVolumeColormap, heatmapBandIndex, heatmapAllBands, globalEnergyHeatmapEnabled, globalEnergyHeatmapScaleDb, discontinuityHeatmapEnabled, discontinuityHeatmapMode, discontinuityHeatmapScale, objectEnergyHeatmapEnabled, objectEnergyColormap, objectEnergyVolumeMix, objectEnergyVolumeGammaAccumulate, objectEnergyVolumeGammaMip, objectEnergyHeatmapBandCount, objectEnergyHeatmapResolution, objectEnergyHeatmapFalloffRadius, objectEnergyHeatmapOpacity, volumeRefreshMs, volumeSmoothInterpolation, objectCustomGradientStops, speakerCustomGradientStops}` | `loadEffectiveRenderPrefs` (lines 325–448): `enabled`/`objectColors` → Boolean (missing = false); sphere size 0.03..0.2; speaker size 0.04..0.2; scaleDb round 1..40; discontinuity scale 0.05..2; mix 0..1; gammas via `clampVolumeGamma`; bandCount round 1..12; resolution round 8..64; radius 0.02..0.5; opacity 0.05..1; refresh round 40..500; colormaps must be in `OBJECT_ENERGY_COLORMAPS`; gradient stops sanitised (2..8 stops, components clamped 0..1, sorted by pos) else default; legacy keys `speakerHeatmapBandIndex`/`speakerHeatmapAllBands` accepted as fallbacks |
| `audioMetering.rateHz.v1` (legacy `diagPlot.meterRateHz.v1`) | `controls/osc.js:621–669` | meter publish rate (10/20/50/100/200) | see §9.2 |
| `omniphony.updateCheck.*` | §2 | | |
| `objectTest.*` | §9.5 | | |

Both `load*Prefs` run at boot (`app.js:240–241`) before listeners, then `apply*PrefsToUi` writes the
values into every control listed above.

---

## 8. DRC / Loudness `#drcPanelRoot > #drcSection` (`index.html:499–540`, `controls/drc.js`, `controls/master.js:167–199`, `listeners/audio-panel-listeners.js:110–117`)

Header (`.panel-header`), left→right:

| element | content |
|---|---|
| title | `section.drc` = "DRC" in en.json (markup fallback "DRC / Loudness") |
| `#drcGaugeRow` | **visible only when `app.oscMeteringEnabled`** (`renderDrcUI`): a 6 px track (`#222`, radius 3) with `#drcGaugeFill` anchored **right** (`right:0`, width %), plus `#drcGainValue` (0.65 rem monospace `#888`, default "0.0 dB") |
| `#drcSummary` | `.panel-summary`, shown while collapsed: `"{mode} ({weight%}) | Loudness ON|OFF"` e.g. "Off (100%) \| Loudness OFF" (`updateDrcSummary`; the words are not localised) |
| `#drcInfoBtn` | hidden; the title text opens `#drcInfoModal` (`drc.infoTitle` "DRC and loudness", body `drc.infoBody`) |
| `#drcSectionToggleBtn` | ▸/▾ |

Gauge (`updateDrcMeterUI(gain)`, fed by event `meter:drc_gain {value}` = linear gain, also in
`state:batch`): `dB = linearToDb(gain)` (non-finite → −100); fill width = `min(100, |dB|/20*100)`
`toFixed(1)` %; text `"{+}dB.toFixed(1)} dB"` with a leading `+` when ≥ 0; fill colour: `dB > 1` →
`#33b5e5` (blue, boost), `dB < −12` → `#ff4444`, `dB < −6` → `#ffbb33`, else `#00c851`.

Content `#drcSectionContent` (card: border `rgba(255,255,255,0.08)`, radius 8 px, grid gap 0.35 rem):

| control | type | keys | value source | on change |
|---|---|---|---|---|
| `#drcControlRow` → `#drcModeSelect` | select `.form-select` (min-width 80 px); options = `effectiveDrcModes()` = dedup(`supportedDrcModes` + current `drcMode`), or `['Off']` when empty; option text = raw mode string | label `input.drc` "DRC", help `help.drc.mode` | `app.drcMode` (snapshot `drcMode`, `supportedDrcModes`) | `app.drcMode = v` (optimistic), `control_drc_mode {value}` → OSC `/omniphony/control/input/drc_mode <string>` |
| `#drcWeightRow` → `#drcWeightSlider` + `#drcWeightValue` | range 0–100 step 1, full width; value `"{n}%"` | label `input.drc_weight` "DRC weight", help `help.drc.weight` (anchor `#drcWeightRow`); label row 0.65 rem `#888` | `app.drcWeight` 0..1 (snapshot `drcWeight` clamped) → slider `round(w*100)` (not rewritten while focused) | `input` (live): `w = clamp(v/100)`, optimistic, `control_drc_weight {value}` → `/omniphony/control/input/drc_weight` float 0..1 |
| divider + `#loudnessToggle` | switch | label `section.loudness` = "Loudness" (markup "Loudness / Dialog Norm"), help `help.drc.loudness` | `app.loudnessEnabled === true` (snapshot `loudness` 0/1) | `app.loudnessEnabled = checked` (optimistic), `updateLoudnessDisplay()`, `control_loudness {enable:0|1}` → `/omniphony/control/loudness` int |
| `#loudnessInfo` | 10 px monospace `#8fa6bd`, single line ellipsis in markup but the JS writes **three lines** via `innerHTML` with `<br>` | — | `renderLoudnessDisplay`: `"source loudness: {src} dBFS"` (`formatNumber(loudnessSource,0)` or `—`), `"target loudness: {src + linearToDb(gain)} dBFS"` (or `—` when either is missing or gain ≤ 0), `"correction: {gain.toFixed(2)} ({formatLinearAsDb(gain)})"` (or `—`) | — |

`renderDrcUI` always forces `#drcControlRow` and `#drcWeightRow` visible (their markup default is
`display:none`), rebuilds the option list only when it differs, then `renderDrcWeightUI()` and
`updateDrcSummary()`. Triggered by `dirty.drcUI` (set when snapshot `drcMode`, `drcWeight` or
`supportedDrcModes` arrive) and directly at the end of `applyInitState`. `dirty.loudness` →
`renderLoudnessDisplay` also refreshes the summary.

---

## 9. Objects `#objectsPanelRoot > #objectsSection` (`index.html:541–581`) and the two pinned editors

### 9.1 Header

| element | behaviour |
|---|---|
| title `section.objects` "Objects" | static |
| `#objectDetailsToggleBtn` (`.panel-toggle-btn`, title `display.showObjectDetails`) | shows `▾` when `app.showObjectDetails` else `▸`, class `active` + `aria-pressed`; click toggles the same state as `#showObjectDetailsToggle` (§7.5) and persists |

### 9.2 Metering row (`index.html:557–571`, `controls/osc.js:603–669`)

`.inline-toggle` with three items: label `osc.metering` "Metering" (help `help.osc.metering`),
switch `#oscMeteringToggle`, and select `#oscMeteringRateSelect` pushed right (`margin-left:auto`,
baked English tooltip "Audio meter publish rate (Hz) — drives the level meters only. Diag
publication has its own rate in the diag plot controls.").

* `#oscMeteringToggle`: exempt from the runtime lock. Value from `get_osc_config().osc_metering_enabled`
  (form load), snapshot `oscMeteringEnabled` (0/1) and event `osc:metering {enabled}` (which also
  nulls decode/render/write times when off). Change → `app.oscMeteringEnabled = v` (optimistic), log
  `log.oscMeteringEnabled`/`Disabled`, `control_osc_metering {enable:0|1}` (host persists the
  config and sends `SetMeteringEnabled`). It is also part of the OSC form dirty key (§3.3).
  Side effect: `#drcGaugeRow` visibility (§8).
* `#oscMeteringRateSelect`: options `10 20 50 100 200` Hz (text "N Hz"), default 50. Pre-connect
  value from localStorage `audioMetering.rateHz.v1` (fallback legacy key). Snapshot `meterRateHz`
  → `syncMeterRateFromRenderer(hz)`: rounds, ignores values outside the list, applies to the select
  and mirrors to localStorage. Change → persist and `control_metering_rate_hz {value}` → OSC
  `/omniphony/control/metering/rate_hz` float (host clamps 1..1000).

### 9.3 Object injection switch (`index.html:575–578`, `controls/object-test.js:1328–1351`)

`#objectTestFeatureRow` `.switch-row` (13 px): label `objectTest.feature` "Object injection", help
`help.objectTestFeature` (anchor `.switch-row`), switch `#objectTestFeatureToggle`. Persisted in
localStorage **`objectTest.feature.v1`** (`'1'|'0'`), applied at boot by `objectTestBoot()` after
the scene exists. On:

* `setIdleFeedRequest('object-test', true)` (`controls/test-idle-feed.js`: ref-counted;
  transitions send `control_speaker_test_idle_feed {enable}` → `/omniphony/control/speaker_test/idle_feed`
  and re-arm every 120 s);
* `pushSource()` registers a source with id `'injection'` (`OBJECT_TEST_SOURCE_ID`), name
  `t('objectTest.markerLabel')` "Test object", cartesian position; it appears in the list with
  badge code `INJ` and sorts **last** (non-numeric id);
* `sendRotation()` restates the orbit; `setSelectedSource('injection')` (opens the editor §9.5).

Off: stop if playing, `removeSource('injection')`, deselect if selected.

### 9.4 Objects list `#objectsList` (`.info-list`; `speakers.js:1304–1672`, `sources.js`, `mute-solo.js`)

Empty state text `objects.none` "No objects.".

**Ordering** (`renderObjectsList`, lines 1611–1641): group rank first (`objectBadge(id).type`:
bed/ADM = 0, `phantom` = 1, `height` = 2), then canonical bed order by displayed code
(`canonicalChannelOrder(formatObjectLabel(id))`: L R C LFE Ls Rs Lb Rb TFL TFR TBL TBR … per the
renderer catalogue or `FALLBACK_BED`), bed channels before non-bed, then numeric ids ascending,
numeric before non-numeric, finally `localeCompare`. The list is fully re-rendered when an id
appears or an existing id's name changes (`updateSource`, `sources.js:1128–1137`); otherwise rows
are patched in place through the dirty sets (§0.8). Items are cached in `objectItems` (Map id →
entry).

**Row anatomy** (`createObjectItem`, `.info-item.object-item`, grid `18px 1fr`, gap 0.45 rem,
padding `0.18rem 0.28rem` in `#overlay`, bg `rgba(255,255,255,0.04)`, radius 6 px; whole row is a
click target → `setSelectedSource(id)`; readouts are `pointer-events:none`, only M/S buttons are
interactive):

```
┌──────┬──────────────────────────────────────────────────────────────┐
│ id   │ .object-head   : .object-coords (x: y: z: | az: el: r:)  .object-topright │
│ strip│ .meter-row     : [pos icon] [dB text] [meter bar] [size gauges] [M][S]    │
│(vert)│ .object-contrib-row : band bars (hidden unless a speaker is selected)     │
└──────┴──────────────────────────────────────────────────────────────┘
```

| part | content / rule |
|---|---|
| `.id-strip.flip` + `<span>` (badge) | vertical text (`writing-mode:vertical-rl`, rotated 180°), 11 px/600 `#d9ecff`, bg `rgba(0,0,0,0.55)`. Text = `objectBadge(id).code` (§9.4.1), or an **icon** `▲` (height) / `◇` (phantom, coloured `#ffe66d`) with class `object-type-icon` (horizontal, 12 px). When an icon is used the row `title` = full display name. Strip classes: `type-height`/`type-phantom`; `has-active-trail` (trail points exist) → bg `rgba(124,231,255,0.28)` + inset ring; `object-colorized` (when `objectColorsEnabled` or the object has an A/B tag) → bg = `color-mix(--object-accent 34%, black 55%)`, text `#edf5ff` |
| `.object-coords` (in `.object-head`, hidden by `body.hide-object-details`) | six `<span class="coord-axis coord-{x,y,z,az,el,r}">` = `"x:0.5"` … from `decomposePosition(pos)`: x/y/z `toFixed(1)`, az/el `toFixed(1)` (degrees), r `toFixed(2)`; az/el/r from the payload's polar fields when present else derived (`az = atan2(x,y)`, `el = atan2(z, hypot(x,y))`). For a **direct** fixed channel the coordinates shown are the destination speaker's. 9 px monospace `#8a9aac`, a `|` separator between xyz and aed |
| `.object-topright` | 95 px, 10 px `#b9c7d8`, right aligned: direct channel → `"→ {speakerName}"` (title `channelEdit.destinationSpeaker + ': ' + name`); else `getObjectDominantSpeakerText(id)` = `"{speakerId} {formatLinearAsDb(bestGain)}"` using the selected heatmap band's gains (`sourceBandGains[heatmapBandIndex]`) else the full-band gains, or `—` |
| `.object-position-icon` | 16×16 SVG top view (`positionIconMarkup`): frame rect (stroke `currentColor` `#5d6b7d`, or black when `spatialize === 0` i.e. direct), 3.2 px square marker at `cx = 2+(x+1)/2*12`, `cy = 2+(1−y)/2*12`, fill `hsl(240·(1−clamp01(z)), 75%, 52%)` (blue floor → green mid → red ceiling); tooltip `"X x.xx  Y y.yy  Z z.zz"` prefixed by the label (`"{label} → {speaker} — …"` for direct) |
| `.fixed-metric` (dB text) | 8 ch monospace right-aligned: `"{rms.toFixed(1)} dB"` (`formatNumber(rms,1)`), `— dB` when no meter |
| `.meter-bar.level-meter` | 6 px bar; `.meter-fill` clipped to `--level` = `dbToMeterPercent(peak)` where scale is **−60…+6 dBFS** (0 dBFS at 90.9 %); a red headroom zone `rgba(255,80,80,0.32)` from 90.9 % to 100 %; `.meter-peak` 2 px cursor at `dbToMeterPercent(peakHoldDbfs)` (backend-computed hold), hidden when ≤ 0.1 %, class `over` (solid `#ff3b3b`) when hold ≥ 0 dBFS; `.meter-fill.contribution` overlay (cyan→yellow gradient) showing this object's contribution to the **selected speaker** (`rms + linearToDb(gain)` mapped through the same scale) while the base fill drops to opacity 0.38 |
| `.object-size-gauges` | three 2 px bars labelled W / D / H (7 px labels), widths = `sourceSizes[id].{w,d,h}` × 100 % (from `source:size` events); colours orange / blue / green gradients |
| `.object-meter-actions` | `M` and `S` `.toggle-btn` (11 px). `active` class when muted / when this id is the solo target |
| `.object-contrib-row > .band-contrib-bars` | only when a speaker is selected **and** per-band gains exist: one `.band-row` per crossover band = label (band range text, or `heatmap.bandFull` / `tf('heatmap.bandIndex',{index})`), a 6 px bar filled to `min(100, gain·100)` % coloured by `bandColor(b, n)` (red lowest → blue highest), and `formatLinearAsDb(gain)` |

Row state classes (`updateObjectItem`, `updateObjectControlsUI`): `is-selected` (bg
`rgba(46,110,64,0.45)`, border `rgba(90,200,120,0.35)`) when `app.selectedSourceId === id`;
`is-muted` (opacity 0.35) when in `objectMuted`; `is-dimmed` (opacity 0.45) when another object is
the solo target **or** the object's metadata gain ≤ −128 dB (`metadataGainDb`, "silent" objects,
which are also hidden in 3D).

**Colours** (`sources.js:179–291`): with `objectColorsEnabled`, or always for objects tagged
`A`/`B` (`sourceTags`, from `object:source_tag` events or an id prefix `a_`/`b_`), the row gets
`--object-accent` = the object's base colour: tag A → `#ff8b6b`, tag B → `#62d7c7`, else
`OBJECT_COLOR_PALETTE[numericId % 16]` (or FNV-1a hash of the id for non-numeric ids); the 16-entry
palette is at `sources.js:179–196`.

**Mute / solo** (`mute-solo.js:205–293`): `toggleMute` flips membership in `objectMuted` +
`objectManualMuted` and sends `control_object_mute {id:Number(id), muted:0|1}` → OSC
`/omniphony/control/object/{id}/mute`; for the injection source it calls `setObjectTestMuted`
instead (its id is not numeric). `toggleSolo`: if another object is soloed → swap (mute the old
target, unmute this, select this); if this is the solo target → unmute everyone else; else mute
every other object and select this one. The solo target is *derived*: the single unmuted object
while ≥2 objects exist and all others are muted (`getSoloTarget`). Selecting a different object
while a solo is active re-targets the solo to it (`setSelectedSource`, `sources.js:945–960`).
Renderer echo `object:mute {id, muted}` updates the sets and re-renders controls.

**Selection** (`setSelectedSource`): sets `app.selectedSourceId`, restyles 3D, calls
`updateObjectControlsUI()` (which decides which pinned editor shows) and scrolls the row into view.
Deselection is done by clicking in the 3D view (picking.js, out of scope) — there is no
deselect control in the panel.

**Meter decay** (`speakers.js:2410–2461`): per animation frame, any source meter not refreshed for
250 ms decays at 45 dB/s (floor −100) and re-marks its row dirty.

#### 9.4.1 Badge codes (`objectBadge`, `sources.js:142–173`)

`getObjectDisplayName` strips a leading `a_`/`v_`/`obj_` (also `:`/`-` separators). The badge
`type` is the renderer's `kind` (`height`/`phantom`, from the position payload). Code: `'injection'`
→ `INJ`; `Ambience_X` → `X`; `Height_X_synth` → `X`; `Diffuse_X` → `X`; `Phantom_A_B` → `A·B`;
`Phantom_A` → `A`; `DirectH_X` → `X↑`; `Direct_X` → `X`; otherwise everything after the first
`_` (or the whole name).

### 9.5 Channel editor `#channelEditSection` (`index.html:704–773`, `controls/virtual-bed.js:553–721`, `listeners/channel-editor-listeners.js`)

Visible iff the selected object's name canonicalises to a bed channel (`selectedChannelName()`,
alias-tolerant via the catalogue: whitespace/`_`/`-` stripped, upper-cased). Title
`#channelEditTitle` = `t('channelEdit.title') + ' — ' + name` ("Channel Editor — LFE"). The
rebuild is skipped while a field inside the section has focus (unless forced) and during a gizmo
drag. Rows in DOM order:

| control | type | keys | value / rule | on change |
|---|---|---|---|---|
| `#channelEditGainSlider` + `#channelEditGainBox` | range −24…+12 step 0.1 (`.gain-slider`), box `.gain-box` (54 px, right) | label `channelEdit.gain` "Gain", help `help.channelEdit.gain` | slider = `gainDb`; box `"{+}g.toFixed(1)} dB"` (`+` prefix when > 0) | `input` → box only; `change` → `applyChannelGain(name, v)` (rounded to 0.1 dB); **double-click** → 0 dB (box "0 dB") and apply |
| `#channelEditSpatializeToggle` + `#channelEditSpatializeText` | switch (`.switch-row`) | text `virtualBed.virtual` "Virtual" when on, `virtualBed.direct` "Direct" when off | `checked = spatialize !== false` | `applyChannelPlacement(name, checked)` |
| `#channelEditDirectTarget` (`.channel-direct-target`, grid) | shown (`display:grid`) only when Direct | `channelEdit.destinationSpeaker` "Destination speaker", `#channelEditDirectTargetName` = resolved speaker id/name or `channelEdit.noMatchingSpeaker` "No matching speaker", small `channelEdit.destinationPosition` "The coordinates below are the speaker's actual position." | speaker = renderer-reported `sourceDirectSpeakerIndices[selectedId]` else label match in `currentLayoutSpeakers` | — |
| `.coord-mode-row` radio `#channelEditCartesianMode` (`name=channelCoordMode`, value cartesian) + `common.cartesian` "Cartesian:" | radio | | checked per `mode` (below) | `app.channelEditCoordMode = 'cartesian'`, force re-render |
| Cartesian table `.cart-coord-table` | heads "X" "Y" "Z" (titles "Omniphony X (left/right)", "Y (rear/front)", "Z (down/up)"); row `speaker.normalizedCoords` "Norm.": `#channelEditXInput/YInput/ZInput` number step 0.001; row `speaker.metersCoords` "Real (m)": `#channelEditXMetersInput/…` step 0.01 | | Norm = `formatNumber(v,3)` of the ADM position (or of the destination speaker when Direct); metres = `normalizedToMeters(norm)` `formatNumber(v,2)` | Norm change → `applyChannelCartesian(name, x, y, z)` with the *untouched* axes pulled from canonical state; metres change → edited axis in metres, others canonical, → scene units → `applyChannelSceneCartesian` |
| `#channelEditCartesianGizmoBtn` `.toggle-btn` `speaker.edit3d` "3D Edit" | button | | `active` when `cartesianEditArmed && activeEditMode==='cartesian'` | toggles `cartesianEditArmed` (clears polar), `updateSpeakerGizmo()` (3D gizmo, later phase) |
| radio `#channelEditPolarMode` + `common.polar` "Polar:" | radio | | | `channelEditCoordMode = 'polar'` |
| Polar table | heads "Az°" "El°" "Dist"; Norm row `#channelEditAzInput` (step 0.1), `#channelEditElInput` (0.1), `#channelEditRInput` (step 0.001 min 0.01); Real row: two empty cells + `#channelEditRMetersInput` (step 0.01 min 0.01) | | az/el `formatNumber(v,1)`, r `formatNumber(v,3)`, r metres = `hypot(mx,my,mz)` `toFixed(2)` | Az/El/R change → `applyChannelPolar(name, az, el, r)` (others canonical); R metres → `r / metersPerUnit()` |
| `#channelEditPolarGizmoBtn` "3D Edit" | | | `active` when `polarEditArmed && activeEditMode==='polar'` | toggles polar arming |

Mode selection: Direct with a resolved speaker → mode follows the speaker's `coord_mode`; else
`app.channelEditCoordMode`. When Direct, every position input, both radios and both 3D Edit
buttons are **disabled** (only gain and the switch remain editable); when Direct without a
matching speaker, all coordinate fields are blanked.

Commit path (`commitChannel`, `virtual-bed.js:268–279`): mutate the channel in the effective set,
`app.virtualBed = buildLayoutPayload(channels)` = `{radius_m, speakers:[{name, coord_mode:
'cartesian'|'polar', spatialize, x,y,z (clamped −1..1) | azimuth,elevation,distance (>0),
gain_db (omitted when 0, 0.1 dB rounding)}]}`, `invoke('control_virtual_bed', {value: JSON})` →
OSC `/omniphony/control/virtual_bed <json string>` (empty string = renderer built-in poses),
then `syncVirtualBedObjects(true)` and `renderChannelEditor(true)`. Optimistic: the renderer echoes
`virtualBed` in the snapshot. `resetVirtualBed()` (§5 button, and the one-shot
`materializeDefaultVirtualBed()` on the first snapshot that reports `virtualBed == null`) rebuilds
every channel at its catalogue/fallback cartesian corner with `spatialize` per catalogue (LFE
direct), gain 0, and pushes the whole bed.

### 9.6 Object injection editor `#objectTestEditSection` (`index.html:587–701`, `controls/object-test.js`, host `commands/gain.rs:120–207`)

Visible iff `featureOn && app.selectedSourceId === 'injection'` (`renderObjectTestEditor`). Title
`section.objectTest` "Object injection" with help `help.objectTest`. Body `.editor-body` in DOM
order:

| # | control | type | keys | persisted key / default | behaviour |
|---|---|---|---|---|---|
| 1 | `.object-test-transport`: `#objectTestPlayBtn` | 34 px round `.transport-btn` with play ▶ / pause ❚❚ SVG; `aria-pressed` = playing; playing state = green tint `rgba(82,226,162,0.18)`, text `#7af0c0`, pulsing ring animation (2 s) | title/aria-label = `objectTest.stop` "Stop the test signal" when playing else `objectTest.play` "Play the test signal" | — | click: `enabled = !enabled`; if starting also `sendRotation()`; `send()`; re-render; `pushSource()` |
| 1b | `#objectTestSignalSelect` | select `pink` "Pink noise", `bursts` "Noise bursts", `low` "Low band (time cues)", `high` "High band (level cues)", `band` "8 kHz band (elevation)", `tone` "Tone (500 Hz)", `clicks` "Clicks", `clip` "Audio file" (`objectTest.signal*`) | label `objectTest.signal` "Test signal" (`.transport-label`), help `help.objectTestSignal` (anchor `.object-test-transport`) | `objectTest.signal.v1` / `pink` | change → persist, re-render (shows clip row iff `clip`), and if playing `send()` (restarts the stimulus) |
| 2 | `#objectTestClipRow` (`display:flex` only when signal = clip): `#objectTestClipName` + `#objectTestClipBtn` | note + `.ui-btn.ui-btn-compact` `objectTest.clipChoose` "Choose…" | name: `objectTest.clipNone` "No file chosen"; after a renderer answer (`state:object_test_clip` JSON `{name, seconds, truncated}` or `{error}`): `"{name} · {seconds.toFixed(1)} s"` + `" · truncated"` (`objectTest.clipTruncated`) when truncated; an error is shown in `var(--danger,#ff8080)` | — | Choose → native dialog (`@tauri-apps/plugin-dialog` `open`, filter WAV) → `control_object_test_clip {path}` → OSC `/omniphony/control/object_test/clip` |
| 3 | `#objectTestAdmViewRow` → `#objectTestAdmViewToggle` | switch (12 px row) | `objectTest.admView` "ADM view (square)", help `help.objectTestAdmView` | `objectTest.admView.v1` / off | rebuild the faces in unit-cube coordinates instead of room coordinates |
| 4 | hint note | 10 px | `objectTest.hint` "Click or drag on a face to place the source." | | |
| 5 | `#objectTestFaces` | the CAD sheet (§9.6.1) | | | |
| 6 | `#objectTestCoords` | 10 px tabular note | text `"x {x}   y {y}   z {z}"` (`toFixed(2)`) | | live |
| 7 | `#objectTestSnapRow` → `#objectTestSnapToggle` | switch (13 px) | `objectTest.snap` "Snap to grid", help `help.objectTestSnap` | `objectTest.snap.v1` / off | `disabled` while no grid is known (`get_vbap_grid_nodes` → `{x[],y[],z[]}` node lists, refreshed on connect and on `vbap:cartesian` changes via `refreshVbapGridNodes`); turning on re-snaps the current position; note `#objectTestSnapNote` = `objectTest.snapNoGrid` "No Cartesian grid: the backend is not precomputed on one." shown when on without a grid. Holding **Alt** during a drag bypasses the snap |
| 8 | `.editor-row.gain-row`: `#objectTestLevelSlider` + `#objectTestLevelBox` | range −60…0 step 1; box `"{n} dBFS"` | `objectTest.level` "Test level", help `help.objectTestLevel` | `objectTest.levelDb.v1` / −8 (clamped −60..0) | `input` → persist, re-render, if playing `send()` (level sent linear `10^(dB/20)`) |
| 9 | `#objectTestRotationRow` → `#objectTestRotationAxis` | select `z` "Horizontal (about Z)", `x` "About X", `y` "About Y", `free` "Free axis" (`objectTest.axisZ/X/Y/Free`) | `objectTest.rotationAxis` "Rotation axis", help `help.objectTestRotation` | `objectTest.rotation.v2` (JSON `{axis, radius, period, azimuth, elevation}`) / `z` | `applyRotation()`: persist, re-render, redraw orbit path, `sendRotation()` |
| 10 | `#objectTestRadiusSlider` (inside `.object-test-radius-wrap` with `.object-test-radius-ticks` tick marks at √2, √3, 2√2, 2√3 → 35.355 %, 43.301 %, 70.711 %, 86.603 % of the track) + `#objectTestRadiusBox` | range 0–4 step 0.01; box `objectTest.radiusOff` "off" when 0, else `r.toFixed(2)` + `" √2"`/`" √3"`/`" 2√2"`/`" 2√3"` when exactly on a mark | `objectTest.radius` "Radius", help `help.objectTestRadius` | radius 0 | `input`: `snapRadius` (snap within ±0.04 of a mark), `applyRotation()` |
| 11 | `#objectTestPeriodSlider` + `#objectTestPeriodBox` | range 0–1000 step 1 = **log scale** position: `period = 0.5·(30/0.5)^(pos/1000)` rounded to 0.05 s below 1 s, 0.1 s below 10 s, 0.5 s above; box `"{p.toFixed(2)} s"` below 1 s else `toFixed(1)` | `objectTest.period` "Turn time", help `help.objectTestPeriod` | period 4 s (slider 508) | `input` → `applyRotation()` |
| 12 | `#objectTestFreeAxisRows` (`display:flex` only when axis = free): `#objectTestAzimuthSlider` −180…180 step 1 + box `"{n}°"`; `#objectTestElevationSlider` −90…90 step 1 + box | ranges | `objectTest.axisAzimuth` "Axis azimuth", `objectTest.axisElevation` "Axis elevation" (no help) | 0 / 0 | `input` → `applyRotation()` |
| 13 | `#objectTestIsolationRow` → `#objectTestIsolationSelect` | select `test_only` "Test only" / `with_programme` "With programme" | `objectTest.isolation` "Programme", help `help.objectTestIsolation` | `objectTest.isolation.v1` / `test_only` | change → persist, if playing `send()` |
| 14 | `#objectTestActions` → `#objectTestCentreBtn` | `.ui-btn.ui-btn-compact`, right-aligned | `objectTest.centre` "Centre the source" | | `setPosition([0,0,0])` (listener origin) |

Renderer contract: `send()` → `control_object_test {on: enabled && !muted, x, y, z, level (linear),
size: 0, isolation, signal}` → OSC `/omniphony/control/object_test [int on, f x, f y, f z,
f level, f size, s isolation, s signal]` (host clamps xyz −1..1, level/size 0..1); it fires on
every position change while playing (slides, no restart). `sendRotation()` →
`control_object_test_rotation {axis, radius, period, azimuth, elevation}` → OSC
`/omniphony/control/object_test/rotation` (radius clamped 0..4, period 0.05..600).
Position is persisted in `objectTest.position.v1` (JSON `[x,y,z]`, default `[0,1,0]` front-centre).
The renderer reports the live position + meter on `objectTest:position {x,y,z,peakDbfs,rmsDbfs}`
(also batched): while playing the 3D object follows the reported position; the sheet marker always
shows the *placed* position. `beforeunload` stops the test. Muting from the list (M/S) sends
`on:false` while remembering `enabled`, so unmute resumes.
`renderCoords` also writes `#objectTestSummary`, which does **not** exist in the markup (dead).

#### 9.6.1 The CAD sheet `#objectTestFaces` (`object-test.js:576–1184`) — placement widget

One SVG (`width:100%; max-height:300px`, crosshair cursor, `touch-action:none`), viewBox normalised
to 100 on its larger side, laid out first-angle: **side wall** view top-left (`objectTest.faceSide`
"Side wall": horizontal = depth, back at left/front at right; vertical = height, ceiling up),
**front wall** top-right (`objectTest.faceFront` "Front wall": horizontal = lateral, vertical =
height), **floor plan** bottom-right (`objectTest.facePlan` "Floor (plan)": horizontal = lateral,
vertical = depth with front at the top), and a 45° dashed mitre line in the empty bottom-left
cell. Face rectangles follow the room extents (`roomRatio` width/length/rear/height/lower and the
`centerBlend` depth warp via `normalizedOmniphonyToScenePosition`) unless ADM view (unit cube,
square faces). Gutter = 0.24 × largest extent. Each face has: box (`rgba(255,255,255,0.05)`
fill, stroke `rgba(217,236,255,0.35)`), dashed axes through the **origin**, caption (3.4 units),
depth end-labels `objectTest.axisFront` "front" / `objectTest.axisBack` "back" on the two faces
that show depth, an optional grid-tick path (snap nodes projected on the edges, thinned when
closer than 1.2 units), the orbit polyline (`rgba(92,255,154,0.85)`, 96 samples of the clamped
orbit mirrored from the renderer's `position_at`), and the marker circle r 2.4 (`#5cff9a`).
Four single-axis sliders live in the gutters (`SLIDERS`, lines 659–664): `x` below the front
view, `z` left of the front view, `ySide` below the side view, `yFloor` left of the floor view;
each is a hairline track with a centre tick and a green thumb (r 2.1), keyboard-focusable
(`role=slider`, aria labels `objectTest.sliderX` "Left / right", `sliderY` "Back / front",
`sliderZ` "Floor / ceiling"; arrows ±0.02, PageUp/Down ±0.1, Home/End = ±1 in ADM units).
Pointer down on a face/slider captures the pointer; drags set the two face axes (or the one
slider axis) leaving the others untouched; sliders win over faces where their grab area (±3.2
units) overlaps. Rebuilt when any extent, the blend, the view mode or the locale changes
(`builtRatioKey`), and on `dirty.roomRatio` via `onRoomRatioChanged()`.

---

## 10. Elements deliberately out of the "panels" phase (entry points only)

| element | where | phase |
|---|---|---|
| `#speakerGradientEditor`, `#objectGradientEditor` (`scene/gradient-editor.js`) | §7.8 | gradient editor: keep the "Custom" option and an empty placeholder |
| 3D "3D Edit" gizmo arming (`updateSpeakerGizmo`, `scene/gizmos.js`) | §9.5 | viewport gizmos |
| Heatmap volumes, gain-table subscription, trails, labels, band cursor over the viewport | §7 | viewport / later |
| WebGL recovery on layout changes (`visual-recovery.js`) | §0.1 | n/a for wgpu |
| Native file dialogs (`pick_orender_path`, `pick_bridge_path`, WAV picker) | §3.5, §4, §9.6 | host-side; needs rfd or equivalent |
| GitHub update fetch (`fetch()` from the webview) | §2 | host HTTP |
| Scene effects bar (`controls/scene-effects-bar.js`) | §7.9 | viewport overlay |

## 11. Things this spec could not determine from the sources

* Exact byte layout of the `/omniphony/state/*` messages behind snapshot fields (only the
  Tauri-side camelCase names are cited; `app_state.rs` is the authority the engineer already has).
* `OscControlMsg::Reconnect` semantics and how the host chooses `listen_port` when `osc_port = 0`
  (in `osc_listener.rs`, not read here).
* `get_vbap_grid_nodes` output when the backend is polar/realtime (returns `None` → snap disabled).
* The `--panel-open-max-height` CSS variable value for the OSC form (defined outside the read ranges).
* `ui/layout/overlay-layout-state.js` localStorage key names for panel width/collapse.
* `objectTest.hint` and the object-test summary element referenced by JS do not exist in
  `index.html`; treat `#objectTestSummary` as dead code.
* `section.drc` and `section.loudness` en.json strings ("DRC", "Loudness") differ from the
  markup fallbacks ("DRC / Loudness", "Loudness / Dialog Norm"); the en.json strings win at runtime.
