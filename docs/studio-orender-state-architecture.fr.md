# Refactor State Architecture Studio / orender

## Contexte

Le flux d'etat entre `orender` et `Studio` s'est fragilise.

Les symptomes observes ont concerne:

- `master gain` incorrect ou non initialise au demarrage
- sortie audio affichee comme `default` alors qu'un autre device est utilise
- liste des devices absente ou refresh sans effet
- latence non affichee alors que le moteur audio tourne et que le metering est actif

Le probleme n'est pas le calcul audio lui-meme. `orender` connait deja:

- le device audio cible
- le sample rate effectif
- les metriques de latence

Le probleme est l'architecture du transport et du modele d'etat.

## Diagnostic

Aujourd'hui, plusieurs concepts differents sont melanges:

- `persisted`: preferences sauvegardees par Studio (`osc_config.json`)
- `requested`: ce que Studio demande a `orender` via OSC
- `effective`: ce que le backend audio utilise reellement
- `telemetry`: valeurs live a haute frequence, par exemple latence instantanee et meters

En plus, le meme etat logique existe en plusieurs copies:

- protocole OSC cote `orender`
- `AppState` cote Tauri
- etat JS cote frontend

Le systeme depend donc d'alignements implicites:

- certains panneaux lisent la config persistante
- d'autres lisent l'etat runtime
- une partie du boot depend d'un snapshot UDP best-effort
- une partie de la latence depend du flux `metering`

Ca explique pourquoi une evolution du renderer ou du backend a pu casser l'UI audio/latence sans toucher directement au calcul de latence.

## Causes racines

### 1. Etat audio ambigu

Le champ `audio/output_device` a longtemps porte plusieurs sens a la fois:

- preference launch-time
- demande runtime
- device reel

Ce n'est pas tenable.

### 2. Emission d'etat conditionnelle incomplete

Le chemin `send_audio_state_if_changed()` ne republiait l'etat que si `sample_rate` ou `sample_format` changeaient.

Un changement de device pouvait donc ne produire aucun nouvel evenement OSC.

### 3. Telemetry et etat d'exploitation couples

La latence live remonte via le flux metering, alors que l'UI en depend aussi comme etat de fonctionnement normal.

Ce n'est pas toujours un probleme pour un meter.
Ca en devient un pour un panneau de supervision.

### 4. Boot non fiable

Le snapshot initial etait transporte en UDP sans protocole explicite de completion.

Une perte ou une course de demarrage pouvait laisser Studio avec un etat partiel.

## Objectif cible

Le systeme doit exposer quatre couches separees:

### A. Persisted config

Source de verite:

- `omniphony-studio/src-tauri/src/config.rs`

Contient:

- host/ports OSC
- `audio_output_device` preferee pour le lancement
- sample rate prefere
- mode de ramp

Usage:

- uniquement pour preparer un lancement ou rehydrater l'UI avant connexion
- jamais comme source de verite sur l'etat effectif du moteur

### B. Requested runtime state

Source de verite:

- `AudioControl.requested`
- `RendererControl.live`

Contient:

- device demande
- target latency demandee
- backend de rendu demande
- autres parametres runtime modifiables via OSC

Usage:

- visible dans l'UI comme "demande actuelle"
- doit etre publie explicitement

### C. Effective runtime state

Source de verite:

- backend audio actif
- topologie active
- etat applique dans `AudioControl.applied`

Contient:

- device effectif
- sample rate effectif
- format effectif
- backend de rendu effectif

Usage:

- visible dans l'UI comme "etat reel"
- ne doit jamais etre derive de la config Studio

### D. Telemetry

Source de verite:

- writer audio / metering / renderer timing

Contient:

- `latency_instant`
- `latency_control`
- `decode_time_ms`
- `render_time_ms`
- `write_time_ms`
- meters objets/speakers

Usage:

- uniquement pour supervision live
- ne doit pas porter les champs de configuration/exploitation durables

## Proposition d'architecture

### 1. Contrat OSC clair

Le protocole doit distinguer explicitement:

- `/omniphony/state/audio/output_device/requested`
- `/omniphony/state/audio/output_device/effective`
- `/omniphony/state/audio/sample_rate/effective` si necessaire plus tard
- `/omniphony/state/latency_target/requested`
- `/omniphony/state/latency_target/effective` si le backend applique une valeur differente

