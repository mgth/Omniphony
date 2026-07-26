# Phantom extraction methods

The phantom-extraction pre-stage (`orender_engine/src/phantom_extract.rs`) pulls
localized ("phantom") sources out of a channel bed and re-emits them as discrete
objects, subtracting them from the source channels. It runs before the
bed→height object generators, so whatever those generators receive is the
residual bed. Two methods are selectable with the **Per-band extraction** switch
(`method` param) in Studio's phantom row.

## Broadband (default, `method = 0`)

Time-domain, per-sample Wiener/pan-ratio estimation on channel pairs and joint
arcs (front L-C-R, back pair, side arcs in 7.1, then ring/wide pairs per the
**Passes** setting). Each extracted phantom is one object, dynamically panned
between its two source speakers by the correlated-energy balance.

- Zero latency.
- One pan estimate per pair: simultaneous sources at different positions inside
  the same pair merge into an averaged position (or gate the extraction off via
  the coherence term).
- **Relocalize center / sides** collapse an arc into a single relocated object
  instead of two phantoms.

## Per-band (spectral, `method = 1`)

Frequency-domain (`orender_engine/src/phantom_spectral.rs`): the positionable
bed channels go through an STFT (1024/512, sine windows — exact WOLA identity);
each bin is encoded to a virtual horizontal B-format whose smoothed active
intensity yields a **direction of arrival** and a **directness** `1 − ψ` per
bin. The direct part of each bin is routed (softly, between the two adjacent
sectors) to one of eight fixed azimuth sector objects (`Direct_F`, `Direct_FR`,
…, `Direct_FL`) and subtracted from every bed channel in the frequency domain.
Sector object positions follow the smoothed energy-weighted mean DOA of their
content; **Lift** sets their height, **Extraction** caps the per-bin depth (the
residual never drops below `1 − strength`).

- Simultaneous sources at different positions **and different frequency bands**
  are extracted independently — the case the broadband method cannot resolve.
- Direction is estimated globally from all channels (full 360°), not per
  adjacent pair.
- **Latency: one FFT frame (1024 samples ≈ 21 ms at 48 kHz) on the whole bed**
  while the method is active. The STFT channels, the bypass channels (LFE,
  unpositioned — plain delay line) and the sector objects stay mutually
  aligned. This is audio-late and within typical lipsync tolerance, but it is
  not compensated anywhere.
- **Passes** and the **Relocalize** switches have no effect in this mode.

### 3D inputs — the `heights` switch

When the input already carries height channels (5.1.4, 7.1.4…), the broadband
method — and the spectral method with **Extract heights** off — leave them
untouched on the bypass delay. With **Extract heights** on (the default,
spectral only), the tops join the same analysis in 3D:

- Every positionable channel is encoded by its full **3D unit direction**
  (bed on the floor plane, tops at the ceiling corners), adding an up-facing
  dipole `U` to the virtual B-format. The per-bin DOA gains an **elevation**
  and the directness uses the 3D intensity norm.
- Four **high sector objects** (`DirectH_FR`, `DirectH_BR`, `DirectH_BL`,
  `DirectH_FL` — 90° each, centred on the ceiling corners) extend the eight
  floor sectors. A bin's direct part splits between the two rings by its
  elevation, normalised by the **Height split** param (degrees; default 35°
  ≈ the corner-top channel elevation, which then reads fully high); the
  azimuth soft-assignment applies within each ring as before. Intensity
  elevations are compressed (a 50/50 floor↔ceiling pan reads ≈ half the
  geometric elevation), so Height split is the ear-tuning knob: **lower
  values push extracted content toward the ceiling**. Live, no re-plan.
- **Inter-plane phantoms come out for free**: content panned between a floor
  channel and a top (say L↔Tfl) reads an intermediate elevation, so it lands
  partly in `Direct_FL` and partly in `DirectH_FL` at the same azimuth — and
  both sectors' dynamic positions (now energy-weighted **3D** means) sit at
  the true intermediate height. No second pass, no extra latency.
- Content mixed *within* the top plane (Tfl↔Tfr pans, top ambience) is
  extracted intra-plane into the high ring the same way the bed is into the
  floor ring; the residual tops keep playing at their channel positions.
- Zenith-heavy content (a lone `Tc`) pulls its sector position toward the room
  centre overhead instead of a wall.
- Bed-only inputs are **bit-identical** to the planar analysis (the up dipole
  is exactly zero), so the switch is a strict extension.

### How the per-band analysis works (walkthrough)

The method never reasons about *channels* — it reconstructs the **sound
field**: "standing at the centre of the room while the bed speakers play,
where does the sound come from?" `Direct_F` means *the front direction*, not
the centre channel. Step by step, once per hop (512 samples):

1. **Three virtual microphones.** Every positionable bed channel has a known
   canonical azimuth (C at 0°, L at −30°, the surrounds at their side/back
   corners…), so three imaginary coincident microphones at the centre can be
   synthesized as plain weighted sums of the channel spectra:
   `W` (omni) = Σ spectra; `F` (front-facing figure-8) = Σ spectra·cos(az);
   `R` (right-facing figure-8) = Σ spectra·sin(az). No extra FFTs — the
   dipoles are linear combinations of the per-channel spectra.
