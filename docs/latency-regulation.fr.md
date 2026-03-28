# Algorithme De Régulation De La Latence

Ce document décrit la régulation actuelle de la latence de sortie temps réel dans `omniphony-renderer`, avec un focus sur le modèle de contrôle partagé et sur les différences backend entre `ASIO` et `PipeWire`.

## Objectifs

Le contrôleur de latence a quatre rôles principaux :

1. Maintenir la sortie audible proche d'une latence cible configurée.
2. Récupérer proprement après une dérive basse ou haute du buffer, sans laisser fuiter d'audio instable.
3. Supporter le resampling adaptatif local lorsqu'il est activé.
4. Exposer assez d'état à l'UI pour rendre les recoveries observables.

La cible de long terme n'est pas "la latence la plus basse possible". La cible est "une latence stable proche du setpoint demandé, avec un comportement de recovery prévisible".

## Modèle Central

La logique de régulation partagée vit dans [adaptive_runtime.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/audio_output/src/adaptive_runtime.rs).

### Domaines

Deux domaines d'échantillons sont importants :

- Domaine d'entrée : échantillons décodés/rendus écrits dans le ring buffer du backend.
- Domaine de sortie : échantillons réellement consommés par le callback backend après resampling local.

Le contrôle de latence est volontairement exprimé dans le domaine d'entrée, pour que le contrôleur raisonne sur le même stock audio indépendamment du sample rate de sortie.

### Grandeurs Mesurées

À chaque callback, le backend calcule :

- `available_input_samples` : remplissage courant du ring buffer.
- `output_fifo_input_domain_samples` : contenu du FIFO du resampler local reconverti en samples du domaine d'entrée.
- `callback_input_domain_samples` : taille du callback reconvertie dans le domaine d'entrée.
- `control_available` : `ring + output_fifo - callback/2`.
- `control_latency_ms` : `control_available / (sample_rate * channels)`.
- `measured_latency_ms` : `control_latency_ms + estimation de latence graphe/backend`.

`control_latency_ms` est la quantité utilisée pour la régulation. `measured_latency_ms` est l'estimation totale affichée à l'utilisateur.

### Remplissage Cible

La latence cible est convertie en niveau de remplissage cible :

- target fill = `target_latency_ms * input_sample_rate * channel_count / 1000`

Ce niveau de remplissage est le centre du contrôleur.

## Machine D'État De Recovery Partagée

La machine d'état de recovery expose les états UI :

- `stable`
- `low-recover`
- `settling`
- `high-recover`

### Low Recovery

Le low recovery est utilisé quand le buffer tombe trop en dessous de la cible.

Progression :

1. `stable -> low-recover`
2. `low-recover -> settling`
3. `settling -> stable`

Pendant `low-recover`, la sortie est mutée.

### Settling

`settling` existe pour éviter de rouvrir l'audio immédiatement après le refill. Le but est de rendre la latence effective de retour moins aléatoire.

Comportement actuel :

- la sortie reste mutée
- si le buffer retombe franchement trop bas, retour en `low-recover`
- si le buffer est un peu trop haut, on trim pendant le mute
- si le buffer reste assez longtemps dans la fenêtre de settling, transition vers `stable`

Temps de sortie actuel :

- `200 ms` de temps stable cumulé dans la fenêtre

Demi-fenêtre de settling actuelle :

- `max(callback_input_domain_samples / 4, near_far_threshold_samples / 2)`

Donc la fenêtre de settling n'est plus dimensionnée uniquement par la taille du callback ; elle est aussi ancrée sur la bande `near/far` configurée.

### High Recovery

Le high recovery est utilisé quand le buffer dépasse trop la cible.

Comportement :

- on jette agressivement de l'audio bufferisé pendant le mute
- on revient vers la cible plus vite que par la seule servo lente

## Logique Near/Far

La bande `near/far` est dérivée de l'erreur de buffer par rapport à la cible :

- `near` si `abs(control_available - target_fill) < near_far_threshold`
- `far` sinon

Cette bande sert à la fois pour l'UI et pour décider si les actions de far-mode sont éligibles.

La distinction importante est :

- la bande indique à quelle distance on est de la cible
- l'état de recovery indique ce que la machine de recovery est réellement en train de faire

Les deux sont liés, mais ce n'est pas la même information.

## Resampling Adaptatif Local

Quand le resampling adaptatif est activé, une servo PI décale légèrement le ratio du resampler local autour du ratio de base.

La logique partagée vit dans :

