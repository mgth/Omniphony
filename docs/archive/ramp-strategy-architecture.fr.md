# Architecture de Ramp Strategy

## But

Ce document cadre le refactor du ramping dans
`omniphony-renderer/renderer/src/spatial_renderer.rs`.

L'objectif est double:

- sortir la logique de rampe du coeur de `SpatialRenderer`
- permettre a un contributeur d'injecter son propre comportement de ramping

La contrainte importante est la suivante:

- le trait ne doit pas retourner "une position ou des gains"
- dans tous les cas, le trait doit produire les gains finaux consommes par le mix

## Probleme Actuel

Aujourd'hui, `SpatialRenderer` gere en direct:

- l'etat de position courante / cible
- la duree de rampe
- l'interpolation de position
- le calcul des gains via le backend
- le choix `RampMode::Off | Frame | Sample`

Le resultat est correct pour le cas "interpolation de position active", mais il
pose deux limites:

- le renderer reste couple a une seule interpretation de la rampe
- quand `position_interpolation == false`, la rampe continue de faire evoluer la
  position logique au lieu d'interpoler directement les tables de gains

Le deuxieme point est le vrai probleme fonctionnel:

- si le backend travaille sans interpolation de position, le comportement
  attendu est de conserver les positions de depart et d'arrivee telles quelles
- la transition audible doit se faire entre les gains de depart et d'arrivee

## Direction Retenue

Introduire un vrai trait public `RampStrategy` dans le crate `renderer`.

Ce trait:

- recoit un contexte de calcul de gains
- manipule un `ChannelRampState` reutilisable
- ecrit toujours les gains de sortie dans cet etat
- decide lui-meme comment evolue la rampe

Le renderer runtime garde uniquement:

- le snapshot live
- la boucle de mix
- les gains metadata / user / speaker
- les delays
- l'auto-gain
- le monitoring

Le calcul des gains spatiaux finaux pendant une rampe sort du coeur du renderer.

## API Visee

Le module expose les briques suivantes:

- `RampContext`
- `RampRenderParams`
- `RampTarget`
- `ChannelRampState`
- `RampStatus`
- `RampBlock`
- `RampStrategy`

### RampContext

`RampContext` encapsule ce dont une strategie a besoin pour produire des gains:

- le backend de rendu prepare
- l'identite de la topologie active
- les parametres live necessaires pour construire un `RenderRequest`
- le nombre de sorties du backend

Il expose surtout:

- `compute_gains(position)`

Ainsi, une implementation externe n'a pas besoin d'aller chercher directement
des details internes de `SpatialRenderer`.

### ChannelRampState

`ChannelRampState` porte l'etat persistant par canal:

- position courante / cible
- duree et progression de rampe
- gains courants / cible / de sortie
- cache optionnel des gains pour les cas stationnaires

Le point important est que l'etat re-utilise des buffers pre-alloues:

- pas d'allocation par sample
- pas de `Vec<f32>` dans le hot path

### RampStrategy

Le contrat retenu est:

```rust
pub trait RampStrategy: Send + Sync {
    fn name(&self) -> &'static str;

    fn update_target(
        &self,
        state: &mut ChannelRampState,
        target: RampTarget,
        sample_index: Option<u64>,
        ctx: &RampContext<'_>,
    );

    fn evaluate(
        &self,
        state: &mut ChannelRampState,
        progress: RampProgress,
        ctx: &RampContext<'_>,
    ) -> RampStatus;
}
```

Le point cle:

- la strategie ne retourne pas une position
- elle remplit `state.output_gains`
- le renderer mixe ensuite ces gains comme avant

`RampProgress` decrit la position absolue dans la rampe:

- `completed_units`
- `total_units`

Le renderer reste responsable de:

- choisir a quelle frequence appeler `evaluate(...)`
- avancer la progression reelle de la rampe
- traiter `RampMode::Off` en amont

Ainsi:

- `RampMode::Off` bypass la strategie
- `RampMode::Frame` appelle `evaluate(...)` une fois par bloc
- `RampMode::Sample` appelle `evaluate(...)` pour chaque sample

## Strategies Integrees

La premiere tranche d'implementation ajoute deux strategies concretes.

### 1. PositionRampStrategy

Elle reproduit le comportement actuel:

- la rampe fait evoluer la position
- les gains finaux sont calcules depuis cette position
- le cache est reutilise quand la position et les parametres live sont stables

Cette strategie reste la bonne valeur par defaut quand
`position_interpolation == true`.

### 2. GainTableRampStrategy

Elle traite le cas ou `position_interpolation == false`:

- la position logique saute directement a la cible
- les gains courants sont interpolés vers les gains cibles
- la transition audible se fait donc entre tables de gains, pas entre positions

Le rendu final devient coherent avec un backend sans interpolation de position.

## Selection de Strategie

Sans override explicite:

- `position_interpolation == true` -> `PositionRampStrategy`
- `position_interpolation == false` -> `GainTableRampStrategy`

Le renderer expose en plus un point d'extension:

- `set_ramp_strategy(Arc<dyn RampStrategy>)`
- `clear_ramp_strategy()`

Un contributeur peut ainsi injecter son propre comportement sans modifier le
mix runtime.

## Contraintes de Performance

Le design retenu doit respecter:

- zero allocation par sample
- zero `HashMap` dans le hot path de la strategie
- reutilisation de `Gains`
- cache explicite pour le cas stationnaire

Le cout supplementaire principal du refactor est:

- un appel virtuel par canal / bloc ou par sample selon la strategie

Ce cout est acceptable au regard du gain d'extensibilite, a condition que les
implementations concretes restent simples et previsibles.

## Premiere Tranche Livree

La premiere tranche doit faire seulement ceci:

1. introduire le module `ramp_strategy`
2. exporter le trait public
3. recabler `SpatialRenderer` sur cette abstraction
4. fournir les strategies integrees `position` et `gain_table`
5. conserver le comportement existant quand l'interpolation de position est active

Les evolutions suivantes pourront ensuite ajouter:

- d'autres politiques de rampe
- des courbes d'interpolation non lineaires
- des strategies specialisees par backend ou par type d'objet
