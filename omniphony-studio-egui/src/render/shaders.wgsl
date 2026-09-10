// Scene shaders for the egui spike. Lighting happens in linear light; the
// fragment shaders encode to sRGB because the offscreen target is Rgba8Unorm
// and egui's swapchain expects gamma-encoded values (egui itself writes
// gamma-space colours into the same non-sRGB target).

struct Globals {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    cam_pos: vec4<f32>,
};
@group(0) @binding(0) var<uniform> globals: Globals;

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    return pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
}

// --- instanced spheres -----------------------------------------------------

struct SphereIn {
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) center: vec3<f32>,
    @location(3) radius: f32,
    @location(4) color: vec4<f32>,
};

struct SphereOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) world: vec3<f32>,
};

@vertex
fn vs_sphere(in: SphereIn) -> SphereOut {
    var out: SphereOut;
    let world = in.center + in.pos * in.radius;
    out.clip = globals.view_proj * vec4<f32>(world, 1.0);
    out.normal = in.normal;
    out.color = in.color;
    out.world = world;
    return out;
}

@fragment
fn fs_sphere(in: SphereOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(globals.light_dir.xyz);
    let v = normalize(globals.cam_pos.xyz - in.world);
    let diffuse = max(dot(n, l), 0.0);
    let h = normalize(l + v);
    let specular = pow(max(dot(n, h), 0.0), 40.0) * 0.3;
    let rim = pow(1.0 - max(dot(n, v), 0.0), 3.0) * 0.12;
    let lit = in.color.rgb * (0.25 + 0.75 * diffuse) + vec3<f32>(specular + rim);
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

// --- composite into egui's pass --------------------------------------------
// One full-screen triangle; the viewport set by egui clips it to the widget.

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
