# Omniphony Studio — Phase 2 specification: RIGHT overlay, save footer, scene-effects bar, band cursor

Source tree: `omniphony-studio/`
(branch `feat/studio-egui-panels`). All paths below are relative to that
directory unless prefixed with `src-tauri/`. Line numbers are from the tree as
read on 2026-09-10.

This document covers, in DOM order, everything inside `#speakersOverlay`
(`src/index.html:775-950`), then the three fixed elements that sit outside
both overlays: `#bandCursor` (`index.html:952`), `#saveFooterRoot` with its
`#sceneEffectBar` (`index.html:954-1008`). It also documents the side-panel
collapse/resize model (`src/ui/layout/overlay-layout-state.js`,
`src/ui/side-panels.js`) and how the 3D viewport relates to the overlays
(`src/core/render/render-surface-controller.js`,
`src/core/viewport/window-viewport.js`).

Reading order inside the right overlay:

1. `#audioPanelMount` → replaced at boot by `#audioPanelRoot` (`src/ui/audio-panel.js`) — §2
2. `#rendererPanelMount` → replaced by `#rendererPanelRoot` (`src/ui/renderer-panel.js`) — §3
3. `#speakersSection` (static HTML) — §4
4. `#speakerEditSection` (static HTML, pinned below the scroll) — §4.6

Then §5 save footer, §6 scene-effects bar, §7 band cursor, §8 layout state /
side panels / viewport, §9 out-of-scope entry points, §10 open points.

---

## 0. Conventions used by every panel

### 0.1 Where values come from

- **`AppState`** (`src-tauri/src/app_state.rs`, copied verbatim into the egui
  crate at `src/model/app_state.rs`) is the host-side model. The web UI never
  reads it directly: it receives (a) the whole struct serialised as the
  `state:snapshot_ready` payload (camelCase keys, `app_state.rs:417-655`) and
  applied by `applyInitState` (`src/init.js:93-716`), and (b) incremental
  Tauri events (`src/tauri-bridge.js`). In the egui port the panels read the
  model directly; the JS-side `app` object (`src/state.js:100-579`) is the
  mirror whose field names are quoted below as "`app.xxx`", with the
  `AppState` field given alongside when it differs.
- Every control's **default** given below is the *baked HTML default* (what
  the web page shows before the first snapshot). The renderer's snapshot
  overwrites it; the `AppState::default()` values (`app_state.rs:796-910`)
  are what the host reports before any OSC state has arrived.
- **Sending**: every change goes through a Tauri command in
  `src-tauri/src/commands/*.rs`; each command builds one OSC message. The
  OSC address and argument clamps are quoted for each control so the egui
  host (which has no Tauri layer) can send the same message from
  `src/host/control.rs`.
- **Optimistic local write**: unless stated otherwise, the JS writes the new
  value into `app` *before* sending, re-renders, then sends. The renderer
  echoes the canonical value on the next state broadcast, which overwrites
  the optimistic value. Controls marked "not optimistic" only send and wait
  for the echo.
- **UI flush**: renders are batched by dirty flags (`src/flush.js:72-234`) and
  run once per `requestAnimationFrame`. This matters only for ordering: a
  flag set from an event handler is rendered on the next frame, never
  synchronously.

### 0.2 i18n

- `data-i18n="key"` = the element's text is `t(key)`; `data-i18n-title` = its
  tooltip; `data-i18n-html` = innerHTML; `data-help-i18n="help.x"` = the
  element becomes a clickable "inline help" trigger (`src/controls/inline-help.js:85-102`):
  clicking the name toggles a help panel inserted *after* the enclosing
  `.control-row` / `.inline-toggle` (or the `data-help-anchor` selector),
  styled `font-size:11px; color:#d9ecff; background:rgba(120,200,255,0.10);
  border:1px solid rgba(120,200,255,0.30); border-radius:6px`
  (`inline-help.js:36-50`). The trigger gets `cursor:pointer` and a dotted
  underline `underline dotted rgba(217,236,255,0.4)` with offset 2px. Only one
  help panel is open at a time across the whole UI; any outside click closes
  it. When `t(key) === key` (no translation) the trigger is not wired.
- Keys are quoted from `src/i18n/en.json` (943 lines); the English string is
  the fallback text baked in the HTML.
- Some texts are **hardcoded English, not translated**; they are flagged
  "(not i18n)".

### 0.3 Shared widget vocabulary (CSS semantics)

| Widget | Markup | Look / meaning | Source |
|---|---|---|---|
| Section | `.info-section` | column; `margin-top:0.75rem; padding-top:0.5rem; border-top:1px solid rgba(255,255,255,0.12)`; the first section of `#audioPanelRoot` has no top margin | `app.css:1100-1110` |
| Section header | `.panel-header` > `.panel-header-main` (title + optional summary + optional info button) + `.panel-toggle-btn` | flex row space-between | `app.css:312-345`, `ui-primitives.js:1-19` |
| Panel title | `.info-title.panel-title` | `font-weight:600` | `app.css:1281` |
| Collapsed summary | `.panel-summary` | `font-size:11px; color:#9eb4c8; nowrap; ellipsis`; shown only while the section is collapsed | `app.css:339-347` |
| Chevron toggle | `.panel-toggle-btn` | `min-width:1.8rem; height:1.3rem; font-size:11px; bg rgba(255,255,255,0.08); border 1px rgba(255,255,255,0.2); radius 6px; color #d9ecff`; text `▸` when collapsed, `▾` when open | `app.css:1318-1340`, `modals.js:194-340` |
| Collapsible body | `.conditional-params` (+`.open`) | closed: `max-height:0; opacity:0; pointer-events:none; overflow:hidden`; open: `max-height:none; opacity:1; gap:0.2rem` | `app.css:1383-1397` |
| Info button | `.info-icon-btn` | 18×18 px circle, text "i", `font-size:11px`; `[aria-pressed="true"]` / `.is-active` turns green (`bg rgba(82,226,162,0.28); border rgba(82,226,162,0.6); color #eaffe4`) | `app.css:1294-1316` |
| Switch (boolean) | `input[type=checkbox]` inside `.switch-row`, `.inline-toggle`, `.control-row` or `.renderer-subpanel-actions` | rendered as a 34×18 px pill toggle: track `rgba(255,255,255,0.18)`, knob 12 px `#d9ecff`, checked track `rgba(80,200,120,0.45)`, knob translates 16 px. **Per project rule, egui must use switches, never checkboxes.** | `app.css:1055-1098` |
| Control row | `.control-row` | grid `1fr auto auto auto`, gap 0.35rem, `margin-top:0.2rem` (inline `grid-template-columns` overrides are quoted where used) | `app.css:1722-1729` |
| Switch row | `.switch-row` | flex space-between, gap 0.5rem, `margin-top:0.25rem` | `app.css:943-949` |
| Inline toggle | `.inline-toggle` | flex space-between, gap 0.5rem, `margin-top:0.2rem` | `app.css:982-989` |
| Text/number field | `.delay-input` | `width:52px; bg rgba(255,255,255,0.08); border 1px rgba(255,255,255,0.2); radius 6px; color #dfe8f3; font-size:11px; padding 0.1rem 0.3rem; text-align:right`; focus border `rgba(255,255,255,0.45)` | `app.css:1752-1766` |
| Toggle button | `.toggle-btn` | `bg rgba(255,255,255,0.08); border 1px rgba(255,255,255,0.2); color #dfe8f3; radius 6px; font-size:11px; padding 0.1rem 0.35rem`; `.active` → `bg rgba(255,255,255,0.2); color #fff` | `app.css:2058-2071` |
| UI button | `.ui-btn` (+`.ui-btn-primary`, `.ui-btn-compact`) | `bg rgba(255,255,255,0.08); border 1px rgba(255,255,255,0.2); color #cfd9e8; radius 6px; font-size 12px; padding 0.15rem 0.6rem`; primary: `bg rgba(255,255,255,0.1); border rgba(255,255,255,0.25); color #d9ecff`; compact padding `0.1rem 0.45rem`. All `.ui-btn`/`.toggle-btn` have `user-select:none` (first-click rule) | `app.css:451-510`, `ui-primitives.js:21-34` |
| Slider | `input[type=range].gain-slider` | native range, `width:100%` | `app.css:2054` |
| Value box | `.gain-box` | `min-width:54px; text-align:right; bg rgba(255,255,255,0.08); font-size 11px; color #d9ecff; radius 6px` | `app.css:2089-2097` |
| Meter bar | `.meter-bar` > `.meter-fill` (+`.meter-peak`, `.meter-marker`, `.meter-range-mask`) | 6 px high pill; track is a faint 4-stop gradient (`rgba(77,215,255,0.18)` 0% → `rgba(123,255,106,0.18)` 60% → `rgba(255,209,58,0.18)` 82% → `rgba(255,93,93,0.18)` 100%); fill is the same gradient at full alpha (`#4dd7ff → #7bff6a 60% → #ffd13a 82% → #ff5d5d 100%`) clipped with `clip-path: inset(0 calc(100% - var(--level)) 0 0)` | `app.css:1553-1560`, `1670-1678` |
| Level meter | `.meter-bar.level-meter` | adds a fixed red "headroom" zone from `left:90.9%` to the right edge (`rgba(255,80,80,0.32)` with a 1 px `rgba(255,230,230,0.75)` left border); `.meter-peak` is a 2 px slice at `--level` (`clip-path: inset(0 calc(100% - var(--level) - 1px) 0 var(--level))`, transition 0.1 s); `.meter-peak.over` = solid `#ff3b3b` with glow | `app.css:1679-1720` |
| Marker | `.meter-marker` | absolute 2 px wide, `top:-1px;bottom:-1px`, opacity 0.95; `.min` = `rgba(255,160,90,0.95)`, `.max` = `rgba(255,213,106,0.95)` | `app.css:1625-1643` |
| Range mask | `.meter-range-mask` | `rgba(6,9,14,0.42)` overlay used to dim the part of a bar outside the min..max window | `app.css:1561-1567` |
| Fixed metric | `.fixed-metric` | monospace, `width:8ch`, tabular nums, right aligned | `app.css:1540-1547` |
| Sub-values | `.meter-subvalues` | centred flex, gap 0.7rem, 10 px monospace `#b9c7d8` | `app.css:1605-1619` |

Meter scale (all level meters): `METER_DB_MIN = -60`, `METER_DB_MAX = +6`
dBFS; `dbToMeterPercent(db) = clamp(((db+60)/66)*100, 0, 100)`
(`src/mute-solo.js:52-59`). 0 dBFS = 90.909 %. Percentages are written with
`toFixed(1)`.

### 0.4 Global enable/visibility rules that apply to every control in the right overlay

1. **Runtime lock** (`src/runtime-connection.js`): while
   `app.oscStatusState !== 'connected'`, *every* `button`, `input`, `select`,
   `textarea` under `#overlay` and `#speakersOverlay` is `disabled`, except
   the ids in `EXEMPT_CONTROL_IDS` (OSC config controls, the section
   toggles `inputSectionToggleBtn`, `roomGeometryToggleBtn`,
   `displaySectionToggleBtn`, `audioOutputSectionToggleBtn`,
   `telemetryGaugesToggleBtn`, `rendererSectionToggleBtn`, the two panel
   collapse buttons `leftPanelCollapseBtn`/`rightPanelCollapseBtn`, and the
   Renderer/Binaural tab buttons). The previous `disabled` state is
   remembered and restored on reconnect (`runtime-connection.js:67-96`).
   The listed section toggles are themselves disabled while not connected
   (`:69-74`). Note that `autoGainSectionToggleBtn`, `diagPlotToggleBtn`,
   `resamplePlotToggleBtn`, the speaker editor tabs and the layout buttons
   are NOT exempt, i.e. they lock too.
2. **Producer capability classes** on `<body>` (`src/init.js:76-91`):
   - `cap-no-audio` when the handshake has arrived and the producer has no
     `audio` domain (embedded liborender in mpv). CSS `app.css:2702-2706`
     hides every child of `#audioOutputSectionContent` except
     `#outputChannelMappingRow` and `#outputChannelMappingWarning`.
   - `cap-no-resampler` when `controlConfig` lacks `adaptive_resampling`:
     `#latencySection` is hidden wholesale (`app.css:2711-2713`).
   - `cap-embedded` (embedded producer AND connected): hides
     `.osc-config-actions` (left overlay) and *shows* `.output-mode-mpv-note`
     in the renderer panel (`app.css:2757-2759`).
   Before the handshake nothing is hidden.
3. **Speaker-layout freeze**: `app.renderBackendState.frozenSpeakers`
   (`AppState.render_backend_state.frozen_speakers`) disables every
   layout-editing control (see §4). `frozenRoomRatio` is for the left
   overlay.
4. **Output mode classes** (`src/controls/binaural.js:423-451`):
   `body.output-binaural` when `binaural.outputMode === 'binaural'`,
   `body.output-cascaded` when additionally `binaural.mode === 'cascaded'`.
   They gate the Speakers section headers/lists (§4.1).
5. **Editing tabs**: `body.studio-tab-binaural` (renderer panel Renderer /
   Binaural tabs, §3.2) and `body.speaker-tab-test` (speaker editor Edit /
   Test tabs, §4.6).

### 0.5 Numeric formatting helpers

- `formatNumber(v, d)` = `v.toFixed(d)` or `'—'` for non-numbers
  (`src/coordinates.js:14-19`).
- `formatLinearAsDb(x)` = `${(20·log10 x).toFixed(1)} dB`, or `'-∞ dB'` for
  x ≤ 0 (`mute-solo.js:72-78`).
- `linearToDb(x, floor=-100)` (`src/audio-math.js:20-26`).

---

## 1. The right overlay container (`#speakersOverlay`)

`index.html:775-950`; CSS `app.css:571-620`, `215-240`, `55-160`.

- `position:fixed; top:1rem; right:1rem; width:var(--panel-width-right);
  height:calc(100vh - 2rem)`; `bg rgba(0,0,0,0.65)` with `backdrop-filter:
  blur(8px)`; `border:1px solid rgba(255,255,255,0.2); border-radius:12px;
  padding:0.75rem 1rem; font-size:14px; line-height:1.45; z-index:2;
  display:flex; flex-direction:column; gap:0.4rem`.
- Children, in order:
  1. `#speakersOverlayScroll` — `flex:1 1 auto; min-height:0; overflow-y:auto;
     overflow-x:hidden; scrollbar-gutter:stable both-edges; scrollbar-width:thin`;
     column with gap 0.4rem; every direct child is `flex:0 0 auto; width:100%`.
     It contains `#audioPanelRoot`, `#rendererPanelRoot`, `#speakersSection`.
  2. `#speakerEditSection` — pinned *below* the scroll area (a sibling, not
     inside it) so it stays visible while the list scrolls; bounded height
     with its own internal scroll (`app.css:231-245`, comment).
  3. The resize handle and collapse button are appended at runtime (§8).
- **Rule of the project** (CLAUDE.md): expanding any section must never
  change the 3D viewport size; the overlay is fixed-size and the sections
  scroll inside it. See §8.3.

---

## 2. Audio panel (`#audioPanelRoot`)

Markup `src/ui/audio-panel.js:3-322`; mounted by `mountAudioPanel()`
(`:324-330`) which replaces `#audioPanelMount` (`index.html:777`).
Listeners `src/listeners/audio-panel-listeners.js`. Renders:
`src/controls/audio.js` (output section), `src/controls/latency.js`
(latency + resample meters), `src/controls/adaptive.js` (latency controls
form), `src/controls/master.js` (master, auto-gain), `src/controls/resample-plot.js`
and `src/controls/diag-plot.js` (plots, entry points only).

