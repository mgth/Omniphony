# DSP Validation — Phase 1 Measurement Report

Measured on 2026-07-30, commit 8160dbb, x86_64 Linux, `cargo test -p renderer`.

Thresholds are theory-derived (see
`docs/superpowers/specs/2026-07-30-dsp-validation-harness-design.md`, D2). A
"misses" verdict is a finding about the engine, not a defect in the test.

| Metric | Theoretical target | Measured | Verdict |
| --- | --- | --- | --- |
| LR4 reconstruction flatness (4 bands, 48 kHz) | ±0.25 dB | +0.0018 dB at 320.8 Hz | meets |
| VBAP energy conservation (7.1.4, 512 dirs) | ±0.25 dB | −24.6592 dB at az=66.5° el=−86.4° | misses |
| VBAP seam continuity ratio (7.1.4, 512 dirs) | < 0.65 | 0.9991 at az=77.7° el=−22.6° | misses |
| ITD magnitude vs model (worst of 7 azimuths) | ±3 samples | +36.822 samples at az=+90° | misses |
| ITD antisymmetry (worst of ±30/60/90°) | \|sum\| ≤ 1 sample | −1.829 samples at ±60° | misses |
| ITD monotonicity (0/30/60/90°) | strictly increasing | [0.104, 13.713, 26.062, 5.343] — falls at 90° | misses |

## Raw output

```
[measure] lr4_flatness cutoffs=[80.0, 200.0, 500.0] fs=48000: worst deviation +0.0018 dB at 320.8 Hz (target ±0.25 dB)
[measure] vbap_energy 7.1.4 (11 spatialized speakers, 512 directions): worst -24.6592 dB at az=66.5 el=-86.4 (target ±0.25 dB)
[measure] vbap_seams 7.1.4 (512 directions): worst ‖Δ0.5°‖/‖Δ1°‖ ratio 0.9991 at az=77.7 el=-22.6 (continuous ⇒ ≈0.5, target <0.65)
[measure] itd az=  +0.0°: measured  +0.104 samples (   +2.2 µs), model  +0.000 samples, delta +0.104 samples (target ±3)
[measure] itd az= +30.0°: measured -13.713 samples ( -285.7 µs), model -12.534 samples, delta -1.180 samples (target ±3)
[measure] itd az= -30.0°: measured +12.212 samples ( +254.4 µs), model +12.534 samples, delta -0.322 samples (target ±3)
[measure] itd az= +60.0°: measured -26.062 samples ( -543.0 µs), model -23.427 samples, delta -2.635 samples (target ±3)
[measure] itd az= -60.0°: measured +24.234 samples ( +504.9 µs), model +23.427 samples, delta +0.806 samples (target ±3)
[measure] itd az= +90.0°: measured  +5.343 samples ( +111.3 µs), model -31.479 samples, delta +36.822 samples (target ±3)
[measure] itd az= -90.0°: measured  -3.903 samples (  -81.3 µs), model +31.479 samples, delta -35.383 samples (target ±3)
[measure] itd antisymmetry ±30°: -13.713 vs +12.212, sum -1.502 samples (target |sum| ≤ 1)
[measure] itd antisymmetry ±60°: -26.062 vs +24.234, sum -1.829 samples (target |sum| ≤ 1)
[measure] itd antisymmetry ±90°: +5.343 vs -3.903, sum +1.440 samples (target |sum| ≤ 1)
[measure] itd monotonicity |lag| at 0/30/60/90°: [0.103639506, 13.713405, 26.06226, 5.343322] (target strictly increasing)
```

## Observations

**LR4 flatness** is the one metric with margin to spare: +0.0018 dB is roughly
two orders of magnitude inside the ±0.25 dB tolerance, which is what the
allpass-cascade argument predicts once coefficient and float error are the only
contributors left.

**VBAP energy** is conserved almost everywhere but collapses toward the nadir.
The worst direction, el = −86.4°, is below every speaker in the 7.1.4 layout —
there is no triplet enclosing it, so the reported −24.66 dB is a measurement of
what the panner does outside the convex hull, not of the normalisation itself.
Whether that region should be gated at all, clamped, or excluded from the sweep
is a decision for phase 2 rather than something to ratchet the tolerance around.

