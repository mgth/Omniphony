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
          <div style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
            <div id="inputStatusInfo" class="input-panel-status">requested Pipe bridge • active Pipe bridge • —</div>
            <button id="inputInfoBtn" type="button" class="info-icon-btn" data-i18n-title="input.infoButton" title="Audio input info">i</button>
          </div>
          <div class="input-panel-grid">
            <div class="input-panel-row">
              <label for="inputModeSelect" data-i18n="input.mode" data-help-i18n="help.input.mode">Mode</label>
              <select id="inputModeSelect" class="delay-input">
                <option value="pipe_bridge" data-i18n="input.mode.pipe_bridge">Pipe bridge</option>
                <option value="pipewire_bridge" data-i18n="input.mode.pipewire_bridge">PipeWire bridge</option>
              </select>
            </div>
          </div>
          <div id="inputBridgeFields" class="input-panel-stack">
            <div class="input-panel-subtitle" data-i18n="input.bridgeInput">Bridge Input</div>
            <div class="input-panel-row">
              <label for="oscBridgePathInput" data-i18n="input.bridgeBinary" data-help-i18n="help.input.bridge">Bridge</label>
              <div style="display:flex;align-items:center;gap:0.4rem;min-width:0">
                <input id="oscBridgePathInput" type="text" value="" spellcheck="false" placeholder="Auto-detect" data-i18n-placeholder="input.autoDetect" class="delay-input" style="min-width:0;flex:1 1 auto" />
                ${secondaryButton({ id: 'oscBridgeBrowseBtn', text: 'Browse', textKey: 'input.browse' })}
              </div>
            </div>
            <div id="oscBridgePathStatus" class="input-panel-inline-status" aria-live="polite"></div>
            <div class="input-panel-row">
              <label data-i18n="input.pipe" data-help-i18n="help.input.pipe">Pipe</label>
              <input id="pipeStatus" class="delay-input" type="text" spellcheck="false" placeholder="Auto-detect" data-i18n-placeholder="input.autoDetect" style="width:100%;min-width:0;box-sizing:border-box;text-align:left" />
            </div>
          </div>
          <div id="inputLiveFields" class="input-panel-stack">
            <div class="input-panel-subtitle" data-i18n="input.liveSource">Live Source</div>
            <div class="input-panel-row">
              <label for="inputBackendSelect" data-i18n="input.backend" data-help-i18n="help.input.backend">Backend</label>
              <select id="inputBackendSelect" class="delay-input">
                <option value="pipewire" data-i18n="input.backend.pipewire">PipeWire</option>
                <option value="asio" data-i18n="input.backend.asio">ASIO</option>
              </select>
            </div>
            <div class="input-panel-row">
              <label for="inputNodeInput" data-i18n="input.node" data-help-i18n="help.input.node">Node</label>
              <input id="inputNodeInput" class="delay-input" type="text" placeholder="omniphony" />
            </div>
            <div class="input-panel-row">
              <label for="inputDescriptionInput" data-i18n="input.description" data-help-i18n="help.input.description">Description</label>
              <input id="inputDescriptionInput" class="delay-input" type="text" placeholder="Omniphony Bridge Input" />
            </div>
            <div class="input-panel-row">
              <div class="title-with-info">
                <label for="inputClockModeSelect" data-i18n="input.clock">Clock</label>
                <button id="inputClockInfoBtn" type="button" class="info-icon-btn" data-i18n-title="input.clockInfoButton" title="Input clock info">i</button>
              </div>
              <select id="inputClockModeSelect" class="delay-input">
                <option value="dac" data-i18n="input.clock.dac">DAC</option>
                <option value="pipewire" data-i18n="input.clock.pipewire">PipeWire</option>
                <option value="upstream" data-i18n="input.clock.upstream">Upstream (advanced)</option>
              </select>
            </div>
            <div class="input-panel-row">
              <label for="inputLayoutInput" data-i18n="input.layout" data-help-i18n="help.input.layout">Layout</label>
              <div style="display:flex;align-items:center;gap:0.4rem;min-width:0">
                <input id="inputLayoutInput" class="delay-input" type="text" placeholder="No imported layout" data-i18n-placeholder="input.noImportedLayout" readonly style="min-width:0;flex:1 1 auto" />
                ${secondaryButton({ id: 'inputLayoutBrowseBtn', text: 'Import', textKey: 'input.import' })}
              </div>
            </div>
            <div class="input-panel-inline-grid">
              <div class="input-panel-field">
                <label for="inputChannelsInput" class="input-panel-inline-label" data-i18n="input.channels" data-help-i18n="help.input.channels" data-help-anchor=".input-panel-inline-grid">Channels</label>
                <input id="inputChannelsInput" class="delay-input" type="number" min="1" step="1" value="2" />
              </div>
              <div class="input-panel-field">
                <label for="inputSampleRateInput" class="input-panel-inline-label" data-i18n="audio.sampleRate" data-help-i18n="help.input.sampleRate" data-help-anchor=".input-panel-inline-grid">Sample rate</label>
                <input id="inputSampleRateInput" class="delay-input" type="number" min="1" step="1" value="192000" />
              </div>
            </div>
            <div class="input-panel-triple-grid">
              <div class="input-panel-field">
                <label for="inputMapSelect" class="input-panel-inline-label" data-i18n="input.map" data-help-i18n="help.input.map" data-help-anchor=".input-panel-triple-grid">Map</label>
                <select id="inputMapSelect" class="delay-input">
                  <option value="7.1-fixed" data-i18n="input.map.sevenOneFixed">7.1 fixed</option>
                </select>
              </div>
              <div class="input-panel-field">
                <div class="title-with-info">
                  <label for="inputLfeModeSelect" class="input-panel-inline-label" data-i18n="input.lfe">LFE</label>
                  <button id="inputLfeInfoBtn" type="button" class="info-icon-btn" data-i18n-title="input.lfeInfoButton" title="Input LFE info">i</button>
                </div>
                <select id="inputLfeModeSelect" class="delay-input">
                  <option value="object" data-i18n="input.lfe.object">Object</option>
                  <option value="direct" data-i18n="input.lfe.direct">Direct</option>
                  <option value="drop" data-i18n="input.lfe.drop">Drop</option>
                </select>
              </div>
            </div>
        </div>
          <div class="input-panel-actions">
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
