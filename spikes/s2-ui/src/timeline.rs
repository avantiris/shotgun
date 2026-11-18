//! Synthetic project, interval lookup of visible clips, and the two timeline
//! renderers under test: (a) egui painter, (b) instanced wgpu via paint callback.

use egui_wgpu::wgpu;
use std::sync::{Arc, Mutex};

pub const TRACKS: usize = 20;
pub const RULER_H: f32 = 18.0;
/// Clips narrower than this (physical px) are merged into "dense run" quads.
pub const MIN_PX: f64 = 2.0;
/// Clips at least this wide (points) get a label.
pub const LABEL_MIN_PT: f32 = 64.0;

/// One track: clips sorted by start, non-overlapping (struct of arrays).
pub struct Track {
    pub starts: Vec<f64>,
    pub ends: Vec<f64>,
    pub colors: Vec<u32>, // 0xRRGGBB
    pub names: Vec<String>,
}

pub struct Project {
    pub tracks: Vec<Track>,
    pub duration: f64,
    pub clip_count: usize,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

const PALETTE: [u32; 8] = [
    0x4C8DD6, 0x57B894, 0xD6A24C, 0xC7604F, 0x8E6BD1, 0x4CB5C4, 0xB8588F, 0x7C9A45,
];

impl Project {
    pub fn synthetic(clips: usize) -> Self {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let per = clips / TRACKS;
        let mut tracks = Vec::with_capacity(TRACKS);
        let mut duration = 0.0f64;
        let mut id = 0usize;
        for _ in 0..TRACKS {
            let mut t = 0.0;
            let mut tr = Track {
                starts: Vec::with_capacity(per),
                ends: Vec::with_capacity(per),
                colors: Vec::with_capacity(per),
                names: Vec::with_capacity(per),
            };
            for _ in 0..per {
                t += if rng.f() < 0.3 { rng.f() * 3.0 } else { 0.0 }; // gap
                let len = 0.5 + rng.f() * rng.f() * 9.5; // 0.5 .. 10 s, skewed short
                tr.starts.push(t);
                tr.ends.push(t + len);
                tr.colors.push(PALETTE[(rng.next() % 8) as usize]);
                tr.names.push(format!("clip {id}"));
                id += 1;
                t += len;
            }
            duration = duration.max(t);
            tracks.push(tr);
        }
        Project { tracks, duration, clip_count: id }
    }
}

/// What to draw for one visible element of a track.
pub struct Item {
    pub x0: f32, // physical px relative to timeline left
    pub x1: f32,
    pub color: u32,
    /// Some(clip index) for a real clip, None for a merged run of sub-pixel clips.
    pub clip: Option<usize>,
}

/// Visit visible items of a track. `t0` = time at left edge, `pps` = physical
/// pixels per second, `width` = viewport width in physical px.
/// Cost: O(log n) to find the first clip, then O(1) per emitted clip and
/// O(log n) per emitted dense run. Emits at most ~width/MIN_PX items.
pub fn visit_track(tr: &Track, t0: f64, pps: f64, width: f64, mut f: impl FnMut(Item)) -> usize {
    let n = tr.starts.len();
    let t1 = t0 + width / pps;
    let mut i = tr.ends.partition_point(|&e| e <= t0);
    let mut touched = 0;
    while i < n && tr.starts[i] < t1 {
        touched += 1;
        let s = tr.starts[i];
        let e = tr.ends[i];
        if (e - s) * pps >= MIN_PX {
            f(Item {
                x0: ((s - t0) * pps) as f32,
                x1: ((e - t0) * pps) as f32,
                color: tr.colors[i],
                clip: Some(i),
            });
            i += 1;
        } else {
            let bucket_end = s + MIN_PX / pps;
            let mut j = i + 1 + tr.starts[i + 1..].partition_point(|&st| st < bucket_end);
            // The last clip of the bucket may be a wide one that merely starts
            // inside it: leave it to be drawn as a normal clip.
            if j - 1 > i && (tr.ends[j - 1] - tr.starts[j - 1]) * pps >= MIN_PX {
                j -= 1;
            }
            let x0 = ((s - t0) * pps) as f32;
            let x1 = (((tr.ends[j - 1] - t0) * pps) as f32).max(x0 + 1.0);
            f(Item { x0, x1, color: 0x6F7C8C, clip: None });
            i = j;
        }
    }
    touched
}

// ---------------------------------------------------------------- GPU side

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub rect: [f32; 4],
    pub color: [u8; 4],
    pub params: [f32; 4],
}

impl Instance {
    pub fn new(x: f32, y: f32, w: f32, h: f32, rgb: u32, a: u8, radius: f32, border: f32) -> Self {
        Instance {
            rect: [x, y, w, h],
            color: [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, a],
            params: [radius, border, 0.0, 0.0],
        }
    }
}

/// Lives in `egui_wgpu::Renderer::callback_resources`.
pub struct TimelineGpu {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

impl TimelineGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("timeline"),
            source: wgpu::ShaderSource::Wgsl(include_str!("timeline.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("timeline"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timeline uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("timeline"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("timeline"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("timeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Unorm8x4, 2 => Float32x4],
                })],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 4096;
        let instances = Self::make_buffer(device, capacity);
        TimelineGpu { pipeline, uniform, bind, instances, capacity, count: 0 }
    }

    fn make_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timeline instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

/// The egui paint callback: uploads this frame's instances, draws them in one call.
pub struct TimelineCallback {
    pub instances: Arc<Mutex<Vec<Instance>>>,
    pub size_px: [f32; 2],
}

impl egui_wgpu::CallbackTrait for TimelineCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _sd: &egui_wgpu::ScreenDescriptor,
        _enc: &mut wgpu::CommandEncoder,
        res: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gpu: &mut TimelineGpu = res.get_mut().unwrap();
        let inst = self.instances.lock().unwrap();
        if inst.len() > gpu.capacity {
            gpu.capacity = inst.len().next_power_of_two();
            gpu.instances = TimelineGpu::make_buffer(device, gpu.capacity);
        }
        queue.write_buffer(&gpu.instances, 0, bytemuck::cast_slice(&inst));
        queue.write_buffer(
            &gpu.uniform,
            0,
            bytemuck::cast_slice(&[self.size_px[0], self.size_px[1], 0.0, 0.0]),
        );
        gpu.count = inst.len() as u32;
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        res: &egui_wgpu::CallbackResources,
    ) {
        let gpu: &TimelineGpu = res.get().unwrap();
        if gpu.count == 0 {
            return;
        }
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &gpu.bind, &[]);
        pass.set_vertex_buffer(0, gpu.instances.slice(..));
        pass.draw(0..4, 0..gpu.count);
    }
}
