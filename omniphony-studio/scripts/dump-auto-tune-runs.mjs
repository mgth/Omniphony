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

import { AUTO_TUNE_DEFAULTS, createAutoTuneStateMachine } from '../src/auto-tune/state-machine.js';

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
 *
 * `ring` is a shared flag the runner raises to make the loop oscillate
 * regardless of kp — used to model a perturbation that leaves the loop
 * ringing after the disturbance is gone.
 */
function plant({ kpCrit = 40, noisePpm = 80, ringPpm = 0, rng, ring }) {
  return (t, kp) => {
    const noise = (rng() - 0.5) * noisePpm;
    if (ring && ring.on) return ringPpm * Math.sin(t / 700) + noise;
    if (kp < kpCrit) return noise;
    // Past the critical gain the loop rings; amplitude grows with the excess.
    const excess = Math.min(8, kp / kpCrit);
    const amplitude = 900 * excess;
    return amplitude * Math.sin(t / 700) + noise;
  };
}

/**
 * Latency error in ms, as a *declared* shape rather than a closure — the Rust
 * replay has to reproduce it, and a description it can read beats a formula it
 * has to be told about scenario by scenario.
 *
 * Returning null means "no error": the runner then adds its own tiny jitter,
 * which is the only case that pulls a second random per sample.
 */
function latencyErrFn(spec) {
  if (!spec) return null;
  switch (spec.kind) {
    // Sustained sinusoidal error: too large to converge, so tuningKi iterates.
    case 'sine':
      return (i) => spec.offset + Math.sin(i / spec.periodSamples) * spec.amp;
    // Barely-decaying error: never converges, but never worsens either — the
    // one shape that lands in the `too-slow` branch. See `kiTooSlow` below.
    case 'decay':
      return (i) => spec.from * Math.exp(-i / spec.tauSamples);
    default:
      throw new Error(`unknown latency error kind: ${spec.kind}`);
  }
}

/**
 * Drive one scenario and record it.
 *
 * Everything that shapes a run is declared, not coded: the plant, the latency
 * error, and the user actions (`ackEvery` polls for a prompt and answers it,
 * `cancelAt` cancels at a fixed sample index). All of it is echoed into the
 * recording so the Rust replay reads the setup instead of hardcoding it per
 * scenario name — the first version did that, and it is exactly the kind of
 * duplication that lets the two harnesses drift apart in silence.
 *
 * Actions are indexed by sample, not wall-clock, so they land at a
 * reproducible point in the timeline.
 */
