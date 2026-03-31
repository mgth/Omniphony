# PipeWire Bridge Investigation

## Objet

Ce document résume les recherches menées sur le bridge PipeWire utilisé par `orender` pour exposer `omniphony` comme cible audio `pipewire/omniphony` pour `mpv`, avec du passthrough IEC61937 / TrueHD.

Le but est de permettre à quelqu'un de reprendre l'investigation sans repartir de zéro.

Fichier principalement concerné :

- `/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/live_input.rs`

Contexte fonctionnel :

- `mpv` ouvre un device PipeWire nommé `pipewire/omniphony`
- `mpv` demande un format `spdif-truehd`, typiquement `192000 Hz`, `7.1`, `8ch`
- `orender` doit recevoir les bursts IEC61937 et les décoder / router

## Symptomatologie observée

Symptôme principal côté `mpv` :

- `ao/pipewire` passe par `unconnected -> connecting -> paused`
- puis tombe sur `state=error error=no target node available`

Symptômes principaux côté `orender` selon le backend :

- soit le node apparaît dans PipeWire mais n'est pas utilisable comme cible
- soit le node est visible et relié, mais la négociation de ports/buffers ne démarre jamais
- soit le monitor/capture du sink virtuel échoue avec `no target node available`

Important :

- l'ordre de lancement n'est pas la cause
- les logs `mpv` montrent qu'il voit bien `pipewire/omniphony`
- le problème est donc dans la nature du node exposé ou dans sa négociation PipeWire

Correctif important après la première version de cette note :

- le déblocage effectif du streaming n'a pas été apporté principalement par le travail sur le type de format annoncé
- le facteur décisif identifié ensuite est la négociation et l'allocation des buffers PipeWire
- en particulier, les changements qui ont commencé à forcer des tailles de buffers explicites et suffisantes ont débloqué le passage en streaming

Correctif important après les séries suivantes :

- le streaming a bien été débloqué un temps, mais avec un comportement encore dégradé
- un second symptôme a ensuite été observé :
  - audio haché de façon très régulière
  - vidéo jouée à environ `1/4` de sa vitesse normale
- ce second symptôme a permis d'isoler un problème distinct : certains backends PipeWire arrivaient à recevoir des données, mais avec une cadence effective alignée sur `48000 Hz` au lieu du domaine transport `192000 Hz`

## Récapitulatif par type de node / backend PipeWire

### 1. `PwExportedNode`

#### Hypothèse

Exporter directement un `spa_node` custom via `pw_core_export()` devait produire un vrai sink utilisable par `mpv`.

#### Ce qui a été observé

Le backend `PwExportedNode` a d'abord été sous-estimé. Les séries plus récentes ont montré qu'il n'est pas ignoré par PipeWire :

- le pair ajoute bien des listeners
- il interroge massivement `enum_params` côté node
- il interroge massivement `port_enum_params` côté port
- il envoie `set_io`
- il envoie `set_param(id=4)`
- le node passe ensuite en `configured=true`

Cela prouve que `PwExportedNode` est bien vu comme un `spa_node` réel et sérieusement exploré par le pair.

#### Ce qui n'est jamais arrivé

Même après enrichissement des params annoncés au niveau node, on n'a jamais vu :

- `send_command`
- `port_set_param`
- `port_use_buffers`
- `port_set_io`
- `process`

#### Conclusion

- `PwExportedNode` n'est pas une impasse triviale de type "node invisible"
- le blocage est plus haut niveau : le pair inspecte le node, mais ne l'active jamais
- il reste une piste sérieuse, à condition de simplifier / durcir la structure du node et du port

### 2. `PwClientNode`

#### Hypothèse initiale

Utiliser `pw_client_node` devait se rapprocher du comportement natif PipeWire et débloquer la négociation complète.

#### Ce qui a été prouvé

Le backend `PwClientNode` a franchi nettement plus d'étapes que `PwExportedNode`.

On a vu :

- création du `client-node`
- `proxy bound`
- `set_activation`
- `transport`
- `set_io`
- `set_param id=4`
- `set_param id=11`
- republis d'état avec `refresh configured state`

