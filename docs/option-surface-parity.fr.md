# Matrice de parité des options (CLI / Studio live / mpv-omniphony)

Ce document recense les options du renderer Omniphony et leur disponibilité sur
les trois surfaces de contrôle, avec pour chaque écart la raison (justifié vs à
corriger). Il sert de référence pour le chantier de parité
`feat/option-surface-parity`.

## Les trois surfaces

| Surface | Mécanisme | Moment |
|---|---|---|
| **CLI** (`orender`) | flags clap → config YAML | démarrage (+ `--save-config`) |
| **Studio live** | OSC `/omniphony/control/*` | à chaud |
| **mpv-omniphony** | OSC `/omniphony/control/*` (via `liborender`) | à chaud |

Studio et mpv partagent la **même surface OSC** : une option éditable dans l'un
l'est généralement dans l'autre. La différence vient des **capabilities** que le
renderer annonce (`runtime_control/src/snapshot.rs::build_renderer_capabilities_json`) :
en mode embarqué (mpv), `liborender` est **sans audio** (mpv possède la chaîne
audio), donc les domaines `audio` et `input` sont retirés.

Sources de vérité : `omniphony-renderer/src/cli/command.rs` (CLI),
`renderer/src/config.rs` + `renderer/src/config_fields.rs` (config),
`renderer/src/options.rs` (registre des options live),
`runtime_control/src/osc.rs` + `orender_engine/src/osc/dispatch.rs` +
`runtime_control/src/command.rs` (OSC), `osc-contract/src/lib.rs` (adresses).

Légende statut : ✅ OK · 🟡 écart **justifié** (ne pas corriger) · 🔴 écart **à corriger**.

---

## Matrice

### Spatialisation cœur (VBAP / évaluation)

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `enable_vbap` | ✅ | ✅ | ✅ | ✅ | — |
| résolutions polaires (az/el/dist/dist-max) | ✅ | ✅ | ✅ | ✅ | — |
| grille cartésienne (x/y/z/z-neg) | ✅ | ✅ | ✅ | ✅ | — |
| `render_evaluation_mode` (polar/cartesian) | ✅ | ✅ | ✅ | ✅ | — |
| `position_interpolation` | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_allow_negative_z` | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_table` (table précalculée) | ✅ | — | — | 🟡 | chemin *load-time*, non éditable à chaud (réinit). |
| `speaker_layout` / `current_layout` | ✅ | ✅ | ✅ | ✅ | édition live via `config/layout`. |

### Sélection de backend

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `render_backend` (vbap/barycenter/experimental_distance/hybrid) | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 1)** : `--render-backend`. |
| `barycenter` (localize) | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 1)** : `--barycenter-localize`. |
| `experimental_distance_*` (6 params) | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 1)** : `--experimental-distance-*`. |
| `hybrid_external/internal/smoothing/metric` | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 1)** : `--hybrid-external-backend`, `--hybrid-internal-backend`, `--hybrid-curve-smoothing`, `--hybrid-metric`. |
| `hybrid_curve` (`Vec<[f32;2]>`) | — | ✅ | ✅ | 🟡 | courbe via éditeur canvas Studio ; pas adapté à un flag CLI. Reste Studio-only. |

