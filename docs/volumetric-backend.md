# Volumetric backend: VBAP with depth

The `volumetric` render backend pans with VBAP and renders, on top of it, how
far inside the room an object sits. It exists because VBAP pans by direction
alone: an object on the front wall and the same object halfway to the
listener get the same gains, and only the distance attenuation tells them
apart. Object positions in the formats Omniphony renders are room-relative,
so that discards the one thing the stream says about an object inside the
room. The barycenter backend does render depth, by placing the power
centroid on the object over every loudspeaker, at the price of a blurrier
direction. The volumetric backend keeps VBAP's direction and adds the depth.

## The model

For an object at position `t` in the room (after the room ratios), the
backend:

1. takes VBAP's gains for the direction of `t`, the loudspeaker face the ray
   from the listener through `t` crosses;
2. measures where that ray leaves the *loudspeaker surface*: the
   triangulation VBAP pans with, lifted to the loudspeakers' real distances.
   With the surface at distance `d` along the ray and the object at `r`, the
   **depth** is `1 − r/d`: `0` on the surface and beyond it, `1` at the
   listener, linear between;
3. moves that share of the object's power from the VBAP face to a *central
   distribution*, which stands for "at the listener", with an equal-power
   crossfade renormalised to unit power.

Geometrically this is the triangulation of the loudspeaker directions with
the listener as one more vertex every face is joined to: a fan of tetrahedra.
It is consistent by construction (no Delaunay step, no arbitrary diagonal
inside the room), mirror-symmetric whenever the layout is, and free of the
sliver cells a tetrahedralisation of co-spherical loudspeakers produces. The
weight of the listener vertex is what the central distribution plays.

On the surface and beyond it the depth is `0` and the gains are VBAP's, bit
for bit: there is no seam at the hull, and the out-of-hull modes apply
unchanged outside it.

### The central distribution

`central`, a backend parameter:

| Value | What plays the central share | Character |
|---|---|---|
| `antipode` (default) | VBAP at the direction opposite the object as it is panned: the loudspeakers facing it across the listener. The opposite direction is panned as a bare direction, never clamped to the horizon, so an object overhead has its antipode under the floor. | The object stays a pair of images, the near wall and the far wall. Their power centroid is kept on the object, so they meet at equal level at the listener, not before. |
| `uniform` | Equal power over every spatialized loudspeaker | The image dissolves into the whole array as the object nears the listener. Continuous through the listener's position. |

With `antipode` the opposite direction is undefined at the listener's exact
position, where the uniform distribution takes over; an object passing
through the listener switches images there, as VBAP itself switches
direction there.

### The depth curve

`depth_curve`, a backend parameter in `[0.25, 4]`, default `1`: an exponent
on the depth. Below `1` the centre takes over sooner, above `1` the wall
holds on longer. `1` is linear in the distance along the ray.

### Virtual loudspeakers

The surface is triangulated on its own, always closed with the virtual
poles, whatever out-of-hull mode the VBAP underneath renders with: a
direction that leaves an open hull (where that VBAP folds its gains onto the
boundary) still meets the surface, so the depth stays continuous there.
Virtual loudspeakers (the poles, and the centres that close a coplanar face)
have no distance of their own. Each is placed on the plane of the real
loudspeakers it downmixes onto when those are coplanar and that plane does
not run through the listener, else at those loudspeakers' mean distance. A
7.1.4 without floor speakers thus gets a cone of a floor hanging from its
bed ring; a wall closed around a virtual centre keeps being a flat wall.

### Rounding

An object authored on a wall measures a few ulps inside it. Linear depths
below `1e-6` are taken as rounding, through a ramp rather than a step, and
before the depth curve: a curve below `1` would turn a step of that size into
a large one. On the surface the gains are therefore VBAP's exactly.

### The VBAP underneath

The VBAP the backend pans with is tuned under the backend's own id: its
spread policy and out-of-hull mode are the `volumetric` entry of the param
bag, not the `vbap` one, so a volumetric render is set up independently of a
plain VBAP one. The Studio shows both sets on the backend's parameter form.

## What it costs

Two sweeps of the surface per object (a 3×3 product per face, as VBAP's own
hit test: one ahead, one behind for the antipode's weight), one dot product
per plane, and with `antipode` a second VBAP evaluation at the opposite
direction. No allocation per request; the surface is built once per
topology. The
depth is a pure function of position, so both sampled tables hold it, the
polar one on its distance axis.

## What it does not do

Rendering an interior position with loudspeakers on the periphery means
feeding the wall facing the object: any amplitude method does, and the
images it produces are in-head or diffuse, with the comb filtering and the
small sweet spot of coherent signals from facing walls. The volumetric
backend does not escape that; it chooses how few loudspeakers take part and
how their weights move. It renders no *nearness* either: level, the
direct-to-reverberant ratio and the spectrum do that, and the distance
attenuation decorator applies on top of it as on every backend.

A loudspeaker inside the room (in front, below eye level) is a vertex of the
loudspeaker surface like any other today. Making it capture the objects
around it is the next step of this design and needs the surface to become a
volume mesh around that vertex.

## Checking it

`renderer/src/render_backend/volumetric_backend.rs` holds the model and its
tests: VBAP bit for bit on and beyond the surface, depth linear along a ray
and mirror-symmetric, gains mirror-symmetric on a layout whose faces are
unambiguous, unit power at every depth, the antipode's power centroid on the
object, continuity across a face edge, a finite floor under an open hull,
and the virtual-centre placement. Listening: select `volumetric` in the
Studio's renderer panel and move an object along a ray into the room,
against `vbap`, `barycenter` and VBAP with the distance-diffuse decorator.
