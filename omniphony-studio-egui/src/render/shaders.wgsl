// Scene shaders. Lit and unlit meshes shade in linear light and encode to
// sRGB on output, because the offscreen target is Rgba8Unorm and egui's
// swapchain expects gamma-encoded values (egui writes gamma-space colours into
// the same non-sRGB target). Trail points are written raw, as the Studio's
// custom ShaderMaterial does (see the phase 1 spec, colour-space caveat).
//
// Lights follow scene/setup.js: warm key + cool fill directional lights, an
// ambient term and a sky/ground hemisphere, with three.js' physical
// convention (Lambert = albedo / pi).

struct Globals {
    view_proj: mat4x4<f32>,
    cam_pos: vec4<f32>,
    cam_right: vec4<f32>,
    cam_up: vec4<f32>,
    // xyz = direction towards the light, w = intensity
    light1: vec4<f32>,
    light1_color: vec4<f32>,
    light2: vec4<f32>,
    light2_color: vec4<f32>,
    // rgb = ambient colour * intensity, w = hemisphere intensity
    ambient: vec4<f32>,
    hemi_sky: vec4<f32>,
    hemi_ground: vec4<f32>,
    // xy = viewport size in physical pixels
    viewport: vec4<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

const PI: f32 = 3.14159265;

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
}

// --- instanced meshes (spheres, cubes, quads, cones, discs) ----------------

struct MeshIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) m0: vec4<f32>,
    @location(3) m1: vec4<f32>,
    @location(4) m2: vec4<f32>,
    @location(5) m3: vec4<f32>,
    @location(6) color: vec4<f32>,
    // rgb = emissive colour (linear); w = gloss (0 matte .. 1 glossy), or < 0 for unlit
    @location(7) emissive: vec4<f32>,
};

struct MeshOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec3<f32>,
    @location(3) emissive: vec4<f32>,
};

@vertex
fn vs_mesh(in: MeshIn) -> MeshOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    let world = model * vec4<f32>(in.pos, 1.0);
    var out: MeshOut;
    out.clip = globals.view_proj * world;
    out.normal = normalize((model * vec4<f32>(in.normal, 0.0)).xyz);
    out.color = in.color;
    out.world = world.xyz;
    out.emissive = in.emissive;
    return out;
}

fn shade(n_in: vec3<f32>, world: vec3<f32>, albedo: vec3<f32>, gloss: f32) -> vec3<f32> {
    let v = normalize(globals.cam_pos.xyz - world);
    // Double-sided: flip normals facing away.
    var n = n_in;
    if (dot(n, v) < 0.0) { n = -n; }
    var lit = albedo * globals.ambient.rgb / PI;
    let hemi_t = 0.5 + 0.5 * n.y;
    lit += albedo * mix(globals.hemi_ground.rgb, globals.hemi_sky.rgb, hemi_t) * globals.ambient.w / PI;
    let shininess = mix(8.0, 96.0, gloss);
    let spec_strength = mix(0.08, 0.7, gloss);
    let l1 = normalize(globals.light1.xyz);
    let d1 = max(dot(n, l1), 0.0);
    lit += albedo * globals.light1_color.rgb * globals.light1.w * d1 / PI;
    let h1 = normalize(l1 + v);
    lit += globals.light1_color.rgb * globals.light1.w * pow(max(dot(n, h1), 0.0), shininess) * spec_strength * 0.25;
    let l2 = normalize(globals.light2.xyz);
    let d2 = max(dot(n, l2), 0.0);
    lit += albedo * globals.light2_color.rgb * globals.light2.w * d2 / PI;
    let h2 = normalize(l2 + v);
    lit += globals.light2_color.rgb * globals.light2.w * pow(max(dot(n, h2), 0.0), shininess) * spec_strength * 0.12;
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.08 * gloss;
    return lit + vec3<f32>(rim);
}

@fragment
fn fs_mesh(in: MeshOut) -> @location(0) vec4<f32> {
    if (in.emissive.w < 0.0) {
        return vec4<f32>(to_srgb(in.color.rgb), in.color.a);
    }
    let lit = shade(normalize(in.normal), in.world, in.color.rgb, in.emissive.w) + in.emissive.rgb;
    return vec4<f32>(to_srgb(lit), in.color.a);
}

// --- lines -----------------------------------------------------------------

