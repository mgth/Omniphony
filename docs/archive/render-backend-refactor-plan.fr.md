# Plan de Refactor du Backend de Rendu

## But

Ce document cadre un refactor de `omniphony-renderer` pour permettre deux
niveaux de substitution:

- choisir l'algorithme de préparation / précalcul du backend de gains
- changer le moteur de rendu spatial actif au runtime

L'objectif n'est pas de supprimer VBAP, mais de sortir d'une architecture où
VBAP structure directement les types centraux du renderer.

## Constat Actuel

Le code actuel possède déjà un point d'appui important:

- la topologie de rendu active est publiée via `ArcSwap`
- le thread audio recharge cette topologie lock-free à chaque frame

En revanche, la topologie et le moteur runtime restent fortement couplés à
VBAP:

- `RenderTopology` stocke directement un `VbapPanner`
- les plans de rebuild sont VBAP-spécifiques
- `SpatialRenderer::render_frame()` appelle directement
  `vbap.get_gains_cartesian(...)`
- les paramètres live exposent principalement des concepts `vbap_*`

Conclusion:

- le swap atomique de données de topologie existe déjà
- le swap d'un backend de rendu générique n'existe pas encore

## Cible d'Architecture

Séparer trois couches:

- une abstraction de backend de rendu
- une topologie de rendu immutable et publiable atomiquement
- une boucle runtime de mix agnostique du backend actif

Le renderer runtime doit conserver:

- la boucle de mix
- le ramping
- les gains live par objet
- les gains live par enceinte
- les delays
- l'auto-gain
- le monitoring

Le calcul de gains spatiaux doit sortir du coeur de `SpatialRenderer`.

## Direction Recommandée

### 1. Introduire un backend générique

Créer un module `render_backend` avec:

- `RenderBackend`
- `RenderBackendKind`
- `RenderRequest`
- `RenderResponse`

Première étape pragmatique:

- utiliser un `enum RenderBackend`
- encapsuler VBAP comme premier backend concret

Cela évite d'introduire trop tôt des objets dynamiques et permet de garder un
coût runtime simple.

### 2. Encapsuler VBAP derrière cette abstraction

Créer un wrapper `VbapBackend` chargé de:

- posséder le `VbapPanner`
- appliquer la logique spécifique du backend VBAP
- calculer les gains à partir d'une requête générique

La logique qui doit migrer vers ce backend comprend notamment:

- room ratios
- warp de profondeur
- spread-from-distance
- distance diffuse
- appel direct à `get_gains_cartesian`

### 3. Généraliser `RenderTopology`

`RenderTopology` doit contenir:

- le layout
- le backend actif
- les mappings dérivés backend -> enceintes
- les informations de dimension nécessaires au mix

Le nommage doit devenir générique:

- `vbap_to_speaker_mapping` -> `backend_to_speaker_mapping`

### 4. Généraliser les plans de rebuild

L'API de rebuild doit cesser d'exprimer uniquement des concepts VBAP.

Direction visée:

- conserver des paramètres spécialisés par backend
- exposer une couche de préparation / publication générique

Exemple de cible:

- `TopologyBuildPlan::Vbap(...)`
- `TopologyBuildPlan::ExperimentalDistance(...)`

### 5. Rendre `SpatialRenderer` backend-agnostique

`SpatialRenderer::render_frame()` ne doit plus connaître `VbapPanner`.

Le coeur de la boucle doit demander:

- "calcule les gains pour cette position et ces paramètres live"

et recevoir:

- un vecteur de gains

Le moteur runtime garde alors son rôle de:

- snapshot runtime
- interpolation / ramping
- application des gains
- accumulation vers les sorties

## Ordre de Refactor Recommandé

### Phase 1: abstraction sans changement fonctionnel

- ajouter `render_backend`
- créer `VbapBackend`
- remplacer `RenderTopology.vbap` par `RenderTopology.backend`
- adapter `SpatialRenderer` pour passer par le backend générique

Résultat attendu:

- comportement audio inchangé
- architecture prête à accueillir d'autres backends

### Phase 2: généralisation du rebuild

- remplacer `VbapRebuildParams` par une couche de plan plus générique
- conserver une variante VBAP fonctionnelle
- brancher la publication atomique sur cette nouvelle couche

### Phase 3: ajout du backend expérimental

- implémenter un backend distance-based
- lui donner sa propre config de build et de runtime
- permettre la sélection explicite du backend actif

## Contraintes de Design

- pas d'allocations par sample dans le hot path
- le thread audio ne doit pas prendre de lock long pour changer de backend
- les backends doivent produire un format de gains commun au mix final
- le cache de gains doit être invalide quand le backend actif change

## Verdict

Le code actuel n'est pas prêt tel quel pour swaper facilement les modèles de
rendu, mais il possède déjà la bonne primitive de publication runtime:

- `ArcSwap<RenderTopology>`

Le refactor nécessaire consiste surtout à:

- retirer le couplage direct à `VbapPanner`
- faire de VBAP un backend parmi d'autres
- garder la boucle audio séparée du modèle de rendu

## Première Tranche d'Implémentation

La première tranche recommandée est volontairement conservatrice:

1. introduire l'abstraction backend
2. brancher VBAP derrière elle
3. recâbler `RenderTopology`
4. recâbler `SpatialRenderer`
5. ne rien changer au rendu effectif

Une fois cette tranche en place, l'ajout d'un backend expérimental devient une
évolution locale, au lieu d'une refonte transversale.
