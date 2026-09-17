//! Reception-timed, bounded diagnostic history independent of UI frames.
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    time::{Duration, Instant},
};
pub const MAX_METRICS: usize = 64;
pub const MAX_SAMPLES: usize = 12_001; // 60 seconds at the maximum 200 Hz rate
pub const WINDOW: Duration = Duration::from_secs(60);
pub type Series = HashMap<String, VecDeque<(f64, f64)>>;
struct Sample {
    sequence: u64,
    at: Instant,
    value: f64,
}
pub struct History {
    started: Instant,
    sequence: u64,
    series: HashMap<String, VecDeque<Sample>>,
    latest: Instant,
}
impl Default for History {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            latest: now,
            sequence: 0,
            series: HashMap::new(),
        }
    }
}
/// A UI-owned cache. Pause freezes this copy; collection continues in History.
#[derive(Default)]
pub struct Trace {
    pub series: Series,
    pub end_ms: f64,
    started: Option<Instant>,
    sequence: u64,
}
impl History {
    /// A producer change invalidates data, not the view's collection interests.
    pub fn restart(&mut self) {
        self.started = Instant::now();
        self.latest = self.started;
        self.sequence = 0;
        for samples in self.series.values_mut() {
            samples.clear();
        }
    }

    pub fn select(&mut self, selected: &BTreeSet<String>) {
        self.series.retain(|key, _| selected.contains(key));
        for key in selected.iter().take(MAX_METRICS) {
            if !self.series.contains_key(key) && self.series.len() < MAX_METRICS {
                self.series.insert(key.clone(), VecDeque::new());
            }
        }
    }
    pub fn record(&mut self, values: &serde_json::Value, at: Instant) {
        self.sequence = self.sequence.wrapping_add(1);
        self.latest = at;
        for (key, samples) in &mut self.series {
            while samples
                .front()
                .is_some_and(|s| at.saturating_duration_since(s.at) > WINDOW)
            {
                samples.pop_front();
            }
            if let Some(value) = values
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .filter(|v| v.is_finite())
            {
                if samples.len() >= MAX_SAMPLES {
                    samples.pop_front();
                }
                samples.push_back(Sample {
                    sequence: self.sequence,
                    at,
                    value,
                });
            }
        }
    }
    /// Copy only arrivals newer than the UI cursor; repeated paints add nothing.
    pub fn copy_to(&self, trace: &mut Trace) {
        if trace.started != Some(self.started) {
            *trace = Trace {
                started: Some(self.started),
                ..Default::default()
            };
        }
        trace.series.retain(|key, _| self.series.contains_key(key));
        trace.end_ms = self
            .latest
            .saturating_duration_since(self.started)
            .as_secs_f64()
            * 1000.0;
        let cutoff = trace.end_ms - WINDOW.as_secs_f64() * 1000.0;
        for (key, samples) in &self.series {
            let target = trace.series.entry(key.clone()).or_default();
            let first = samples.partition_point(|s| s.sequence <= trace.sequence);
            target.extend(samples.range(first..).map(|s| {
                (
                    s.at.saturating_duration_since(self.started).as_secs_f64() * 1000.0,
                    s.value,
                )
            }));
            while target.len() > MAX_SAMPLES || target.front().is_some_and(|(t, _)| *t < cutoff) {
                target.pop_front();
            }
        }
        trace.sequence = self.sequence;
    }
}
pub fn select(state: &super::commands::SharedState, selected: &BTreeSet<String>) {
    state.inner.lock().unwrap().diagnostics.select(selected);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arrivals_survive_missing_frames_without_duplicate_paint_samples() {
        let mut history = History::default();
        history.select(&BTreeSet::from(["x".into()]));
        let start = history.started;
        for i in 0..5 {
            history.record(
                &serde_json::json!({"x": i}),
                start + Duration::from_millis(i * 10),
            );
        }
        let mut trace = Trace::default();
        history.copy_to(&mut trace);
        history.copy_to(&mut trace);
        assert_eq!(trace.series["x"].len(), 5);
        assert_eq!(trace.series["x"][4], (40.0, 4.0));
        history.record(&serde_json::json!({"x": 8}), start + Duration::from_secs(2));
        assert_eq!(trace.end_ms, 40.0); // paused view is untouched
        history.copy_to(&mut trace);
        assert_eq!(trace.end_ms, 2000.0);
    }
    #[test]
    fn history_is_bounded_and_reconnect_clears_the_view() {
        let mut history = History::default();
        history.select(&(0..100).map(|i| i.to_string()).collect());
        assert_eq!(history.series.len(), MAX_METRICS);
        for _ in 0..MAX_SAMPLES + 10 {
            history.record(&serde_json::json!({"0": 1}), history.started);
        }
        assert_eq!(history.series["0"].len(), MAX_SAMPLES);
        history.record(
            &serde_json::json!({"0": 2}),
            history.started + WINDOW + Duration::from_millis(1),
        );
        assert_eq!(history.series["0"].len(), 1);
        let mut trace = Trace::default();
        history.copy_to(&mut trace);
        History::default().copy_to(&mut trace);
        assert!(trace.series.is_empty());
    }
    #[test]
    fn missing_or_invalid_metrics_do_not_fabricate_values() {
        let mut history = History::default();
        history.select(&BTreeSet::from(["x".into()]));
        history.record(&serde_json::json!({"y": 2}), history.started);
        history.record(&serde_json::json!({"x": null}), history.started);
        assert!(history.series["x"].is_empty());
    }
}
