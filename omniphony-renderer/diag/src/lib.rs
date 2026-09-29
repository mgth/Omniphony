//! Generic diagnostic-metric registry.
//!
//! Lets any thread in the renderer publish named, grouped, labelled metrics
//! that the Studio plot discovers dynamically. The intent is to make adding
//! a new instrumentation point a 2-line change at the producer site, with
//! zero plumbing through OSC/Tauri/JS:
//!
//! ```ignore
//! let metric = diag.register("my_metric", "My metric", "iec958", "us");
//! // ... in hot path:
//! metric.store((value as f64).to_bits(), Ordering::Relaxed);
//! ```
//!
//! The Studio listens to a single `diag:schema` event (sent when the set of
//! registered metrics changes) plus a `diag:values` event sent at the same
//! cadence as the meter bundle. The user picks which metrics to plot at
//! runtime via a multi-select dropdown.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiagSchemaItem {
    pub name: String,
    pub label: String,
    pub group: String,
    pub unit: String,
    /// Which tab the Studio diag plot shows this metric in: "base" (obvious,
    /// clearly-meaningful signals) or "advanced" (everything else). Defaults
    /// to "advanced" at registration; the renderer promotes the base set via
    /// `set_tier`.
    pub tier: String,
}

/// Pre-allocated metric handle exposed by a producer (e.g. an audio backend
/// that does not depend on the registry crate). Bundles the atomic with its
/// schema description so the consumer can `register_external` it in one call.
#[derive(Clone)]
pub struct DiagAtomicHandle {
    pub name: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub unit: &'static str,
    pub atomic: Arc<AtomicU64>,
}

struct DiagEntry {
    schema: DiagSchemaItem,
    value: Arc<AtomicU64>,
}

pub struct DiagRegistry {
    entries: Mutex<BTreeMap<String, DiagEntry>>,
}

impl DiagRegistry {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Register (or look up) a metric. Returns a shared atomic holding the
    /// metric value as `f64` bits — store via `(value as f64).to_bits()`.
    /// Idempotent: calling twice with the same name returns the same handle.
    pub fn register(&self, name: &str, label: &str, group: &str, unit: &str) -> Arc<AtomicU64> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(existing) = entries.get(name) {
            return Arc::clone(&existing.value);
        }
        let value = Arc::new(AtomicU64::new(0));
        entries.insert(
            name.to_string(),
            DiagEntry {
                schema: DiagSchemaItem {
                    name: name.to_string(),
                    label: label.to_string(),
                    group: group.to_string(),
                    unit: unit.to_string(),
                    tier: "advanced".to_string(),
                },
                value: Arc::clone(&value),
            },
        );
        value
    }

    /// Register a metric backed by a pre-existing atomic handle. Useful when
    /// the writer (e.g. an audio backend that pre-allocates its atomics)
    /// just wants them exposed in the schema/values stream.
    ///
    /// Idempotent: no-op if the same `Arc` is already registered under that
    /// name. Replaces the entry if the `Arc` is different — necessary when
    /// a producer (audio writer) is recreated and hands in a NEW atomic
    /// handle; without this the diag plot would keep reading the dead
    /// atomic of the previous incarnation and the metric would appear
    /// frozen.
    pub fn register_external(
        &self,
        name: &str,
        label: &str,
        group: &str,
        unit: &str,
        value: Arc<AtomicU64>,
    ) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(existing) = entries.get(name) {
            if Arc::ptr_eq(&existing.value, &value) {
                return;
            }
        }
        entries.insert(
            name.to_string(),
            DiagEntry {
                schema: DiagSchemaItem {
                    name: name.to_string(),
                    label: label.to_string(),
                    group: group.to_string(),
                    unit: unit.to_string(),
                    tier: "advanced".to_string(),
                },
                value,
            },
        );
    }

    /// Override the tier ("base" | "advanced") of an already-registered
    /// metric. No-op if the metric isn't registered yet. Lets the renderer
    /// classify which metrics appear in the diag plot's "base" tab; everything
    /// left at the registration default stays in "advanced".
    pub fn set_tier(&self, name: &str, tier: &str) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(entry) = entries.get_mut(name) {
            if entry.schema.tier != tier {
                entry.schema.tier = tier.to_string();
            }
        }
    }

    /// Returns the schema as JSON if any metric is registered, else `None`.
    /// Cheap enough to emit every meter-bundle tick — the Studio only rebuilds
    /// the chip row when the metric set actually changes.
    pub fn schema_json(&self) -> Option<String> {
        let entries = self.entries.lock().unwrap();
        if entries.is_empty() {
            return None;
        }
        let items: Vec<&DiagSchemaItem> = entries.values().map(|e| &e.schema).collect();
        serde_json::to_string(&serde_json::json!({ "items": items })).ok()
    }

    /// Returns the current values of every registered metric as a JSON object
    /// keyed by metric name. Sent every meter bundle so the Studio plot can
    /// trace selected metrics over time.
    pub fn values_json(&self) -> Option<String> {
        let entries = self.entries.lock().unwrap();
        if entries.is_empty() {
            return None;
        }
        let obj: BTreeMap<&str, f64> = entries
            .iter()
            .map(|(name, e)| {
                (
                    name.as_str(),
                    f64::from_bits(e.value.load(Ordering::Relaxed)),
                )
            })
            .collect();
        serde_json::to_string(&obj).ok()
    }
}

impl Default for DiagRegistry {
    fn default() -> Self {
        Self::new()
    }
}
