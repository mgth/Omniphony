//! The one contract every pluggable component follows.
//!
//! Render backends, bed→height object generators and the phantom-extraction
//! stage are all *plugins*: a stable string id, a label, and the parameters
//! they declare as data ([`ParamSpec`]). This module holds what they share:
//!
//! - [`PluginFactory`], the common trait (`BackendFactory` and
//!   `ObjectGeneratorFactory` extend it with how to build their kind);
//! - [`PluginRegistry`], one registry with one rule: ids are exact, and a
//!   later registration **replaces** an earlier one, so a host can override a
//!   built-in;
//! - [`PluginListing`], the one format a registry publishes to the UI (id,
//!   label, declared parameters — the serde of [`ParamSpec`]);
//! - [`PluginParams`], the one value store (`kind → plugin id → key →
//!   ParamValue`) held by `RendererControl`, written by the OSC controls and
//!   kept by the Save button, never at once (docs/persistence-policy.md).
//!
//! See `docs/custom-render-backend-integration.md` for a contributor's view.

use std::collections::HashMap;

use crate::backend_params::{ParamSpec, ParamValue};
use crate::config::RenderConfig;

/// What every plugin declares, whatever it builds.
pub trait PluginFactory: Send + Sync {
    /// Stable identifier the host selects it by (e.g. `"vbap"`, `"pad"`).
    /// Matched exactly.
    fn id(&self) -> &'static str;
    /// Human-facing name shown in the UI. Defaults to the id.
    fn label(&self) -> &'static str {
        self.id()
    }
    /// Key of a localized name in Studio's catalogues (built-ins). `None`
    /// shows `label` as is.
    fn i18n_key(&self) -> Option<&'static str> {
        None
    }
    /// Tunable parameters, as data. The host stores values generically and
    /// the UI renders controls from this. Defaults to none.
    fn param_schema(&self) -> Vec<ParamSpec> {
        Vec::new()
    }
    /// Like [`param_schema`](Self::param_schema), with the plugin's stored
    /// values, so a plugin whose schema depends on its own state can report a
    /// *dynamic* schema (the scriptable backend exposes the params its
    /// selected `.lua` declares). Defaults to the static schema.
    fn param_schema_for(&self, params: &ParamMap) -> Vec<ParamSpec> {
        let _ = params;
        self.param_schema()
    }
}

/// One plugin's values: parameter key → value.
pub type ParamMap = HashMap<String, ParamValue>;
/// One kind's values: plugin id → [`ParamMap`].
pub type ParamBag = HashMap<String, ParamMap>;

/// A registered plugin's UI-facing identity and declared parameters — the
/// format every registry publishes (`available_backends`,
/// `/state/object_generators`, `/state/phantom`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginListing {
    pub id: &'static str,
    pub label: &'static str,
    #[serde(rename = "i18nKey", skip_serializing_if = "Option::is_none")]
    pub i18n_key: Option<&'static str>,
    pub params: Vec<ParamSpec>,
}

impl PluginListing {
    /// The listing of `factory`, with its static schema.
    pub fn of<F: PluginFactory + ?Sized>(factory: &F) -> Self {
        Self::with_params(factory, factory.param_schema())
    }

    fn with_params<F: PluginFactory + ?Sized>(factory: &F, params: Vec<ParamSpec>) -> Self {
        Self {
            id: factory.id(),
            label: factory.label(),
            i18n_key: factory.i18n_key(),
            params,
        }
    }

    /// The declared parameter `key`, if any.
    pub fn spec(&self, key: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|spec| spec.key == key)
    }
}

/// Ordered set of plugin factories of one kind, keyed by exact id.
pub struct PluginRegistry<F: ?Sized> {
    factories: Vec<Box<F>>,
}

impl<F: ?Sized> PluginRegistry<F> {
    pub const fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }
}