struct LineIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(in: LineIn) -> LineOut {
    var out: LineOut;
    out.clip = globals.view_proj * vec4<f32>(in.pos, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    return vec4<f32>(to_srgb(in.color.rgb), in.color.a);
}

// --- billboard sprites (halos, discs), world-sized ---------------------------
// params.x selects the alpha profile:
//   0 = diffuse halo (radial gradient of sources.js createHaloTexture)
//   1 = soft disc, params.y = edge softness

struct SpriteIn {
    @builtin(vertex_index) vi: u32,
    @location(0) center: vec3<f32>,
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
    @location(3) params: vec4<f32>,
};

struct SpriteOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec4<f32>,
};

fn quad_corner(vi: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-0.5, -0.5), vec2<f32>(0.5, -0.5), vec2<f32>(0.5, 0.5),
        vec2<f32>(-0.5, -0.5), vec2<f32>(0.5, 0.5), vec2<f32>(-0.5, 0.5),
    );
    return corners[vi];
}

@vertex
fn vs_sprite(in: SpriteIn) -> SpriteOut {
    let c = quad_corner(in.vi);
    let world = in.center + (globals.cam_right.xyz * c.x + globals.cam_up.xyz * c.y) * in.size;
    var out: SpriteOut;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.uv = c + vec2<f32>(0.5, 0.5);
    out.color = in.color;
    out.params = in.params;
    return out;
}

fn halo_alpha(r: f32) -> f32 {
    // Stops: 0.0→1.0, 0.12→0.96, 0.34→0.54, 0.64→0.14, 0.86→0.03, 1.0→0.0
    if (r < 0.12) { return mix(1.0, 0.96, r / 0.12); }
    if (r < 0.34) { return mix(0.96, 0.54, (r - 0.12) / 0.22); }
    if (r < 0.64) { return mix(0.54, 0.14, (r - 0.34) / 0.30); }
    if (r < 0.86) { return mix(0.14, 0.03, (r - 0.64) / 0.22); }
    if (r < 1.0) { return mix(0.03, 0.0, (r - 0.86) / 0.14); }
    return 0.0;
}

@fragment
fn fs_sprite(in: SpriteOut) -> @location(0) vec4<f32> {
    let r = length(in.uv * 2.0 - vec2<f32>(1.0, 1.0));
    var a: f32;
    if (in.params.x < 0.5) {
        a = halo_alpha(r);
    } else {
        let soft = max(in.params.y, 0.001);
        a = 1.0 - smoothstep(1.0 - soft, 1.0, r);
    }
    return vec4<f32>(to_srgb(in.color.rgb), in.color.a * a);
}

// --- trail points, pixel-sized (trails.js diffuse mode) ---------------------

struct PointIn {
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec3<f32>,
    @location(1) size: f32,
    @location(2) color: vec4<f32>,
};

struct PointOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_point(in: PointIn) -> PointOut {
    let c = quad_corner(in.vi);
    var clip = globals.view_proj * vec4<f32>(in.pos, 1.0);
    // gl_PointSize = clamp(size * 110 / depth, 0.4, 44) pixels
    let depth = max(0.1, clip.w);
    let px = clamp(in.size * (110.0 / depth), 0.4, 44.0);
    let offset = c * px * 2.0 / globals.viewport.xy;
    clip = vec4<f32>(clip.xy + offset * clip.w, clip.zw);
    var out: PointOut;
    out.clip = clip;
    out.uv = c + vec2<f32>(0.5, 0.5);
    out.color = in.color;
    return out;
}

@fragment
fn fs_point(in: PointOut) -> @location(0) vec4<f32> {
    let centered = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
    let radius = length(centered);
    let mask = 1.0 - smoothstep(0.25, 1.0, radius);
    let alpha = mask * in.color.a;
    if (alpha <= 0.001) { discard; }
    // Written raw: the Studio's point shader skips the sRGB OETF.
    return vec4<f32>(in.color.rgb, alpha);
}

// --- composite into egui's pass --------------------------------------------

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_samp: sampler;

struct BlitOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> BlitOut {
    var out: BlitOut;
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi >> 1u) * 4 - 1);
    out.clip = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, 1.0 - (y + 1.0) * 0.5);
    return out;
}

@fragment
fn fs_blit(in: BlitOut) -> @location(0) vec4<f32> {
    return textureSample(scene_tex, scene_samp, in.uv);
}
