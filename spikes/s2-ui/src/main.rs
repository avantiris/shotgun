//! Spike S2 prototype: winit + wgpu + egui panels + timeline (egui painter vs
//! instanced wgpu). Throwaway code. See docs/spikes/s2-ui.md.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_wgpu::wgpu;
use egui_winit::winit;
use s2_ui::timeline::{self, Instance, Project, TimelineCallback, TimelineGpu, RULER_H, TRACKS};
use s2_ui::wl_data::{self, DataEvent, WlData};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Window, WindowId};

#[derive(Clone, Copy, PartialEq, Debug)]
enum TimelineMode {
    Egui,
    Custom,
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Labels {
    None,
    Egui,
    EguiClip, // egui text with a per-label clip rect
    Glyphon,
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Script,
    Idle,
    Keys,
}

#[derive(Clone, Debug)]
struct Args {
    clips: usize,
    timeline: TimelineMode,
    labels: Labels,
    mode: Mode,
    secs: f64,
    present: wgpu::PresentMode,
    size: (u32, u32),
    latency: u32,
}

fn parse_args() -> Args {
    let mut a = Args {
        clips: 50_000,
        timeline: TimelineMode::Custom,
        labels: Labels::Egui,
        mode: Mode::Script,
        secs: 10.0,
        present: wgpu::PresentMode::Fifo,
        size: (1800, 1000),
        latency: 2,
    };
    let v: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < v.len() {
        let val = v.get(i + 1).cloned().unwrap_or_default();
        match v[i].as_str() {
            "--clips" => a.clips = val.parse().unwrap(),
            "--timeline" => {
                a.timeline = match val.as_str() {
                    "egui" => TimelineMode::Egui,
                    "custom" => TimelineMode::Custom,
                    _ => panic!("--timeline egui|custom"),
                }
            }
            "--labels" => {
                a.labels = match val.as_str() {
                    "none" => Labels::None,
                    "egui" => Labels::Egui,
                    "egui-clip" => Labels::EguiClip,
                    "glyphon" => Labels::Glyphon,
                    _ => panic!("--labels none|egui|egui-clip|glyphon"),
                }
            }
            "--mode" => {
                a.mode = match val.as_str() {
                    "script" => Mode::Script,
                    "idle" => Mode::Idle,
                    "keys" => Mode::Keys,
                    _ => panic!("--mode script|idle|keys"),
                }
            }
            "--secs" => a.secs = val.parse().unwrap(),
            "--latency" => a.latency = val.parse().unwrap(),
            "--present" => {
                a.present = match val.as_str() {
                    "fifo" => wgpu::PresentMode::Fifo,
                    "mailbox" => wgpu::PresentMode::Mailbox,
                    "immediate" => wgpu::PresentMode::Immediate,
                    _ => panic!("--present fifo|mailbox|immediate"),
                }
            }
            "--size" => {
                let (w, h) = val.split_once('x').unwrap();
                a.size = (w.parse().unwrap(), h.parse().unwrap());
            }
            other => panic!("unknown arg {other}"),
        }
        i += 2;
    }
    a
}

#[derive(Debug)]
enum UserEvent {
    Repaint(Duration),
    Mark(&'static str),
    Redraw,
    Exit,
    Data(DataEvent),
}

fn process_cpu_time() -> f64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

#[derive(Default)]
struct Stats {
    ui: Vec<f32>,
    tess: Vec<f32>,
    acquire: Vec<f32>,
    encode: Vec<f32>,
    submit: Vec<f32>,
    present: Vec<f32>,
    cpu: Vec<f32>,
    interval: Vec<f32>,
    items: Vec<f32>,
    fill: Vec<f32>,
    label_ms: Vec<f32>,
    labels: Vec<f32>,
    prims: Vec<f32>,
    verts: Vec<f32>,
    last_present: Option<Instant>,
}

fn pct(v: &[f32], skip: usize) -> (f32, f32, f32, f32, f32) {
    let mut s: Vec<f32> = v.iter().skip(skip).copied().collect();
    if s.is_empty() {
        return (0.0, 0.0, 0.0, 0.0, 0.0);
    }
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| s[(((s.len() - 1) as f64) * p).round() as usize];
    let mean = s.iter().sum::<f32>() / s.len() as f32;
    (q(0.5), q(0.95), q(0.99), *s.last().unwrap(), mean)
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    egui_state: egui_winit::State,
    viewer_view: wgpu::TextureView,
    viewer_id: egui::TextureId,
    viewer_pipeline: wgpu::RenderPipeline,
    viewer_uniform: wgpu::Buffer,
    viewer_bind: wgpu::BindGroup,
    text: Option<s2_ui::text::GlyphonLabels>,
}

struct App {
    args: Args,
    proxy: EventLoopProxy<UserEvent>,
    ctx: egui::Context,
    gfx: Option<Gfx>,
    project: Project,
    instances: Arc<Mutex<Vec<Instance>>>,
    glabels: Arc<Mutex<Vec<s2_ui::text::Label>>>,
    t0: f64,
    pps: f64,
    tl_width_px: f32,
    tl_size_px: [u32; 2],
    start: Instant,
    stats: Stats,
    sliders: [f32; 6],
    playing: Arc<AtomicBool>,
    repaint_at: Option<Instant>,
    redraws: u64,
    window_events: u64,
    event_kinds: BTreeMap<&'static str, u64>,
    marks: Vec<(&'static str, Instant, f64, u64, u64)>,
    last_data: String,
    wl: Option<WlData>,
    frame_items: usize,
    frame_labels: usize,
    frame_fill_ms: f32,
    frame_label_ms: f32,
}

const VIEWER_WGSL: &str = r#"
struct U { t: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
struct V { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn vs(@builtin(vertex_index) i: u32) -> V {
    var o: V;
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    o.pos = vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(p.x, 1.0 - p.y);
    return o;
}
@fragment fn fs(v: V) -> @location(0) vec4<f32> {
    let bar = floor(v.uv.x * 8.0) / 8.0;
    let sweep = smoothstep(0.0, 0.01, abs(fract(v.uv.x - u.t.x * 0.25) - 0.5) - 0.48);
    let c = vec3<f32>(0.5 + 0.5 * cos(6.283 * (bar + vec3<f32>(0.0, 0.33, 0.67)))) * (0.35 + 0.65 * v.uv.y);
    return vec4<f32>(mix(c, vec3<f32>(1.0), sweep), 1.0);
}
"#;

impl App {
    fn init_gfx(&mut self, el: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("shotgun S2 UI spike")
            .with_name("shotgun-s2", "shotgun-s2")
            .with_inner_size(winit::dpi::LogicalSize::new(self.args.size.0, self.args.size.1));
        let window = Arc::new(el.create_window(attrs).unwrap());

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            // display handle is passed as recommended for wgpu 30
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle()))
        });
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .unwrap();
        let info = adapter.get_info();
        println!("adapter: {} ({:?}, driver {} {})", info.name, info.backend, info.driver, info.driver_info);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let caps = surface.get_capabilities(&adapter);
        println!("surface present modes: {:?}", caps.present_modes);
        println!("surface formats: {:?}", caps.formats);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let mut config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
        config.format = format;
        config.present_mode = self.args.present;
        config.desired_maximum_frame_latency = self.args.latency;
        config.view_formats = vec![format];
        println!("surface alpha modes: {:?}", caps.alpha_modes);
        if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        }
        surface.configure(&device, &config);
        println!(
            "surface: {}x{} {:?} present_mode={:?} max_frame_latency={}",
            config.width, config.height, config.format, config.present_mode, config.desired_maximum_frame_latency
        );

