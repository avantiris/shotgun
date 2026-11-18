//! s1 play <file> <dmabuf|vkdec|cpu-vaapi|cpu-vulkan|cpu-sw> [secs=10]
//! Minimal winit + wgpu window: plays the file against the wall clock and logs
//! frame pacing. Decode runs on its own thread; import + render on the main thread.
use crate::dec::*;
use crate::gpu::*;
use ffmpeg_next::ffi::*;
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct Decoded {
    frame: FrameGuard,
    pts_secs: f64,
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    gpu: Gpu,
    renderer: Renderer,
    importer: Importer,
    rx: Receiver<Decoded>,
    pending: Option<Decoded>,
    current: Option<(wgpu::BindGroup, FrameGuard)>,
    start: Option<Instant>,
    fps: f64,
    // stats
    last_present: Option<Instant>,
    last_new_present: Option<Instant>,
    present_intervals: Vec<f64>,
    new_frame_intervals: Vec<f64>,
    import_ms: Vec<f64>,
    shown: u64,
    dropped: u64,
    underruns: u64,
    repeats: u64,
    late_ms: Vec<f64>,
    decode_done: bool,
    long_gaps: Vec<(f64, f64)>,
    warm: bool,
}

struct App {
    file: String,
    path: Path,
    secs: f64,
    state: Option<State>,
    instance: Option<wgpu::Instance>,
}

fn spawn_decoder(file: String, path: Path, dev: Option<HwDevice>, secs: f64) -> (Receiver<Decoded>, f64, u32, u32) {
    let (tx, rx) = sync_channel::<Decoded>(4);
    let (info_tx, info_rx) = sync_channel(1);
    std::thread::Builder::new()
        .name("s1-decode".into())
        .spawn(move || {
            let mut d = Decoder::open(&file, path.hw(), dev.as_ref(), 0);
            info_tx.send((d.fps, d.width, d.height)).unwrap();
            let mut loops = 0f64;
            let dur = d.ts_to_secs(d.duration_ts);
            loop {
                while d.next_frame() {
                    let pts_secs = d.ts_to_secs(d.frame_pts() - d.start_ts) + loops * dur;
                    let frame = match path {
                        Path::Dmabuf | Path::DmabufNoCache => {
                            d.sync_vaapi();
                            unsafe { av_frame_clone(d.frame) }
                        }
                        Path::VkDec => unsafe { av_frame_clone(d.frame) },
                        _ => unsafe { av_frame_clone(d.download()) },
                    };
                    if tx.send(Decoded { frame: FrameGuard(frame), pts_secs }).is_err() {
                        return;
                    }
                    if pts_secs > secs + 2.5 {
                        return;
                    }
                }
                // loop the file if the run is longer than the clip
                loops += 1.0;
                d.seek_keyframe(d.start_ts);
            }
        })
        .unwrap();
    let (fps, w, h) = info_rx.recv().unwrap();
    (rx, fps, w, h)
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title("shotgun spike S1")
                    .with_inner_size(winit::dpi::PhysicalSize::new(1920, 1080)),
            )
            .unwrap(),
        );
        let instance = self.instance.take().unwrap();
        let surface = instance.create_surface(window.clone()).expect("surface");
        let gpu = Gpu::new(instance, Some(&surface), self.path.needs_video_device());
        let caps = surface.get_capabilities(&gpu.adapter);
        // shader outputs display-referred R'G'B': use a non-sRGB format so it is not re-encoded
        let format = pick_format(&caps.formats);
        let size = window.inner_size();
        let mut cfg = surface.get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1)).unwrap();
        cfg.format = format;
        cfg.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&gpu.device, &cfg);
        let refresh = window.current_monitor().and_then(|m| m.refresh_rate_millihertz());
        let dev = match self.path {
            Path::VkDec => Some(gpu.ffmpeg_vulkan_device()),
            Path::CpuSw => None,
            p => Some(HwDevice::create(p.hw())),
        };
        let (rx, fps, w, h) = spawn_decoder(self.file.clone(), self.path, dev, self.secs);
        println!(
            "play file={} path={:?} video={}x{}@{:.2} window={}x{} surface_format={:?} present_mode=Fifo monitor_refresh={:?} mHz present_modes={:?}",
            crate::file_label(&self.file), self.path, w, h, fps, size.width, size.height, format, refresh, caps.present_modes
        );
        let renderer = Renderer::new(&gpu, format);
        self.state = Some(State {
            window: window.clone(),
            surface,
            renderer,
            importer: Importer::new(self.path),
            gpu,
            rx,
            pending: None,
            current: None,
            start: None,
            fps,
            last_present: None,
            last_new_present: None,
            present_intervals: vec![],
            new_frame_intervals: vec![],
            import_ms: vec![],
            shown: 0,
            dropped: 0,
            underruns: 0,
            repeats: 0,
            late_ms: vec![],
            decode_done: false,
            long_gaps: vec![],
            warm: false,
        });
        window.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(s) = self.state.as_mut() else { return };
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                let mut cfg = s.surface.get_default_config(&s.gpu.adapter, size.width.max(1), size.height.max(1)).unwrap();
                let caps = s.surface.get_capabilities(&s.gpu.adapter);
                cfg.format = pick_format(&caps.formats);
                cfg.present_mode = wgpu::PresentMode::Fifo;
                s.surface.configure(&s.gpu.device, &cfg);
                println!("resized to {}x{}", size.width, size.height);
            }
            WindowEvent::RedrawRequested => {
                s.redraw();
                // 1 s warm-up (window map, surface reconfigure, first imports) is excluded from the stats
                if !s.warm && s.start.is_some_and(|t| t.elapsed().as_secs_f64() >= 1.0) {
                    s.reset_stats();
                }
                if s.start.is_some_and(|t| t.elapsed().as_secs_f64() >= self.secs + 1.0) {
                    s.report();
                    el.exit();
                    // skip teardown ordering problems in throwaway code
                    std::process::exit(0);
                }
                s.window.request_redraw();
            }
            _ => {}
        }
    }
}

