/**
 * Memory diagnostic sampler (opt-in).
 *
 * Kept in-tree for memory hunts: Windows Studio once climbed from ~200 MB past
 * 1 GB (native WebView2 growth, fixed by the `state:batch` emit coalescing and
 * the energy-volume upload throttle). To localise such growth the sampler
 * separates the three memory pools and traces each over time:
 *   1. host-process RSS / virtual  (Rust side, via the `debug_memory_stats`
 *                                   command; on Windows it also sums the whole
 *                                   process tree — host + WebView2 children —
 *                                   which is where a WebView2 leak shows up)
 *   2. WebView JS heap             (`performance.memory`, Chromium/WebView2 only —
 *                                   absent under WebKitGTK, hence the RSS anchor)
 *   3. WebGL / Three.js resources  (`renderer.info.memory` geometries/textures +
 *                                   `renderer.info.programs` — live cumulative
 *                                   counts, NOT reset by rendering, so a steady
 *                                   climb is a dispose leak)
 * Plus the live sizes of every per-id AppState map (`debug_state_sizes`) so a
 * Rust-side map growth is caught at a glance.
 *
 * Whichever curve has an unbounded positive slope is the culprit. The sampler is
 * itself bounded (ring buffer) so it never contributes to the growth it measures.
 *
 * Off by default; nothing runs unless explicitly enabled. Usage (DevTools console):
 *   localStorage.setItem('spatialviz.memory_sampler', '1')  // auto-start on next launch
 *   window.omniphonyDebug.memory.start()                    // or start right now
 *   window.omniphonyDebug.memory.stop()                     // pause sampling
 *   window.omniphonyDebug.memory.dump()                     // CSV of all samples so far
 * The CSV also auto-saves to Downloads/omniphony-memory.csv while sampling, so a
 * clean capture can be taken with DevTools CLOSED (DevTools itself runs inside
 * WebView2 and inflates the very process tree being measured). Release builds
 * have no DevTools unless compiled with `cargo build --features devtools`.
 */

import { invoke } from '@tauri-apps/api/core';
import { renderer } from './scene/setup.js';

const AUTO_START_KEY = 'spatialviz.memory_sampler';

const SAMPLE_INTERVAL_MS = 2000;
// 1 h of history at 2 s cadence. The buffer is capped so the diagnostic tool
// cannot itself leak while hunting a leak.
const MAX_SAMPLES = 1800;

// Persist the CSV to disk every N samples (see the header note on DevTools).
const AUTOSAVE_EVERY = 8; // ~16 s at the 2 s sample cadence

const samples = [];
let intervalId = null;
let startedAt = 0;
let tickCount = 0;
let savedPathLogged = false;

const MB = 1024 * 1024;
const toMB = (bytes) => (typeof bytes === 'number' ? +(bytes / MB).toFixed(1) : null);

async function takeSample() {
  const now = Date.now();
  const elapsedS = startedAt ? +((now - startedAt) / 1000).toFixed(1) : 0;

  // WebGL / Three.js — synchronous, always available.
  const info = renderer?.info;
  const geometries = info?.memory?.geometries ?? null;
  const textures = info?.memory?.textures ?? null;
  const programs = info?.programs?.length ?? null;

  // JS heap — Chromium/WebView2 only (undefined under WebKitGTK).
  const jsHeapMB = toMB(performance?.memory?.usedJSHeapSize);

  // Rust host process + AppState map sizes — may throw if Tauri is unavailable
  // (e.g. a plain browser dev build); fail soft so the WebGL/JS pools still log.
  let rssMB = null;
  let virtualMB = null;
  let treeRssMB = null;
  let stateSizes = null;
  try {
    const mem = await invoke('debug_memory_stats');
    rssMB = toMB(mem?.rssBytes);
    virtualMB = toMB(mem?.virtualBytes);
    treeRssMB = toMB(mem?.treeRssBytes);
  } catch (_) { /* not in a Tauri host */ }
  try {
    stateSizes = await invoke('debug_state_sizes');
  } catch (_) { /* not in a Tauri host */ }

  const sample = {
    t: now,
    elapsedS,
    rssMB,
    treeRssMB,
    virtualMB,
    jsHeapMB,
    geometries,
    textures,
    programs,
    state: stateSizes,
  };

  samples.push(sample);
  if (samples.length > MAX_SAMPLES) {
    samples.splice(0, samples.length - MAX_SAMPLES);
  }

  // One compact line per tick so the trend is visible live in the console.
  const stateStr = stateSizes
    ? ` src=${stateSizes.sources} bandGains=${stateSizes.objectBandGains} spkLvl=${stateSizes.speakerLevels}`
    : '';
  // eslint-disable-next-line no-console
  console.log(
    `[mem] +${elapsedS}s rss=${rssMB}MB tree=${treeRssMB}MB jsHeap=${jsHeapMB}MB`
      + ` geom=${geometries} tex=${textures} prog=${programs}${stateStr}`
  );

  tickCount += 1;
  if (tickCount % AUTOSAVE_EVERY === 0) {
    saveToFile();
  }
}