**VBAP seams**: a ratio of 0.9991 is the signature of a jump — halving the
angular step did not halve the gain-vector difference at all. Energy stayed
conserved at that direction, which is exactly the failure mode the ratio metric
was added to catch.

**ITD** tracks the Woodworth model well at 0°, ±30° and ±60° (worst delta −2.635
samples, inside ±3). At ±90° the measurement breaks down entirely: the sign
flips and the magnitude collapses to ~5 and ~4 samples where the model predicts
∓31.5. That single pair of points is what drives all three ITD verdicts —
magnitude (+36.8 samples of error), monotonicity (|lag| falls from 26.1 at 60°
to 5.3 at 90°), and it is the ±90° row of the antisymmetry table. Antisymmetry
additionally misses at ±30° and ±60° by 0.5 and 0.8 samples beyond the
1-sample bound, so it is not solely a ±90° artefact.

The ±90° behaviour is consistent with either a genuine engine issue at full
interaural deflection or a limitation of the cross-correlation estimator when
the contralateral HRIR is heavily shadowed and spectrally dissimilar to the
ipsilateral one — the two signals are no longer near-copies of each other, which
is the assumption the lag estimate rests on. Phase 2 records the deferral; it
does not diagnose it.

## Gating decision

There is no issue tracker in this workflow, so each metric marked "misses" is
tracked in this report instead of by issue number. Task 11 lands each as a
tracked deferral (`#[ignore]` carrying the measured value) rather than a gate.

| Metric | Issue | Deferred value recorded in `#[ignore]` |
| --- | --- | --- |
| VBAP energy conservation (`vbap_conserves_energy_over_the_sphere`) | tracked in this report | measured −24.6592 dB at az=66.5° el=−86.4°, target ±0.25 dB |
| VBAP seam continuity (`vbap_gains_are_continuous_across_triplet_boundaries`) | tracked in this report | measured ratio 0.9991 at az=77.7° el=−22.6°, target < 0.65 |
| ITD magnitude vs model (`itd_magnitude_tracks_the_model`) | tracked in this report | measured delta +36.822 samples at az=+90°, target ±3 samples |
| ITD antisymmetry (`itd_is_antisymmetric_about_the_median_plane`) | tracked in this report | measured sum −1.829 samples at ±60°, target \|sum\| ≤ 1 sample |
| ITD monotonicity (`itd_magnitude_grows_toward_the_interaural_axis`) | tracked in this report | measured \|lag\| [0.104, 13.713, 26.062, 5.343] at 0/30/60/90°, target strictly increasing |

LR4 reconstruction flatness (`lr4_reconstruction_is_magnitude_flat`) meets its
target and lands as a live gate with no deferral.

### Wide matrix

Task 12 adds an opt-in wide matrix behind `--features renderer/wide-matrix`.
Its LR4 case is a live gate; the two cases that widen an already-deferred metric
inherit that deferral, so `cargo test --workspace --features renderer/wide-matrix`
stays green unless something *new* breaks.

| Wide case | Status | Deferred value recorded in `#[ignore]` |
| --- | --- | --- |
| `lr4_reconstruction_is_magnitude_flat_wide` (3 cutoff sets × 44.1/48/96 kHz) | gate | — |
| `vbap_conserves_energy_over_the_sphere_wide` (5.1, 7.1, 7.1.4, 9.1.6 × 4 spreads, 8192 dirs) | deferred | 5.1 spread=0 has a silent direction at az=−117.4° el=86.5° |
| `itd_magnitude_tracks_the_model_wide` (13 azimuths, 30° apart) | deferred | measured delta −39.954 samples at az=−120°, target ±3 samples |

The wide VBAP case fails earlier and harder than its narrow counterpart: on the
5.1 preset the spatialized speakers are coplanar, so directions near the zenith
fall outside the convex hull entirely and receive no energy at all rather than
merely mis-normalised energy. That is the same convex-hull question the
narrow-gate observation above raises, seen from a layout with no height layer.

The wide ITD case reaches azimuths the narrow gate never visits (±120° and
±150°) and breaks down there in the same way it does at ±90°: at az = −120° the
measured lag is −16.5 samples where the model predicts +23.4, a sign flip.

## Addendum — the ITD failure bracketed