On a aussi ajouté puis validé :

- `pw_core_events.add_mem/remove_mem`
- `mmap` de la mémoire partagée PipeWire
- résolution des pointeurs `transport`, `set_activation`, `set_io`
- suppression de `node_subscribe_params` qui causait `unknown resource 3 op:1`
- écriture explicite d'un état d'activation minimal dans la zone `pw_node_activation`

#### Hypothèses testées sur `PwClientNode`

1. Le blocage venait de l'absence de mémoire partagée mappée.

- Faux
- `add_mem` arrivait bien
- les pointeurs étaient cohérents

2. Le blocage venait de l'absence de transition d'activation côté client.

- Faux au moins en première approximation
- l'état d'activation a été écrit explicitement
- cela n'a pas débloqué les callbacks de port

3. Le blocage venait d'un bruit protocolaire `unknown resource 3 op:1`.

- Faux comme cause racine
- ce bruit a disparu après suppression de `node_subscribe_params`
- aucun effet décisif sur la suite

#### Ce qui n'est jamais arrivé

Malgré tous les ajouts précédents, on n'a jamais vu :

- `add_port`
- `port_set_param`
- `port_use_buffers`
- `port_set_io`

#### Conclusion

- `PwClientNode` est le backend qui a donné le plus d'informations utiles
- il n'échoue pas par "node introuvable"
- il échoue plus loin, dans la négociation réelle des ports
- il reste le dernier backend "prometteur" si on veut poursuivre la piste bas niveau

#### Réévaluation après déblocage du streaming

La conclusion utile n'est plus seulement "le type de node PipeWire est le facteur principal".

Les modifications qui ont réellement commencé à débloquer le streaming sont surtout celles qui ont rendu la négociation de buffers explicite :

- construction d'un `SPA_PARAM_Buffers` dédié dans [live_input.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/live_input.rs:4577)
- annonce de :
  - `buffers = 8`
  - `blocks = 1`
  - `size = nominal_size`
  - `stride = port_bytes_per_frame`
  - `align = 16`
- allocation effective d'une taille minimale cohérente côté node exporté dans [live_input.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/live_input.rs:4205)

Le point le plus important est :

- `nominal_size = channels * sizeof(u16) * (sample_rate / 100)`

Pour le cas `8ch @ 192000 Hz`, cela donne environ `30720` octets par buffer, avec plancher à `1024`.

Effet pratique :

- avant, le node pouvait exister et sembler partiellement négocié, mais avec des buffers trop petits, implicites, ou incohérents pour le flux IEC61937 / TrueHD
- après ces changements, PipeWire reçoit une géométrie de buffers explicite et suffisamment grande
- c'est très probablement cela qui a fait sauter le verrou du streaming

#### État actuel

- `PwClientNode` a finalement été rétrogradé comme piste de reprise principale
- malgré l'enrichissement des `node.props`, il reste bloqué strictement au niveau node-level
- au moment actuel de l'enquête, le backend par défaut a été remis sur `PwExportedNode`

### 3. `PwAdapter` avec `support.null-audio-sink` + monitor capture

#### Hypothèse initiale

Publier un sink virtuel natif PipeWire avec `adapter` / `support.null-audio-sink`, puis capturer son monitor, devait éviter les complexités du `client-node`.

#### Ce qui a été mis en place

- publication d'un sink `omniphony`
- découverte via registry
- récupération de `global_id`
- récupération de `object.serial`
- récupération de `node.name`
- ciblage du stream de monitor capture avec :
  - `target.object=object.serial`
  - puis `target.object=node.name`
  - puis `node.target`
  - puis `target.id` direct

#### Hypothèses testées sur `PwAdapter`

1. La cible n'était pas découverte.

- Faux
- le registry voyait bien `id=... node.name=omniphony media.class=Audio/Sink`

2. `target.object` utilisait la mauvaise valeur.

- Testé avec `object.serial`
- Testé avec `node.name`
- Faux dans les deux cas comme cause suffisante

