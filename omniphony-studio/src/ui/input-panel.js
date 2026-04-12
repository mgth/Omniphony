import { panelHeader, primaryButton, secondaryButton } from './ui-primitives.js';

export function inputPanelMarkup() {
  return `
      <div id="inputPanelRoot">
      <div class="info-section" id="audioInputSection">
        ${panelHeader({
          titleKey: 'section.audioInput',
          titleText: 'Audio Input',
          summaryId: 'inputSummary',
          summaryText: 'Pipe bridge • active Pipe bridge • pipe',
          toggleId: 'inputSectionToggleBtn'
        })}
        <div id="inputSectionContent" class="conditional-params">
        <div class="input-panel-shell">
          <div id="inputStatusInfo" class="input-panel-status">requested Pipe bridge • active Pipe bridge • —</div>
          <div class="input-panel-grid">
            <div class="input-panel-row">
              <label for="inputModeSelect" data-i18n="input.mode">Mode</label>
              <select id="inputModeSelect" class="delay-input">
                <option value="pipe_bridge" data-i18n="input.mode.pipe_bridge">Pipe bridge</option>
                <option value="pipewire" data-i18n="input.mode.pipewire">PipeWire</option>
                <option value="pipewire_bridge" data-i18n="input.mode.pipewire_bridge">PipeWire bridge</option>
              </select>
            </div>
          </div>
          <div id="inputBridgeFields" class="input-panel-stack">
            <div class="input-panel-subtitle" data-i18n="input.bridgeInput">Bridge Input</div>
            <div class="input-panel-row">
              <label for="oscBridgePathInput" data-i18n="input.bridgeBinary">Bridge</label>
              <div style="display:flex;align-items:center;gap:0.4rem;min-width:0">
                <input id="oscBridgePathInput" type="text" value="" spellcheck="false" placeholder="Auto-detect" data-i18n-placeholder="input.autoDetect" class="delay-input" style="min-width:0;flex:1 1 auto" />
                ${secondaryButton({ id: 'oscBridgeBrowseBtn', text: 'Browse', textKey: 'input.browse' })}
              </div>
            </div>
            <div id="oscBridgePathStatus" class="input-panel-inline-status" aria-live="polite"></div>
            <div class="input-panel-row">
              <label data-i18n="input.pipe">Pipe</label>
              <input id="pipeStatus" class="delay-input" type="text" spellcheck="false" placeholder="Auto-detect" data-i18n-placeholder="input.autoDetect" style="width:100%;min-width:0;box-sizing:border-box;text-align:left" />
            </div>
          </div>
          <div id="inputLiveFields" class="input-panel-stack">
            <div class="input-panel-subtitle" data-i18n="input.liveSource">Live Source</div>
            <div class="input-panel-row">
              <label for="inputBackendSelect" data-i18n="input.backend">Backend</label>
              <select id="inputBackendSelect" class="delay-input">
                <option value="pipewire" data-i18n="input.backend.pipewire">PipeWire</option>
                <option value="asio" data-i18n="input.backend.asio">ASIO</option>
              </select>
            </div>
            <div class="input-panel-row">
              <label for="inputNodeInput" data-i18n="input.node">Node</label>
              <input id="inputNodeInput" class="delay-input" type="text" placeholder="omniphony_input_7_1" />
            </div>
            <div class="input-panel-row">
              <label for="inputDescriptionInput" data-i18n="input.description">Description</label>
              <input id="inputDescriptionInput" class="delay-input" type="text" placeholder="Omniphony Input 7.1" />
            </div>
            <div class="input-panel-row">
              <label for="inputClockModeSelect" data-i18n="input.clock">Clock</label>
              <select id="inputClockModeSelect" class="delay-input">
                <option value="dac" data-i18n="input.clock.dac">DAC</option>
                <option value="pipewire" data-i18n="input.clock.pipewire">PipeWire</option>
                <option value="upstream" data-i18n="input.clock.upstream">Upstream</option>
              </select>
            </div>
            <div class="input-panel-row">
              <label for="inputLayoutInput" data-i18n="input.layout">Layout</label>
              <div style="display:flex;align-items:center;gap:0.4rem;min-width:0">
                <input id="inputLayoutInput" class="delay-input" type="text" placeholder="No imported layout" data-i18n-placeholder="input.noImportedLayout" readonly style="min-width:0;flex:1 1 auto" />
                ${secondaryButton({ id: 'inputLayoutBrowseBtn', text: 'Import', textKey: 'input.import' })}
              </div>
            </div>
            <div class="input-panel-inline-grid">
              <div class="input-panel-field">
                <label for="inputChannelsInput" class="input-panel-inline-label" data-i18n="input.channels">Channels</label>
                <input id="inputChannelsInput" class="delay-input" type="number" min="1" step="1" value="8" />
              </div>
              <div class="input-panel-field">
                <label for="inputSampleRateInput" class="input-panel-inline-label" data-i18n="audio.sampleRate">Sample rate</label>
                <input id="inputSampleRateInput" class="delay-input" type="number" min="1" step="1" value="48000" />
              </div>
            </div>
            <div class="input-panel-triple-grid">
              <div class="input-panel-field">
                <label for="inputFormatSelect" class="input-panel-inline-label" data-i18n="input.format">Format</label>
                <select id="inputFormatSelect" class="delay-input">
                  <option value="f32">f32</option>
                  <option value="s16">s16</option>
                </select>
              </div>
              <div class="input-panel-field">
                <label for="inputMapSelect" class="input-panel-inline-label" data-i18n="input.map">Map</label>
                <select id="inputMapSelect" class="delay-input">
                  <option value="7.1-fixed">7.1 fixed</option>
                </select>
              </div>
              <div class="input-panel-field">
                <label for="inputLfeModeSelect" class="input-panel-inline-label" data-i18n="input.lfe">LFE</label>
                <select id="inputLfeModeSelect" class="delay-input">
                  <option value="object" data-i18n="input.lfe.object">Object</option>
                  <option value="direct" data-i18n="input.lfe.direct">Direct</option>
                  <option value="drop" data-i18n="input.lfe.drop">Drop</option>
                </select>
              </div>
            </div>
          </div>
          <div class="input-panel-actions">
            ${secondaryButton({ id: 'inputRefreshBtn', text: 'Refresh', textKey: 'input.refresh' })}
            ${primaryButton({ id: 'inputApplyBtn', text: 'Apply', textKey: 'input.apply' })}
          </div>
        </div>
        </div>
      </div>
      </div>`;
}

export function mountInputPanel() {
  const mountEl = document.getElementById('audioInputPanelMount');
  if (!mountEl) {
    return;
  }
  mountEl.outerHTML = inputPanelMarkup();
}