The three ITD deferrals above all report their failure at the sampled azimuths
(±90°, and ±120° in the wide matrix), which makes the defect look like a
broad lateral/rear inaccuracy. A denser sweep run afterwards shows it is
something much sharper: a **discontinuity between 80° and 85°**.

| Azimuth | Measured lag (samples) | Model (samples) | Delta |
| ---: | ---: | ---: | ---: |
| +0° | +0.104 | +0.000 | +0.10 |
| +30° | −13.713 | −12.534 | −1.18 |
| +60° | −26.062 | −23.427 | −2.64 |
| +75° | −29.950 | −27.856 | −2.09 |
| **+80°** | **−31.387** | **−29.156** | **−2.23** |
| **+85°** | **+6.991** | **−30.364** | **+37.36** |
| +88° | +5.957 | −31.044 | +37.00 |
| +89° | +5.693 | −31.264 | +36.96 |
| +90° | +5.343 | −31.479 | +36.82 |
| +91° | +5.567 | −31.264 | +36.83 |
| +95° | +6.132 | −30.364 | +36.50 |
| +100° | +2.946 | −29.156 | +32.10 |
| +120° | +15.912 | −23.427 | +39.34 |

Up to +80° the rendered ITD tracks Woodworth closely and monotonically, running
2–3 samples long in exactly the way per-ear HRIR group delay predicts. Between
+80° and +85° it **inverts sign and collapses in magnitude**, and never
recovers: every azimuth beyond that point reports the contralateral ear as the
*early* one, which inverts the primary localisation cue for all lateral and rear
sources.

Two things follow from the bracketing:

- **The measurement is sound.** A harness that agrees with the model to within
  2–3 samples across 0–80°, then disagrees by 37, is not mis-measuring; it is
  reporting a real discontinuity in the rendered output.
- **The defect is narrow.** Whatever changes between 80° and 85° is a single
  branch or lookup boundary, not a diffuse accuracy problem. The three ITD
  deferrals are very likely one root cause, not three.

Worth noting for whoever picks this up: this region is where the source
approaches the interaural axis, and it is close to — but not exactly at — the
y = 0 plane where the front/rear hemisphere distinction flips (`cos 85° ≈ 0.09`).
Both the front/back folding in `binaural/itd.rs` and the HRIR direction lookup
in `binaural/measured.rs` are plausible places for a boundary at that angle.

---

# Phase 2 — Resolution

Investigated 2026-07-30 after the phase 1 report. The headline correction: **the
ITD findings were defects in this harness, not in the renderer.** The VBAP
findings are real, and now have a root cause.

## Retraction: the ITD deferrals

The addendum above bracketed an apparent sign inversion between 80° and 85° and
argued the harness was sound because it agreed with theory up to 80°. That
reasoning was wrong.

Instrumenting `binaural/mod.rs` showed the engine computing the direction
exactly right (az = 85.00°, el = 0.00°) and applying the correct delay to the
correct ear (`itd_l = 0.000633 s` = 30.4 samples, smoothly tracking Woodworth).
The HRIR pair was fine too, and the ITD delay line holds 144 samples against a
31-sample maximum, so neither clamped.

The fault was the excitation. `render_single_object_binaural` fed the *same*
40-sample `make_pcm` buffer to every `render_frame` call, making the signal
periodic at `BLOCK_SAMPLES`. Cross-correlation resolves lag only modulo that
period, and the aliased peaks scored within 0.01 % of the true one — at az = 80°
lag −31 scored 25.2028 while lag +9 scored 25.2006. Which peak won was decided
by noise, and the loser looked like a sign inversion.

Two fixes, and all three ITD gates now pass as live gates:

| Metric | Target | Now |
| --- | --- | --- |
| ITD magnitude vs model | ±3 samples | ≤ 0.748 samples, all azimuths |
| ITD antisymmetry | \|sum\| ≤ 1 sample | exactly 0.000 at ±30/60/90° |
| ITD monotonicity | strictly increasing | [0.0, 12.84, 23.99, 32.23] |

- `make_pcm_block` generates a continuous aperiodic stream.
- `estimate_lag_checked` now refuses to resolve an ambiguous correlation and
  says why, instead of returning a confidently wrong answer. This is the change
  that stops the failure mode recurring: it turns a silent error into a loud one.
