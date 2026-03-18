# Omniphony Studio

![Omniphony Studio preview](omniphony-studio.png)

Omniphony Studio est l’interface de supervision, de visualisation 3D et de contrôle live de la suite Omniphony.

Le projet est conçu pour fonctionner avec `omniphony-renderer`, qui fournit le moteur temps réel, l’état OSC et les contrôles audio utilisés par le studio. Le studio ne produit pas le rendu audio lui-même : il sert d’interface de supervision et de pilotage pour `orender`.

## Principe

- Le serveur écoute des messages OSC en UDP.
- Au démarrage, le studio envoie `/omniphony/register [listen_port]` vers `<host>:9000` pour s’enregistrer auprès du renderer.
- Tant qu’il est actif, le studio envoie `/omniphony/heartbeat [listen_port]` toutes les 5 secondes vers la même destination pour maintenir l’inscription côté renderer.
- Les positions reçues sont diffusées en WebSocket au front web.
- Le front affiche chaque source comme une sphère dans un volume 3D normalisé `[-1, 1]`.
- Le menu **Layout** permet de choisir la configuration d’enceintes chargée depuis `../layouts/*.yaml` et affichée dans la scène.

## Formats OSC supportés

Le serveur accepte le format historique du prototype et des variantes avec identifiant embarqué dans l’adresse ou coordonnées sphériques.

### 1) Position cartésienne

```text
/source/position id x y z
```

### 2) Position cartésienne avec id dans l’adresse

```text
/source/<id>/position x y z
/object/<id>/position x y z
/channel/<id>/position x y z
```

### 3) Position sphérique

```text
/source/<id>/aed azimuth elevation distance
```

### 4) Suppression d'une source

```text
/source/remove id
/source/<id>/remove
```

## Options CLI

```bash
node server.js --host 127.0.0.1 --osc-port 0 --osc-rx-port 9000 --http-port 3000
```

- `--osc-port` : port UDP local d’écoute OSC. `0` laisse l’OS choisir un port dynamique.
- `--host` / `--osc-host` : hôte renderer cible pour l’enregistrement.
- `--osc-rx-port` : port UDP côté renderer recevant `/omniphony/register` et les contrôles.
- `--http-port` : port HTTP du studio.

## Lancer le projet

```bash
npm install
npm start
```

Puis ouvrir : [http://localhost:3000](http://localhost:3000)

## Vérification rapide

```bash
node --test
```

## Notes de dev

- Vue 3D, sélection d’un HP :
  ne pas réutiliser tel quel le matériau normal des objets pour représenter les non-contributeurs. La sphère source de base est déjà chaude/orangée, donc même une version "normale mais estompée" peut être perçue comme rouge. Si on veut distinguer les contributeurs d’un HP sélectionné, il faut soit :
  - ne colorer que les contributeurs
  - soit mettre les non-contributeurs dans un état vraiment neutre ou quasi invisible
  - mais éviter de compter sur la seule baisse d’opacité du matériau source

## Messages envoyés par le studio vers le renderer

| Message OSC | Fréquence | Args |
|---|---|---|
| `/omniphony/register` | une fois au démarrage | `[int listen_port]` |
| `/omniphony/heartbeat` | toutes les 5 s | `[int listen_port]` |

## Heartbeat renderer

- `/omniphony/heartbeat/ack` : la session est valide.
- `/omniphony/heartbeat/unknown` : le studio se ré-enregistre automatiquement avec `/omniphony/register`.
- timeout d’ACK (> ~10 s) : le studio tente périodiquement un `/omniphony/register` jusqu’au retour des réponses.
