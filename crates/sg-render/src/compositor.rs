//! GPU compositing on wgpu.

use sg_core::{Grade, Shift};
use wgpu::util::DeviceExt;

/// One picture to draw: fitted into the frame, turned and faded.
/// How a picture moves while the shutter of one frame is open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    /// Displacement when the shutter opens and when it closes. Equal: no blur.
    pub from: Shift,
    pub to: Shift,
    /// The picture repeats mirrored beyond its edges, so that moving it
    /// uncovers no black (whip, zoom). Otherwise it moves as a whole (slide).
    pub mirror: bool,
}

/// One picture to draw.
#[derive(Clone, Copy)]
pub struct LayerDraw<'a> {
    pub view: &'a wgpu::TextureView,
    /// Stored size of the picture in pixels.
    pub size: (u32, u32),
    /// Degrees the stored picture is turned (counter-clockwise) for display:
    /// the rotation recorded in the file.
    pub rotation: i32,
    pub opacity: f32,
    /// Centre relative to the frame centre, in frame widths and heights (y down).
    pub x: f32,
    pub y: f32,
    /// 1 fits the picture into the frame.
    pub scale: f32,
    /// The user's rotation, degrees clockwise.
    pub turn_degrees: f32,
    /// Fractions cut off at left, top, right, bottom of the displayed picture.
    pub crop: [f32; 4],
    /// Draw at one texture pixel per frame pixel instead of fitting (titles).
    pub natural_size: bool,
    /// Colour treatment (match and look) of the picture, if it has one.
    pub grade: Option<&'a Grade>,
    /// A number that differs from frame to frame, for the grain.
    pub seed: f32,
    /// Movement during this frame (a transition), if any.
    pub motion: Option<Motion>,
}