Sections in order: 2.1 Audio Output, 2.2 Latency, 2.3 Diagnostics, 2.4 Master.

### 2.1 Audio Output section (`#audioOutputSection`, `audio-panel.js:6-70`)

**Header** (`panelHeader`, `ui-primitives.js:1-19`): title
`data-i18n="section.audioOutput"` ("Audio Output"); summary
`#audioOutputSummary` (`.panel-summary`, hidden while open); toggle
`#audioOutputSectionToggleBtn`.

Collapse state: `app.audioOutputSectionOpen` (default `false`;
`app.js:248` calls `setAudioOutputSectionOpen(false)` at boot).
`setAudioOutputSectionOpen(open)` (`modals.js:266-285`): toggles
`.open` on `#audioOutputSectionContent`, shows the summary only when
collapsed, sets the chevron text, toggles `.section-collapsed` on the
section, and emits `omniphony:overlay-layout-changed` with reason
`'audio-output-section-toggle'`. Listener: `modal-and-toggle-listeners.js:293-297`.
The state is **not persisted**.

**Summary text** (`audio.js:199-223`, re-rendered on every `dirty.audioFormat` flush):
- If the producer has no `audio` domain (`hasProducerDomain('audio')` false):
  `${t('audio.channelMapping')}: ${t(mappingKey)}` where mappingKey is
  `audio.channelMapping.byName` if `getLiveOption('output_channel_mapping') === 'by_name'`
  else `audio.channelMapping.byIndex`.
- Otherwise `tf('audio.summary', {device, rate, format})` =
  `"{device} • {rate} • {format}"` where
  `device` = label of `app.audioOutputDeviceEffective || app.audioOutputDevice`
  looked up in `app.audioOutputDevices` (fallback: the raw value), or
  `t('status.defaultOutputDevice')` ("Default") when empty and
  `app.oscSnapshotReady`, or `'—'` before the snapshot;
  `rate` = `${app.audioSampleRate} Hz` or `'—'`;
  `format` = `app.audioSampleFormat || '—'`.
  If `app.audioError` is set: `summary + ' • Error: ' + app.audioError` (not i18n).

**Body** (`#audioOutputSectionContent.conditional-params`), rows in order:

| # | Element | Type | Label (`data-i18n` / help) | Value source | On change | Visible / enabled |
|---|---|---|---|---|---|---|
| 1 | `#audioFormatInfo` | text line | — (format `status.audioFormat` = `"audio: {rate} / {format}"`, plus `" • Error: {audioError}"` when set) | `app.audioSampleRate` (`AppState.audio.audio_sample_rate`), `app.audioSampleFormat`, `app.audioError` | read-only | always |
| 2 | `#audioOutputBackendSelect` | select, options `device` (`audio.backendDevice` "Device"), `file` (`audio.backendFile` "File / Stream") | `audio.outputBackend` "Output" / `help.audio.outputBackend`; row grid `auto minmax(0,1fr)`; label `font-size:12px` | `app.audioOutputBackend` (`AppState.audio.audio_output_backend`, default `'device'`; rendered `file` iff value `=== 'file'`) | `applyAudioOutputBackendNow()` (`audio.js:320-328`): optimistic write, `invoke('control_audio_output_backend', {backend})` → OSC `/omniphony/control/audio/output_backend` string (`commands/audio.rs:91-99`). Deliberately NOT part of the batch audio config. | disabled when `!app.oscSnapshotReady \|\| !hasAudioDomain`; row hidden by `cap-no-audio` |
| 3 | `#audioOutputDeviceRow` → `#audioOutputDeviceSelect` + `#refreshOutputDevicesBtn` | select + compact secondary button "↺" (`title` key `audio.refreshDevices` "Refresh device list") | `audio.outputDevice` "Output device" / `help.audio.outputDevice` | options = `[{value:'', label: oscSnapshotReady ? t('status.defaultOutputDevice') : '—'}, ...app.audioOutputDevices]` (`AppState.audio.audio_output_devices: [{value,label}]`), plus the current `app.audioOutputDevice` appended if not in the list. Selected = `app.audioOutputDevice || ''` unless `app.audioOutputDeviceEditing` (then the DOM value is kept) (`audio.js:117-137`) | `change` → `applyAudioOutputDeviceNow()` (`audio.js:311-318`): `app.audioOutputDevice = value \|\| null`, `sendAudioConfig()` (§2.1.1). `focus` sets `audioOutputDeviceEditing=true`; any pointerdown elsewhere clears it. Refresh button → `invoke('refresh_output_devices')` → OSC `/omniphony/control/audio/output_devices/refresh` (no args) | row hidden when backend is `file`; select disabled when `!oscSnapshotReady \|\| !hasAudioDomain` |
| 4 | `#outputChannelMappingRow` → two `.toggle-btn` `#outputChannelMappingByIndex` (`data-option-value="by_index"`, `audio.channelMapping.byIndex` "By index", baked `.active`) and `#outputChannelMappingByName` (`by_name`, "By name") | toggle-button pair bound by `data-option="output_channel_mapping"` | `audio.channelMapping` "Channel mapping" / `help.audio.channelMapping` | `getLiveOption('output_channel_mapping')` (= `app.options.output_channel_mapping`, else the `optionsSchema` default; `state.js:638-643`). `.active` on the button whose `data-option-value` equals the value (`options-binder.js:83-100`) | click → `setOption(key, value)` (`options-binder.js:36-48`): optimistic `app.options[key]=value`, `invoke('control_option',{key,value})` → OSC `/omniphony/control/option [String key, String value]` (`commands/engine.rs:70-91`; bools would be int 0/1, numbers float), then `dirty.audioFormat` | always visible (even under `cap-no-audio`) |
| 5 | `#outputChannelMappingWarning` | warning text `font-size:11px; color:#ffb24d` | `t('audio.channelMapping.warning')` ("These speakers can't be placed by name:") + `' '` + names joined by `', '` | `app.outputChannelMappingUnroutable` (`AppState.live_options.output_channel_mapping_unroutable`) | read-only | shown only when mapping is `by_name` AND the list is non-empty (`audio.js:745-759`) |
| 6 | `#audioOutputPipeRow` → `#audioOutputPipeToggle` | switch (`.switch-row`) | `audio.namedPipe` "Named pipe" / `help.audio.namedPipe` (anchor `.switch-row`) | checked = `isFileBackend && app.audioOutputFile !== '-'` (`audio.js:141-142`) | `applyAudioOutputNamedPipeNow()` (`audio.js:332-349`): ON → `app.audioOutputFile = app.audioOutputPipePath` (remembered path, may be empty) and, if non-empty, `invoke('control_audio_output_file',{path})`; OFF → remember the current path into `app.audioOutputPipePath`, set `app.audioOutputFile='-'`, `invoke('control_audio_output_file',{path:'-'})` → OSC `/omniphony/control/audio/output_file` string | row shown only when backend is `file`; disabled when `!oscSnapshotReady \|\| !hasAudioDomain` |
| 7 | `#audioOutputFileRow` → `#audioOutputFileInput` | text field, placeholder `/path/to/fifo` | `audio.outputFile` "Destination" / `help.audio.outputFile` | `app.audioOutputFile === '-' ? '' : app.audioOutputFile` unless `app.audioOutputFileEditing` | `change` or Enter → `applyAudioOutputFileNow()` (`audio.js:351-365`): trimmed; empty → `app.audioOutputFile=''` and nothing sent; else `app.audioOutputFile = app.audioOutputPipePath = value`, `invoke('control_audio_output_file',{path})`. `focus` sets editing flag + selects all; pointerdown elsewhere clears editing | row shown only when named pipe is on (`useNamedPipe`) |
| 8 | `#audioOutputFileFormatRow` → `#audioOutputFileFormatSelect` | select, options `raw_f32` (`audio.formatRawF32` "Raw f32 (LE)"), `caf` (`audio.formatCaf` "CAF (float)") | `audio.outputFileFormat` "Format" / `help.audio.outputFileFormat` | `app.audioOutputFileFormat` if in `['raw_f32','caf']` else `raw_f32` | `applyAudioOutputFileFormatNow()` (`audio.js:367-373`): optimistic, `invoke('control_audio_output_file_format',{format})` → OSC `/omniphony/control/audio/output_file_format` string | row shown only when backend is `file` |
| 9 | `#audioSampleRateInput` + `#audioSampleRateMenuBtn` ("▾", compact) + `#audioSampleRateMenu` | text field (`inputmode=numeric`, baked value `0`) with a dropdown of presets | `audio.sampleRate` "Sample rate" / `help.audio.sampleRate`; row grid `auto 1fr` | `String(app.audioSampleRate || 0)` unless `app.audioSampleRateEditing` | `change` on the field → `applyAudioSampleRateNow()` (`audio.js:301-309`): `requested = max(0, round(Number(value) || 0))`; `app.audioSampleRate = requested > 0 ? requested : null`; `sendAudioConfig()`; closes the menu. Menu button toggles the menu (`audio.js:267-294`): one button per preset in `AUDIO_SAMPLE_RATE_PRESETS = [0, 32000, 44100, 48000, 88200, 96000, 176400, 192000]` (`state.js:631`), labelled `t('status.nativeRate')` ("Native (0)") for 0 else `${rate} Hz`; item style `display:block; text-align:left; color:#d9ecff; padding 0.25rem 0.35rem; font-size 12px`, hover `rgba(255,255,255,0.12)`; clicking an item writes the value in the field and applies. Menu container: absolute below the field, `bg rgba(10,11,16,0.96); border 1px rgba(255,255,255,0.2); radius 8px; max-height 180px; overflow auto`. Outside pointerdown closes it | disabled when `!oscSnapshotReady \|\| !hasAudioDomain` |

