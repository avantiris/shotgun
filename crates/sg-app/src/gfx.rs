//! Window, wgpu surface and egui renderer. Settings follow spike S2
//! (docs/spikes/s2-ui.md, "Decision").

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, anyhow};
use egui_wgpu::wgpu;
use egui_winit::winit;
use winit::event_loop::ActiveEventLoop;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::Window;

pub struct Gfx {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    pub egui_state: egui_winit::State,
}

pub enum Frame {
    /// Presented; CPU time from input to submit in milliseconds.
    Presented { cpu_ms: f32 },
    /// The compositor is not showing the window; nothing was drawn.
    Skipped,
}

impl Gfx {
    pub fn new(el: &ActiveEventLoop, ctx: &egui::Context, app_id: &str) -> anyhow::Result<Gfx> {
        let t0 = Instant::now();
        let attrs = Window::default_attributes()
            .with_title("Shotgun")
            .with_name(app_id, app_id)
            .with_inner_size(winit::dpi::LogicalSize::new(1600, 900));
        let window = Arc::new(el.create_window(attrs).context("creating the window")?);
        let t_window = Instant::now();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle()))
        });
        let surface = instance.create_surface(window.clone()).context("creating the surface")?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .context("no Vulkan adapter for this surface")?;
        let t_adapter = Instant::now();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .context("opening the GPU device")?;
        let t_device = Instant::now();

        let caps = surface.get_capabilities(&adapter);
        // egui blends in gamma space and wants a non-sRGB target.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .or_else(|| caps.formats.first().copied())
            .ok_or_else(|| anyhow!("the surface offers no texture format"))?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow!("the adapter cannot present to this surface"))?;
        config.format = format;
        config.view_formats = vec![format];
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 2;
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        }
        surface.configure(&device, &config);

        let renderer = egui_wgpu::Renderer::new(
            &device,
            format,
            egui_wgpu::RendererOptions { msaa_samples: 1, ..Default::default() },
        );
        let egui_state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let info = adapter.get_info();
        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
        log::info!(
            "gpu: {} ({}), {}x{} {:?}; window {:.1} ms, adapter {:.1} ms, device {:.1} ms, surface+egui {:.1} ms",
            info.name,
            info.driver_info,
            config.width,
            config.height,
            config.format,
            ms(t0, t_window),
            ms(t_window, t_adapter),
            ms(t_adapter, t_device),
            ms(t_device, Instant::now()),
        );
        Ok(Gfx { window, surface, device, queue, config, renderer, egui_state })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Runs the UI once and uploads what it changed in egui's textures.
    fn build(&mut self, ctx: &egui::Context, ui: &mut dyn FnMut(&mut egui::Ui)) -> Built {
        let raw = self.egui_state.take_egui_input(&self.window);
        let out = ctx.run_ui(raw, |root| ui(root));
        self.egui_state.handle_platform_output(&self.window, out.platform_output);
        // Applied even if the frame is not drawn, or the font atlas would
        // miss updates.
        for (id, delta) in &out.textures_delta.set {
            for d in delta {
                self.renderer.update_texture(&self.device, &self.queue, *id, d);
            }
        }
        Built {
            prims: ctx.tessellate(out.shapes, out.pixels_per_point),
            pixels_per_point: out.pixels_per_point,
            free: out.textures_delta.free.iter().copied().collect(),
        }
    }

    fn draw(&mut self, built: &Built, view: &wgpu::TextureView) {
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: built.pixels_per_point,
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let extra = self.renderer.update_buffers(&self.device, &self.queue, &mut encoder, &built.prims, &screen);
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui"),
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
            self.renderer.render(&mut pass.forget_lifetime(), &built.prims, &screen);
        }
        for id in &built.free {
            self.renderer.free_texture(id);
        }
        self.queue.submit(extra.into_iter().chain(std::iter::once(encoder.finish())));
    }

    /// Runs the UI and presents it.
    pub fn render(&mut self, ctx: &egui::Context, mut ui: impl FnMut(&mut egui::Ui)) -> Frame {
        let start = Instant::now();
        let built = self.build(ctx, &mut ui);
        let built_at = Instant::now();
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return Frame::Skipped;
            }
            // Hidden workspace or minimised: the next event that matters
            // will ask for a redraw; do not spin.
            other => {
                log::debug!("surface not available: {other:?}");
                return Frame::Skipped;
            }
        };
        let acquired = Instant::now();
        self.draw(&built, &frame.texture.create_view(&Default::default()));
        let submitted = Instant::now();
        // Lets winit pace redraws by Wayland frame callbacks.
        self.window.pre_present_notify();
        self.queue.present(frame);
        let cpu = (built_at - start) + (submitted - acquired);
        Frame::Presented { cpu_ms: cpu.as_secs_f32() * 1000.0 }
    }

    /// Renders the UI into an off-screen texture and writes it as a PNG.
    /// For automated checks: works while the window is on a hidden workspace
    /// and never touches the screen. Blocks; development use only.
    pub fn screenshot(
        &mut self,
        ctx: &egui::Context,
        mut ui: impl FnMut(&mut egui::Ui),
        path: &Path,
    ) -> anyhow::Result<()> {
        // Two passes: egui settles sizes and focus one frame late.
        self.build(ctx, &mut ui);
        let built = self.build(ctx, &mut ui);
        let (width, height) = (self.config.width, self.config.height);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("screenshot"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        self.draw(&built, &texture.create_view(&Default::default()));

        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("screenshot"),
            size: row as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: None },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .context("waiting for the GPU")?;

        let swap = matches!(self.config.format, wgpu::TextureFormat::Bgra8Unorm);
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        {
            let data = buffer.slice(..).get_mapped_range().context("reading the screenshot back")?;
            for line in data.chunks_exact(row as usize) {
                for px in line[..(width * 4) as usize].as_chunks::<4>().0 {
                    let (r, b) = if swap { (px[2], px[0]) } else { (px[0], px[2]) };
                    rgb.extend_from_slice(&[r, px[1], b]);
                }
            }
        }
        let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgb)?;
        Ok(())
    }
}

struct Built {
    prims: Vec<egui::ClippedPrimitive>,
    pixels_per_point: f32,
    free: Vec<egui::TextureId>,
}

impl Gfx {
    /// The GPU device and queue, shared with the preview engine.
    pub fn device(&self) -> (wgpu::Device, wgpu::Queue) {
        (self.device.clone(), self.queue.clone())
    }

    /// Makes a texture rendered elsewhere showable in the UI.
    pub fn register_texture(&mut self, view: &wgpu::TextureView) -> egui::TextureId {
        self.renderer.register_native_texture(&self.device, view, wgpu::FilterMode::Linear)
    }

    /// Points an id from [`register_texture`](Self::register_texture) at another texture.
    pub fn replace_texture(&mut self, id: egui::TextureId, view: &wgpu::TextureView) {
        self.renderer.update_egui_texture_from_wgpu_texture(&self.device, view, wgpu::FilterMode::Linear, id);
    }
}
