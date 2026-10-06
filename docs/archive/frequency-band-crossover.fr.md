# Crossover Multi-Bandes par Fréquence

## Objectif

Ce document décrit l'implémentation du crossover fréquentiel dans Omniphony, qui permet de router chaque objet audio vers un sous-ensemble d'enceintes selon la plage de fréquences, avec des gains VBAP indépendants par bande.

Le cas d'usage principal est un système avec des subwoofers (enceintes basse fréquence) et des enceintes large-bande : chaque objet est splitté en bandes, et chaque bande est spatialisée uniquement sur les enceintes capables de la reproduire.

---

## Activation

Le crossover s'active automatiquement dès qu'au moins une enceinte `spatializable` du layout courant possède un champ `freq_low` ou `freq_high` non nul.

```json
{
  "id": "Sub_L",
  "az": -30, "el": 0, "r": 3.0,
  "spatialize": true,
  "freqLow": 20,
  "freqHigh": 80
}
```

`freq_low` / `freqLow` définit la fréquence minimale reproductible par l'enceinte.
`freq_high` / `freqHigh` définit sa fréquence maximale.

Une enceinte sans `freq_low` est considérée comme valide à partir de 0 Hz.
Une enceinte sans `freq_high` est considérée comme valide jusqu'à l'infini.

---

## Architecture Générale

```
    objet audio PCM
         │
         ▼
  LR4CrossoverBank
   ┌─────┴──────┐
   │  splitter  │  fc = 80 Hz
   └─────┬──────┘
    LP ◄─┘   HP = signal − LP
    │              │
    ▼              ▼
 Bande 0        Bande 1
(< 80 Hz)     (≥ 80 Hz)
    │              │
    ▼              ▼
 VBAP sur       VBAP sur
 subwoofers   enceintes LB
    │              │
    └──────┬───────┘
           ▼
     mix de sortie
```

Pour N bandes, N−1 splitters LR4 sont chaînés. La somme de toutes les bandes reconstruit le signal original à l'erreur numérique près (< 1e-5).

---

## Calcul des Bandes — `crossover/bands.rs`

`compute_bands(layout)` dérive les bandes fréquentielles depuis l'ensemble des `freq_low` et `freq_high` des enceintes spatialisables :

1. Collecter toutes les valeurs finies `freq_low` et `freq_high` distinctes des enceintes spatialisables.
2. Trier et dédupliquer (tolérance 0.1 Hz).
3. Construire les arêtes : `[0, f1, f2, …, ∞]`
4. Produire un `FreqBand` par intervalle `[lo, hi)`.

Une enceinte est incluse dans la bande `[lo, hi)` si sa plage utile chevauche la bande :

```rust
freq_low.unwrap_or(0.0) < hi
    && freq_high.unwrap_or(f32::INFINITY) >= lo
```

```
Exemple : freq_low = {120, 250}, freq_high = {80, 200}
  Arêtes    : [0, 80, 120, 200, 250, ∞]
  Bande 0   : [0–80 Hz]    → enceintes dont la plage couvre 0–80
  Bande 1   : [80–120 Hz]  → enceintes dont la plage couvre 80–120
  Bande 2   : [120–200 Hz] → enceintes dont la plage couvre 120–200
  Bande 3   : [200–250 Hz] → enceintes dont la plage couvre 200–250
  Bande 4   : [250–∞ Hz]   → enceintes dont la plage couvre 250–∞
```

L'affectation n'est **pas exclusive** : une enceinte peut apparaître dans plusieurs bandes si sa plage fréquentielle en chevauche plusieurs.

Les bandes vides éventuelles sont conservées pour garder une topologie fidèle aux coupures configurées.

Si aucun `freq_low` ni `freq_high` n'est défini, une seule bande `[0, ∞)` est retournée et le chemin crossover n'est pas activé.

---

## Filtres — `crossover/filter.rs`

### Filtre Linkwitz-Riley 4e ordre (LR4)

Chaque splitter est une paire de filtres passe-bas Butterworth du 2e ordre en cascade :

```
LR4_LP(x) = BW2_LP(BW2_LP(x))
LR4_HP(x) = x − LR4_LP(x)          ← complément exact
```

