# Architecture modèle de gains / stratégie d'évaluation

## But

Séparer proprement deux responsabilités qui sont encore partiellement mélangées dans le renderer:

- le **modèle de gains**
- la **stratégie d'évaluation**

Le modèle définit comment calculer les gains à partir de la position d'un objet et de celles des enceintes.

La stratégie d'évaluation définit comment ce modèle est exécuté:

- en temps réel
- via une table précalculée
- via une grille cartésienne ou polaire
- ou toute autre forme de cache/interpolation ultérieure

Le point important est le suivant:

- un backend ne doit plus décider lui-même s'il fonctionne en temps réel ou via une table
- cette décision doit être externalisée

## Problème actuel

Aujourd'hui, plusieurs notions sont encore trop centrées sur `VBAP`:

- `RenderBackend`
- `vbap_table_mode`
- `vbap_cart_*`
- `vbap_polar_*`

Cela crée une confusion:

- `VBAP` apparaît comme à la fois un modèle de gains et une stratégie de précalcul
- `polar/cartesian` apparaît comme un détail propre à `VBAP`
- alors que cette notion doit devenir générique

Le backend `experimental_distance` montre précisément cette limite:

- son algorithme de gains est bien distinct de `VBAP`
- mais l'architecture actuelle ne sépare pas encore clairement la loi de gains de la manière de l'évaluer

## Cible conceptuelle

Le renderer doit être structuré autour de trois couches:

1. `GainModel`
2. `EvaluationStrategy`
3. `PreparedRenderEngine`

### 1. GainModel

Le modèle calcule des gains à partir de:

- la position de l'objet
- les positions des enceintes
- le contexte partagé
- les paramètres propres au modèle

Exemples:

- `VbapModel`
- `ExperimentalDistanceModel`

Le modèle ne sait pas s'il est utilisé:

- en temps réel
- via une table cartésienne
- via une table polaire

Il expose seulement une fonction de calcul de gains.

### 2. EvaluationStrategy

La stratégie d'évaluation décide comment appeler ou approximer le modèle.

Exemples:

- `Realtime`
- `PrecomputedCartesian`
- `PrecomputedPolar`

Elle peut:

- appeler directement le modèle à chaque frame
- précalculer une grille puis interpoler
- préparer des caches spécialisés

### 3. PreparedRenderEngine

Le moteur réellement utilisé par le runtime est une combinaison:

- `GainModel`
- `EvaluationStrategy`

Cette combinaison est préparée lors du rebuild de topologie.

Le runtime audio ne consomme ensuite qu'une interface unifiée:

- `evaluate(request) -> gains`

## Contrats cibles

### Contrat du modèle

```rust
pub trait GainModel {
    fn compute_gains(&self, req: &GainModelRequest, out: &mut Gains);
    fn capabilities(&self) -> GainModelCapabilities;
}
```

Exemple de requête:

```rust
pub struct GainModelRequest<'a> {
    pub object_position: [f32; 3],
    pub speakers: &'a [[f32; 3]],
    pub room: RoomTransform,
    pub shared: SharedRenderParams,
    pub model_params: ModelParamsRef<'a>,
}
```

### Contrat de la stratégie

```rust
pub trait EvaluationStrategy {
    fn capabilities(&self) -> EvaluationCapabilities;

    fn prepare(
        &self,
        model: &dyn GainModel,
        setup: &EvaluationSetup,
    ) -> anyhow::Result<Box<dyn PreparedEvaluator>>;
}
```

Puis:

```rust
pub trait PreparedEvaluator {
    fn evaluate(&self, req: &RenderRequest, out: &mut Gains);
}
```

## Exemples d'assemblage

### Cas 1: VBAP avec table cartésienne

- `GainModel = VbapModel`
- `EvaluationStrategy = PrecomputedCartesian`

### Cas 2: VBAP avec table polaire

- `GainModel = VbapModel`
- `EvaluationStrategy = PrecomputedPolar`

### Cas 3: ExperimentalDistance en temps réel

- `GainModel = ExperimentalDistanceModel`
- `EvaluationStrategy = Realtime`

### Cas 4: ExperimentalDistance avec grille cartésienne

Pas nécessaire immédiatement, mais l'architecture doit le permettre:

- `GainModel = ExperimentalDistanceModel`
- `EvaluationStrategy = PrecomputedCartesian`

## Capacités explicites

L'UI et le runtime ne doivent plus raisonner avec des `if backend == vbap`.

Il faut des capacités explicites.

### Capacités du modèle

```rust
pub struct GainModelCapabilities {
    pub supports_distance_model: bool,
    pub supports_spread: bool,
    pub supports_distance_diffuse: bool,
    pub supports_position_interpolation: bool,
}
```

