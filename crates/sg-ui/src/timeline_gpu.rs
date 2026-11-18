//! The instanced quad renderer behind the timeline (spike S2): every visible
//! clip, run of tiny clips, track background and selection mark is one
//! 36-byte instance; the whole timeline is one buffer upload and one draw.

use std::sync::{Arc, Mutex, PoisonError};

use egui_wgpu::wgpu;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    /// x, y, width, height in physical pixels, relative to the timeline rect.
    rect: [f32; 4],
    color: [u8; 4],
    /// Corner radius in pixels, border strength 0..1, unused, unused.
    params: [f32; 4],
}

impl Instance {
    pub fn new(rect: [f32; 4], color: egui::Color32, radius: f32, border: f32) -> Instance {
        Instance { rect, color: color.to_array(), params: [radius, border, 0.0, 0.0] }
    }
}

/// Lives in `egui_wgpu::Renderer::callback_resources`; the application
/// inserts it once the device exists.
pub struct TimelineGpu {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

impl TimelineGpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> TimelineGpu {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("timeline"),
            source: wgpu::ShaderSource::Wgsl(include_str!("timeline.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            label: Some("timeline size"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("timeline"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("timeline"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("timeline"),
            layout: Some(&pipeline_layout),
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
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                // Premultiplied alpha, colours in gamma space: what egui itself does.
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
        TimelineGpu { pipeline, uniform, bind, instances: TimelineGpu::buffer(device, capacity), capacity, count: 0 }
    }

    fn buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("timeline instances"),
            size: (capacity * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }
}

/// The paint callback: uploads this frame's instances and draws them.
pub struct TimelineCallback {
    pub instances: Arc<Mutex<Vec<Instance>>>,
    /// Size of the timeline rect in physical pixels.
    pub size: [f32; 2],
}

impl egui_wgpu::CallbackTrait for TimelineCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<TimelineGpu>() else { return Vec::new() };
        let instances = self.instances.lock().unwrap_or_else(PoisonError::into_inner);
        if instances.len() > gpu.capacity {
            gpu.capacity = instances.len().next_power_of_two();
            gpu.instances = TimelineGpu::buffer(device, gpu.capacity);
        }
        queue.write_buffer(&gpu.instances, 0, bytemuck::cast_slice(&instances));
        queue.write_buffer(&gpu.uniform, 0, bytemuck::cast_slice(&[self.size[0], self.size[1], 0.0, 0.0]));
        gpu.count = instances.len() as u32;
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(gpu) = resources.get::<TimelineGpu>() else { return };
        if gpu.count == 0 {
            return;
        }
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &gpu.bind, &[]);
        pass.set_vertex_buffer(0, gpu.instances.slice(..));
        pass.draw(0..4, 0..gpu.count);
    }
}