impl<F: PluginFactory + ?Sized> PluginRegistry<F> {
    /// Register a plugin. A later registration with the same id replaces the
    /// earlier one, in its place, so a host can override a built-in.
    pub fn register(&mut self, factory: Box<F>) {
        let id = factory.id();
        match self.factories.iter_mut().find(|f| f.id() == id) {
            Some(slot) => *slot = factory,
            None => self.factories.push(factory),
        }
    }

    /// Look a plugin up by its exact id.
    pub fn get(&self, id: &str) -> Option<&F> {
        self.factories
            .iter()
            .find(|f| f.id() == id)
            .map(|f| f.as_ref())
    }

    /// Every registered plugin, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &F> + '_ {
        self.factories.iter().map(|f| f.as_ref())
    }

    /// Ids of all registered plugins, in registration order.
    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.factories.iter().map(|f| f.id())
    }

    /// The listing of every registered plugin, with its static schema.
    pub fn listings(&self) -> Vec<PluginListing> {
        self.iter().map(PluginListing::of).collect()
    }

    /// Like [`listings`](Self::listings), resolving each plugin's *dynamic*
    /// schema against its stored values (`bag` is keyed by plugin id).
    pub fn listings_with(&self, bag: &ParamBag) -> Vec<PluginListing> {
        let empty = ParamMap::new();
        self.iter()
            .map(|f| {
                PluginListing::with_params(f, f.param_schema_for(bag.get(f.id()).unwrap_or(&empty)))
            })
            .collect()
    }
}

/// The kinds of plugin, each with its own id namespace in [`PluginParams`]:
/// a contributor's generator named like a backend cannot collide with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginKind {
    /// Render backends (`render.backend_params`).
    Backend,
    /// Bed→height object generators (`render.generator_params`).
    ObjectGenerator,
    /// The phantom-extraction stage, one plugin
    /// ([`PHANTOM_EXTRACT_ID`]; `render.phantom_extract_params`).
    PhantomExtract,
}

impl PluginKind {
    pub const ALL: [PluginKind; 3] = [
        PluginKind::Backend,
        PluginKind::ObjectGenerator,
        PluginKind::PhantomExtract,
    ];

    fn index(self) -> usize {
        match self {
            PluginKind::Backend => 0,
            PluginKind::ObjectGenerator => 1,
            PluginKind::PhantomExtract => 2,
        }
    }
}

/// The id the phantom-extraction stage is listed and stored under.
pub const PHANTOM_EXTRACT_ID: &str = "phantom_extract";

/// Every plugin's stored values: `kind → plugin id → key → value`. Sparse:
/// an absent key is the plugin's declared default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginParams {
    bags: [ParamBag; 3],
}

impl PluginParams {
    pub fn bag(&self, kind: PluginKind) -> &ParamBag {
        &self.bags[kind.index()]
    }

    pub fn bag_mut(&mut self, kind: PluginKind) -> &mut ParamBag {
        &mut self.bags[kind.index()]
    }

    /// The values of one plugin, if it has any.
    pub fn plugin(&self, kind: PluginKind, id: &str) -> Option<&ParamMap> {
        self.bag(kind).get(id)
    }

    pub fn get(&self, kind: PluginKind, id: &str, key: &str) -> Option<&ParamValue> {
        self.plugin(kind, id).and_then(|values| values.get(key))
    }

    pub fn set(&mut self, kind: PluginKind, id: &str, key: &str, value: ParamValue) {
        self.bag_mut(kind)
            .entry(id.to_string())
            .or_default()
            .insert(key.to_string(), value);
    }

    pub fn is_empty(&self) -> bool {
        self.bags.iter().all(HashMap::is_empty)
    }

