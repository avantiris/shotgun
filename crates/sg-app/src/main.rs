//! Shotgun: wiring of window, UI and worker threads.
//!
//! The winit event loop is the UI thread. It never blocks: disk work runs on
//! worker threads and comes back as [`UserEvent`]s. Redraws happen only when
//! something asks for one (docs/spikes/s2-ui.md, "Event-driven redraw").

mod gfx;
mod jobs;
mod script;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use crossbeam_channel::Sender;
use egui_winit::winit;
use sg_project::{Opened, ProjectEntry, ProjectError};
use sg_ui::{Command, DevStep, Shell};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use crate::gfx::{Frame, Gfx};
use crate::jobs::Serial;
use crate::script::Step;

const USAGE: &str = "\
shotgun [options]
  --open <folder>      open the project in this library folder at start
  --script <steps>     run scripted steps, e.g. \"new:Demo;wait:500;rename:X;undo;close;quit\"
  --exit-after <secs>  quit after this many seconds
                       script steps: filter:<text> new:<name> open:<folder> rename:<name> undo redo close
                       perf esc wait:<ms> shot:<file.png> quit
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
            self.pending_jobs += 1;
            let proxy = self.proxy.clone();
            let library = self.library.clone();
            match command {
                Command::Create { name } => {
                    self.open_requested = Some(Instant::now());
                    self.io.run(move || {
                        let result = sg_project::create(&library, &name)
                            .and_then(|created| open_project(&created.path, &proxy))
                            .map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Opened(result));
                    });
                }
                Command::Open { path } => {
                    self.open_requested = Some(Instant::now());
                    self.io.run(move || {
                        let result = open_project(&path, &proxy).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Opened(result));
                    });
                }
                Command::Close { writer } => {
                    self.saw_editor_frame = false;
                    self.io.run(move || {
                        if let Some(thread) = writer.close() {
                            let _ = thread.join();
                        }
                        let _ = proxy.send_event(scan_library(&library));
                    });
                }
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

    fn quit(&mut self, el: &ActiveEventLoop) {
        // The surface must go before the event loop closes the display.
        self.gfx = None;
        el.exit();
    }

    fn screenshot(&mut self, path: &Path) {
        let Some(gfx) = &mut self.gfx else { return };
        let now_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let shell = &mut self.shell;
        let mut commands = Vec::new();
        match gfx.screenshot(&self.ctx, |ui| commands.extend(shell.frame(ui, now_unix)), path) {
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
        let Some(gfx) = &mut self.gfx else { return };
        let now_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let shell = &mut self.shell;
        let mut commands = Vec::new();
        let frame = gfx.render(&self.ctx, |ui| commands.extend(shell.frame(ui, now_unix)));
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
                self.shell.opened(result);
                self.job_done();
            }
            UserEvent::Script(step, ack) => self.script_step(step, ack),
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
    let App { mut shell, io, init_error, frames, redraws, .. } = app;
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