Compatibilite:

- garder les anciens chemins legacy temporairement
- mais les traiter comme alias documentes

### 2. Snapshot bootstrap obligatoire

Le boot doit suivre ce protocole:

1. Studio s'enregistre
2. `orender` envoie un snapshot complet
3. `orender` envoie un marqueur `snapshot_complete`
4. Studio n'autorise les controles sensibles qu'apres ce marqueur

Le snapshot doit contenir:

- `requested state`
- `effective state` connu a ce moment
- dernier etat de supervision utile non nul si disponible

### 3. Live state hors metering

Les etats de supervision importants doivent pouvoir remonter hors du seul flux metering:

- target latency demandee
- target latency effective
- device effectif
- sample rate effectif
- erreurs audio

Le metering reste reserve aux valeurs rapides:

- latence instantanee
- latence control
- meters
- timings decode/render/write

### 4. Reducer unique cote Studio

Aujourd'hui, le meme champ peut etre touche dans:

- `init.js`
- `tauri-bridge.js`
- `controls/*.js`
- `AppState` Rust

Direction cible:

- un schema d'etat unique
- un reducer JS unique qui applique tous les evenements Tauri
- `applyInitState()` reutilise ce meme reducer au lieu de dupliquer la logique

Le but est de supprimer les divergences entre boot et live.

## Plan de travail

### Phase 1. Stabilisation immediate

Objectif:

- retablir un etat coherent sans attendre le grand refactor

Actions:

- separer `requested` et `effective` pour l'audio
- republier l'etat audio si le device change, meme sans changement de format
- garder `snapshot_complete`
- faire remonter la target latency hors metering

Statut:

- deja engage

### Phase 2. Schema d'etat explicite

Objectif:

- supprimer l'ambiguite des champs

Actions:

- introduire des structs dediees cote Studio et renderer:
  - `AudioPersistedState`
  - `AudioRequestedState`
  - `AudioEffectiveState`
  - `AudioTelemetryState`
- remplacer les champs audio aplatis dans `AppState` quand possible
- documenter le schema dans `OSC_PROTOCOL.md`

### Phase 3. Canal live-state durable

Objectif:

- ne plus dependre du flux metering pour l'etat d'exploitation

Actions:

- emettre un bundle `state` periodique faible frequence ou event-driven
- y placer les changements de:
  - audio effective state
  - latency target
  - backend effectif
  - erreurs runtime

Le metering reste sur son canal actuel.

### Phase 4. Reducer front unique

Objectif:

- supprimer la logique dupliquee `init/live`

Actions:

- creer un module unique d'application d'evenements d'etat
- utiliser ce module:
  - pour le snapshot initial
  - pour les evenements incrementaux
- reduire `init.js` a de l'hydratation via ce reducer

### Phase 5. Tests de scenario

Objectif:

- verrouiller les regressions sur les cas qui cassent aujourd'hui

Scenarios minimaux:

1. Studio demarre sans `orender`, puis lance `orender`
2. `orender` deja lance, puis Studio se connecte
3. changement de device output sans restart
4. refresh devices sur PipeWire
5. metering on/off pendant session
6. perte/reconnexion OSC

Assertions:

- `master` n'affiche jamais une valeur par defaut dangereuse
- `audio output requested` et `effective` sont visibles et differents si necessaire
- la liste des devices est non vide si le backend sait la lister
- `latency_target` existe des le snapshot
- `latency_instant/control` apparaissent des que l'audio tourne et que le metering est actif

## Changement d'architecture recommande

La recommandation principale est simple:

- ne plus modeler l'UI sur un melange de config Studio et d'evenements OSC opportunistes

Il faut un contrat clair:

- config persistante pour lancer
- etat runtime demande pour piloter
- etat runtime effectif pour superviser
- telemetry pour mesurer

Tant que ces quatre couches ne sont pas clairement separees, ce type de casse restera facile.

## Prochaine etape

La prochaine etape propre est:

1. finaliser la separation `requested/effective` sur l'audio
2. ajouter un etat runtime effectif de latence cible si necessaire
3. centraliser l'application des evenements Studio dans un reducer unique
4. retester les 6 scenarios de reference