- [lib.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/audio_output/src/lib.rs)
- [adaptive_runtime.rs](/home/user/dev/spatial-renderer/Omniphony/omniphony-renderer/audio_output/src/adaptive_runtime.rs)

Entrées :

- remplissage de contrôle courant
- remplissage cible
- gains configurés `kp_near`, `ki`
- `max_adjust`
- `integral_discharge_ratio`

Sorties :

- ratio effectif du resampling local
- valeur affichée de rate-adjust
- bande adaptative courante (`near` ou `far`)

La boucle PI n'est qu'une partie du système. Elle ne remplace pas les hard recoveries. Elle essaie de recentrer le système avant qu'un hard recovery devienne nécessaire.

## Comportement Au Démarrage

### ASIO

Le démarrage ASIO réutilise maintenant la machine d'état normale de low recovery au lieu d'utiliser un pre-fill gate dédié.

Flux actuel :

1. le stream démarre muté en `low-recover`
2. le refill se fait avec la même logique que pour un low-buffer recovery classique
3. `settling` stabilise la latence de retour
4. transition vers `stable`

En plus, quand le recovery de démarrage se termine, ASIO reset explicitement :

- l'état interne du resampler local
- le FIFO du resampler

et garde encore un callback muté avant de rendre le premier bloc audible. Le but est d'éviter qu'un transitoire de démarrage accumulé dans l'état du resampler ne fuie vers la sortie.

### PipeWire

PipeWire n'utilise pas exactement le même chemin de démarrage dédié. Son cycle de stream et de callbacks est déjà piloté par le graphe PipeWire, donc le démarrage dépend moins d'un gate spécifique.

## Différences ASIO / PipeWire

C'est la section backend spécifique la plus importante.

### 1. Modèle De Callback

`ASIO` :

- la taille de callback est déterminée par le driver / backend CPAL
- elle peut être relativement grossière et très dépendante du driver
- cela rend les seuils de recovery plus sensibles à la granularité du callback

`PipeWire` :

- la cadence des callbacks est liée au quantum du graphe
- elle est en général plus régulière
- cela facilite le tuning du settling et de la servo

### 2. Mesure De Latence

`ASIO` :

- n'a pas actuellement de vraie mesure directe de latence graphe/driver
- utilise une estimation de milieu de callback
- la latence totale affichée est donc un modèle, pas une valeur driver mesurée

`PipeWire` :

- échantillonne la latence graphe downstream via `pw_stream_get_time()`
- inclut un vrai délai de scheduling du graphe dans `measured_latency_ms`

C'est pour ça que deux backends peuvent sembler aussi stables à l'oreille tout en affichant des chiffres de latence différents.

### 3. Comportement Sans Resampling

`ASIO` :

- sans resampling adaptatif local, il repose toujours sur la logique de recovery far-mode partagée
- il n'y a pas d'équivalent séparé à la servo backend native de PipeWire

`PipeWire` :

- a deux régimes :
  - chemin avec resampler local
  - chemin avec servo native backend quand le resampler local n'est pas utilisé

Donc PipeWire est structurellement plus flexible, mais les deux backends ne sont pas des miroirs exacts.

### 4. Stratégie De Démarrage

`ASIO` :

- le démarrage est maintenant explicitement traité comme un low recovery
- le mute / recovery / fade suit volontairement la même logique qu'une recovery low classique

`PipeWire` :

- le démarrage est plus naturellement absorbé par le cycle de callback du backend
- n'a pas besoin du même forçage de recovery de démarrage

### 5. Sensibilité Aux Seuils

`ASIO` est plus sensible à :

- la largeur de fenêtre de settling
- les seuils de transition refill / settling
- le nettoyage des transitoires de démarrage

`PipeWire` est plus sensible à :

- la taille de quantum du graphe
- la qualité de la mesure de latence backend
- la séparation entre contrôle par resampler local et contrôle natif backend

## Interprétation Pratique Actuelle

Quand on debug le système, il faut interpréter les états comme suit :

- `stable` : aucune machine de recovery active
- `low-recover` : la sortie est mutée parce que le système reconstruit la latence depuis un buffer trop bas
- `settling` : la sortie est toujours mutée pendant que le système essaie de revenir à une latence moins aléatoire
- `high-recover` : de l'audio bufferisé est jeté parce que la latence est trop haute
- `near` / `far` : distance à la cible, pas état de mute en soi

Si l'audio se comporte mal, il faut toujours regarder à la fois :

- la bande : `near` / `far`
- l'état : `stable` / `low-recover` / `settling` / `high-recover`

La bande explique où le contrôleur se situe par rapport à la cible. L'état explique ce que la machine de recovery est réellement en train de faire.
