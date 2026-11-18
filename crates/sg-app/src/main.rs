//! Shotgun: wiring of window, UI and worker threads.
//!
//! The winit event loop is the UI thread. It never blocks: disk work runs on
//! worker threads and comes back as [`UserEvent`]s. Redraws happen only when
//! something asks for one (docs/spikes/s2-ui.md, "Event-driven redraw").

mod gfx;
mod jobs;
mod script;
mod wl_data;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use crossbeam_channel::Sender;
use egui_winit::winit;
use sg_core::AssetId;
use sg_play::{Backend, Player};
use sg_project::{ImportEvent, Importer, Opened, ProjectEntry, ProjectError};
use sg_ui::{Command, DevStep, Incoming, Shell, Transport, ViewerState};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::gfx::{Frame, Gfx};
use crate::jobs::Serial;
use crate::script::Step;
use crate::wl_data::{DataEvent, WlData};

const USAGE: &str = "\
shotgun [options]
  --open <folder>      open the project in this library folder at start
  --script <steps>     run scripted steps, e.g. \"new:Demo;wait:500;rename:X;undo;close;quit\"
  --exit-after <secs>  quit after this many seconds
                       script steps: filter:<text> new:<name> open:<folder> rename:<name> undo redo close
                       import:<path or link> asset:<n> add del perf esc wait:<ms> shot:<file.png> quit
  --bench-start        print the time to the first library frame and quit
  --bench-open         with --open: print the time to the first editor frame and quit
Environment: SHOTGUN_LIBRARY (library folder), SHOTGUN_APP_ID (Wayland app id), SHOTGUN_LOG (error..trace)";

#[derive(Default)]
struct Args {
    open: Option<String>,
    script: Vec<Step>,
    exit_after: Option<f64>,
    bench_start: bool,
    bench_open: bool,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--open" => args.open = Some(value()?),
            "--script" => args.script = script::parse(&value()?)?,
            "--exit-after" => args.exit_after = Some(value()?.parse().context("--exit-after needs seconds")?),
            "--bench-start" => args.bench_start = true,
            "--bench-open" => args.bench_open = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    Ok(args)
}

enum UserEvent {
    /// egui wants a repaint after this delay.
    Repaint(Duration),
    /// A worker changed something visible.
    Wake,
    Library(Result<Vec<ProjectEntry>, String>),
    Opened(Result<Opened, String>),
    /// A scripted step and the channel that tells the script thread it is done.
    Script(Step, Sender<()>),
    Import(ImportEvent),
    Poster {
        root: PathBuf,
        id: AssetId,
        rgba: Option<Vec<u8>>,
    },
    /// Drag-and-drop or clipboard data from the Wayland data device.
    Data(DataEvent),
    /// Files chosen in the file picker.
    Picked(Vec<PathBuf>),
    Exit,
}

struct App {
    args: Args,
    started: Instant,
    proxy: EventLoopProxy<UserEvent>,
    ctx: egui::Context,
    shell: Shell,
    gfx: Option<Gfx>,
    io: Serial,
    /// Small reads for the UI (posters), kept off the project I/O queue.
    reads: Serial,
    importer: Option<Importer>,
    /// Running imports and when each was requested.
    imports: HashMap<u64, Instant>,
    wl: Option<WlData>,
    /// The preview engine; started once the GPU device exists.
    player: Option<Player>,
    /// The viewer texture as registered with the UI renderer, and the
    /// player's output generation it belongs to.
    viewer_texture: Option<(egui::TextureId, u64)>,
    /// The snapshot the player was last given.
    synced: Option<(PathBuf, std::sync::Arc<sg_core::Project>)>,
    library: PathBuf,
    repaint_at: Option<Instant>,
    /// Jobs sent to workers whose result has not come back yet.
    pending_jobs: usize,
    /// A scripted step waiting for its jobs to finish.
    script: Option<ScriptWait>,
    open_requested: Option<Instant>,
    saw_library_frame: bool,
    saw_editor_frame: bool,
    frames: u64,
    redraws: u64,
    init_error: Option<anyhow::Error>,
}

fn open_project(path: &Path, proxy: &EventLoopProxy<UserEvent>) -> Result<Opened, ProjectError> {
    let proxy = proxy.clone();
    sg_project::open(path, move || {
        let _ = proxy.send_event(UserEvent::Wake);
    })
}