function record({
  name,
  samples,
  options = {},
  plantOpts = {},
  latencyErr = null,
  ackEvery = null,
  cancelAt = null,
  ringOnceDuringRecovery = false,
}) {
  const rng = makeRng(4242);
  // Raised by the runner below when the machine enters perturbationRecovering.
  const ring = { on: false, spent: false };
  const rate = plant({ ...plantOpts, rng, ring: ringOnceDuringRecovery ? ring : null });
  const latencyErrOf = latencyErrFn(latencyErr);

  const events = [];
  const steps = [];
  const fsm = createAutoTuneStateMachine(options);

  // The kp the renderer would actually be running: the last one *applied*, not
  // `context.currentKp`. The two diverge the moment the sweep declares
  // oscillation — the machine emits applyParams{kpNear: 0.6·kpCrit} but leaves
  // currentKp at kpCrit (state-machine.js:196-203). Feeding the plant
  // currentKp, as the first version of this file did, left it ringing above
  // the critical gain for the whole of every run: no scenario ever saw a calm
  // loop after the sweep, which is precisely the regime the rest of the
  // procedure is tuned in.
  let appliedKp = 0;
  fsm.on((event, payload) => {
    if (event === 'applyParams' && typeof payload?.kpNear === 'number') {
      appliedKp = payload.kpNear;
    }
    events.push({ event, payload });
  });

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
      const ppm = rate(t, appliedKp);
      fsm.pushSample({
        t,
        latencySmoothedMs: 200 + (latencyErrOf ? latencyErrOf(i) : (rng() - 0.5) * 0.02),
        latencyTargetMs: 200,
        resampleRatio: 1 + ppm / 1e6,
        phase: 'stable'
      });
      if (cancelAt === i) fsm.cancel();
      if (ackEvery && i % ackEvery === 0) {
        // Poll for a prompt and answer it; the FSM ignores an ack it did not
        // ask for, so this is safe to fire blind.
        fsm.userAck('perturbation');
        fsm.abbreviate();
      }
      // One-shot ring: raise the flag the first time recovery starts, drop it
      // for good when that recovery ends. A ring on every recovery would make
      // the back-off re-enter tuningKi for ever.
      if (ringOnceDuringRecovery && !ring.spent) {
        const recovering = fsm.getState() === 'perturbationRecovering';
        if (recovering) ring.on = true;
        else if (ring.on) {
          ring.on = false;
          ring.spent = true;
        }
      }
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

  return {
    name,
    // The setup, so the replay can reconstruct the stimulus from data.
    setup: {
      options,
      plant: { kpCrit: 40, noisePpm: 80, ringPpm: 0, ...plantOpts },
      latencyErr,
      ackEvery,
      cancelAt,
      ringOnceDuringRecovery,
    },
    finalState: fsm.getState(),
    context: fsm.getContext(),
    events,
    steps,
  };
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
    cancelAt: 200
  }),

  // A latency error too large to converge, so `tuningKi` has to iterate
  // instead of settling on the first palier. Without this the ki branches
  // (too-slow, diverging, overshoot, the iteration cap) are never recorded —
  // every other scenario converges immediately and walks straight past them.
  record({
    name: 'kiIterates',
    samples: 4000,
    options: FAST,
    latencyErr: { kind: 'sine', offset: 3, amp: 2, periodSamples: 40 }
  }),

  // The `too-slow` branch of tuningKi, which nothing above reaches: it needs an
  // error that neither converges (so the palier ends undecided), nor worsens
  // (that is `diverging`), nor improves by the 20 % the heuristic wants (that
  // is `still-converging`). A barely-decaying error is the only shape left —
  // half-mean ratio ≈ 0.997, well inside the gap between "not worse" and
  // "meaningfully better". Peak ≈ mean, so it is not read as overshoot either.
  record({
    name: 'kiTooSlow',
    samples: 4000,
    options: FAST,
    latencyErr: { kind: 'decay', from: 3, tauSamples: 20000 }
  }),

  // The ki back-off after a perturbation that leaves the loop ringing: the one
  // path where the machine *lowers* ki (×0.7) and re-enters tuningKi with the
  // iteration budget nearly spent. Needs oscillation during recovery
  // regardless of kp — by then kp is 0.6·kpCrit and the plant is quiet — so
  // the runner rings the loop for the duration of the first recovery only.
  //
  // perturbationRecoverMs is 30 s here, not FAST's 14 s: the detector discards
  // a 10 s warm-up and then wants four mean crossings, which at the plant's
  // ~4.4 s period needs about 20 s of usable window. At 14 s it saw a single
  // half-period and read the ringing loop as calm.
  //
  // The noise is raised for a second, unrelated reason: tightening picks the
  // update interval from the rate spread, and at the default 80 ppm every run
  // now lands on the clean branch (5). Before the applied-kp fix above they
  // all landed on the dirty one (10), because the loop never stopped ringing.
  // 200 ppm keeps this run dirty so both branches stay recorded; past ~600 the
  // sweep itself stops detecting oscillation.
  record({
    name: 'perturbationLeavesRinging',
    samples: 16000,
    options: { ...FAST, perturbationRecoverMs: 30000 },
    plantOpts: { ringPpm: 1200, noisePpm: 200 },
    ringOnceDuringRecovery: true,
    ackEvery: 20
  }),

  // Acknowledge the perturbation prompt as soon as it is raised, then let the
  // long run be abbreviated.
  record({
    name: 'fullRunWithAcks',
    samples: 16000,
    options: FAST,
    ackEvery: 20
  })
];

const out = {
  _comment:
    'GENERATED from src/auto-tune/state-machine.js — do not edit. ' +
    'node scripts/dump-auto-tune-runs.mjs > this file. ' +
    'The JS is the reference; the Rust port is asserted against these runs.',
  stepMs: STEP_MS,
  options: FAST,
  // Every scenario overrides the durations, so replaying them proves nothing
  // about the defaults — a port could ship a 15 s kp palier and still replay
  // perfectly. Recorded so the Rust side can assert them field by field.
  defaults: AUTO_TUNE_DEFAULTS,
  runs
};

process.stdout.write(`${JSON.stringify(out)}\n`);
