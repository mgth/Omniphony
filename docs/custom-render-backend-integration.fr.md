# Integrer un Backend de Rendu Personnalise

## But

Ce document explique comment ajouter son propre modele de gains / backend de rendu dans `omniphony-renderer` apres le refactor recent.

L'objectif est de permettre a un contributeur de:

- comprendre ou se brancher
- implementer son propre calcul de gains
- declarer ses capacites
- l'exposer au runtime et a Studio

Ce guide parle de "backend" au sens Omniphony actuel:

- un **modele de gains** concret
- prepare puis execute par le pipeline de rendu

## Vue d'ensemble

L'architecture pertinente est maintenant separee en quatre couches:

1. `GainModel`
2. `PreparedRenderEngine`
3. `TopologyBuildPlan`
4. UI/runtime pilotee par `backend_id` et capacites

Les points d'entree principaux sont:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)
- [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)
- [`live_params.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/live_params.rs)
- [`snapshot.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/snapshot.rs)
- [`vbap.js`](/home/user/dev/spatial-renderer/Omniphony/omniphony-studio/src/controls/vbap.js)

## Architecture actuelle

### 1. Identite backend

L'identite produit d'un backend est centralisee dans:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

Le registre statique contient:

- `BackendDescriptor`
- `backend_descriptors()`
- `backend_descriptor()`
- `backend_descriptor_by_id()`

Chaque backend possede:

- un `backend_id` stable, par exemple `vbap`
- un label utilisateur, par exemple `VBAP`
- un `RenderBackendKind`
- un `GainModelKind`

### 2. Contrat du modele

Un backend concret implemente le trait `GainModel` dans:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

Le contrat actuel expose:

```rust
pub trait GainModel: Send + Sync + 'static {
    fn kind(&self) -> GainModelKind;
    fn backend_id(&self) -> &'static str;
    fn backend_label(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;
    fn speaker_count(&self) -> usize;
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse;
    fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()>;
}
```

Le hot path audio consomme ensuite un `PreparedRenderEngine`, pas votre type concret directement.

### 3. Build / rebuild backend

La preparation de topologie est centralisee dans:

- [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)

Ce module contient:

- `BackendBuildPlan`
- un plan de build concret par backend
- `TopologyBuildPlan`
- `prepare_topology_build_plan(...)`

Le runtime ne choisit plus son backend via un gros `match` dans `live_params.rs`.
`live_params.rs` rassemble les entrees live, puis delegue au registre.

### 4. Capacites backend

Les capacites sont exposees par `BackendCapabilities` dans:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

Elles pilotent:

- le snapshot runtime
- l'OSC state
- l'affichage Studio

Studio ne doit plus raisonner avec:

- `if backend == vbap`

mais avec des capacites comme:

- `supports_spread`
- `supports_distance_model`
- `supports_precomputed_cartesian`
- `supports_precomputed_polar`

## Procedure d'integration

## Etape 1: declarer l'identite du backend

Ajouter un descripteur dans le registre de:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

Exemple conceptuel:

```rust
BackendDescriptor {
    kind: RenderBackendKind::MyModel,
    gain_model_kind: GainModelKind::MyModel,
    id: "my_model",
    label: "My Model",
}
```

Aujourd'hui cela suppose encore d'ajouter une variante enum:

- `RenderBackendKind`
- `GainModelKind`

Le cout d'integration est cependant borne a cet endroit pour l'identite produit.

## Etape 2: implementer le modele de gains

Ajouter votre struct concrete dans:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)

ou dans un module dedie si le code grossit.

Exemple minimal:

```rust
pub struct MyModelBackend {
    speaker_positions: Vec<[f32; 3]>,
}

impl GainModel for MyModelBackend {
    fn kind(&self) -> GainModelKind { ... }
    fn backend_id(&self) -> &'static str { "my_model" }
    fn backend_label(&self) -> &'static str { "My Model" }
    fn capabilities(&self) -> BackendCapabilities { ... }
    fn speaker_count(&self) -> usize { ... }
    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse { ... }
    fn save_to_file(&self, path: &Path, speaker_layout: &SpeakerLayout) -> Result<()> { ... }
}
```

### Recommandations runtime

- ne faites pas d'allocation dans `compute_gains()`
- n'utilisez pas de `HashMap` dans le hot path
- utilisez des buffers ou tables prepares lors du build si necessaire
- gardez le format de sortie sous forme de `Gains`

## Etape 3: declarer les capacites

Dans `capabilities()`, declarez seulement ce qui est reellement supporte.

Exemple:

```rust
BackendCapabilities {
    supports_realtime: true,
    supports_precomputed_polar: false,
    supports_precomputed_cartesian: true,
    supports_position_interpolation: true,
    supports_distance_model: false,
    supports_spread: false,
    supports_spread_from_distance: false,
    supports_distance_diffuse: false,
    supports_heatmap_cartesian: true,
    supports_table_export: false,
}
```

Ces flags ont des effets visibles:

- modes d'evaluation disponibles
- sections masquees/affichees dans Studio
- debug heatmap
- export eventuel de table

Ne sur-declarez pas une capacite "pour plus tard". La UI et le runtime se fient a ces flags.

