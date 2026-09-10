//! wgpu scene renderer hosted by an egui paint callback.
//!
//! Pattern (the same one Rerun uses): egui's own render pass has no depth
//! attachment, so the scene is rendered in `prepare` into an offscreen
//! MSAA colour + depth target sized to the widget in physical pixels, resolved
//! into a sampled texture, and composited in `paint` with a full-screen
//! triangle inside egui's pass. No CPU copies anywhere; the device and queue
//! are egui's.

pub mod camera;

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

const MSAA_SAMPLES: u32 = 4;
const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const INITIAL_INSTANCES: usize = 256;
const INITIAL_LINE_VERTICES: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Globals {
    view_proj: [[f32; 4]; 4],
    light_dir: [f32; 4],
    cam_pos: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct SphereInstance {
    pub center: [f32; 3],
    pub radius: f32,
    /// Linear RGBA.
    pub color: [f32; 4],
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

const MESH_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const INSTANCE_ATTRS: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![2 => Float32x3, 3 => Float32, 4 => Float32x4];
const LINE_ATTRS: [wgpu::VertexAttribute; 2] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];

/// Everything the callback needs for one frame. Built on the UI thread,
/// consumed on the render thread through an `Arc`.
pub struct FrameData {
    pub instances: Vec<SphereInstance>,
    pub lines: Vec<LineVertex>,
    pub view_proj: Mat4,
    pub cam_pos: Vec3,
    pub light_dir: Vec3,
    /// Widget size in physical pixels.
    pub size_px: [u32; 2],
    /// Linear RGBA clear colour.
    pub clear: [f64; 4],
}

pub struct ViewportCallback(pub Arc<FrameData>);

struct Targets {
    size: [u32; 2],
    msaa_view: wgpu::TextureView,
    resolve_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    blit_bind_group: wgpu::BindGroup,
}

pub struct SceneRenderer {
    sphere_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    blit_pipeline: wgpu::RenderPipeline,
    globals_buf: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    mesh_vertices: wgpu::Buffer,
    mesh_indices: wgpu::Buffer,
    mesh_index_count: u32,
    instance_buf: wgpu::Buffer,
    instance_capacity: usize,
    line_buf: wgpu::Buffer,
    line_capacity: usize,
    targets: Option<Targets>,
}

impl SceneRenderer {
    pub fn new(device: &wgpu::Device, egui_target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("studio-spike scene shaders"),
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

        let depth_state = |write: bool| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let msaa = wgpu::MultisampleState {
            count: MSAA_SAMPLES,
            mask: !0,
            alpha_to_coverage_enabled: false,
        };
        let scene_target = [Some(wgpu::ColorTargetState {
            format: SCENE_FORMAT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];

        let sphere_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spheres"),
            layout: Some(&scene_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_sphere"),
                compilation_options: Default::default(),
                buffers: &[
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<MeshVertex>() as u64,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &MESH_ATTRS,
                    }),
                    Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<SphereInstance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &INSTANCE_ATTRS,
                    }),
                ],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(depth_state(true)),
            multisample: msaa,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_sphere"),
                compilation_options: Default::default(),
                targets: &scene_target,
            }),
            multiview_mask: None,
            cache: None,
        });

        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lines"),
            layout: Some(&scene_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &LINE_ATTRS,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            depth_stencil: Some(depth_state(false)),
            multisample: msaa,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                compilation_options: Default::default(),
                targets: &scene_target,
            }),
            multiview_mask: None,
            cache: None,
        });

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
                topology: wgpu::PrimitiveTopology::TriangleList,
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

        let (vertices, indices) = unit_sphere(20, 32);
        let mesh_vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sphere vertices"),
            contents: bytemuck::cast_slice(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let mesh_indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sphere indices"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let instance_buf = vertex_buffer(
            device,
            "instances",
            INITIAL_INSTANCES * std::mem::size_of::<SphereInstance>(),
        );
        let line_buf = vertex_buffer(
            device,
            "lines",
            INITIAL_LINE_VERTICES * std::mem::size_of::<LineVertex>(),
        );

        Self {
            sphere_pipeline,
            line_pipeline,
            blit_pipeline,
            globals_buf,
            globals_bind_group,
            blit_layout,
            sampler,
            mesh_vertices,
            mesh_indices,
            mesh_index_count: indices.len() as u32,
            instance_buf,
            instance_capacity: INITIAL_INSTANCES,
            line_buf,
            line_capacity: INITIAL_LINE_VERTICES,
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
        let msaa = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene msaa"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
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
        let globals = Globals {
            view_proj: frame.view_proj.to_cols_array_2d(),
            light_dir: frame.light_dir.extend(0.0).to_array(),
            cam_pos: frame.cam_pos.extend(1.0).to_array(),
        };
        queue.write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));

        if frame.instances.len() > self.instance_capacity {
            self.instance_capacity = frame.instances.len().next_power_of_two();
            self.instance_buf = vertex_buffer(
                device,
                "instances",
                self.instance_capacity * std::mem::size_of::<SphereInstance>(),
            );
        }
        if !frame.instances.is_empty() {
            queue.write_buffer(
                &self.instance_buf,
                0,
                bytemuck::cast_slice(&frame.instances),
            );
        }
        if frame.lines.len() > self.line_capacity {
            self.line_capacity = frame.lines.len().next_power_of_two();
            self.line_buf = vertex_buffer(
                device,
                "lines",
                self.line_capacity * std::mem::size_of::<LineVertex>(),
            );
        }
        if !frame.lines.is_empty() {
            queue.write_buffer(&self.line_buf, 0, bytemuck::cast_slice(&frame.lines));
        }
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
        if !frame.lines.is_empty() {
            pass.set_pipeline(&self.line_pipeline);
            pass.set_vertex_buffer(0, self.line_buf.slice(..));
            pass.draw(0..frame.lines.len() as u32, 0..1);
        }
        if !frame.instances.is_empty() {
            pass.set_pipeline(&self.sphere_pipeline);
            pass.set_vertex_buffer(0, self.mesh_vertices.slice(..));
            pass.set_vertex_buffer(1, self.instance_buf.slice(..));
            pass.set_index_buffer(self.mesh_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.mesh_index_count, 0, 0..frame.instances.len() as u32);
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
            label: Some("studio-spike scene"),
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
        size: bytes as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// UV sphere of radius 1 (normals = positions).
fn unit_sphere(stacks: u32, slices: u32) -> (Vec<MeshVertex>, Vec<u32>) {
    let mut vertices = Vec::with_capacity(((stacks + 1) * (slices + 1)) as usize);
    for i in 0..=stacks {
        let v = i as f32 / stacks as f32;
        let phi = v * std::f32::consts::PI;
        let (sp, cp) = phi.sin_cos();
        for j in 0..=slices {
            let u = j as f32 / slices as f32;
            let theta = u * std::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let p = [sp * ct, sp * st, cp];
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

/// sRGB byte colour to linear RGBA.
pub fn linear_rgba(r: u8, g: u8, b: u8, a: f32) -> [f32; 4] {
    let f = |c: u8| (c as f32 / 255.0).powf(2.2);
    [f(r), f(g), f(b), a]
}

/// HSV (h in degrees) to linear RGBA, for per-object hues.
pub fn hsv_linear(h: f32, s: f32, v: f32, a: f32) -> [f32; 4] {
    let h = h.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    let (r, g, b) = match i as i32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r.powf(2.2), g.powf(2.2), b.powf(2.2), a]
}
