import { wireInlineHelpFromMarkup } from '../controls/inline-help.js';

export function rendererPanelMarkup() {
  return `
      <div id="rendererPanelRoot" class="renderer-panel-root">
      <div class="info-section renderer-panel-shell" id="rendererSection">
        <div class="renderer-panel-header-block" style="display:grid;gap:0.2rem">
          <div class="panel-header">
            <div class="panel-header-main">
            <div class="info-title panel-title" data-i18n="section.renderer">Renderer</div>
            <div style="display:flex;gap:0.25rem;flex:0 0 auto;margin-left:auto">
              <button id="outputModeHeadphonesBtn" type="button" class="toggle-btn output-mode-btn">🎧 <span data-i18n="outputMode.headphones">Headphones</span></button>
              <button id="outputModeSpeakersBtn" type="button" class="toggle-btn output-mode-btn">🔊 <span data-i18n="outputMode.speakers">Speakers</span></button>
            </div>
            <div id="rendererPerfWrap" style="display:none;min-width:180px;flex:0 0 auto">
              <div style="display:grid;gap:0.18rem;min-width:180px">
                <div style="display:grid;grid-template-columns:180px max-content;align-items:center;gap:0.35rem">
                  <div class="meter-bar" style="width:180px;min-width:180px;overflow:visible">
                    <div id="rendererPerfDecodeFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(140,214,255,0.95), rgba(104,170,255,0.95));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfRenderFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(112,170,255,0.92), rgba(88,132,255,0.92));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfCrossoverFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(255,214,120,0.96), rgba(255,166,94,0.96));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfWriteFill" class="meter-fill" style="background:linear-gradient(90deg, rgba(180,255,184,0.95), rgba(80,218,120,0.95));clip-path:inset(0 100% 0 0)"></div>
                    <div id="rendererPerfDecodeMaxMarker" class="meter-marker min" style="background:#ffd54a"></div>
                    <div id="rendererPerfRenderMaxMarker" class="meter-marker min" style="background:#ffb84a"></div>
                    <div id="rendererPerfCrossoverMaxMarker" class="meter-marker min" style="background:#ffeb8a"></div>
                    <div id="rendererPerfWriteMaxMarker" class="meter-marker min" style="background:#ff8b4a"></div>
                  </div>
                  <span id="rendererPerfFrameValue" style="display:inline-block;min-width:5.4rem;text-align:right;font-size:10px;white-space:nowrap;font-variant-numeric:tabular-nums;color:#9eb4c8">frame —</span>
                </div>
                <div style="display:grid;grid-template-columns:repeat(4, max-content);align-items:center;gap:0.28rem;font-size:10px;color:#d9ecff;white-space:nowrap;font-variant-numeric:tabular-nums">
                  <span id="rendererPerfDecodeValue" style="display:inline-block;min-width:5.4rem;text-align:right">decode —</span>
                  <span id="rendererPerfCrossoverValue" style="display:inline-block;min-width:5.4rem;text-align:right">cross —</span>
                  <span id="rendererPerfRenderValue" style="display:inline-block;min-width:5.4rem;text-align:right">render —</span>
                  <span id="rendererPerfWriteValue" style="display:inline-block;min-width:5.4rem;text-align:right">write —</span>
                </div>
                <div style="display:grid;grid-template-columns:repeat(4, max-content);align-items:center;gap:0.28rem;font-size:10px;color:#92a9bc;white-space:nowrap;font-variant-numeric:tabular-nums">
                  <span id="rendererPerfDecodeMaxValue" style="display:inline-block;min-width:5.4rem;text-align:right">max —</span>
                  <span id="rendererPerfCrossoverMaxValue" style="display:inline-block;min-width:5.4rem;text-align:right">max —</span>
                  <span id="rendererPerfRenderMaxValue" style="display:inline-block;min-width:5.4rem;text-align:right">max —</span>
                  <span id="rendererPerfWriteMaxValue" style="display:inline-block;min-width:5.4rem;text-align:right">max —</span>
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
          <div class="output-mode-mpv-note" style="font-size:0.65rem;color:#8fa6bd;padding:0 0.1rem;" data-i18n="outputMode.mpvNote">mpv host: the output mode is applied at player start — restart playback after switching.</div>
          <div class="info-section renderer-subpanel binaural-subpanel" id="binauralHrtfSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff" data-help-i18n="help.binaural.hrtf">HRTF</div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
                <select id="binauralHrirSource" class="form-select" style="font-size:0.75rem;padding:0.1rem 0.2rem;width:auto;min-width:90px;">
                  <option value="saf" data-i18n="binaural.hrtfSource.kemar">KEMAR (measured)</option>
                  <option value="synthetic" data-i18n="binaural.hrtfSource.synthetic">Synthetic</option>
                  <option value="pinna" data-i18n="binaural.hrtfSource.pinna">Pinna (parametric)</option>
                  <option value="prtf" data-i18n="binaural.hrtfSource.prtf">PRTF (Spagnol)</option>
                  <option value="sofa" data-i18n="binaural.hrtfSource.sofa">SOFA file</option>
                </select>
                <button id="sofaBrowseBtn" type="button" class="toggle-btn" style="display:none" data-i18n="backend.file.browse" data-i18n-title="binaural.sofaBrowseTitle" title="Browse the sofacoustics.org HRTF database">Browse…</button>
              </div>
            </div>
            <div class="renderer-subpanel-body" style="margin-top:0.25rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.3rem">
              <div id="binauralSofaInfo" style="display:none;font-size:0.65rem;word-break:break-all;"></div>
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.headRadius" data-help-i18n="help.binaural.headRadius" data-help-anchor=".binaural-help-row">Head radius (cm)</span>
                  <span id="binauralHeadRadiusVal">8.8</span>
                </div>
                <input id="binauralHeadRadius" type="range" min="5" max="15" step="0.1" value="8.75" style="width:100%;" />
              </div>
              <div id="binauralPinnaControls" style="display:none;gap:0.3rem;">
                <div class="binaural-help-row" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem;">
                  <span style="font-size:0.65rem;color:#888;" data-i18n="binaural.pinnaPreset" data-help-i18n="help.binaural.pinnaPreset" data-help-anchor=".binaural-help-row">Pinna preset (D)</span>
                  <select id="binauralPinnaPreset" class="form-select" style="font-size:0.7rem;padding:0.1rem 0.2rem;width:auto;">
                    <option value="pbnh" data-i18n="binaural.pinnaPreset.pbnh">PB &amp; NH</option>
                    <option value="rd" data-i18n="binaural.pinnaPreset.rd">RD</option>
                  </select>
                </div>
                <div class="binaural-help-row">
                  <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                    <span data-i18n="binaural.pinnaDScale" data-help-i18n="help.binaural.pinnaDScale" data-help-anchor=".binaural-help-row">Elevation factor D (%)</span>
                    <span id="binauralPinnaDScaleVal">100</span>
                  </div>
                  <input id="binauralPinnaDScale" type="range" min="50" max="150" step="5" value="100" style="width:100%;" />
                </div>
                <div class="binaural-help-row">
                  <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                    <span data-i18n="binaural.pinnaDepth" data-help-i18n="help.binaural.pinnaDepth" data-help-anchor=".binaural-help-row">Pinna echoes (%)</span>
                    <span id="binauralPinnaDepthVal">100</span>
                  </div>
                  <input id="binauralPinnaDepth" type="range" min="0" max="100" step="5" value="100" style="width:100%;" />
                </div>
              </div>
              <div id="binauralPrtfControls" style="display:none;gap:0.3rem;">
                <div class="binaural-help-row">
                  <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                    <span data-i18n="binaural.prtfDepth" data-help-i18n="help.binaural.prtfDepth" data-help-anchor=".binaural-help-row">Pinna coloration (%)</span>
                    <span id="binauralPrtfDepthVal">100</span>
                  </div>
                  <input id="binauralPrtfDepth" type="range" min="0" max="100" step="5" value="100" style="width:100%;" />
                </div>
                <div class="binaural-help-row">
                  <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                    <span data-i18n="binaural.prtfFreqScale" data-help-i18n="help.binaural.prtfFreqScale" data-help-anchor=".binaural-help-row">Notch frequency scale (%)</span>
                    <span id="binauralPrtfFreqScaleVal">100</span>
                  </div>
                  <input id="binauralPrtfFreqScale" type="range" min="50" max="150" step="5" value="100" style="width:100%;" />
                </div>
              </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel binaural-subpanel" id="binauralDistanceSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff" data-i18n="binaural.distanceTitle">Distance</div>
            </div>
            <div class="renderer-subpanel-body" style="margin-top:0.25rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.3rem">
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.distanceScale" data-help-i18n="help.binaural.distanceScale" data-help-anchor=".binaural-help-row">Distance scale (m / unit)</span>
                  <span id="binauralUnitScaleVal">1.0</span>
                </div>
                <input id="binauralUnitScale" type="range" min="0.1" max="10" step="0.1" value="1" style="width:100%;" />
              </div>
              <div class="inline-toggle" style="margin-top:0">
                <div data-i18n="binaural.airAbsorption" data-help-i18n="help.binaural.airAbsorption">Air absorption (distance HF roll-off)</div>
                <input id="binauralAirAbsorption" type="checkbox" checked />
              </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel binaural-subpanel" id="binauralRoomSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff" data-i18n="binaural.roomTitle">Listening room</div>
            </div>
            <div class="renderer-subpanel-body" style="margin-top:0.25rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.3rem">
              <div class="switch-row" style="margin-top:0;font-size:0.7rem;color:#8fa6bd;">
                <span data-i18n="binaural.earlyReflections" data-help-i18n="help.binaural.earlyReflections">Early reflections</span>
                <input id="binauralReflEnabled" type="checkbox" />
              </div>
              <div id="binauralReflParams" style="display:none;gap:0.3rem">
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.reflectionLevel" data-help-i18n="help.binaural.reflectionLevel" data-help-anchor=".binaural-help-row">Reflection level</span>
                  <span id="binauralReflLevelVal">0.50</span>
                </div>
                <input id="binauralReflLevel" type="range" min="0" max="1" step="0.01" value="0.5" style="width:100%;" />
              </div>
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.roomDims" data-help-i18n="help.binaural.room" data-help-anchor=".binaural-help-row">Room W × D × H (m)</span>
                  <span id="binauralReflRoomVal">4.0 × 5.0 × 2.7</span>
                </div>
                <input id="binauralReflRoomW" type="range" min="1" max="20" step="0.1" value="4" style="width:100%;" />
                <input id="binauralReflRoomD" type="range" min="1" max="20" step="0.1" value="5" style="width:100%;" />
                <input id="binauralReflRoomH" type="range" min="1" max="20" step="0.1" value="2.7" style="width:100%;" />
              </div>
              </div>
              <div class="switch-row" style="font-size:0.7rem;color:#8fa6bd;margin-top:0.2rem;border-top:1px solid rgba(255,255,255,0.05);padding-top:0.3rem;">
                <span data-i18n="binaural.lateReverb" data-help-i18n="help.binaural.lateReverb">Late reverb</span>
                <input id="binauralRevEnabled" type="checkbox" />
              </div>
              <div id="binauralRevParams" style="display:none;gap:0.3rem">
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.reverbLevel" data-help-i18n="help.binaural.reverbLevel" data-help-anchor=".binaural-help-row">Reverb level</span>
                  <span id="binauralRevLevelVal">0.25</span>
                </div>
                <input id="binauralRevLevel" type="range" min="0" max="1" step="0.01" value="0.25" style="width:100%;" />
              </div>
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.rt60" data-help-i18n="help.binaural.rt60" data-help-anchor=".binaural-help-row">RT60 (s)</span>
                  <span id="binauralRevRt60Val">0.35</span>
                </div>
                <input id="binauralRevRt60" type="range" min="0.1" max="1.5" step="0.05" value="0.35" style="width:100%;" />
              </div>
              </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel binaural-subpanel" id="binauralTrackingSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff" data-i18n="binaural.headTrackingTitle" data-help-i18n="help.binaural.headTracking">Head tracking (Sensors2OSC)</div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
                <button id="binauralRecenter" type="button" class="form-button" data-i18n="binaural.recenter">Recenter</button>
              </div>
            </div>
            <div class="renderer-subpanel-body" style="margin-top:0.25rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.3rem">
              <div class="inline-toggle" style="margin-top:0">
                <div data-i18n="binaural.oscAddressLabel" data-help-i18n="help.binaural.oscAddress">OSC address</div>
                <input id="binauralTrackAddress" type="text" class="form-input" placeholder="/android/rotationvector" style="font-size:0.7rem;width:11rem;" />
              </div>
              <div class="inline-toggle" style="margin-top:0">
                <div data-i18n="binaural.trackFormatLabel" data-help-i18n="help.binaural.trackFormat">Format</div>
                <select id="binauralTrackFormat" class="form-select" style="font-size:0.75rem;padding:0.1rem 0.2rem;width:auto;min-width:90px;">
                  <option value="auto" data-i18n="common.auto">Auto</option>
                  <option value="quat" data-i18n="binaural.trackFormat.quat">Quaternion</option>
                  <option value="rotvec" data-i18n="binaural.trackFormat.rotvec">Rotation vector</option>
                  <option value="euler" data-i18n="binaural.trackFormat.euler">Euler</option>
                </select>
              </div>
              <div class="binaural-help-row">
                <div style="font-size:0.65rem;color:#888;margin-bottom:0.15rem;display:flex;justify-content:space-between;">
                  <span data-i18n="binaural.trackSmoothing" data-help-i18n="help.binaural.trackSmoothing" data-help-anchor=".binaural-help-row">Smoothing</span>
                  <span id="binauralTrackSmoothingVal">0.20</span>
                </div>
                <input id="binauralTrackSmoothing" type="range" min="0" max="0.99" step="0.01" value="0.2" style="width:100%;" />
              </div>
              <div class="inline-toggle" style="margin-top:0">
                <div data-i18n="binaural.invertRotation" data-help-i18n="help.binaural.invertRotation">Invert rotation</div>
                <input id="binauralTrackInvert" type="checkbox" />
              </div>
              <div style="display:flex;align-items:center;gap:0.5rem">
                <span style="font-size:0.65rem;color:#888;" data-i18n="binaural.pose">Pose</span>
                <span id="binauralPoseReadout" style="font-size:10px;color:#8fa6bd;font-family:ui-monospace, monospace;">yaw 0°  pitch 0°  roll 0°</span>
              </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel" id="evaluationSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div class="title-with-info" style="margin:0;font-size:12px;font-weight:600;color:#ffffff">
                <span data-i18n="evaluation.title">Evaluation</span>
                <button id="evaluationInfoBtn" type="button" class="info-icon-btn" data-i18n-title="evaluation.infoButton" title="Evaluation mode info">i</button>
              </div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
                <select id="renderEvaluationModeSelect" class="delay-input" style="width:auto;min-width:13rem;text-align:left">
                  <option value="auto" data-i18n="common.auto">Auto</option>
                  <option value="realtime" data-i18n="eval.mode.realtime">Realtime</option>
                  <option value="precomputed_polar" data-i18n="eval.mode.precomputedPolar">Precomputed polar</option>
                  <option value="precomputed_cartesian" data-i18n="eval.mode.precomputedCartesian">Precomputed cartesian</option>
                </select>
                <div id="renderEvaluationModeEffective" class="vbap-step" style="min-width:8rem;text-align:right">—</div>
              </div>
            </div>
            <div id="evaluationSectionContent" class="conditional-params open">
            <div class="renderer-subpanel-body" style="margin-top:0.25rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.18rem">
              <div id="renderEvaluationCartesianBlock">
              <div class="control-row" id="renderEvaluationCartesianRow" style="margin-top:0;grid-template-columns:1fr auto;align-items:start">
                <label style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="eval.cartesianGrid" data-help-i18n="help.eval.cartesianGrid">Cartesian grid</label>
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
                <label style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="eval.polarGrid" data-help-i18n="help.eval.polarGrid">Polar grid</label>
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
                <span style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="vbap.positionInterpolation" data-help-i18n="help.vbap.positionInterpolation">Position interpolation</span>
              </div>
              <input id="vbapPositionInterpolationToggleEl" type="checkbox" />
            </div>
            <div class="control-row" id="objectSizeIntervalsRow" style="margin-top:0.25rem;grid-template-columns:1fr auto;align-items:center">
              <div class="title-with-info" style="min-width:0">
                <span style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="evaluation.objectSizeIntervals" data-help-i18n="help.eval.objectSizeIntervals">Object size intervals</span>
              </div>
              <input id="objectSizeIntervalsInput" class="delay-input" type="number" min="0" step="1" style="width:5rem" />
            </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel" id="rampSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div style="margin:0;font-size:12px;font-weight:600;color:#ffffff" data-i18n="renderer.rampTitle">Ramp</div>
            </div>
            <div id="rampSectionContent" class="conditional-params open">
              <div class="renderer-subpanel-body" style="margin-top:0.25rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.18rem">
                <div class="control-row" id="rampModeRow" style="margin-top:0;grid-template-columns:1fr auto;align-items:center">
                  <div class="title-with-info" style="min-width:0;font-size:12px;font-weight:600">
                    <label for="rampModeSelect" style="font-size:12px;font-weight:600;white-space:nowrap;color:#ffffff" data-i18n="audio.rampMode">Ramp mode</label>
                    <button id="rampModeInfoBtn" type="button" class="info-icon-btn" data-i18n-title="rampMode.infoButton" title="Ramp mode info">i</button>
                  </div>
                  <select id="rampModeSelect" class="delay-input" style="min-width:9rem">
                    <option value="off" data-i18n="audio.rampModeOff">Off</option>
                    <option value="frame" data-i18n="audio.rampModeFrame" selected>Per frame</option>
                    <option value="interp" data-i18n="audio.rampModeInterp">Per sample (interpolated)</option>
                    <option value="sample" data-i18n="audio.rampModeSample">Per sample</option>
                  </select>
                </div>
              </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel" id="backendParametersSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div class="renderer-subpanel-titlebar" style="display:flex;align-items:center;gap:0.45rem;min-width:0">
                <div class="title-with-info" style="margin:0;font-size:12px;font-weight:600;color:#ffffff">
                  <span data-i18n="backend.title">Backend</span>
                  <button id="backendInfoBtn" type="button" class="info-icon-btn" data-i18n-title="backend.infoButton" title="Render backend info">i</button>
                </div>
                <div id="vbapStatus" class="vbap-status" style="margin:0;font-size:11px;min-width:0">—</div>
              </div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
	              <select id="renderBackendSelect" class="delay-input" style="width:auto;min-width:10.5rem;text-align:left">
	                <option value="vbap">VBAP</option>
	                <option value="barycenter">Barycenter</option>
	                <option value="experimental_distance">Distance</option>
	                <option value="hybrid">Hybrid</option>
	              </select>
              <button id="restoreBackendBtn" type="button" class="secondary-btn" style="display:none;white-space:nowrap" data-i18n="backend.restore">Restore backend</button>
              <div id="renderBackendEffective" class="vbap-step" style="min-width:5.4rem;text-align:right">—</div>
            </div>
          </div>
            <div id="backendParametersSectionContent" class="conditional-params open">
          <div id="backendSpecificParamsSection" style="display:flex;flex-direction:column">
          <div class="info-section" id="hybridSection" style="margin:0;padding:0;border:none;background:none;display:none;order:-1">
            <div style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div class="title-with-info" style="margin:0;font-size:12px;font-weight:600;color:#ffffff">
                <span data-i18n="hybrid.title">Hybrid backend</span>
                <button id="hybridInfoBtn" type="button" class="info-icon-btn" data-i18n-title="hybrid.infoButton" title="Hybrid backend info">i</button>
              </div>
            </div>
            <div id="hybridSectionContent" class="conditional-params open">
              <div id="hybridParamTabs" style="display:flex;gap:0.25rem;margin-top:0.25rem;flex-wrap:wrap"></div>
              <div id="hybridConfigPanel">
                <div style="margin-top:0.2rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.3rem">
                  <div class="control-row" style="margin-top:0;grid-template-columns:1fr auto;align-items:center">
                    <label for="hybridExternalBackendSelect" style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="hybrid.external" data-help-i18n="help.hybrid.external">External backend (ratio = 1)</label>
                    <select id="hybridExternalBackendSelect" class="delay-input" style="min-width:9rem"></select>
                  </div>
                  <div class="control-row" style="margin-top:0;grid-template-columns:1fr auto;align-items:center">
                    <label for="hybridInternalBackendSelect" style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="hybrid.internal" data-help-i18n="help.hybrid.internal">Internal backend (ratio = 0)</label>
                    <select id="hybridInternalBackendSelect" class="delay-input" style="min-width:9rem"></select>
                  </div>
                  <div class="control-row" style="margin-top:0;grid-template-columns:1fr auto;align-items:center">
                    <label for="hybridMetricSelect" style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="distance.metric" data-help-i18n="help.hybrid.metric">Distance metric</label>
                    <select id="hybridMetricSelect" class="delay-input" style="min-width:9rem">
                      <option value="chebyshev" data-i18n="distance.metric.chebyshev">Chebyshev</option>
                      <option value="spherical" data-i18n="distance.metric.spherical">Spherical</option>
                    </select>
                  </div>
                  <div class="control-row" style="margin-top:0">
                    <label style="font-size:12px;white-space:nowrap"><span data-i18n="hybrid.smoothing" data-help-i18n="help.hybrid.smoothing">Curve smoothing</span> <span id="hybridCurveSmoothingVal">0.00</span></label>
                    <input id="hybridCurveSmoothingSlider" type="range" min="0" max="1" step="0.01" value="0" class="gain-slider" />
                  </div>
                  <div style="font-size:11px;color:#b8b8b8;margin-top:0.1rem">
                    <span data-i18n="hybrid.curveHint">Blend curve — X: distance (center → cube surface), Y: external ratio. Double-click to add a point, double-click a point to remove it.</span>
                  </div>
                  <canvas id="hybridCurveCanvas" width="320" height="180" style="width:100%;height:180px;background:rgba(0,0,0,0.25);border:1px solid rgba(255,255,255,0.12);border-radius:6px;cursor:crosshair;touch-action:none"></canvas>
                  <div id="hybridPointEditor" style="display:none;align-items:center;gap:0.3rem;font-size:11px;color:#ffffff">
                    <span data-i18n="hybrid.selectedPoint" data-help-i18n="help.hybrid.selectedPoint">Point</span>
                    <label for="hybridPointXInput" data-i18n="hybrid.pointDistance">d</label>
                    <input id="hybridPointXInput" class="delay-input" type="number" step="0.01" style="width:4.5rem" disabled />
                    <label for="hybridPointYInput" data-i18n="hybrid.pointRatio">ratio</label>
                    <input id="hybridPointYInput" class="delay-input" type="number" min="0" max="1" step="0.01" style="width:4.5rem" disabled />
                  </div>
                </div>
              </div>
            </div>
          </div>
          </div>
          </div>
          </div>
          <div class="info-section renderer-subpanel" id="distanceDiffuseSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div class="title-with-info" style="margin:0;font-size:12px;font-weight:600;color:#ffffff">
                <span data-i18n="distance.title">Distance Diffuse</span>
                <button id="distanceDiffuseInfoBtn" type="button" class="info-icon-btn" data-i18n-title="distance.infoButton" title="Distance diffuse info">i</button>
              </div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
                <input id="distanceDiffuseToggle" type="checkbox" />
              </div>
            </div>
            <div id="distanceDiffuseParams" class="conditional-params">
              <div class="renderer-subpanel-body" style="margin-top:0.25rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.18rem">
                <div class="control-row" style="margin-top:0;grid-template-columns:1fr auto;align-items:center">
                  <label for="distanceDiffuseMetricSelect" style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="distance.metric" data-help-i18n="help.distanceDiffuse.metric">Distance metric</label>
                  <select id="distanceDiffuseMetricSelect" class="delay-input" style="min-width:9rem">
                    <option value="spherical" data-i18n="distance.metric.spherical">Spherical</option>
                    <option value="chebyshev" data-i18n="distance.metric.chebyshev">Chebyshev</option>
                  </select>
                </div>
                <div class="control-row" style="margin-top:0.15rem;grid-template-columns:1fr auto;align-items:center">
                  <label style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="distance.mirrorAxes" data-help-i18n="help.distanceDiffuse.mirrorAxes">Mirror axes</label>
                  <span id="distanceDiffuseSymmetry" style="font-size:11px;color:#8fa6bd" data-i18n="distance.symmetry.axisZ">Half-turn about Z</span>
                </div>
                <div class="switch-row" style="margin-top:0;font-size:0.7rem;color:#8fa6bd">
                  <span data-i18n="distance.mirrorAxis.x">X — left / right</span>
                  <input id="distanceDiffuseMirrorX" type="checkbox" checked />
                </div>
                <div class="switch-row" style="margin-top:0;font-size:0.7rem;color:#8fa6bd">
                  <span data-i18n="distance.mirrorAxis.y">Y — front / back</span>
                  <input id="distanceDiffuseMirrorY" type="checkbox" checked />
                </div>
                <div class="switch-row" style="margin-top:0;font-size:0.7rem;color:#8fa6bd">
                  <span data-i18n="distance.mirrorAxis.z">Z — up / down</span>
                  <input id="distanceDiffuseMirrorZ" type="checkbox" />
                </div>
                <div class="control-row" style="margin-top:0">
                  <label style="font-size:12px;white-space:nowrap"><span data-i18n="distance.threshold" data-help-i18n="help.distanceDiffuse.threshold">Threshold</span> <span id="distanceDiffuseThresholdVal">1.00</span></label>
                  <input id="distanceDiffuseThresholdSlider" type="range" min="0.1" max="2.0" step="0.01" value="1.0" class="gain-slider" />
                </div>
                <div class="control-row" style="margin-top:0.15rem">
                  <label style="font-size:12px;white-space:nowrap"><span data-i18n="distance.curve" data-help-i18n="help.distanceDiffuse.curve">Curve</span> <span id="distanceDiffuseCurveVal">1.00</span></label>
                  <input id="distanceDiffuseCurveSlider" type="range" min="0.5" max="2.0" step="0.05" value="1.0" class="gain-slider" />
                </div>
              </div>
            </div>
          </div>
          <div class="info-section renderer-subpanel" id="distanceModelSection" style="margin:0;padding:0.4rem 0.5rem;border:1px solid rgba(255,255,255,0.08);border-radius:8px;background:rgba(255,255,255,0.03)">
            <div class="renderer-subpanel-bar" style="display:flex;align-items:center;justify-content:space-between;gap:0.4rem">
              <div class="title-with-info" style="margin:0;font-size:12px;font-weight:600;color:#ffffff">
                <span data-i18n="distance.model">Distance model</span>
                <button id="distanceModelInfoBtn" type="button" class="info-icon-btn" data-i18n-title="distance.modelInfoButton" title="Distance model info">i</button>
              </div>
              <div class="renderer-subpanel-actions" style="display:flex;align-items:center;gap:0.35rem">
                <select id="distanceModelSelect" class="delay-input" style="width:auto;min-width:10.5rem;text-align:left">
                  <option value="none" data-i18n="distance.model.none">None</option>
                  <option value="linear" data-i18n="distance.model.linear">Linear</option>
                  <option value="quadratic" data-i18n="distance.model.quadratic">Quadratic</option>
                  <option value="inverse-square" data-i18n="distance.model.inverseSquare">Inverse-square</option>
                </select>
              </div>
            </div>
            <div id="distanceModelMetricRow" class="renderer-subpanel-body" style="margin-top:0.25rem;margin-left:1rem;padding:0.3rem 0.4rem;background:rgba(255,255,255,0.03);border-radius:6px;display:grid;gap:0.18rem">
              <div class="control-row" style="margin-top:0;grid-template-columns:1fr auto;align-items:center">
                <label for="distanceModelMetricSelect" style="font-size:12px;white-space:nowrap;color:#ffffff" data-i18n="distance.metric" data-help-i18n="help.distanceModel.metric">Distance metric</label>
                <select id="distanceModelMetricSelect" class="delay-input" style="min-width:9rem">
                  <option value="spherical" data-i18n="distance.metric.spherical">Spherical</option>
                  <option value="chebyshev" data-i18n="distance.metric.chebyshev">Chebyshev</option>
                </select>
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
  // Attach the inline help popovers (click a param name → panel below) to the
  // hand-written renderer params annotated with `data-help-i18n`.
  wireInlineHelpFromMarkup(document.getElementById('rendererPanelRoot') || document);
}