La propriété fondamentale est : `LR4_LP(x) + LR4_HP(x) = x` — reconstruction parfaite garantie sans compromis de phase, sans délai de groupe supplémentaire, et sans déphasage entre bandes.

> **Note d'implémentation** : un filtre HP Butterworth séparé *ne* produit *pas* de reconstruction parfaite avec un LP Butterworth (la somme n'est pas plate). Le HP doit être le complément arithmétique.

### Coefficients Butterworth 2e ordre

```
k    = tan(π × fc / fs)
q    = √2
norm = 1 + k/q + k²
b0   = k²/norm,  b1 = 2b0,  b2 = b0
a1   = 2(k²−1)/norm,  a2 = (1 − k/q + k²)/norm
```

Implémentation Direct Form II Transposée (stable, faible accumulation d'erreur numérique).

### État par objet

Chaque objet audio maintient un vecteur d'états de biquads alloué à la première frame :

```
état count = 2 × (N_bandes − 1)
```

Les états sont persistants entre frames (filtre à mémoire) — ils sont stockés dans `SpatialRenderer::crossover_filter_states: HashMap<usize, Vec<BiquadState>>`, indexé par `input_channel_idx`.

---

## Intégration dans le Renderer — `spatial_renderer.rs`

### Construction

Au moment de `SpatialRenderer::new()`, si le layout contient des coupures finies (`freq_low` et/ou `freq_high`) :

1. `compute_bands(layout)` → `Vec<FreqBand>` stocké dans `self.crossover_bands`
2. Extraction des fréquences de coupure depuis les bandes
3. `LR4CrossoverBank::new(cutoffs, sample_rate)` → `self.crossover_filter_bank`

### Chemin de rendu (par objet, par frame)

```rust
if self.crossover_filter_bank.is_some() {
    // Allocation lazy des états de filtre pour cet objet
    let obj_states = self.crossover_filter_states.entry(idx).or_insert(...);

    // Calcul des gains VBAP par bande (position finale ou interpolée)
    let band_gains: Vec<Gains> = self.crossover_bands.iter()
        .map(|b| b.compute_gains(render_params, position))
        .collect();

    // Mix sample par sample
    for sample_idx in 0..sample_length {
        let bands = filter_bank.process_sample(raw_sample, obj_states);
        for (b, band) in self.crossover_bands.iter().enumerate() {
            for (gi, &g) in band_gains[b].iter().enumerate() {
                output[band.speaker_indices[gi]] += bands.get(b) * g;
            }
        }
    }

    // Monitoring : gains étendus à num_speakers pour compatibilité
    object_band_gains_out.push((idx, full_band_gains));
    object_gains_out.push((idx, summed_gains));
    continue;   // ← saute le chemin VBAP mono-bande
}
```

Les trois `RampMode` (Off, Frame, Sample) sont supportés. `last_band_gains` capture les gains de la dernière position calculée pour le monitoring.

### Gains en taille pleine (full-size)

Les gains retournés par `FreqBand::compute_gains()` sont indexés localement (indices 0..N dans la bande). Avant d'être émis vers le studio, ils sont étendus à `num_speakers` entrées avec les gains des enceintes hors-bande à 0 :

```rust
let mut full = Gains::zeroed(self.num_speakers);
for (gi, &g) in bg.iter().enumerate() {
    full[band.speaker_indices[gi]] = g;
}
```

---

## Pipeline de Monitoring

Les gains par bande sont transmis au studio à chaque frame via OSC :

```
/omniphony/meter/object/{id}/band/{b}/gains   [g_spk0, g_spk1, …]
```

Un message par bande, un float par enceinte (gain VBAP linéaire, 0–1).

Le gain scalaire `object_gains` (somme des bandes par enceinte) est également émis pour maintenir la compatibilité avec le slider de contribution existant :

```
/omniphony/meter/object/{id}/gains   [g_spk0, g_spk1, …]
```

---

## Studio — Réception et Affichage

### Rust backend (`omniphony-studio`)

| Fichier | Rôle |
|---|---|
| `osc_parser.rs` | Parse `/band/{b}/gains` → `OscEvent::MeterObjectBandGains` |
| `app_state.rs` | `object_band_gains: HashMap<String, Vec<Vec<f64>>>` |
| `osc_listener.rs` | Met à jour le cache, émet l'événement Tauri `source:band_gains` |

### Frontend JS

| Fichier | Rôle |
|---|---|
| `state.js` | `sourceBandGains: Map<id, Vec<Vec<f64>>>` |
| `tauri-bridge.js` | Listener `source:band_gains` → `updateSourceBandGains()` |
| `sources.js` | `updateSourceBandGains()`, `getSelectedSourceBandContributions()` |
| `speakers.js` | `updateSpeakerBandBars()`, DOM `.band-row` par enceinte |

### Affichage dans la liste des enceintes

Quand un objet est sélectionné et que le crossover est actif, chaque enceinte de la liste affiche des barres de contribution par bande :

```
< 80 Hz   ████░░░░░░  -6.0 dB
≥ 80 Hz   ██████████  -0.5 dB
```

- La plage fréquentielle est calculée côté frontend depuis `app.currentLayoutSpeakers[].freqLow` et `freqHigh` (même algorithme que `compute_bands`).
- Le gain est affiché en dB (`20 × log₁₀(gain_linéaire)`).
- Les barres disparaissent si ≤ 1 bande (pas de crossover actif).
- Couleurs : cyan (bande 0), vert (1), jaune (2), rouge (3).

---

## Fichiers Modifiés

| Fichier | Nature du changement |
|---|---|
| `renderer/src/crossover/bands.rs` | Nouveau — calcul des bandes depuis le layout |
| `renderer/src/crossover/filter.rs` | Nouveau — LR4 bank + SmallBands |
| `renderer/src/crossover/mod.rs` | Nouveau — module crossover |
| `renderer/src/speaker_layout.rs` | Ajout `freq_low` / `freq_high` + builders associés |
| `renderer/src/spatial_renderer.rs` | Chemin crossover dans `render_frame()`, `RenderedFrame::object_band_gains` |
| `src/runtime_osc/state_emit.rs` | Émission `/band/{b}/gains` dans `send_meter_bundle()` |
| `src/cli/decode/sample_write.rs` | Passage de `object_band_gains` aux 2 call sites |
| `src-tauri/src/osc_parser.rs` | `MeterObjectBandGains` + parsing |
| `src-tauri/src/app_state.rs` | `object_band_gains` + `freq_low` / `freq_high` dans `LiveSpeakerConfig` |
| `src-tauri/src/osc_listener.rs` | Handler + émission `source:band_gains` |
| `src-tauri/src/main.rs` | Commandes `control_speaker_freq_low` et `control_speaker_freq_high` |
| `src-tauri/src/layouts.rs` | `freq_low` / `freq_high` dans `Speaker` et `RawSpeaker` |
| `src/state.js` | `sourceBandGains` Map |
| `src/tauri-bridge.js` | Listener `source:band_gains` |
| `src/sources.js` | `updateSourceBandGains()`, `getSelectedSourceBandContributions()` |
| `src/speakers.js` | `updateSpeakerBandBars()`, `updateAllSpeakerBandBars()`, DOM |
| `src/styles/app.css` | `.band-row`, `.band-bar`, `.band-label`, `.band-db` |
| `src/index.html` | Champs `freqLow` et `freqHigh` dans l'éditeur d'enceinte |
| `src/listeners/speaker-editor-listeners.js` | Listeners sur `speakerEditFreqLowInput` et `speakerEditFreqHighInput` |

---

## Limites et Points d'Attention

- **Maximum 8 bandes** : `SmallBands` est backed par un tableau de taille fixe `[f32; 8]`, soit 7 fréquences de coupure au maximum.
- **Reconstruction parfaite** : garantie par construction (`HP = signal − LP`). Vérifiée par les tests unitaires dans `filter.rs` (erreur < 1e-5 après état stable).
- **Pas de crossover = pas d'overhead** : si aucun `freq_low` ni `freq_high` n'est défini, `crossover_filter_bank` reste `None` et le chemin standard est emprunté.
- **États de filtre par objet** : alloués lazily et jamais libérés (HashMap). Si un objet disparaît et réapparaît, ses états sont remis à zéro par la réallocation.
- **Monitoring de gain** : le champ `object_gains` (slider de contribution) contient la *somme* des gains de toutes les bandes, ce qui n'est pas un gain VBAP au sens strict pour les enceintes présentes dans plusieurs bandes. Il reste utile comme indicateur d'activité global.