- The engine-symmetry gates run against `HrirSource::Synthetic`, which is
  time-aligned and symmetric by construction, so they measure the renderer
  rather than the bundled dataset.

### What this did uncover

The measured KEMAR set is **not left/right symmetric** and **not time-aligned**:
intrinsic interaural lag is −1.103 samples at az = +30° but −0.168 at −30°, and
reaches −6.998 at +90°, where it is not even resolvable on the −90° side. That
violates the contract `HrirProvider` states in its own doc comment —
implementors "must return time-aligned FIRs (no bulk interaural delay) for safe
interpolation" — which matters because `HrirSet` blends the three nearest
measurements, and blending misaligned FIRs combs their shared content instead of
interpolating it.

`hrir_providers_return_time_aligned_pairs` now asserts that contract and is
deferred against the bundled set. This is a genuine finding, and it is the one
the false ITD alarm was standing in front of.

## VBAP: one root cause, two symptoms, and a trade-off

Both VBAP deferrals come from the same place. Every shipped layout is a stack of
horizontal rings — 7.1.4 is {0°, 35.26°}, 5.1 and 7.1 are {−12.6°, 0°} — and
`prepare_effective_speaker_dirs` returns the first successful triangulation
without checking whether the resulting hull *covers the sphere*. It triangulates
fine and leaves one pole outside, so no dummy is ever injected there.

| Layout | Zenith | Nadir |
| --- | --- | --- |
| 5.1 | **−148 dB** | 0.00 dB |
| 7.1 | **−148 dB** | 0.00 dB |
| 7.1.4 | 0.00 dB | **−150 dB** |
| 9.1.6 | 0.00 dB | 0.00 dB |

Below the hull, energy decays as `cos²(elevation)` — at az = 66.5° it falls
−0.78 dB at el = −22.6°, −3.22 dB at −45°, −24.62 dB at −86.4°, to silence at
the nadir — and the panner steps between degenerate triplets on the way, which
is the seam.

**An attempted fix was reverted.** Measuring pole coverage after triangulation
and injecting a dummy only where a pole is genuinely uncovered does restore
energy completely: sphere-wide worst case goes from −24.66 dB to +0.0000 dB, all
four layouts reach 0.00 dB at both poles, and the seam disappears. But
`backend_conformance` then fails with a gain-vector jump of L2 1.4142 across a
0.05 step at [0, 0, −1]: the dummy's redistributed energy snaps between single
speakers instead of spreading, so the pole becomes discontinuous.

Trading a silent pole for a stepping one is not an improvement. The fix needs
**continuous redistribution at the pole**, not dummy injection alone. Both
deferrals now record this so the next attempt starts from it rather than
rediscovering it.

## Seam metric: two wrong formulations

Worth recording, because both are easy to reintroduce.

1. *Requiring `‖Δg‖` to halve when the step halves* tests differentiability. VBAP
   gains are continuous but not differentiable — at a speaker's own direction
   the gain peaks and falls away on both sides — so this flagged every speaker
   direction as a seam (ratio 0.7048 at az = −45.6°, which is the −45° speaker).
2. *Probing `‖Δg‖` across one fixed small step* misses a jump unless the jump
   lands inside that step. At 0.01° it passed on a layout with a known
   discontinuity. **A test that cannot fail is worse than no test.**

The detector now sweeps a degree per lattice point and bisects toward whatever
produced the change: a continuous function's difference collapses as the
interval shrinks, a jump keeps its magnitude. Verified in both directions —
0.001311 with the hull covered, 0.807848 without.

It has a known blind spot: it sweeps azimuth at fixed elevation and never
crosses a pole, where azimuth is degenerate. `backend_conformance` covers that,
and is what caught the regression above.

## Current gate state

| Gate | Status |
| --- | --- |
| Null: 7.1.4 / binaural / crossover | live |
| LR4 reconstruction flatness | live (+0.0018 dB) |
| ITD magnitude / antisymmetry / monotonicity | live (all three) |
| VBAP energy conservation | deferred — uncovered pole |
| VBAP seam continuity | deferred — same root cause |
| HRIR time alignment | deferred — bundled KEMAR violates the contract |
| Wide: LR4, ITD magnitude | live |
| Wide: VBAP energy with spread | deferred — MDAP spread loses 3.009 dB at spread = 0.25 |

