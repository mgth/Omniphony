import { linearToDb } from '../audio-math.js';
import { app } from '../state.js';
import { invoke } from '@tauri-apps/api/core';
import { t } from '../i18n.js';

import { inDrcPanel } from '../ui/panel-roots.js';

function getDrcControlRowEl() { return inDrcPanel('drcControlRow'); }
function getDrcModeSelectEl() { return inDrcPanel('drcModeSelect'); }
function getDrcGaugeRowEl() { return inDrcPanel('drcGaugeRow'); }
function getDrcGaugeFillEl() { return inDrcPanel('drcGaugeFill'); }
function getDrcGainValueEl() { return inDrcPanel('drcGainValue'); }
function getDrcWeightRowEl() { return inDrcPanel('drcWeightRow'); }
function getDrcWeightSliderEl() { return inDrcPanel('drcWeightSlider'); }
function getDrcWeightValueEl() { return inDrcPanel('drcWeightValue'); }
function getDrcSummaryEl() { return inDrcPanel('drcSummary'); }

function normalizeDrcModes(modes) {
    const seen = new Set();
    const normalized = [];
    (Array.isArray(modes) ? modes : []).forEach((mode) => {
        const value = String(mode ?? '').trim();
        if (!value || seen.has(value)) {
            return;
        }
        seen.add(value);
        normalized.push(value);
    });
    return normalized;
}

function effectiveDrcModes() {
    const supportedModes = normalizeDrcModes(app.supportedDrcModes);
    const currentMode = typeof app.drcMode === 'string' ? app.drcMode.trim() : '';
    if (currentMode.length > 0) {
        supportedModes.push(currentMode);
    }
    const merged = normalizeDrcModes(supportedModes);
    if (merged.length > 0) {
        return merged;
    }
    return ['Off'];
}

export function renderDrcUI() {
    const drcControlRowEl = getDrcControlRowEl();
    const drcModeSelectEl = getDrcModeSelectEl();
    const drcGaugeRowEl = getDrcGaugeRowEl();
    const drcWeightRowEl = getDrcWeightRowEl();
    const drcSummaryEl = getDrcSummaryEl();

    if (!drcControlRowEl || !drcModeSelectEl || !drcGaugeRowEl) return;

    const modes = effectiveDrcModes();
    const selectedMode = typeof app.drcMode === 'string' && app.drcMode.trim().length > 0
        ? app.drcMode.trim()
        : modes[0];

    drcControlRowEl.style.display = 'flex';
    drcGaugeRowEl.style.display = app.oscMeteringEnabled ? 'flex' : 'none';
    if (drcWeightRowEl) drcWeightRowEl.style.display = 'block';

    // Update modes if changed
    const currentOptions = Array.from(drcModeSelectEl.options).map(o => o.value);
    if (JSON.stringify(currentOptions) !== JSON.stringify(modes)) {
        drcModeSelectEl.innerHTML = '';
        modes.forEach(mode => {
            const opt = document.createElement('option');
            opt.value = mode;
            opt.textContent = mode;
            drcModeSelectEl.appendChild(opt);
        });
    }

    if (selectedMode && drcModeSelectEl.value !== selectedMode) {
        drcModeSelectEl.value = selectedMode;
    }

    renderDrcWeightUI();
    updateDrcSummary();
}

export function updateDrcSummary() {
    const drcSummaryEl = getDrcSummaryEl();
    if (!drcSummaryEl) return;

    const modes = effectiveDrcModes();
    const currentMode = typeof app.drcMode === 'string' && app.drcMode.trim().length > 0
        ? app.drcMode.trim()
        : (modes[0] || 'Off');
    const drcPart = `${currentMode} (${Math.round(app.drcWeight * 100)}%)`;
    const loudnessPart = app.loudnessEnabled ? 'Loudness ON' : 'Loudness OFF';
    
    drcSummaryEl.textContent = `${drcPart} | ${loudnessPart}`;
}

export function renderDrcWeightUI() {
    const sliderEl = getDrcWeightSliderEl();
    const valueEl = getDrcWeightValueEl();
    if (!sliderEl || !valueEl) return;

    const weight = typeof app.drcWeight === 'number' ? app.drcWeight : 1.0;
    const percent = Math.round(Math.max(0, Math.min(1, weight)) * 100);
    if (sliderEl !== document.activeElement && Number(sliderEl.value) !== percent) {
        sliderEl.value = String(percent);
    }
    valueEl.textContent = `${percent}%`;
}

export function updateDrcMeterUI(gain) {
    const drcGaugeFillEl = getDrcGaugeFillEl();
    const drcGainValueEl = getDrcGainValueEl();

    if (!drcGaugeFillEl || !drcGainValueEl) return;

    const gainDb = linearToDb(gain);
    const displayedDb = Number.isFinite(gainDb) ? gainDb : -100;

    // Max absolute delta for gauge display (e.g. +/-20dB).
    const maxDelta = 20;
    const percent = Math.min(100, (Math.abs(displayedDb) / maxDelta) * 100);
    
    drcGaugeFillEl.style.width = `${percent.toFixed(1)}%`;
    drcGainValueEl.textContent = `${displayedDb >= 0 ? '+' : ''}${displayedDb.toFixed(1)} dB`;

    // Color gauge based on direction and amount.
    if (displayedDb > 1.0) {
        drcGaugeFillEl.style.background = '#33b5e5';
    } else if (displayedDb < -12) {
        drcGaugeFillEl.style.background = '#ff4444';
    } else if (displayedDb < -6) {
        drcGaugeFillEl.style.background = '#ffbb33';
    } else {
        drcGaugeFillEl.style.background = '#00c851';
    }
}

let drcListenersBound = false;

export function bindDrcListeners() {
    if (drcListenersBound) return;

    const modeSelect = getDrcModeSelectEl();
    if (modeSelect) {
        modeSelect.addEventListener('change', (e) => {
            const mode = e.target.value;
            app.drcMode = mode;
            invoke('control_drc_mode', {
                value: mode
            }).catch(err => console.error('[drc]', err));
        });
    }

    const weightSlider = getDrcWeightSliderEl();
    if (weightSlider) {
        weightSlider.addEventListener('input', (e) => {
            const percent = Number(e.target.value);
            const weight = Math.max(0, Math.min(1, percent / 100));
            app.drcWeight = weight;
            const valueEl = getDrcWeightValueEl();
            if (valueEl) valueEl.textContent = `${Math.round(weight * 100)}%`;
            invoke('control_drc_weight', { value: weight }).catch(err => console.error('[drc]', err));
        });
    }

    drcListenersBound = true;
}
