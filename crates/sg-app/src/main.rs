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
use sg_ui::{Command, DevStep, ExportPreset, ExportRequest, ExportUpdate, Incoming, Shell, Transport, ViewerState};
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
                       size:<w>x<h> exportpanel export:<master|vertical|gif|share:<MB>> (see script.rs for all)
  --bench-start        print the time to the first library frame and quit
  --bench-open         with --open: print the time to the first editor frame and quit
Environment: SHOTGUN_LIBRARY (library folder), SHOTGUN_KEYS (key file instead of ~/.config/shotgun/keys.conf), SHOTGUN_APP_ID (Wayland app id), SHOTGUN_LOG (error..trace),
  SHOTGUN_AUDIO (null | sink:<name>), SHOTGUN_NO_CLIPBOARD (exports leave the clipboard alone)";

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
    /// The inputs sound can be recorded from, (name, label).
    Inputs(Vec<(String, String)>),
    /// A key of a MIDI keyboard went down (true) or up: pitch and velocity.
    Midi(bool, u8, u8),
    /// The settings of this machine, read at start.
    Settings(sg_project::settings::Settings),
    /// News from a plugin job.
    Plugins(sg_ui::PluginUpdate),
    /// A track's voice was listened to.
    Voice {
        root: PathBuf,
        track: sg_core::TrackId,
        style: Option<sg_media::voice::VoiceStyle>,
        result: Result<sg_media::voice::VoiceStats, String>,
        /// The settings that were found for it.
        fitted: Option<sg_media::voice::Suggestion>,
        /// How loud the recordings on the tracks beside it are.
        beside: Vec<(sg_core::TrackId, f32)>,
    },
    /// The global folder as read, and what was just done there.
    Global(Result<Vec<sg_project::global::GlobalEntry>, String>, Option<String>),
    GlobalPoster {
        file: PathBuf,
        rgba: Option<Vec<u8>>,
    },
    /// Whether the mark of copied clips is on the clipboard now.
    ClipsOffered(bool),
    /// egui wants a repaint after this delay.
    Repaint(Duration),
    /// A worker changed something visible.
    Wake,
    Library(Result<Vec<ProjectEntry>, String>),
    Opened(Result<Opened, String>),
    History(Result<Vec<sg_project::HistoryEntry>, String>),
    Export(ExportUpdate),
    /// A scripted step and the channel that tells the script thread it is done.
    Script(Step, Sender<()>),
    Import(ImportEvent),
    Beats {
        root: PathBuf,
        id: AssetId,
        beats: Result<sg_media::analysis::Beats, String>,
    },
    Synced {
        root: PathBuf,
        take: AssetId,
        found: Result<sg_media::analysis::Offset, String>,
    },
    Colour {
        root: PathBuf,
        scenario: sg_core::ScenarioId,
        measured: Vec<(AssetId, Result<sg_core::ColourStats, String>)>,
        group: Option<u64>,
    },
    LookSaved {
        root: PathBuf,
        result: Result<String, String>,
    },
    Looks {
        root: PathBuf,
        looks: Vec<(String, Vec<sg_core::Module>)>,
        luts: Vec<String>,
    },
    /// A file was made from an asset (`what` was done to it): where it is,
    /// to be taken into the project it was made in.
    Made {
        root: PathBuf,
        label: String,
        what: &'static str,
        result: Result<PathBuf, String>,
    },
    /// A video with the person cut out was made: where it is.
    CutOut {
        root: PathBuf,
        id: AssetId,
        label: String,
        result: Result<PathBuf, String>,
    },
    Stabilised {
        id: AssetId,
        label: String,
        result: Result<(), String>,
    },
    /// A MIDI file as read.
    MidiFile {
        root: PathBuf,
        name: String,
        result: Result<Vec<u8>, String>,
    },
    /// A file written into the exports folder: its path.
    Written {
        root: PathBuf,
        result: Result<String, String>,
    },
    LutImported {
        root: PathBuf,
        result: Result<String, String>,
    },
    FontImported {
        root: PathBuf,
        result: Result<String, String>,
    },
    Fonts {
        root: PathBuf,
        fonts: Vec<String>,
    },
    Peaks {
        root: PathBuf,
        id: AssetId,
        peaks: Option<sg_media::peaks::Peaks>,
    },
    Poster {
        root: PathBuf,
        id: AssetId,
        rgba: Option<Vec<u8>>,
    },
    ProjectPoster {
        path: PathBuf,
        rgba: Option<Vec<u8>>,
    },
    /// Something went wrong off the UI thread: say so in the banner.
    Problem(String),
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
    /// Stops the step of the Native Instruments set-up that is running.
    plugin_step: Option<sg_media::Cancel>,
    /// The MIDI keyboards, while they are listened to.
    midi: Option<sg_play::MidiIn>,
    /// Running imports and when each was requested.
    imports: HashMap<u64, Instant>,
    wl: Option<WlData>,
    /// The preview engine; started once the GPU device exists.
    player: Option<Player>,
    /// The viewer texture as registered with the UI renderer, and the
    /// player's output generation it belongs to.
    viewer_texture: Option<(egui::TextureId, u64)>,
    angles_texture: Option<(egui::TextureId, u64)>,
    /// The snapshot the player was last given.
    synced: Option<(PathBuf, std::sync::Arc<sg_core::Project>)>,
    /// Jobs started here that show among the import jobs.
    local_jobs: u64,
    /// The way to stop each of them, by the number it is shown under.
    local_cancels: std::collections::HashMap<u64, sg_media::Cancel>,
    /// Assets whose stabilised copy is being made.
    stabilising: std::collections::HashSet<AssetId>,
    /// Scratch for the level meters read each frame.
    meters: Vec<f32>,
    /// Stops the running export.
    export_cancel: Option<sg_media::Cancel>,
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
                Command::Close { writer, root, poster, rename } => {
                    self.pending_jobs += 1;
                    self.saw_editor_frame = false;
                    // The player lets go of the project now, and says when
                    // it and the plugins have: only then may the folder move.
                    let settled = self.player.as_ref().map(|player| {
                        player.clear_project();
                        player.settled()
                    });
                    self.synced = None;
                    self.io.run(move || {
                        if let Some(thread) = writer.close() {
                            let _ = thread.join();
                        }
                        let let_go = settled.is_none_or(|settled| settled.recv_timeout(Duration::from_secs(5)).is_ok());
                        // The folder takes the project's name (feedback F16.26).
                        let root = if rename && let_go {
                            sg_project::follow_name(&root).unwrap_or_else(|e| {
                                log::warn!("{}: the folder keeps its name: {e}", root.display());
                                root
                            })
                        } else {
                            root
                        };
                        // The picture the library shows for it. Without one the tile keeps its letters.
                        if let Some(asset) = poster {
                            let _ = sg_project::save_project_poster(&root, asset);
                        }
                        let _ = proxy.send_event(scan_library(&library));
                        let _ = proxy.send_event(UserEvent::Settings(sg_project::settings::load(&library)));
                    });
                }
                Command::Export { root, project, request } => {
                    let Some(gfx) = &self.gfx else { continue };
                    // The export runs plugins of its own, set as the ones that are heard now.
                    let plugged =
                        project.sequences.first().is_some_and(|s| s.tracks.iter().any(|t| !t.plugins.is_empty()));
                    if let (true, Some(player)) = (plugged, &self.player) {
                        player.save_plugins();
                    }
                    let (device, queue) = gfx.device();
                    let cancel = sg_media::Cancel::new();
                    self.export_cancel = Some(cancel.clone());
                    self.pending_jobs += 1;
                    let spawned = std::thread::Builder::new().name("sg-export".into()).spawn(move || {
                        let update = run_export(root, project, request, &device, &queue, &cancel, &proxy);
                        let _ = proxy.send_event(UserEvent::Export(update));
                    });
                    if let Err(e) = spawned {
                        self.shell.export_update(ExportUpdate::Failed(e.to_string()));
                        self.export_cancel = None;
                        self.pending_jobs -= 1;
                    }
                }
                Command::CancelExport => {
                    if let Some(cancel) = &self.export_cancel {
                        cancel.cancel();
                    }
                }
                Command::LoadHistory { root } => {
                    self.pending_jobs += 1;
                    self.io.run(move || {
                        let result = sg_project::history(&root, 200).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::History(result));
                    });
                }
                Command::Restore { root, writer, commit, description } => {
                    self.pending_jobs += 1;
                    self.open_requested = Some(Instant::now());
                    self.saw_editor_frame = false;
                    self.io.run(move || {
                        // The writer saves and commits what was open, then the
                        // files go back to the chosen state as a new commit.
                        if let Some(thread) = writer.close() {
                            let _ = thread.join();
                        }
                        let result = sg_project::restore(&root, &commit, &description)
                            .and_then(|()| open_project(&root, &proxy))
                            .map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Opened(result));
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
                    // (A job made here, a cut-out or the like, or one of the importer's.)
                    if let Some(cancel) = self.local_cancels.get(&job) {
                        cancel.cancel();
                    } else if let Some(importer) = &self.importer {
                        importer.cancel(job);
                    }
                }
                Command::MeasureVoice { root, track, style, parts, gains, beside } => {
                    self.pending_jobs += 1;
                    let cancel = sg_media::Cancel::new();
                    let spawned = std::thread::Builder::new().name("sg-analysis".into()).spawn(move || {
                        // Measured, set up, and the set-up checked on the sound itself.
                        let fitted = sg_project::analysis::voice_sound(&root, &parts, &gains, &cancel)
                            .map_err(|e| e.to_string())
                            .and_then(|sound| {
                                sg_play::fit_voice(&sound, style)
                                    .ok_or_else(|| "there is too little sound on this track to listen to".to_owned())
                            });
                        let (result, fitted) = match fitted {
                            Ok((stats, set)) => (Ok(stats), Some(set)),
                            Err(e) => (Err(e), None),
                        };
                        // (How loud what plays beside it is: for a word on the balance.)
                        let beside = beside
                            .into_iter()
                            .filter_map(|(id, parts, gains)| {
                                let level = sg_project::analysis::level(&root, &parts, &gains, &cancel).ok()??;
                                Some((id, level))
                            })
                            .collect();
                        let _ = proxy.send_event(UserEvent::Voice { root, track, style, result, fitted, beside });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                Command::LoadBeats { root, id } => {
                    self.pending_jobs += 1;
                    let cancel = sg_media::Cancel::new();
                    let spawned = std::thread::Builder::new().name("sg-analysis".into()).spawn(move || {
                        let beats = sg_project::analysis::beats(&root, id, &cancel).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Beats { root, id, beats });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                Command::SyncTake { root, song, take } => {
                    self.pending_jobs += 1;
                    let cancel = sg_media::Cancel::new();
                    let spawned = std::thread::Builder::new().name("sg-analysis".into()).spawn(move || {
                        let found = sg_project::analysis::sync(&root, song, take, &cancel).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Synced { root, take, found });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                Command::AnalyseColour { root, scenario, assets, group } => {
                    self.pending_jobs += 1;
                    let cancel = sg_media::Cancel::new();
                    let spawned = std::thread::Builder::new().name("sg-analysis".into()).spawn(move || {
                        let measured = assets
                            .iter()
                            .map(|asset| {
                                (
                                    asset.id,
                                    sg_project::analysis::colour(&root, asset, &cancel).map_err(|e| e.to_string()),
                                )
                            })
                            .collect();
                        let _ = proxy.send_event(UserEvent::Colour { root, scenario, measured, group });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                Command::SaveLook { root, name, modules } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let result = sg_project::analysis::save_look(&root, &name, &modules).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::LookSaved { root, result });
                    });
                }
                Command::ImportLut { root, path } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let result = sg_project::analysis::import_lut(&root, &path).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::LutImported { root, result });
                    });
                }
                Command::ReadMidi { root, path } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        // (A MIDI file is small; anything else of that name is not read whole.)
                        let result = match std::fs::metadata(&path) {
                            Ok(found) if found.len() > 16 << 20 => Err("it is too large to be a MIDI file".to_owned()),
                            _ => std::fs::read(&path).map_err(|e| e.to_string()),
                        };
                        let _ = proxy.send_event(UserEvent::MidiFile { root, name, result });
                    });
                }
                Command::WriteExport { root, stem, extension, bytes } => {
                    self.pending_jobs += 1;
                    self.io.run(move || {
                        let folder = root.join("exports");
                        let free = (1..10_000)
                            .map(|n| {
                                folder.join(if n == 1 {
                                    format!("{stem}.{extension}")
                                } else {
                                    format!("{stem}-{n}.{extension}")
                                })
                            })
                            .find(|path| !path.exists());
                        let result = match free {
                            Some(path) => std::fs::create_dir_all(&folder)
                                .and_then(|()| std::fs::write(&path, &bytes))
                                .map(|()| path.to_string_lossy().into_owned())
                                .map_err(|e| e.to_string()),
                            None => Err("no free name is left".to_owned()),
                        };
                        let _ = proxy.send_event(UserEvent::Written { root, result });
                    });
                }
                Command::ImportFont { root, path } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let result = sg_project::analysis::import_font(&root, &path).map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::FontImported { root, result });
                    });
                }
                Command::LoadFonts { root } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let fonts = sg_project::analysis::list_fonts(&root);
                        let _ = proxy.send_event(UserEvent::Fonts { root, fonts });
                    });
                }
                Command::LoadLooks { root } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let (looks, luts) =
                            (sg_project::analysis::load_looks(&root), sg_project::analysis::list_luts(&root));
                        let _ = proxy.send_event(UserEvent::Looks { root, looks, luts });
                    });
                }
                Command::Stretch { root, asset, semitones, tempo } => {
                    self.pending_jobs += 1;
                    self.local_jobs += 1;
                    let job = u64::MAX - self.local_jobs;
                    let cancel = sg_media::Cancel::new();
                    self.local_cancels.insert(job, cancel.clone());
                    let spawned = std::thread::Builder::new().name("sg-stretch".into()).spawn(move || {
                        let label = asset.label.clone();
                        let mut last = -1i32;
                        let mut progress = |fraction: f32| {
                            if (fraction * 100.0) as i32 != last {
                                last = (fraction * 100.0) as i32;
                                let event = ImportEvent::Progress {
                                    job,
                                    label: label.clone(),
                                    stage: "stretching",
                                    fraction: Some(fraction),
                                };
                                let _ = proxy.send_event(UserEvent::Import(event));
                            }
                        };
                        progress(0.0);
                        let result =
                            sg_project::analysis::stretch(&root, &asset, semitones, tempo, &cancel, &mut progress)
                                .map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Import(ImportEvent::Finished { job }));
                        let _ = proxy.send_event(UserEvent::Made {
                            root,
                            label: asset.label.clone(),
                            what: "given another pitch or tempo",
                            result,
                        });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                Command::CutOut { asset, .. } if !self.stabilising.insert(asset.id) => {}
                Command::CutOut { root, asset } => {
                    self.pending_jobs += 1;
                    self.local_jobs += 1;
                    let job = u64::MAX - self.local_jobs;
                    let cancel = sg_media::Cancel::new();
                    self.local_cancels.insert(job, cancel.clone());
                    let library = sg_project::library_dir();
                    let spawned = std::thread::Builder::new().name("sg-cut-out".into()).spawn(move || {
                        let label = asset.label.clone();
                        let mut last = -1i32;
                        let mut progress = |fraction: f32| {
                            if (fraction * 100.0) as i32 != last {
                                last = (fraction * 100.0) as i32;
                                let event = ImportEvent::Progress {
                                    job,
                                    label: label.clone(),
                                    stage: "cutting out",
                                    fraction: Some(fraction),
                                };
                                let _ = proxy.send_event(UserEvent::Import(event));
                            }
                        };
                        progress(0.0);
                        let result = sg_project::analysis::cut_out(&library, &root, &asset, &cancel, &mut progress)
                            .map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Import(ImportEvent::Finished { job }));
                        let _ = proxy.send_event(UserEvent::CutOut {
                            root,
                            id: asset.id,
                            label: asset.label.clone(),
                            result,
                        });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                // One run per asset: a second request while it is being made changes nothing.
                Command::Stabilise { asset, .. } if !self.stabilising.insert(asset.id) => {}
                Command::Stabilise { root, asset } => {
                    self.pending_jobs += 1;
                    self.local_jobs += 1;
                    // Shown among the import jobs; numbered from the top so as not to collide with them.
                    let job = u64::MAX - self.local_jobs;
                    let cancel = sg_media::Cancel::new();
                    self.local_cancels.insert(job, cancel.clone());
                    let spawned = std::thread::Builder::new().name("sg-stabilise".into()).spawn(move || {
                        let label = asset.label.clone();
                        let mut last = -1i32;
                        let mut progress = |fraction: f32| {
                            // One event per percent.
                            if (fraction * 100.0) as i32 != last {
                                last = (fraction * 100.0) as i32;
                                let event = ImportEvent::Progress {
                                    job,
                                    label: label.clone(),
                                    stage: "stabilising",
                                    fraction: Some(fraction),
                                };
                                let _ = proxy.send_event(UserEvent::Import(event));
                            }
                        };
                        let result = sg_project::analysis::stabilise(&root, &asset, &cancel, &mut progress)
                            .map_err(|e| e.to_string());
                        let _ = proxy.send_event(UserEvent::Import(ImportEvent::Finished { job }));
                        let _ = proxy.send_event(UserEvent::Stabilised {
                            id: asset.id,
                            label: asset.label.clone(),
                            result,
                        });
                    });
                    if spawned.is_err() {
                        self.pending_jobs -= 1;
                    }
                }
                Command::LoadPeaks { root, id } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let peaks = sg_media::peaks::Peaks::read(&sg_project::cache::peaks(&root, id)).ok();
                        let _ = proxy.send_event(UserEvent::Peaks { root, id, peaks });
                    });
                }
                Command::Reveal { path } => {
                    // The desktop's opener, on a thread of its own: it may take
                    // its time, and what it says when it fails goes to the banner.
                    let spawned = std::thread::Builder::new().name("sg-reveal".into()).spawn(move || {
                        // A folder itself, the folder of a file; if that is
                        // not there yet (no export so far), the nearest one that is.
                        let mut folder =
                            if path.is_dir() { path } else { path.parent().map(Path::to_owned).unwrap_or(path) };
                        while !folder.is_dir() {
                            match folder.parent() {
                                Some(parent) => folder = parent.to_owned(),
                                None => break,
                            }
                        }
                        let result = std::process::Command::new("xdg-open")
                            .arg(&folder)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::piped())
                            .output();
                        let problem = match result {
                            Ok(output) if output.status.success() => return,
                            Ok(output) => {
                                String::from_utf8_lossy(&output.stderr).lines().last().unwrap_or("").to_owned()
                            }
                            Err(e) => e.to_string(),
                        };
                        let _ = proxy.send_event(UserEvent::Problem(format!(
                            "The file manager could not be opened ({problem}). The file is in {}",
                            folder.display()
                        )));
                    });
                    if let Err(e) = spawned {
                        self.shell.report_error(format!("Cannot start the file manager: {e}"));
                    }
                }
                Command::LoadProjectPoster { path } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let rgba = sg_project::read_project_poster(&path).ok();
                        let _ = proxy.send_event(UserEvent::ProjectPoster { path, rgba });
                    });
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
                Command::Monitor(input) => {
                    if let Some(player) = &self.player {
                        // (Automated runs name their input through the environment, as for recording.)
                        let named = std::env::var("SHOTGUN_AUDIO_IN").ok();
                        player.monitor(input.map(|(source, hear)| (named.or(source), hear)));
                    }
                }
                Command::MonitorChain(mix) => {
                    if let Some(player) = &self.player {
                        player.monitor_chain(mix.map(|mix| *mix));
                    }
                }
                Command::Loop(range) => {
                    if let Some(player) = &self.player {
                        player.set_loop(range);
                    }
                }
                Command::Hand(track) => {
                    if let Some(player) = &self.player {
                        player.set_hand(track.map(|t| t.0));
                    }
                }
                Command::Note { on, pitch, velocity } => {
                    if let Some(at) = self.player.as_ref().and_then(|p| p.note(on, pitch, velocity)) {
                        self.shell.note_heard(on, pitch, velocity, at);
                    }
                }
                Command::ListInputs => {
                    self.reads.run(move || {
                        let inputs = sg_play::inputs().unwrap_or_default();
                        let listed = inputs.into_iter().map(|i| (i.name, i.label)).collect();
                        let _ = proxy.send_event(UserEvent::Inputs(listed));
                    });
                }
                Command::SaveInput(input) => {
                    self.reads.run(move || {
                        let mut settings = sg_project::settings::load(&library);
                        settings.audio_input = input;
                        if let Err(e) = sg_project::settings::save(&library, &settings) {
                            log::warn!("cannot save the settings: {e}");
                        }
                    });
                }
                Command::Record { root, source, lead_in } => match &self.player {
                    Some(player) => {
                        // Recorded beside the project's caches, then imported like any file.
                        let stamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_secs());
                        let path = root.join(".shotgun/takes").join(format!("take-{stamp}.wav"));
                        // The environment names an input for automated runs (a null sink's monitor).
                        player.record(path, std::env::var("SHOTGUN_AUDIO_IN").ok().or(source), lead_in);
                    }
                    None => self.shell.recording_failed("There is no sound engine to record with.".to_owned()),
                },
                Command::Metronome(on) => {
                    if let Some(player) = &self.player {
                        player.set_click(on);
                    }
                }
                Command::StopRecording => {
                    if let Some(player) = &self.player {
                        player.pause();
                    }
                }
                Command::PluginEditor { key, .. } => {
                    if let Some(player) = &self.player {
                        player.open_plugin_editor(key);
                    }
                }
                Command::Plugins(request) => {
                    use sg_project::plugins;
                    use sg_ui::{NiStep, PluginRequest, PluginUpdate};
                    let found = move |proxy: &EventLoopProxy<UserEvent>, library: &Path| {
                        // The program was updated since the Windows plugins were set up: their
                        // loaders are of the bridge before, and are made anew before they are listed.
                        if plugins::bridge_stale() {
                            let line = |text: &str| {
                                let _ = proxy.send_event(UserEvent::Plugins(PluginUpdate::Line(text.to_owned())));
                            };
                            line("The bridge to Windows plugins was updated: setting the plugins up again.");
                            if let Err(e) =
                                plugins::bridge(&plugins::ni_prefix(library), &sg_media::Cancel::new(), line)
                            {
                                line(&format!("That did not work: {e}"));
                            }
                        }
                        let update = PluginUpdate::Found {
                            plugins: plugins::scan(&plugins::folders()),
                            ni: plugins::ni_state(library),
                        };
                        let _ = proxy.send_event(UserEvent::Plugins(update));
                    };
                    match request {
                        // (Done by the shell itself: it never comes this far.)
                        PluginRequest::Play(..) => {}
                        PluginRequest::Stop => {
                            if let Some(cancel) = &self.plugin_step {
                                cancel.cancel();
                            }
                        }
                        PluginRequest::Scan => {
                            self.reads.run(move || found(&proxy, &library));
                        }
                        PluginRequest::Run(step) => {
                            let cancel = sg_media::Cancel::new();
                            self.plugin_step = Some(cancel.clone());
                            // Its own thread: a step runs for minutes (an installer, or Native Access while it is open).
                            let spawned = std::thread::Builder::new().name("sg-plugins".into()).spawn(move || {
                                let prefix = plugins::ni_prefix(&library);
                                // Everything a step prints is also kept in a file: the
                                // panel shows the last lines only, and a failure needs all of them.
                                let log_file = plugins::ni_log(&library);
                                let mut kept =
                                    std::fs::OpenOptions::new().create(true).append(true).open(&log_file).ok();
                                let mut keep = |text: &str| {
                                    use std::io::Write;
                                    if let Some(file) = &mut kept {
                                        let _ = writeln!(file, "{text}");
                                    }
                                };
                                let stamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_secs());
                                keep(&format!("\n== {step:?}, unix time {stamp}, prefix {}", prefix.display()));
                                let _ = proxy.send_event(UserEvent::Plugins(PluginUpdate::LogFile(log_file)));
                                let line = |text: &str| {
                                    keep(text);
                                    let _ = proxy.send_event(UserEvent::Plugins(PluginUpdate::Line(text.to_owned())));
                                };
                                let result = match step {
                                    NiStep::Setup => plugins::ni_setup(&prefix, &cancel, line),
                                    NiStep::Launch => plugins::ni_launch(&prefix, &cancel, line),
                                    NiStep::Bridge => plugins::bridge(&prefix, &cancel, line),
                                    NiStep::Check => plugins::ni_check(&prefix, &cancel, line),
                                };
                                if let Err(e) = &result {
                                    keep(&format!("== failed: {e}"));
                                }
                                let finished = PluginUpdate::Finished(result.map_err(|e| e.to_string()));
                                let _ = proxy.send_event(UserEvent::Plugins(finished));
                                found(&proxy, &library);
                            });
                            if let Err(e) = spawned {
                                self.shell.plugins_update(PluginUpdate::Finished(Err(e.to_string())));
                            }
                        }
                    }
                }
                Command::ListGlobal | Command::MakeGlobal { .. } | Command::RemoveGlobal { .. } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let dir = sg_project::global::dir(&library);
                        // What was asked for first, then the folder as it is after it.
                        let done = match &command {
                            Command::MakeGlobal { root, asset } => {
                                sg_project::global::add(&dir, root, asset).map(|_| {
                                    Some(format!(
                                        "\"{}\" is global now: every project finds it in the Global tab.",
                                        asset.label
                                    ))
                                })
                            }
                            Command::RemoveGlobal { file } => {
                                sg_project::global::remove(&dir, file).map(|()| Some("Removed from Global.".to_owned()))
                            }
                            _ => Ok(None),
                        };
                        let result = done
                            .and_then(|news| Ok((sg_project::global::list(&dir)?, news)))
                            .map_err(|e| format!("{e} ({})", dir.display()));
                        let event = match result {
                            Ok((entries, news)) => UserEvent::Global(Ok(entries), news),
                            Err(e) => UserEvent::Global(Err(e), None),
                        };
                        let _ = proxy.send_event(event);
                    });
                }
                Command::LoadGlobalPoster { file } => {
                    self.pending_jobs += 1;
                    self.reads.run(move || {
                        let rgba = sg_project::global::read_poster(&file).ok();
                        let _ = proxy.send_event(UserEvent::GlobalPoster { file, rgba });
                    });
                }
                Command::OfferClips => {
                    // Automated runs must never touch the user's clipboard.
                    if std::env::var_os("SHOTGUN_NO_CLIPBOARD").is_none() {
                        offer_clips(proxy.clone());
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
                if let Some(player) = &self.player {
                    self.shell.set_playhead(player.frame());
                }
                let before = self.shell.error_shown().map(str::to_owned);
                let commands = self.shell.dev(dev);
                // Nobody reads the banner in a scripted run: say it in the log.
                if let Some(message) = self.shell.error_shown().filter(|m| before.as_deref() != Some(*m)) {
                    log::warn!("script step answered: {message}");
                }
                self.run_commands(commands);
            }
            Step::Shot(path) => self.screenshot(&path),
            Step::TimelineBench(frames) => self.timeline_bench(frames),
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
            // Surface coordinates are the points the UI lays out in.
            DataEvent::Drop { mime, data, x, y } => match incoming_from(&mime, data) {
                Some(incoming) => {
                    let commands = self.shell.incoming_at(incoming, x as f32, y as f32);
                    self.run_commands(commands);
                    return self.request_redraw();
                }
                None => self.shell.report_error("Nothing in there that can be imported.".to_owned()),
            },
            DataEvent::Paste { mime, data } => match incoming_from(&mime, data) {
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
        for news in player.take_plugin_news() {
            if news.starts_with("Cannot record") || news.starts_with("The recording") {
                self.shell.recording_failed(news);
            } else if let Some(note) = news.strip_prefix("Note: ") {
                self.shell.notify(note.to_owned());
            } else {
                self.shell.report_error(news);
            }
        }
        let takes = player.take_recordings();
        let mut imports = Vec::new();
        for take in takes {
            imports.extend(self.shell.take_recorded(take.path, take.start, take.frames, take.lost));
        }
        for spent in self.shell.take_spent_takes() {
            self.reads.run(move || {
                let _ = std::fs::remove_file(spent);
            });
        }
        if !imports.is_empty() {
            self.run_commands(imports);
            return self.sync_player();
        }
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
        // The camera-angle monitor is a second texture, there only while the
        // sequence has camera tracks.
        let angles = player.angles().map(|(view, generation, count, cols, rows)| {
            let texture = match self.angles_texture {
                Some((id, seen)) if seen == generation => id,
                Some((id, _)) => {
                    gfx.replace_texture(id, &view);
                    id
                }
                None => gfx.register_texture(&view),
            };
            self.angles_texture = Some((texture, generation));
            sg_ui::AnglesView { texture, count, cols, rows }
        });
        // The chain on screen: 3.5 s of it behind what is heard, and the
        // 0.4 s that are laid down already and about to be.
        let watched = self.shell.watched();
        player.watch(watched.map(|(track, _)| track), watched.and_then(|(_, spectrum_at)| spectrum_at));
        let per_second = 48_000 / sg_play::SCOPE_STEP as usize;
        let (behind, ahead) = (per_second * 7 / 2, per_second * 2 / 5);
        let mut scope = Vec::new();
        if watched.is_some() {
            player.scope(behind, ahead, &mut scope);
        }
        ViewerState {
            spectrum: watched.and_then(|_| player.spectrum()),
            scope,
            scope_ahead: ahead,
            eq_response: Some(sg_play::response_db),
            texture: Some(texture),
            frame: player.frame(),
            playing: player.is_playing(),
            stats: player_lines(player),
            master_meter: player.take_meters(&mut self.meters),
            limiting_db: player.take_limiting(),
            loudness: player.loudness(),
            input_level: player.take_input_level(),
            stages: {
                let mut stages = vec![[0.0; 3]; sg_play::MAX_METERS];
                player.stages(&mut stages);
                stages
            },
            meters: self.meters.clone(),
            angles,
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

    /// Frame-time benchmark of the UI with the timeline zooming and scrolling
    /// (the script of spike S2), rendered off-screen at 144 steps per second
    /// of script time. Blocks the UI thread; development use only.
    fn timeline_bench(&mut self, frames: usize) {
        self.sync_player();
        let view = self.viewer_state();
        let Some(gfx) = &mut self.gfx else { return };
        let shell = &mut self.shell;
        let mut cpu = Vec::with_capacity(frames);
        let mut quads = 0;
        // Warm-up frames (fonts, pipeline) are not counted, as in the spike.
        for i in 0..frames + 30 {
            let _ = shell.dev(DevStep::TimelineBench(i as f64 / 144.0));
            let ms = gfx.bench_frame(&self.ctx, |ui| drop(shell.frame(ui, 0, &view)));
            if i >= 30 {
                cpu.push(ms);
                quads = quads.max(shell.timeline_quads());
            }
            // Keep the GPU from piling up frames: the next one starts when this one is done.
            if i % 4 == 3 {
                gfx.wait_idle();
            }
        }
        println!(
            "BENCH timeline frames={} cpu_p50={:.2} cpu_p95={:.2} cpu_p99={:.2} cpu_max={:.2} max_quads={quads}",
            cpu.len(),
            percentile(&cpu, 0.5),
            percentile(&cpu, 0.95),
            percentile(&cpu, 0.99),
            percentile(&cpu, 1.0)
        );
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
                    Ok(player) => {
                        self.player = Some(player);
                        // MIDI keyboards, if the machine has a sequencer: their notes come in as events.
                        let proxy = self.proxy.clone();
                        // (An automated run is its own client: stand-in keyboards address it by name.)
                        let name = std::env::var("SHOTGUN_APP_ID").unwrap_or_else(|_| "Shotgun".to_owned());
                        let listening = sg_play::MidiIn::start(&name, move |on, pitch, velocity| {
                            let _ = proxy.send_event(UserEvent::Midi(on, pitch, velocity));
                        });
                        match listening {
                            Ok(midi) => self.midi = Some(midi),
                            Err(e) => log::info!("MIDI keyboards are not listened to: {e}"),
                        }
                    }
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
            UserEvent::Export(update) => {
                let finished = !matches!(update, ExportUpdate::Progress { .. });
                self.shell.export_update(update);
                if finished {
                    self.export_cancel = None;
                    self.job_done();
                } else {
                    self.request_redraw();
                }
            }
            UserEvent::History(result) => {
                self.shell.set_history(result);
                self.job_done();
            }
            UserEvent::Opened(result) => {
                let commands = self.shell.opened(result);
                self.run_commands(commands);
                self.job_done();
            }
            UserEvent::Script(step, ack) => self.script_step(step, ack),
            UserEvent::Import(event) => self.import_event(event),
            UserEvent::Voice { root, track, style, result, fitted, beside } => {
                self.shell.heard_beside(beside);
                self.shell.voice_measured(&root, track, style, result, fitted);
                self.job_done();
            }
            UserEvent::Beats { root, id, beats } => {
                self.shell.beats_loaded(&root, id, beats);
                self.job_done();
            }
            UserEvent::Synced { root, take, found } => {
                self.shell.synced(&root, take, found);
                self.job_done();
            }
            UserEvent::Colour { root, scenario, measured, group } => {
                for (asset, stats) in measured {
                    self.shell.colour_measured(&root, scenario, asset, stats, group);
                }
                self.job_done();
            }
            UserEvent::LookSaved { root, result } => {
                self.shell.look_saved(&root, result);
                self.job_done();
            }
            UserEvent::MidiFile { root, name, result } => {
                self.shell.midi_read(&root, &name, result);
                self.job_done();
            }
            UserEvent::Written { root, result } => {
                self.shell.file_written(&root, result);
                self.job_done();
            }
            UserEvent::LutImported { root, result } => {
                let commands = self.shell.lut_imported(&root, result);
                // A table of the same name may have been replaced.
                if let Some(player) = &self.player {
                    player.forget_fonts();
                }
                self.job_done();
                self.run_commands(commands);
            }
            UserEvent::FontImported { root, result } => {
                let commands = self.shell.font_imported(&root, result);
                // A font of the same name may have been replaced.
                if let Some(player) = &self.player {
                    player.forget_fonts();
                }
                self.job_done();
                self.run_commands(commands);
            }
            UserEvent::Fonts { root, fonts } => {
                self.shell.fonts_loaded(&root, fonts);
                self.job_done();
            }
            UserEvent::Looks { root, looks, luts } => {
                self.shell.looks_loaded(&root, looks, luts);
                self.job_done();
            }
            UserEvent::Made { root, label, what, result } => {
                let here = self.shell.session().is_some_and(|session| session.root == root);
                match result {
                    Ok(path) if !here => self.shell.notify(format!(
                        "{label} was {what} after its project was closed. The file is {}: drop it into the project to use it.",
                        path.display()
                    )),
                    Ok(path) => {
                        let commands = self.shell.incoming(sg_ui::Incoming::Paths(vec![path]));
                        self.run_commands(commands);
                    }
                    // (Stopped by hand: nothing to report.)
                    Err(e) if e == "cancelled" => {}
                    Err(e) => self.shell.report_error(format!("{label} could not be {what}: {e}")),
                }
                self.job_done();
            }
            UserEvent::CutOut { root, id, label, result } => {
                self.stabilising.remove(&id);
                let here = self.shell.session().is_some_and(|session| session.root == root);
                match result {
                    // Its project was closed meanwhile: the file stays where
                    // it was made and goes into no other project.
                    Ok(path) if !here => {
                        self.shell.notify(format!(
                            "{label} was cut out after its project was closed. The file is {}: drop it into the project to use it.",
                            path.display()
                        ));
                    }
                    // The file is taken into the project like one that was dropped.
                    Ok(path) => {
                        let commands = self.shell.incoming(sg_ui::Incoming::Paths(vec![path]));
                        self.run_commands(commands);
                    }
                    Err(e) if e == "cancelled" => {}
                    Err(e) => self.shell.report_error(format!("The person could not be cut out of {label}: {e}")),
                }
                self.job_done();
            }
            UserEvent::Stabilised { id, label, result } => {
                self.stabilising.remove(&id);
                match result {
                    // The player reopens the asset and finds the new copy.
                    Ok(()) => {
                        if let Some(player) = &self.player {
                            player.asset_changed(id);
                        }
                    }
                    Err(e) => self.shell.report_error(format!("{label} could not be stabilised: {e}")),
                }
                self.job_done();
            }
            UserEvent::Peaks { root, id, peaks } => {
                self.shell.peaks_loaded(&root, id, peaks);
                self.job_done();
            }
            UserEvent::Problem(message) => self.shell.report_error(message),
            UserEvent::ClipsOffered(offered) => self.shell.clips_offered(offered),
            UserEvent::Plugins(update) => self.shell.plugins_update(update),
            UserEvent::Inputs(inputs) => self.shell.inputs_listed(inputs),
            // A MIDI keyboard plays what the computer keyboard plays: the
            // instrument of the open piano roll.
            UserEvent::Midi(on, pitch, velocity) => {
                if let Some(at) = self.player.as_ref().and_then(|p| p.note(on, pitch, velocity)) {
                    self.shell.note_heard(on, pitch, velocity, at);
                }
            }
            UserEvent::Settings(settings) => self.shell.set_input(settings.audio_input),
            UserEvent::Global(result, news) => {
                self.shell.global_listed(result, news);
                self.job_done();
            }
            UserEvent::GlobalPoster { file, rgba } => {
                self.shell.global_poster_loaded(&self.ctx, file, rgba);
                self.job_done();
            }
            UserEvent::ProjectPoster { path, rgba } => {
                self.shell.project_poster_loaded(&self.ctx, path, rgba);
                self.job_done();
            }
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
        // egui-winit swallows Ctrl+C, X and V and hands egui clipboard
        // events instead, which only text fields understand. Outside a text
        // field they must arrive as keys, for the key map (feedback F4.4:
        // copy, cut and paste of clips did nothing).
        gfx.egui_state.set_clipboard_shortcuts(self.ctx.text_edit_focused());
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

/// Reads the user's changes to the key map (`SHOTGUN_KEYS`, or
/// `shotgun/keys.conf` in the configuration folder). A missing file is
/// written with the built-in keys as comments, unless this is an automated
/// run, which leaves the user's folders alone.
fn load_keys(write_if_missing: bool) {
    let path = std::env::var_os("SHOTGUN_KEYS").map(PathBuf::from).or_else(|| {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(config.join("shotgun/keys.conf"))
    });
    let Some(path) = path else { return };
    let shown = path.display().to_string();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let changed = sg_ui::keymap::with_changes(&text, &shown);
            for problem in &changed.problems {
                log::warn!("{shown}: {problem}");
            }
            sg_ui::keymap::install(changed);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && write_if_missing => {
            let written = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, sg_ui::keymap::default_file()));
            match written {
                Ok(()) => sg_ui::keymap::install(sg_ui::keymap::with_changes("", &shown)),
                Err(e) => log::warn!("could not write {shown}: {e}"),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("could not read {shown}: {e}"),
    }
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

    // Before there is a window: one small file, read once.
    let by_hand = args.script.is_empty() && args.exit_after.is_none() && !args.bench_start && !args.bench_open;
    load_keys(by_hand);

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
        plugin_step: None,
        midi: None,
        wl: None,
        player: None,
        viewer_texture: None,
        angles_texture: None,
        synced: None,
        meters: vec![0.0; sg_play::MAX_METERS],
        local_jobs: 0,
        local_cancels: std::collections::HashMap::new(),
        export_cancel: None,
        library,
        repaint_at: None,
        pending_jobs: 1,
        stabilising: std::collections::HashSet::new(),
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
            "audio      quantum {}  delay {}  xruns {}  underruns {}  rt policy {}  worst callback {} us",
            s.audio.quantum,
            s.audio.delay_frames,
            s.audio.xruns,
            s.audio.underruns,
            s.audio.sched_policy,
            s.audio.process_max_ns / 1000
        ),
    ];
    lines.extend(s.decode_error.map(|e| format!("decode error: {e}")));
    lines
}

/// Runs one export on the calling thread and reports progress as events.
fn run_export(
    root: PathBuf,
    project: std::sync::Arc<sg_core::Project>,
    request: ExportRequest,
    device: &egui_wgpu::wgpu::Device,
    queue: &egui_wgpu::wgpu::Queue,
    cancel: &sg_media::Cancel,
    proxy: &EventLoopProxy<UserEvent>,
) -> ExportUpdate {
    let preset = match request.preset {
        ExportPreset::Master => sg_export::Preset::Master,
        ExportPreset::Vertical => sg_export::Preset::Vertical,
        ExportPreset::Share => sg_export::Preset::Share {
            megabytes: request.megabytes,
            compression: sg_export::Compression::from_level(request.compression),
        },
        ExportPreset::Gif => {
            sg_export::Preset::Gif { compression: sg_export::Compression::from_level(request.compression) }
        }
        ExportPreset::Sound => sg_export::Preset::Sound {
            format: sg_export::SoundFormat::from_level(request.sound_format),
            stems: request.stems,
            loudness: request.loudness,
        },
    };
    let job = sg_export::Job { root, project, preset, range: request.range };
    // One event per stage change or percent, not one per frame.
    let mut last = ("", -1i32);
    let mut progress = |stage: &'static str, fraction: f32| {
        let step = (stage, (fraction * 100.0) as i32);
        if step != last {
            last = step;
            let _ = proxy.send_event(UserEvent::Export(ExportUpdate::Progress { stage, fraction }));
        }
    };
    match sg_export::export(&job, device, queue, cancel, &mut progress) {
        Ok(done) => {
            if let Some((loudness, peak, stems)) = done.sound {
                log::info!("sound: {loudness:?} LUFS, peak {peak:.1} dB, {stems} stems");
            }
            log::info!(
                "exported {} ({} bytes, {} frames) in {:.1} s; render pass {:.1} fps with {}",
                done.path.display(),
                done.bytes,
                done.frames,
                done.seconds,
                done.render_fps,
                done.encoder
            );
            // Automated runs must never touch the user's clipboard.
            let allowed = std::env::var_os("SHOTGUN_NO_CLIPBOARD").is_none();
            let on_clipboard = request.to_clipboard && allowed && copy_file_to_clipboard(&done.path);
            ExportUpdate::Done {
                path: done.path.display().to_string(),
                bytes: done.bytes,
                seconds: done.seconds,
                on_clipboard,
                note: done.sound.map_or(String::new(), |(loudness, peak, stems)| {
                    let loud = loudness.map_or("silent".to_owned(), |l| format!("{l:.1} LUFS"));
                    let tracks =
                        if stems > 0 { format!(", and {stems} tracks in a folder beside it") } else { String::new() };
                    format!("Loudness {loud}, highest peak {peak:.1} dB{tracks}")
                }),
            }
        }
        Err(sg_export::ExportError::Cancelled) => ExportUpdate::Failed("cancelled".to_owned()),
        Err(e) => ExportUpdate::Failed(e.to_string()),
    }
}