Additional side effects of the audio-format render (`audio.js:104-226`):
it also reflects the ramp-mode select of the renderer panel (§3.4), calls
`reflectBoundOptions()` (all `data-option` controls), refreshes several
left-overlay panels (fixed-channel sources, virtual bed, channel editor —
out of this document's scope) and `renderCrossoverInfoDisplay()` (§3.5).

#### 2.1.1 The audio config apply flow (`sendAudioConfig`)

`src/controls/audio.js:35-102`. Several controls (output device, sample
rate, latency target, every adaptive-resampling field) share one batched
message:

1. `buildAudioConfigPayload()` assembles `{ outputDevice, sampleRate,
   latencyTargetMs: app.latencyRequestedMs || app.latencyTargetMs || null,
   adaptiveResampling: { enabled, enableFarMode, forceSilenceInFarMode,
   hardRecoverHighInFarMode, hardRecoverLowInFarMode, farModeReturnFadeInMs,
   kpNear, ki, integralDischargeRatio, maxAdjust, highRecoverEntryMarginMs,
   updateIntervalCallbacks, lowRecoverSettleStableMs, lowRecoverEntryMarginMs,
   lowRecoverExitMarginMs, lowRecoverSettleMarginMs, lowRecoverRefillDeltaAlpha,
   controlSmoothingCutoffHz, controlSmoothingOrder, paused, usePreBridgeClock,
   useOutputPacing, disableBackpressure } }` from the `app.adaptiveResampling*`
   mirrors (raw, unclamped).
2. `invoke('control_audio_config', {payload})` (`commands/audio.rs:22-67`):
   the host parses it into `audio_config::AudioConfig`, calls `.resolve()`
   (`src-tauri/src/audio_config.rs:138-234`) and sends the *effective*
   config as a JSON **string** on OSC `/omniphony/control/config/audio`.
   Resolution rules (integers stay integers on the wire —
   `audio_config.rs:96-103`):
   - `outputDevice`: trimmed; empty → absent.
   - `sampleRate`, `latencyTargetMs`: finite and > 0 else absent; rounded to u32.
   - `farModeReturnFadeInMs`: round, ≥ 0 (default 0).
   - `kpNear`, `ki` default 1.0 (unbounded); `integralDischargeRatio` default 0.25 (unbounded);
     `maxAdjust` default 0.01 (unbounded).
   - `highRecoverEntryMarginMs`: round, ≥ 1 (default 1000); `updateIntervalCallbacks`: round, ≥ 1 (default 1).
   - `lowRecoverSettleStableMs` ≥ 0 (default 200); `lowRecoverEntryMarginMs` ≥ 0 (18);
     `lowRecoverExitMarginMs` ≥ 0 (6); `lowRecoverSettleMarginMs` ≥ 0 (6);
     `lowRecoverRefillDeltaAlpha` clamp 0..1 (0.5); `controlSmoothingCutoffHz` ≥ 0.001 (0.5);
     `controlSmoothingOrder` round then clamp 1..2 (1).
   - Non-finite numbers fall back to the default.
   The command returns the effective config; `applyEffectiveAudioConfig`
   (`audio.js:69-93`) copies the 14 numeric fields back into `app` and sets
   `dirty.adaptiveResampling`, so a corrected value is displayed.
3. Unless `apply:false`, `invoke('control_audio_config_apply')` → OSC
   `/omniphony/control/config/audio/apply` (no args). Every caller in this
   document uses the default (`apply:true`).

The rejection path: if the host cannot parse the payload it logs to stderr
and returns `None`; the JS still sends the apply (see comment
`commands/audio.rs:33-37`).

There is no "staged vs applied" indicator in the UI for this flow; the only
dirty state is the per-field dirty flags of the adaptive form (§2.2.5).

### 2.2 Latency section (`#latencySection`, `audio-panel.js:71-282`)

Hidden wholesale under `body.cap-no-resampler`. Not collapsible as a
section; instead the *controls form* below the meters collapses
(`#telemetryGaugesForm`).

#### 2.2.1 Header grid

A 2-column grid (`auto minmax(0,1fr)`, 3 rows) plus a right-hand button
cluster:

- Row 1 col 1: title `.info-title` `data-i18n="section.latency"` ("Latency").
  It is turned into the clickable trigger of the *Latency Gauges* info modal
  (`modal-and-toggle-listeners.js:140-147`, `triggerSelector:
  '#latencySection .info-title'`), styled with the dotted underline; the
  `#telemetryGaugesInfoBtn` is then hidden (`display:none`).
- Row 1 col 2: the **latency meter** `.meter-bar` (overflow visible) containing:
  - `#latencyMeterFill.meter-fill.latency` — gradient `#52e2a2 0% → #ffd56a 60% → #ff8a5c 82% → #ff4d4d 100%`, glow `rgba(255,160,90,0.35)` (`app.css:933-936`).
  - `#latencyRawMinMask` (`.meter-range-mask`, anchored left) and `#latencyRawMaxMask` (anchored right).
  - four 5×5 px round dots above the bar (`top:-11px`): `#latencyTargetMarker` `#52e2a2`, `#latencyNearLowMarker` `#4ad6ff`, `#latencyLowExitMarker` `#c08bff`, `#latencyNearHighMarker` `#ffb84a`; all initially hidden.
  - `#latencyRawMinMarker.meter-marker.min`, `#latencyCtrlMarker` (`#58a0ff`, `top:-4px;bottom:-4px`), `#latencySmoothedMarker` (`#c879ff`, same extent, hidden), `#latencyRawMaxMarker.meter-marker.max`.
- Row 2 col 2: two `.meter-subvalues` lines:
  - `#latencyRawMinValue` | `#latencyRawInfo` | `#latencyRawMaxValue`
  - `#latencyCtrlInfo` | `#latencySmoothedInfo` | `#latencyDownstreamInfo`
  (separators are literal `|` spans at opacity 0.45).
- Row 3 col 1: `#resampleMeterLabel.meter-mini-label` `data-i18n="telemetry.resample"` ("Resample").
- Row 3 col 2: `#resampleMeterBody` (grid, gap 0.05rem): the **resample meter**
  `.meter-bar.resample-meter-shell` (symmetric red→amber→cyan→amber→red track,
  `app.css:1569-1579`) with `.resample-meter-center` (2 px `rgba(217,236,255,0.6)` at 50 %),
  `#resampleNegMeterFill.meter-fill.resample-neg` (gradient `#ffd56a → #ff8a5c`),
  `#resamplePosMeterFill.meter-fill.resample-pos` (`#8af0ff → #7bffb8`),
  `#resampleNegNearMarker` / `#resamplePosNearMarker` (`.meter-marker.min`, `#ffd54a`);
  then `#resampleRatioInfo` (10 px monospace `#b9c7d8`, centred, default `—`).
- Right cluster (flex, gap 0.35rem):
  - `#resamplePlotToggleBtn.info-icon-btn` — SVG polyline `2,12 5,8 8,10 11,4 14,7` in a 16×16 viewBox (a small line chart), `title` key `telemetry.plotToggle` ("Toggle resample / latency plot"), `aria-pressed`.
  - `#telemetryGaugesInfoBtn.info-icon-btn` "i", title `telemetry.infoButton` (hidden at runtime, see above).
  - `#telemetryGaugesToggleBtn.panel-toggle-btn` "▸", title `telemetry.toggle` ("Show latency controls").

#### 2.2.2 Latency readouts (`renderLatencyDisplay`, `latency.js:132-170`)

Rendered on `dirty.latency`. Values (`app.*` ← events):

| Element | Text | Source |
|---|---|---|
| `#latencyRawInfo` | `formatNumber(latencyInstantMs, 0) + ' ms'` or `—` | `app.latencyInstantMs` ← event `latency:instant` (`tauri-bridge.js:703`), snapshot `latencyInstantMs` |
| `#latencyCtrlInfo` | `ctrl {n} ms` / `ctrl —` (0 decimals) | `app.latencyControlMs` ← `latency:control` |
| `#latencySmoothedInfo` | `smoothed {n} ms` (2 decimals) / `smoothed —` | `app.latencySmoothedMs` ← `latency:smoothed` |
| `#latencyDownstreamInfo` | `path {n} ms` (0 decimals) / `path —` | `app.latencyDownstreamMs` ← `latency:downstream` |
| `#latencyRawMinValue` | `tf('status.minValue',{value})` = `min {value}` (0 decimals or `—`) | `app.timingStats.latency.min` ← `latency:stats` |
| `#latencyRawMaxValue` | `max {value}` | `app.timingStats.latency.max` |

The prefixes `ctrl`, `smoothed`, `path` are hardcoded (not i18n); the
`min`/`max` come from `status.minValue`/`status.maxValue`.

Other latency-related events: `latency` (`app.latencyMs`), `latency:target`
(`app.latencyTargetMs`), `latency:requested` (`app.latencyRequestedMs`; also
seeds `latencyTargetMs` and `latencyMs` when they are still null)
(`tauri-bridge.js:697-798`). The host rounds every one of these to integer
ms except `smoothed` (`app_state.rs:687-741`).

#### 2.2.3 Latency meter geometry (`renderLatencyMeterUI`, `latency.js:262-406`)

- Scale: `maxMs = target === null ? 2000 : max(100, 2 × target)` where
  `target = app.latencyRequestedMs ?? app.latencyTargetMs ?? app.latencyMs`.
- Fill `--level` = `min(100, max(0, raw)/maxMs × 100)` with
  `raw = app.latencyInstantMs ?? app.latencyTargetMs ?? app.latencyMs` (0 % if null).
- Min/max masks: from `app.timingStats.latency` (`{min,max,mean,count}` or
  null). Left mask width = `min/maxMs %`; right mask width = `100 − max/maxMs %`.
  Both hidden when either is null or `max < min`.
- `#latencyRawMinMarker` left = `calc(min% − 1px)`, `#latencyRawMaxMarker` at
  `max%`; hidden when null.
- `#latencyCtrlMarker` at `(app.latencyControlMs ?? latencyTargetMs ?? latencyMs)`.
- `#latencySmoothedMarker` at `app.latencySmoothedMs` (hidden when null).
- `#latencyTargetMarker` at `(app.latencyTargetMs ?? app.latencyMs)`, offset −2px.
- Threshold dots relative to the same target: near-low at `target − lowRecoverEntryMarginMs`,
  low-exit at `target − lowRecoverExitMarginMs`, near-high at `target + highRecoverEntryMarginMs`;
  each clamped to `[0, maxMs]`, hidden when the margin is not finite or the target is null.
  Their opacity is `1` when far mode is enabled (`app.adaptiveResamplingEnableFarMode === true`)
  else `0.28`.

**Backend windows** (`src-tauri/src/osc_listener.rs:1799-1830, 1892-1916`
and `timing_stats.rs`): `latency:stats` is emitted at 4 Hz with
`{ latency: {min,max,mean,count}|null, decode|render|crossover|write:
{ avg: stats(1000 ms)|null, max: stats(5000 ms)|null } }`. Latency window
span `LATENCY_RAW_WINDOW_MS = 4000`, stage max window
`RENDER_TIME_WINDOW_MS = 5000`, stage mean window
`RENDER_TIME_AVERAGE_WINDOW_MS = 1000`. `TimeWindow` is a 200-bucket ring
(`timing_stats.rs:28`); the egui crate already ships a copy at
`src/host/timing_stats.rs`.

#### 2.2.4 Resample meter (`renderResampleRatioDisplay`, `latency.js:179-253`)

- Entire row (label, body, ppm text) hidden when `app.adaptiveResamplingEnabled !== true`.
- `app.resampleRatio` (← event `resample_ratio`, snapshot `resampleRatio`):
  null → text `—`, both fills collapsed (`clip-path: inset(0 50% 0 50%)`), markers hidden.
- Text: `ppm = round((ratio − 1) × 1e6)`; `${ppm >= 0 ? '+' : ''}${ppm} ppm`.
- `farBound = max(1e-6, app.adaptiveResamplingMaxAdjust || 1e-6)`;
  `nearBound = clamp(maxAdjust, 0, farBound)` (in practice equal to farBound);
  `magnitude = min(1, |ratio − 1| / farBound) × 50` (percent of half the bar).
- Negative deviation: `#resampleNegMeterFill` clip `inset(0 50% 0 (50 − magnitude)%)`
  (grows leftwards from the centre); positive: `#resamplePosMeterFill` clip
  `inset(0 (50 − magnitude)% 0 50%)`.
- Near markers at `50 ± nearBound/farBound × 50 %` (i.e. the bar ends when
  nearBound = farBound), each `left: calc(p% − 1px)`. (Far markers
  `#resampleNegFarMarker`/`#resamplePosFarMarker` are referenced by the code
  but no longer exist in the markup — dead code.)

#### 2.2.5 Latency controls form (`#telemetryGaugesForm.telemetry-gauges-form`)

Collapsed by default: `app.telemetryGaugesOpen` (default `false`,
`app.js:247`), toggled by `#telemetryGaugesToggleBtn` →
`setTelemetryGaugesOpen(!open)` (`modals.js:194-205`): toggles `.open` on
the form, chevron `▾`/`▸`, emits overlay-layout-changed
(`'telemetry-gauges-toggle'`). CSS: closed = `max-height:0; opacity:0;
pointer-events:none`; open = `margin-top:0.35rem; max-height:2000px`
(`app.css:1186-1207`). Form chrome: `padding 0.45rem 0.5rem; bg
rgba(255,255,255,0.05); border 1px rgba(255,255,255,0.12); radius 8px`.

Between the header and the form sits `#resamplePlotContainer`
(`display:none` by default) holding `#resamplePlotCanvas` 600×140 (§2.2.7).

**Row A — target latency + band indicator** (`.control-row`, grid `auto auto 1fr`):

| Element | Type | Label / help | Value | On change | Enabled |
|---|---|---|---|---|---|
| `#latencyTargetInput` | number, `min=1 step=1`, baked `500`, width 5.5rem | `audio.targetLatency` "Target latency" / `help.audio.targetLatency` | `max(1, round(app.latencyRequestedMs ?? app.latencyTargetMs ?? app.latencyMs))`, or `''` if null; not overwritten while `app.latencyTargetEditing` or `app.latencyTargetDirty` (`latency.js:161-163`) | `focus` → editing=true + select all; `input` → `latencyTargetDirty=true`; Enter → `applyLatencyTargetNow()` (`latency.js:583-593`): `requested = max(1, round(Number(value) \|\| 0))`; writes `app.latencyRequestedMs = app.latencyTargetMs = requested`; clears dirty/editing; `invoke('control_latency_target',{value})` → OSC `/omniphony/control/latency_target` int ≥ 1 (`commands/resampling.rs:93-101`). Note this command is separate from the batch config, but the batch (§2.1.1) also carries `latencyTargetMs` | runtime lock only |
| `#latencyTargetApplyBtn` | `.ui-btn.ui-btn-primary`, text `adaptive.apply` ("Apply") | — | — | click → `applyLatencyTargetNow()` | `disabled = !app.latencyTargetDirty`; opacity `0.4` / cursor `default` when disabled |
| green dot | 0.38 rem circle `#52e2a2` with `box-shadow 0 0 0 1px rgba(255,255,255,0.14)`, title `telemetry.targetMarkerTitle` | legend for the target marker | — | cosmetic | — |
| `#adaptiveBandIndicator` | `#adaptiveRuntimeStateText` (10 px uppercase, letter-spacing 0.04em, `#8fa6bd`, min-width 7.5em, right aligned) + `#adaptiveBandDot` (0.6 rem circle) + `#adaptiveBandText` (12 px `#d9ecff`) | — | state text = `app.adaptiveResamplingState ?? '—'` (← event `adaptive_resampling:state`, snapshot `adaptiveResamplingState`); band text = `app.adaptiveResamplingBand ?? '—'` (← `adaptive_resampling:band`); dot colour: `hard` → `#ff4d4d`, `far` → `#ff9a5c`, `near` → `#52e2a2`, else `rgba(255,255,255,0.25)` (`adaptive.js:299-315`) | read-only | — |
| `#adaptiveResamplingInfoBtn` | `.info-icon-btn` "i", title `adaptive.infoButton` ("Latency controls info") | opens `#adaptiveResamplingInfoModal` (title `adaptive.infoTitle`, body `adaptive.infoBody`, close `common.close`). Stays as a visible button (no `.title-with-info`/`.panel-header` ancestor to promote) | — | click | — |

**Row B — `#adaptiveResamplingAdvancedForm.adaptive-advanced-form`** (always
visible inside the open form; column, gap 0.45rem). Three
`.adaptive-subpanel`s (`padding 0.45rem 0.5rem; bg rgba(255,255,255,0.04);
border 1px rgba(255,255,255,0.1); radius 8px`), each starting with a 10 px
uppercase caption (`letter-spacing:0.08em; color:#8fa6bd`), then an
actions row `Cancel` / `Apply`.

Shared rules for the numeric fields below (`audio-panel-listeners.js:169-461`,
`adaptive.js:61-357`):
- `focus` → `app.<field>Editing = true` and select all; `input` →
  `Editing = true`, `Dirty = true`, re-render. The select
  (`controlSmoothingOrder`) only sets Dirty on `change`.
- While a field is Editing or Dirty its value is not overwritten by state.
- A row has class `.adaptive-param-disabled` (opacity 0.42; inputs 0.72)
  when the *gate* in the table is false; the input is `disabled` as well.
- **Apply** (`#adaptiveResamplingAdvancedApplyBtn`, `.ui-btn.ui-btn-primary`,
  text `adaptive.apply`) and **Cancel** (`#adaptiveResamplingAdvancedCancelBtn`,
  `.ui-btn`, text `common.cancel`) are enabled iff any of the 14 dirty flags
  is set (`adaptiveDirty`, `adaptive.js:332-356`); disabled → opacity 0.45,
  cursor default.
- Apply (`audio-panel-listeners.js:193-244`) reads every field from the DOM,
  clamps client-side as listed, writes all of them into `app`, calls
  `sendAudioConfig()`, then `resetAdaptiveResamplingAdvancedDirtyState()`
  (all Dirty/Editing false) and re-renders. Cancel only resets the flags
  (fields are re-filled from `app` on the next render).
- Special clamp on Apply: `lowRecoverExitMarginMs = min(value, max(0,
  lowRecoverEntryMarginMs − 0.1))` and the corrected value is written back
  into the input (hysteresis must be well-formed).
- The **switches** in these subpanels are NOT part of the dirty/apply cycle:
  each sends immediately (`sendAudioConfig()` after an optimistic write).

`hasAudioDomain` below = `hasProducerDomain('audio')`;
`adaptiveEnabled` = `hasAudioDomain && app.adaptiveResamplingEnabled === true`;
`farModeEnabled` = `hardRecoverHigh || hardRecoverLow || forceSilence` (derived,
`adaptive.js:111-114`; the same derivation is written into
`app.adaptiveResamplingEnableFarMode` by `syncAdaptiveFarModeDerived` whenever
one of the three switches changes, `audio-panel-listeners.js:128-137`).

**Subpanel 1 — `adaptive.globalActions` "Global far actions"**

| Element | Type | Label / help | `app` field (AppState) / default | Displayed as | Gate |
|---|---|---|---|---|---|
| `#adaptiveFarHardRecoverHighToggle` | switch | `adaptive.hardRecoverHigh` "Hard recover high in far mode" / `help.adaptive.hardRecoverHigh` | `adaptiveResamplingHardRecoverHighInFarMode` (`adaptive_resampling_hard_recover_high_in_far_mode`, u8) / JS default `true`, host default `1` | checked | disabled when `!hasAudioDomain`; change → optimistic + `syncAdaptiveFarModeDerived()` + `sendAudioConfig()` |
| `#adaptiveHighRecoverEntryMarginInput` (row `#adaptiveHighRecoverEntryMarginRow`) + unit `ms` + symbol `#adaptiveHighRecoverEntryMarginSymbol` (0.38 rem dot `#ffb84a`, title `telemetry.thresholdMarkerTitle`) | number `min=1 step=1`, baked 120, width 8rem | `adaptive.threshold` "High-recover entry margin" / `help.adaptive.threshold` | `adaptiveResamplingHighRecoverEntryMarginMs` (i64) / JS 120, host 1000 | `String(max(1, round(v)))`; Apply clamp `max(1, round)` | gate `farModeEnabled` (row dimmed + input disabled); symbol opacity `1` / `0.42` |
| `#adaptiveFarHardRecoverLowToggle` | switch | `adaptive.hardRecoverLow` "Hard recover low in far mode" / `help.adaptive.hardRecoverLow` | `adaptiveResamplingHardRecoverLowInFarMode` / JS `false`, host `0` | checked | `!hasAudioDomain`; immediate send |
| `#adaptiveLowRecoverEntryMarginMsInput` (row `#adaptiveLowRecoverEntryMarginMsRow`) + `ms` + symbol `#adaptiveLowRecoverEntryMarginSymbol` (`#4ad6ff`, title `telemetry.lowThresholdMarkerTitle`) | number `min=0 step=0.1`, baked 18, width 7rem | `adaptive.lowRecoverEntryMargin` "Low-recover entry margin" / `help.adaptive.lowRecoverEntryMargin` | `adaptiveResamplingLowRecoverEntryMarginMs` (f64) / 18 | `toFixed(1)`; Apply clamp `max(0, v)` | gate `hasAudioDomain` |
| `#adaptiveLowRecoverExitMarginMsInput` (row `#adaptiveLowRecoverExitMarginMsRow`) + `ms` + symbol `#adaptiveLowRecoverExitMarginSymbol` (`#c08bff`, title `telemetry.lowExitMarkerTitle`) | number `min=0 step=0.1`, baked 6, width 7rem | `adaptive.lowRecoverExitMargin` "Low-recover exit margin" / `help.adaptive.lowRecoverExitMargin` | `adaptiveResamplingLowRecoverExitMarginMs` / 6 | `toFixed(1)`; Apply clamp `min(max(0,v), entry − 0.1)` | gate `hasAudioDomain` |
| `#adaptiveFarSilenceToggle` (row `#adaptiveFarSilenceRow`) | switch | `adaptive.silenceFar` "Silence in Far Mode" / `help.adaptive.silenceFar` | `adaptiveResamplingForceSilenceInFarMode` / JS `false`, host `1` | checked | `!hasAudioDomain`; immediate send |
| `#adaptiveFarFadeInMsInput` (row `#adaptiveFarFadeRow`) | number `min=0 step=1`, baked 0, width 8rem | `adaptive.fadeNearReturn` "Fade-In on Near Return" / `help.adaptive.fadeNearReturn` | `adaptiveResamplingFarModeReturnFadeInMs` (i64) / JS 0, host 500 | `String(max(0, round(v ?? 0)))`; Apply `max(0, round)` | gate `forceSilenceInFarMode === true` (`adaptive.js:133-139`) |

**Subpanel 2 — `adaptive.resamplingController` "Local resampling controller"**

Caption row also carries three `.ui-btn`s on the right (flex-shrink 0):

| Button | Text | Behaviour | Enabled / visible |
|---|---|---|---|
| `#autoTunePiBtn` | `autoTune.openButton` "Auto-tune…", title `autoTune.openButtonTitle` | `openAutoTuneWizard()` (`src/auto-tune/wizard-ui.js`, **wizard: out of scope**, fills `#autoTunePiWizardModal`) | runtime lock |
| `#adaptivePauseBtn` | `⏸ ${t('adaptive.pause')}` or, when paused, `▶ ${t('adaptive.resume')}` | on `pointerup` (not click — the disabled attribute is rewritten every flush and would cancel a click, `audio-panel-listeners.js:254-277`): toggles `app.adaptiveResamplingPaused`, re-renders, `sendAudioConfig()` (the `paused` flag rides the batch) | `disabled = !adaptiveEnabled`; opacity 0.45 when disabled. Paused style: `bg rgba(255,180,0,0.18); border rgba(255,180,0,0.5); color #ffd87a`; else `bg rgba(255,255,255,0.08); border rgba(255,255,255,0.2); color #d9ecff` |
| `#adaptiveRatioResetBtn` (`.adaptive-ratio-reset-btn`, `display:none` by default) | `adaptive.resetRatio` "Reset ratio" | `invoke('control_adaptive_resampling_reset_ratio')` → OSC `/omniphony/control/adaptive_resampling/reset_ratio` int 1 | shown only when `adaptiveEnabled && paused`; disabled when `!adaptiveEnabled` |

`app.adaptiveResamplingPaused` ← event `adaptive_resampling:pause` (`enabled !== 0`) and snapshot `adaptiveResamplingPaused` (u8).

| Element | Type | Label / help | `app` field / default | Displayed as | Gate |
|---|---|---|---|---|---|
| `#adaptiveResamplingToggle` | switch | `adaptive.title` "Adaptive Resampling" / `help.adaptive.title` | `adaptiveResamplingEnabled` (`adaptive_resampling` u8) / `false` | checked | disabled when `!hasAudioDomain`; change → optimistic + `sendAudioConfig()`; also hides/shows the resample meter (§2.2.4) |
| `#adaptiveUpdateIntervalCallbacksInput` (row `#adaptiveUpdateIntervalRow`) | number `min=1 step=1`, baked 10, width 8rem | `adaptive.updateInterval` "Update interval" / `help.adaptive.updateInterval` | `adaptiveResamplingUpdateIntervalCallbacks` / JS 10, host 1 | `String(max(1, round))`; Apply `max(1, round)` | gate `adaptiveEnabled` |
| `#adaptiveMaxAdjustInput` (row `#adaptiveMaxAdjustRow`) + unit `ppm` + a 2×12 px `#ffd54a` bar symbol (title `telemetry.resampleMarkerTitle`) | number `min=0.001 step=1`, baked 10000, width 7rem | `adaptive.max` "Adaptive max" / `help.adaptive.max` | `adaptiveResamplingMaxAdjust` (ratio, f64) / 0.01 | **ppm**: `round(maxAdjust × 1e6)`; Apply: `ppm = max(1, round(v))`, `maxAdjust = max(1e-6, ppm/1e6)` | gate `adaptiveEnabled` |
| `#adaptiveKpNearInput` (row `#adaptiveKpNearRow`) | number `min=0.001 step=0.001`, baked 10, width 8rem | `adaptive.kpNear` "Adaptive KP" / `help.adaptive.kpNear` | `adaptiveResamplingKpNear` / JS 10.0, host 1.0 | `toFixed(3)`; Apply `max(0.01, v)` | gate `adaptiveEnabled` |
| `#adaptiveKiInput` (row `#adaptiveKiRow`) | number `min=0 step=0.001`, baked 50, width 8rem | `adaptive.ki` "Adaptive Ki" / `help.adaptive.ki` | `adaptiveResamplingKi` / JS 50.0, host 1.0 | `toFixed(3)`; Apply `max(0, v)` | gate `adaptiveEnabled` |
| `#adaptiveIntegralDischargeRatioInput` (row `#adaptiveIntegralDischargeRow`) | number `min=0 max=1 step=0.001`, baked 0.25, width 8rem | `adaptive.integralDischarge` "Integral discharge" / `help.adaptive.integralDischarge` (help text says it has little effect — project memory: exclude from tuning) | `adaptiveResamplingIntegralDischargeRatio` / 0.25 | `toFixed(3)`; Apply clamp 0..1 | gate `adaptiveEnabled` |

**Subpanel 3 — `adaptive.stabilizationPhases` "Stabilization phases"**

| Element | Type | Label / help | `app` field / default | Displayed as | Gate |
|---|---|---|---|---|---|
| `#adaptiveLowRecoverSettleStableMsInput` (row `#adaptiveLowRecoverSettleStableMsRow`) + `ms` | number `min=0 step=1`, baked 200, width 7rem | `adaptive.lowRecoverSettleStable` "Settling hold" / `help.adaptive.lowRecoverSettleStable` | `adaptiveResamplingLowRecoverSettleStableMs` / 200 | `String(max(0, round))`; Apply `max(0, round)` | `hasAudioDomain` |
| `#adaptiveLowRecoverSettleMarginMsInput` (row `#adaptiveLowRecoverSettleMarginMsRow`) + `ms` | number `min=0 step=0.1`, baked 6, width 7rem | `adaptive.lowRecoverSettleMargin` "Settling margin" / `help.adaptive.lowRecoverSettleMargin` | `adaptiveResamplingLowRecoverSettleMarginMs` / 6 | `toFixed(1)`; Apply `max(0, v)` | `hasAudioDomain` |
| `#adaptiveLowRecoverRefillDeltaAlphaInput` (row `#adaptiveLowRecoverRefillDeltaAlphaRow`) | number `min=0 max=1 step=0.01`, baked 0.5, width 8rem | `adaptive.lowRecoverRefillDeltaAlpha` "Refill EMA α" / `help.adaptive.lowRecoverRefillDeltaAlpha` | `adaptiveResamplingLowRecoverRefillDeltaAlpha` / 0.5 | `toFixed(2)`; Apply clamp 0..1 | `hasAudioDomain` |
| `#adaptiveControlSmoothingCutoffHzInput` (row `#adaptiveControlSmoothingCutoffRow`) | number `min=0.001 max=20 step=0.05`, baked 0.5, width 8rem | `adaptive.controlSmoothingCutoffHz` "IIR cutoff (Hz)" / `help.adaptive.controlSmoothingCutoffHz` | `adaptiveResamplingControlSmoothingCutoffHz` / 0.5 | `toFixed(3)`; Apply `max(0.001, v \|\| 0.5)` | `hasAudioDomain` |
| `#adaptiveControlSmoothingOrderSelect` (row `#adaptiveControlSmoothingOrderRow`) | select: `1` (`adaptive.controlSmoothingOrder.opt1` "1 (single pole, 6 dB/oct)"), `2` (`…opt2` "2 (Butterworth, 12 dB/oct)") | `adaptive.controlSmoothingOrder` "IIR order" / `help.adaptive.controlSmoothingOrder` | `adaptiveResamplingControlSmoothingOrder` (u32) / 1 | `String(v ?? 1)` unless Dirty; Apply `clamp(round, 1, 2)` | `hasAudioDomain` |
| `#adaptiveUsePreBridgeClockToggle` (row `#adaptiveUsePreBridgeClockRow`) | switch | `adaptive.usePreBridgeClock` "Pre-bridge clock (PI input)" / `help.adaptive.usePreBridgeClock`; also `title` = `adaptive.usePreBridgeClockTitle` | `adaptiveResamplingUsePreBridgeClock` (u8) / false | checked | `hasAudioDomain`; change → optimistic + `sendAudioConfig()` (immediate) |
| `#adaptiveUseOutputPacingToggle` (row `#adaptiveUseOutputPacingRow`) | switch | `adaptive.useOutputPacing` "Output pacing (post-render)" / `help.adaptive.useOutputPacing`; title `adaptive.useOutputPacingTitle` | `adaptiveResamplingUseOutputPacing` / false | checked | same |
| `#adaptiveDisableBackpressureToggle` (row `#adaptiveDisableBackpressureRow`) | switch | `adaptive.disableBackpressure` "Disable back-pressure (diag)" / `help.adaptive.disableBackpressure`; title `adaptive.disableBackpressureTitle` | `adaptiveResamplingDisableBackpressure` / false | checked | same |

Note: `AppState` still exposes the individual OSC commands
(`control_adaptive_resampling*`, `commands/resampling.rs:12-185`, addresses
`/omniphony/control/adaptive_resampling/{enable_far_mode,
force_silence_in_far_mode, hard_recover_high_in_far_mode,
hard_recover_low_in_far_mode, far_mode_return_fade_in_ms, kp_near, ki,
integral_discharge_ratio, max_adjust, update_interval_callbacks,
high_recover_entry_margin_ms, pause}`), but the web UI no longer calls any
of them: everything goes through the batched `/omniphony/control/config/audio`
+ `/apply` pair. The egui port should do the same.

#### 2.2.6 Info modals reachable from this section

- `#telemetryGaugesInfoModal` (title `telemetry.infoTitle` "Latency Gauges",
  body `telemetry.infoBody`, close `common.close`) — opened by clicking the
  "Latency" title.
- `#adaptiveResamplingInfoModal` (title `adaptive.infoTitle` "Latency
  controls", body `adaptive.infoBody`) — opened by `#adaptiveResamplingInfoBtn`.
- Modal mechanics (`modals.js:61-188`, `modal-and-toggle-listeners.js:48-80`):
  `.info-modal.open` overlay; closes on the Close button, on a click on the
  backdrop, or Escape (`modal-and-toggle-listeners.js:317-339`).

#### 2.2.7 Resample plot (entry point only)

`#resamplePlotToggleBtn` → `toggleResamplePlot()` (`resample-plot.js:62-64`):
shows/hides `#resamplePlotContainer`, toggles `.is-active` + `aria-pressed`
on the button, and while visible polls every 20 ms
(`latencySmoothedMs`, `latencyTargetMs`, `resampleRatio`) into the
auto-tune sparkline renderer (`src/auto-tune/sparkline.js`). Window length
comes from `getPlotWindowMs()` of the diag plot (options 5/10/30/60 s,
default 10 s, persisted under localStorage `diagPlot.windowMs.v1`). **The
plot itself is out of scope for the panels phase**; the button and the
container's presence are the entry point.

### 2.3 Diagnostics section (`#diagSection`, `audio-panel.js:283-292`)

- Header row: title `data-i18n="section.diagnostics"` ("Diagnostics") and
  `#diagPlotToggleBtn.info-icon-btn` (SVG: circle r=6 with a clock hand
  `M8 4v4l3 2`; title `telemetry.diagPlotToggle` "Toggle diagnostic-metrics
  plot"; `aria-pressed`).
- `#diagPlotContainer` (`display:none`) → `#diagPlotControls` (flex-wrap row,
  11 px `#b9c7d8`) built dynamically by `controls/diag-plot.js` and
  `#diagPlotCanvas` 600×240.
- Click → `toggleDiagPlot()` (`diag-plot.js`). **Out of scope** except:
  the plot's control strip contains the **diag rate** select (label "diag",
  options `[10, 20, 50, 100, 200]` Hz, default 50, localStorage
  `diagPlot.diagRateHz.v1`; change → `invoke('control_diag_rate_hz',{value})`
  → OSC `/omniphony/control/diag/rate_hz` float clamped 1..1000,
  `commands/diag.rs:23-32`) and the **diag publication** toggle (while the
  plot is open the UI sends `invoke('control_diag_publication_enabled',{enable})`
  → `/omniphony/control/diag/enabled` int, with a 1 s keep-alive,
  `diag-plot.js:71, 390-396`). The renderer's value arrives in the snapshot
  as `diagRateHz` (`AppState.diag_rate_hz`) and is applied by
  `syncDiagRateFromRenderer` (`init.js:113`). Diag schema/values arrive as
  events `diag:schema` / `diag:values` (JSON strings) into `app.diagSchema`
  / `app.diagValues`.
- This section is visible in both producer variants (comment `app.css:2707-2710`).

### 2.4 Master section (`#masterSection`, `audio-panel.js:293-320`)

**Header** `.master-header` (flex, gap 0.5rem):

| Element | Description | Source / behaviour |
|---|---|---|
| title `.info-title` | `data-i18n="master.title"` ("Master"), `data-help-i18n="help.master.gain"` with anchor `.master-header` (inline help panel appears below the whole header) | — |
| master meter `.meter-bar.level-meter` (`flex:1 1 auto`) | `#masterMeterFill.meter-fill` + `#masterMeterPeak.meter-peak` | `updateMasterMeterUI` (`master.js:125-159`), on `dirty.masterMeter` |
| `#masterMeterText.fixed-metric` | `${formatNumber(rmsDb,1)} dB`, or `t('status.masterMeter')` ("— dB") before the first meter | — |
| `#autoGainSectionToggleBtn.panel-toggle-btn` | "▸", title `autoGain.toggle` ("Auto-gain settings") | toggles `app.autoGainSectionOpen` via `setAutoGainSectionOpen` (`modals.js:287-298`): `.open` on `#autoGainSection`, chevron; default closed (`app.js:254`). NOT exempt from the runtime lock |

**Master meter ballistics** — the backend is the single source of truth:

- `masterLevel` (`state.js:22-25`) is set by `updateMasterLevel(meter)`
  (`speakers.js:2401-2408`) from event `master:meter` (also inside
  `state:batch`). Payload `{ meter: { peakDbfs, rmsDbfs, peakHoldDbfs } }`
  (`osc_listener.rs:2371-2392`). When the engine does not publish
  `/omniphony/meter/master`, the host reconstructs it from the speaker
  meters (`derived_master_meter`, `osc_listener.rs:1955-1986`): peak = max
  speaker peak; rms = 20·log10(sqrt(mean(rms_linear²))) with floor −60;
  payload carries `"derived": true`. NOTE: the JS `updateMasterLevel` drops
  `peakHoldDbfs` (it only copies peak/rms), so `master.js:121` falls back to
  `holdDb = peakDb` — the master hold cursor therefore tracks the raw peak,
  not the backend hold. An egui port reading the model directly can use
  the real hold.
- Bar `--level` = `dbToMeterPercent(peakDb)` (bar follows the **peak**, the
  numeric readout is the **RMS**). Peak cursor `--level` =
  `dbToMeterPercent(holdDb)`; opacity 1 when `holdPercent > 0.1` else 0;
  class `over` when `holdDb ≥ 0`.
- Peak-hold algorithm (`src-tauri/src/peak_hold.rs`, copied in the egui
  crate as `src/host/peak_hold.rs`): hold 1000 ms, then decay at
  120 dB/s measured from the previous update, never below the current
  peak; a new peak ≥ the held value re-arms; when the decayed value lands
  within 0.1 bar-percent of the live peak the hold re-arms. Keys:
  `"master"`, `"spk:{id}"`, `"src:{id}"`, `"ear:{id}"`.
- Note: the speaker/object meters ALSO decay client-side in
  `decayMeters` (`speakers.js:2410-2461`): after `METER_DECAY_START_MS = 250`
  without a new sample, peak and rms fall at `METER_DECAY_DB_PER_SEC = 45`
  dB/s down to −100. The master meter has no such client decay.

**Gain row** (`.control-row`, `#masterSection .control-row { grid-template-columns: 1fr auto }`):

| Element | Type | Value | On change | Enabled |
|---|---|---|---|---|
| `#masterGainSlider.gain-slider` | range `min=0 max=2 step=0.01`, baked 1 | `app.masterGain` (`AppState.master_gain`, null until known); slider value `hasValue ? masterGain : 1` where `hasValue = finite && > 0` | `input` (live while dragging): if `!oscSnapshotReady` just re-render; ignore non-finite or ≤ 0; else `app.masterGain = value`, re-render, `invoke('control_master_gain',{gain})` → OSC `/omniphony/control/realtime/master_gain [Float clamp(0..2), Int seq]` (`commands/gain.rs:55-65`; `seq` = monotonically increasing `realtime_seq`). `dblclick` → reset to 1 and send | `disabled = !oscSnapshotReady \|\| !hasValue \|\| !supportsRealtimeKey('master_gain')` (`producerCapabilities.realtime` must list `master_gain`) |
| `#masterGainBox.gain-box` | text | `formatLinearAsDb(masterGain)` (e.g. `0.0 dB`) or `—` | — | — |

Event `master:gain` (`{value}`) updates `app.masterGain`.

**Auto-gain block** `#autoGainSection.conditional-params` (collapsed by default):

| Element | Type | Label / help | Value | On change | Enabled |
|---|---|---|---|---|---|
| `#clipIndicator.clip-indicator` | 9×9 px dot, `bg rgba(255,255,255,0.18); border 1px rgba(255,255,255,0.25)`; `.clip-active` = `#ff3b30` with glow `0 0 6px rgba(255,59,48,0.9)` (`app.css:965-980`); `title="Clip"`, `aria-label="Clip indicator"` (not i18n) | sits left of the auto-gain label | event `clip:detected` `{speaker}` → `flashClipIndicator()` (`master.js:89-102`): add `.clip-active`, remove after 1000 ms (timer restarted on each clip); also `flashSpeakerClip(speaker)` (§4.3). Works regardless of the auto-gain switch | — | — |
| `#autoGainToggle` | switch (`.switch-row`) | `autoGain.title` "Auto-gain (anti-clip)" / `help.master.autoGain` (anchor `.switch-row`) | `app.autoGain === true` (`AppState.auto_gain`) | optimistic; `invoke('control_auto_gain',{enable:0/1})` → `/omniphony/control/auto_gain` int | `disabled = !oscSnapshotReady` |
| `#autoGainCeilingSlider.gain-slider` (row `#autoGainCeilingRow`) with label text `autoGain.ceiling` "Ceiling" / `help.master.ceiling` and value `#autoGainCeilingVal` | range `min=-12 max=0 step=0.1`, baked −1 | `app.autoGainCeilingDb` (`auto_gain_ceiling_db`, JS default −1.0); value text `${db.toFixed(1)} dB` | `input` → optimistic, `invoke('control_auto_gain_ceiling',{db})` → `/omniphony/control/auto_gain_ceiling` float clamp −12..0 | `disabled = !oscSnapshotReady` |

The ceiling row is always shown inside the block (no dependency on the toggle).

### 2.5 Items the brief lists under "audio panel" that live elsewhere

- **Ear mute / headphone meter** — rows of the Speakers section, §4.2.
- **DRC gain meter** — left overlay, `#drcSection` header (`index.html:500-510`):
  `#drcGaugeRow` (shown only when `app.oscMeteringEnabled`) with a 6 px bar
  `#drcGaugeFill` anchored right and `#drcGainValue`. `updateDrcMeterUI(gain)`
  (`controls/drc.js:110-136`) on event `meter:drc_gain` `{value}` (linear):
  `db = linearToDb(gain)` (−100 if not finite); width `min(100, |db|/20 × 100)%`;
  text `${db>=0?'+':''}${db.toFixed(1)} dB`; colour `#33b5e5` if db > 1,
  `#ff4444` if db < −12, `#ffbb33` if db < −6, else `#00c851`.
- **Loudness** — same DRC section: `#loudnessToggle` switch (`section.loudness`
  / `help.drc.loudness`; change → optimistic `app.loudnessEnabled`,
  `invoke('control_loudness',{enable})` → `/omniphony/control/loudness` int;
  listener lives in `audio-panel-listeners.js:110-117`) and `#loudnessInfo`
  (three lines, `master.js:167-194`: `source loudness: {n} dBFS`,
  `target loudness: {source+correction} dBFS`, `correction: {gain.toFixed(2)} ({dB})`,
  all not i18n; from `app.loudnessSource`, `app.loudnessGain`, snapshot
  `loudness`/`loudnessSource`/`loudnessGain`). The DRC summary
  (`drc.js:83-95`) shows `"{mode} ({weight}%) | Loudness ON|OFF"`.
- **Timing readouts** (`render:time_ms`, `decode:time_ms`,
  `crossover:time_ms`, `write:time_ms`, `frame:duration_ms`) — rendered in
  the Renderer panel header, §3.1.2.
- **Metering rate** — Objects section header of the left overlay
  (`#oscMeteringToggle` + rate select, `controls/osc.js:620-670`):
  options `[10,20,50,100,200]` Hz, default 50, localStorage
  `audioMetering.rateHz.v1`; change → `invoke('control_metering_rate_hz',{value})`
  → `/omniphony/control/metering/rate_hz` float clamp 1..1000; the renderer's
  value arrives as snapshot `meterRateHz`. `osc:metering` event
  `{enabled}` sets `app.oscMeteringEnabled` (drives the renderer perf
  gauges' visibility and the DRC gauge).

---

## 3. Renderer panel (`#rendererPanelRoot`)

Markup `src/ui/renderer-panel.js:3-530` (mounted by `mountRendererPanel()`,
`:532-541`, which also runs `wireInlineHelpFromMarkup` on the root).
Listeners: `src/listeners/renderer-panel-listeners.js` (backend / evaluation
/ distance), `src/controls/binaural.js` (output mode, tabs, binaural
subpanels), `src/listeners/audio-panel-listeners.js:563-567` (ramp mode),
`options-binder.js` (crossover `data-option` controls). Renders:
`src/controls/vbap.js`, `controls/binaural.js`, `controls/distance-diffuse.js`,
`controls/master.js:205-228` (distance model), `controls/hybrid-curve.js`,
`controls/latency.js:415-576` (perf gauges), `controls/audio.js:234-259`
(crossover info).

Structure:

```
#rendererSection.info-section.renderer-panel-shell
  .renderer-panel-header-block
    .panel-header  (title | #outputModeSelect | #rendererPerfWrap | #rendererSectionToggleBtn)
    #rendererSummary.panel-summary
  #rendererSectionContent.conditional-params
    .renderer-panel-stack (grid, gap 0.35rem)
      .output-mode-mpv-note
      #rendererTabsBar (Renderer | Binaural)
      #binauralHrtfSection      .binaural-subpanel  (Binaural tab)
      #binauralDistanceSection  .binaural-subpanel
      #binauralRoomSection      .binaural-subpanel
      #binauralTrackingSection  .binaural-subpanel
      #evaluationSection                             (Renderer tab)
      #rampSection
      #crossoverSection                              (both tabs)
      #backendParametersSection  (+ #hybridSection, #backendGenericParamsSection)
      #distanceDiffuseSection
      #distanceModelSection
```

Every subpanel uses `.info-section.renderer-subpanel` with inline
`margin:0; padding:0.4rem 0.5rem; border:1px solid rgba(255,255,255,0.08);
border-radius:8px; background:rgba(255,255,255,0.03)`; its
`.renderer-subpanel-bar` is a flex space-between row holding a 12 px bold
white title and `.renderer-subpanel-actions`; its `.renderer-subpanel-body`
is `margin-top:0.25rem; margin-left:1rem; padding:0.3rem 0.4rem;
background:rgba(255,255,255,0.03); border-radius:6px; display:grid;
gap:0.18rem` (binaural bodies have no left margin and gap 0.3rem).
`app.css:1414-1470` adds `min-width:0; overflow:hidden` guards so nothing
in the panel can widen the overlay.

### 3.1 Header

Collapse: `app.rendererSectionOpen` (default `false`, `app.js:250`);
`#rendererSectionToggleBtn` → `setRendererSectionOpen` (`modals.js:321-340`),
same mechanics as §2.1 (summary shown only when collapsed, reason
`'renderer-section-toggle'`).

#### 3.1.1 Title and output mode

- Title `.info-title.panel-title` `data-i18n="section.renderer"` ("Renderer").
- `#outputModeSelect.form-select` (`margin-left:auto`), title
  `outputMode.selectTitle` ("Output mode"). Options:
  `speaker` (`outputMode.speakers` "Speakers"), `binaural-direct`
  (`outputMode.headphones` "Headphones"), `binaural-cascaded`
  (`outputMode.headphonesVirtual` "Headphones (virtual room)").
  - Value ← `applyBinauralState(b)` (`binaural.js:423-442`):
    `b.outputMode === 'binaural' ? (b.mode === 'cascaded' ? 'binaural-cascaded' : 'binaural-direct') : 'speaker'`
    from `AppState.binaural` (passthrough JSON, keys `outputMode`, `mode`).
    Not written while the select has focus.
  - **Not optimistic**: `change` sends only (`binaural.js:72-92`):
    `speaker` → `control_output_mode {value:'speaker'}`;
    `binaural-direct` → `control_output_mode {value:'binaural'}` then
    `control_binaural_mode {value:'direct'}`; `binaural-cascaded` →
    `control_output_mode 'binaural'` + `control_binaural_mode 'cascaded'`.
    OSC `/omniphony/control/output_mode` string (`speaker|binaural`) and
    `/omniphony/control/binaural_mode` string (`direct|cascaded`)
    (`commands/binaural.rs:12-41`).
  - Side effects of the state echo: `body.output-binaural`,
    `body.output-cascaded` (see §0.4), `setSpeakersGhosted(binaural)`
    (3D: speaker opacity × 0.18, labels 0.3), and the editing tab auto-aims to
    Binaural / Renderer **only when the `outputMode/mode` pair actually
    changes** (`lastSeenOutputMode`, `binaural.js:437-441`).

#### 3.1.2 Renderer performance gauges (`#rendererPerfWrap`)

`renderer-panel.js:18-46`; rendered by `renderRenderTimeUI`
(`latency.js:415-576`) on `dirty.renderTime`. Visible (`display:block`)
only when `app.oscMeteringEnabled === true` (event `osc:metering`,
snapshot `oscMeteringEnabled`); otherwise `display:none`. Fixed width
180 px; sits between the output-mode select and the chevron.

Layout (grid, gap 0.18rem):
1. Row: a 180 px `.meter-bar` (overflow visible) with four stacked
   segment fills and four max markers, then `#rendererPerfFrameValue`
   (10 px, `#9eb4c8`, right aligned, min-width 5.4rem).
2. Row: four 10 px `#d9ecff` readouts `#rendererPerfDecodeValue`,
   `#rendererPerfCrossoverValue`, `#rendererPerfRenderValue`,
   `#rendererPerfWriteValue` (min-width 5.4rem each, right aligned).
3. Row: four `#92a9bc` max readouts `#rendererPerf{Decode,Crossover,Render,Write}MaxValue`.

Segment fills (each a `.meter-fill` with its own gradient, clipped with
`clip-path: inset(0 right% 0 left%)`):
- `#rendererPerfDecodeFill` `rgba(140,214,255,0.95) → rgba(104,170,255,0.95)`
- `#rendererPerfRenderFill` `rgba(112,170,255,0.92) → rgba(88,132,255,0.92)`
- `#rendererPerfCrossoverFill` `rgba(255,214,120,0.96) → rgba(255,166,94,0.96)`
- `#rendererPerfWriteFill` `rgba(180,255,184,0.95) → rgba(80,218,120,0.95)`
Max markers (`.meter-marker.min` with overridden colours):
`#rendererPerfDecodeMaxMarker #ffd54a`, `#rendererPerfRenderMaxMarker #ffb84a`,
`#rendererPerfCrossoverMaxMarker #ffeb8a`, `#rendererPerfWriteMaxMarker #ff8b4a`.

Computation (`latency.js:441-529`):
- Instantaneous: `dec = max(0, app.decodeTimeMs)`, `rndTotal = max(0, app.renderTimeMs)`,
  `cro = min(rndTotal, max(0, app.crossoverTimeMs))`, `rnd = rndTotal − cro`,
  `wri = max(0, app.writeTimeMs)` (crossover time is *contained* in render time).
- Window aggregates from `app.timingStats[stage].max.max` / `.avg.mean`
  (null when empty). `croMax = min(rndTotalMax, croMaxRaw)`,
  `rndMax = rndTotalMax − croMax`; same for the averages.
- Scale `scaleMs = frameDurationMs` if finite > 0, else
  `max(0.01, dec+cro+rnd+wri, decMax+croMax+rndMax+wriMax)`.
- Segments are drawn end to end in the order decode, crossover, render,
  write: `setSegment(el, startMs, endMs)` with left `= start/scale×100`,
  right `= 100 − end/scale×100`, both clamped 0..100, `toFixed(1)`.
- Markers at the cumulative maxima: decodeMax; decodeMax+croMax;
  +rndMax; +wriMax (hidden when every contributing max is null).
- Readouts show the **1 s average** (not the instantaneous value), held
  for `RENDER_TIME_DISPLAY_HOLD_MS = 350` ms between refreshes
  (`getStableRenderPerfValue`), formatted `msWithPct`: `${formatNumber(ms,3)} ms`
  plus ` (${pct}%)` when a frame budget is known, pct = `ms/frame×100` with
  0 decimals when ≥ 10 else 1 decimal. Texts use `tf('renderer.perf.decode',{value})`
  = `decode {value}`, `renderer.perf.crossover` = `cross {value}`,
  `renderer.perf.render` = `render {value}`, `renderer.perf.write` =
  `write {value}`, `renderer.perf.max` = `max {value}`,
  `renderer.perf.frame` = `frame {value}` (`{value}` = `${formatNumber(frame,3)} ms` or `—`).
- Sources: events `decode:time_ms`, `render:time_ms`, `crossover:time_ms`,
  `write:time_ms` (`{value}`; non-finite → null), `frame:duration_ms`
  (`{value}` > 0 else ignored), `latency:stats`, `osc:metering`
  (disabling metering nulls decode/render/write) (`tauri-bridge.js:569-627`);
  snapshot `decodeTimeMs`, `renderTimeMs`, `writeTimeMs`, `frameDurationMs`
  (note `crossoverTimeMs` is NOT read from the snapshot by `applyInitState`).

#### 3.1.3 Summary (`#rendererSummary`)

`renderEvaluationMode` (`vbap.js:328-333`):
`${backendLabel(backend)} / ${tf('renderer.summary',{mode})}` where
`renderer.summary` = `"mode: {mode}"`, `backend = effective || selection || 'vbap'`,
`mode = formatEvaluationModeLabel(effectiveMode || selection)`
(`auto` → `t('common.auto')`, `realtime` → `t('eval.mode.realtime')`,
`precomputed_polar` → `t('common.polarShort')` "Polar", `precomputed_cartesian`
→ `t('common.cartesianShort')` "Cartesian", else `—`).
`backendLabel` (`vbap.js:88-97`): the renderer-provided `effectiveLabel` when
the id is the effective backend; else `VBAP`, `Barycenter`, `Distance`
(`experimental_distance`), `Hybrid`, or the raw id.

### 3.2 Content head: mpv note and tabs

- `.output-mode-mpv-note` (`data-i18n="outputMode.mpvNote"`, 0.65 rem
  `#8fa6bd`): shown only under `body.cap-embedded` (`app.css:2757-2759`).
- `#rendererTabsBar`: two `.toggle-btn.renderer-tab-btn` buttons
  `#rendererTabRendererBtn` (`rendererTabs.renderer` "Renderer") and
  `#rendererTabBinauralBtn` (`rendererTabs.binaural` "Binaural"), each
  `flex:1 1 0; opacity:0.55`; `.active` → `opacity:1; border rgba(86,156,255,0.8);
  bg rgba(86,156,255,0.18); color #cfe4ff` (`app.css:2738-2747`).
  Click → `setStudioTab(bool)` (`binaural.js:40-46`): toggles
  `body.studio-tab-binaural` and the `.active` classes. Pure UI state, not
  persisted, initial = Renderer. Exempt from the runtime lock.
  CSS gating (`app.css:2728-2737`): with `studio-tab-binaural` the
  `#evaluationSection`, `#rampSection`, `#backendParametersSection`,
  `#distanceDiffuseSection`, `#distanceModelSection` are hidden; without it
  every `.binaural-subpanel` is hidden. `#crossoverSection` is visible on both tabs.

### 3.3 Binaural tab subpanels

All driven by `initBinauralPanel()` (`binaural.js:60-406`) and
`applyBinauralState(b)` (`:409-609`) where `b = AppState.binaural`
(passthrough JSON from `/omniphony/state/binaural`, applied on every
`state:snapshot_ready`). While applying, an `applying` guard suppresses
`change`/`input` echoes. `setVal(id, v)` never writes a focused control.
`send(cmd,args)` = `invoke(cmd,args)` with console error on failure. All
sends below are **not optimistic** (sliders update their value label
locally, then send).

#### 3.3.1 HRTF (`#binauralHrtfSection`)

Title: "HRTF" (not i18n) with `data-help-i18n="help.binaural.hrtf"`.

| Element | Type | Label / help | Value (from `b`) | On change | Visible |
|---|---|---|---|---|---|
| `#binauralHrirSource.form-select` (in the bar) | select: `saf` (`binaural.hrtfSource.kemar` "KEMAR (measured)"), `synthetic` (`…synthetic`), `pinna` (`…pinna` "Pinna (parametric)"), `prtf` (`…prtf` "PRTF (Spagnol)"), `sofa` (`…sofa` "SOFA file") | — | `b.hrirSource` | `saf`/`synthetic`/`sofa` → `control_hrir_source {value}`; `pinna` → `sendPinna()` = `control_hrir_source {value:'pinna:<preset>:<dScale>:<depth>'}` (preset `pbnh|rd`, ints); `prtf` → `control_hrir_source 'prtf:<freqScale>:<depth>'`. OSC `/omniphony/control/binaural/hrir_source` string (`commands/binaural.rs:81-90`; the SOFA browser sends `sofa:<local path>`) | always |
| `#sofaBrowseBtn.toggle-btn` (in the bar) | button `backend.file.browse` "Browse…", title `binaural.sofaBrowseTitle` | — | — | opens `#sofaBrowserModal` (`sofa-browser.js:508-516`, **SOFA browser out of scope**) | only when source `=== 'sofa'` |
| `#binauralSofaInfo` | info line 0.65 rem, word-break | — | (`binaural.js:466-492`) if `b.hrirEffective !== b.hrirSource`: `tf('binaural.hrtfFallback',{effective: label})` in `#e8c46a`, title = `b.hrirError`; else if source is `sofa`: `File: {basename}` (title full path, `#8fa6bd`) or the hardcoded warning `No SOFA file selected — using the embedded KEMAR until you pick one (Browse…).` in `#e8c46a`; else hidden | read-only | see left |
| `#binauralDiffuseFieldEq` | switch (`.inline-toggle`) | `binaural.diffuseFieldEq` "Diffuse-field EQ" / `help.binaural.diffuseFieldEq` | `b.diffuseFieldEq` (bool) | `control_binaural_diffuse_field_eq {enable:0/1}` → `/omniphony/control/binaural/diffuse_field_eq` int | always |
| `#binauralHeadRadius` + value `#binauralHeadRadiusVal` | range `min=5 max=15 step=0.1`, baked 8.75, `width:100%`; label row `.binaural-help-row` 0.65 rem `#888` | `binaural.headRadius` "Head radius (cm)" / `help.binaural.headRadius` | `b.headRadiusM × 100`; value text `toFixed(1)` (baked "8.8") | `input` → value text; `control_binaural_head_radius {value: cm/100}` → `/omniphony/control/binaural/head_radius` float clamp 0.05..0.15 | always |
| `#binauralHrirUpdateLattice.form-select` | select bound by `data-option="hrir_update_lattice"`: `exact` (`binaural.hrirLattice.exact`), `fine`, `balanced`, `coarse` | `binaural.hrirUpdateLatticeLabel` "HRIR update" / `help.hrirUpdateLattice` | `getLiveOption('hrir_update_lattice')` | generic binder → `control_option` (§2.1 row 4) | always |
| `#binauralPinnaControls` (grid) → `#binauralPinnaPreset` select (`pbnh` "PB & NH", `rd` "RD"; label `binaural.pinnaPreset` / `help.binaural.pinnaPreset`), `#binauralPinnaDScale` range `50..150 step 5` baked 100 (label `binaural.pinnaDScale` "Elevation factor D (%)" / `help.binaural.pinnaDScale`, value `#binauralPinnaDScaleVal` integer), `#binauralPinnaDepth` range `0..100 step 5` baked 100 (`binaural.pinnaDepth` "Pinna echoes (%)" / `help.binaural.pinnaDepth`, value `#binauralPinnaDepthVal`) | — | — | **not reflected from state** (the renderer carries them in the source string only; the JS never parses it back) | any change → `sendPinna()` | only when source `=== 'pinna'` (`display:grid`) |
| `#binauralPrtfControls` → `#binauralPrtfDepth` range `0..100 step 5` baked 100 (`binaural.prtfDepth` "Pinna coloration (%)" / `help.binaural.prtfDepth`, `#binauralPrtfDepthVal`), `#binauralPrtfFreqScale` range `50..150 step 5` baked 100 (`binaural.prtfFreqScale` "Notch frequency scale (%)" / `help.binaural.prtfFreqScale`, `#binauralPrtfFreqScaleVal`) | — | — | not reflected | any change → `sendPrtf()` | only when source `=== 'prtf'` |

#### 3.3.2 Distance (`#binauralDistanceSection`)

Title `binaural.distanceTitle` "Distance".

| Element | Type | Label / help | Value | On change |
|---|---|---|---|---|
| `#binauralUnitScale` + `#binauralUnitScaleVal` | range `0.1..10 step 0.1`, baked 1; value `toFixed(1)` | `binaural.distanceScale` "Distance scale (m / unit)" / `help.binaural.distanceScale` | `b.unitScaleM` | `control_binaural_unit_scale {value}` → `/omniphony/control/binaural/unit_scale` float clamp 0.01..100 |
| `#binauralAirAbsorption` | switch, baked checked | `binaural.airAbsorption` "Air absorption (distance HF roll-off)" / `help.binaural.airAbsorption` | `b.airAbsorption` | `control_binaural_air_absorption {enable}` → `/omniphony/control/binaural/air_absorption` int |

#### 3.3.3 Listening room (`#binauralRoomSection`)

Title `binaural.roomTitle` "Listening room". Two switch rows (0.7 rem
`#8fa6bd`) each followed by a parameter block that is shown
(`display:grid`) only while its switch is checked
(`syncBinauralParamVisibility`, `binaural.js:51-58`, re-run on every state
apply and on the switch change).

| Element | Type | Label / help | Value | On change |
|---|---|---|---|---|
| `#binauralReflEnabled` | switch | `binaural.earlyReflections` "Early reflections" / `help.binaural.earlyReflections` | `b.reflections.enabled` | `control_binaural_reflections_enabled {enable}` → `/omniphony/control/binaural/reflections/enabled` |
| `#binauralReflLevel` + `#binauralReflLevelVal` (in `#binauralReflParams`) | range `0..1 step 0.01`, baked 0.5, `toFixed(2)` | `binaural.reflectionLevel` "Reflection level" / `help.binaural.reflectionLevel` | `b.reflections.level` | `control_binaural_reflections_level` → `.../reflections/level` float 0..1 |
| `#binauralReflRoomW`, `#binauralReflRoomD`, `#binauralReflRoomH` + `#binauralReflRoomVal` | three stacked ranges `1..20 step 0.1`, baked 4 / 5 / 2.7; value `W.toFixed(1) × D × H` (baked "4.0 × 5.0 × 2.7") | `binaural.roomDims` "Room W × D × H (m)" / `help.binaural.room` | `b.reflections.roomM` `[w,d,h]` | each → `control_binaural_reflections_room {axis:'width'\|'depth'\|'height', value}` → `.../reflections/room_width|room_depth|room_height` float clamp 1..20 |
| `#binauralReflWallCutoff` + `#binauralReflWallCutoffVal` | range `1..20 step 0.5` (kHz), baked 6, `toFixed(1)` | `binaural.wallDamping` "Wall damping (kHz)" / `help.binaural.wallDamping` | `b.reflections.wallCutoffHz / 1000` | `control_binaural_reflections_wall_cutoff {value: kHz×1000}` → `.../reflections/wall_cutoff` float clamp 1000..20000 |
| `#binauralRevEnabled` (row has `border-top:1px solid rgba(255,255,255,0.05); padding-top:0.3rem`) | switch | `binaural.lateReverb` "Late reverb" / `help.binaural.lateReverb` | `b.reverb.enabled` | `control_binaural_reverb_enabled` → `.../reverb/enabled` |
| `#binauralRevLevel` + `Val` (in `#binauralRevParams`) | range `0..1 step 0.01`, baked 0.25, `toFixed(2)` | `binaural.reverbLevel` / `help.binaural.reverbLevel` | `b.reverb.level` | `control_binaural_reverb_level` → `.../reverb/level` 0..1 |
| `#binauralRevRt60` + `Val` | range `0.1..1.5 step 0.05`, baked 0.35, `toFixed(2)` | `binaural.rt60` "RT60 (s)" / `help.binaural.rt60` | `b.reverb.rt60S` | `control_binaural_reverb_rt60` → `.../reverb/rt60` clamp 0.1..3.0 |
| `#binauralRevSize` + `Val` | range `0.5..2 step 0.05`, baked 1, `toFixed(2)` | `binaural.reverbSize` "Room size (×)" / `help.binaural.reverbSize` | `b.reverb.size` | `control_binaural_reverb_size` → `.../reverb/size` clamp 0.5..2 |
| `#binauralRevLowRatio` + `Val` | range `-2..2 step 0.1`, baked 0 — **log2 slider**: displayed/sent ratio = `2^value` (`toFixed(2)`, baked "1.00"); state → slider `log2(ratio)` | `binaural.reverbBassDecay` "Bass decay (× RT60)" / `help.binaural.reverbBassDecay` | `b.reverb.rt60LowRatio` (> 0) | `control_binaural_reverb_rt60_low_ratio {value: ratio}` → `.../reverb/rt60_low_ratio` clamp 0.25..4 |
| `#binauralRevHighRatio` + `Val` | same log2 mapping | `binaural.reverbTrebleDecay` "Treble decay (× RT60)" / `help.binaural.reverbTrebleDecay` | `b.reverb.rt60HighRatio` | `control_binaural_reverb_rt60_high_ratio` → `.../reverb/rt60_high_ratio` clamp 0.25..4 |

#### 3.3.4 Head tracking (`#binauralTrackingSection`)

Title `binaural.headTrackingTitle` "Head tracking (Sensors2OSC)" with
`data-help-i18n="help.binaural.headTracking"`. Bar actions: two
`.form-button`s `#binauralRecenter` (`binaural.recenter` "Recenter" →
`control_head_recenter` → `/omniphony/control/head/recenter` int 1) and
`#binauralCalibrate` (`binaural.calibrateAxes` "Calibrate axes", title
`help.binaural.calibrateAxes`): sends `control_head_calibrate {step}` with
`step = ['front','left','up'][calibrationStep] ?? 'front'` where
`calibrationStep` ← `b.tracking.calibrationStep`; OSC
`/omniphony/control/head/calibrate` string.

Body:

| Element | Type | Label / help | Value | On change |
|---|---|---|---|---|
| `#binauralCalibratePrompt` | 0.65 rem line | — | key `binaural.calibratePromptLeft` when step 1, `binaural.calibratePromptUp` when step 2, `binaural.calibrateDone` when `b.tracking.axesCalibrated === true`, else hidden; colour `#e8c46a` while a step is pending, `#8fa6bd` when done (`binaural.js:579-593`) | read-only |
| `#binauralTrackAddress.form-input` | text, placeholder `/android/rotationvector`, width 11rem | `binaural.oscAddressLabel` "OSC address" / `help.binaural.oscAddress` | `b.tracking.address` | `change` → `control_head_tracking_address {value}` → `/omniphony/control/head/tracking/address` string (trimmed) |
| `#binauralTrackFormat.form-select` | select: `auto` (`common.auto`), `quat` (`binaural.trackFormat.quat` "Quaternion"), `rotvec` ("Rotation vector"), `euler` ("Euler") | `binaural.trackFormatLabel` "Format" / `help.binaural.trackFormat` | `b.tracking.format` | `control_head_tracking_format {value}` → `.../head/tracking/format` (validated set) |
| `#binauralTrackSmoothing` + `#binauralTrackSmoothingVal` | range `0..0.99 step 0.01`, baked 0.2, `toFixed(2)` | `binaural.trackSmoothing` "Smoothing" / `help.binaural.trackSmoothing` | `b.tracking.smoothing` | `control_head_tracking_smoothing` → `.../head/tracking/smoothing` float clamp 0..0.999 |
| `#binauralTrackInvert` | switch | `binaural.invertRotation` "Invert rotation" / `help.binaural.invertRotation` | `b.tracking.invert` | `control_head_tracking_invert {enable}` → `.../head/tracking/invert` int |
| `#binauralPoseReadout` | 10 px monospace `#8fa6bd`, label `binaural.pose` "Pose" | — | `yaw {y}°  pitch {p}°  roll {r}°` (two spaces, integers) computed from the quaternion `b.headPose {w,x,y,z}`: `yaw = atan2(2(wz+xy), 1−2(y²+z²))`, `pitch = asin(clamp(2(wx−zy)))`, `roll = atan2(2(wy+zx), 1−2(x²+y²))` (`binaural.js:596-605`). Live poses also arrive as event `binaural:head_pose` but that only feeds the 3D head, not this readout | read-only |

### 3.4 Renderer tab: Evaluation, Ramp

#### 3.4.1 Evaluation (`#evaluationSection`, `renderer-panel.js:273-344`)

Bar: `.title-with-info` with `evaluation.title` "Evaluation" and
`#evaluationInfoBtn` (title `evaluation.infoButton`). At runtime the
"Evaluation" span is promoted to the modal trigger and the button is hidden
(`modal-and-toggle-listeners.js:31-46`); modal `#evaluationInfoModal`
(title `evaluation.infoTitle`, body `evaluation.infoBody`).

| Element | Type | Value | On change | Visible / enabled |
|---|---|---|---|---|
| `#renderEvaluationModeSelect.delay-input` (bar, `min-width:13rem`) | select; baked options `auto` (`common.auto`), `realtime` (`eval.mode.realtime`), `precomputed_polar` (`eval.mode.precomputedPolar`), `precomputed_cartesian` (`eval.mode.precomputedCartesian`). **Rebuilt at runtime** (`vbap.js:289-324`) from `app.renderBackendState.allowedEvaluationModes` (fallback: the four) plus the current selection/effective mode if missing; option text = `formatEvaluationModeLabel` (§3.1.3, so polar/cartesian read "Polar"/"Cartesian" here) | `app.evaluationModeState.selection` (`AppState.render_evaluation_mode_state.selection`), falling back to the first visible mode | `change` (`renderer-panel-listeners.js:235-249`): reject if not in the allowed list or unchanged; optimistic write; `markRecomputePending()`; `invoke('control_render_evaluation_mode',{value})` → `/omniphony/control/render_evaluation_mode` string (validated set, `commands/render.rs:391-406`) | `disabled` when the allowed list is empty |
| `#renderEvaluationModeEffective.vbap-step` (bar, `min-width:8rem`, right) | text | `formatEvaluationModeLabel(app.evaluationModeState.effective)` or `—` | read-only | — |

`#evaluationSectionContent.conditional-params.open` (always open) →
`.renderer-subpanel-body` containing two blocks whose visibility depends on
`visibleMode = selection === 'auto' ? (effective || 'auto') : selection`
and the backend `capabilities` (`AppState.render_backend_state.capabilities`,
camelCase keys `supportsPrecomputedCartesian`, `supportsPrecomputedPolar`…)
(`applyEvaluationModeVisibility`, `vbap.js:184-206`; uses both `hidden` and
`display:none !important`):

**Cartesian grid block** `#renderEvaluationCartesianBlock` — shown iff
`visibleMode === 'precomputed_cartesian' && capabilities.supportsPrecomputedCartesian`.
Row `#renderEvaluationCartesianRow` (grid `1fr auto`, `align-items:start`):
label `eval.cartesianGrid` "Cartesian grid" / `help.eval.cartesianGrid`; a
4-column grid of number inputs then a 4-column grid of step readouts
(`.vbap-step`, 10 px `#86a7c3` centred):

| Input | Attrs / placeholder | `app.vbapCartesianState.*` (`AppState.vbap_cartesian`) | Change → command (OSC `/omniphony/control/render_evaluation/cartesian/...`) | Step readout |
|---|---|---|---|---|
| `#vbapCartXSizeInput` | `min=1 step=1`, placeholder `X` | `xSize` (null → empty) | `value = max(1, round(v \|\| 1))`; `control_render_evaluation_cartesian_x_size` → `x_size` int ≥ 1 | `#vbapCartXStepInfo`: `2/xSize × metersPerUnit × 1000` → `${formatNumber(mm,1)}mm` or `—` |
| `#vbapCartYSizeInput` | `Y` | `ySize` | `..._y_size` → `y_size` | `#vbapCartYStepInfo` same formula |
| `#vbapCartZSizeInput` | `Z+` | `zSize` | `..._z_size` → `z_size` | `#vbapCartZStepInfo`: `1/zSize` (not 2) |
| `#vbapCartZNegSizeInput` | `min=0 step=1`, `Z-` | `zNegSize` (default 0; rendered `max(0, round)`) | `value = max(0, round(v \|\| 0))`; `..._z_neg_size` → `z_neg_size` int ≥ 0 | `#vbapCartZNegStepInfo`: `—` when `app.vbapAllowNegativeZ === false` or zNegSize = 0, else `1/zNegSize` in mm |

`metersPerUnit = app.metersPerUnit ?? 1` (room scale). Every change also
calls `markRecomputePending()` and `updateVbapCartesian()`, which
re-fetches the snap grid (`refreshVbapGridNodes`, `get_vbap_grid_nodes`)
and refreshes the 3D face grid (out of scope). Values are echoed by events
`render_evaluation:cartesian:{x_size,y_size,z_size,z_neg_size}` (`{value}`;
≤ 0 → null, z_neg < 0 → 0) and by the snapshot `vbapCartesian`.

**Polar grid block** `#renderEvaluationPolarBlock` — shown iff
`visibleMode === 'precomputed_polar' && capabilities.supportsPrecomputedPolar`.
Row `#renderEvaluationPolarRow`: label `eval.polarGrid` "Polar grid" /
`help.eval.polarGrid`; `.vbap-polar-grid` (3 columns × 2 rows, width
13.1rem) then `.vbap-grid-3` step readouts:

| Cell | Element | Attrs / placeholder | `app.vbapPolarState.*` | Change → command (`.../render_evaluation/polar/...`) |
|---|---|---|---|---|
| r1c1 | `#vbapPolarAzimuthResolutionInput` | `min=1 step=1`, `az n` | `azimuthResolution` | `max(1, round(v \|\| 1))` → `control_render_evaluation_polar_azimuth_resolution` → `azimuth_resolution` int ≥ 1 |
| r1c2 | `#vbapPolarElevationResolutionInput` | `el n` | `elevationResolution` | `..._elevation_resolution` → `elevation_resolution` |
| r1c3 | `#vbapPolarDistanceResInput` | `d n` | `distanceRes` | `..._distance_res` → `distance_res` |
| r2c1 | `#vbapAzimuthRangeInfo.vbap-polar-meta` | text | constant `-180..180` | — |
| r2c2 | `#vbapElevationRangeInfo.vbap-polar-meta` | text | `—` when `app.vbapAllowNegativeZ === null`, `-90..90` when true, `0..90` when false | — |
| r2c3 | `#vbapPolarDistanceMaxInput` | `min=0.01 step=0.01`, `d max` | `distanceMax` | `max(0.01, v \|\| 2)` → `..._distance_max` → `distance_max` float ≥ 0.01 |
| steps | `#vbapPolarAzStepInfo` / `#vbapPolarElStepInfo` / `#vbapPolarDistStepInfo` | `.vbap-step` | `360/azRes` → `${formatNumber(x,2)}°`; `(allowNegZ === false ? 90 : 180)/elRes` → `°`; `distanceMax/distanceRes` → `formatNumber(x,3)`; `—` when undefined | — |

Echo events `render_evaluation:polar:{azimuth_resolution,elevation_resolution,distance_res,distance_max}`,
`vbap:allow_negative_z` (`{enabled}` → `app.vbapAllowNegativeZ`, re-renders
the polar block), snapshot `vbapPolar`, `vbapAllowNegativeZ`.

**Position interpolation** `#renderEvaluationPositionInterpolationRow`
(`.inline-toggle`, shown iff the cartesian OR polar block is shown):
label `vbap.positionInterpolation` "Position interpolation" /
`help.vbap.positionInterpolation`; switch `#vbapPositionInterpolationToggleEl`;
checked = `app.vbapPositionInterpolation !== false` (snapshot
`vbapPolar.positionInterpolation`, event `render_evaluation:position_interpolation {enabled}`);
change → optimistic, `markRecomputePending()`,
`control_render_evaluation_position_interpolation {enable:0/1}` →
`/omniphony/control/render_evaluation/position_interpolation` int.

**Object size intervals** `#objectSizeIntervalsRow` (`.control-row`, grid
`1fr auto`): label `evaluation.objectSizeIntervals` "Object size intervals"
/ `help.eval.objectSizeIntervals`; `#objectSizeIntervalsInput.delay-input`
number `min=0 step=1`, width 5rem; value `String(app.objectSizeIntervals ?? 0)`
(snapshot `objectSizeIntervals`, u32) unless focused; change →
`value = max(0, round(v \|\| 0))` written back into the input, optimistic,
`markRecomputePending()`, `control_render_evaluation_object_size_intervals`
→ `/omniphony/control/render_evaluation/object_size_intervals` int ≥ 0.
Row hidden when the capabilities object reports
`supportsEventSize === false` (or snake `supports_event_size`); shown when
capabilities are unknown (`vbap.js:344-357`).

#### 3.4.2 Ramp (`#rampSection`, `renderer-panel.js:345-365`)

Title `renderer.rampTitle` "Ramp". Body row `#rampModeRow` (grid `1fr auto`):
`.title-with-info` with `<label for=rampModeSelect>` `audio.rampMode` "Ramp
mode" (12 px bold) and `#rampModeInfoBtn` (promoted: the label becomes the
trigger of `#rampModeInfoModal`, title `rampMode.infoTitle`, body
`rampMode.infoBody`).

`#rampModeSelect.delay-input` (`min-width:9rem`): options `off`
(`audio.rampModeOff`), `frame` (`audio.rampModeFrame`, baked `selected`),
`interp` (`audio.rampModeInterp` "Per sample (interpolated)"), `sample`
(`audio.rampModeSample`). Value ← `app.rampMode` if in
`['off','frame','sample','interp']` else `frame` (`audio.js:173-175`;
snapshot `rampMode`, JS default `'sample'`, host default `"sample"`).
Change → `applyRampModeNow()` (`audio.js:375-384`): **only `off|frame|sample`
are accepted — selecting `interp` is silently ignored** (the host command
`control_ramp_mode` also rejects it, `commands/engine.rs:49-61`, OSC
`/omniphony/control/ramp_mode` string). Optimistic write.

### 3.5 Crossover (`#crossoverSection`, `renderer-panel.js:366-384`) — both tabs

Title `renderer.crossoverTitle` "Crossover". Body:

| Element | Type | Label | Value | On change | Visible |
|---|---|---|---|---|---|
| `#crossoverTypeSelect.delay-input` (row `#crossoverTypeRow`, grid `1fr auto`) | select bound `data-option="crossover_type"`: `lr4` (`renderer.crossoverType.lr4` "LR4 (low latency)"), `fir` (`renderer.crossoverType.fir` "Linear-phase FIR") | `renderer.crossoverTypeLabel` "Filter" (12 px bold; help key `help.crossoverType` exists in en.json but is NOT wired in the markup) | `getLiveOption('crossover_type')` | generic binder → `control_option {key:'crossover_type', value}` | always |
| `#crossoverTransitionInput.delay-input` (row `#crossoverTransitionRow`) | number bound `data-option="crossover_fir_transition_ratio"`, `min=0.05 max=2 step=0.05`, baked 0.5, width 5rem | `renderer.crossoverTransitionLabel` "FIR transition" (help `help.crossoverFirTransition` exists but is not wired) | `getLiveOption('crossover_fir_transition_ratio')` (not overwritten while focused) | `change` → `control_option` with a JSON number (host forwards as float; renderer clamps to its declared range and echoes) | row shown only when `crossover_type === 'fir'` (`audio.js:238-241`) |
| `#crossoverInfo` (0.65 rem `#888`) | text | — | from `app.crossover` (`AppState.live_options.crossover`, `{engine, bands, cutoffsHz, taps, latencyMs}` or null): if null or `bands <= 1` → `t('renderer.crossoverInfoNone')` ("No band edges in the layout — crossover inactive"); else `engine === 'fir'` → `tf('renderer.crossoverInfoFir',{bands, low, taps, latency})` = `"{bands} bands · {taps} taps · {latency} ms · low cut {low} Hz"` with `low = round(cutoffsHz[0])`, `taps.toLocaleString()`, `latency = latencyMs.toFixed(1)` (`'0.0'` if unknown); else `renderer.crossoverInfoIir` = `"{bands} bands · IIR · no added latency · low cut {low} Hz"` (`audio.js:234-259`) | read-only | always |

### 3.6 Backend (`#backendParametersSection`, `renderer-panel.js:385-454`)

Bar (`.renderer-subpanel-bar`, wraps):
- `.renderer-subpanel-titlebar`: `.title-with-info` with `backend.title`
  "Backend" and `#backendInfoBtn` (title `backend.infoButton`; promoted →
  the "Backend" span opens `#backendInfoModal`), then `#vbapStatus.vbap-status`
  (11 px, `min-width:0`, ellipsis).
- `.renderer-subpanel-actions`: `#renderBackendSelect.delay-input`
  (`width:min(100%,10.5rem)`), `#restoreBackendBtn.secondary-btn`
  (`backend.restore` "Restore backend", `display:none`), `#renderBackendEffective.vbap-step`
  (`min-width:5.4rem`, right).

**Backend info modal** (`modals.js:116-140`): body = `t('backend.infoBody')`
plus, when `help.backend.<id>` exists for the selected/effective id, `<br><br><strong>{label}</strong><br>{help}`.
Known keys: `help.backend.vbap`, `.barycenter`, `.experimental_distance`,
`.hybrid`, `.example`, `.script`.

| Element | Value | On change | Enabled |
|---|---|---|---|
| `#vbapStatus` | `renderVbapStatus` (`vbap.js:265-287`): if `app.recomputeError` (string) → the message, class `error`, `color:#ff7676`, title = message; else if `app.vbapRecomputing === true` → `t('vbap.status.computing')` ("computing..."), class `computing` (`#ffbf66`); else if `=== false` → `t('vbap.status.ready')` ("up to date"), class `ready` (`#78e08f`); else (`null`) → `t('vbap.status.idle')` ("—") | read-only. Sources: event `vbap:recomputing {enabled}` (also clears the ack watchdog and, when true, the error), `speakers:recompute_error {message}` (sets the error and forces recomputing=false), snapshot `vbapRecomputing`, `recomputeError`. **Watchdog** (`markRecomputePending`, `vbap.js:218-263`): every control that triggers a recompute sets `vbapRecomputing=true` optimistically and arms an 8000 ms timer; if no `vbap:recomputing` broadcast arrives, `recomputeError = t('vbap.status.noAck')` ("engine did not answer — is it the renderer you expected?") | — |
| `#renderBackendSelect` | baked options `vbap` "VBAP", `barycenter` "Barycenter", `experimental_distance` "Distance", `hybrid` "Hybrid" (not i18n); **rebuilt** from `app.renderBackendState.availableBackends` (`[{id,label,params:[...]}]`) when the engine publishes a list (`syncBackendOptions`, `vbap.js:367-390`); value = `app.renderBackendState.selection` (default `'vbap'`) | `change` (`renderer-panel-listeners.js:153-163`): ignore empty/unchanged; optimistic; `markRecomputePending()`; `control_render_backend {value}` → `/omniphony/control/render_backend` string (any non-empty id, lower-cased) | `disabled = frozenSpeakers` |
| `#restoreBackendBtn` | — | `click`: only if `app.renderBackendState.restoreBackendAvailable === true`; `markRecomputePending()`; `control_restore_render_backend` → `/omniphony/control/render_backend/restore` int 1 | **Always hidden and disabled** by `renderRenderBackend` (`vbap.js:655-658`), regardless of `restoreBackendAvailable` — effectively dead UI |
| `#renderBackendEffective` | `backendLabel(app.renderBackendState.effective)` or `—` | read-only | — |

Which backend's controls are shown (`applyRendererBackendVisibility`,
`vbap.js:99-153`): `visibleBackend = selection === 'script' ? selection :
(effective || selection)` (the script backend follows the selection so its
file field is reachable while the build fails). In hybrid mode
`paramBackend` is the active hybrid tab (§3.6.2), else the backend itself.

`#backendParametersSectionContent.conditional-params.open` →
`#backendSpecificParamsSection` (`display:flex; flex-direction:column`,
shown when generic params or the hybrid section apply, else `display:none`)
containing `#hybridSection` (order −1, so it renders first) and the
dynamically created `#backendGenericParamsSection`.

#### 3.6.1 Schema-generated backend parameters (`#backendGenericParamsSection`)

`renderGenericBackendParams(targetBackend)` (`vbap.js:575-636`). Container
created once: `.control-section.renderer-subpanel-body` with inline
`margin-top:0.25rem; margin-left:1rem; padding:0.3rem 0.4rem;
background:rgba(255,255,255,0.03); border-radius:6px; display:grid;
gap:0.18rem; font-size:11px`. Hidden (and any open help closed) when the
target has no schema or is bespoke (`BESPOKE_BACKENDS = ['hybrid']`).

Schema: `availableBackends[].params = [{ key, label, help?, kind: {type,...},
default }]`. Values: `app.renderBackendState.backendParamValuesById[backend][key]`
(`AppState.render_backend_state.backend_param_values_by_id`) else
`spec.default`. Rebuilt when the target backend or the locale changes;
otherwise only values are refreshed (never for the focused control).

Localisation (`vbap.js:421-433`): label = `t('backendParam.'+key)` if
translated else `spec.label || key`; help = `t('backendParamHelp.'+key)` else
`spec.help`; enum option = `t('backendParamOption.'+key+'.'+value)` else the
schema label. Known keys in en.json: `sharpness`, `localize`, `spread_min`,
`spread_max`, `spread_from_distance`, `spread_distance_range`,
`spread_distance_curve`, `size_to_spread_mode` (options `max`, `mean`,
`projection_perpendicular`).

Each param is a `.generated-param-field` holding a `.control-row.generated-param-row`
(label + control) and, when `spec.help` exists, an inline help panel below
(label is the trigger, §0.2). Control by `kind.type` (`buildParamControl`, `vbap.js:439-565`):

| kind.type | Control | Send |
|---|---|---|
| `bool` | switch (`input[type=checkbox]` in `.control-row` → pill) | `change` → `sendBackendParam(key, checked, backend)` |
| `enum` | select of `kind.options[{value,label}]` | `change` → string value |
| `path` | text `.delay-input`, `min-width:12rem`, placeholder `/path/to/backend.lua` | `change` → trimmed string (not overwritten while focused) |
| `file` | text `.delay-input` (`min-width:10rem`, placeholder `name.<ext>` or `/path/to/file`) + `.mini-btn` Browse (`backend.file.browse`; shown only when `rendererIsLocal`, from `invoke('renderer_is_local')`; click → `pick_backend_file_path {extensions}` native dialog then send) + optional `.mini-btn` Edit (`backend.file.edit`, when `kind.editable`) → `openBackendFileEditor(...)` (**script editor, out of scope**, `controls/script-editor.js`) | `change` → trimmed string |
| `int` / `float` (default) | range `min=kind.min??0 max=kind.max??1 step=(int?1:kind.step??0.01)` + `.val` readout (`inline-block; min-width:3.5em; text-align:right`, raw value string) | `input` → readout only; `change` → `sendBackendParam(key, int ? round : Number)` |

`sendBackendParam(key, value, backend)` → `invoke('control_backend_param',
{key, value, backend?})` → OSC `/omniphony/control/backend/param
[String backend?, String key, Bool|Float|String value]`
(`commands/render.rs:287-314`; a JSON number becomes an OSC Float). No
optimistic write: the renderer echoes `backendParamValuesById`. No
recompute marking either.

#### 3.6.2 Hybrid backend (`#hybridSection`, `renderer-panel.js:407-451`)

Shown only when the visible backend is `hybrid`. Header: `.title-with-info`
with `hybrid.title` "Hybrid backend" and `#hybridInfoBtn` (title
`hybrid.infoButton`) — **this button has no listener anywhere** (only
defined in the markup), so it is inert.

`#hybridParamTabs` (`renderHybridParamTabs`, `vbap.js:716-747`): a row of
buttons, hidden unless `selection === 'hybrid'`. Tabs = `['hybrid',
...distinct(externalBackend ?? 'vbap', internalBackend ?? 'barycenter')]`.
Text: `t('hybrid.tabMix')` ("Mix") for `hybrid`, else `backendLabel(id)`.
Style: `padding 0.2rem 0.6rem; font-size 11px; radius 6px 6px 0 0; border
1px rgba(255,255,255,0.14) (no bottom); color #fff`; active →
`background rgba(255,255,255,0.12); font-weight 600`. Click →
`app.hybridParamTab = id` and re-render. The active tab is coerced to the
first tab when it is no longer in the list.

- Tab `hybrid` shows `#hybridConfigPanel`; other tabs show the generic
  params of that inner backend (edits are addressed with the `backend`
  argument, so an inner backend is tuned independently).

`#hybridConfigPanel` body (indented box like the subpanel bodies):

| Element | Type | Label / help | Value (`app.renderBackendState.hybrid.*` ← `AppState.render_backend_state.hybrid`) | On change |
|---|---|---|---|---|
| `#hybridExternalBackendSelect.delay-input` | select, options = available backends minus `hybrid` (`syncBackendOptions(..., {'hybrid'})`); empty until the list arrives | `hybrid.external` "External backend (ratio = 1)" / `help.hybrid.external` | `externalBackend` | `change`: valid if non-empty, ≠ `hybrid` and (no list yet or in list); optimistic; `markRecomputePending()`; `control_hybrid_external_backend` → `/omniphony/control/hybrid/external_backend` string |
| `#hybridInternalBackendSelect` | same | `hybrid.internal` "Internal backend (ratio = 0)" / `help.hybrid.internal` | `internalBackend` | `control_hybrid_internal_backend` → `.../hybrid/internal_backend` |
| `#hybridMetricSelect` | select `chebyshev` (`distance.metric.chebyshev`), `spherical` (`distance.metric.spherical`) | `distance.metric` "Distance metric" / `help.hybrid.metric` | `metric` (default `chebyshev`) | optimistic; recompute; `control_hybrid_metric` → `.../hybrid/metric` |
| `#hybridCurveSmoothingSlider.gain-slider` + `#hybridCurveSmoothingVal` | range `0..1 step 0.01`, baked 0; label `hybrid.smoothing` "Curve smoothing" / `help.hybrid.smoothing`; value `formatNumber(v,2)` | — | `curveSmoothing` (clamped 0..1) | `input` → local redraw only; `change` → recompute + `control_hybrid_curve_smoothing` → `.../hybrid/curve_smoothing` float 0..1 |
| hint | 11 px `#b8b8b8` text `hybrid.curveHint` | — | — | — |
| `#hybridCurveCanvas` | 320×180 canvas (`width:100%; height:180px; bg rgba(0,0,0,0.25); border 1px rgba(255,255,255,0.12); radius 6px; cursor:crosshair`) | — | `curve` (`[[x,y],...]`, sanitised by the host to the unit square, `app_state.rs:196-259`) | **Curve editor = out of scope** (`controls/hybrid-curve.js`). Contract for later: drag a point (endpoints locked at x=0/x=1, interior points kept strictly between neighbours ±1e-3), double-click empty = add, double-click a point = remove (not endpoints), Delete/Backspace removes the selected point; every edit sets the curve, `markRecomputePending()`, and sends `control_hybrid_curve {points}` (debounced 60 ms while dragging, immediate on release) → `/omniphony/control/hybrid/curve` flat floats clamped 0..1. The drawn curve is sampled by the host (`sample_hybrid_curve`, 96 samples) so the preview matches the renderer. Selecting a point also shows an iso-distance shape in the 3D scene |
| `#hybridPointEditor` (`display:none` → `flex` when a point is selected) | `hybrid.selectedPoint` "Point" (`help.hybrid.selectedPoint`), `<label>` `hybrid.pointDistance` "d" + `#hybridPointXInput` number step 0.01 width 4.5rem, `hybrid.pointRatio` "ratio" + `#hybridPointYInput` number `0..1 step 0.01` | — | selected point: X shown as `norm × maxDistance` (`maxDistance = √3` for spherical metric, 1 for chebyshev), `toFixed(3)`; X disabled for endpoints | `change` → commit (immediate send) |

#### 3.6.3 Frozen layout side effect

`renderRenderBackend` also disables `#layoutSelect` (absent in the DOM, see
§4.4), `#importLayoutBtn` and `#exportLayoutBtn` when `frozenSpeakers`
(`vbap.js:662-670`).

### 3.7 Distance Diffuse (`#distanceDiffuseSection`, `renderer-panel.js:455-500`)

Bar: `.title-with-info` `distance.title` "Distance Diffuse" +
`#distanceDiffuseInfoBtn` (promoted → the title opens `#distanceDiffuseInfoModal`,
title `distance.infoTitle`, body `distance.infoBody`); actions: the master
switch `#distanceDiffuseToggle` (a `.renderer-subpanel-actions` checkbox →
pill).

`#distanceDiffuseParams.conditional-params` gets `.open` iff
`app.distanceDiffuseState.enabled === true` (`distance-diffuse.js:53-55`),
i.e. the parameters collapse when the effect is off.

| Element | Type | Label / help | Value (`app.distanceDiffuseState.*` ← `AppState.distance_diffuse` + `mirrorAxes` from the snapshot) | On change |
|---|---|---|---|---|
| `#distanceDiffuseToggle` | switch | — | `enabled === true` | optimistic; `control_distance_diffuse_enabled {enable}` → `/omniphony/control/distance_diffuse/enabled` int (no recompute mark) |
| `#distanceDiffuseMetricSelect.delay-input` | select `spherical`, `chebyshev` | `distance.metric` / `help.distanceDiffuse.metric` | `metric` (default `spherical`) | optimistic; `markRecomputePending()`; `control_distance_diffuse_metric` → `.../distance_diffuse/metric` (validated `spherical|chebyshev`) |
| `#distanceDiffuseSymmetry` (11 px `#8fa6bd`, right of label `distance.mirrorAxes` "Mirror axes" / `help.distanceDiffuse.mirrorAxes`) | text | — | `t(symmetryI18nKey(mirrorAxes))` (`distance-diffuse.js:23-32`): 0 flips → `distance.symmetry.none`; 1 → `distance.symmetry.plane{X|Y|Z}`; 2 → `distance.symmetry.axis{untouched axis}`; 3 → `distance.symmetry.origin`. The element's `data-i18n` is rewritten so locale changes re-resolve it | read-only |
| `#distanceDiffuseMirrorX` / `Y` / `Z` | three switches (`.switch-row`, 0.7 rem `#8fa6bd`), baked X and Y checked | `distance.mirrorAxis.x` "X — left / right", `.y` "Y — front / back", `.z` "Z — up / down" | `mirrorAxes.{x,y,z}` (JS default `{x:true,y:true,z:false}`) | optimistic; recompute; `control_distance_diffuse_mirror_axes {value}` where value = the enabled letters joined (`'xy'`, `'y'`, …) or `'none'` → `.../distance_diffuse/mirror_axes` string |
| `#distanceDiffuseThresholdSlider.gain-slider` + `#distanceDiffuseThresholdVal` | range `0.1..2.0 step 0.01`, baked 1.0; label `distance.threshold` "Threshold" / `help.distanceDiffuse.threshold`; value `formatNumber(v,2)` or `—` | — | `threshold` | `input` (live) → optimistic + `control_distance_diffuse_threshold` → `.../distance_diffuse/threshold` float ≥ 0.01 |
| `#distanceDiffuseCurveSlider` + `#distanceDiffuseCurveVal` | range `0.5..2.0 step 0.05`, baked 1.0; label `distance.curve` "Curve" / `help.distanceDiffuse.curve` | — | `curve` | `input` → `control_distance_diffuse_curve` → `.../distance_diffuse/curve` float ≥ 0 |

### 3.8 Distance model (`#distanceModelSection`, `renderer-panel.js:501-525`)

Bar: `.title-with-info` `distance.model` "Distance model" +
`#distanceModelInfoBtn` (promoted → opens `#distanceModelInfoModal`, title
`distance.modelInfoTitle`, body `distance.modelInfoBody`); action
`#distanceModelSelect.delay-input` (`min-width:10.5rem`): `none`
(`distance.model.none`), `linear`, `quadratic`, `inverse-square`
(`distance.model.inverseSquare`). Value `app.distanceModel` if in the set
else `none` (snapshot `distanceModel` — either a string or
`{value, metric}`; `AppState.distance_model: {value, metric}`). Change →
optimistic, `markRecomputePending()`, `control_distance_model` →
`/omniphony/control/distance_model` string (validated).

Body `#distanceModelMetricRow` (hidden when `app.distanceModel === 'none'`,
`master.js:218-222`): `#distanceModelMetricSelect` `spherical`/`chebyshev`,
label `distance.metric` / `help.distanceModel.metric`; value
`app.distanceModelMetric` (default `spherical`); change → optimistic,
recompute, `control_distance_model_metric` → `/omniphony/control/distance_model_metric`.

### 3.9 Items the brief lists under "renderer panel" that live elsewhere

| Item | Where | Command / event |
|---|---|---|
| Loudness | left overlay DRC section (§2.5) | `control_loudness` |
| Metering rate | left overlay Objects header (§2.5) | `control_metering_rate_hz` |
| Diag rate / publication | inside the diag plot controls (§2.3) | `control_diag_rate_hz`, `control_diag_publication_enabled` |
| Log level | `#logLevelSelect` in `#logOverlay` (`index.html:17-24`, options `off,error,warn,info,debug,trace` with keys `log.levelOption.*`); change → `control_log_level` → `/omniphony/control/log_level` (`setup-listeners.js:94-104`); state ← event `state:log_level`, snapshot `logLevel` | — |
| Bridge path | left overlay Audio Input section (`#oscBridgePathInput`, `listeners/input-panel-listeners.js:73,190`) | `control_render_bridge_path` → `/omniphony/control/render/bridge_path`; state `app.renderBridgePath` ← event `render:bridge_path`, snapshot `renderBridgePath` |
| Input pipe | same section (`#pipeStatus`, `controls/input.js:298-301`) | `control_render_input_pipe` → `/omniphony/control/render/input_pipe`; ← event `state:input_pipe`, snapshot `orenderInputPipe` |
| Config save / reload | save footer (§5) | `control_save_config`, `control_reload_config` |
| Config status / path | About modal `#aboutConfigPath`, `#aboutRendererVersion` (`controls/config.js:55-108`): path in red `#ff7676` with `about.configMissing` / `about.configParseError` when `renderConfigStatus` is `missing`/`parse_error`; `about.configDefaults` in `#ffb347` when connected with no path | ← events `render:config_path`, `render:config_status`, `render:version`, `render:executable`, `render:abi` |

---
