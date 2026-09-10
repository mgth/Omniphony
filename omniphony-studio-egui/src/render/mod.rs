//! wgpu scene renderer hosted by an egui paint callback.
//!
//! Pattern (the same one Rerun uses): egui's own render pass has no depth
//! attachment, so the scene is rendered in `prepare` into an offscreen
//! MSAA colour + depth target sized to the widget in physical pixels, resolved
//! into a sampled texture, and composited in `paint` with a full-screen
//! triangle inside egui's pass. No CPU copies anywhere; the device and queue
//! are egui's.
//!
//! Draw order inside the scene pass follows three.js: depth-tested lines,
//! opaque meshes, then every blended element sorted by render order and
//! distance (far to near), then pixel-sized trail points, then overlay lines
//! (billboard rings) with depth testing off.

pub mod camera;

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

const MSAA_SAMPLES: u32 = 4;
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    cam_pos: [f32; 4],
    cam_right: [f32; 4],
    cam_up: [f32; 4],
    light1: [f32; 4],
    light1_color: [f32; 4],
    light2: [f32; 4],
    light2_color: [f32; 4],
    ambient: [f32; 4],
    hemi_sky: [f32; 4],
    hemi_ground: [f32; 4],
    viewport: [f32; 4],
}

/// One instanced mesh: full model matrix, linear RGBA, emissive + gloss.
/// `emissive[3] < 0` draws unlit (three.js `MeshBasicMaterial`).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct MeshInstance {
    pub model: [[f32; 4]; 4],
    pub color: [f32; 4],
    pub emissive: [f32; 4],
}

impl MeshInstance {
    pub const UNLIT: f32 = -1.0;

    pub fn new(model: Mat4, color: [f32; 4], emissive: [f32; 4]) -> Self {
        Self {
            model: model.to_cols_array_2d(),
            color,
            emissive,
        }
    }

    pub fn unlit(model: Mat4, color: [f32; 4]) -> Self {
        Self::new(model, color, [0.0, 0.0, 0.0, Self::UNLIT])
    }

    pub fn translation(&self) -> Vec3 {
        Vec3::new(self.model[3][0], self.model[3][1], self.model[3][2])
    }
}

/// Unit meshes available to instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MeshKind {
    /// Radius 1.
    Sphere,
    /// `[-0.5, 0.5]³`.
    Cube,
    /// `[-0.5, 0.5]²` in XY, normal +Z (three.js `PlaneGeometry(1, 1)`).
    Quad,
    /// Radius 1, height 1 centred, apex +Y (three.js `ConeGeometry(1, 1)`).
    Cone,
    /// Radius 1 in XY, normal +Z (three.js `CircleGeometry(1)`).
    Disc,
}

impl MeshKind {
    pub const ALL: [MeshKind; 5] = [
        MeshKind::Sphere,
        MeshKind::Cube,
        MeshKind::Quad,
        MeshKind::Cone,
        MeshKind::Disc,
    ];
}

/// A mesh to draw this frame. `blend == false` draws in the opaque pass with
/// depth writes; blended items are sorted by `order` then far to near.
#[derive(Clone, Copy, Debug)]
pub struct MeshItem {
    pub kind: MeshKind,
    pub instance: MeshInstance,
    pub blend: bool,
    pub depth_test: bool,
    /// three.js `renderOrder`.
    pub order: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeshVertex {
    pos: [f32; 3],
    normal: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct LineVertex {
    pub pos: [f32; 3],
    /// Linear RGBA.
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SpriteInstance {
    pub center: [f32; 3],
    /// Quad side in scene units.
    pub size: f32,
    /// Linear RGBA.
    pub color: [f32; 4],
    /// x = profile (0 halo, 1 disc), y = disc edge softness.
    pub params: [f32; 4],
}

impl SpriteInstance {
    pub const HALO: f32 = 0.0;
    pub const DISC: f32 = 1.0;
}

/// Pixel-sized point sprite (trail point).
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct PointInstance {
    pub pos: [f32; 3],
    /// `size` attribute of trails.js (pixels at the reference depth).
    pub size: f32,
    /// Linear RGB + alpha, written raw.
    pub color: [f32; 4],
}

const MESH_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4
];
const LINE_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
const SPRITE_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x4, 3 => Float32x4];
const POINT_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32, 2 => Float32x4];