/// Puts the mark of copied timeline clips on the clipboard and tells the
/// shell whether that worked. The selection is served by a thread of this
/// process until something else is copied.
fn offer_clips(proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    let spawned = std::thread::Builder::new().name("sg-clipboard".into()).spawn(move || {
        let source = Source::Bytes(sg_ui::CLIPS_ON_CLIPBOARD.as_bytes().into());
        match Options::new().prepare_copy(source, MimeType::Text) {
            Ok(prepared) => {
                let _ = proxy.send_event(UserEvent::ClipsOffered(true));
                if let Err(e) = prepared.serve() {
                    log::warn!("clipboard: {e}");
                }
            }
            Err(e) => {
                log::warn!("clipboard: {e}");
                let _ = proxy.send_event(UserEvent::ClipsOffered(false));
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("clipboard: {e}");
    }
}

/// Offers an exported file on the clipboard the way file managers, browsers
/// and chat apps each expect it (spike S4, D7). The selection is served by
/// a thread of this process until something else is copied.
fn copy_file_to_clipboard(path: &Path) -> bool {
    use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};
    let Ok(bytes) = std::fs::read(path) else { return false };
    let text = path.to_string_lossy().into_owned();
    // Percent-encode everything outside the unreserved set, keeping slashes.
    let encoded: String = text
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let uri = format!("file://{encoded}");
    let source = |data: Vec<u8>, mime: &str| MimeSource {
        source: Source::Bytes(data.into_boxed_slice()),
        mime_type: MimeType::Specific(mime.to_owned()),
    };
    let mut sources = vec![
        source(format!("{uri}\r\n").into_bytes(), "text/uri-list"),
        source(format!("copy\n{uri}").into_bytes(), "x-special/gnome-copied-files"),
        source(text.into_bytes(), "text/plain;charset=utf-8"),
    ];
    // Raw bytes only for GIFs: no application takes raw video from a clipboard.
    if path.extension().is_some_and(|e| e == "gif") {
        sources.insert(2, source(bytes, "image/gif"));
    }
    let spawned = std::thread::Builder::new().name("sg-clipboard".into()).spawn(move || {
        let mut options = Options::new();
        // Foreground: this thread serves paste requests until the selection is replaced.
        options.foreground(true);
        if let Err(e) = options.copy_multi(sources) {
            log::warn!("clipboard: {e}");
        }
    });
    spawned.is_ok()
}