fn scan_library(library: &Path) -> UserEvent {
    UserEvent::Library(sg_project::scan(library).map_err(|e| e.to_string()))
}

impl App {
    fn request_redraw(&self) {
        if let Some(gfx) = &self.gfx {
            gfx.window.request_redraw();
        }
    }

    fn run_commands(&mut self, commands: Vec<Command>) {
        for command in commands {
            let proxy = self.proxy.clone();
            let library = self.library.clone();
            match command {
                Command::Create { name } => {
                    self.pending_jobs += 1;
                    self.open_requested = Some(Instant::now());
                    self.io.run(move || {
                        let result = sg_project::create(&library, &name)
                            .and_then(|created| open_project(&created.path, &proxy))
                            .map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Opened(result));
                    });
                }
                Command::Open { path } => {
                    self.pending_jobs += 1;
                    self.open_requested = Some(Instant::now());
                    self.io.run(move || {
                        let result = open_project(&path, &proxy).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Opened(result));
                    });
                }
                Command::Close { writer } => {
                    self.pending_jobs += 1;
                    self.saw_editor_frame = false;
                    self.io.run(move || {
                        if let Some(thread) = writer.close() {
                            let _ = thread.join();
                        }
                        let _ = proxy.send_event(scan_library(&library));
                    });
                }
                Command::Import { root, known, source } => {
                    if let Some(importer) = &self.importer {
                        self.pending_jobs += 1;
                        self.imports.insert(importer.import(root, known, source), Instant::now());
                    }
                }
                Command::RestoreCache { root, assets } => {
                    if let Some(importer) = &self.importer {
                        self.pending_jobs += 1;
                        self.imports.insert(importer.restore_cache(root, assets), Instant::now());
                    }
                }
                Command::CancelImport(job) => {
                    if let Some(importer) = &self.importer {
                        importer.cancel(job);
                    }
                }
                Command::LoadPoster { root, id } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let rgba = sg_project::read_poster(&root, id).ok();
                        let _ = proxy.send_event(UserEvent::Poster { root, id, rgba });
                    });
                }
                Command::PickFiles => {
                    // The portal dialog is another process' window; waiting for it
                    // happens on its own thread.
                    let spawned = std::thread::Builder::new().name("sg-file-picker".into()).spawn(move || {
                        let picked =
                            pollster::block_on(rfd::AsyncFileDialog::new().set_title("Add assets").pick_files());
                        let paths = picked.unwrap_or_default().iter().map(|f| f.path().to_owned()).collect();
                        let _ = proxy.send_event(UserEvent::Picked(paths));
                    });
                    if let Err(e) = spawned {
                        self.shell.report_error(format!("Cannot open the file picker: {e}"));
                    }
                }
                Command::Transport(transport) => {
                    if let Some(player) = &self.player {
                        match transport {
                            Transport::Toggle => player.toggle(),
                            Transport::Seek(frame) => player.seek(frame),
                            Transport::Step(frames) => player.step(frames),
                        }
                    }
                }
                Command::ReadClipboard => match &self.wl {
                    Some(wl) => wl.paste(),
                    None => self.shell.report_error("The clipboard is not available.".to_owned()),
                },
            }
        }
    }

    fn job_done(&mut self) {
        self.pending_jobs = self.pending_jobs.saturating_sub(1);
        if self.pending_jobs == 0 {
            match self.script.take() {
                Some(ScriptWait::Finish(ack)) => {
                    let _ = ack.send(());
                }
                Some(ScriptWait::Start(step, ack)) => self.script_step(step, ack),
                None => {}
            }
        }
        self.request_redraw();
    }

    /// Runs a scripted step once no job is pending, and acknowledges it
    /// once the jobs it started have finished. A script therefore sees the
    /// app in the state a user would wait for (library read, project open).
    fn script_step(&mut self, step: Step, ack: Sender<()>) {
        if self.pending_jobs > 0 {
            self.script = Some(ScriptWait::Start(step, ack));
            return;
        }
        match step {
            Step::Dev(dev) => {
                let commands = self.shell.dev(dev);
                self.run_commands(commands);
            }
            Step::Shot(path) => self.screenshot(&path),
            // Handled by the script thread.
            Step::WaitMs(_) | Step::Quit => {}
        }
        if self.pending_jobs == 0 {
            let _ = ack.send(());
        } else {
            self.script = Some(ScriptWait::Finish(ack));
        }
        self.request_redraw();
    }

    fn incoming(&mut self, incoming: Incoming) {
        let commands = self.shell.incoming(incoming);
        self.run_commands(commands);
        self.request_redraw();
    }

    fn data_event(&mut self, event: DataEvent) {
        match event {
            DataEvent::DragEnter { on_our_surface, .. } => self.shell.set_drag_hover(on_our_surface),
            DataEvent::DragLeave => self.shell.set_drag_hover(false),
            DataEvent::DragMotion { .. } | DataEvent::Selection { .. } => return,
            DataEvent::Drop { mime, data, .. } | DataEvent::Paste { mime, data } => match incoming_from(&mime, data) {
                Some(incoming) => return self.incoming(incoming),
                None => self.shell.report_error("Nothing in there that can be imported.".to_owned()),
            },
            DataEvent::PasteEmpty => self.shell.report_error("The clipboard is empty.".to_owned()),
        }
        self.request_redraw();
    }

    fn import_event(&mut self, event: ImportEvent) {
        match &event {
            ImportEvent::Usable { job, asset, .. } => {
                if let Some(started) = self.imports.get(job) {
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    log::info!("asset usable {ms:.0} ms after the request: {} ({} bytes)", asset.label, asset.size);
                }
            }
            ImportEvent::Finished { job } if self.imports.remove(job).is_some() => {
                self.shell.import_event(event);
                return self.job_done();
            }
            _ => {}
        }
        if let (ImportEvent::Derived { id, what, .. }, Some(player)) = (&event, &self.player) {
            // New audio, proxy or conform: the player reopens what it had.
            if !matches!(what, sg_project::Derived::Poster | sg_project::Derived::Peaks | sg_project::Derived::Scan) {
                player.asset_changed(*id);
            }
        }
        self.shell.import_event(event);
        self.request_redraw();
    }

    /// Gives the player the current snapshot if it changed. A pointer
    /// comparison; called after every batch of events.
    fn sync_player(&mut self) {
        let Some(player) = &self.player else { return };
        let current = self.shell.session().map(|s| (s.root.clone(), s.project().clone()));
        let same = match (&self.synced, &current) {
            (Some((root, project)), Some((new_root, new_project))) => {
                root == new_root && std::sync::Arc::ptr_eq(project, new_project)
            }
            (None, None) => true,
            _ => false,
        };
        if !same {
            match &current {
                Some((root, project)) => player.set_project(root.clone(), project.clone()),
                None => player.clear_project(),
            }
            self.synced = current;
        }
    }

    /// What the viewer shows this frame.
    fn viewer_state(&mut self) -> ViewerState {
        let (Some(player), Some(gfx)) = (&self.player, &mut self.gfx) else { return ViewerState::default() };
        let (view, generation) = player.output();
        let texture = match self.viewer_texture {
            Some((id, seen)) if seen == generation => id,
            Some((id, _)) => {
                gfx.replace_texture(id, &view);
                id
            }
            None => gfx.register_texture(&view),
        };
        self.viewer_texture = Some((texture, generation));
        ViewerState {
            texture: Some(texture),
            frame: player.frame(),
            playing: player.is_playing(),
            stats: player_lines(player),
        }
    }

    fn quit(&mut self, el: &ActiveEventLoop) {
        // The player holds the GPU device: it goes first.
        if let Some(player) = self.player.take() {
            for line in player_lines(&player) {
                log::info!("{line}");
            }
        }
        // The data device and the surface must go before the event loop closes
        // the display.
        if let Some(wl) = self.wl.take() {
            wl.shutdown();
        }
        self.gfx = None;
        el.exit();
    }

    fn screenshot(&mut self, path: &Path) {
        self.sync_player();
        let view = self.viewer_state();
        let Some(gfx) = &mut self.gfx else { return };
        let now_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let shell = &mut self.shell;
        let mut commands = Vec::new();
        match gfx.screenshot(&self.ctx, |ui| commands.extend(shell.frame(ui, now_unix, &view)), path) {
            Ok(()) => log::info!("wrote {}", path.display()),
            Err(e) => log::error!("screenshot failed: {e:#}"),
        }
        self.run_commands(commands);
    }

    fn redraw(&mut self, el: &ActiveEventLoop) {
        // A hidden window gets no second frame from the compositor, so the open
        // benchmark makes the editor its first frame instead of the library.
        if self.args.bench_open && self.shell.session().is_none() {
            return;
        }
        self.redraws += 1;
        let view = self.viewer_state();
        let Some(gfx) = &mut self.gfx else { return };
        let now_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let shell = &mut self.shell;
        let mut commands = Vec::new();
        let frame = gfx.render(&self.ctx, |ui| commands.extend(shell.frame(ui, now_unix, &view)));
        if let Frame::Presented { cpu_ms } = frame {
            self.shell.perf_mut().push_frame(cpu_ms);
            self.frames += 1;
            let since_start = self.started.elapsed().as_secs_f64() * 1000.0;
            if !self.saw_library_frame && self.shell.library_loaded() {
                self.saw_library_frame = true;
                log::info!("library on screen {since_start:.1} ms after start");
                if self.args.bench_start {
                    println!("BENCH library_frame_ms={since_start:.1}");
                    self.quit(el);
                    return;
                }
            }
            if !self.saw_editor_frame && self.shell.session().is_some() {
                self.saw_editor_frame = true;
                let open_ms = self.open_requested.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
                log::info!("editor on screen {open_ms:.1} ms after the open request");
                if self.args.bench_open {
                    println!("BENCH editor_frame_ms={since_start:.1} open_ms={open_ms:.1}");
                    self.quit(el);
                    return;
                }
            }
        }
        if !commands.is_empty() {
            self.run_commands(commands);
            self.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let app_id = std::env::var("SHOTGUN_APP_ID").unwrap_or_else(|_| "shotgun".to_owned());
        match Gfx::new(el, &self.ctx, &app_id) {
            Ok(gfx) => {
                if let Some((display, surface)) = wl_data::wayland_ptrs(&*gfx.window) {
                    let proxy = self.proxy.clone();
                    let sink = move |event| {
                        let _ = proxy.send_event(UserEvent::Data(event));
                    };
                    // SAFETY: the pointers come from the live winit window, and `quit`
                    // shuts the data thread down before the event loop is dropped.
                    match unsafe { WlData::spawn(display, surface, sink) } {
                        Ok(wl) => self.wl = Some(wl),
                        Err(e) => log::warn!("drag-and-drop and paste are unavailable: {e}"),
                    }
                }
                let (device, queue) = gfx.device();
                let proxy = self.proxy.clone();
                let wake = move || {
                    let _ = proxy.send_event(UserEvent::Wake);
                };
                // Sound is the clock. Without a device the null backend keeps time.
                let player = Player::start(device.clone(), queue.clone(), audio_backend(), wake.clone()).or_else(|e| {
                    log::warn!("no audio output ({e}); playing silently");
                    Player::start(device, queue, Backend::Null, wake)
                });
                match player {
                    Ok(player) => self.player = Some(player),
                    Err(e) => log::error!("the preview engine did not start: {e}"),
                }
                gfx.window.request_redraw();
                self.gfx = Some(gfx);
            }
            Err(e) => {
                self.init_error = Some(e);
                el.exit();
            }
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Repaint(delay) => {
                if delay.is_zero() {
                    self.request_redraw();
                } else if let Some(at) = Instant::now().checked_add(delay) {
                    self.repaint_at = Some(self.repaint_at.map_or(at, |t| t.min(at)));
                }
            }
            UserEvent::Wake => self.request_redraw(),
            UserEvent::Library(result) => {
                self.shell.set_library(result);
                self.job_done();
                if let Some(folder) = self.args.open.take() {
                    let commands = self.shell.dev(DevStep::Open(folder));
                    self.run_commands(commands);
                }
            }
            UserEvent::Opened(result) => {
                let commands = self.shell.opened(result);
                self.run_commands(commands);
                self.job_done();
            }
            UserEvent::Script(step, ack) => self.script_step(step, ack),
            UserEvent::Import(event) => self.import_event(event),
            UserEvent::Poster { root, id, rgba } => {
                self.shell.poster_loaded(&self.ctx, &root, id, rgba);
                self.job_done();
            }
            UserEvent::Data(event) => self.data_event(event),
            UserEvent::Picked(paths) => {
                if !paths.is_empty() {
                    self.incoming(Incoming::Paths(paths));
                }
            }
            UserEvent::Exit => self.quit(el),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::RedrawRequested => return self.redraw(el),
            WindowEvent::CloseRequested => return self.quit(el),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = &mut self.gfx {
                    gfx.resize(size.width, size.height);
                }
            }
            _ => {}
        }
        let Some(gfx) = &mut self.gfx else { return };
        if gfx.egui_state.on_window_event(&gfx.window, &event).repaint {
            gfx.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.sync_player();
        match self.repaint_at {
            Some(at) if Instant::now() >= at => {
                self.repaint_at = None;
                self.request_redraw();
                el.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => el.set_control_flow(ControlFlow::WaitUntil(at)),
            None => el.set_control_flow(ControlFlow::Wait),
        }
    }
}

struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        // wgpu and winit are chatty at info level.
        metadata.level() <= log::Level::Warn
            || metadata.target().starts_with("sg_")
            || metadata.target().starts_with("shotgun")
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("[{:<5}] {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

fn spawn_script(steps: Vec<Step>, proxy: EventLoopProxy<UserEvent>) -> std::io::Result<()> {
    std::thread::Builder::new().name("sg-script".into()).spawn(move || {
        for step in steps {
            match step {
                Step::WaitMs(ms) => std::thread::sleep(Duration::from_millis(ms)),
                Step::Quit => {
                    let _ = proxy.send_event(UserEvent::Exit);
                    return;
                }
                step => {
                    let (ack, done) = crossbeam_channel::bounded(1);
                    if proxy.send_event(UserEvent::Script(step, ack)).is_err() || done.recv().is_err() {
                        return;
                    }
                }
            }
        }
    })?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let started = Instant::now();
    let mut args = parse_args()?;
    let level = std::env::var("SHOTGUN_LOG").ok().and_then(|l| l.parse().ok()).unwrap_or(log::LevelFilter::Info);
    // Setting the logger only fails if one is already set, which cannot be the case here.
    let _ = log::set_logger(&StderrLog);
    log::set_max_level(level);

    let event_loop = EventLoop::<UserEvent>::with_user_event().build().context("connecting to the Wayland display")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let proxy = event_loop.create_proxy();

    // The library is read while the window and GPU come up.
    let library = sg_project::library_dir();
    let io = Serial::spawn("sg-project-io")?;
    let importer = Importer::new({
        let proxy = proxy.clone();
        move |event| {
            let _ = proxy.send_event(UserEvent::Import(event));
        }
    })?;
    io.run({
        let (proxy, library) = (proxy.clone(), library.clone());
        move || {
            let _ = proxy.send_event(scan_library(&library));
        }
    });

    let ctx = egui::Context::default();
    sg_ui::theme::apply(&ctx);
    ctx.set_request_repaint_callback({
        let proxy = proxy.clone();
        move |info| {
            let _ = proxy.send_event(UserEvent::Repaint(info.delay));
        }
    });

    spawn_script(std::mem::take(&mut args.script), proxy.clone())?;
    if let Some(secs) = args.exit_after {
        let proxy = proxy.clone();
        std::thread::Builder::new().name("sg-exit-timer".into()).spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(secs));
            let _ = proxy.send_event(UserEvent::Exit);
        })?;
    }

    let mut app = App {
        args,
        started,
        proxy,
        ctx,
        shell: Shell::new(),
        gfx: None,
        io,
        reads: Serial::spawn("sg-reads")?,
        importer: Some(importer),
        imports: HashMap::new(),
        wl: None,
        player: None,
        viewer_texture: None,
        synced: None,
        library,
        repaint_at: None,
        pending_jobs: 1,
        script: None,
        open_requested: None,
        saw_library_frame: false,
        saw_editor_frame: false,
        frames: 0,
        redraws: 0,
        init_error: None,
    };
    event_loop.run_app(&mut app).context("event loop")?;

    // The window is gone; finish what is still being written.
    let App { mut shell, io, reads, importer, init_error, frames, redraws, .. } = app;
    log::info!(
        "presented {frames} frames ({redraws} redraw requests) in {:.1} s, process CPU {:.2} s",
        started.elapsed().as_secs_f64(),
        process_cpu_seconds().unwrap_or(f64::NAN)
    );
    if let Some(writer) = shell.take_writer()
        && let Some(thread) = writer.close()
    {
        let _ = thread.join();
    }
    io.finish();
    reads.finish();
    if let Some(importer) = importer {
        importer.shutdown();
    }
    init_error.map_or(Ok(()), Err)
}