impl State {
    fn redraw(&mut self) {
        // Pick the newest frame that is due. The clock starts when the first frame is available.
        if self.pending.is_none() {
            self.pending = self.rx.try_recv().ok();
        }
        if self.start.is_none() && self.pending.is_some() {
            self.start = Some(Instant::now());
        }
        let now = self.start.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
        let frame_dur = 1.0 / self.fps;
        let mut chosen: Option<Decoded> = None;
        loop {
            match &self.pending {
                Some(p) if p.pts_secs <= now => {
                    if chosen.take().is_some() {
                        self.dropped += 1; // an older due frame is skipped: we are late
                    }
                    chosen = self.pending.take();
                    match self.rx.try_recv() {
                        Ok(f) => self.pending = Some(f),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            self.decode_done = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        if let Some(c) = chosen {
            let lateness = now - c.pts_secs;
            self.late_ms.push(lateness * 1000.0);
            let t = Instant::now();
            let bg = self.importer.import_frame(&self.gpu, &mut self.renderer, c.frame.0);
            self.import_ms.push(t.elapsed().as_secs_f64() * 1000.0);
            self.current = Some((bg, c.frame));
            self.shown += 1;
            let n = Instant::now();
            if let Some(l) = self.last_new_present {
                let iv = (n - l).as_secs_f64() * 1000.0;
                self.new_frame_intervals.push(iv);
                if iv > 1500.0 / self.fps {
                    self.long_gaps.push((now, iv));
                }
            }
            self.last_new_present = Some(n);
        } else if self.start.is_some() {
            // nothing new to show at this refresh
            if self.pending.is_none() && !self.decode_done && self.last_new_present.is_some_and(|l| l.elapsed().as_secs_f64() > frame_dur) {
                self.underruns += 1; // a frame was due but the decoder had not delivered it
            }
            self.repeats += 1;
        }
        let Some((bg, _)) = &self.current else {
            std::thread::sleep(Duration::from_millis(1));
            return;
        };
        let st = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            other => {
                eprintln!("surface: {:?}", std::mem::discriminant(&other));
                std::thread::sleep(Duration::from_millis(2));
                return;
            }
        };
        let view = st.texture.create_view(&Default::default());
        let mut enc = self.gpu.device.create_command_encoder(&Default::default());
        self.renderer.draw(&mut enc, &view, bg);
        self.gpu.queue.submit([enc.finish()]);
        self.gpu.queue.present(st);
        let n = Instant::now();
        if let Some(l) = self.last_present {
            self.present_intervals.push((n - l).as_secs_f64() * 1000.0);
        }
        self.last_present = Some(n);
    }

    fn reset_stats(&mut self) {
        self.warm = true;
        self.present_intervals.clear();
        self.new_frame_intervals.clear();
        self.late_ms.clear();
        self.import_ms.clear();
        self.long_gaps.clear();
        (self.shown, self.dropped, self.underruns, self.repeats) = (0, 0, 0, 0);
    }

    fn report(&mut self) {
        let stat = |v: &mut Vec<f64>| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.is_empty() {
                return "n=0".to_string();
            }
            format!(
                "n={} mean={:.2} p50={:.2} p95={:.2} p99={:.2} max={:.2}",
                v.len(),
                v.iter().sum::<f64>() / v.len() as f64,
                percentile(v, 0.5),
                percentile(v, 0.95),
                percentile(v, 0.99),
                v[v.len() - 1]
            )
        };
        let elapsed = self.start.unwrap().elapsed().as_secs_f64() - 1.0;
        let expected = (elapsed * self.fps).floor() as u64;
        println!("  elapsed={:.2}s video_frames_due={} shown={} dropped(skipped late)={} decoder_underruns={} refreshes_without_new_frame={}", elapsed, expected, self.shown, self.dropped, self.underruns, self.repeats);
        println!("  present interval ms (every swap):        {}", stat(&mut self.present_intervals));
        println!("  new-video-frame interval ms:              {}", stat(&mut self.new_frame_intervals));
        println!("  frame lateness at pick ms (now - pts):    {}", stat(&mut self.late_ms));
        println!("  import cost on render thread ms:          {}", stat(&mut self.import_ms));
        let over = self.new_frame_intervals.iter().filter(|&&x| x > 1500.0 / self.fps).count();
        println!("  new-frame intervals > 1.5x frame duration: {} at (t_s, ms): {:?}", over, self.long_gaps.iter().map(|(t, i)| format!("{t:.2}s:{i:.1}")).collect::<Vec<_>>());
    }
}

pub fn cmd_play(args: &[String]) {
    let file = args[2].clone();
    let path = Path::parse(&args[3]);
    let secs: f64 = crate::arg_kv(args, "secs").map(|s| s.parse().unwrap()).unwrap_or(10.0);
    let el = EventLoop::new().unwrap();
    el.set_control_flow(ControlFlow::Poll);
    let mut d = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(el.owned_display_handle()));
    d.backends = wgpu::Backends::VULKAN;
    d.flags = wgpu::InstanceFlags::empty();
    let instance = wgpu::Instance::new(d);
    let mut app = App { file, path, secs, state: None, instance: Some(instance) };
    el.run_app(&mut app).unwrap();
}

/// 8-bit non-sRGB format: the shader outputs display-referred R'G'B' which must
/// not be re-encoded (an Rgba16Float surface would be treated as linear scRGB).
fn pick_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    for want in [wgpu::TextureFormat::Bgra8Unorm, wgpu::TextureFormat::Rgba8Unorm] {
        if formats.contains(&want) {
            return want;
        }
    }
    formats[0]
}