Suite is green at roughly 10.2 s against a 7.42 s baseline. That is ~2.8 s added
against a stated budget of 2 s; the scenes are already at their documented
minimum (0.125 s), so closing the remaining gap means cutting coverage rather
than waste.

## Attempt 2 at the pole fix — also reverted

The prescribed fix was: inject a dummy where a pole is uncovered, and replace
`redistribute_dummy_in_triangle`'s triangle-local folding with a precomputed,
direction-independent weight vector per dummy, then renormalise to `Σg² = 1`.

It was implemented in full and reverted. Results:

- **Energy: solved.** Sphere-wide worst case −0.0000 dB, and every pre-existing
  pole test (`test_energy_conservation_at_pole`, `test_z_sweep_continuity`,
  `test_vertical_z_axis_no_silence`, `test_coplanar_layout_still_falls_back_to_dummy_poles`)
  stayed green.
- **Continuity: worse.** The surviving jump went from 0.807848 to 1.255847, at
  az = 71.28° el = −59.9°.

The gain vectors either side of that azimuth explain why, and they refute the
premise the fix was built on:

```
az=71.27  g=[0, 0.977, 0, 0, 0, 0, 0.211, 0, 0, 0, 0]
az=71.28  g=[0, 0,     0, 0, 0, 0, 1.000, 0, 0, 0, 0]
```

Energy sits on one or two speakers and snaps — **there is no ring spreading at
all**, so the dummy's gain is zero in this direction. The source is not being
panned through a triangle containing the dummy; it is going through
`fold_out_of_hull`, which projects out-of-hull directions onto the nearest
triangle and snaps as the nearest triangle changes.

So the dummy is *not* closing the hull below the bed ring, and no redistribution
scheme can help while that is true. Redistribution was never the discontinuity's
source; `fold_out_of_hull` is.

### The lead for attempt 3

`NativeVbapLayout::build` calls `find_ls_triplets(&effective_dirs, true)` with
`omit_large_triangles` hard-coded to `true`. A triangle joining the bed ring
(el = 0°) to a nadir dummy (el = −90°) spans 90° and is a strong candidate for
omission, which would leave the lower hemisphere outside the hull no matter how
the dummy is redistributed. Passing `false` through
`try_dirs_with_optional_dummy` does *not* test this — `build` ignores it and
re-triangulates with `true` regardless. That hard-coded argument is where the
next attempt should start.

Both VBAP deferrals remain, with this recorded so attempt 3 does not repeat
attempts 1 and 2.

### Correction: the omit_large_triangles lead is wrong

`APERTURE_LIMIT_RAD` is `π` — 180°. `omit_large_triangles` therefore discards
only faces with an edge subtending a full 180°, which essentially never happens;
a bed-ring-to-nadir edge is 90° and is kept. The flag is close to a no-op, and
the hard-coded `true` in `NativeVbapLayout::build` is **not** why the hull stays
open. Disregard that lead.

What the evidence actually leaves open: with a nadir dummy injected, the energy
sweep reports full coverage (−0.0000 dB everywhere, including the nadir), so the
hull *is* closed. Yet at az = 71.28°, el = −59.9° the gains show two real bed
speakers and no dummy contribution at all. A closed hull and a dummy-free
matched triangle at a below-hull direction are contradictory, so the next step is
to instrument the triangle search itself — print which face index and which
vertices `vbap3d` selects for that direction, with and without the dummy — rather
than to theorise about pruning again.

Both prior attempts assumed they knew why the source missed the dummy triangle.
Neither checked. That is the check to run first.

## Retracted: `reset_runtime_state` does not leak

An earlier revision of this report recorded a pre-existing defect — a −20.3 dBFS
residual said to show the previous stream surviving a reset. **That was wrong**,
and the fault was in the test, not the renderer.

It compared a renderer that had been reset against a *freshly constructed* one.
Those are not the same situation: `dsp_fixtures::scene::prepared` primes four
rounds of events, so a fresh renderer carries a ramped-up `slewed_gain` while a
reset one restarts from silence. The residual was the documented 20 ms fade-in,
which is correct behaviour.