/// User plus system CPU time of this process, for the idle check in the log.
fn process_cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the command name, which may itself contain spaces.
    let mut fields = stat.rsplit_once(')')?.1.split_ascii_whitespace();
    let utime: f64 = fields.nth(11)?.parse().ok()?;
    let stime: f64 = fields.next()?.parse().ok()?;
    // USER_HZ is 100 on Linux.
    Some((utime + stime) / 100.0)
}

/// Where a scripted step is waiting.
enum ScriptWait {
    /// Not started: a job was still running when it arrived.
    Start(Step, Sender<()>),
    /// Started: waiting for the jobs it caused.
    Finish(Sender<()>),
}

/// Turns dropped or pasted data into something to import.
fn incoming_from(mime: &str, data: Vec<u8>) -> Option<Incoming> {
    if mime == "text/uri-list" {
        let entries = wl_data::parse_uri_list(&data);
        let paths: Vec<PathBuf> = entries.iter().filter(|e| e.starts_with('/')).map(PathBuf::from).collect();
        if !paths.is_empty() {
            return Some(Incoming::Paths(paths));
        }
        // A link dragged from a browser arrives as a one-line URI list.
        return entries.into_iter().next().map(Incoming::Text);
    }
    if mime.starts_with("image/") {
        return Some(Incoming::Image { data, mime: mime.to_owned() });
    }
    let text = String::from_utf8_lossy(&data).trim().to_owned();
    // File managers also offer copied files as plain paths.
    if text.starts_with('/') && text.lines().all(|l| Path::new(l.trim()).exists()) {
        return Some(Incoming::Paths(text.lines().map(|l| PathBuf::from(l.trim())).collect()));
    }
    (!text.is_empty()).then_some(Incoming::Text(text))
}