3. Il fallait passer le `global_id` au `stream.connect()`.

- Testé
- la doc locale `pw_stream_connect()` dit au contraire que `target_id` devrait rester `PW_ID_ANY`
- pas de déblocage

4. Le problème venait du déclenchement trop tôt avant résolution de la cible.

- Faux
- le code attendait explicitement la présence du node dans le registry avant de lancer la capture

#### Ce qui a été observé

Le chemin le plus avancé donnait :

- `target.object` correctement résolu
- `Unconnected -> Connecting -> Paused`
- `io_changed`
- puis `Paused -> Error("no target node available")`

#### Conclusion

- le sink `support.null-audio-sink` existe bien
- mais il n'est pas exploitable pour ce monitor-capture dans cette configuration
- ce backend a été considéré comme une impasse pratique

### 4. `PwFilter`

#### Hypothèse initiale

Un `pw_filter` publié comme `Audio/Sink` devait exposer un vrai node PipeWire utilisable par `mpv`, sans passer par un monitor d'un sink virtuel séparé.

#### Ce qui a été observé

Côté `orender` :

- `Publishing PipeWire bridge filter sink`
- `state changed: 0 -> 1`
- `connected`
- `state changed: 1 -> 2`

Mais jamais :

- `format negotiated`
- `first process callback`
- `ingest`

Côté `mpv` :

- `ao/pipewire` voit `pipewire/omniphony`
- passe `connecting -> paused`
- puis `state=error error=no target node available`

#### Hypothèses testées sur `PwFilter`

1. Le problème venait du timing.

- Faux
- `mpv` voit bien le device, donc le node existe

2. Le problème venait de propriétés de port incohérentes héritées d'un chemin PCM.

On a retiré :

- `format.dsp = "32 bit raw U32LE"`
- `port.direction = "in"`

Cela n'a pas suffi.

#### Point dur observé

- `pw_filter_get_node_id(filter)` reste `4294967295`

Interprétation pratique :

- le filter se connecte partiellement
- mais PipeWire ne lui attribue jamais un vrai node id exploitable comme cible `PW_KEY_TARGET_OBJECT`
- donc `mpv` ne peut pas se linker réellement dessus

#### Conclusion

- `PwFilter` est visible, mais pas cibleable comme un vrai sink de sortie pour `mpv`
- en pratique, c'est une régression par rapport à `PwClientNode`

### 5. `PwStream`

#### Ce qui a été observé (première phase)

`PwStream` a d'abord fourni le diagnostic le plus clair sur le problème de cadence.

Les logs initiaux montraient :

- `callback chunk: bytes=8192 transport_ms=2.667 rate=192000Hz channels=8`
- environ `94` callbacks par seconde
- donc un débit effectif proche de `48000` frames/s

Calcul utile :

- `8192 / (8ch * 2 bytes) = 512` frames transport par callback
- `512 / 192000 = 2.667 ms` de transport par callback
- `94 * 512 ≈ 48000` frames transport effectifs par seconde

Conclusion de cette première phase :

- sans `StreamFlags::DRIVER`, le stream est contraint par le graphe PipeWire à `48000 Hz`
- cela explique le symptôme "audio haché + vidéo à 1/4 de vitesse"

#### Phase 2 : `StreamFlags::DRIVER` + `trigger_process()`

L'ajout du flag `DRIVER` donne au stream le contrôle du rythme du graphe. Il doit déclencher chaque cycle lui-même via `trigger_process()`.

Changements appliqués :

- `StreamFlags::AUTOCONNECT | MAP_BUFFERS | RT_PROCESS | DRIVER`
- dépendance `pipewire = { version = "0.9.2", features = ["v0_3_34"] }` pour activer `trigger_process()`
- premier `trigger_process()` au passage `Paused -> Streaming`
- `node.force-rate = "192000"` dans les propriétés du stream

#### Bugs successifs identifiés sur `PwStream` DRIVER

##### Bug 1 : les early-returns tuaient la boucle DRIVER