        let mut renderer = egui_wgpu::Renderer::new(
            &device,
            format,
            egui_wgpu::RendererOptions { msaa_samples: 1, ..Default::default() },
        );
        renderer.callback_resources.insert(TimelineGpu::new(&device, format));

        // Viewer placeholder: a texture rendered by our own wgpu pass, shown via egui.
        let viewer_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer"),
            size: wgpu::Extent3d { width: 1280, height: 720, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let viewer_view = viewer_tex.create_view(&Default::default());
        let viewer_id = renderer.register_native_texture(&device, &viewer_view, wgpu::FilterMode::Linear);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewer"),
            source: wgpu::ShaderSource::Wgsl(VIEWER_WGSL.into()),
        });
        let viewer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewer"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let viewer_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewer u"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let viewer_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &viewer_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: viewer_uniform.as_entire_binding() }],
        });

        let egui_state = egui_winit::State::new(
            self.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );

        let text = (self.args.labels == Labels::Glyphon)
            .then(|| s2_ui::text::GlyphonLabels::new(&device, &queue, format));

        // DnD + clipboard through our own wl_data_device on winit's display.
        if let Some((d, s)) = wl_data::wayland_ptrs(&*window) {
            let proxy = self.proxy.clone();
            self.wl = Some(unsafe {
                WlData::spawn(d, s, move |ev| {
                    let _ = proxy.send_event(UserEvent::Data(ev));
                })
            });
        }

        let mut gfx = Gfx {
            window,
            surface,
            device,
            queue,
            config,
            renderer,
            egui_state,
            viewer_view,
            viewer_id,
            viewer_pipeline,
            viewer_uniform,
            viewer_bind,
            text,
        };
        // Render the viewer once so idle mode shows something.
        let mut enc = gfx.device.create_command_encoder(&Default::default());
        Self::viewer_pass(&mut gfx, &mut enc, 0.0);
        gfx.queue.submit([enc.finish()]);
        gfx.window.request_redraw();
        self.gfx = Some(gfx);
    }

    fn viewer_pass(gfx: &mut Gfx, enc: &mut wgpu::CommandEncoder, t: f32) {
        gfx.queue.write_buffer(&gfx.viewer_uniform, 0, bytemuck::cast_slice(&[t, 0.0, 0.0, 0.0f32]));
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("viewer"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &gfx.viewer_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&gfx.viewer_pipeline);
        pass.set_bind_group(0, &gfx.viewer_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    fn script(&mut self, t: f64) {
        let w = self.tl_width_px.max(100.0) as f64;
        let dur = self.project.duration;
        let (lo, hi) = ((w / dur).ln(), 200.0f64.ln());
        let z = 0.5 - 0.5 * (std::f64::consts::TAU * t / 5.0).cos();
        self.pps = (lo + (hi - lo) * z).exp();
        let visible = w / self.pps;
        let center = dur * (0.5 + 0.45 * (std::f64::consts::TAU * t / 7.0).sin());
        self.t0 = (center - visible / 2.0).clamp(0.0, (dur - visible).max(0.0));
    }

    fn ui(&mut self, ui: &mut egui::Ui, viewer_id: egui::TextureId) {
        let track_h = 17.0;
        egui::Panel::bottom("timeline")
            .exact_size(RULER_H + TRACKS as f32 * track_h + 16.0)
            .show(ui, |ui| self.timeline_ui(ui));
        egui::Panel::left("assets").default_size(260.0).show(ui, |ui| {
            ui.heading("Assets");
            ui.label(format!("{} clips, {} tracks", self.project.clip_count, TRACKS));
            ui.label(format!("timeline: {:?}, labels: {:?}", self.args.timeline, self.args.labels));
            ui.separator();
            for i in 0..24 {
                ui.label(format!("asset_{i:03}.mov   00:0{}:1{}", i % 6, i % 10));
            }
            ui.separator();
            ui.label("last DnD / clipboard event:");
            ui.label(&self.last_data);
        });
        egui::Panel::right("inspector").default_size(280.0).show(ui, |ui| {
            ui.heading("Inspector");
            let names = ["Position X", "Position Y", "Scale", "Rotation", "Opacity", "Volume"];
            for (v, n) in self.sliders.iter_mut().zip(names) {
                ui.add(egui::Slider::new(v, 0.0..=1.0).text(n));
            }
            ui.separator();
            ui.label(format!("zoom: {:.3} px/s", self.pps));
            ui.label(format!("scroll: {:.1} s", self.t0));
            ui.label(format!("visible items: {}", self.frame_items));
            ui.label(format!("labels: {}", self.frame_labels));
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let avail = ui.available_size();
            let s = (avail.x / 16.0).min(avail.y / 9.0);
            ui.centered_and_justified(|ui| {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(viewer_id, egui::vec2(16.0 * s, 9.0 * s))));
            });
        });
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let (rect, _resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let width_px = rect.width() * ppp;
        self.tl_width_px = width_px;
        self.tl_size_px = [width_px as u32, (rect.height() * ppp) as u32];
        let track_h = ((rect.height() - RULER_H) / TRACKS as f32).floor();
        let painter = ui.painter_at(rect);
        let font = egui::FontId::proportional(11.0);
        let label_color = egui::Color32::from_gray(245);
        let mut items = 0usize;
        let fill_ms;
        let mut label_ms = 0.0f32;
        let tl_start = Instant::now();
        let mut labels = 0usize;
        let (t0, pps) = (self.t0, self.pps);
        let label_mode = self.args.labels;
        let mut glabels = self.glabels.lock().unwrap();
        glabels.clear();

        // Label helper shared by both timeline modes (egui text paths).
        let egui_label = |painter: &egui::Painter, x0: f32, x1: f32, y: f32, name: &str| {
            let lx = rect.left() + x0.max(0.0) / ppp + 4.0;
            let right = rect.left() + x1 / ppp - 3.0;
            match label_mode {
                Labels::Egui => {
                    painter.text(egui::pos2(lx, y), egui::Align2::LEFT_CENTER, name, font.clone(), label_color);
                }
                Labels::EguiClip => {
                    let clip = egui::Rect::from_min_max(egui::pos2(lx, y - 8.0), egui::pos2(right, y + 8.0));
                    painter.with_clip_rect(clip).text(
                        egui::pos2(lx, y),
                        egui::Align2::LEFT_CENTER,
                        name,
                        font.clone(),
                        label_color,
                    );
                }
                _ => {}
            }
        };

        match self.args.timeline {
            TimelineMode::Egui => {
                painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));
                for (ti, tr) in self.project.tracks.iter().enumerate() {
                    let y = rect.top() + RULER_H + ti as f32 * track_h;
                    if ti % 2 == 0 {
                        painter.rect_filled(
                            egui::Rect::from_min_size(egui::pos2(rect.left(), y), egui::vec2(rect.width(), track_h)),
                            0.0,
                            egui::Color32::from_gray(30),
                        );
                    }
                    timeline::visit_track(tr, t0, pps, width_px as f64, |it| {
                        items += 1;
                        let r = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + it.x0 / ppp, y + 1.0),
                            egui::pos2(rect.left() + it.x1 / ppp, y + track_h - 1.0),
                        );
                        let [_, rr, gg, bb] = it.color.to_be_bytes();
                        let col = egui::Color32::from_rgb(rr, gg, bb);
                        match it.clip {
                            Some(ci) => {
                                painter.rect_filled(r, 3.0, col);
                                if label_mode != Labels::None && (it.x1.min(width_px) - it.x0.max(0.0)) / ppp >= timeline::LABEL_MIN_PT {
                                    labels += 1;
                                    egui_label(&painter, it.x0, it.x1, y + track_h * 0.5, &tr.names[ci]);
                                }
                            }
                            None => {
                                painter.rect_filled(r, 0.0, col);
                            }
                        }
                    });
                }
                fill_ms = tl_start.elapsed().as_secs_f32() * 1000.0;
            }
            TimelineMode::Custom => {
                let mut inst = self.instances.lock().unwrap();
                inst.clear();
                let h_px = rect.height() * ppp;
                inst.push(Instance::new(0.0, 0.0, width_px, h_px, 0x181818, 255, 0.0, 0.0));
                for (ti, tr) in self.project.tracks.iter().enumerate() {
                    let y = RULER_H + ti as f32 * track_h; // points, relative
                    if ti % 2 == 0 {
                        inst.push(Instance::new(0.0, y * ppp, width_px, track_h * ppp, 0x1E1E1E, 255, 0.0, 0.0));
                    }
                    let (iy, ih) = ((y + 1.0) * ppp, (track_h - 2.0) * ppp);
                    timeline::visit_track(tr, t0, pps, width_px as f64, |it| {
                        items += 1;
                        match it.clip {
                            Some(ci) => {
                                inst.push(Instance::new(it.x0, iy, it.x1 - it.x0, ih, it.color, 255, 3.0 * ppp, 1.0));
                                if label_mode != Labels::None && (it.x1.min(width_px) - it.x0.max(0.0)) / ppp >= timeline::LABEL_MIN_PT {
                                    labels += 1;
                                    glabels.push(s2_ui::text::Label {
                                        track: ti as u32,
                                        clip: ci as u32,
                                        x: it.x0.max(0.0) + 4.0 * ppp,
                                        y: iy,
                                        right: it.x1 - 3.0 * ppp,
                                        bottom: iy + ih,
                                    });
                                    // egui text is added after the callback shape (below) so it draws on top:
                                    // collected through a second pass to keep ordering simple.
                                }
                            }
                            None => inst.push(Instance::new(it.x0, iy, it.x1 - it.x0, ih, it.color, 255, 0.0, 0.0)),
                        }
                    });
                }
                drop(inst);
                painter.add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    TimelineCallback { instances: self.instances.clone(), size_px: [width_px, h_px] },
                ));
                fill_ms = tl_start.elapsed().as_secs_f32() * 1000.0;
                let lab_start = Instant::now();
                if matches!(label_mode, Labels::Egui | Labels::EguiClip) {
                    for l in glabels.iter() {
                        let name = &self.project.tracks[l.track as usize].names[l.clip as usize];
                        let y = rect.top() + (l.y + l.bottom) * 0.5 / ppp;
                        egui_label(&painter, l.x - 4.0 * ppp, l.right + 3.0 * ppp, y, name);
                    }
                }
                label_ms = lab_start.elapsed().as_secs_f32() * 1000.0;
                if label_mode == Labels::Glyphon {
                    painter.add(egui_wgpu::Callback::new_paint_callback(
                        rect,
                        s2_ui::text::GlyphonCallback { size_px: [width_px as u32, h_px as u32] },
                    ));
                }
            }
        }
        drop(glabels);

        // Ruler ticks + playhead (few shapes, egui painter in both modes).
        let step_px = 100.0f64;
        let step_t = step_px * ppp as f64 / pps;
        let mag = 10f64.powf(step_t.log10().ceil());
        let mut tt = (t0 / mag).floor() * mag;
        while (tt - t0) * pps < width_px as f64 {
            let x = rect.left() + ((tt - t0) * pps) as f32 / ppp;
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + RULER_H)],
                egui::Stroke::new(1.0, egui::Color32::from_gray(110)),
            );
            painter.text(
                egui::pos2(x + 3.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                format!("{tt:.0}s"),
                egui::FontId::monospace(10.0),
                egui::Color32::from_gray(170),
            );
            tt += mag;
        }
        let px = rect.center().x;
        painter.line_segment(
            [egui::pos2(px, rect.top()), egui::pos2(px, rect.bottom())],
            egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 80, 80)),
        );
        self.frame_items = items;
        self.frame_fill_ms = fill_ms;
        self.frame_label_ms = label_ms;
        self.frame_labels = labels;
    }

    fn redraw(&mut self) {
        let Some(mut gfx) = self.gfx.take() else { return };
        self.redraws += 1;
        let t = self.start.elapsed().as_secs_f64();
        let animating = self.args.mode == Mode::Script || self.playing.load(Ordering::Relaxed);
        if self.args.mode == Mode::Script {
            self.script(t);
        }

        let a = Instant::now();
        let raw = gfx.egui_state.take_egui_input(&gfx.window);
        let ctx = self.ctx.clone();
        let viewer_id = gfx.viewer_id;
        let out = ctx.run_ui(raw, |ui| self.ui(ui, viewer_id));
        gfx.egui_state.handle_platform_output(&gfx.window, out.platform_output);
        let b = Instant::now();
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let c = Instant::now();

        let frame = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            other => {
                eprintln!("surface: {other:?}, reconfiguring");
                gfx.surface.configure(&gfx.device, &gfx.config);
                gfx.window.request_redraw();
                self.gfx = Some(gfx);
                return;
            }
        };
        let d = Instant::now();

        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                gfx.renderer.update_texture(&gfx.device, &gfx.queue, *id, delta);
            }
        }
        let sd = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gfx.config.width, gfx.config.height],
            pixels_per_point: out.pixels_per_point,
        };
        let mut enc = gfx.device.create_command_encoder(&Default::default());
        if animating {
            Self::viewer_pass(&mut gfx, &mut enc, t as f32);
        }
        if let Some(text) = gfx.text.take() {
            // glyphon: shape/layout + atlas upload for this frame's labels
            let labels = self.glabels.lock().unwrap();
            let text = text.prepare(&gfx.device, &gfx.queue, &self.project, &labels, self.tl_size_px, out.pixels_per_point);
            gfx.renderer.callback_resources.insert(text);
        }
        let cmds = gfx.renderer.update_buffers(&gfx.device, &gfx.queue, &mut enc, &prims, &sd);
        {
            let view = frame.texture.create_view(&Default::default());
            let pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.07, g: 0.07, b: 0.07, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            gfx.renderer.render(&mut pass.forget_lifetime(), &prims, &sd);
        }
        if self.args.labels == Labels::Glyphon {
            gfx.text = gfx.renderer.callback_resources.remove::<s2_ui::text::GlyphonLabels>();
        }
        for id in &out.textures_delta.free {
            gfx.renderer.free_texture(id);
        }
        let e = Instant::now();
        gfx.queue.submit(cmds.into_iter().chain(std::iter::once(enc.finish())));
        let f = Instant::now();
        gfx.window.pre_present_notify();
        gfx.queue.present(frame);
        let g = Instant::now();

        let ms = |x: Instant, y: Instant| (y - x).as_secs_f32() * 1000.0;
        let s = &mut self.stats;
        s.ui.push(ms(a, b));
        s.tess.push(ms(b, c));
        s.acquire.push(ms(c, d));
        s.encode.push(ms(d, e));
        s.submit.push(ms(e, f));
        s.present.push(ms(f, g));
        s.cpu.push(ms(a, c) + ms(d, f));
        if let Some(lp) = s.last_present {
            s.interval.push(ms(lp, g));
        }
        s.last_present = Some(g);
        s.items.push(self.frame_items as f32);
        s.fill.push(self.frame_fill_ms);
        s.label_ms.push(self.frame_label_ms);
        s.labels.push(self.frame_labels as f32);
        s.prims.push(prims.len() as f32);
        let verts: usize = prims
            .iter()
            .map(|p| match &p.primitive {
                egui::epaint::Primitive::Mesh(m) => m.vertices.len(),
                _ => 0,
            })
            .sum();
        s.verts.push(verts as f32);

        if self.args.mode == Mode::Script {
            gfx.window.request_redraw();
        }
        self.gfx = Some(gfx);
    }

    fn report(&self) {
        let a = &self.args;
        let s = &self.stats;
        let skip = if a.mode == Mode::Script { 30 } else { 0 };
        println!("--- frames: {} (first {skip} skipped in percentiles), wall {:.2} s", s.cpu.len(), self.start.elapsed().as_secs_f64());
        let row = |name: &str, v: &[f32]| {
            let (p50, p95, p99, max, mean) = pct(v, skip);
            println!("{name:<28} p50 {p50:8.3}  p95 {p95:8.3}  p99 {p99:8.3}  max {max:8.3}  mean {mean:8.3}");
        };
        row("cpu total (ms)", &s.cpu);
        row("  ui build (ms)", &s.ui);
        row("  tessellate (ms)", &s.tess);
        row("  encode+buffers (ms)", &s.encode);
        row("  submit (ms)", &s.submit);
        row("acquire/get_current (ms)", &s.acquire);
        row("present() call (ms)", &s.present);
        row("present interval (ms)", &s.interval);
        row("visible items", &s.items);
        row("  timeline walk+fill (ms)", &s.fill);
        row("  timeline egui labels (ms)", &s.label_ms);
        row("labels", &s.labels);
        row("egui clipped primitives", &s.prims);
        row("egui vertices", &s.verts);
        let (p50, p95, p99, max, _) = pct(&s.cpu, skip);
        let (i50, i95, i99, imax, _) = pct(&s.interval, skip);
        let over = s.interval.iter().skip(skip).filter(|&&x| x > 10.4).count();
        println!(
            "RESULT clips={} timeline={:?} labels={:?} present={:?} cpu_p50={p50:.3} cpu_p95={p95:.3} cpu_p99={p99:.3} cpu_max={max:.3} int_p50={i50:.3} int_p95={i95:.3} int_p99={i99:.3} int_max={imax:.3} frames={} intervals_over_1.5x144Hz={over}",
            self.project.clip_count,
            a.timeline,
            a.labels,
            a.present,
            s.cpu.len()
        );
        if let Some(g) = &self.gfx {
            if let Some(m) = g.window.current_monitor() {
                println!("monitor at exit: {:?} {:?} mHz", m.name(), m.refresh_rate_millihertz());
            }
        }
        for w in self.marks.windows(2) {
            let (n0, t0, c0, r0, e0) = w[0];
            let (n1, t1, c1, r1, e1) = w[1];
            let dt = (t1 - t0).as_secs_f64();
            println!(
                "PHASE {n0} -> {n1}: {dt:.2} s, redraws {}, window events {}, process CPU {:.1} ms ({:.2} % of one core)",
                r1 - r0,
                e1 - e0,
                (c1 - c0) * 1000.0,
                (c1 - c0) / dt * 100.0
            );
        }
        println!("window event kinds: {:?}", self.event_kinds);
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_none() {
            self.init_gfx(el);
            self.start = Instant::now();
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Repaint(delay) => {
                if delay.is_zero() {
                    if let Some(g) = &self.gfx {
                        g.window.request_redraw();
                    }
                } else if delay < Duration::from_secs(3600) {
                    let at = Instant::now() + delay;
                    self.repaint_at = Some(self.repaint_at.map_or(at, |x| x.min(at)));
                }
            }
            UserEvent::Redraw => {
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
            }
            UserEvent::Mark(name) => {
                self.marks.push((name, Instant::now(), process_cpu_time(), self.redraws, self.window_events));
            }
            UserEvent::Exit => {
                self.report();
                if let Some(wl) = self.wl.take() {
                    wl.shutdown(); // must stop before winit closes the wl_display
                }
                self.gfx = None;
                el.exit();
            }
            UserEvent::Data(ev) => {
                let line = match &ev {
                    DataEvent::Drop { mime, data, x, y } => format!(
                        "Drop at ({x:.0},{y:.0}) mime={mime} {} bytes: {:?}",
                        data.len(),
                        if mime == "text/uri-list" {
                            format!("{:?}", wl_data::parse_uri_list(data))
                        } else {
                            String::from_utf8_lossy(&data[..data.len().min(80)]).into_owned()
                        }
                    ),
                    DataEvent::DragMotion { .. } => return,
                    other => format!("{other:?}"),
                };
                println!("DATA {line}");
                self.last_data = line;
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::RedrawRequested) {
            self.redraw();
            return;
        }
        self.window_events += 1;
        *self.event_kinds.entry(egui_winit::short_window_event_description(&event)).or_default() += 1;
        let Some(gfx) = self.gfx.as_mut() else { return };
        match &event {
            WindowEvent::CloseRequested => {
                let _ = self.proxy.send_event(UserEvent::Exit);
            }
            WindowEvent::Resized(size) => {
                gfx.config.width = size.width.max(1);
                gfx.config.height = size.height.max(1);
                gfx.surface.configure(&gfx.device, &gfx.config);
            }
            WindowEvent::KeyboardInput { event: k, .. } => {
                println!(
                    "KEY {:?} physical={:?} logical={:?} text={:?} repeat={}",
                    k.state, k.physical_key, k.logical_key, k.text, k.repeat
                );
            }
            _ => {}
        }
        let _ = el;
        let resp = gfx.egui_state.on_window_event(&gfx.window, &event);
        if resp.repaint {
            gfx.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if let Some(at) = self.repaint_at {
            if Instant::now() >= at {
                self.repaint_at = None;
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
                el.set_control_flow(ControlFlow::Wait);
            } else {
                el.set_control_flow(ControlFlow::WaitUntil(at));
            }
        } else {
            el.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn main() {
    let args = parse_args();
    println!("args: {args:?}");
    let t = Instant::now();
    let project = Project::synthetic(args.clips);
    println!(
        "project: {} clips, {} tracks, duration {:.0} s, built in {:.1} ms",
        project.clip_count,
        TRACKS,
        project.duration,
        t.elapsed().as_secs_f64() * 1000.0
    );

    let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();
    let ctx = egui::Context::default();
    {
        // Any thread may call ctx.request_repaint(); this wakes the winit loop.
        let proxy = proxy.clone();
        ctx.set_request_repaint_callback(move |info| {
            let _ = proxy.send_event(UserEvent::Repaint(info.delay));
        });
    }
    let playing = Arc::new(AtomicBool::new(false));

    // Driver thread: phases + exit.
    {
        let proxy = proxy.clone();
        let ctx = ctx.clone();
        let playing = playing.clone();
        let (mode, secs) = (args.mode, args.secs);
        std::thread::spawn(move || {
            let sleep = |s: f64| std::thread::sleep(Duration::from_secs_f64(s));
            match mode {
                Mode::Script | Mode::Keys => sleep(secs),
                Mode::Idle => {
                    sleep(2.0);
                    let _ = proxy.send_event(UserEvent::Mark("idle-A"));
                    sleep(3.0);
                    // 60 "new video frame" notifications per second from this non-UI thread, two ways.
                    for (name, via_egui) in [
                        ("playback-A: 180 x ctx.request_repaint()", true),
                        ("playback-B: 180 x EventLoopProxy -> Window::request_redraw()", false),
                    ] {
                        let _ = proxy.send_event(UserEvent::Mark(name));
                        playing.store(true, Ordering::Relaxed);
                        let mut next = Instant::now();
                        for _ in 0..180 {
                            if via_egui {
                                ctx.request_repaint();
                            } else {
                                let _ = proxy.send_event(UserEvent::Redraw);
                            }
                            next += Duration::from_nanos(16_666_667);
                            std::thread::sleep(next.saturating_duration_since(Instant::now()));
                        }
                        playing.store(false, Ordering::Relaxed);
                        sleep(0.3); // let the last frame settle
                    }
                    let _ = proxy.send_event(UserEvent::Mark("idle-B"));
                    sleep(3.0);
                    let _ = proxy.send_event(UserEvent::Mark("end"));
                }
            }
            let _ = proxy.send_event(UserEvent::Exit);
        });
    }

    let mut app = App {
        pps: 30.0,
        t0: project.duration * 0.37,
        args,
        proxy,
        ctx,
        gfx: None,
        project,
        instances: Default::default(),
        glabels: Default::default(),
        tl_width_px: 1800.0,
        tl_size_px: [1, 1],
        start: Instant::now(),
        stats: Stats::default(),
        sliders: [0.5, 0.5, 0.3, 0.0, 1.0, 0.8],
        playing,
        repaint_at: None,
        redraws: 0,
        window_events: 0,
        event_kinds: BTreeMap::new(),
        marks: Vec::new(),
        last_data: "(none)".into(),
        wl: None,
        frame_items: 0,
        frame_labels: 0,
        frame_fill_ms: 0.0,
        frame_label_ms: 0.0,
    };
    event_loop.run_app(&mut app).unwrap();
}
