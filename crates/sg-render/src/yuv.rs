//! Y'CbCr pictures converted to RGB on the GPU. Export uses it so that a
//! 4K frame crosses to the GPU as 12 MB of planes instead of 33 MB of RGBA
//! made on the CPU first (perf log, M6).

use wgpu::util::DeviceExt;

/// Three 8-bit planes, rows tightly packed, top row first.
pub struct YuvPlanes<'a> {
    pub size: (u32, u32),
    pub chroma_size: (u32, u32),
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    /// Luma weights of red and blue (0.2126 and 0.0722 for BT.709).
    pub kr: f32,
    pub kb: f32,
    /// Full range (0 to 255) instead of video range (16 to 235).
    pub full_range: bool,
}

pub struct YuvImporter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}

const SHADER: &str = "
struct Params { r: vec4<f32>, g: vec4<f32>, b: vec4<f32> }
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var luma: texture_2d<f32>;
@group(0) @binding(2) var cb: texture_2d<f32>;
@group(0) @binding(3) var cr: texture_2d<f32>;
@group(0) @binding(4) var planes: sampler;

struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var v: V;
    v.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    v.uv = vec2<f32>(p.x, 1.0 - p.y);
    return v;
}

@fragment
fn fs(v: V) -> @location(0) vec4<f32> {
    let c = vec4<f32>(
        textureSample(luma, planes, v.uv).r,
        textureSample(cb, planes, v.uv).r,
        textureSample(cr, planes, v.uv).r,
        1.0,
    );
    return vec4<f32>(dot(params.r, c), dot(params.g, c), dot(params.b, c), 1.0);
}
";

/// Rows of the matrix taking (y, cb, cr, 1), each 0 to 1 as sampled, to R'G'B'.
fn matrix(kr: f32, kb: f32, full_range: bool) -> [[f32; 4]; 3] {
    let kg = 1.0 - kr - kb;
    let (y_scale, c_scale, y_zero) =
        if full_range { (1.0, 1.0, 0.0) } else { (255.0 / 219.0, 255.0 / 224.0, 16.0 / 255.0) };
    let c_zero = 128.0 / 255.0;
    let row = |cb: f32, cr: f32| {
        let (cb, cr) = (cb * c_scale, cr * c_scale);
        [y_scale, cb, cr, -(y_scale * y_zero + (cb + cr) * c_zero)]
    };
    [
        row(0.0, 2.0 * (1.0 - kr)),
        row(-2.0 * kb * (1.0 - kb) / kg, -2.0 * kr * (1.0 - kr) / kg),
        row(2.0 * (1.0 - kb), 0.0),
    ]
}

impl YuvImporter {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> YuvImporter {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv import"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let plane = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv import"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(48),
                    },
                    count: None,
                },
                plane(1),
                plane(2),
                plane(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv import"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv import"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        // Nearest: each chroma sample covers its four pixels, which is what
        // the CPU conversion behind the preview does. Export must match it.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuv import"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        YuvImporter { device, queue, layout, pipeline, sampler }
    }

    /// The picture as an RGBA texture layers can sample; `None` if a plane
    /// is shorter than its size says.
    pub fn import(&self, p: &YuvPlanes<'_>) -> Option<wgpu::Texture> {
        let (w, h) = p.size;
        let (cw, ch) = p.chroma_size;
        let fits = |data: &[u8], w: u32, h: u32| w > 0 && h > 0 && data.len() >= w as usize * h as usize;
        if !fits(p.y, w, h) || !fits(p.u, cw, ch) || !fits(p.v, cw, ch) {
            return None;
        }
        let texture = |label, width, height, format, usage| wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        };
        let plane = |data: &[u8], width: u32, height: u32| {
            let usage = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
            self.device
                .create_texture_with_data(
                    &self.queue,
                    &texture("yuv plane", width, height, wgpu::TextureFormat::R8Unorm, usage),
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &data[..width as usize * height as usize],
                )
                .create_view(&Default::default())
        };
        let (y, u, v) = (plane(p.y, w, h), plane(p.u, cw, ch), plane(p.v, cw, ch));
        let params = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("yuv import"),
            contents: bytemuck::cast_slice(&matrix(p.kr, p.kb, p.full_range)),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&y) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&u) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&v) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let target = self.device.create_texture(&texture("picture", w, h, wgpu::TextureFormat::Rgba8Unorm, usage));
        let view = target.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuv import"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::matrix;

    #[test]
    fn bt709_video_range_corners() {
        let m = matrix(0.2126, 0.0722, false);
        let rgb = |y: f32, cb: f32, cr: f32| {
            m.map(|row| row[0] * y / 255.0 + row[1] * cb / 255.0 + row[2] * cr / 255.0 + row[3])
        };
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);
        assert!(near(rgb(235.0, 128.0, 128.0), [1.0, 1.0, 1.0]));
        assert!(near(rgb(16.0, 128.0, 128.0), [0.0, 0.0, 0.0]));
        assert!(near(rgb(63.0, 102.0, 240.0), [1.0, 0.0, 0.0]), "{:?}", rgb(63.0, 102.0, 240.0));
        assert!(near(rgb(32.0, 240.0, 118.0), [0.0, 0.0, 1.0]), "{:?}", rgb(32.0, 240.0, 118.0));
    }
}