### Capacités de la stratégie

```rust
pub struct EvaluationCapabilities {
    pub kind: EvaluationKind,
    pub supports_cartesian_resolution: bool,
    pub supports_polar_resolution: bool,
}
```

### Compatibilité modèle / stratégie

Il faut aussi pouvoir déclarer quelles combinaisons sont valides.

Exemple:

- `VbapModel` supporte `Realtime`, `PrecomputedCartesian`, `PrecomputedPolar`
- `ExperimentalDistanceModel` supporte d'abord `Realtime`, puis éventuellement d'autres stratégies plus tard

## Nommage cible

Il faut sortir les paramètres génériques du domaine `VBAP`.

### À renommer

- `vbap_table_mode` -> `evaluation_mode`
- `vbap_cart_x_size` -> `cartesian_precompute_x_size`
- `vbap_cart_y_size` -> `cartesian_precompute_y_size`
- `vbap_cart_z_size` -> `cartesian_precompute_z_size`
- `vbap_cart_z_neg_size` -> `cartesian_precompute_z_neg_size`
- `vbap_polar_*` -> `polar_precompute_*`

### Nouveaux concepts

- `gain_model`
- `evaluation_mode`

Exemple:

```rust
pub enum GainModelKind {
    Vbap,
    ExperimentalDistance,
}

pub enum EvaluationKind {
    Realtime,
    PrecomputedCartesian,
    PrecomputedPolar,
}
```

## Ce qui est générique et ce qui ne l'est pas

### Générique

Relève de la stratégie d'évaluation:

- temps réel vs précalcul
- cartésien vs polaire
- résolutions et tailles de grille

### Spécifique au modèle

Relève du modèle lui-même:

- paramètres propres à `VBAP`
- paramètres propres à `ExperimentalDistance`

### Partagé

Peut rester dans un bloc commun si utilisé par plusieurs modèles:

- room geometry
- ramp mode
- éventuellement certains paramètres de distance ou de diffusion, mais seulement s'ils sont réellement utilisés par plusieurs modèles

## Impact UI

L'UI doit proposer deux choix séparés:

- `Render model`
- `Evaluation mode`

Puis afficher:

- les paramètres du modèle si le modèle les supporte
- les paramètres de la stratégie si la stratégie les supporte

Exemple:

### `VbapModel + PrecomputedCartesian`

Afficher:

- paramètres du modèle VBAP
- paramètres de précalcul cartésien

### `ExperimentalDistanceModel + Realtime`

Afficher:

- paramètres distance spécifiques
- pas de paramètres de table cartésienne/polaire

### `ExperimentalDistanceModel + PrecomputedCartesian`

Afficher:

- paramètres distance spécifiques
- paramètres de grille cartésienne

## Migration depuis l'état actuel

La migration doit être incrémentale.

### Étape 1

Introduire les nouveaux concepts sans casser le runtime:

- `GainModelKind`
- `EvaluationKind`

### Étape 2

Conserver l'implémentation actuelle mais la remapper conceptuellement:

- `VBAP + polar/cartesian` devient `VbapModel + Precomputed*`
- `ExperimentalDistance` devient `ExperimentalDistanceModel + Realtime`

### Étape 3

Remplacer l'abstraction actuelle `RenderBackend` par quelque chose de plus précis:

- `PreparedRenderEngine`

### Étape 4

Déplacer les paramètres:

- ce qui relève de l'évaluation hors du domaine `vbap_*`
- ce qui relève du modèle dans des blocs de paramètres propres

### Étape 5

Adapter l'UI:

- deux sélecteurs séparés
- visibilité fondée sur les capacités

### Étape 6

Ajouter plus tard de nouvelles combinaisons:

- `ExperimentalDistance + PrecomputedCartesian`
- autres modèles futurs

## Recommandation pratique

La première version du refactor doit viser une compatibilité simple:

- `VbapModel + PrecomputedCartesian`
- `VbapModel + PrecomputedPolar`
- `ExperimentalDistanceModel + Realtime`

Cela permet:

- d'introduire la bonne séparation conceptuelle
- sans imposer immédiatement un grand nombre de combinaisons nouvelles

## Conclusion

La bonne séparation n'est pas:

- `backend = VBAP` avec un mode interne `polar/cartesian`

mais:

- `modèle de gains`
- `stratégie d'évaluation`

Le modèle définit la loi de gains.

La stratégie définit comment cette loi est exécutée:

- temps réel
- table cartésienne
- table polaire

Cette architecture:

- généralise le précalcul à tous les backends
- évite les champs et concepts trop `VBAP`-centrés
- permet à l'UI d'exprimer proprement les compatibilités réelles
- prépare l'arrivée de nouveaux modèles sans refiger l'architecture autour de `VBAP`
