//! Tiny manual test for Wayland file drag-and-drop and clipboard paste next to winit 0.30.
//!
//!   cargo run --release --bin dnd_test            # window stays 120 s
//!
//! Drag files from a file manager onto the window; copy a URL / image / files
//! somewhere and press Ctrl+V (physical V key) or P in the window. Results are
//! printed to stdout. The window turns green while a drag hovers over it.
//!
//! Options: --secs N (auto-exit), --auto-paste S (paste S seconds after start, for scripted runs)

use std::sync::Arc;
use std::time::Duration;

use egui_wgpu::wgpu;
use egui_winit::winit;
use s2_ui::wl_data::{self, DataEvent, WlData};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Window, WindowId};

enum Ev {
    Data(DataEvent),
    Paste,
    Exit,
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

struct App {
    proxy: EventLoopProxy<Ev>,
    gfx: Option<Gfx>,
    wl: Option<WlData>,
    hover: bool,
    mods: ModifiersState,
}

fn describe(mime: &str, data: &[u8]) -> String {
    if mime == "text/uri-list" {
        let items = wl_data::parse_uri_list(data);
        let exist: Vec<bool> = items.iter().map(|p| std::path::Path::new(p).exists()).collect();
        format!("file list {items:?} exists_on_disk={exist:?}")
    } else if mime.starts_with("image/") {
        format!("image, {} bytes, magic {:02x?}", data.len(), &data[..data.len().min(8)])
    } else {
        format!("text {:?}", String::from_utf8_lossy(&data[..data.len().min(300)]))
    }
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title("shotgun S2 DnD / paste test")
                    .with_name("shotgun-s2-dnd", "shotgun-s2-dnd")
                    .with_inner_size(winit::dpi::LogicalSize::new(700, 400)),
            )
            .unwrap(),
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
            el.owned_display_handle(),
        )));
        let surface = instance.create_surface(window.clone()).unwrap();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        }))
        .unwrap();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let size = window.inner_size();
        let config = surface.get_default_config(&adapter, size.width.max(1), size.height.max(1)).unwrap();
        surface.configure(&device, &config);

        let (d, s) = wl_data::wayland_ptrs(&*window).expect("not running on Wayland");
        let proxy = self.proxy.clone();
        self.wl = Some(unsafe {
            WlData::spawn(d, s, move |ev| {
                let _ = proxy.send_event(Ev::Data(ev));
            })
        });
        window.request_redraw();
        self.gfx = Some(Gfx { window, surface, device, queue, config });
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        match ev {
            Ev::Exit => {
                if let Some(wl) = self.wl.take() {
                    wl.shutdown();
                }
                self.gfx = None;
                el.exit();
            }
            Ev::Paste => {
                println!("PASTE requested");
                if let Some(wl) = &self.wl {
                    wl.paste();
                }
            }
            Ev::Data(ev) => {
                match ev {
                    DataEvent::DragEnter { mimes, on_our_surface } => {
                        self.hover = true;
                        println!("DND enter on_our_surface={on_our_surface} mimes={mimes:?}");
                    }
                    DataEvent::DragMotion { .. } => return,
                    DataEvent::DragLeave => {
                        self.hover = false;
                        println!("DND leave");
                    }
                    DataEvent::Drop { mime, data, x, y } => {
                        self.hover = false;
                        println!("DND DROP at ({x:.0},{y:.0}) mime={mime}: {}", describe(&mime, &data));
                    }
                    DataEvent::Selection { mimes } => println!("CLIPBOARD changed, mimes={mimes:?}"),
                    DataEvent::Paste { mime, data } => {
                        println!("PASTE RESULT mime={mime}: {}", describe(&mime, &data))
                    }
                    DataEvent::PasteEmpty => println!("PASTE RESULT: clipboard empty or no usable type"),
                }
                if let Some(g) = &self.gfx {
                    g.window.request_redraw();
                }
            }
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(g) = self.gfx.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => {
                let _ = self.proxy.send_event(Ev::Exit);
            }
            WindowEvent::Resized(s) => {
                g.config.width = s.width.max(1);
                g.config.height = s.height.max(1);
                g.surface.configure(&g.device, &g.config);
                g.window.request_redraw();
            }
            WindowEvent::Focused(f) => println!("FOCUS {f}"),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event: k, .. } if k.state == ElementState::Pressed && !k.repeat => {
                println!("KEY physical={:?} logical={:?}", k.physical_key, k.logical_key);
                let paste = match k.physical_key {
                    PhysicalKey::Code(KeyCode::KeyV) => self.mods.control_key(),
                    PhysicalKey::Code(KeyCode::KeyP) => true,
                    _ => false,
                };
                if paste {
                    let _ = self.proxy.send_event(Ev::Paste);
                }
            }
            WindowEvent::RedrawRequested => {
                let frame = match g.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
                    _ => {
                        g.surface.configure(&g.device, &g.config);
                        g.window.request_redraw();
                        return;
                    }
                };
                let view = frame.texture.create_view(&Default::default());
                let mut enc = g.device.create_command_encoder(&Default::default());
                let c = if self.hover {
                    wgpu::Color { r: 0.05, g: 0.35, b: 0.1, a: 1.0 }
                } else {
                    wgpu::Color { r: 0.08, g: 0.08, b: 0.1, a: 1.0 }
                };
                enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(c), store: wgpu::StoreOp::Store },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                g.queue.submit([enc.finish()]);
                g.queue.present(frame);
            }
            _ => {}
        }
    }
}

fn main() {
    let mut secs = 120.0;
    let mut auto_paste = None;
    let v: Vec<String> = std::env::args().skip(1).collect();
    for w in v.chunks(2) {
        match w[0].as_str() {
            "--secs" => secs = w[1].parse().unwrap(),
            "--auto-paste" => auto_paste = Some(w[1].parse::<f64>().unwrap()),
            o => panic!("unknown arg {o}"),
        }
    }
    println!("Drop files on the window; Ctrl+V (or P) pastes the clipboard. Exits after {secs} s.");
    let el = EventLoop::<Ev>::with_user_event().build().unwrap();
    el.set_control_flow(ControlFlow::Wait);
    let proxy = el.create_proxy();
    {
        let proxy = proxy.clone();
        std::thread::spawn(move || {
            if let Some(s) = auto_paste {
                std::thread::sleep(Duration::from_secs_f64(s));
                let _ = proxy.send_event(Ev::Paste);
                std::thread::sleep(Duration::from_secs_f64((secs - s).max(0.0)));
            } else {
                std::thread::sleep(Duration::from_secs_f64(secs));
            }
            let _ = proxy.send_event(Ev::Exit);
        });
    }
    let mut app = App { proxy, gfx: None, wl: None, hover: false, mods: ModifiersState::empty() };
    el.run_app(&mut app).unwrap();
}