### Distance / spread

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `spread_from_distance`, `spread_distance_range/curve` | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_spread_min/max` | ✅ | ✅ | ✅ | ✅ | — |
| `distance_diffuse` (+threshold/curve) | ✅ | ✅ | ✅ | ✅ | — |
| `vbap_distance_model` (none/linear/…) | ✅ | ✅ | ✅ | ✅ | — |
| `distance_model_metric` (spherical/chebyshev) | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 2)** : `--distance-model-metric`. |
| `distance_diffuse_metric` (spherical/chebyshev) | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 2)** : `--distance-diffuse-metric` (+ `--distance-diffuse-mirror-axes`). |
| `size_to_spread_mode` (max/mean/projection_perpendicular) | ✅ | ✅ | ✅ | ✅ | **Corrigé (Partie 2)** : `--size-to-spread-mode`. |

### Gain / loudness

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `master_gain` | ✅ | ✅ | ✅ | ✅ | — |
| `use_loudness` | ✅ | ✅ | ✅ | ✅ | — |
| `auto_gain` | ✅ | ✅ | ✅ | ✅ | **Corrigé** : `auto_gain` est désormais un live param (`/omniphony/control/auto_gain`), toggle Studio + persisté. Renderer-domain → marche aussi en mpv. |

### Géométrie de la pièce

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `room_ratio` + rear/lower/center_blend | ✅ | ✅ | ✅ | ✅ | — |
| `room_*_m` (mètres) | (ratios) | ✅ | ✅ | 🟡 | représentation alternative ; le CLI exprime l'équivalent via `--room-ratio*`. Pas un manque fonctionnel. |

### Bed conformance

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `bed_conform` | ✅ | — | — | 🟡 | **Écart justifié** (révisé) : `bed_conform` n'est pas un paramètre renderer-domain mais un mode de **conformance de sortie** couplé à l'écrivain audio du CLI (`src/cli/decode/` : sortie d'un bed 7.1.2 brut + canaux objets, recréation du writer sur changement de nb de canaux). Les beds eux-mêmes sont **déjà** gérés côté moteur dans le VBAP (`configure_beds`/`bed_indices`) — opérant en CLI comme en mpv. mpv possède sa propre chaîne de sortie, donc le mode de conformance brut ne s'y applique pas. Non porté. |

### Sortie audio / latence / resampling (host audio)

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `output_device`, `output_sample_rate` | ✅ | ✅ | 🟡 | 🟡 | mpv possède la chaîne audio → domaine `audio` retiré des capabilities. **Justifié.** |
| `latency_target` | ✅ | ✅ | 🟡 | 🟡 | idem. **Justifié.** |
| `pw_quantum` | ✅ | — | — | 🟡 | *load-time* PipeWire. **Justifié.** |
| `enable_adaptive_resampling` | ✅ | ✅ | 🟡 | 🟡 | resampling = host audio ; sans objet en mpv. **Justifié.** |
| tuning PI (`kp_near`, `ki`, `max_adjust`, far-mode, marges…) | ✅ | ✅ | 🟡 | 🟡 | **Corrigé en standalone (Partie 3)** : `--adaptive-resampling-kp-near`, `--adaptive-resampling-ki`, `--adaptive-resampling-max-adjust`, `--adaptive-resampling-*-far-mode*`, marges de récupération. En mpv : sans objet (host audio absent) → justifié. |
| `adaptive_resampling_integral_discharge_ratio` | — | (✅) | — | 🟡 | **non opérant** → volontairement non exposé en CLI (cf. note). |
| `ramp_mode` | ✅ | ✅ | 🟡 | 🟡 | géré par le pipeline de rendu mpv en embarqué. **Justifié.** |

### Entrée live

| Option | CLI (`input-live`) | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `live_input.*` (backend/node/channels/format/clock/map/lfe) | ✅ | ✅ | 🟡 | 🟡 | mpv fournit l'entrée décodée → domaine `input` retiré. **Justifié.** |

### OSC / monitoring / divers

| Option | CLI | OSC/Studio | mpv | Statut | Pourquoi |
|---|:--:|:--:|:--:|:--:|---|
| `osc`, `osc_host`, `osc_port`, `osc_rx_port` | ✅ | n/a | n/a | ✅ | configuration du transport OSC lui-même. |
| `osc_metering` | ✅ (startup) | ✅ (par client) | ✅ | ✅ | CLI pré-active ; Studio/mpv togglent par client à chaud. |
| `meter_rate` / `diag_rate` (cadences) | 🔴 | ✅ | ✅ | 🔴 | aucun flag CLI. **Hors-scope ce tour-ci** (à corriger plus tard). |
| `drc_mode` / `drc_weight` | 🔴 | ✅ | ✅ | 🔴 | aucun flag CLI. **Hors-scope ce tour-ci.** |
| `presentation` (substream) | ✅ | — | — | 🟡 | sélection *load-time* du bridge. **Justifié.** |
| `bridge_path` | ✅ | ✅ | (host) | ✅ | éditable via `render/bridge_path`. |
| `continuous`, `no_drain_pipe`, `log_object_positions` | ✅ | — | — | 🟡 | comportements *load-time* / debug. **Justifié.** |

---

## Synthèse des écarts

Faits :

1. **Sélection de backend en CLI** : `--render-backend` + params barycenter / hybrid / experimental_distance → **Partie 1**.
2. **Métriques & size_to_spread en CLI** : `--distance-model-metric`, `--distance-diffuse-metric`, `--size-to-spread-mode` → **Partie 2**.
3. **Tuning resampling en CLI** (standalone) : flags PI au-delà de enable/update-interval → **Partie 3**.
4. **`auto_gain`** : contrôle live OSC/Studio → **Partie 4**. `bed_conform` : **réévalué comme écart justifié** (mode de sortie couplé au CLI, beds déjà gérés côté moteur) → non porté.

Restent à corriger (🔴) : `meter_rate`/`diag_rate` et `drc_mode`/`drc_weight`
n'ont toujours pas de flag CLI.

## Note — `integral_discharge_ratio`

Le paramètre `adaptive_resampling_integral_discharge_ratio` est **non opérant**
sur l'implémentation actuelle du PI adaptive resampling. Il est volontairement
**exclu** de toute exposition CLI et de tout outil de tuning/recommandation,
même si la doc le mentionne encore.