/// Scene lighting in the three.js frame (x depth, y up, z right). Defaults
/// are `scene/setup.js`: key `#fff7ea` 2.35 from `(3.6, 4.8, 1.4)`, fill
/// `#b8d4ff` 1.05 from `(-2.8, 1.1, -3.8)`, ambient `#ffffff` 0.24,
/// hemisphere sky `#dcecff` / ground `#0d0f14` 0.12.
#[derive(Clone, Copy, Debug)]
pub struct Lighting {
    pub key_dir: Vec3,
    pub key_color: [f32; 3],
    pub key_intensity: f32,
    pub fill_dir: Vec3,
    pub fill_color: [f32; 3],
    pub fill_intensity: f32,
    pub ambient: [f32; 3],
    pub ambient_intensity: f32,
    pub hemi_sky: [f32; 3],
    pub hemi_ground: [f32; 3],
    pub hemi_intensity: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            key_dir: Vec3::new(3.6, 4.8, 1.4).normalize(),
            key_color: linear_rgb(0xff, 0xf7, 0xea),
            key_intensity: 2.35,
            fill_dir: Vec3::new(-2.8, 1.1, -3.8).normalize(),
            fill_color: linear_rgb(0xb8, 0xd4, 0xff),
            fill_intensity: 1.05,
            ambient: [1.0, 1.0, 1.0],
            ambient_intensity: 0.24,
            hemi_sky: linear_rgb(0xdc, 0xec, 0xff),
            hemi_ground: linear_rgb(0x0d, 0x0f, 0x14),
            hemi_intensity: 0.12,
        }
    }
}

/// Everything the callback needs for one frame. Built on the UI thread,
/// consumed on the render thread through an `Arc`.
pub struct FrameData {
    pub meshes: Vec<MeshItem>,
    /// Depth-tested line list.
    pub lines: Vec<LineVertex>,
    /// Line list drawn last with depth testing off.
    pub overlay_lines: Vec<LineVertex>,
    pub sprites_alpha: Vec<SpriteInstance>,
    pub sprites_additive: Vec<SpriteInstance>,
    pub points: Vec<PointInstance>,
    pub view_proj: Mat4,
    pub cam_pos: Vec3,
    pub cam_right: Vec3,
    pub cam_up: Vec3,
    pub lighting: Lighting,
    /// Widget size in physical pixels.
    pub size_px: [u32; 2],
    /// Linear RGBA clear colour.
    pub clear: [f64; 4],
}

