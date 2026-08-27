/**
 * Record what the frontend's auto-tune detectors decide, so the Rust port can
 * be asserted against them.
 *
 * The direction matters: here the **JS is the reference**. It is the
 * implementation that has been driving real tuning runs, so the port is
 * correct exactly insofar as it agrees with it. (The geometry crate runs the
 * other way — there Rust is canonical and the JS mirror is checked against it.)
 *
 * These verdicts change kp/ki on a live audio path, so "looks equivalent" is
 * not good enough: a detector that fires one palier earlier retunes the loop
 * differently, and nothing in a diff would show it.
 *
 * Regenerate after any change to `src/auto-tune/detectors.js`:
 *
 *   node scripts/dump-auto-tune-goldens.mjs > scripts/golden/auto-tune.json
 *
 * Note `JSON.stringify` writes a non-finite number as `null`; the Rust side
 * reads a null jump ratio as infinity, which is what a flat baseline produces.
 */

import {
  computePalierStats,
  detectOscillationAbsolute,
  detectOscillationByJump,
  detectSaturation,
  detectConvergence,
  detectSourceLoss,
  computeRateStats
} from '../src/auto-tune/detectors.js';

/** Deterministic pseudo-random, so the vectors are reproducible. */
function makeRng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

/**
 * A telemetry window. `ppm(i, t)` returns the resampler correction in ppm.
 */
function series({ count, stepMs = 250, ppm, target = 200, error = () => 0, phase = () => 'stable' }) {
  const out = [];
  for (let i = 0; i < count; i += 1) {
    const t = i * stepMs;
    const value = ppm(i, t);
    out.push({
      t,
      latencySmoothedMs: target + error(i, t),
      latencyTargetMs: target,
      resampleRatio: value === null ? null : 1 + value / 1e6,
      phase: phase(i, t)
    });
  }
  return out;
}

const rng = makeRng(12345);

// Quasi-flat noise: the regime a low-kp palier sits in.
const flat = series({ count: 120, ppm: () => (rng() - 0.5) * 100 });
// A clear oscillation: what a too-high kp produces.
const oscillating = series({ count: 120, ppm: (i) => 4000 * Math.sin(i / 3) });
// Pinned at the adjustment limit.
const saturated = series({ count: 80, ppm: () => 9900 });
// Saturated only recently, so the hold is not yet satisfied.
const brieflySaturated = series({ count: 80, ppm: (i) => (i > 76 ? 9900 : 100) });
// Converged: error far inside 0.02% of a 200 ms target (= 0.04 ms).
const converged = series({ count: 120, ppm: () => 50, error: () => 0.001 });
// Error too large to count as converged.
const notConverged = series({ count: 120, ppm: () => 50, error: () => 5 });
// Two separate dips into low-recover.
const flapping = series({
  count: 80,
  ppm: () => 100,
  phase: (i) => ((i > 20 && i < 26) || (i > 60 && i < 66) ? 'low-recover' : 'stable')
});
// One long low-recover stretch: one event, not many.
const oneLongDip = series({
  count: 80,
  ppm: () => 100,
  phase: (i) => (i > 20 && i < 70 ? 'low-recover' : 'stable')
});
// Gaps: samples with no usable ratio must be skipped, not treated as zero.
const withGaps = series({ count: 120, ppm: (i) => (i % 7 === 0 ? null : 500) });
// Too few samples past the warm-up to say anything.
const tooShort = series({ count: 6, ppm: () => 100 });

// ── Fixtures that sit ON the decision boundaries ────────────────────────────
//
// The windows above are all comfortably far from every threshold, so they
// confirm the port on easy cases and would miss a discrepancy near an edge —
// which is exactly where a port goes wrong. These pin the boundaries
// themselves: one fixture just inside each, one just outside.

const STEP_MS = 250;
const WARMUP_SAMPLES = 10000 / STEP_MS; // discarded by palierWarmupMs

/** Warm-up filler, then an explicit list of post-warmup ppm values. */
function afterWarmup(values, { target = 200, error = () => 0, phase = () => 'stable' } = {}) {
  const all = [...Array.from({ length: WARMUP_SAMPLES }, () => 0), ...values];
  return series({
    count: all.length,
    stepMs: STEP_MS,
    ppm: (i) => all[i],
    target,
    error,
    phase
  });
}

/** `halfPeriods` alternating blocks of ±amplitude: yields halfPeriods-1 crossings. */
function squareWave(halfPeriods, amplitude, samplesPerHalf = 6) {
  const values = [];
  for (let h = 0; h < halfPeriods; h += 1) {
    const v = h % 2 === 0 ? amplitude : -amplitude;
    for (let i = 0; i < samplesPerHalf; i += 1) values.push(v);
  }
  return values;
}

// Crossing floor is 4. Amplitude ±750 gives a peak-to-peak of exactly 1500,
// the amplitude floor, so these also sit on that edge.
const atCrossingFloor = afterWarmup(squareWave(5, 750));   // 4 crossings
const belowCrossingFloor = afterWarmup(squareWave(4, 750)); // 3 crossings

// Amplitude floor is 1500 peak-to-peak, with crossings well clear of theirs.
const atAmplitudeFloor = afterWarmup(squareWave(11, 750));
const belowAmplitudeFloor = afterWarmup(squareWave(11, 749.5)); // p-p 1499