// Write the current CSV to disk via the Rust host. Returns the path (or null).
async function saveToFile() {
  const csv = buildCsv();
  try {
    const path = await invoke('debug_write_memory_csv', { csv });
    if (!savedPathLogged) {
      savedPathLogged = true;
      // eslint-disable-next-line no-console
      console.log(`[mem] CSV auto-saved to: ${path} (overwritten every `
        + `${(AUTOSAVE_EVERY * SAMPLE_INTERVAL_MS) / 1000}s)`);
    }
    return path;
  } catch (_) {
    return null;
  }
}

function buildCsv() {
  const header = 't_ms,elapsed_s,rss_mb,tree_rss_mb,virtual_mb,js_heap_mb,geometries,textures,programs,'
    + 'sources,source_levels,speaker_levels,object_speaker_gains,'
    + 'object_band_gains,speaker_gains,object_mutes,speaker_mutes,layouts';
  const rows = samples.map((s) => {
    const st = s.state || {};
    return [
      s.t, s.elapsedS, s.rssMB, s.treeRssMB, s.virtualMB, s.jsHeapMB,
      s.geometries, s.textures, s.programs,
      st.sources, st.sourceLevels, st.speakerLevels, st.objectSpeakerGains,
      st.objectBandGains, st.speakerGains, st.objectMutes,
      st.speakerMutes, st.layouts,
    ].map((v) => (v === null || v === undefined ? '' : v)).join(',');
  });
  return [header, ...rows].join('\n');
}

export function startMemorySampler() {
  if (intervalId) return;
  startedAt = Date.now();
  // Immediate first sample so a baseline is recorded at t≈0.
  takeSample();
  intervalId = setInterval(takeSample, SAMPLE_INTERVAL_MS);
  // eslint-disable-next-line no-console
  console.log(`[mem] sampler started (every ${SAMPLE_INTERVAL_MS / 1000}s) — `
    + 'use window.omniphonyDebug.memory.dump() for CSV');
}

export function stopMemorySampler() {
  if (intervalId) {
    clearInterval(intervalId);
    intervalId = null;
  }
}

/**
 * Register the console handle and, if the localStorage opt-in flag is set,
 * start sampling. Called once at boot; costs a single localStorage read when
 * the sampler is disabled.
 */
export function installMemoryDiagnostics() {
  if (typeof window === 'undefined') return;

  // Merge into the shared debug handle (visual-recovery.js also writes to it).
  const existing = window.omniphonyDebug && typeof window.omniphonyDebug === 'object'
    ? window.omniphonyDebug
    : {};
  window.omniphonyDebug = {
    ...existing,
    memory: {
      samples,
      start: startMemorySampler,
      stop: stopMemorySampler,
      dump() {
        const csv = buildCsv();
        // eslint-disable-next-line no-console
        console.log(csv);
        return csv;
      },
      save: saveToFile,
    },
  };

  let autoStart = false;
  try {
    autoStart = window.localStorage?.getItem(AUTO_START_KEY) === '1';
  } catch (_) { /* storage unavailable */ }
  if (autoStart) {
    startMemorySampler();
  }
}