Les chemins de retour anticipé dans `process()` (buffer vide, `byte_len == 0`, données manquantes, etc.) ne reprogrammaient pas de cycle suivant.

Conséquence :

- un seul callback "vide" pouvait stopper définitivement la boucle

Correctif :

- tous les chemins non terminaux replanifient maintenant un cycle

##### Bug 2 : spinning à très haute fréquence

En re-triggerant immédiatement sur les callbacks vides, la boucle pouvait monter vers `250000 Hz`.

Conséquence :

- `Streaming -> Paused -> Streaming` toutes les ~16 secondes
- reset du décodeur TrueHD
- seulement quelques secondes d'audio utile par cycle

Correctif :

- limitation des triggers idle avec `PW_DRIVER_IDLE_TRIGGER_INTERVAL`

##### Bug 3 : mauvais quantum dérivé de `pw_stream_get_time_n()`

Le `pw_time.size` avait d'abord été interprété comme un nombre de frames transport, alors qu'il correspond ici aux samples interleavés.

Exemple observé :

- `pw_time.size = 4096`
- avec `8` canaux cela correspond en réalité à `512` frames transport

Conséquence :

- le quantum calculé était `21.333 ms` au lieu de `2.667 ms`
- le DRIVER se re-déclenchait 8x trop lentement

Correctif :

- conversion explicite `transport_frames = pw_time.size / channels`

Après correction, les logs montrent bien :

- `pw_time: rate=1/192000 size=4096 transport_frames=512 quantum_ms=2.667`

##### Bug 4 : re-trigger synchrone depuis `process()`

Même avec le bon quantum, appeler `trigger_process()` directement depuis `process()` faisait arriver le callback suivant immédiatement, de façon quasi réentrante.

Conséquence :

- seulement 2 callbacks visibles
- puis plus rien

Correctif :

- abandon du `trigger_process()` synchrone
- passage à une planification différée via la boucle `iterate()` du mainloop
- chaque callback planifie le prochain cycle à `now + quantum` au lieu de l'exécuter immédiatement

#### État actuel de `PwStream` + DRIVER

Le backend `PwStream` n'est plus à considérer comme une simple impasse quarter-rate.

Les derniers logs établissent :

- le flux IEC61937 rentre bien à `192000 Hz`
- le `pw_time` est cohérent avec `512` frames / `2.667 ms`
- la boucle DRIVER tient maintenant plusieurs centaines de cycles par seconde
- l'ingest est stable :
  - `process_calls=332..367/s`
  - `bytes=2.7..3.0 MB/s`
  - `sync_buffers=44..49/s`
  - `packets=44..49/s`
- le bridge décode réellement des frames :
  - `data_type=0x16`
  - `sr=48000`
  - `sample_count=40`
  - `ch=12`
- `orender` commence à voir les objets
- la sortie audio PipeWire 48 kHz est créée et passe en `Streaming`

Cela déplace le problème :

- le verrou principal n'est plus la capture PipeWire d'entrée
- le verrou restant est plus loin dans le pipeline, après le démarrage du rendu / de la sortie / de l'OSC
- un arrêt ou décrochage du process est maintenant suspecté, mais pas encore expliqué par les logs existants

#### Bug 5 : `RT_PROCESS` + `Rc<RefCell>` → ABRT après 1–2 minutes

`StreamFlags::RT_PROCESS` fait tourner le callback `process()` dans un thread OS dédié (thread RT séparé du mainloop).

Or la boucle principale appelait `drain_scheduled_pw_stream_trigger()` → `borrow_mut()` sur un `Rc<RefCell<PwDriverTriggerSchedule>>`, pendant que le thread RT appelait simultanément `schedule_pw_stream_driver_trigger()` → `borrow_mut()` sur le même `RefCell`.

Double-borrow → `RefCell` panique à l'intérieur d'une frontière FFI → `panic_cannot_unwind` → `SIGABRT`.

Correctif :

- suppression de `StreamFlags::RT_PROCESS`
- le callback `process()` tourne alors sur le thread mainloop
- tous les accès `Rc<RefCell>` sont de nouveau single-thread → safe

