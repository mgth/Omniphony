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

## Récapitulatif par type de node / backend PipeWire

### 1. `PwExportedNode`

#### Hypothèse

Exporter directement un `spa_node` custom via `pw_core_export()` devait produire un vrai sink utilisable par `mpv`.

#### Ce qui a été observé

- le node était publié
- mais la négociation ne progressait pas suffisamment
- le chemin ne donnait pas les callbacks nécessaires côté ports / buffers

#### Conclusion

- backend trop limité pour notre usage réel
- il n'a jamais franchi le seuil de négociation utile
- abandonné au profit de `PwClientNode`

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

- après les essais `PwAdapter` / `PwFilter`, le backend par défaut a été remis sur `PwClientNode`

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

#### Statut

- présent dans le code
- non poussé aussi loin que `PwClientNode`, `PwAdapter` ou `PwFilter` dans cette série d'essais

#### Conclusion

- backend non invalidé formellement
- mais pas assez exploré pour être aujourd'hui la piste prioritaire

## Hypothèses globales déjà invalidées

Les hypothèses suivantes ont été testées puis invalidées :

- `orender` démarre trop tard par rapport à `mpv`
- `mpv` ne voit pas le node `omniphony`
- la seule annonce du format IEC958 / TrueHD suffisait à elle seule à débloquer le streaming

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

## Piste de reprise prioritaire

Si quelqu'un reprend l'investigation, il faut désormais prioriser dans cet ordre :

1. la géométrie des buffers annoncés aux ports PipeWire
2. la taille réellement allouée aux `spa_buffer`
3. la cohérence entre `size`, `stride`, `channels`, `sample_rate` et le type de transport IEC61937
4. seulement ensuite les détails de type de node (`PwClientNode`, `PwExportedNode`, `PwAdapter`, `PwFilter`)

Autrement dit :

- le type de node reste important
- mais la première zone à re-vérifier en cas de régression est désormais la négociation des buffers, pas seulement la visibilité du node ou le type de format annoncé
- le problème est seulement un mauvais `target.object`
- le problème est seulement l'absence de `add_mem`
- le problème est seulement l'activation partagée du `client-node`
- le problème est seulement le bruit `unknown resource 3 op:1`

## Ce qui est établi avec un bon niveau de confiance

1. `mpv` voit bien `pipewire/omniphony`.

2. L'erreur `no target node available` signifie ici "node visible mais pas réellement linkable comme target de sortie".

3. `PwAdapter` et `PwFilter` exposent des objets visibles dans PipeWire, mais pas des cibles de sortie valides pour `mpv` dans cette configuration.

4. `PwClientNode` est le backend le plus avancé côté protocole natif, mais la négociation des ports n'aboutit jamais.

## Pistes de reprise recommandées

### Priorité 1 : reprendre `PwClientNode`

Pourquoi :

- c'est le seul backend qui a réellement dépassé le simple stade "node visible"
- il ne bloque pas sur `target node available`
- il bloque plus tard, dans la négociation `port_*`

Points à instrumenter ou comparer :

- séquence exacte attendue après `set_param id=4` et `id=11`
- conditions qui déclenchent `add_port` / `port_set_param`
- layout exact et transitions réelles de `pw_node_activation`
- éventuels `update` / `port_update` manquants ou incomplets
- comparaison avec une implémentation PipeWire native de `pw_client_node`

Signal attendu si cette piste progresse :

- apparition de `add_port`
- puis `port_set_param`
- puis `port_use_buffers` / `port_set_io`

### Priorité 2 : vérifier si `mpv` vise bien un node et non un device/session object intermédiaire

Pourquoi :

- `ao_pipewire.c` côté `mpv` passe `PW_KEY_TARGET_OBJECT = ao->device`
- `ao->device` vaut `omniphony`
- `mpv` attend un node de type sink réellement cibleable

À clarifier :

- faut-il exposer un autre type d'objet PipeWire que celui actuellement créé
- faut-il passer par une factory/session-manager différente

### Priorité 3 : explorer sérieusement `PwStream`

Pourquoi :

- moins bas niveau que `PwClientNode`
- potentiellement plus "natif" qu'un `filter` mal matérialisé ou qu'un `adapter` monitoré

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

- le backend par défaut a été remis sur `PwClientNode`
- de nombreuses traces additionnelles existent toujours dans `live_input.rs`
- elles sont utiles pour la reprise

## Checklist de reprise

1. Vérifier que le backend sélectionné est bien `PwClientNode`.
2. Relancer `mpv` avec les traces PipeWire actuelles.
3. Relever uniquement :
   - `set_activation`
   - `transport`
   - `set_io`
   - `set_param`
   - `refresh configured state`
   - `add_port`
   - `port_set_param`
   - `port_use_buffers`
   - `port_set_io`
4. Comparer la séquence obtenue à une implémentation PipeWire native de `pw_client_node`.
5. Ne pas revenir sur `PwAdapter` ou `PwFilter` sans hypothèse nouvelle.

## Résumé très court

- `PwExportedNode` : trop faible, abandon
- `PwClientNode` : meilleure piste, bloque sur `port_*`
- `PwAdapter` : sink visible, monitor inutilisable
- `PwFilter` : node visible, mais pas une vraie target pour `mpv`
- cause non résolue à ce stade, mais le meilleur point de reprise est clairement `PwClientNode`
