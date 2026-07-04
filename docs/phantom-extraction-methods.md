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

### Model limits

- The diffuseness split is exact only for direction-balanced content.
  Independent (uncorrelated) channels whose direction vectors do not sum to
  zero — e.g. front-heavy ambience in L+R+C — read partly "direct" toward the
  set's mean direction and are partially extracted there.
- Two sources overlapping in the same time-frequency bin share a single DOA
  (the usual W-disjointness assumption); they get placed between their true
  positions. Spectrally disjoint sources are the case this method wins on.

## Interaction with the object generators

Both methods run before the bed→height generators, which therefore see the
direct-removed residual bed:

- **DirAC** (`dirac`): recommended combination with the per-band method — the
  direct sound leaves the bed as localized sector objects, and the generator's
  diffuseness estimate on the residual is cleaner, so the ceiling layer carries
  ambience only. It shares the same virtual B-format model.
- **PAD** / **copy_up**: also operate on the residual; with strong extraction
  there is simply less correlated content left to hold down (PAD) or copy up.

The stage only runs on bed-only content; streams that already carry objects are
untouched.