impl FrameData {
    pub fn new(
        view_proj: Mat4,
        cam_pos: Vec3,
        cam_right: Vec3,
        cam_up: Vec3,
        size_px: [u32; 2],
    ) -> Self {
        Self {
            meshes: Vec::with_capacity(256),
            lines: Vec::with_capacity(512),
            overlay_lines: Vec::with_capacity(16 * 1024),
            sprites_alpha: Vec::new(),
            sprites_additive: Vec::with_capacity(64),
            points: Vec::with_capacity(4096),
            view_proj,
            cam_pos,
            cam_right,
            cam_up,
            lighting: Lighting::default(),
            size_px,
            clear: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

pub struct ViewportCallback(pub Arc<FrameData>);

struct Targets {
    size: [u32; 2],
    msaa_view: wgpu::TextureView,
    resolve_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    blit_bind_group: wgpu::BindGroup,
}

struct GrowBuffer {
    buf: wgpu::Buffer,
    capacity_bytes: usize,
    label: &'static str,
}

impl GrowBuffer {
    fn new(device: &wgpu::Device, label: &'static str, capacity_bytes: usize) -> Self {
        Self {
            buf: vertex_buffer(device, label, capacity_bytes),
            capacity_bytes,
            label,
        }
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) {
        if bytes.len() > self.capacity_bytes {
            self.capacity_bytes = bytes.len().next_power_of_two();
            self.buf = vertex_buffer(device, self.label, self.capacity_bytes);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.buf, 0, bytes);
        }
    }
}

struct UnitMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

/// A run of instances sharing a mesh kind and pipeline, as laid out in the
/// instance buffer.
#[derive(Clone, Copy)]
struct Run {
    kind: MeshKind,
    first: u32,
    count: u32,
    blend: bool,
    depth_test: bool,
}

pub struct SceneRenderer {
    mesh_opaque: wgpu::RenderPipeline,
    mesh_blend_depth: wgpu::RenderPipeline,
    mesh_blend_nodepth: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    line_overlay: wgpu::RenderPipeline,
    sprite_alpha: wgpu::RenderPipeline,
    sprite_additive: wgpu::RenderPipeline,
    point_pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    unit_meshes: Vec<(MeshKind, UnitMesh)>,
    instances: GrowBuffer,
    lines: GrowBuffer,
    sprites: GrowBuffer,
    points: GrowBuffer,
    runs: Vec<Run>,
    line_count: u32,
    overlay_first: u32,
    overlay_count: u32,
    sprite_alpha_count: u32,
    sprite_additive_first: u32,
    sprite_additive_count: u32,
    point_count: u32,
    targets: Option<Targets>,
}

impl SceneRenderer {
    pub fn new(device: &wgpu::Device, egui_target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("studio scene shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        let depth_state = |write: bool, test: bool| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(if test {
                wgpu::CompareFunction::Less
            } else {
                wgpu::CompareFunction::Always
            }),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let msaa = wgpu::MultisampleState {
            count: MSAA_SAMPLES,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };
        let alpha_target = [Some(wgpu::ColorTargetState {
            format: SCENE_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        // three.js AdditiveBlending: src alpha, dst one.
        let additive_target = [Some(wgpu::ColorTargetState {
            format: SCENE_FORMAT,
            blend: Some(wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::SrcAlpha,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            }),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let mesh_buffers = [
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MeshVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &MESH_ATTRS,
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<MeshInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &INSTANCE_ATTRS,
            }),
        ];
        let line_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<LineVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &LINE_ATTRS,
        })];
        let sprite_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<SpriteInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &SPRITE_ATTRS,
        })];
        let point_buffers = [Some(wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<PointInstance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &POINT_ATTRS,
        })];

        let make = |label: &str,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout>],
                    topology: wgpu::PrimitiveTopology,
                    depth: wgpu::DepthStencilState,
                    targets: &[Option<wgpu::ColorTargetState>]| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&scene_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    topology,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: Some(depth),
                multisample: msaa,
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets,
                }),
                multiview_mask: None,
                cache: None,
            })
        };

        let tri = wgpu::PrimitiveTopology::TriangleList;
        let lines_topo = wgpu::PrimitiveTopology::LineList;
        let mesh_opaque = make(
            "mesh opaque",
            "vs_mesh",
            "fs_mesh",
            &mesh_buffers,
            tri,
            depth_state(true, true),
            &alpha_target,
        );
        let mesh_blend_depth = make(
            "mesh blend",
            "vs_mesh",
            "fs_mesh",
            &mesh_buffers,
            tri,
            depth_state(false, true),
            &alpha_target,
        );
        let mesh_blend_nodepth = make(
            "mesh blend nodepth",
            "vs_mesh",
            "fs_mesh",
            &mesh_buffers,
            tri,
            depth_state(false, false),
            &alpha_target,
        );
        let line_pipeline = make(
            "lines",
            "vs_line",
            "fs_line",
            &line_buffers,
            lines_topo,
            depth_state(false, true),
            &alpha_target,
        );
        let line_overlay = make(
            "lines overlay",
            "vs_line",
            "fs_line",
            &line_buffers,
            lines_topo,
            depth_state(false, false),
            &alpha_target,
        );
        let sprite_alpha = make(
            "sprites alpha",
            "vs_sprite",
            "fs_sprite",
            &sprite_buffers,
            tri,
            depth_state(false, false),
            &alpha_target,
        );
        let sprite_additive = make(
            "sprites additive",
            "vs_sprite",
            "fs_sprite",
            &sprite_buffers,
            tri,
            depth_state(false, false),
            &additive_target,
        );
        let point_pipeline = make(
            "trail points",
            "vs_point",
            "fs_point",
            &point_buffers,
            tri,
            depth_state(false, false),
            &alpha_target,
        );

        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_blit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: tri,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blit"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: egui_target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let unit_meshes = MeshKind::ALL
            .iter()
            .map(|&kind| {
                let (vertices, indices) = match kind {
                    MeshKind::Sphere => unit_sphere(24, 24),
                    MeshKind::Cube => unit_cube(),
                    MeshKind::Quad => unit_quad(),
                    MeshKind::Cone => unit_cone(14),
                    MeshKind::Disc => unit_disc(24),
                };
                (
                    kind,
                    UnitMesh {
                        vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("unit mesh vertices"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        }),
                        indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("unit mesh indices"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        }),
                        index_count: indices.len() as u32,
                    },
                )
            })
            .collect();

        Self {
            mesh_opaque,
            mesh_blend_depth,
            mesh_blend_nodepth,
            line_pipeline,
            line_overlay,
            sprite_alpha,
            sprite_additive,
            point_pipeline,
            blit_pipeline,
            globals_buf,
            globals_bind_group,
            blit_layout,
            sampler,
            unit_meshes,
            instances: GrowBuffer::new(
                device,
                "instances",
                512 * std::mem::size_of::<MeshInstance>(),
            ),
            lines: GrowBuffer::new(device, "lines", 16384 * std::mem::size_of::<LineVertex>()),
            sprites: GrowBuffer::new(
                device,
                "sprites",
                512 * std::mem::size_of::<SpriteInstance>(),
            ),
            points: GrowBuffer::new(
                device,
                "points",
                8192 * std::mem::size_of::<PointInstance>(),
            ),
            runs: Vec::new(),
            line_count: 0,
            overlay_first: 0,
            overlay_count: 0,
            sprite_alpha_count: 0,
            sprite_additive_first: 0,
            sprite_additive_count: 0,
            point_count: 0,
            targets: None,
        }
    }

    fn ensure_targets(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        let size = [size[0].max(1), size[1].max(1)];
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let tex = |label: &str, samples: u32, format: wgpu::TextureFormat, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let msaa = tex(
            "scene msaa",
            MSAA_SAMPLES,
            SCENE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let resolve = tex(
            "scene resolve",
            1,
            SCENE_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = tex(
            "scene depth",
            MSAA_SAMPLES,
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let resolve_view = resolve.create_view(&Default::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resolve_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.targets = Some(Targets {
            size,
            msaa_view: msaa.create_view(&Default::default()),
            resolve_view,
            depth_view: depth.create_view(&Default::default()),
            blit_bind_group,
        });
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &FrameData) {
        let l = &frame.lighting;
        let globals = Globals {
            view_proj: frame.view_proj.to_cols_array_2d(),
            cam_pos: frame.cam_pos.extend(1.0).to_array(),
            cam_right: frame.cam_right.extend(0.0).to_array(),
            cam_up: frame.cam_up.extend(0.0).to_array(),
            light1: l.key_dir.extend(l.key_intensity).to_array(),
            light1_color: [l.key_color[0], l.key_color[1], l.key_color[2], 1.0],
            light2: l.fill_dir.extend(l.fill_intensity).to_array(),
            light2_color: [l.fill_color[0], l.fill_color[1], l.fill_color[2], 1.0],
            ambient: [
                l.ambient[0] * l.ambient_intensity,
                l.ambient[1] * l.ambient_intensity,
                l.ambient[2] * l.ambient_intensity,
                l.hemi_intensity,
            ],
            hemi_sky: [l.hemi_sky[0], l.hemi_sky[1], l.hemi_sky[2], 1.0],
            hemi_ground: [l.hemi_ground[0], l.hemi_ground[1], l.hemi_ground[2], 1.0],
            viewport: [frame.size_px[0] as f32, frame.size_px[1] as f32, 0.0, 0.0],
        };
        queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));

        // Opaque first, grouped by kind; then blended sorted by render order
        // and distance, split into runs of equal kind and depth-test flag.
        let mut ordered: Vec<MeshInstance> = Vec::with_capacity(frame.meshes.len());
        self.runs.clear();
        for &kind in &MeshKind::ALL {
            let first = ordered.len() as u32;
            ordered.extend(
                frame
                    .meshes
                    .iter()
                    .filter(|m| !m.blend && m.kind == kind)
                    .map(|m| m.instance),
            );
            let count = ordered.len() as u32 - first;
            if count > 0 {
                self.runs.push(Run {
                    kind,
                    first,
                    count,
                    blend: false,
                    depth_test: true,
                });
            }
        }
        let cam = frame.cam_pos;
        let mut blended: Vec<(&MeshItem, f32)> = frame
            .meshes
            .iter()
            .filter(|m| m.blend)
            .map(|m| (m, (m.instance.translation() - cam).length_squared()))
            .collect();
        blended.sort_by(|a, b| a.0.order.cmp(&b.0.order).then_with(|| b.1.total_cmp(&a.1)));
        for (item, _) in blended {
            let extend = self
                .runs
                .last()
                .is_some_and(|r| r.blend && r.kind == item.kind && r.depth_test == item.depth_test);
            if extend {
                self.runs.last_mut().unwrap().count += 1;
            } else {
                self.runs.push(Run {
                    kind: item.kind,
                    first: ordered.len() as u32,
                    count: 1,
                    blend: true,
                    depth_test: item.depth_test,
                });
            }
            ordered.push(item.instance);
        }
        self.instances
            .upload(device, queue, bytemuck::cast_slice(&ordered));

        let mut line_bytes: Vec<u8> = Vec::with_capacity(
            (frame.lines.len() + frame.overlay_lines.len()) * std::mem::size_of::<LineVertex>(),
        );
        line_bytes.extend_from_slice(bytemuck::cast_slice(&frame.lines));
        line_bytes.extend_from_slice(bytemuck::cast_slice(&frame.overlay_lines));
        self.lines.upload(device, queue, &line_bytes);
        self.line_count = frame.lines.len() as u32;
        self.overlay_first = frame.lines.len() as u32;
        self.overlay_count = frame.overlay_lines.len() as u32;

        let mut sprite_bytes: Vec<u8> = Vec::with_capacity(
            (frame.sprites_alpha.len() + frame.sprites_additive.len())
                * std::mem::size_of::<SpriteInstance>(),
        );
        sprite_bytes.extend_from_slice(bytemuck::cast_slice(&frame.sprites_alpha));
        sprite_bytes.extend_from_slice(bytemuck::cast_slice(&frame.sprites_additive));
        self.sprites.upload(device, queue, &sprite_bytes);
        self.sprite_alpha_count = frame.sprites_alpha.len() as u32;
        self.sprite_additive_first = frame.sprites_alpha.len() as u32;
        self.sprite_additive_count = frame.sprites_additive.len() as u32;

        self.points
            .upload(device, queue, bytemuck::cast_slice(&frame.points));
        self.point_count = frame.points.len() as u32;
    }

    fn unit_mesh(&self, kind: MeshKind) -> &UnitMesh {
        &self
            .unit_meshes
            .iter()
            .find(|(k, _)| *k == kind)
            .expect("unit mesh")
            .1
    }

    fn encode_scene(&self, encoder: &mut wgpu::CommandEncoder, frame: &FrameData) {
        let Some(t) = &self.targets else { return };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &t.msaa_view,
                depth_slice: None,
                resolve_target: Some(&t.resolve_view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: frame.clear[0],
                        g: frame.clear[1],
                        b: frame.clear[2],
                        a: frame.clear[3],
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &t.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.globals_bind_group, &[]);

        if self.line_count > 0 {
            pass.set_pipeline(&self.line_pipeline);
            pass.set_vertex_buffer(0, self.lines.buf.slice(..));
            pass.draw(0..self.line_count, 0..1);
        }

        pass.set_vertex_buffer(1, self.instances.buf.slice(..));
        for run in &self.runs {
            let pipeline = match (run.blend, run.depth_test) {
                (false, _) => &self.mesh_opaque,
                (true, true) => &self.mesh_blend_depth,
                (true, false) => &self.mesh_blend_nodepth,
            };
            let mesh = self.unit_mesh(run.kind);
            pass.set_pipeline(pipeline);
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, run.first..run.first + run.count);
        }

        if self.sprite_alpha_count > 0 {
            pass.set_pipeline(&self.sprite_alpha);
            pass.set_vertex_buffer(0, self.sprites.buf.slice(..));
            pass.draw(0..6, 0..self.sprite_alpha_count);
        }
        if self.sprite_additive_count > 0 {
            pass.set_pipeline(&self.sprite_additive);
            pass.set_vertex_buffer(0, self.sprites.buf.slice(..));
            pass.draw(
                0..6,
                self.sprite_additive_first..self.sprite_additive_first + self.sprite_additive_count,
            );
        }
        if self.point_count > 0 {
            pass.set_pipeline(&self.point_pipeline);
            pass.set_vertex_buffer(0, self.points.buf.slice(..));
            pass.draw(0..6, 0..self.point_count);
        }
        if self.overlay_count > 0 {
            pass.set_pipeline(&self.line_overlay);
            pass.set_vertex_buffer(0, self.lines.buf.slice(..));
            pass.draw(
                self.overlay_first..self.overlay_first + self.overlay_count,
                0..1,
            );
        }
    }
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(renderer) = resources.get_mut::<SceneRenderer>() else {
            return Vec::new();
        };
        let frame = &*self.0;
        renderer.ensure_targets(device, frame.size_px);
        renderer.upload(device, queue, frame);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("studio scene"),
        });
        renderer.encode_scene(&mut encoder, frame);
        vec![encoder.finish()]
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(renderer) = resources.get::<SceneRenderer>() else {
            return;
        };
        let Some(t) = &renderer.targets else { return };
        let vp = info.viewport_in_pixels();
        if vp.width_px <= 0 || vp.height_px <= 0 {
            return;
        }
        pass.set_viewport(
            vp.left_px as f32,
            vp.top_px as f32,
            vp.width_px as f32,
            vp.height_px as f32,
            0.0,
            1.0,
        );
        pass.set_pipeline(&renderer.blit_pipeline);
        pass.set_bind_group(0, &t.blit_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn vertex_buffer(device: &wgpu::Device, label: &str, bytes: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(16) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

// ---------------------------------------------------------------------------
// Unit meshes
// ---------------------------------------------------------------------------

fn unit_sphere(stacks: u32, slices: u32) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(((stacks + 1) * (slices + 1)) as usize);
    for i in 0..=stacks {
        let phi = i as f32 / stacks as f32 * std::f32::consts::PI;
        let (sp, cp) = phi.sin_cos();
        for j in 0..=slices {
            let theta = j as f32 / slices as f32 * std::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let p = [sp * ct, cp, sp * st];
            vertices.push(MeshVertex { pos: p, normal: p });
        }
    }
    let mut indices = Vec::with_capacity((stacks * slices * 6) as usize);
    for i in 0..stacks {
        for j in 0..slices {
            let a = i * (slices + 1) + j;
            let b = a + slices + 1;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    (vertices, indices)
}

fn unit_cube() -> (Vec<MeshVertex>, Vec<u32>) {
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [-1.0, 0.0, 0.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
    ];
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (n, u, v) in faces {
        let n = Vec3::from_array(n);
        let u = Vec3::from_array(u);
        let v = Vec3::from_array(v);
        let base = vertices.len() as u32;
        for (su, sv) in [(-0.5, -0.5), (0.5, -0.5), (0.5, 0.5), (-0.5, 0.5)] {
            let p = n * 0.5 + u * su + v * sv;
            vertices.push(MeshVertex {
                pos: p.to_array(),
                normal: n.to_array(),
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

fn unit_quad() -> (Vec<MeshVertex>, Vec<u32>) {
    let n = [0.0, 0.0, 1.0];
    let vertices = vec![
        MeshVertex {
            pos: [-0.5, -0.5, 0.0],
            normal: n,
        },
        MeshVertex {
            pos: [0.5, -0.5, 0.0],
            normal: n,
        },
        MeshVertex {
            pos: [0.5, 0.5, 0.0],
            normal: n,
        },
        MeshVertex {
            pos: [-0.5, 0.5, 0.0],
            normal: n,
        },
    ];
    (vertices, vec![0, 1, 2, 0, 2, 3])
}

fn unit_cone(segments: u32) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // Side: apex at +0.5, base ring at -0.5.
    for i in 0..segments {
        let a0 = i as f32 / segments as f32 * std::f32::consts::TAU;
        let a1 = (i + 1) as f32 / segments as f32 * std::f32::consts::TAU;
        let (s0, c0) = a0.sin_cos();
        let (s1, c1) = a1.sin_cos();
        let am = (a0 + a1) * 0.5;
        let n = Vec3::new(am.cos(), 0.5, am.sin()).normalize().to_array();
        let base = vertices.len() as u32;
        vertices.push(MeshVertex {
            pos: [0.0, 0.5, 0.0],
            normal: n,
        });
        vertices.push(MeshVertex {
            pos: [c0, -0.5, s0],
            normal: n,
        });
        vertices.push(MeshVertex {
            pos: [c1, -0.5, s1],
            normal: n,
        });
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    // Base cap.
    let center = vertices.len() as u32;
    vertices.push(MeshVertex {
        pos: [0.0, -0.5, 0.0],
        normal: [0.0, -1.0, 0.0],
    });
    for i in 0..segments {
        let a = i as f32 / segments as f32 * std::f32::consts::TAU;
        vertices.push(MeshVertex {
            pos: [a.cos(), -0.5, a.sin()],
            normal: [0.0, -1.0, 0.0],
        });
    }
    for i in 0..segments {
        let a = center + 1 + i;
        let b = center + 1 + (i + 1) % segments;
        indices.extend_from_slice(&[center, b, a]);
    }
    (vertices, indices)
}

fn unit_disc(segments: u32) -> (Vec<MeshVertex>, Vec<u32>) {
    let n = [0.0, 0.0, 1.0];
    let mut vertices = vec![MeshVertex {
        pos: [0.0, 0.0, 0.0],
        normal: n,
    }];
    for i in 0..segments {
        let a = i as f32 / segments as f32 * std::f32::consts::TAU;
        vertices.push(MeshVertex {
            pos: [a.cos(), a.sin(), 0.0],
            normal: n,
        });
    }
    let mut indices = Vec::with_capacity((segments * 3) as usize);
    for i in 0..segments {
        indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % segments]);
    }
    (vertices, indices)
}

// ---------------------------------------------------------------------------
// Colour helpers
// ---------------------------------------------------------------------------

/// sRGB byte colour to linear RGB.
pub fn linear_rgb(r: u8, g: u8, b: u8) -> [f32; 3] {
    let f = |c: u8| (c as f32 / 255.0).powf(2.2);
    [f(r), f(g), f(b)]
}

/// `#rrggbb` (sRGB) to linear RGB.
pub fn hex_linear(hex: u32) -> [f32; 3] {
    linear_rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

/// Linear interpolation of linear RGB (three.js `Color.lerp` interpolates the
/// stored linear components).
pub fn lerp_rgb(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

pub fn scale_rgb(c: [f32; 3], k: f32) -> [f32; 3] {
    [c[0] * k, c[1] * k, c[2] * k]
}

pub fn with_alpha(c: [f32; 3], a: f32) -> [f32; 4] {
    [c[0], c[1], c[2], a]
}
