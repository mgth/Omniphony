import { panelHeader, primaryButton, secondaryButton } from './ui-primitives.js';

export function audioPanelMarkup() {
  return `
      <div id="audioPanelRoot">
      <div class="info-section" id="audioOutputSection">
        ${panelHeader({
          titleKey: 'section.audioOutput',
          titleText: 'Audio Output',
          summaryId: 'audioOutputSummary',
          summaryText: '—',
          toggleId: 'audioOutputSectionToggleBtn'
        })}
        <div id="audioOutputSectionContent" class="conditional-params">
          <div id="audioFormatInfo">audio: — / —</div>
          <div class="control-row" style="margin-top:0.3rem;grid-template-columns:auto minmax(0, 1fr)">
            <label for="audioOutputBackendSelect" style="font-size:12px;white-space:nowrap" data-i18n="audio.outputBackend" data-help-i18n="help.audio.outputBackend">Output</label>
            <div style="display:flex;align-items:center;gap:0.3rem;min-width:0">
              <select id="audioOutputBackendSelect" class="delay-input" style="flex:1 1 auto;min-width:0">
                <option value="device" data-i18n="audio.backendDevice">Device</option>
                <option value="file" data-i18n="audio.backendFile">File / Stream</option>
              </select>
            </div>
          </div>
          <div id="audioOutputDeviceRow" class="control-row" style="margin-top:0.3rem;grid-template-columns:auto minmax(0, 1fr)">
            <label for="audioOutputDeviceSelect" style="font-size:12px;white-space:nowrap" data-i18n="audio.outputDevice" data-help-i18n="help.audio.outputDevice">Output device</label>
            <div style="display:flex;align-items:center;gap:0.3rem;min-width:0">
              <select id="audioOutputDeviceSelect" class="delay-input" style="flex:1 1 auto;min-width:0">
                <option value="">Default</option>
              </select>
              ${secondaryButton({ id: 'refreshOutputDevicesBtn', text: '↺', title: 'Refresh device list', titleKey: 'audio.refreshDevices', compact: true, extraClass: 'audio-device-refresh-btn' })}
            </div>
          </div>
          <div id="outputChannelMappingRow" class="control-row" style="margin-top:0.3rem;grid-template-columns:auto minmax(0, 1fr)">
            <label style="font-size:12px;white-space:nowrap" data-i18n="audio.channelMapping" data-help-i18n="help.audio.channelMapping">Channel mapping</label>
            <span style="display:flex;gap:0.25rem;justify-self:end">
              <button id="outputChannelMappingByIndex" type="button" class="toggle-btn active" data-option="output_channel_mapping" data-option-value="by_index" data-i18n="audio.channelMapping.byIndex">By index</button>
              <button id="outputChannelMappingByName" type="button" class="toggle-btn" data-option="output_channel_mapping" data-option-value="by_name" data-i18n="audio.channelMapping.byName">By name</button>
            </span>
          </div>
          <div id="outputChannelMappingWarning" style="display:none;margin-top:0.2rem;font-size:11px;color:#ffb24d"></div>
          <div id="audioOutputPipeRow" class="switch-row" style="display:none;margin-top:0.3rem">
            <span style="font-size:12px;color:#ffffff" data-i18n="audio.namedPipe" data-help-i18n="help.audio.namedPipe" data-help-anchor=".switch-row">Named pipe</span>
            <input id="audioOutputPipeToggle" type="checkbox" />
          </div>
          <div id="audioOutputFileRow" class="control-row" style="display:none;margin-top:0.3rem;grid-template-columns:auto minmax(0, 1fr)">
            <label for="audioOutputFileInput" style="font-size:12px;white-space:nowrap" data-i18n="audio.outputFile" data-help-i18n="help.audio.outputFile">Destination</label>
            <div style="display:flex;align-items:center;gap:0.3rem;min-width:0">
              <input id="audioOutputFileInput" class="delay-input" type="text" placeholder="/path/to/fifo" style="flex:1 1 auto;min-width:0" />
            </div>
          </div>
          <div id="audioOutputFileFormatRow" class="control-row" style="display:none;margin-top:0.3rem;grid-template-columns:auto minmax(0, 1fr)">
            <label for="audioOutputFileFormatSelect" style="font-size:12px;white-space:nowrap" data-i18n="audio.outputFileFormat" data-help-i18n="help.audio.outputFileFormat">Format</label>
            <div style="display:flex;align-items:center;gap:0.3rem;min-width:0">
              <select id="audioOutputFileFormatSelect" class="delay-input" style="flex:1 1 auto;min-width:0">
                <option value="raw_f32" data-i18n="audio.formatRawF32">Raw f32 (LE)</option>
                <option value="caf" data-i18n="audio.formatCaf">CAF (float)</option>
              </select>
            </div>
          </div>
          <div class="control-row" style="margin-top:0.3rem;grid-template-columns:auto 1fr">
            <label for="audioSampleRateInput" style="font-size:12px;white-space:nowrap" data-i18n="audio.sampleRate" data-help-i18n="help.audio.sampleRate">Sample rate</label>
            <div id="audioSampleRateControl" style="position:relative;display:flex;align-items:center;gap:0.2rem;flex:1 1 auto;min-width:0">
              <input id="audioSampleRateInput" class="delay-input" type="text" inputmode="numeric" value="0" style="flex:1 1 auto;min-width:0" />
              ${secondaryButton({ id: 'audioSampleRateMenuBtn', text: '▾', compact: true })}
              <div id="audioSampleRateMenu" style="position:absolute;left:0;right:0;top:calc(100% + 0.2rem);display:none;z-index:20;background:rgba(10,11,16,0.96);border:1px solid rgba(255,255,255,0.2);border-radius:8px;padding:0.2rem;max-height:180px;overflow:auto"></div>
            </div>
          </div>
        </div>
      </div>
      <div class="info-section" id="latencySection">
        <div style="display:flex;align-items:flex-start;justify-content:space-between;gap:0.5rem">
          <div style="display:grid;grid-template-columns:auto minmax(0,1fr);grid-template-rows:auto auto auto;column-gap:0.5rem;row-gap:0.18rem;align-items:start;min-width:0;flex:1 1 auto">
            <div class="info-title" style="margin:0;grid-column:1;grid-row:1" data-i18n="section.latency">Latency</div>
            <div class="meter-bar" style="grid-column:2;grid-row:1;align-self:center;overflow:visible">
              <div id="latencyMeterFill" class="meter-fill latency"></div>
              <div id="latencyRawMinMask" class="meter-range-mask" style="left:0;width:0;display:none"></div>
              <div id="latencyRawMaxMask" class="meter-range-mask" style="right:0;width:0;display:none"></div>
              <div id="latencyTargetMarker" class="meter-marker" style="display:none;background:#52e2a2;top:-11px;bottom:auto;height:5px;width:5px;border-radius:50%"></div>
              <div id="latencyNearLowMarker" class="meter-marker" style="display:none;background:#4ad6ff;top:-11px;bottom:auto;height:5px;width:5px;border-radius:50%"></div>
              <div id="latencyLowExitMarker" class="meter-marker" style="display:none;background:#c08bff;top:-11px;bottom:auto;height:5px;width:5px;border-radius:50%"></div>
              <div id="latencyNearHighMarker" class="meter-marker" style="display:none;background:#ffb84a;top:-11px;bottom:auto;height:5px;width:5px;border-radius:50%"></div>
              <div id="latencyRawMinMarker" class="meter-marker min"></div>
              <div id="latencyCtrlMarker" class="meter-marker" style="background:#58a0ff;top:-4px;bottom:-4px"></div>
              <div id="latencySmoothedMarker" class="meter-marker" style="display:none;background:#c879ff;top:-4px;bottom:-4px"></div>
              <div id="latencyRawMaxMarker" class="meter-marker max"></div>
            </div>
            <div style="grid-column:2;grid-row:2;min-width:0">
              <div class="meter-subvalues">
                <span id="latencyRawMinValue">min —</span>
                <span aria-hidden="true" style="opacity:0.45">|</span>
                <span id="latencyRawInfo">—</span>
                <span aria-hidden="true" style="opacity:0.45">|</span>
                <span id="latencyRawMaxValue">max —</span>
              </div>
              <div class="meter-subvalues" style="margin-top:0.12rem">
                <span id="latencyCtrlInfo">ctrl —</span>
                <span aria-hidden="true" style="opacity:0.45">|</span>
                <span id="latencySmoothedInfo">smoothed —</span>
                <span aria-hidden="true" style="opacity:0.45">|</span>
                <span id="latencyDownstreamInfo">path —</span>
              </div>
            </div>
            <div id="resampleMeterLabel" class="meter-mini-label" style="grid-column:1;grid-row:3;transform:translateY(-2px)" data-i18n="telemetry.resample">Resample</div>
            <div id="resampleMeterBody" style="grid-column:2;grid-row:3;display:grid;gap:0.05rem;min-width:0">
              <div class="meter-bar resample-meter-shell">
                <div class="resample-meter-center"></div>
                <div id="resampleNegMeterFill" class="meter-fill resample-neg"></div>
                <div id="resamplePosMeterFill" class="meter-fill resample-pos"></div>
                <div id="resampleNegNearMarker" class="meter-marker min" style="background:#ffd54a"></div>
                <div id="resamplePosNearMarker" class="meter-marker min" style="background:#ffd54a"></div>
              </div>
              <div id="resampleRatioInfo" style="font-size:10px;color:#b9c7d8;font-family:ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, 'Liberation Mono', 'Courier New', monospace;text-align:center">
                —
              </div>
            </div>
          </div>
          <div style="display:flex;align-items:center;gap:0.35rem;justify-content:flex-end;flex:0 0 auto">
            <button id="resamplePlotToggleBtn" type="button" class="info-icon-btn" data-i18n-title="telemetry.plotToggle" title="Toggle resample plot" aria-pressed="false"><svg width="14" height="14" viewBox="0 0 16 16" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" fill="none" aria-hidden="true"><polyline points="2,12 5,8 8,10 11,4 14,7"/></svg></button>
            <button id="telemetryGaugesInfoBtn" type="button" class="info-icon-btn" data-i18n-title="telemetry.infoButton" title="Latency panel info">i</button>
            <button id="telemetryGaugesToggleBtn" type="button" class="panel-toggle-btn" data-i18n-title="telemetry.toggle" title="Show latency controls">▸</button>
          </div>
        </div>
        <div id="resamplePlotContainer" style="display:none;margin-top:0.35rem">
          <canvas id="resamplePlotCanvas" width="600" height="140" style="display:block;width:100%;height:auto;border-radius:8px"></canvas>
        </div>
        <div id="telemetryGaugesForm" class="telemetry-gauges-form">
          <div class="control-row" style="margin-top:0;grid-template-columns:auto auto 1fr">
            <label for="latencyTargetInput" style="font-size:12px;white-space:nowrap" data-i18n="audio.targetLatency" data-help-i18n="help.audio.targetLatency">Target latency</label>
            <div style="display:flex;align-items:center;gap:0.35rem">
              <input id="latencyTargetInput" class="delay-input" type="number" min="1" step="1" value="500" style="width:5.5rem" />
              ${primaryButton({ id: 'latencyTargetApplyBtn', text: 'Apply', textKey: 'adaptive.apply' })}
              <span aria-hidden="true" data-i18n-title="telemetry.targetMarkerTitle" title="Latency gauge target marker" style="display:inline-block;width:0.38rem;height:0.38rem;border-radius:50%;background:#52e2a2;box-shadow:0 0 0 1px rgba(255,255,255,0.14)"></span>
            </div>
            <div style="display:flex;align-items:center;justify-content:flex-end;gap:0.35rem;min-width:0">
              <div id="adaptiveBandIndicator" style="display:flex;align-items:center;gap:0.4rem;color:#d9ecff;font-size:12px">
                <span id="adaptiveRuntimeStateText" style="font-size:10px;letter-spacing:0.04em;text-transform:uppercase;color:#8fa6bd;min-width:7.5em;text-align:right">—</span>
                <span id="adaptiveBandDot" style="width:0.6rem;height:0.6rem;border-radius:999px;background:rgba(255,255,255,0.25);display:inline-block"></span>
                <span id="adaptiveBandText">—</span>
              </div>
              <button id="adaptiveResamplingInfoBtn" type="button" class="info-icon-btn" data-i18n-title="adaptive.infoButton" title="Adaptive Resampling Info">i</button>
            </div>
          </div>
          <div id="adaptiveResamplingAdvancedForm" class="adaptive-advanced-form">
            <div class="adaptive-subpanel">
              <div class="control-row" style="margin-top:0">
                <div style="grid-column:1 / -1;font-size:10px;letter-spacing:0.08em;text-transform:uppercase;color:#8fa6bd" data-i18n="adaptive.globalActions">Global far actions</div>
              </div>
              <div class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveFarHardRecoverHighToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.hardRecoverHigh" data-help-i18n="help.adaptive.hardRecoverHigh">Hard recover high in far mode</label>
                <input id="adaptiveFarHardRecoverHighToggle" type="checkbox" />
              </div>
              <div id="adaptiveHighRecoverEntryMarginRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveHighRecoverEntryMarginInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.threshold" data-help-i18n="help.adaptive.threshold">High-recover entry margin</label>
                <div style="display:flex;align-items:center;gap:0.35rem">
                  <input id="adaptiveHighRecoverEntryMarginInput" class="delay-input" type="number" min="1" step="1" value="120" style="width:8rem" />
                  <span style="font-size:11px;color:#8fa6bd">ms</span>
                  <span id="adaptiveHighRecoverEntryMarginSymbol" aria-hidden="true" data-i18n-title="telemetry.thresholdMarkerTitle" title="Latency gauge high-recover entry marker" style="display:inline-block;width:0.38rem;height:0.38rem;border-radius:50%;background:#ffb84a;box-shadow:0 0 0 1px rgba(255,255,255,0.14)"></span>
                </div>
              </div>
              <div class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveFarHardRecoverLowToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.hardRecoverLow" data-help-i18n="help.adaptive.hardRecoverLow">Hard recover low in far mode</label>
                <input id="adaptiveFarHardRecoverLowToggle" type="checkbox" />
              </div>
              <div id="adaptiveLowRecoverEntryMarginMsRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveLowRecoverEntryMarginMsInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.lowRecoverEntryMargin" data-help-i18n="help.adaptive.lowRecoverEntryMargin">Low-recover entry margin</label>
                <div style="display:flex;align-items:center;gap:0.35rem">
                  <input id="adaptiveLowRecoverEntryMarginMsInput" class="delay-input" type="number" min="0" step="0.1" value="18" style="width:7rem" />
                  <span style="font-size:11px;color:#8fa6bd">ms</span>
                  <span id="adaptiveLowRecoverEntryMarginSymbol" aria-hidden="true" data-i18n-title="telemetry.lowThresholdMarkerTitle" title="Latency gauge low-recover entry marker" style="display:inline-block;width:0.38rem;height:0.38rem;border-radius:50%;background:#4ad6ff;box-shadow:0 0 0 1px rgba(255,255,255,0.14)"></span>
                </div>
              </div>
              <div id="adaptiveLowRecoverExitMarginMsRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveLowRecoverExitMarginMsInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.lowRecoverExitMargin" data-help-i18n="help.adaptive.lowRecoverExitMargin">Low-recover exit margin</label>
                <div style="display:flex;align-items:center;gap:0.35rem">
                  <input id="adaptiveLowRecoverExitMarginMsInput" class="delay-input" type="number" min="0" step="0.1" value="6" style="width:7rem" />
                  <span style="font-size:11px;color:#8fa6bd">ms</span>
                  <span id="adaptiveLowRecoverExitMarginSymbol" aria-hidden="true" data-i18n-title="telemetry.lowExitMarkerTitle" title="Latency gauge low-recover exit marker" style="display:inline-block;width:0.38rem;height:0.38rem;border-radius:50%;background:#c08bff;box-shadow:0 0 0 1px rgba(255,255,255,0.14)"></span>
                </div>
              </div>
              <div id="adaptiveFarSilenceRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveFarSilenceToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.silenceFar" data-help-i18n="help.adaptive.silenceFar">Silence in Far Mode</label>
                <input id="adaptiveFarSilenceToggle" type="checkbox" />
              </div>
              <div id="adaptiveFarFadeRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveFarFadeInMsInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.fadeNearReturn" data-help-i18n="help.adaptive.fadeNearReturn">Fade-In on Near Return</label>
                <input id="adaptiveFarFadeInMsInput" class="delay-input" type="number" min="0" step="1" value="0" style="width:8rem" />
              </div>
            </div>
            <div class="adaptive-subpanel">
              <div class="control-row" style="margin-top:0">
                <div style="display:flex;align-items:center;justify-content:space-between;gap:0.5rem;grid-column:1 / -1;flex-wrap:wrap">
                  <div style="font-size:10px;letter-spacing:0.08em;text-transform:uppercase;color:#8fa6bd;min-width:0;flex:1 1 auto" data-i18n="adaptive.resamplingController">Local resampling controller</div>
                  <div style="display:flex;align-items:center;gap:0.4rem;flex-shrink:0">
                    ${secondaryButton({ id: 'autoTunePiBtn', text: 'Auto-tune…', textKey: 'autoTune.openButton', titleKey: 'autoTune.openButtonTitle', title: 'Run the PI auto-tuner' })}
                    ${secondaryButton({ id: 'adaptivePauseBtn', text: '⏸ Pause' })}
                    ${secondaryButton({ id: 'adaptiveRatioResetBtn', text: 'Reset ratio', textKey: 'adaptive.resetRatio', extraClass: 'adaptive-ratio-reset-btn' })}
                  </div>
                </div>
              </div>
              <div class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveResamplingToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.title" data-help-i18n="help.adaptive.title">Adaptive resampling</label>
                <input id="adaptiveResamplingToggle" type="checkbox" />
              </div>
              <div id="adaptiveUpdateIntervalRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveUpdateIntervalCallbacksInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.updateInterval" data-help-i18n="help.adaptive.updateInterval">Update interval</label>
                <input id="adaptiveUpdateIntervalCallbacksInput" class="delay-input" type="number" min="1" step="1" value="10" style="width:8rem" />
              </div>
              <div id="adaptiveMaxAdjustRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveMaxAdjustInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.max" data-help-i18n="help.adaptive.max">Adaptive max</label>
                <div style="display:flex;align-items:center;gap:0.35rem">
                  <input id="adaptiveMaxAdjustInput" class="delay-input" type="number" min="0.001" step="1" value="10000" style="width:7rem" />
                  <span style="font-size:11px;color:#8fa6bd">ppm</span>
                  <span aria-hidden="true" data-i18n-title="telemetry.resampleMarkerTitle" title="Resample gauge near max marker" style="display:inline-block;width:2px;height:12px;border-radius:999px;background:#ffd54a;box-shadow:0 0 0 1px rgba(255,255,255,0.08)"></span>
                </div>
              </div>
              <div id="adaptiveKpNearRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveKpNearInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.kpNear" data-help-i18n="help.adaptive.kpNear">Adaptive KP</label>
                <input id="adaptiveKpNearInput" class="delay-input" type="number" min="0.001" step="0.001" value="10" style="width:8rem" />
              </div>
              <div id="adaptiveKiRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveKiInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.ki" data-help-i18n="help.adaptive.ki">Adaptive Ki</label>
                <input id="adaptiveKiInput" class="delay-input" type="number" min="0" step="0.001" value="50" style="width:8rem" />
              </div>
              <div id="adaptiveIntegralDischargeRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveIntegralDischargeRatioInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.integralDischarge" data-help-i18n="help.adaptive.integralDischarge">Integral discharge</label>
                <input id="adaptiveIntegralDischargeRatioInput" class="delay-input" type="number" min="0" max="1" step="0.001" value="0.25" style="width:8rem" />
              </div>
            </div>
            <div class="adaptive-subpanel">
              <div class="control-row" style="margin-top:0">
                <div style="grid-column:1 / -1;font-size:10px;letter-spacing:0.08em;text-transform:uppercase;color:#8fa6bd" data-i18n="adaptive.stabilizationPhases">Stabilization phases</div>
              </div>
              <div id="adaptiveLowRecoverSettleStableMsRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveLowRecoverSettleStableMsInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.lowRecoverSettleStable" data-help-i18n="help.adaptive.lowRecoverSettleStable">Settling hold</label>
                <div style="display:flex;align-items:center;gap:0.35rem">
                  <input id="adaptiveLowRecoverSettleStableMsInput" class="delay-input" type="number" min="0" step="1" value="200" style="width:7rem" />
                  <span style="font-size:11px;color:#8fa6bd">ms</span>
                </div>
              </div>
              <div id="adaptiveLowRecoverSettleMarginMsRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveLowRecoverSettleMarginMsInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.lowRecoverSettleMargin" data-help-i18n="help.adaptive.lowRecoverSettleMargin">Settling margin</label>
                <div style="display:flex;align-items:center;gap:0.35rem">
                  <input id="adaptiveLowRecoverSettleMarginMsInput" class="delay-input" type="number" min="0" step="0.1" value="6" style="width:7rem" />
                  <span style="font-size:11px;color:#8fa6bd">ms</span>
                </div>
              </div>
              <div id="adaptiveLowRecoverRefillDeltaAlphaRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveLowRecoverRefillDeltaAlphaInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.lowRecoverRefillDeltaAlpha" data-help-i18n="help.adaptive.lowRecoverRefillDeltaAlpha">Refill EMA α</label>
                <input id="adaptiveLowRecoverRefillDeltaAlphaInput" class="delay-input" type="number" min="0" max="1" step="0.01" value="0.5" style="width:8rem" />
              </div>
              <div id="adaptiveControlSmoothingCutoffRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveControlSmoothingCutoffHzInput" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.controlSmoothingCutoffHz" data-help-i18n="help.adaptive.controlSmoothingCutoffHz">IIR cutoff (Hz)</label>
                <input id="adaptiveControlSmoothingCutoffHzInput" class="delay-input" type="number" min="0.001" max="20" step="0.05" value="0.5" style="width:8rem" />
              </div>
              <div id="adaptiveControlSmoothingOrderRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveControlSmoothingOrderSelect" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.controlSmoothingOrder" data-help-i18n="help.adaptive.controlSmoothingOrder">IIR order</label>
                <select id="adaptiveControlSmoothingOrderSelect" style="font-size:11px;background:rgba(255,255,255,0.06);color:#d9ecff;border:1px solid rgba(255,255,255,0.18);border-radius:4px;padding:0.1rem 0.25rem">
                  <option value="1" data-i18n="adaptive.controlSmoothingOrder.opt1">1 (single pole, 6 dB/oct)</option>
                  <option value="2" data-i18n="adaptive.controlSmoothingOrder.opt2">2 (Butterworth, 12 dB/oct)</option>
                </select>
              </div>
              <div id="adaptiveUsePreBridgeClockRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveUsePreBridgeClockToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.usePreBridgeClock" data-help-i18n="help.adaptive.usePreBridgeClock" data-i18n-title="adaptive.usePreBridgeClockTitle" title="Feed the PI servo with the IEC958 source-clock signal (pre-decoder) instead of the post-decode ring level. The source clock is smooth by construction, so the PI reacts directly to genuine hardware drift without the decoder's batching ripple.">Pre-bridge clock (PI input)</label>
                <input id="adaptiveUsePreBridgeClockToggle" type="checkbox" />
              </div>
              <div id="adaptiveUseOutputPacingRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveUseOutputPacingToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.useOutputPacing" data-help-i18n="help.adaptive.useOutputPacing" data-i18n-title="adaptive.useOutputPacingTitle" title="Buffer rendered speaker PCM and drain it into the ring buffer in lockstep with IEC958 chunk arrival, so the ring sees a smooth flow regardless of the decoder's burst pattern. Adds ~64 ms of pre-roll latency on startup; eliminates the 3.1 Hz sawtooth at the source.">Output pacing (post-render)</label>
                <input id="adaptiveUseOutputPacingToggle" type="checkbox" />
              </div>
              <div id="adaptiveDisableBackpressureRow" class="control-row" style="margin-top:0.2rem">
                <label for="adaptiveDisableBackpressureToggle" style="font-size:12px;white-space:nowrap" data-i18n="adaptive.disableBackpressure" data-help-i18n="help.adaptive.disableBackpressure" data-i18n-title="adaptive.disableBackpressureTitle" title="Diagnostic: stop blocking the renderer when the output buffer is full — push what fits and drop the overflow instead of waiting. This unhooks the source (mpv) from the DAC drain clock, removing the back-pressure relaxation sawtooth at the cost of dropped samples on overflow. Leave off for normal playback.">Disable back-pressure (diag)</label>
                <input id="adaptiveDisableBackpressureToggle" type="checkbox" />
              </div>
            </div>
            <div style="margin-top:0.3rem;display:flex;justify-content:flex-end;gap:0.35rem">
              ${secondaryButton({ id: 'adaptiveResamplingAdvancedCancelBtn', text: 'Cancel', textKey: 'common.cancel' })}
              ${primaryButton({ id: 'adaptiveResamplingAdvancedApplyBtn', text: 'Apply', textKey: 'adaptive.apply' })}
            </div>
          </div>
        </div>
      </div>
      <div class="info-section" id="diagSection">
        <div style="display:flex;align-items:center;justify-content:space-between;gap:0.5rem">
          <div class="info-title" style="margin:0" data-i18n="section.diagnostics">Diagnostics</div>
          <button id="diagPlotToggleBtn" type="button" class="info-icon-btn" data-i18n-title="telemetry.diagPlotToggle" title="Toggle diagnostic-metrics plot" aria-pressed="false"><svg width="14" height="14" viewBox="0 0 16 16" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" fill="none" aria-hidden="true"><circle cx="8" cy="8" r="6"/><path d="M8 4v4l3 2"/></svg></button>
        </div>
        <div id="diagPlotContainer" style="display:none;margin-top:0.35rem">
          <div id="diagPlotControls" style="display:flex;flex-wrap:wrap;gap:0.35rem;align-items:center;margin-bottom:0.35rem;font-size:11px;color:#b9c7d8"></div>
          <canvas id="diagPlotCanvas" width="600" height="240" style="display:block;width:100%;height:auto;border-radius:8px"></canvas>
        </div>
      </div>
      <div class="info-section" id="masterSection">
        <div class="master-header">
          <div class="info-title" style="margin:0;white-space:nowrap" data-i18n="master.title" data-help-i18n="help.master.gain" data-help-anchor=".master-header">Master</div>
          <div class="meter-bar level-meter" style="flex:1 1 auto;min-width:0">
            <div id="masterMeterFill" class="meter-fill"></div>
            <div id="masterMeterPeak" class="meter-peak"></div>
          </div>
          <div id="masterMeterText" class="fixed-metric" style="text-align:right">— dB</div>
          <button id="autoGainSectionToggleBtn" type="button" class="panel-toggle-btn" data-i18n-title="autoGain.toggle" title="Auto-gain settings">▸</button>
        </div>
        <div class="control-row">
          <input id="masterGainSlider" class="gain-slider" type="range" min="0" max="2" step="0.01" value="1" />
          <div id="masterGainBox" class="gain-box">0.0 dB</div>
        </div>
        <div id="autoGainSection" class="conditional-params">
          <div class="switch-row">
            <span style="display:flex;align-items:center;gap:0.4rem;font-size:12px;color:#ffffff">
              <span id="clipIndicator" class="clip-indicator" title="Clip" aria-label="Clip indicator"></span>
              <span data-i18n="autoGain.title" data-help-i18n="help.master.autoGain" data-help-anchor=".switch-row">Auto-gain (anti-clip)</span>
            </span>
            <input id="autoGainToggle" type="checkbox" />
          </div>
          <div class="control-row" id="autoGainCeilingRow">
            <label style="font-size:12px;white-space:nowrap;color:#ffffff" for="autoGainCeilingSlider"><span data-i18n="autoGain.ceiling" data-help-i18n="help.master.ceiling">Ceiling</span> <span id="autoGainCeilingVal">-1.0 dB</span></label>
            <input id="autoGainCeilingSlider" class="gain-slider" type="range" min="-12" max="0" step="0.1" value="-1" />
          </div>
        </div>
      </div>
      </div>`;
}

export function mountAudioPanel() {
  const mountEl = document.getElementById('audioPanelMount');
  if (!mountEl) {
    return;
  }
  mountEl.outerHTML = audioPanelMarkup();
}
