//! Replay the recorded frontend runs through the Rust state machine.
//!
//! `scripts/dump-auto-tune-runs.mjs` drove the JS machine through a set of
//! scenarios and recorded, for each, the state after every sample, the events
//! it emitted, and the tuning values it held. This walks the same telemetry
//! through [`super::state_machine::AutoTune`] and requires the same sequence.
//!
//! Sequence and values, not just outcome: two implementations can reach the
//! same final kp/ki along different paths, and the path is what patches a live
//! audio loop.
//!
//! The stimulus is rebuilt here rather than replayed from a recorded sample
//! stream, because the plant *reacts* to the kp the machine applies — a run
//! only oscillates because kp got too high. So this file has to reproduce the
//! recorder's plant exactly. Everything that shapes it is read from the
//! recording's per-run `setup` block; nothing is keyed off a scenario name.

#[cfg(test)]
mod tests {
    use crate::auto_tune::state_machine::{Ack, AutoTune, Event, Options, State};
    use crate::auto_tune::wire::{event_name, event_payload};
    use serde_json::Value;

    fn runs() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../scripts/golden/auto-tune-runs.json"
        );
        let text = std::fs::read_to_string(path).expect(
            "recorded runs missing — regenerate with \
             `node scripts/dump-auto-tune-runs.mjs > scripts/golden/auto-tune-runs.json`",
        );
        serde_json::from_str(&text).expect("recording must be valid JSON")
    }

    /// The scenario options the recorder used, over the machine's defaults.
    fn options(recorded: &Value) -> Options {
        let get = |key: &str, fallback: f64| recorded[key].as_f64().unwrap_or(fallback);
        let d = Options::default();
        Options {
            kp_palier_ms: get("kpPalierMs", d.kp_palier_ms),
            ki_palier_ms: get("kiPalierMs", d.ki_palier_ms),
            perturbation_recover_ms: get("perturbationRecoverMs", d.perturbation_recover_ms),
            long_run_default_ms: get("longRunDefaultMs", d.long_run_default_ms),
            long_run_min_abbreviate_ms: get("longRunMinAbbreviateMs", d.long_run_min_abbreviate_ms),
            long_run_stats_window_ms: get("longRunStatsWindowMs", d.long_run_stats_window_ms),
            tightening_palier_ms: get("tighteningPalierMs", d.tightening_palier_ms),
            sample_retention_ms: get("sampleRetentionMs", d.sample_retention_ms),
            kp_max: get("kpMax", d.kp_max),
            ..d
        }
    }

    /// Compare two payloads, tolerating float representation noise. Reports
    /// the first difference as a dotted path so a failure names the field.
    fn payload_diff(ours: &Value, theirs: &Value, path: &str) -> Option<String> {
        match (ours, theirs) {
            (Value::Object(a), Value::Object(b)) => {
                let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
                keys.sort();
                keys.dedup();
                for k in keys {
                    let sub = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    match (a.get(k), b.get(k)) {
                        (Some(x), Some(y)) => {
                            if let Some(d) = payload_diff(x, y, &sub) {
                                return Some(d);
                            }
                        }
                        // The JS omits nothing it sets, so a key on one side
                        // only is a real difference — except for an explicit
                        // null, which serde and JSON.stringify disagree about.
                        (Some(x), None) if !x.is_null() => {
                            return Some(format!("{sub}: we emit {x}, the frontend omits it"))
                        }
                        (None, Some(y)) if !y.is_null() => {
                            return Some(format!("{sub}: we omit it, the frontend emits {y}"))
                        }
                        _ => {}
                    }
                }
                None
            }
            (Value::Array(a), Value::Array(b)) => {
                if a.len() != b.len() {
                    return Some(format!("{path}: {} items, recorded {}", a.len(), b.len()));
                }
                for (i, (x, y)) in a.iter().zip(b).enumerate() {
                    if let Some(d) = payload_diff(x, y, &format!("{path}[{i}]")) {
                        return Some(d);
                    }
                }
                None
            }
            (Value::Number(a), Value::Number(b)) => {
                let (x, y) = (
                    a.as_f64().unwrap_or(f64::NAN),
                    b.as_f64().unwrap_or(f64::NAN),
                );
                // Relative tolerance: these run from 1e-3 (ki) to 1e6 (ppm).
                let scale = x.abs().max(y.abs()).max(1.0);
                if (x - y).abs() > scale * 1e-9 {
                    Some(format!("{path}: {x}, recorded {y}"))
                } else {
                    None
                }
            }
            (a, b) if a == b => None,
            (a, b) => Some(format!("{path}: {a}, recorded {b}")),
        }
    }

    /// The recorded runs all override the durations, so replaying them says
    /// nothing about what the machine does when nobody overrides anything —
    /// which is every real run. Checked field by field against the frontend's
    /// `AUTO_TUNE_DEFAULTS`.
    #[test]
    fn the_defaults_match_the_frontend() {
        let recorded = runs();
        let js = &recorded["defaults"];
        let d = Options::default();
        let pairs: [(&str, f64); 23] = [
            ("initialKp", d.initial_kp),
            ("initialMaxAdjust", d.initial_max_adjust),
            ("initialUpdateInterval", d.initial_update_interval),
            ("kpMax", d.kp_max),
            ("kpPalierMs", d.kp_palier_ms),
            ("kpBaselinePaliers", d.kp_baseline_paliers as f64),
            ("kiPalierMs", d.ki_palier_ms),
            ("kiMaxIterations", f64::from(d.ki_max_iterations)),
            ("kiMin", d.ki_min),
            ("perturbationRecoverMs", d.perturbation_recover_ms),
            ("longRunDefaultMs", d.long_run_default_ms),
            ("longRunMinAbbreviateMs", d.long_run_min_abbreviate_ms),
            ("longRunStatsWindowMs", d.long_run_stats_window_ms),
            ("tighteningPalierMs", d.tightening_palier_ms),
            ("ziegerKpScale", d.zieger_kp_scale),
            ("initialKiFromKpDivisor", d.initial_ki_from_kp_divisor),
            ("sampleRetentionMs", d.sample_retention_ms),
            ("maxAdjustFloor", d.max_adjust_floor),
            ("maxAdjustSafetyMargin", d.max_adjust_safety_margin),
            ("maxAdjustWarnThreshold", d.max_adjust_warn_threshold),
            ("updateIntervalCleanStdPpm", d.update_interval_clean_std_ppm),
            ("updateIntervalClean", d.update_interval_clean),
            ("updateIntervalDefault", d.update_interval_default),
        ];
        for (key, ours) in pairs {
            let theirs = js[key]
                .as_f64()
                .unwrap_or_else(|| panic!("frontend default {key} is missing from the recording"));
            assert!(
                (ours - theirs).abs() < 1e-12,
                "default {key}: Rust has {ours}, the frontend has {theirs}"
            );
        }
        // And nothing new on the frontend side went unnoticed.
        let count = js.as_object().expect("defaults object").len();
        assert_eq!(
            count,
            pairs.len(),
            "the frontend has {count} defaults, this test knows {}",
            pairs.len()
        );
    }

    #[test]
    fn every_recorded_run_replays_identically() {
        let recorded = runs();
        let step_ms = recorded["stepMs"].as_f64().expect("stepMs");
        let run_list = recorded["runs"].as_array().expect("runs");
        assert!(run_list.len() >= 7, "the recording looks truncated");

        for run in run_list {
            let name = run["name"].as_str().unwrap();
            let setup = &run["setup"];
            let opts = options(&setup["options"]);
            let mut plant = Plant::from(&setup["plant"], &setup["latencyErr"]);
            let ack_every = setup["ackEvery"].as_u64();
            let cancel_at = setup["cancelAt"].as_u64();
            let ring_once = setup["ringOnceDuringRecovery"].as_bool().unwrap_or(false);

            let mut fsm = AutoTune::new(opts);
            let mut emitted: Vec<(&'static str, Value)> = Vec::new();
            let mut states: Vec<String> = Vec::new();

            // The kp the plant sees is the last one *applied*, not the one in
            // the context — the two part company as soon as the sweep declares
            // oscillation. See the note in the recorder.
            let mut applied_kp = 0.0_f64;
            let observe = |events: Vec<Event>,
                           emitted: &mut Vec<(&'static str, Value)>,
                           applied_kp: &mut f64| {
                for e in events {
                    if let Event::ApplyParams {
                        kp_near: Some(kp), ..
                    } = e
                    {
                        *applied_kp = kp;
                    }
                    emitted.push((event_name(&e), event_payload(&e)));
                }
            };

            observe(fsm.start(0.0), &mut emitted, &mut applied_kp);

            let recorded_steps = run["steps"].as_array().unwrap();
            let last_index = recorded_steps
                .last()
                .and_then(|s| s["i"].as_u64())
                .unwrap_or(0) as usize;

            for i in 0..=last_index {
                let t = i as f64 * step_ms;
                let ppm = plant.rate_ppm(t, applied_kp, i);
                let sample = crate::auto_tune::detectors::Sample {
                    t,
                    latency_smoothed_ms: Some(200.0 + plant.latency_err(i)),
                    latency_target_ms: Some(200.0),
                    resample_ratio: Some(1.0 + ppm / 1e6),
                    phase: Some("stable".to_string()),
                };
                observe(fsm.push_sample(sample), &mut emitted, &mut applied_kp);

                if cancel_at == Some(i as u64) {
                    observe(fsm.cancel(), &mut emitted, &mut applied_kp);
                }
                if ack_every.is_some_and(|n| i as u64 % n == 0) {
                    observe(
                        fsm.user_ack(Ack::Perturbation, t),
                        &mut emitted,
                        &mut applied_kp,
                    );
                    fsm.abbreviate();
                }
                if ring_once {
                    plant.update_ring(fsm.state() == State::PerturbationRecovering);
                }

                // Compare the tuning values wherever the recorder captured
                // them. Without this the test only checks the *shape* of a
                // run: a port that moves through the same states while
                // computing a different kp or ki passes, which is the failure
                // this whole harness exists to prevent.
                if let Some(step) = recorded_steps
                    .iter()
                    .find(|s| s["i"].as_u64() == Some(i as u64))
                {
                    let p = fsm.progress();
                    let expect = |key: &str| step[key].as_f64();
                    let check = |what: &str, got: f64, want: Option<f64>| {
                        if let Some(w) = want {
                            assert!(
                                (got - w).abs() < 1e-9,
                                "{name} @ i={i}: {what} is {got}, recorded {w}"
                            );
                        }
                    };
                    check("currentKp", p.current_kp, expect("currentKp"));
                    check("currentKi", p.current_ki, expect("currentKi"));
                    for (what, got, key) in [
                        ("kpCrit", p.kp_crit, "kpCrit"),
                        ("kpFinal", p.kp_final, "kpFinal"),
                        ("kiFinal", p.ki_final, "kiFinal"),
                        ("maxAdjustFinal", p.max_adjust_final, "maxAdjustFinal"),
                        (
                            "updateIntervalFinal",
                            p.update_interval_final,
                            "updateIntervalFinal",
                        ),
                    ] {
                        match (got, step[key].as_f64()) {
                            (Some(g), Some(w)) => assert!(
                                (g - w).abs() < 1e-9,
                                "{name} @ i={i}: {what} is {g}, recorded {w}"
                            ),
                            (None, Some(w)) => {
                                panic!("{name} @ i={i}: {what} is unset, recorded {w}")
                            }
                            (Some(g), None) => {
                                panic!("{name} @ i={i}: {what} is {g}, recorded null")
                            }
                            (None, None) => {}
                        }
                    }
                    assert_eq!(
                        u64::from(p.ki_iteration),
                        step["kiIteration"].as_u64().unwrap_or(0),
                        "{name} @ i={i}: kiIteration diverged"
                    );
                }

                states.push(fsm.state().as_str().to_string());
                if fsm.state() == State::Completed
                    || fsm.state() == State::Cancelled
                    || fsm.state() == State::Error
                {
                    break;
                }
            }

            let expected_final = run["finalState"].as_str().unwrap();
            assert_eq!(
                fsm.state().as_str(),
                expected_final,
                "{name}: final state diverged (reached {:?})",
                fsm.state()
            );

            // The distinct states visited, in order — the shape of the run.
            let mut visited: Vec<String> = Vec::new();
            for s in &states {
                if visited.last() != Some(s) {
                    visited.push(s.clone());
                }
            }
            let expected_visited: Vec<String> = {
                let mut v: Vec<String> = Vec::new();
                for s in recorded_steps {
                    let s = s["state"].as_str().unwrap().to_string();
                    if v.last() != Some(&s) {
                        v.push(s);
                    }
                }
                v
            };
            assert_eq!(
                visited, expected_visited,
                "{name}: the sequence of states diverged"
            );

            let recorded_events = run["events"].as_array().unwrap();
            let names: Vec<&str> = emitted.iter().map(|(n, _)| *n).collect();
            let expected_names: Vec<&str> = recorded_events
                .iter()
                .map(|e| e["event"].as_str().unwrap())
                .collect();
            assert_eq!(
                names, expected_names,
                "{name}: the sequence of events diverged"
            );

            // And the payloads, which the wizard reads field by field.
            for (i, ((ev, ours), recorded)) in emitted.iter().zip(recorded_events).enumerate() {
                let theirs = &recorded["payload"];
                if theirs.is_null() && ours.is_null() {
                    continue;
                }
                if let Some(d) = payload_diff(ours, theirs, "") {
                    panic!("{name}: event #{i} ({ev}) payload differs — {d}");
                }
            }
        }
    }

    /// The recorder's plant, reproduced: quiet noise below a critical gain,
    /// growing oscillation above it, plus a one-shot ring the runner raises
    /// during perturbation recovery. Must match
    /// `scripts/dump-auto-tune-runs.mjs` exactly, including the RNG.
    struct Plant {
        kp_crit: f64,
        noise_ppm: f64,
        ring_ppm: f64,
        latency_err: LatencyErr,
        ring_on: bool,
        ring_spent: bool,
    }

    /// The declared shape of the latency error, mirroring `latencyErrFn`.
    enum LatencyErr {
        /// No declared error: the recorder adds its own jitter, and that is the
        /// only case that pulls a *second* random per sample.
        Jitter,
        Sine {
            offset: f64,
            amp: f64,
            period_samples: f64,
        },
        Decay {
            from: f64,
            tau_samples: f64,
        },
    }

    impl Plant {
        fn from(plant: &Value, latency_err: &Value) -> Self {
            let num = |v: &Value, k: &str| v[k].as_f64().expect("plant field");
            let err = match latency_err["kind"].as_str() {
                None => LatencyErr::Jitter,
                Some("sine") => LatencyErr::Sine {
                    offset: num(latency_err, "offset"),
                    amp: num(latency_err, "amp"),
                    period_samples: num(latency_err, "periodSamples"),
                },
                Some("decay") => LatencyErr::Decay {
                    from: num(latency_err, "from"),
                    tau_samples: num(latency_err, "tauSamples"),
                },
                Some(other) => panic!("unknown latency error kind: {other}"),
            };
            Self {
                kp_crit: num(plant, "kpCrit"),
                noise_ppm: num(plant, "noisePpm"),
                ring_ppm: num(plant, "ringPpm"),
                latency_err: err,
                ring_on: false,
                ring_spent: false,
            }
        }

        /// The recorder pulls the rate noise first, then the latency jitter —
        /// unless the scenario declares its own error, in which case only the
        /// first is drawn.
        fn rng_at(&self, index: usize) -> (f64, f64) {
            let pulls_jitter = matches!(self.latency_err, LatencyErr::Jitter);
            let mut state: u32 = 4242;
            let mut next = || {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                state as f64 / 4294967296.0
            };
            let mut a = 0.0;
            let mut b = 0.0;
            for _ in 0..=index {
                a = next();
                if pulls_jitter {
                    b = next();
                }
            }
            (a, b)
        }

        fn rate_ppm(&self, t: f64, kp: f64, index: usize) -> f64 {
            let (r, _) = self.rng_at(index);
            let noise = (r - 0.5) * self.noise_ppm;
            if self.ring_on {
                return self.ring_ppm * (t / 700.0).sin() + noise;
            }
            if kp < self.kp_crit {
                return noise;
            }
            let excess = (kp / self.kp_crit).min(8.0);
            900.0 * excess * (t / 700.0).sin() + noise
        }

        fn latency_err(&self, index: usize) -> f64 {
            match self.latency_err {
                LatencyErr::Jitter => {
                    let (_, r) = self.rng_at(index);
                    (r - 0.5) * 0.02
                }
                LatencyErr::Sine {
                    offset,
                    amp,
                    period_samples,
                } => offset + (index as f64 / period_samples).sin() * amp,
                LatencyErr::Decay { from, tau_samples } => {
                    from * (-(index as f64) / tau_samples).exp()
                }
            }
        }

        /// One-shot ring: raised the first time recovery starts, dropped for
        /// good when that recovery ends.
        fn update_ring(&mut self, recovering: bool) {
            if self.ring_spent {
                return;
            }
            if recovering {
                self.ring_on = true;
            } else if self.ring_on {
                self.ring_on = false;
                self.ring_spent = true;
            }
        }
    }
}