Restated so it isolates the property that actually matters — two renderers given
*different* prior content, both reset, then fed identical blocks — the assertion
passes. `reset_runtime_state` erases the previous stream.

That also settles the open question about the channel-state refactor, which
replaced a synchronous clear under a mutex with a flag consumed at the top of the
next `render_frame`. The four call sites outside the renderer
(`orender_engine/src/engine.rs` 649/810/871,
`src/cli/decode/spatial_metadata.rs:86`) get the same guarantee as before.

Covered by `reset_runtime_state_erases_the_previous_stream`, a live gate.

# Is the out-of-hull fix standard? — researched verdict

Four independent researchers plus adversarial verification, against primary
sources and by executing reference implementations.

**Answer: the removed fade was wrong and matches no authoritative source. The
new behaviour achieves the standard's outcome by a non-standard mechanism.**

## The fade has no precedent

No authoritative source prescribes attenuating a source by how far outside the
hull it lies. Across Pulkki's Pd/Max external, its SuperCollider port, Csound,
Ardour, EAR, libspatialaudio, IEM AllRADecoder, SAF and polarch's MATLAB
library, no `cos(fold angle)` gain scalar exists. Omniphony's cos²-energy law
appears to be an invention. (Absence of evidence after targeted searching, not
proof of non-existence.)

## Full level below the hull is required

ITU-R BS.2127-1 — the ADM renderer, and the sole broadcast authority since
EBU Tech 3388 v2 withdrew the EBU's competing spec — makes full-sphere coverage
a *defining property* of a valid panner (§6.1.1): "At least one region is able
to handle any given direction." There is no out-of-hull case: a direction no
region handles is a malformed panner, not a direction to attenuate. §7.3.10
confirms the intent — out-of-range positions clamp to the boundary at full gain,
and "the sum of the squares of the loudspeaker gains will always be 1".

This was measured, not just read. The official EAR reference implementation was
run on BS.2051 System D (4+5+0) at Omniphony's exact test azimuth of 66.5°:
**0.0000 dB at elevations 0, −22.6, −45, −86.4 and −90**. A 4096-point sphere
sweep across four layouts gave zero unhandled directions and |g| = 1.000000
everywhere.

BS.2051-3 confirms this is the normal case: of ten standard layouts only two
have any bottom-layer speaker, and BS.2076-3 explicitly permits objects at
elevation −90°. An uncovered nadir is the renderer's problem to solve, not an
authoring error to punish with silence.

## The mechanism is not ours

BS.2127 §6.1.3.1 appends virtual loudspeakers unconditionally — "0,0,−1 (below
the listener) is always added" — and §6.1.2.2 redistributes the virtual gain
over the adjacent ring at 1/√n, then power-normalises. MPEG-H does the same
(Fraunhofer, DAGA 2015). Pulkki's own Pd external and Csound instead clamp
negatives and renormalise on a single argmax face — full level, but
discontinuous below the hull.

Omniphony does neither: it folds onto the nearest boundary faces blended by
`score^12`. That yields the right **level** and better **continuity** than
Pulkki, but a more localised **image** at steep downward angles than BS.2127,
which diffuses to a uniform 1/√5 across the bottom ring at nadir.

## The parity target genuinely conflicts

This module claims parity with SAF, and **SAF really does fade to silence**. Its
`saf_vbap.c` truncates dummy gains with a bare `memmove` and no renormalisation
("they have served their purpose and can now be laid to rest"). Re-executed on
Omniphony's 7.1.4 at az 66.5°, that gives −1.12 dB at −22.6°, −4.32 dB at −45°,
−26.35 dB at −86.4° and silence at nadir — *steeper* than the fade just removed,
and on ring-only layouts exactly the cos(angle) law we had.

A literal parity argument favours reverting. Three things outweigh it: SAF's own
public header promises the table is energy-normalised (`sum(gains^2) = 1`),
which the truncation violates; SAF ships no VBAP test at all; and SPARTA's
Panner — SAF's own reference application — cancels the fade at its default
`roomCoeff = 0.5`. The attenuation looks like an artefact of the truncation
rather than a designed fade, though SAF's intent is genuinely unknown.

## Status