#### Tentative : `node.group` pour caler la capture sur l'horloge du DAC

L'hypothèse était que mettre les deux streams (capture et sortie) dans le même `node.group` PipeWire ferait piloter la capture par l'horloge hardware du DAC, éliminant le drift de -20000 ppm.

Ce qui a été fait :

- `node.group = "omniphony-renderer"` ajouté sur le stream de sortie (`audio_output/src/pipewire.rs`)
- `node.group = "omniphony-renderer"` ajouté sur le stream de capture (`live_input.rs`)
- `StreamFlags::DRIVER` retiré du stream de capture
- boucle iterate remplacée par un simple `iterate(50ms)`

Ce qui a été observé :

- `pw_time: rate=1/96000 size=1024` — le graphe PipeWire tourne à 96kHz
- `process_calls = 94/s` = exactement `96000 / 1024 = 93.75` — le node.group est bien actif
- mais `94 callbacks/s × 512 samples = ~48 000 samples/s` effectifs sur un flux IEC61937 à 192kHz
- → le flux IEC61937 est consommé à 25% de sa vitesse réelle
- → le buffer mpv se remplit et bloque la vidéo
- → son haché, vidéo au ralenti

Leçon :

- PipeWire ne "multiplie pas" les samples par callback pour compenser un ratio de fréquence 4:1
- tous les nœuds d'un même groupe reçoivent la même **durée de quantum** (ici ~10.67ms)
- pour un flux 192kHz, cela livre `10.67ms × 192kHz = 2048 samples` attendus, mais le graphe en livre seulement 512 (aligned sur le quantum 96kHz du DAC)
- l'approche `node.group` est donc incompatible avec un flux IEC61937 192kHz piloté par un DAC 48kHz

Retour au mode `DRIVER` après cet échec.

#### Paramètres clés `PwStream` en vigueur

```
node.force-rate = "192000"
StreamFlags: AUTOCONNECT | MAP_BUFFERS | DRIVER   (RT_PROCESS retiré — cf. Bug 5)
PW_STREAM_ACCUMULATE_CALLBACKS = 4
PW_DRIVER_IDLE_TRIGGER_INTERVAL = 2ms
```

## Hypothèses globales déjà invalidées

Les hypothèses suivantes ont été testées puis invalidées :

- `orender` démarre trop tard par rapport à `mpv`
- `mpv` ne voit pas le node `omniphony`
- la seule annonce du format IEC958 / TrueHD suffisait à elle seule à débloquer le streaming
- le ralentissement `1/4` venait seulement du sample-rate des frames décodées côté runtime
- `node.rate` / `node.lock-rate` suffisaient à corriger la cadence d'un backend `PwStream`

## Ce qui a le plus probablement débloqué le streaming

Avec le recul, la meilleure explication n'est pas un changement isolé de backend PipeWire, mais l'introduction d'une négociation/allocation de buffers beaucoup plus explicite.

Les éléments les plus crédibles sont :

- [live_input.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/live_input.rs:4577)
  - construction d'un `SPA_PARAM_Buffers`
  - `buffers = 8`
  - `blocks = 1`
  - `size = nominal_size`
  - `stride = channels * sizeof(u16)`
  - `align = 16`
- [live_input.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/src/cli/decode/live_input.rs:4205)
  - allocation d'une taille réelle `requested_size = max(data0.maxsize, nominal_size)`
  - garantie qu'un buffer nul ou trop petit soit remonté à une taille exploitable

Conclusion pratique :

- la publication du bon type de sink et du bon type de format reste nécessaire
- mais ce n'est vraisemblablement pas ce qui a "débloqué" le streaming
- le changement décisif semble être le passage à des buffers explicitement dimensionnés pour le flux passthrough

## Ce qui a ensuite cassé la qualité temporelle (résolu)

Une fois le streaming débloqué, un second problème indépendant est apparu :

- audio haché
- vidéo à `1/4` de vitesse

Le cas démontré explicitement était `PwStream` sans flag `DRIVER` :