## Etape 4: ajouter un plan de build backend

Ajouter un plan de build concret dans:

- [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)

Exemple:

```rust
#[derive(Clone)]
pub struct MyModelBuildPlan {
    pub speaker_positions: Vec<[f32; 3]>,
    pub custom_param: f32,
}

impl MyModelBuildPlan {
    pub fn build_gain_model(&self) -> Result<Box<dyn GainModel>> {
        Ok(Box::new(MyModelBackend::new(...)))
    }
}
```

Puis brancher ce plan dans:

- `BackendBuildPlan`
- `TopologyBuildPlan::build_topology()`
- `TopologyBuildPlan::log_summary()`

## Etape 5: brancher la preparation du plan

Le dernier point de raccord est:

- `prepare_topology_build_plan(...)`
  dans [`backend_registry.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/backend_registry.rs)

Cette fonction recoit:

- le `layout`
- les `LiveParams`
- les `BackendRebuildParams`
- la config d'evaluation construite par le runtime

Elle doit:

1. reconnaitre `backend_id`
2. construire le `BackendBuildPlan` adapte
3. choisir le `evaluation_mode` effectif
4. retourner un `TopologyBuildPlan`

Exemple conceptuel:

```rust
match live.backend_id() {
    "my_model" => Some(TopologyBuildPlan {
        layout,
        backend_id: "my_model".to_string(),
        backend_build: BackendBuildPlan::MyModel(MyModelBuildPlan { ... }),
        evaluation_mode: ...,
        evaluation_build_config,
    }),
    ...
}
```

## Etape 6: definir les parametres de rebuild

Si votre backend a besoin de parametres propres pour reconstruire sa topologie apres une mise a jour live, etendez:

- [`live_params.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/live_params.rs)

Aujourd'hui `BackendRebuildParams` contient encore un bloc `vbap`.

Si votre backend a des besoins de rebuild persistants:

- ajoutez un bloc backend-specifique equivalent
- conservez `backend_id` comme cle de selection
- evitez de recoder la logique de selection ailleurs

## Etape 7: exposer le backend a l'OSC et a la config

Le parsing backend passe encore par:

- [`render_backend.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/renderer/src/render_backend.rs)
  avec `RenderBackendKind::from_str()`

et est utilise notamment dans:

- [`runtime_control/src/osc.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/osc.rs)
- [`src/cli/decode/bootstrap.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/bootstrap.rs)

Il faut donc:

- ajouter votre backend dans le registre d'identite
- verifier qu'il est accepte par `from_str()`

Si vous voulez qu'il soit selectionnable par config, verifier aussi:

- [`runtime_control/src/persist.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/persist.rs)

## Etape 8: exposer les capacites a Studio

Le snapshot runtime diffuse l'etat backend dans:

- [`snapshot.rs`](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/runtime_control/src/snapshot.rs)

Studio le consomme dans:

- [`tauri-bridge.js`](/home/user/dev/spatial-renderer/Omniphony/omniphony-studio/src/tauri-bridge.js)
- [`vbap.js`](/home/user/dev/spatial-renderer/Omniphony/omniphony-studio/src/controls/vbap.js)

En theorie, si vos capacites sont correctes, la plupart des sections UI s'adapteront deja.

En pratique, verifier au minimum:

- le nom affiche du backend
- les modes d'evaluation disponibles
- les sections masquees/affichees
- le comportement du heatmap si supporte

## Etape 9: valider

Minimum recommande:

1. `cargo fmt`
2. `cargo check` dans `omniphony-renderer`
3. `cargo check` dans `omniphony-studio/src-tauri`
4. test manuel de selection backend
5. test manuel de rebuild si le layout change

## Checklist rapide

- ajouter un descripteur backend
- ajouter ou etendre `RenderBackendKind`
- ajouter ou etendre `GainModelKind`
- implementer `GainModel`
- declarer `BackendCapabilities`
- ajouter un plan de build backend
- brancher `prepare_topology_build_plan(...)`
- verifier `from_str()` / config / OSC
- verifier Studio
- verifier `cargo check`

## Conseils de conception

### Si votre modele est purement temps reel

- supportez seulement `supports_realtime`
- laissez `supports_precomputed_* = false`
- gardez le build plan simple

### Si votre modele a besoin de caches

- construisez-les dans le build plan
- pas dans `compute_gains()`

### Si votre modele ne sait pas exporter de table

- laissez `supports_table_export = false`
- retournez une erreur explicite dans `save_to_file()`

### Si votre modele ne supporte pas `spread` ou `distance_model`

- mettez les flags a `false`
- ne laissez pas la UI envoyer des controles qui n'ont pas de sens

## Limitations actuelles

L'architecture est maintenant bien plus contributive qu'avant, mais elle n'est pas encore completement dynamique.

Il reste encore des points a enum:

- `RenderBackendKind`
- `GainModelKind`

Donc ajouter un backend n'est pas encore un simple depot d'un module externe.

En revanche, le cout est maintenant localise:

- identite dans `render_backend.rs`
- build dans `backend_registry.rs`
- implementation du modele

Le coeur audio, le runtime live et la Studio n'ont plus besoin d'etre refondus pour chaque nouveau modele.