2. **Per frequency bin** (smoothed over ~80 ms so estimates don't jitter),
   the three signals combine into the **active intensity vector** — the
   direction the energy of *that band* flows through the centre:
   `i_front = Re(W*·F)`, `i_right = Re(W*·R)`.
   - Direction of arrival: `az = atan2(i_right, i_front)` (0° = front).
   - Diffuseness `ψ`: when the intensity magnitude matches the bin energy,
     the band arrives from one clear direction (`ψ ≈ 0`); when the channels
     cancel each other (decorrelated ambience), the vector shrinks
     (`ψ ≈ 1`). The extraction depth is `d = strength · (1 − ψ)`.
3. **Routing.** The fraction `d` of the bin — taken from `W`, the omni mix —
   is credited to the sector object covering its DOA: eight 45° slices,
   `Direct_F` at ±22.5° around straight ahead, `Direct_FR` around 45°, and so
   on. Assignment is soft between the two adjacent sectors (a source at 20°
   splits F/FR proportionally — no click when it crosses a boundary). The
   remaining `1 − d` of the bin stays in the original bed channels.
4. **A sector object's audio** is the inverse FFT of everything credited to
   its slice this frame: all bands, from all channels jointly, whose
   direction points its way.
5. **A sector object's position** is the energy-weighted mean DOA of those
   same bands (smoothed ~80 ms), projected on the room perimeter, `z` set by
   **Lift**. An empty sector falls back to its static slice centre. The
   object therefore *moves inside its 45° slice*, tracking where its current
   content actually comes from.

Worked example — a film mix:

- Dialogue in C: its bands arrive from 0° exactly → credited to `Direct_F`,
  which hovers at front-centre, right next to the bed's C object. **Seeing
  both a static `C` and a moving `Direct_F` is the expected picture**: the
  bed channel keeps the residual `1 − d` (extraction never removes a channel,
  it drains its directional energy — bounded by **Extraction**), while the
  sector object carries the extracted direct part. Raise **Extraction**
  toward 1 and the bed C fades in favour of `Direct_F`; lower it for the
  opposite.
- A stereo phantom centre (dialogue panned equally L/R) *also* arrives from
  ≈0° → same `Direct_F`. That is the point of the feature: the phantom image
  *between* two speakers becomes a real object at its perceived position.
- A guitar panned L-to-C points at −15…−20° → mostly `Direct_F`, pulling its
  mean DOA leftward: the "wandering F".
- Meanwhile a bright effect in the surrounds has its *own* bins pointing
  backwards → `Direct_B`, simultaneously and independently — the case the
  broadband method averages away.

### Model limits

- The diffuseness split is exact only for direction-balanced content.
  Independent (uncorrelated) channels whose direction vectors do not sum to
  zero — e.g. front-heavy ambience in L+R+C — read partly "direct" toward the
  set's mean direction and are partially extracted there.
- Two sources overlapping in the same time-frequency bin share a single DOA
  (the usual W-disjointness assumption); they get placed between their true
  positions. Spectrally disjoint sources are the case this method wins on.

## Design notes — why not "the broadband principle, per band"?

A natural question: the broadband method's *shape* (pairwise Wiener between
adjacent speakers, one phantom panned by the level ratio) could have been kept
and simply run **per frequency band** — a classic pan-frequency upmix
(Avendano/Jot style). It was considered and rejected; the per-band method
deliberately switches to DirAC's global-field formulation instead:

- **Pairwise doesn't compose.** Pairs and arcs overlap on shared speakers
  (C, the surround corners), so the broadband code needs an explicit claiming
  order; per band, that ordering problem multiplies by the number of bins.
  Sources spanning three or more speakers still confuse every pairwise
  estimate. The intensity vector reads **all channels jointly** and gives one
  consistent 360° answer per band, for any channel count, with no ordering.
- **Object count.** Per-band pairwise either keeps one moving object per pair
  (re-importing the averaging problem inside each pair, just narrower) or
  needs several objects per pair to keep simultaneous sources apart —
  an unbounded plan. The field formulation lands on a **fixed set of eight
  sector objects** regardless of input layout: stable renderer channel slots,
  no allocation, realtime-friendly.
- **Cost.** Pairwise-per-band needs its own spectral estimate per pair;
  the field analysis reuses the *same* per-channel FFTs for everything —
  the three virtual microphones are linear combinations, not extra
  transforms.
- **Diffuseness comes for free.** The same intensity vector that gives the
  DOA gives `ψ`, the principled direct/diffuse split — exactly what the
  downstream height generators want (the `dirac` backend uses the same
  virtual B-format model, so the two stages agree on what "diffuse" means).
- **Why keep broadband at all:** zero latency and lower cost. When one
  dominant source moves between two speakers, the per-pair pan ratio tracks
  it just as well — the default stays broadband; per-band is the opt-in for
  complex, layered mixes.

The one thing the pairwise shape did that the field method gives up is the
per-pair *placement between exact speaker positions*; the sector DOA mean
recovers it in practice (step 5 above) without being tied to pairs.

## Interaction with the object generators

Both methods run before the bed→height generators, which therefore see the
direct-removed residual bed:

- **DirAC** (`dirac`): recommended combination with the per-band method — the
  direct sound leaves the bed as localized sector objects, and the generator's
  diffuseness estimate on the residual is cleaner, so the ceiling layer carries
  ambience only. It shares the same virtual B-format model.
- **PAD** / **copy_up**: also operate on the residual; with strong extraction
  there is simply less correlated content left to hold down (PAD) or copy up.

The stage only runs on channel-based content; streams that already carry
objects are untouched. On 3D channel inputs the generators stay no-op (the
content already has height), but the spectral extraction still applies — see
the `heights` switch above.