- les callbacks indiquaient un domaine transport annoncé à `192000 Hz`
- mais un débit effectif compatible avec environ `48000` frames/s

Ce problème est maintenant résolu via `StreamFlags::DRIVER` + planification correcte du quantum (voir section `PwStream` phase 2, bugs 1–4).

Leçon retenue :

- "streaming débloqué" ne veut pas dire "backend valide"
- il faut distinguer :
  - réussite de la négociation
  - justesse de la cadence réelle

## Piste de reprise prioritaire

La capture PipeWire d'entrée est maintenant stable et le pipeline TrueHD fonctionne. Le problème restant est le drift de clock entre le DRIVER software (capture) et le DAC hardware (sortie), qui force le resampler adaptatif à corriger ~-20000 ppm (≈ -2%).

### Boucle de rétroaction DRIVER ↔ sortie (piste suivante)

Le DRIVER software pilote sa boucle avec un timer basé sur `pw_time.size / channels / rate`. Ce timer est approximatif et tourne légèrement trop vite (~+2%), d'où les -20000 ppm de correction imposés par le resampler adaptatif en sortie.

Idée : lire le `rate_adjust()` de `PipewireWriter` (qui expose la correction en cours du contrôleur PI) depuis la boucle de capture, et s'en servir pour ajuster `dynamic_trigger_interval` en temps réel.

Principe :
- `rate_adjust() < 1.0` → le DRIVER avance trop vite → allonger légèrement `dynamic_trigger_interval`
- `rate_adjust() > 1.0` → le DRIVER avance trop lentement → raccourcir `dynamic_trigger_interval`
- objectif : amener `rate_adjust()` vers `1.0` et réduire la correction resampler à ±quelques ppm

Prérequis :
- exposer `rate_adjust()` depuis `PipewireWriter` (déjà présent comme méthode publique)
- passer un `Arc<PipewireWriter>` (ou juste un `Arc<AtomicU32>` wrappant `current_rate_adjust`) à `run_pipewire_bridge_capture_stream`
- appliquer une correction bornée (ex : ±5%) sur `dynamic_trigger_interval` après chaque callback

Ordre de priorité actuel :

1. Implémenter la boucle de rétroaction DRIVER ↔ `rate_adjust()`
2. Vérifier que les pops résiduels disparaissent avec un drift réduit
3. Si régression sur l'entrée : vérifier la géométrie des buffers et la cadence DRIVER en premier

## Ce qui est établi avec un bon niveau de confiance

1. `mpv` voit bien `pipewire/omniphony`.

2. L'erreur `no target node available` signifie ici "node visible mais pas réellement linkable comme target de sortie".

3. `PwAdapter` et `PwFilter` exposent des objets visibles dans PipeWire, mais pas des cibles de sortie valides pour `mpv` dans cette configuration.

4. `PwClientNode` reçoit un handshake node-level avancé, mais la négociation des ports n'aboutit jamais.

5. `PwStream` DRIVER peut désormais faire passer les données au bon rythme effectif pour un transport `192000 Hz`.

6. `PwExportedNode` est bien reconnu comme `spa_node`, mais le pair ne passe jamais à `send_command` / `port_*` / `process`.

7. L'état actuel le plus prometteur est : entrée PipeWire fonctionnelle, décodage effectif, rendu démarré, puis arrêt ou décrochage ultérieur encore à qualifier.

## Pistes de reprise recommandées

### Priorité 1 : poursuivre l'instrumentation du chemin `PwStream` / rendu / sortie

Pourquoi :

- c'est la seule piste qui a maintenant démontré :
  - capture stable
  - parsing IEC61937
  - décodage
  - apparition des objets
  - démarrage de la sortie PipeWire
- le problème a clairement migré en aval de la capture

Points à instrumenter ensuite :

- raison exacte de sortie du process
- `panic` éventuel
- erreur remontée au niveau `main`
- sortie contrôlée de la boucle capture
- interaction entre rendu spatial, writer lifecycle et sortie PipeWire

Signal attendu :

- soit un `panic at ...`
- soit `orender exiting with error: ...`
- soit `capture loop exiting ...`
- soit une autre erreur explicite en aval du bridge d'entrée