    /// Read every kind's values from a config: the current keys, then the
    /// read-only legacy ones migrated into them — a legacy value wins, as it
    /// did when it was the only place the value lived.
    ///
    /// - `object_generator_params` held one flat map for "the active
    ///   generator", cleared whenever the selection changed: it belongs to
    ///   `object_generator_id`. Without a selected generator it was inert (the
    ///   next selection cleared it) and has nowhere to go.
    /// - `phantom_params` is the phantom stage's map; its old `method` entry
    ///   is `phantom_extract_mode`'s migration, not a parameter.
    pub fn from_config(render: &RenderConfig) -> Self {
        let mut params = Self::default();
        *params.bag_mut(PluginKind::Backend) = render.backend_params.clone();
        *params.bag_mut(PluginKind::ObjectGenerator) = render.generator_params.clone();
        if !render.phantom_extract_params.is_empty() {
            params.bag_mut(PluginKind::PhantomExtract).insert(
                PHANTOM_EXTRACT_ID.to_string(),
                render.phantom_extract_params.clone(),
            );
        }
        if let (Some(legacy), Some(id)) = (
            render.object_generator_params.as_ref(),
            crate::config_fields::object_generator_id::get(render)
                .filter(|id| !id.trim().is_empty() && !id.trim().eq_ignore_ascii_case("none")),
        ) {
            for (key, value) in legacy {
                params.set(
                    PluginKind::ObjectGenerator,
                    id.trim(),
                    key,
                    ParamValue::Float(*value),
                );
            }
        }
        if let Some(legacy) = render.phantom_params.as_ref() {
            for (key, value) in legacy.iter().filter(|(key, _)| *key != "method") {
                params.set(
                    PluginKind::PhantomExtract,
                    PHANTOM_EXTRACT_ID,
                    key,
                    ParamValue::Float(*value),
                );
            }
        }
        params
    }

    /// Write every kind's values into a config (an empty map keeps its key
    /// out of the file) and drop the legacy keys they were migrated from.
    pub fn store_to_config(&self, render: &mut RenderConfig) {
        render.backend_params = self.bag(PluginKind::Backend).clone();
        render.generator_params = self
            .bag(PluginKind::ObjectGenerator)
            .iter()
            .filter(|(_, values)| !values.is_empty())
            .map(|(id, values)| (id.clone(), values.clone()))
            .collect();
        render.phantom_extract_params = self
            .plugin(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID)
            .cloned()
            .unwrap_or_default();
        render.object_generator_params = None;
        render.phantom_params = None;
    }

