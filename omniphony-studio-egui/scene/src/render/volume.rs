//! Ray-marched energy volumes (`scene/energy-volume-core.js`).
//!
//! Each provider owns one slot: an `n³` RGBA16F 3D texture (the Studio uses
//! RGBA32F; half floats keep the values already normalised on the CPU) with a
//! uniform block and a bind group per sampler kind. The unit cube is drawn
//! with front faces culled (three.js `BackSide`) and premultiplied blending.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glam::Mat4;

/// Texture format of the volume slots.
const VOLUME_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct VolumeUniforms {
    pub model: [[f32; 4]; 4],
    /// xyz = box min, w = inv_max.
    pub box_min: [f32; 4],
    /// xyz = box max, w = opacity.
    pub box_max: [f32; 4],
    /// gamma_accumulate, gamma_mip, step_norm, mix.
    pub params: [f32; 4],
    /// colormap, steps, precolored, custom_stop_count.
    pub iparams: [i32; 4],
    /// `(pos, r, g, b)`.
    pub custom_stops: [[f32; 4]; 8],
}

/// Texel data to upload: `n³` RGBA half floats, `i + n*(j + n*k)` layout
/// (i = depth/x, j = height/y, k = width/z).
pub struct VolumeData {
    pub n: u32,
    pub texels: Vec<u16>,
}

/// One volume to draw this frame.
pub struct VolumeDraw {
    /// Provider slot (stable across frames so the texture is reused).
    pub slot: usize,
    pub uniforms: VolumeUniforms,
    /// New texel data when the provider rebuilt this frame.
    pub upload: Option<Arc<VolumeData>>,
    /// Linear (true) or nearest sampling.
    pub smooth: bool,
}

struct Slot {
    n: u32,
    uniform_buf: wgpu::Buffer,
    bind_nearest: wgpu::BindGroup,
    bind_linear: wgpu::BindGroup,
    texture: wgpu::Texture,
}

pub struct VolumeRenderer {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler_nearest: wgpu::Sampler,
    sampler_linear: wgpu::Sampler,
    slots: Vec<Option<Slot>>,
    /// Draw list of the current frame, in `slots` indices.
    draws: Vec<(usize, bool)>,
}

impl VolumeRenderer {
    pub fn new(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        globals_layout: &wgpu::BindGroupLayout,
        mesh_vertex_layout: wgpu::VertexBufferLayout<'_>,
        scene_format: wgpu::TextureFormat,
        depth_format: wgpu::TextureFormat,
        msaa: u32,
    ) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("volume"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("volume"),
            bind_group_layouts: &[Some(globals_layout), Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("volume"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_volume"),
                compilation_options: Default::default(),
                buffers: &[Some(mesh_vertex_layout)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Front),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: depth_format,
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState {
                count: msaa,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_volume"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: scene_format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = |filter: wgpu::FilterMode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("volume"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: filter,
                min_filter: filter,
                mipmap_filter: wgpu::MipmapFilterMode::Nearest,
                ..Default::default()
            })
        };
        Self {
            pipeline,
            layout,
            sampler_nearest: sampler(wgpu::FilterMode::Nearest),
            sampler_linear: sampler(wgpu::FilterMode::Linear),
            slots: Vec::new(),
            draws: Vec::new(),
        }
    }

    fn ensure_slot(&mut self, device: &wgpu::Device, slot: usize, n: u32) {
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, || None);
        }
        if self.slots[slot].as_ref().is_some_and(|s| s.n == n) {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("volume"),
            size: wgpu::Extent3d {
                width: n,
                height: n,
                depth_or_array_layers: n,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: VOLUME_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("volume uniforms"),
            size: std::mem::size_of::<VolumeUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = |sampler: &wgpu::Sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("volume"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniform_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };
        let bind_nearest = bind(&self.sampler_nearest);
        let bind_linear = bind(&self.sampler_linear);
        self.slots[slot] = Some(Slot {
            n,
            uniform_buf,
            bind_nearest,
            bind_linear,
            texture,
        });
    }

    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, draws: &[VolumeDraw]) {
        self.draws.clear();
        for d in draws {
            if let Some(data) = &d.upload {
                self.ensure_slot(device, d.slot, data.n);
                let slot = self.slots[d.slot].as_ref().unwrap();
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &slot.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(&data.texels),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(data.n * 8),
                        rows_per_image: Some(data.n),
                    },
                    wgpu::Extent3d {
                        width: data.n,
                        height: data.n,
                        depth_or_array_layers: data.n,
                    },
                );
            }
            let Some(slot) = self.slots.get(d.slot).and_then(|s| s.as_ref()) else {
                continue;
            };
            queue.write_buffer(&slot.uniform_buf, 0, bytemuck::bytes_of(&d.uniforms));
            self.draws.push((d.slot, d.smooth));
        }
    }

    pub fn encode(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        cube_vertices: &wgpu::Buffer,
        cube_indices: &wgpu::Buffer,
        cube_index_count: u32,
    ) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, cube_vertices.slice(..));
        pass.set_index_buffer(cube_indices.slice(..), wgpu::IndexFormat::Uint32);
        for &(slot, smooth) in &self.draws {
            let Some(s) = self.slots.get(slot).and_then(|s| s.as_ref()) else {
                continue;
            };
            pass.set_bind_group(
                1,
                if smooth {
                    &s.bind_linear
                } else {
                    &s.bind_nearest
                },
                &[],
            );
            pass.draw_indexed(0..cube_index_count, 0, 0..1);
        }
    }
}

/// IEEE 754 half-float bits from an `f32` (round to nearest even).
pub fn f16_bits(value: f32) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let mant = bits & 0x007f_ffff;
    if exp == 0xff {
        // Inf / NaN
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x0080_0000) >> (1 - e);
        let rounded = (m + 0x0fff + ((m >> 13) & 1)) >> 13;
        return sign | rounded as u16;
    }
    let m = ((e as u32) << 23) | mant;
    let rounded = m + 0x0fff + ((m >> 13) & 1);
    sign | ((rounded >> 13) as u16)
}

pub fn model_matrix(box_min: [f32; 3], box_max: [f32; 3]) -> Mat4 {
    let size = glam::Vec3::new(
        box_max[0] - box_min[0],
        box_max[1] - box_min[1],
        box_max[2] - box_min[2],
    );
    let center = glam::Vec3::new(
        (box_max[0] + box_min[0]) * 0.5,
        (box_max[1] + box_min[1]) * 0.5,
        (box_max[2] + box_min[2]) * 0.5,
    );
    Mat4::from_scale_rotation_translation(size, glam::Quat::IDENTITY, center)
}