### Priorité 2 : garder `PwExportedNode` comme backend de référence bas niveau

Pourquoi :

- il reste le backend le plus contrôlable si la piste `PwStream` devait finalement se révéler impossible à fiabiliser
- il a déjà montré un handshake node-level crédible

### Priorité 3 : garder `PwClientNode` comme référence protocolaire

Pourquoi :

- il reste utile pour comprendre le handshake node-level attendu
- mais il ne doit plus être considéré comme la meilleure piste d'implémentation

## Backends à éviter pour l'instant

### `PwAdapter`

À éviter sauf nouvelle idée forte, parce que :

- la cible est correctement découverte
- toutes les variantes `target.object` testées échouent pareil
- le monitor du `null-audio-sink` reste inutilisable dans ce contexte

### `PwFilter`

À éviter comme backend par défaut, parce que :

- `mpv` voit le node
- mais PipeWire refuse quand même le link
- `node_id` reste invalide

## État du code au moment du document

- le backend par défaut est `PwStream` avec `StreamFlags::DRIVER` (sans `RT_PROCESS`)
- `node.force-rate = "192000"` est déclaré dans les propriétés du stream
- `trigger_process()` est planifié via la boucle `iterate()` du mainloop (planification différée, pas synchrone depuis le callback)
- `PW_STREAM_ACCUMULATE_CALLBACKS = 4` (accumulation de 4 quanta avant `process_chunk`)
- `PW_DRIVER_IDLE_TRIGGER_INTERVAL = 2ms` (rate-limit sur tous les triggers, idle ET après vraie donnée)
- le son fonctionne avec ~-20000 ppm de correction resampler côté sortie (DRIVER software légèrement trop rapide)
- la boucle de rétroaction DRIVER ↔ `rate_adjust()` est la prochaine étape (voir section précédente)
- de nombreuses traces additionnelles existent toujours dans `live_input.rs`, utiles pour la reprise
- `handler.rs` contient une correction séparée sur la distinction `DecodedSource::Bridge` vs `DecodedSource::Live`

## Checklist de reprise

1. Vérifier quel backend est sélectionné (`PwStream` DRIVER est le défaut).
2. Pour `PwStream` DRIVER, valider la santé de l'entrée :
   - `process_calls` doit être ~350–500/s (pas 250 000/s = spinning, pas 94/s = graphe 48kHz)
   - `zero_chunks` doit être < 30% des process_calls
   - `sync_buffers` et `packets` doivent être ~44–55/s
   - `bytes` doit être ~2.7–3.1 MB/s
3. Vérifier que le bridge décode des frames :
   - `PipeWire bridge packet: data_type=0x16 payload_bytes=61424`
   - `PipeWire bridge decoded frame: sr=48000 sample_count=40 ch=12`
4. Vérifier la sortie PipeWire :
   - `Audio streaming to PipeWire is now active`
   - `output stream state: Streaming`
5. Si l'entrée est saine et que ça bloque quand même : investiguer en aval (rendu, writer, OSC).
6. En cas de régression sur la cadence : vérifier `pw_time.size / channels` pour le quantum correct.
7. Ne pas revenir sur `PwAdapter` ou `PwFilter` sans hypothèse nouvelle.

## Résumé très court

- `PwExportedNode` : bien reconnu comme `spa_node`, mais jamais activé (port_* jamais appelé)
- `PwClientNode` : handshake node-level avancé, mais jamais de `port_*`
- `PwAdapter` : sink visible, monitor inutilisable
- `PwFilter` : node visible, mais pas une vraie target pour `mpv`
- `PwStream` + `DRIVER` (sans `RT_PROCESS`) : **son fonctionnel**, IEC61937 parsé, TrueHD décodé, rendu spatial actif
- drift résiduel : DRIVER software ~+2% trop rapide → resampler sortie corrige à -20000 ppm
- piste suivante : boucle de rétroaction `dynamic_trigger_interval` ↔ `rate_adjust()` pour réduire ce drift