    /// Put every value that has a declared parameter in that parameter's
    /// type ([`ParamSpec::coerce`]); a value that cannot be read as one, or
    /// that no listing declares, is kept as it is — the plugin falls back to
    /// its default for it, and the next Save does not lose it.
    pub fn canonicalize(&mut self, kind: PluginKind, listings: &[PluginListing]) {
        for (id, values) in self.bag_mut(kind).iter_mut() {
            let Some(listing) = listings.iter().find(|l| l.id == id) else {
                continue;
            };
            for (key, value) in values.iter_mut() {
                if let Some(coerced) = listing.spec(key).and_then(|spec| spec.coerce(value)) {
                    *value = coerced;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy(&'static str, &'static str);
    impl PluginFactory for Dummy {
        fn id(&self) -> &'static str {
            self.0
        }
        fn label(&self) -> &'static str {
            self.1
        }
        fn param_schema(&self) -> Vec<ParamSpec> {
            vec![ParamSpec::bool("on", "On", false)]
        }
    }

    #[test]
    fn a_later_registration_replaces_the_earlier_one_in_place() {
        let mut registry: PluginRegistry<dyn PluginFactory> = PluginRegistry::new();
        registry.register(Box::new(Dummy("a", "A")));
        registry.register(Box::new(Dummy("b", "B")));
        registry.register(Box::new(Dummy("a", "A2")));
        assert_eq!(registry.ids().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(registry.get("a").map(|f| f.label()), Some("A2"));
    }

    #[test]
    fn ids_are_exact() {
        let mut registry: PluginRegistry<dyn PluginFactory> = PluginRegistry::new();
        registry.register(Box::new(Dummy("pad", "PAD")));
        assert!(registry.get("pad").is_some());
        assert!(registry.get("PAD").is_none());
        assert!(registry.get(" pad").is_none());
    }

    #[test]
    fn a_listing_serialises_like_a_backend_listing() {
        let listing = PluginListing::of(&Dummy("a", "A"));
        let json = serde_json::to_value(&listing).unwrap();
        assert_eq!(json["id"], "a");
        assert_eq!(json["label"], "A");
        assert!(json.get("i18nKey").is_none());
        assert_eq!(json["params"][0]["kind"]["type"], "bool");
        assert_eq!(json["params"][0]["default"], false);
    }

    #[test]
    fn kinds_are_separate_namespaces() {
        let mut params = PluginParams::default();
        params.set(PluginKind::Backend, "x", "k", ParamValue::Float(1.0));
        params.set(
            PluginKind::ObjectGenerator,
            "x",
            "k",
            ParamValue::Float(2.0),
        );
        assert_eq!(
            params.get(PluginKind::Backend, "x", "k"),
            Some(&ParamValue::Float(1.0))
        );
        assert_eq!(
            params.get(PluginKind::ObjectGenerator, "x", "k"),
            Some(&ParamValue::Float(2.0))
        );
        assert!(params.plugin(PluginKind::PhantomExtract, "x").is_none());
    }

    #[test]
    fn legacy_keys_migrate_into_the_new_ones_and_are_dropped_on_store() {
        let mut render = RenderConfig {
            object_generator_id: Some("pad".to_string()),
            object_generator_params: Some(HashMap::from([("strength".to_string(), 0.8)])),
            phantom_params: Some(HashMap::from([
                ("method".to_string(), 1.0),
                ("center".to_string(), 1.0),
                ("passes".to_string(), 2.0),
            ])),
            ..Default::default()
        };
        let params = PluginParams::from_config(&render);
        assert_eq!(
            params.get(PluginKind::ObjectGenerator, "pad", "strength"),
            Some(&ParamValue::Float(0.8))
        );
        let phantom = params
            .plugin(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID)
            .unwrap();
        assert_eq!(phantom.len(), 2, "method is a mode migration, not a param");

        params.store_to_config(&mut render);
        assert_eq!(render.object_generator_params, None);
        assert_eq!(render.phantom_params, None);
        assert_eq!(
            render.generator_params["pad"]["strength"],
            ParamValue::Float(0.8)
        );
        assert_eq!(
            render.phantom_extract_params["passes"],
            ParamValue::Float(2.0)
        );
        // What was stored reads back the same, from the new keys alone.
        assert_eq!(PluginParams::from_config(&render), params);
    }

    #[test]
    fn a_legacy_generator_map_without_a_generator_has_nowhere_to_go() {
        let render = RenderConfig {
            object_generator_id: Some("none".to_string()),
            object_generator_params: Some(HashMap::from([("strength".to_string(), 0.8)])),
            ..Default::default()
        };
        assert!(PluginParams::from_config(&render).is_empty());
    }

    #[test]
    fn canonicalize_types_declared_values_and_keeps_the_rest() {
        let mut params = PluginParams::default();
        params.set(
            PluginKind::PhantomExtract,
            "d",
            "on",
            ParamValue::Float(1.0),
        );
        params.set(
            PluginKind::PhantomExtract,
            "d",
            "zzz",
            ParamValue::Float(3.0),
        );
        params.canonicalize(
            PluginKind::PhantomExtract,
            &[PluginListing::of(&Dummy("d", "D"))],
        );
        assert_eq!(
            params.get(PluginKind::PhantomExtract, "d", "on"),
            Some(&ParamValue::Bool(true))
        );
        assert_eq!(
            params.get(PluginKind::PhantomExtract, "d", "zzz"),
            Some(&ParamValue::Float(3.0))
        );
    }
}