impl<'a> LayerDraw<'a> {
    /// The picture fitted into the frame, centred, opaque.
    pub fn fitted(view: &'a wgpu::TextureView, size: (u32, u32), rotation: i32) -> LayerDraw<'a> {
        LayerDraw {
            view,
            size,
            rotation,
            opacity: 1.0,
            x: 0.0,
            y: 0.0,
            scale: 1.0,
            turn_degrees: 0.0,
            crop: [0.0; 4],
            natural_size: false,
            motion: None,
            grade: None,
            seed: 0.0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    /// 2x2 matrix from quad corners (-1..1) to clip space.
    matrix: [f32; 4],
    /// Clip-space centre (xy) and opacity (z).
    offset: [f32; 4],
    /// 2x2 matrix from quad corners to texture coordinate offsets.
    uv: [f32; 4],
    uv_centre: [f32; 4],
    /// Content shift in quad units (xy) and size factor (z) at shutter open and close.
    motion0: [f32; 4],
    motion1: [f32; 4],
    /// Quad repeat count (x) and samples along the movement (y).
    extra: [f32; 4],
    /// Match: rgb gain, contrast; then pivot, saturation, module count, seed.
    grade0: [f32; 4],
    grade1: [f32; 4],
    /// Look modules: (kind, mix, 0, 0) and the four knobs, eight times.
    modules: [[f32; 4]; 16],
}

/// Layers per frame the uniform buffer has room for.
const MAX_LAYERS: usize = 64;
/// Samples along the movement of a blurred layer: at 4 frames of whip
/// across 1.5 frame widths the fastest frame travels about 0.9 widths,
/// 40 pixels between samples at 1080p; the shader shifts the samples from
/// pixel to pixel so that the steps do not line up into ghost edges.
const MOTION_TAPS: f32 = 48.0;
/// Uniform offsets must be multiples of this on every backend.
const UNIFORM_STRIDE: u64 = 512;

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_pipeline: wgpu::RenderPipeline,
    output_pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// Linear-light working target.
    linear: wgpu::Texture,
    linear_view: wgpu::TextureView,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    size: (u32, u32),
    /// Scale 1 fills the frame instead of fitting into it.
    cover: bool,
    luma_pipeline: wgpu::RenderPipeline,
    chroma_pipeline: wgpu::RenderPipeline,
    luma10_pipeline: wgpu::RenderPipeline,
    chroma10_pipeline: wgpu::RenderPipeline,
    /// Targets of the NV12 conversion, made on first use.
    delivery: Option<Delivery>,
}

fn make_targets(device: &wgpu::Device, width: u32, height: u32) -> (wgpu::Texture, wgpu::Texture) {
    let make = |label, format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let attach = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
    (
        make("composite linear", wgpu::TextureFormat::Rgba16Float, attach),
        make("composite output", wgpu::TextureFormat::Rgba8Unorm, attach | wgpu::TextureUsages::COPY_SRC),
    )
}

impl Compositor {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, width: u32, height: u32) -> Compositor {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
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
            label: Some("composite"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |label, vs, fs, format, blend, topology| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some(vs),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState { topology, ..Default::default() },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let layer_pipeline = pipeline(
            "composite layer",
            "layer_vs",
            "layer_fs",
            wgpu::TextureFormat::Rgba16Float,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            wgpu::PrimitiveTopology::TriangleStrip,
        );
        let output_pipeline = pipeline(
            "composite output",
            "output_vs",
            "output_fs",
            wgpu::TextureFormat::Rgba8Unorm,
            None,
            wgpu::PrimitiveTopology::TriangleList,
        );
        let luma_pipeline = pipeline(
            "delivery luma",
            "output_vs",
            "luma_fs",
            wgpu::TextureFormat::R8Unorm,
            None,
            wgpu::PrimitiveTopology::TriangleList,
        );
        let chroma_pipeline = pipeline(
            "delivery chroma",
            "output_vs",
            "chroma_fs",
            wgpu::TextureFormat::Rg8Unorm,
            None,
            wgpu::PrimitiveTopology::TriangleList,
        );
        let luma10_pipeline = pipeline(
            "delivery luma 10",
            "output_vs",
            "luma10_fs",
            wgpu::TextureFormat::Rg8Unorm,
            None,
            wgpu::PrimitiveTopology::TriangleList,
        );
        let chroma10_pipeline = pipeline(
            "delivery chroma 10",
            "output_vs",
            "chroma10_fs",
            wgpu::TextureFormat::Rgba8Unorm,
            None,
            wgpu::PrimitiveTopology::TriangleList,
        );
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("composite layers"),
            contents: &vec![0u8; (UNIFORM_STRIDE as usize) * MAX_LAYERS],
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let (linear, output) = make_targets(&device, width.max(1), height.max(1));
        let (linear_view, output_view) =
            (linear.create_view(&Default::default()), output.create_view(&Default::default()));
        Compositor {
            device,
            queue,
            layer_pipeline,
            output_pipeline,
            layout,
            sampler,
            uniforms,
            linear,
            linear_view,
            output,
            output_view,
            size: (width.max(1), height.max(1)),
            cover: false,
            luma_pipeline,
            chroma_pipeline,
            luma10_pipeline,
            chroma10_pipeline,
            delivery: None,
        }
    }

    /// With `cover`, a picture at scale 1 fills the frame and loses what
    /// does not fit, instead of fitting inside it with bars.
    pub fn set_cover(&mut self, cover: bool) {
        self.cover = cover;
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Changes the frame size. The output view changes with it.
    pub fn resize(&mut self, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if size != self.size {
            let (linear, output) = make_targets(&self.device, size.0, size.1);
            self.linear_view = linear.create_view(&Default::default());
            self.output_view = output.create_view(&Default::default());
            self.linear = linear;
            self.output = output;
            self.size = size;
        }
    }

    /// The finished frame, display-encoded 8-bit RGBA.
    pub fn output_view(&self) -> &wgpu::TextureView {
        &self.output_view
    }

    fn bind(&self, view: &wgpu::TextureView) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniforms,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<LayerUniform>() as u64),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    /// Composites `layers` bottom first over black and encodes the result
    /// into the output texture.
    pub fn render(&mut self, layers: &[LayerDraw<'_>]) {
        let (fw, fh) = (self.size.0 as f32, self.size.1 as f32);
        let layers = &layers[..layers.len().min(MAX_LAYERS)];
        for (i, layer) in layers.iter().enumerate() {
            let turns = layer.rotation.rem_euclid(360) / 90;
            let (w, h) = (layer.size.0 as f32, layer.size.1 as f32);
            // Size as displayed after the file's own rotation.
            let (dw, dh) = if turns % 2 == 1 { (h, w) } else { (w, h) };
            // Rows of the matrix from displayed to stored texture directions:
            // the inverse of turning the picture by `turns` quarter turns.
            let turn = match turns {
                1 => [0.0, -1.0, 1.0, 0.0],
                2 => [-1.0, 0.0, 0.0, -1.0],
                3 => [0.0, 1.0, -1.0, 0.0],
                _ => [1.0, 0.0, 0.0, 1.0],
            };
            let [l, t, r, b] = layer.crop.map(|c| c.clamp(0.0, 0.49));
            // Scale 1 fits the uncropped picture into the frame (text keeps its pixel size).
            let fit = match (layer.natural_size, self.cover) {
                (true, _) => 1.0,
                (false, false) => (fw / dw).min(fh / dh),
                // Vertical export of landscape footage: fill the frame, cut the sides.
                (false, true) => (fw / dw).max(fh / dh),
            };
            // A picture that moves as a whole simply sits elsewhere this frame.
            let whole = layer.motion.filter(|m| !m.mirror).map_or(Shift::REST, |m| m.from);
            let s = fit * layer.scale.max(0.0) * whole.zoom;
            let (hx, hy) = (dw * (1.0 - l - r) * s / 2.0, dh * (1.0 - t - b) * s / 2.0);
            // Clockwise on screen is a negative angle with y up.
            let (sin, cos) = (-layer.turn_degrees).to_radians().sin_cos();
            // Cropping moves the middle of what is left; that offset turns with the picture.
            let (sx, sy) = ((l - r) / 2.0 * dw * s, -(t - b) / 2.0 * dh * s);
            let (cx, cy) =
                ((layer.x + whole.x) * fw + cos * sx - sin * sy, -(layer.y + whole.y) * fh + sin * sx + cos * sy);
            // Movement inside the quad: where the content lay, in quad units
            // (y up), and how far the quad must reach to cover the frame.
            let (motion0, motion1, extra) = match layer.motion.filter(|m| m.mirror) {
                Some(m) => {
                    let state = |shift: Shift| {
                        [-shift.x * fw / hx.max(1.0), shift.y * fh / hy.max(1.0), shift.zoom.max(0.01), 0.0]
                    };
                    let reach = ((fw / 2.0 + cx.abs()) / hx.max(1.0))
                        .max((fh / 2.0 + cy.abs()) / hy.max(1.0))
                        .ceil()
                        .clamp(1.0, 64.0);
                    let taps = if m.from == m.to { 1.0 } else { MOTION_TAPS };
                    (state(m.from), state(m.to), [reach, taps, 0.0, 0.0])
                }
                None => ([0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 1.0, 0.0], [1.0, 1.0, 0.0, 0.0]),
            };
            let (hex, hey) = ((1.0 - l - r) / 2.0, (1.0 - t - b) / 2.0);
            let (du, dv) = ((l - r) / 2.0, (t - b) / 2.0);
            let (mut grade0, mut grade1, mut modules) =
                ([1.0, 1.0, 1.0, 1.0], [0.18, 1.0, 0.0, layer.seed], [[0.0f32; 4]; 16]);
            if let Some(grade) = layer.grade {
                let c = &grade.correction;
                grade0 = [c.gain[0], c.gain[1], c.gain[2], c.contrast];
                grade1 = [c.pivot, c.saturation, grade.look.len().min(8) as f32, layer.seed];
                for (slot, module) in grade.look.iter().take(8).enumerate() {
                    modules[slot * 2] = [module.kind as u8 as f32, module.mix, 0.0, 0.0];
                    modules[slot * 2 + 1] = module.knobs;
                }
            }
            let uniform = LayerUniform {
                matrix: [cos * hx / (fw / 2.0), -sin * hy / (fw / 2.0), sin * hx / (fh / 2.0), cos * hy / (fh / 2.0)],
                offset: [cx / (fw / 2.0), cy / (fh / 2.0), layer.opacity.clamp(0.0, 1.0), 0.0],
                uv: [turn[0] * hex, turn[1] * hey, turn[2] * hex, turn[3] * hey],
                uv_centre: [0.5 + turn[0] * du + turn[1] * dv, 0.5 + turn[2] * du + turn[3] * dv, 0.0, 0.0],
                motion0,
                motion1,
                extra,
                grade0,
                grade1,
                modules,
            };
            self.queue.write_buffer(&self.uniforms, i as u64 * UNIFORM_STRIDE, bytemuck::bytes_of(&uniform));
        }
        let groups: Vec<wgpu::BindGroup> = layers.iter().map(|l| self.bind(l.view)).collect();
        let output_group = self.bind(&self.linear_view);
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let attachment = |view| wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite layers"),
                color_attachments: &[Some(attachment(&self.linear_view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.layer_pipeline);
            for (i, group) in groups.iter().enumerate() {
                pass.set_bind_group(0, group, &[i as u32 * UNIFORM_STRIDE as u32]);
                pass.draw(0..4, 0..1);
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite output"),
                color_attachments: &[Some(attachment(&self.output_view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.output_pipeline);
            pass.set_bind_group(0, &output_group, &[0]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Reads the output back as tightly packed RGBA rows. Blocks; for
    /// export and tests, not for the preview path.
    pub fn read_output(&self) -> Result<Vec<u8>, wgpu::PollError> {
        let (width, height) = self.size;
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite readback"),
            size: row as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            self.output.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            self.output.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        if let Ok(data) = buffer.slice(..).get_mapped_range() {
            for line in data.chunks_exact(row as usize) {
                pixels.extend_from_slice(&line[..(width * 4) as usize]);
            }
        }
        Ok(pixels)
    }

    /// Converts the last rendered frame to NV12 on the GPU (8-bit,
    /// limited-range BT.709, as encoders expect) and reads it back: the luma
    /// plane followed by the interleaved chroma plane, tightly packed. Width
    /// and height must be even. Blocks; for export.
    pub fn read_output_nv12(&mut self) -> Result<Vec<u8>, wgpu::PollError> {
        self.read_delivery(false)
    }

    /// As [`read_output_nv12`](Self::read_output_nv12) in 10 bits: P010,
    /// two bytes per sample (little-endian, the value in the upper ten
    /// bits). The composite is floating point, so a graded 8-bit source
    /// gets real in-between values here instead of banding.
    pub fn read_output_p010(&mut self) -> Result<Vec<u8>, wgpu::PollError> {
        self.read_delivery(true)
    }

    fn read_delivery(&mut self, ten_bit: bool) -> Result<Vec<u8>, wgpu::PollError> {
        let (width, height) = self.size;
        let (cw, ch) = (width / 2, height / 2);
        // Bytes per luma sample and per chroma pair. The 10-bit planes are
        // rendered as the bytes of P010 themselves: two 8-bit channels per
        // sample, low byte first.
        let (luma_bytes, chroma_bytes) = if ten_bit { (2, 4) } else { (1, 2) };
        if self.delivery.as_ref().is_none_or(|d| d.size != self.size || d.ten_bit != ten_bit) {
            let plane = |label, format, w, h| {
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            };
            let (luma_format, chroma_format) = if ten_bit {
                (wgpu::TextureFormat::Rg8Unorm, wgpu::TextureFormat::Rgba8Unorm)
            } else {
                (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm)
            };
            let luma = plane("delivery luma", luma_format, width, height);
            let chroma = plane("delivery chroma", chroma_format, cw, ch);
            let rows = |bytes: u32| bytes.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("delivery readback"),
                size: rows(width * luma_bytes) as u64 * height as u64 + rows(cw * chroma_bytes) as u64 * ch as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.delivery = Some(Delivery {
                size: self.size,
                ten_bit,
                luma_view: luma.create_view(&Default::default()),
                chroma_view: chroma.create_view(&Default::default()),
                luma,
                chroma,
                buffer,
            });
        }
        let group = self.bind(&self.linear_view);
        let Some(delivery) = &self.delivery else { return Ok(Vec::new()) };
        let (luma_row, chroma_row) = (
            (width * luma_bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
            (cw * chroma_bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
        );
        let chroma_offset = luma_row as u64 * height as u64;
        let pipelines = if ten_bit {
            (&self.luma10_pipeline, &self.chroma10_pipeline)
        } else {
            (&self.luma_pipeline, &self.chroma_pipeline)
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (view, pipeline) in [(&delivery.luma_view, pipelines.0), (&delivery.chroma_view, pipelines.1)] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("delivery"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
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
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &group, &[0]);
            pass.draw(0..3, 0..1);
        }
        for (texture, offset, row) in [(&delivery.luma, 0, luma_row), (&delivery.chroma, chroma_offset, chroma_row)] {
            encoder.copy_texture_to_buffer(
                texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &delivery.buffer,
                    layout: wgpu::TexelCopyBufferLayout { offset, bytes_per_row: Some(row), rows_per_image: None },
                },
                texture.size(),
            );
        }
        self.queue.submit([encoder.finish()]);
        delivery.buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None })?;
        let mut out = Vec::with_capacity((width * height * luma_bytes * 3 / 2) as usize);
        if let Ok(data) = delivery.buffer.slice(..).get_mapped_range() {
            let (luma, chroma) = data.split_at(chroma_offset as usize);
            for line in luma.chunks_exact(luma_row as usize) {
                out.extend_from_slice(&line[..(width * luma_bytes) as usize]);
            }
            for line in chroma.chunks_exact(chroma_row as usize) {
                out.extend_from_slice(&line[..(cw * chroma_bytes) as usize]);
            }
        }
        delivery.buffer.unmap();
        Ok(out)
    }

    /// Uploads RGBA pixels as a texture layers can sample.
    pub fn upload(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32, rgba: &[u8]) -> wgpu::Texture {
        device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("picture"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        )
    }
}

struct Delivery {
    size: (u32, u32),
    ten_bit: bool,
    luma: wgpu::Texture,
    luma_view: wgpu::TextureView,
    chroma: wgpu::Texture,
    chroma_view: wgpu::TextureView,
    buffer: wgpu::Buffer,
}
