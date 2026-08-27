//! Replay the recorded frontend runs through the Rust state machine.
//!
//! `scripts/dump-auto-tune-runs.mjs` drove the JS machine through four
//! scenarios and recorded, for each, the state after every sample and the
//! events it emitted. This walks the same telemetry through
//! [`super::state_machine::AutoTune`] and requires the same sequence.
//!
//! Sequence, not outcome: two implementations can reach the same final kp/ki
//! along different paths, and the path is what patches a live audio loop.

#[cfg(test)]
mod tests {
    use crate::auto_tune::state_machine::{Ack, AutoTune, Event, Options, State};
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

    /// The scenario options the recorder used.
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

    /// The event's name as the recorder wrote it.
    fn event_name(event: &Event) -> &'static str {
        match event {
            Event::ApplyParams { .. } => "applyParams",
            Event::Progress { .. } => "progress",
            Event::AwaitUserAction { .. } => "awaitUserAction",
            Event::SourceLost { .. } => "sourceLost",
            Event::SourceRecovered { .. } => "sourceRecovered",
            Event::Complete(_) => "complete",
            Event::Cancelled => "cancelled",
            Event::Error { .. } => "error",
        }
    }

    #[test]
    fn every_recorded_run_replays_identically() {
        let recorded = runs();
        let step_ms = recorded["stepMs"].as_f64().expect("stepMs");
        let base_opts = options(&recorded["options"]);
        let run_list = recorded["runs"].as_array().expect("runs");
        assert!(run_list.len() >= 4, "the recording looks truncated");

        for run in run_list {
            let name = run["name"].as_str().unwrap();
            // `kpNeverOscillates` lowers the ceiling; the recorder stores the
            // merged options per run only implicitly, so recover it from the
            // kp the run gave up at.
            let mut opts = base_opts;
            if name == "kpNeverOscillates" {
                opts.kp_max = 64.0;
            }

            let mut fsm = AutoTune::new(opts);
            let mut emitted: Vec<&'static str> = Vec::new();
            let mut states: Vec<String> = Vec::new();

            for e in fsm.start(0.0) {
                emitted.push(event_name(&e));
            }

            let recorded_steps = run["steps"].as_array().unwrap();
            let last_index = recorded_steps
                .last()
                .and_then(|s| s["i"].as_u64())
                .unwrap_or(0) as usize;

            // `kiIterates` supplies its own latency error and so does not pull
            // the jitter random — the RNG advances once per sample there, twice
            // everywhere else.
            let plant = Plant {
                kp_crit: if name == "kpNeverOscillates" {
                    1e9
                } else {
                    40.0
                },
                pulls_jitter: name != "kiIterates",
            };
            for i in 0..=last_index {
                let t = i as f64 * step_ms;
                let ppm = plant.at(t, fsm.progress().current_kp, i);
                let sample = crate::auto_tune::detectors::Sample {
                    t,
                    latency_smoothed_ms: Some(if name == "kiIterates" {
                        200.0 + 3.0 + (i as f64 / 40.0).sin() * 2.0
                    } else {
                        200.0 + plant.jitter(i)
                    }),
                    latency_target_ms: Some(200.0),
                    resample_ratio: Some(1.0 + ppm / 1e6),
                    phase: Some("stable".to_string()),
                };
                for e in fsm.push_sample(sample) {
                    emitted.push(event_name(&e));
                }

                if name == "cancelledMidSweep" && i == 200 {
                    for e in fsm.cancel() {
                        emitted.push(event_name(&e));
                    }
                }
                if name == "fullRunWithAcks" && i % 20 == 0 {
                    for e in fsm.user_ack(Ack::Perturbation, t) {
                        emitted.push(event_name(&e));
                    }
                    fsm.abbreviate();
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

            let expected_events: Vec<&str> = run["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["event"].as_str().unwrap())
                .collect();
            assert_eq!(
                emitted, expected_events,
                "{name}: the sequence of events diverged"
            );
        }
    }

    /// The recorder's plant, reproduced: quiet noise below a critical gain,
    /// growing oscillation above it. Must match
    /// `scripts/dump-auto-tune-runs.mjs` exactly, including the RNG.
    struct Plant {
        kp_crit: f64,
        /// Whether the scenario also pulls a random for the latency jitter.
        pulls_jitter: bool,
    }

    impl Plant {
        /// The recorder pulls the rate noise first, then the latency jitter —
        /// unless the scenario supplies its own latency error, in which case
        /// only the first is drawn.
        fn rng_at(&self, index: usize) -> (f64, f64) {
            let mut state: u32 = 4242;
            let mut next = || {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                state as f64 / 4294967296.0
            };
            let mut a = 0.0;
            let mut b = 0.0;
            for _ in 0..=index {
                a = next();
                if self.pulls_jitter {
                    b = next();
                }
            }
            (a, b)
        }

        fn at(&self, t: f64, kp: f64, index: usize) -> f64 {
            let (r, _) = self.rng_at(index);
            let noise = (r - 0.5) * 80.0;
            if kp < self.kp_crit {
                return noise;
            }
            let excess = (kp / self.kp_crit).min(8.0);
            900.0 * excess * (t / 700.0).sin() + noise
        }

        fn jitter(&self, index: usize) -> f64 {
            let (_, r) = self.rng_at(index);
            (r - 0.5) * 0.02
        }
    }
}