/// `SHOTGUN_AUDIO=null` plays without a device (automated runs),
/// `SHOTGUN_AUDIO=sink:<node name>` targets a specific sink.
fn audio_backend() -> Backend {
    match std::env::var("SHOTGUN_AUDIO").ok().as_deref() {
        Some("null") => Backend::Null,
        Some(other) => Backend::PipeWire {
            target: other.strip_prefix("sink:").map(str::to_owned),
            quantum: sg_play::DEFAULT_QUANTUM,
        },
        None => Backend::PipeWire { target: None, quantum: sg_play::DEFAULT_QUANTUM },
    }
}

fn percentile(samples: &[f32], q: f32) -> f32 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f32::total_cmp);
    sorted.get(((sorted.len().max(1) - 1) as f32 * q).round() as usize).copied().unwrap_or(0.0)
}

/// Preview numbers for the performance overlay and the exit log.
fn player_lines(player: &Player) -> Vec<String> {
    let s = player.stats();
    let line = |name: &str, v: &[f32]| {
        format!(
            "{name} p50 {:.1}  p95 {:.1}  max {:.1} ms  (n={})",
            percentile(v, 0.5),
            percentile(v, 0.95),
            percentile(v, 1.0),
            v.len()
        )
    };
    let mut lines = vec![
        line("seek proxy", &s.seek_ms),
        line("seek full ", &s.full_ms),
        line("play start", &s.play_start_ms),
        format!("playback   {} frames, {} late   cache {} MB", s.played, s.dropped, s.cache_bytes >> 20),
        format!(
            "audio      quantum {}  delay {}  xruns {}  underruns {}  rt policy {}",
            s.audio.quantum, s.audio.delay_frames, s.audio.xruns, s.audio.underruns, s.audio.sched_policy
        ),
    ];
    lines.extend(s.decode_error.map(|e| format!("decode error: {e}")));
    lines
}