**Level: standard-conformant — done.** **Image below the hull: not BS.2127's.**

If ADM/object conformance ever matters, implement §6.1.2.2 properly:
unconditional nadir speaker, 1/√n downmix over the adjacent ring, power
normalise — rather than folding. Two further divergences are recorded in the
code: `prepare_effective_speaker_dirs` injects pole dummies only when
triangulating the real layout *fails* (BS.2127 adds the nadir speaker always;
SAF adds them whenever no speaker is within 60° of a pole), and the MDAP spread
path folds out-of-hull members at full weight where Pulkki's discards them.

Not to be described as parity with SAF or with Pulkki.

# The ITD tests were flaky: the fixture raced its own HRIR request

After the Phase 2 retraction above put all three ITD gates live, two of them
(`itd_magnitude_tracks_the_model`, `itd_magnitude_grows_toward_the_interaural_axis`)
began failing intermittently in full-suite runs — roughly 1 run in 3 — while
passing every time in isolation. `itd_is_antisymmetric_about_the_median_plane`
was re-deferred with an `#[ignore]` pointing at this report, which never
recorded the cause. This section closes that.

## Not what it looked like

The `#[ignore]` text blamed nondeterminism "under test parallelism" and pointed
at rayon workers missing the `ensure_denormals_flushed` FTZ/DAZ guard. Other
plausible suspects were a process-global HRIR cache or a `OnceLock` mutated by
another test. All of these were wrong: **no state is shared between these tests
at all.** Each `measured_lag` call builds its own `SpatialRenderer`.

The race is inside a single renderer instance. `render_single_object_binaural`
requests `HrirSource::Synthetic`, but a source switch is asynchronous by design
(issue #153): `ensure_source` hands the request to the rebuild worker and frames
keep rendering with the **previous** grid until the new one lands. The previous
grid is the default, `HrirSource::SafKemar`. The fixture then discarded 64 prime
blocks — which is *compute*, not wall-clock, and provides no synchronisation. On
an idle machine the worker won the race; under a loaded one it did not, and the
measurement convolved KEMAR instead of the synthetic set.

KEMAR is not time-aligned and is left/right asymmetric — the property already
deferred as `hrir_providers_return_time_aligned_pairs`. Its intrinsic interaural
lag *is* the observed error, exactly:

| az | synthetic (correct) | observed when the race was lost | KEMAR intrinsic |
| --- | --- | --- | --- |
| 0° | +0.000 | +0.025 … +0.055 | asymmetric at centre |
| +30° | −12.836 | −13.460 | −1.103 |
| −90° | +32.227 | +38.325 | ≈ +6.998 (mirror of +90°) |

## Confirmed, not inferred

A `binaural_rebuild_pending()` accessor was added and the fixture instrumented
to report it at the moment of measurement. Over 25 full-suite runs the
correlation was total: **every** failing run had the rebuild still pending for at
least one azimuth, **every** passing run had zero.

Controlled comparison under 48 competing CPU hogs, same machine, same binary
otherwise:

| Code | Result |
| --- | --- |
| before | 7 / 15 runs failed (47 %) |
| after | 0 / 25 runs failed |

Unloaded, after the fix: 25 / 25 green, and the printed measurements are now
bit-identical run to run.

## The fix

`render_single_object_binaural` settles in two ordered stages: drive frames
until `binaural_rebuild_pending()` clears, and *then* discard the 64 prime
blocks. The order is the point — priming before the swap leaves the delay lines
and convolver holding KEMAR-convolved history, so the measurement would still be
a mixture of the two sets. The settle blocks draw their excitation from a
disjoint seed range, so how many of them the machine's load required cannot
reach the measured window.

Two supporting changes make that observable: the HRIR grid and the source it was
built from now travel as one `Grid` allocation, so swapping a grid in also swaps
the answer to "which source is live" (they cannot disagree), and
`SpatialRenderer::binaural_rebuild_pending` exposes it. This is also the honest
answer for any offline render, where the async swap is latency insurance nobody
needs and determinism is worth more.

`itd_is_antisymmetric_about_the_median_plane` is a live gate again — the gate
state table above was already correct, and is now true again. Note that the
tolerance was never the problem and was not touched, and the suite was not
serialised.
