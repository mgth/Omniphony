export function rendererPanelMarkup() {
  return `
      <div id="rendererPanelRoot" class="renderer-panel-root">
      <div class="info-section renderer-panel-shell" id="rendererSection">
        <div class="renderer-panel-header-block" style="display:grid;gap:0.2rem">
          <div class="panel-header">
            <div class="panel-header-main">
            <div class="info-title panel-title" data-i18n="section.renderer">Renderer</div>
            <div id="rendererPerfWrap" style="display:none;min-width:180px;flex:0 0 auto">
              <div style="display:grid;gap:0.18rem;min-width:180px">
                <div style="display:grid;grid-template-columns:180px max-content;align-items:center;gap:0.35rem">
                  <div class="meter-bar" style="width:180px;min-width:180px;overflow:visible">
                    <div id="rendererPerfDecodeFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(140,214,255,0.95), rgba(104,170,255,0.95));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfRenderFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(112,170,255,0.92), rgba(88,132,255,0.92));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfWriteFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(180,255,184,0.95), rgba(80,218,120,0.95));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfDecodeMaxMarker" class="meter-marker min" style="background:#ffd54a"></div>
                    <div id="rendererPerfRenderMaxMarker" class="meter-marker min" style="background:#ffb84a"></div>
                    <div id="rendererPerfWriteMaxMarker" class="meter-marker min" style="background:#ff8b4a"></div>
                  </div>
                  <span id="rendererPerfFrameValue" style="display:inline-block;min-width:5.4rem;text-align:right;font-size:10px;white-space:nowrap;font-variant-numeric:tabular-nums;color:#9eb4c8">frame —</span>
                </div>
                <div style="display:grid;grid-template-columns:repeat(3, max-content);align-items:center;gap:0.28rem;font-size:10px;color:#d9ecff;white-space:nowrap;font-variant-numeric:tabular-nums">
                  <span id="rendererPerfDecodeValue" style="display:inline-block;min-width:5.4rem;text-align:right">decode —</span>
                  <span id="rendererPerfRenderValue" style="display:inline-block;min-width:5.4rem;text-align:right">render —</span>
                  <span id="rendererPerfWriteValue" style="display:inline-block;min-width:5.4rem;text-align:right">write —</span>
                </div>
              </div>
            </div>
            </div>
            <button id="rendererSectionToggleBtn" type="button" class="panel-toggle-btn">▸</button>
          </div>
          <div style="display:flex;justify-content:flex-end;min-width:0">
            <div id="rendererSummary" class="panel-summary" style="display:none;flex:0 1 auto">—</div>
          </div>
        </div>
        <div id="rendererSectionContent" class="conditional-params">
          <div class="renderer-panel-stack" style="margin-top:0.25rem;display:grid;gap:0.35rem">
          <div class="info-section renderer-subpanel" id="evaluationSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff">Evaluation</div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
                <select id="renderEvaluationModeSelect" class="delay-input" style="width:auto;min-width:13rem;text-align:left">
                  <option value="auto">Auto</option>
                  <option value="realtime">Realtime</option>
                  <option value="precomputed_polar">Precomputed polar</option>
                  <option value="precomputed_cartesian">Precomputed cartesian</option>
                  <option value="from_file">From file</option>
                </select>
                <button id="exportEvaluationArtifactBtn" type="button" class="secondary-btn" style="display:none;white-space:nowrap">Export</button>
                <div id="renderEvaluationModeEffective" class="vbap-step" style="min-width:8rem;text-align:right">—</div>
              </div>
            </div>
            <div id="evaluationSectionContent" class="conditional-params open">
            <div class="renderer-subpanel-body" style="margin-top:0.25rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.18rem">
              <div id="renderEvaluationCartesianBlock">
              <div class="control-row" id="renderEvaluationCartesianRow" style="margin-top:0;grid-template-columns:1fr auto;align-items:start">
                <label style="font-size:12px;white-space:nowrap;color:#ffffff">Cartesian grid</label>
                <div style="display:flex;flex-direction:column;gap:0.15rem;align-items:stretch">
                  <div style="display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:0.15rem">
                    <input id="vbapCartXSizeInput" class="delay-input" type="number" min="1" step="1" placeholder="X" />
                    <input id="vbapCartYSizeInput" class="delay-input" type="number" min="1" step="1" placeholder="Y" />
                    <input id="vbapCartZSizeInput" class="delay-input" type="number" min="1" step="1" placeholder="Z+" />
                    <input id="vbapCartZNegSizeInput" class="delay-input" type="number" min="0" step="1" placeholder="Z-" />
                  </div>
                  <div style="display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:0.15rem">
                    <div id="vbapCartXStepInfo" class="vbap-step">—</div>
                    <div id="vbapCartYStepInfo" class="vbap-step">—</div>
                    <div id="vbapCartZStepInfo" class="vbap-step">—</div>
                    <div id="vbapCartZNegStepInfo" class="vbap-step">—</div>
                  </div>
                </div>
              </div>
              </div>
              <div id="renderEvaluationPolarBlock">
              <div class="control-row" id="renderEvaluationPolarRow" style="margin-top:0.1rem;grid-template-columns:1fr auto;align-items:start">
                <label style="font-size:12px;white-space:nowrap;color:#ffffff">Polar grid</label>
                <div style="display:flex;flex-direction:column;gap:0.15rem;align-items:stretch">
                  <div class="vbap-polar-grid">
                    <input id="vbapPolarAzimuthResolutionInput" class="delay-input" type="number" min="1" step="1" placeholder="az n" style="grid-column:1;grid-row:1" />
                    <input id="vbapPolarElevationResolutionInput" class="delay-input" type="number" min="1" step="1" placeholder="el n" style="grid-column:2;grid-row:1" />
                    <input id="vbapPolarDistanceResInput" class="delay-input" type="number" min="1" step="1" placeholder="d n" style="grid-column:3;grid-row:1" />
                    <div id="vbapAzimuthRangeInfo" class="vbap-polar-meta" style="grid-column:1;grid-row:2">-180..180</div>
                    <div id="vbapElevationRangeInfo" class="vbap-polar-meta" style="grid-column:2;grid-row:2">—</div>
                    <input id="vbapPolarDistanceMaxInput" class="delay-input" type="number" min="0.01" step="0.01" placeholder="d max" style="grid-column:3;grid-row:2" />
                  </div>
                  <div class="vbap-grid-3">
                    <div id="vbapPolarAzStepInfo" class="vbap-step">—</div>
                    <div id="vbapPolarElStepInfo" class="vbap-step">—</div>
                    <div id="vbapPolarDistStepInfo" class="vbap-step">—</div>
                  </div>
                </div>
              </div>
              </div>
            </div>
            <div class="inline-toggle" id="renderEvaluationPositionInterpolationRow" style="margin-top:0.25rem;display:flex;align-items:center;gap:0.35rem">
              <div class="title-with-info" style="min-width:0">
                <span style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="vbap.positionInterpolation">Position interpolation</span>
                <button id="vbapPositionInterpolationInfoBtn" type="button" class="info-icon-btn" data-i18n-title="vbap.positionInterpolationInfoButton" title="VBAP position interpolation info">i</button>
              </div>
              <input id="vbapPositionInterpolationToggleEl" type="checkbox" />
            </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel" id="backendParametersSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div class="renderer-subpanel-titlebar" style="display:flex;align-items:center;gap:0.45rem;min-width:0">
                <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff">Backend</div>
                <div id="vbapStatus" class="vbap-status" style="margin:0;font-size:11px;min-width:0">—</div>
              </div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
              <select id="renderBackendSelect" class="delay-input" style="width:auto;min-width:10.5rem;text-align:left">
                <option value="vbap">VBAP</option>
                <option value="experimental_distance">Distance</option>
              </select>
              <button id="restoreBackendBtn" type="button" class="secondary-btn" style="display:none;white-space:nowrap">Restore backend</button>
              <div id="renderBackendEffective" class="vbap-step" style="min-width:5.4rem;text-align:right">—</div>
            </div>
          </div>
            <div id="backendParametersSectionContent" class="conditional-params open">
          <div id="backendSpecificParamsSection">
          <div class="control-row" id="rampModeRow" style="margin-top:0.3rem;grid-template-columns:1fr auto;align-items:center">
            <div class="title-with-info" style="min-width:0;font-size:12px;font-weight:600">
              <label for="rampModeSelect" style="font-size:12px;font-weight:600;white-space:nowrap;color:#ffffff" data-i18n="audio.rampMode">Ramp mode</label>
              <button id="rampModeInfoBtn" type="button" class="info-icon-btn" data-i18n-title="rampMode.infoButton" title="Ramp mode info">i</button>
            </div>
            <select id="rampModeSelect" class="delay-input" style="min-width:9rem">
              <option value="off" data-i18n="audio.rampModeOff">Off</option>
              <option value="frame" data-i18n="audio.rampModeFrame">Per frame</option>
              <option value="sample" data-i18n="audio.rampModeSample">Per sample</option>
            </select>
          </div>
          <div class="control-row" id="distanceModelControlRow" style="margin-top:0.2rem;grid-template-columns:1fr auto;align-items:center">
            <label for="distanceModelSelect" style="font-size:12px;font-weight:600;white-space:nowrap;color:#ffffff" data-i18n="distance.model">Distance model</label>
            <select id="distanceModelSelect" class="delay-input" style="min-width:11rem">
              <option value="none" data-i18n="distance.model.none">None</option>
              <option value="linear" data-i18n="distance.model.linear">Linear</option>
              <option value="quadratic" data-i18n="distance.model.quadratic">Quadratic</option>
              <option value="inverse-square" data-i18n="distance.model.inverseSquare">Inverse-square</option>
            </select>
          </div>
          <div class="info-section" id="spreadSection" style="margin:0;padding:0;border:none;background:none">
            <div style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff" data-i18n="spread.title">Spread</div>
            </div>
            <div id="spreadSectionContent" class="conditional-params open">
            <div id="spreadInfo">spread: —° / —°</div>
            <div style="margin-top:0.2rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.15rem">
              <div class="control-row" style="margin-top:0">
                <label style="font-size:12px;white-space:nowrap"><span data-i18n="spread.min">Min</span> <span id="spreadMinVal">0°</span></label>
                <input id="spreadMinSlider" type="range" min="0" max="180" step="1" value="0" class="gain-slider" />
              </div>
              <div class="control-row" style="margin-top:0.1rem">
                <label style="font-size:12px;white-space:nowrap"><span data-i18n="spread.max">Max</span> <span id="spreadMaxVal">180°</span></label>
                <input id="spreadMaxSlider" type="range" min="0" max="180" step="1" value="180" class="gain-slider" />
              </div>
            </div>
            </div>
          </div>
          <div class="info-section" id="distanceDiffuseSection" style="margin:0;padding:0;border:none;background:none">
            <div class="title-with-info" style="font-size:12px;font-weight:600;color:#ffffff">
              <span data-i18n="distance.title">Distance Diffuse</span>
              <div class="inline-toggle" style="display:flex;align-items:center;gap:0.35rem">
                <button id="distanceDiffuseInfoBtn" type="button" class="info-icon-btn" data-i18n-title="distance.infoButton" title="Distance diffuse info">i</button>
                <input id="distanceDiffuseToggle" type="checkbox" />
              </div>
            </div>
            <div id="distanceDiffuseParams" class="conditional-params">
              <div class="control-row" style="margin-top:0.2rem">
                <label style="font-size:12px;white-space:nowrap"><span data-i18n="distance.threshold">Threshold</span> <span id="distanceDiffuseThresholdVal">1.00</span></label>
                <input id="distanceDiffuseThresholdSlider" type="range" min="0.1" max="2.0" step="0.01" value="1.0" class="gain-slider" />
              </div>
              <div class="control-row" style="margin-top:0.15rem">
                <label style="font-size:12px;white-space:nowrap"><span data-i18n="distance.curve">Curve</span> <span id="distanceDiffuseCurveVal">1.00</span></label>
                <input id="distanceDiffuseCurveSlider" type="range" min="0.5" max="2.0" step="0.05" value="1.0" class="gain-slider" />
              </div>
            </div>
          </div>
          <div class="info-section" id="spreadFromDistanceSection" style="margin:0;padding:0;border:none;background:none">
            <div class="title-with-info" style="font-size:12px;font-weight:600;color:#ffffff">
              <span data-i18n="spread.distanceTitle">Spread from Distance</span>
              <div class="inline-toggle" style="display:flex;align-items:center;gap:0.35rem">
                <button id="spreadFromDistanceInfoBtn" type="button" class="info-icon-btn" data-i18n-title="spread.distanceInfoButton" title="Spread from distance info">i</button>
                <input id="spreadFromDistanceToggle" type="checkbox" />
              </div>
            </div>
            <div id="spreadFromDistanceParams" class="conditional-params">
              <div class="meter-row" style="margin-top:0.2rem">
                <label style="font-size:12px;white-space:nowrap"><span data-i18n="spread.range">spread range</span> <span id="spreadDistanceRangeVal">1.00</span></label>
                <input id="spreadDistanceRangeSlider" type="range" min="0.1" max="3.0" step="0.01" value="1.0" class="gain-slider" />
              </div>
              <div class="meter-row" style="margin-top:0.15rem">
                <label style="font-size:12px;white-space:nowrap"><span data-i18n="spread.curve">spread curve</span> <span id="spreadDistanceCurveVal">1.00</span></label>
                <input id="spreadDistanceCurveSlider" type="range" min="0.1" max="3.0" step="0.05" value="1.0" class="gain-slider" />
              </div>
            </div>
          </div>
          </div>
          </div>
          </div>
          </div>
        </div>
      </div>
      </div>`;
}

export function mountRendererPanel() {
  const mountEl = document.getElementById('rendererPanelMount');
  if (!mountEl) {
    return;
  }
  mountEl.outerHTML = rendererPanelMarkup();
}
