/**
 * Record what the frontend's auto-tune state machine *does*, so the Rust port
 * can be held to it.
 *
 * The detectors got this treatment in #304, and it earned its keep: the first
 * set of vectors passed immediately and turned out not to bite. The FSM is
 * three times the size and it patches kp/ki on a running audio path, so it
 * gets the same treatment before a line of it is ported.
 *
 * What is recorded, per scenario: every emitted event in order, and the state
 * plus context after each telemetry sample. A port is correct when it produces
 * the same sequence — not merely the same final kp/ki, which two different
 * paths can reach.
 *
 * Regenerate after any change to `src/auto-tune/state-machine.js`:
 *
 *   node scripts/dump-auto-tune-runs.mjs > scripts/golden/auto-tune-runs.json
 *
 * ## The plant
 *
 * Driving the FSM anywhere interesting needs telemetry that *reacts* to the kp
 * it applies — a real run only oscillates because kp got too high. The model
 * below is deliberately crude: below a critical kp the loop is quiet noise,
 * above it the amplitude grows. It is not a simulation of the resampler, and
 * nothing here validates the tuning procedure. It exists to walk the state
 * machine through its transitions reproducibly.
 *
 * ## Determinism
 *
 * `start()` takes its time as an argument, but `userAck('perturbation')`
 * reaches for `Date.now()` — so the FSM is not quite the pure logic its header
 * claims, and cannot be replayed without pinning the clock. It is stubbed here.
 * The Rust port should take the time as a parameter, the way `start` already
 * does.
 */

import { createAutoTuneStateMachine } from '../src/auto-tune/state-machine.js';

/** Deterministic pseudo-random, so a run is reproducible. */
function makeRng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

const STEP_MS = 250;

/**
 * A crude closed loop: quiet below `kpCrit`, growing oscillation above it.
 * Returns rate_adjust in ppm for a given time and applied kp.
 */
function plant({ kpCrit = 40, noisePpm = 80, rng }) {
  return (t, kp) => {
    const noise = (rng() - 0.5) * noisePpm;
    if (kp < kpCrit) return noise;
    // Past the critical gain the loop rings; amplitude grows with the excess.
    const excess = Math.min(8, kp / kpCrit);
    const amplitude = 900 * excess;
    return amplitude * Math.sin(t / 700) + noise;
  };
}

/**
 * Drive one scenario and record it.
 *
 * `actions` maps a sample index to a callback, so user acknowledgements and
 * cancellations land at a reproducible point in the timeline rather than a
 * wall-clock one.
 */
function record({ name, samples, options = {}, actions = {}, plantOpts = {} }) {
  const rng = makeRng(4242);
  const rate = plant({ ...plantOpts, rng });

  const events = [];
  const steps = [];
  const fsm = createAutoTuneStateMachine(options);
  fsm.on((event, payload) => events.push({ event, payload }));

  // `userAck('perturbation')` reads Date.now(); pin it to the sample clock so
  // the recording is reproducible. See the header.
  const realNow = Date.now;
  let clock = 0;
  Date.now = () => clock;

  try {
    fsm.start(0);
    for (let i = 0; i < samples; i += 1) {
      const t = i * STEP_MS;
      clock = t;
      const kp = fsm.getContext().currentKp;
      const ppm = rate(t, kp);
      fsm.pushSample({
        t,
        latencySmoothedMs: 200 + (rng() - 0.5) * 0.02,
        latencyTargetMs: 200,
        resampleRatio: 1 + ppm / 1e6,
        phase: 'stable'
      });
      if (actions[i]) actions[i](fsm);
      // Record only on a change: a full per-sample dump is mostly repetition,
      // and what matters is where the machine moved.
      const state = fsm.getState();
      const ctx = fsm.getContext();
      const last = steps[steps.length - 1];
      const line = { i, t, state, ...ctx };
      if (!last || JSON.stringify({ ...last, i: 0, t: 0 }) !== JSON.stringify({ ...line, i: 0, t: 0 })) {
        steps.push(line);
      }
      if (['completed', 'cancelled', 'error'].includes(state)) break;
    }
  } finally {
    Date.now = realNow;
  }

  return { name, finalState: fsm.getState(), context: fsm.getContext(), events, steps };
}

// Shorter paliers than production, so a scenario walks the whole procedure in
// a few thousand samples instead of hours of simulated time.
//
// A palier must outlast the detectors' own 10 s warm-up
// (`TUNE_THRESHOLDS.oscillation.palierWarmupMs`), which the state machine does
// not parameterise — it calls `computePalierStats` with the defaults. Set them
// shorter and every palier is discarded as warm-up, so nothing is ever
// detected: the sweep just doubles kp to its ceiling and errors out.
//
// It also has to leave enough *usable* window after that warm-up for the
// oscillation to be counted: the detector wants four mean crossings, so the
// post-warm-up span must cover a couple of periods of whatever the plant
// rings at. 30 s leaves 20 s, about four and a half cycles of the model below.
//
// Both of those were wrong in the first version of this file, and both showed
// up the same way — every scenario doubling kp to the ceiling and erroring out
// without ever leaving `holdKp`. Which is the point of recording behaviour
// rather than asserting it.
const FAST = {
  kpPalierMs: 30000,
  kiPalierMs: 30000,
  perturbationRecoverMs: 14000,
  longRunDefaultMs: 30000,
  longRunMinAbbreviateMs: 14000,
  longRunStatsWindowMs: 14000,
  tighteningPalierMs: 30000,
  sampleRetentionMs: 600000
};

const runs = [
  // The kp sweep on its own: doubling until the plant rings.
  record({ name: 'kpSweepToOscillation', samples: 1600, options: FAST }),

  // A plant that never rings within kpMax — the sweep has to give up rather
  // than double for ever.
  record({
    name: 'kpNeverOscillates',
    samples: 2000,
    options: { ...FAST, kpMax: 64 },
    plantOpts: { kpCrit: 1e9 }
  }),

  // Cancelled mid-sweep.
  record({
    name: 'cancelledMidSweep',
    samples: 1600,
    options: FAST,
    actions: { 200: (fsm) => fsm.cancel() }
  }),

  // Acknowledge the perturbation prompt as soon as it is raised, then let the
  // long run be abbreviated.
  record({
    name: 'fullRunWithAcks',
    samples: 16000,
    options: FAST,
    actions: Object.fromEntries(
      // Poll for a prompt every 20 samples and answer it; the FSM ignores an
      // ack it did not ask for, so this is safe to fire blind.
      Array.from({ length: 200 }, (_, k) => [
        k * 20,
        (fsm) => {
          fsm.userAck('perturbation');
          fsm.userAck('ready');
          fsm.abbreviate();
        }
      ])
    )
  })
];

const out = {
  _comment:
    'GENERATED from src/auto-tune/state-machine.js — do not edit. ' +
    'node scripts/dump-auto-tune-runs.mjs > this file. ' +
    'The JS is the reference; the Rust port is asserted against these runs.',
  stepMs: STEP_MS,
  options: FAST,
  runs
};

process.stdout.write(`${JSON.stringify(out)}\n`);