// Hysteresis is a 200 ppm dead-band around the mean: ±199 registers no state
// change at all, ±201 registers every one. An EVEN number of half-periods so
// the mean is exactly zero — with an odd count the mean shifts by one block
// and the band moves off the values being tested.
const insideHysteresis = afterWarmup(squareWave(10, 199));
const outsideHysteresis = afterWarmup(squareWave(10, 201));

// Saturation hold is 3000 ms. The run is measured from the oldest pinned
// sample to the newest, so N pinned samples span (N-1) * STEP_MS.
const atSaturationHold = afterWarmup([
  ...Array.from({ length: 20 }, () => 100),
  ...Array.from({ length: 3000 / STEP_MS + 1 }, () => 9900)
]);
const belowSaturationHold = afterWarmup([
  ...Array.from({ length: 20 }, () => 100),
  ...Array.from({ length: 3000 / STEP_MS }, () => 9900)
]);

// Convergence hold is 10000 ms, tolerance 0.02% of a 200 ms target = 0.04 ms.
const atConvergenceHold = afterWarmup(
  Array.from({ length: 10000 / STEP_MS + 21 }, () => 50),
  { error: (i) => (i >= WARMUP_SAMPLES + 20 ? 0.001 : 5) }
);
// Error exactly at the limit must NOT count: the test is `>= limit` breaks.
const errorAtConvergenceLimit = afterWarmup(
  Array.from({ length: 80 }, () => 50),
  { error: () => 0.04 }
);
// A hair inside it must.
const errorInsideConvergenceLimit = afterWarmup(
  Array.from({ length: 400 }, () => 50),
  { error: () => 0.0399 }
);

// Source loss fires at 2 events inside a 10 s window.
const atSourceLossFloor = afterWarmup(Array.from({ length: 80 }, () => 100), {
  phase: (i) => {
    const k = i - WARMUP_SAMPLES;
    return (k > 40 && k < 44) || (k > 60 && k < 64) ? 'low-recover' : 'stable';
  }
});
const belowSourceLossFloor = afterWarmup(Array.from({ length: 400 }, () => 100), {
  phase: (i) => {
    const k = i - WARMUP_SAMPLES;
    return k > 60 && k < 64 ? 'low-recover' : 'stable';
  }
});

const windows = {
  flat,
  oscillating,
  saturated,
  brieflySaturated,
  converged,
  notConverged,
  flapping,
  oneLongDip,
  withGaps,
  tooShort,
  empty: [],
  atCrossingFloor,
  belowCrossingFloor,
  atAmplitudeFloor,
  belowAmplitudeFloor,
  insideHysteresis,
  outsideHysteresis,
  atSaturationHold,
  belowSaturationHold,
  atConvergenceHold,
  errorAtConvergenceLimit,
  errorInsideConvergenceLimit,
  atSourceLossFloor,
  belowSourceLossFloor
};

const palierStats = {};
for (const [name, samples] of Object.entries(windows)) {
  palierStats[name] = computePalierStats(samples, 0);
}

const out = {
  _comment:
    'GENERATED from src/auto-tune/detectors.js — do not edit. ' +
    'node scripts/dump-auto-tune-goldens.mjs > this file. ' +
    'The JS is the reference; src-tauri/src/auto_tune/detectors.rs is asserted against it.',
  windows,
  palierStats,
  oscillationAbsolute: Object.fromEntries(
    Object.entries(windows).map(([name, s]) => [name, detectOscillationAbsolute(s, 0)])
  ),
  oscillationByJump: {
    // A real oscillation against a flat baseline.
    jumpOverFlat: detectOscillationByJump(palierStats.oscillating, [palierStats.flat]),
    // The same palier compared against itself: no jump.
    noJump: detectOscillationByJump(palierStats.oscillating, [palierStats.oscillating]),
    // Nothing to compare against yet.
    noBaseline: detectOscillationByJump(palierStats.oscillating, []),
    // Baseline present but all null.
    nullBaseline: detectOscillationByJump(palierStats.oscillating, [null, null]),
    // Current palier below the absolute floors.
    quietCurrent: detectOscillationByJump(palierStats.flat, [palierStats.flat]),
    noCurrent: detectOscillationByJump(null, [palierStats.flat])
  },
  saturation: Object.fromEntries(
    Object.entries(windows).map(([name, s]) => [name, detectSaturation(s, 0.01)])
  ),
  saturationNoLimit: detectSaturation(saturated, 0),
  convergence: Object.fromEntries(
    Object.entries(windows).map(([name, s]) => [name, detectConvergence(s)])
  ),
  sourceLoss: Object.fromEntries(
    Object.entries(windows).map(([name, s]) => [name, detectSourceLoss(s)])
  ),
  rateStatsAll: Object.fromEntries(
    Object.entries(windows).map(([name, s]) => [name, computeRateStats(s, null)])
  ),
  rateStatsWindowed: Object.fromEntries(
    Object.entries(windows).map(([name, s]) => [name, computeRateStats(s, 5000)])
  )
};

process.stdout.write(`${JSON.stringify(out, null, 2)}\n`);
