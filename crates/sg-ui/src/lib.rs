//! The user interface: library, editor shell, key map, performance overlay.
//!
//! Everything here runs on the UI thread and therefore does no I/O: work
//! that touches the disk leaves as a [`Command`] for the application to run
//! on a worker thread, and comes back through [`Shell::set_library`],
//! [`Shell::opened`], [`Shell::import_event`] and [`Shell::poster_loaded`].

mod actions;
mod ai_panel;
mod assets;
mod clip_panel;
mod colour_panel;
mod editor;
mod export_panel;
mod history;
mod icons;
pub mod keymap;
mod library;
mod lyrics;
mod mixer;
mod perf;
mod piano_roll;
mod plugins_panel;
mod scope;
pub use scope::{BANDS as SPECTRUM_BANDS, TAPS as SCOPE_TAPS};
mod session;
mod stage;
pub mod theme;
mod timeline;
pub mod timeline_gpu;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use sg_core::{Asset, AssetId, Op};
use sg_project::{ImportEvent, ImportSource, Opened, ProjectEntry, ProjectWriter};

use crate::assets::{AssetRequest, JobView};

use crate::editor::{Editor, EditorRequest};
use crate::export_panel::{ExportAction, ExportPanel};
pub use crate::export_panel::{ExportPreset, ExportRequest, ExportUpdate};
use crate::history::{HistoryPanel, HistoryRequest};
pub use crate::keymap::{Action, Scope};
use crate::library::{Library, LibraryRequest};
pub use crate::perf::Perf;
pub use crate::plugins_panel::{NiStep, PluginRequest, PluginUpdate};
pub use crate::session::Session;
pub use crate::stage::AnglesView;
pub use crate::stage::{Transport, ViewerState};

/// Work the UI asks the application to do off the UI thread.
pub enum Command {
    Create {
        name: String,
    },
    Open {
        path: PathBuf,
    },
    /// Save and stop the writer of the project that was just closed, then
    /// rescan the library.
    Close {
        writer: ProjectWriter,
        root: PathBuf,
        /// The asset whose poster becomes the project's picture in the library.
        poster: Option<AssetId>,
        /// The folder may be given the project's name now (nothing of the
        /// project is still at work in it).
        rename: bool,
    },
    /// Show this folder, or the folder of this file, in the desktop's file manager.
    Reveal {
        path: PathBuf,
    },
    /// Read a project's picture for the library; answered with [`Shell::project_poster_loaded`].
    LoadProjectPoster {
        path: PathBuf,
    },
    /// Take a file, link or pasted image into the project at `root`.
    Import {
        root: PathBuf,
        known: Arc<BTreeMap<AssetId, Arc<Asset>>>,
        source: ImportSource,
    },
    CancelImport(u64),
    /// Rebuild missing cache files and check that media exists, after open.
    RestoreCache {
        root: PathBuf,
        assets: Arc<BTreeMap<AssetId, Arc<Asset>>>,
    },
    /// Find (or read from the cache) the beats of an asset; answered with [`Shell::beats_loaded`].
    LoadBeats {
        root: PathBuf,
        id: AssetId,
    },
    /// Find where a take sits in the song; answered with [`Shell::synced`].
    SyncTake {
        root: PathBuf,
        song: AssetId,
        take: AssetId,
    },
    /// Measure the pictures of assets for the colour match; answered per
    /// asset with [`Shell::colour_measured`].
    AnalyseColour {
        root: PathBuf,
        scenario: sg_core::ScenarioId,
        assets: Vec<Arc<Asset>>,
        /// Undo group the results join (that of the assignment), if any.
        group: Option<u64>,
    },
    /// Write a look chain to `looks/<name>.toml`.
    SaveLook {
        root: PathBuf,
        name: String,
        modules: Vec<sg_core::Module>,
    },
    /// Copy a `.cube` file into `assets/lut/`; answered with [`Shell::lut_imported`].
    ImportLut {
        root: PathBuf,
        path: PathBuf,
    },
    /// Read a MIDI file; answered with [`Shell::midi_read`].
    ReadMidi {
        root: PathBuf,
        path: PathBuf,
    },
    /// Write a file into the project's exports folder under a name that
    /// is free (`stem`, a number if needed, `extension`); answered with
    /// [`Shell::file_written`].
    WriteExport {
        root: PathBuf,
        stem: String,
        extension: String,
        bytes: Vec<u8>,
    },
    /// Write the film looks that come with the program into the
    /// project's `assets/lut/`; answered with [`Shell::lut_imported`].
    AddBuiltinLooks {
        root: PathBuf,
    },
    /// Copy a font file into `assets/font/`; answered with [`Shell::font_imported`].
    ImportFont {
        root: PathBuf,
        path: PathBuf,
    },
    /// List the fonts in the project; answered with [`Shell::fonts_loaded`].
    LoadFonts {
        root: PathBuf,
    },
    /// Read the looks saved in the project; answered with [`Shell::looks_loaded`].
    LoadLooks {
        root: PathBuf,
    },
    /// Cut the person out of a video asset; the result arrives as a new
    /// asset through [`Shell::incoming`], a failure through [`Shell::report_error`].
    CutOut {
        root: PathBuf,
        asset: Arc<Asset>,
    },
    /// Make a copy of an asset's sound at another pitch (semitones) and
    /// tempo (times as fast); it arrives as a new asset through [`Shell::incoming`].
    Stretch {
        root: PathBuf,
        asset: Arc<Asset>,
        semitones: f32,
        tempo: f32,
    },
    /// Make a copy of a video asset out of focus; it arrives as a new asset.
    Blurred {
        root: PathBuf,
        asset: Arc<Asset>,
    },
    /// Make the part of a video that a clip shows anew; the file is then
    /// taken in like a dropped one, and [`Shell::part_failed`] is told if
    /// it could not be made.
    Part {
        root: PathBuf,
        asset: Box<Asset>,
        how: Part,
        /// Where the part begins in the video and how long it is, in the sequence's frames.
        src_in: i64,
        span: i64,
        rate: sg_core::Rate,
    },
    /// Read which models change a clip by a sentence (the file of this
    /// machine); answered with [`Shell::ai_services_loaded`].
    LoadAiServices,
    /// Write them, as the window for it now has them; answered like a load.
    SaveAiServices(Vec<sg_project::ai::Service>),
    /// Find the key an asset's music is in; answered with [`Shell::key_found`].
    FindKey {
        root: PathBuf,
        id: AssetId,
    },
    /// Make the stabilised copy of an asset if it is not there yet.
    Stabilise {
        root: PathBuf,
        asset: Arc<Asset>,
    },
    /// Read the waveform peaks of an asset; answered with [`Shell::peaks_loaded`].
    LoadPeaks {
        root: PathBuf,
        id: AssetId,
    },
    LoadPoster {
        root: PathBuf,
        id: AssetId,
    },
    /// Show the system file picker; the chosen files are imported.
    PickFiles,
    /// Render and encode; progress comes back through [`Shell::export_update`].
    Export {
        root: PathBuf,
        project: Arc<sg_core::Project>,
        request: ExportRequest,
    },
    CancelExport,
    /// Read the list of saved states; answered with [`Shell::set_history`].
    LoadHistory {
        root: PathBuf,
    },
    /// Close the project, bring its files back to this saved state, reopen.
    Restore {
        root: PathBuf,
        writer: ProjectWriter,
        commit: String,
        description: String,
    },
    /// Play, pause or move the playhead.
    Transport(Transport),
    /// Read the clipboard; the content comes back through [`Shell::incoming`].
    ReadClipboard,
    /// Listen to the voice on a track; answered with [`Shell::voice_measured`].
    /// `parts`: (asset, first sample, length) of what its clips play.
    MeasureVoice {
        root: PathBuf,
        track: sg_core::TrackId,
        style: Option<sg_media::voice::VoiceStyle>,
        parts: Vec<(AssetId, i64, i64)>,
        /// How loud each of them is set to play (linear): what the stages get.
        gains: Vec<f32>,
        /// The other tracks that sound, each with its parts and their
        /// gains: how loud they are is told beside the voice
        /// ([`Shell::heard_beside`]).
        beside: Vec<(sg_core::TrackId, Vec<(AssetId, i64, i64)>, Vec<f32>)>,
    },
    /// Start playing and recording sound into a file in this project;
    /// when it ends, [`Shell::take_recorded`] is told.
    Record {
        root: PathBuf,
        /// The PipeWire node to record from; `None`: the system's default input.
        source: Option<String>,
        /// Start playing a bar before the playhead even without the
        /// metronome: the take goes onto a track that already has sound.
        lead_in: bool,
    },
    /// Open the sound input (`None` inside: the system's default) to show
    /// its level and, with the flag, to hear it; `None`: close it.
    Monitor(Option<(Option<String>, bool)>),
    /// Hear the input through the stages of a track with these settings
    /// (the one marked for recording); `None`: as it arrives.
    MonitorChain(Option<Box<sg_core::TrackSettings>>),
    /// Play the stretch between these timeline samples over and over, or
    /// (`None`) play on as usual.
    Loop(Option<(i64, i64)>),
    /// The audio track whose instrument is played by hand from now on, or none.
    Hand(Option<sg_core::TrackId>),
    /// A key going down or up on that instrument. If the timeline plays,
    /// [`Shell::note_heard`] is told where it was heard.
    Note {
        on: bool,
        pitch: u8,
        velocity: u8,
    },
    /// Find the inputs sound can be recorded from; answered with [`Shell::inputs_listed`].
    ListInputs,
    /// Remember the input for the next start of the program.
    SaveInput(Option<String>),
    /// End the recording (and the playback it runs with).
    StopRecording,
    /// The metronome on or off.
    Metronome(bool),
    /// Show the window of a plugin on this track: the one the engine knows by `key`.
    PluginEditor {
        track: sg_core::TrackId,
        key: u64,
    },
    /// Look for plugins, or run or stop a step of the Native Instruments
    /// set-up; answered with [`Shell::plugins_update`].
    Plugins(PluginRequest),
    /// Read the global folder; answered with [`Shell::global_listed`].
    ListGlobal,
    /// Copy an asset of the project into the global folder, then read the folder again.
    MakeGlobal {
        root: PathBuf,
        asset: Arc<Asset>,
    },
    /// Delete a file of the global folder, then read the folder again.
    RemoveGlobal {
        file: PathBuf,
    },
    /// Read the poster of a global asset; answered with [`Shell::global_poster_loaded`].
    LoadGlobalPoster {
        file: PathBuf,
    },
    /// Clips were copied in the timeline: put [`CLIPS_ON_CLIPBOARD`] on the
    /// clipboard, so that a later paste can tell them from something copied
    /// elsewhere since. Answered with [`Shell::clips_offered`].
    OfferClips,
}

/// What the clipboard holds while the last thing copied was clips in the
/// timeline. (The clips themselves stay in the program.)
pub const CLIPS_ON_CLIPBOARD: &str = "Shotgun clips (paste them into the Shotgun timeline)";

/// Something that arrived from outside: a drop, a paste, the file picker.
pub enum Incoming {
    Paths(Vec<PathBuf>),
    Text(String),
    /// A link of which only the sound is imported.
    Sound(String),
    Image {
        data: Vec<u8>,
        mime: String,
    },
}

/// A scripted step, for automated runs that must not inject real input.
#[derive(Clone, Debug, PartialEq)]
pub enum DevStep {
    Filter(String),
    New(String),
    /// Open the project in the library folder with this name.
    Open(String),
    Rename(String),
    Act(Action),
    /// Import a path or a link, as a drop or paste would.
    Import(String),
    Transport(Transport),
    Checkpoint(String),
    /// Set a property of the selected clip: x, y, scale, rotation, opacity,
    /// speed (percent), fadein, fadeout (frames), gain (dB), text.
    Prop(String, String),
    AddTrack(sg_core::TrackKind),
    /// Drop the selected asset on the timeline at a frame, over a track row or ("new") outside the tracks.
    Drop(i64, Option<usize>, Option<sg_core::TrackKind>),
    /// Set a mixer field of the n-th audio track (`master` for the master fader).
    Mix(String, String, f32),
    /// Programme track: cuts to the beat (`true`) or a whip on every cut.
    Programme(bool),
    /// Make the selected asset the song.
    Song,
    /// Give the sequence a tempo in beats per minute (0: none), four to the bar.
    Bars(f32),
    /// A note clip on the n-th audio track at a frame; its piano roll opens.
    NoteClip(usize, i64),
    /// A note in the newest note clip of the n-th audio track: pitch, start and length in ticks.
    Note(usize, u8, i64, i64),
    /// The built-in instrument of the n-th audio track plays the selected
    /// asset's sound, as recorded at this key.
    Sampler(usize, u8),
    /// A key of the open piano roll's instrument going down (true) or up, as if played by hand.
    Hand(u8, bool),
    /// Switch the piano roll's "Record" (what is played is written into the clip).
    RecordNotes,
    /// Mark the n-th audio track as the one recordings go onto ("R" in its header).
    Arm(usize),
    /// Switch "Level" (the input is opened and measured) or, with true, "Hear".
    Monitor(bool),
    /// Put the plugin at this path on the n-th audio track (empty path:
    /// take it off), or with `window` as the path show its window.
    Plugin(usize, String),
    /// The plugins popover: `open`, or a step of the Native Instruments
    /// set-up (`setup`, `launch`, `bridge`).
    Plugins(String),
    /// Listen to the voice on the n-th audio track and set its strip up.
    Voice(usize),
    /// Global assets: `make` copies the selected asset there, `tab` shows
    /// the tab, `use:<n>` takes the n-th entry into the project.
    Global(String),
    /// Synchronise the selected asset (or, with `true`, every video) to the song.
    Sync(bool),
    /// Set the tempo choice of the song (-2 to 2).
    Tempo(i8),
    /// Put the selected asset into a scenario: `new`, `none` or the number of an existing one.
    Scenario(String),
    /// Add a look module to the shown scenario: `<kind>:<k0>,<k1>,<k2>,<k3>`.
    Look(String),
    /// Choose the look-up table file of the first scenario.
    Lut(String),
    /// Make the selected asset the hero of its scenario.
    Hero,
    /// Show a scale in the piano roll: its root (0 is C) and 1 for minor, 0 for major.
    Scale(u8, u8),
    /// Put the film looks that come with the program among the project's tables.
    BuiltinLooks,
    /// Make a copy of the selected video out of focus.
    Blurred,
    /// Find the key of the selected asset's music.
    FindKey,
    /// Make the selected clip's sky transparent.
    Sky,
    /// Change the selected clip by a sentence: `<service's name>:<sentence>`.
    Change(String),
    /// Cut the person out of the selected asset.
    CutOut,
    /// A copy of the selected asset's sound: semitones up, times as fast.
    Stretch(f32, f32),
    /// Open the "Chain" panel (the selected track's chain).
    Chain,
    /// Show the picture of this stage in the "Chain" panel, by its name there.
    Stage(String),
    /// Open the "Lyrics" tab with these words in the project ("|" for a new line).
    Lyrics(String),
    /// Set the sequence frame size.
    Size(u32, u32),
    /// Export with a preset: master, vertical, gif or share:<megabytes>.
    Export(String),
    /// Restore the n-th entry of the loaded history list (0 is the newest).
    Restore(usize),
    /// Zoom (frames per point) and scroll (frame at the left edge) of the timeline.
    TimelineView(f64, f64),
    /// The scripted zoom-and-scroll of the frame-time benchmark at this time in seconds.
    TimelineBench(f64),
    /// Import only the sound of a link.
    ImportSound(String),
    /// Select the n-th asset in the panel (0 is the newest).
    SelectAsset(usize),
}

#[derive(Default)]
pub struct Shell {
    library: Library,
    editor: Editor,
    session: Option<Session>,
    perf: Perf,
    error: Option<String>,
    history: HistoryPanel,
    export: ExportPanel,
    plugins: plugins_panel::PluginsPanel,
    playing: bool,
    /// Files dropped on the timeline: where the next imported assets go
    /// (frame, track row) and how many imports are still expected there.
    dropped_at: Option<(i64, Option<usize>, usize)>,
    /// Recorded takes being imported: the file and the timeline sample it starts at.
    takes: Vec<(PathBuf, i64)>,
    /// Clips that wait for a part of their video made anew.
    parts: Vec<PendingPart>,
    /// The window in which a clip is changed by a sentence, while it is open.
    ai: Option<ai_panel::AiPanel>,
    /// Which models there are for that (`None`: being read), and the file they are written in.
    ai_services: Option<Result<Vec<sg_project::ai::Service>, String>>,
    ai_file: String,
    /// The track that takes the recording in progress, and the timeline sample it was asked for at.
    record_onto: Option<(sg_core::TrackId, i64)>,
    /// How loud the tracks beside the voice that is being listened to are.
    beside_levels: Vec<(sg_core::TrackId, f32)>,
    /// The timeline sample the take in progress is to end at: the end of
    /// the looped stretch, if one was on when the recording began.
    record_until: Option<i64>,
    spent_takes: Vec<PathBuf>,
    /// What the application was last told about monitoring the input.
    monitor: Option<(Option<String>, bool)>,
    /// The settings the application was last told to hear the input through.
    monitor_chain: Option<sg_core::TrackSettings>,
    /// The loop the application was last told of.
    looping: Option<(i64, i64)>,
    /// The track the application was last told is played by hand.
    hand: Option<sg_core::TrackId>,
    /// The key reference is showing.
    help: bool,
    /// Undo group of the angle switches of the playback pass in progress.
    angle_group: Option<u64>,
    /// The playhead as of the last frame, for key actions.
    playhead: i64,
    /// The clipboard carries the mark of copied clips (as far as is known):
    /// Ctrl+V then asks the clipboard what it holds now.
    clips_offered: bool,
    /// The file the last export wrote and the notice that says so, for
    /// the "Show in folder" button beside that notice.
    exported: Option<(String, String)>,
    /// A file to show in the file manager; taken with the frame's commands.
    reveal: Option<PathBuf>,
}

impl Shell {
    pub fn new() -> Shell {
        Shell::default()
    }

    pub fn scope(&self) -> Scope {
        if self.session.is_some() { Scope::Editor } else { Scope::Library }
    }

    pub fn library_loaded(&self) -> bool {
        self.library.loaded()
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    pub fn perf_mut(&mut self) -> &mut Perf {
        &mut self.perf
    }

    pub fn set_library(&mut self, result: Result<Vec<ProjectEntry>, String>) {
        match result {
            Ok(entries) => {
                self.library.set_entries(entries);
                if self.library.pending.as_deref() == Some("Closing...") {
                    self.library.pending = None;
                }
            }
            Err(e) => self.error = Some(format!("Cannot read the library: {e}")),
        }
    }

    pub fn opened(&mut self, result: Result<Opened, String>) -> Vec<Command> {
        self.library.pending = None;
        match result {
            Ok(opened) => {
                self.library.filter.clear();
                self.editor = Editor::default();
                self.dropped_at = None;
                let session = Session::new(opened);
                let restore =
                    Command::RestoreCache { root: session.root.clone(), assets: session.project().assets.clone() };
                self.session = Some(session);
                vec![restore]
            }
            Err(e) => {
                self.error = Some(e);
                Vec::new()
            }
        }
    }

    /// The message in the banner, if there is one. For scripted runs, whose
    /// only reader is the log.
    pub fn error_shown(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Something worth saying that is not an error: shown where the project's notices are.
    pub fn notify(&mut self, text: String) {
        if let Some(session) = &mut self.session {
            session.notice = Some(text);
        }
    }

    pub fn report_error(&mut self, error: String) {
        self.library.pending = None;
        self.error = Some(error);
    }

    /// Builds one frame.
    pub fn frame(&mut self, ui: &mut egui::Ui, now_unix: u64, view: &ViewerState) -> Vec<Command> {
        let ctx = ui.ctx().clone();
        let mut commands = Vec::new();
        // The take being made grows with the playback, with the level that came in.
        if self.editor.recording_sound
            && view.playing
            && let Some(live) = &mut self.editor.timeline.live_take
            && let Some(rate) = self.session.as_ref().and_then(|s| s.project().sequences.first().map(|s| s.rate))
        {
            live.heard(rate.frames_to_samples(view.frame, 48_000), view.input_level);
            ctx.request_repaint();
        }
        // Before the shortcuts: while the piano roll's "Keyboard" is on, the letter keys are a piano.
        self.editor.piano.take_keys(&ctx);
        for action in keymap::take_actions(&ctx, self.scope()) {
            self.act(action, &mut commands);
        }
        self.banner(ui);
        if let Some(path) = self.reveal.take() {
            commands.push(Command::Reveal { path });
        }
        // The open note clip as a MIDI file, asked for in the piano roll.
        if let Some((stem, bytes)) = self.editor.piano.midi_request.take()
            && let Some(session) = &self.session
        {
            let root = session.root.clone();
            commands.push(Command::WriteExport { root, stem, extension: "mid".to_owned(), bytes });
        }
        let mut asset_requests = Vec::new();
        let mut transport = Vec::new();
        let mut scenario_choice = None;
        let mut colour_requests = Vec::new();
        self.playhead = view.frame;
        self.playing = view.playing;
        if !view.playing {
            // Cuts made in one pass of playing undo together; the next pass is its own step.
            self.angle_group = None;
        }
        match &mut self.session {
            Some(session) => {
                let request = self.editor.ui(ui, session, &mut self.error, &mut asset_requests, view, &mut transport);
                let root = session.root.clone();
                if let Some((asset, choice)) = self.editor.timeline.scenario_request.take() {
                    scenario_choice = Some((asset, choice));
                }
                colour_requests = std::mem::take(&mut self.editor.colour_requests);
                if std::mem::take(&mut self.editor.mixer.scan_request) {
                    commands.push(Command::Plugins(PluginRequest::Scan));
                }
                if std::mem::take(&mut self.editor.mixer.popover_request) {
                    self.plugins.open();
                    commands.push(Command::Plugins(PluginRequest::Scan));
                }
                if let Some((track, key)) = self.editor.mixer.editor_request.take() {
                    commands.push(Command::PluginEditor { track, key });
                }
                if let Some((track, style)) = self.editor.mixer.voice_request.take() {
                    commands.extend(Shell::measure_voice(
                        session,
                        &mut self.editor.mixer,
                        track,
                        style,
                        &mut self.error,
                    ));
                }
                if let Some(asset) = self
                    .editor
                    .clip_panel
                    .stabilise_request
                    .take()
                    .and_then(|id| session.project().assets.get(&id).cloned())
                {
                    commands.push(Command::Stabilise { root: root.clone(), asset });
                }
                if let Some(path) = self.editor.clip_panel.font_import.take() {
                    commands.push(Command::ImportFont { root: root.clone(), path });
                }
                if std::mem::take(&mut self.editor.clip_panel.fonts_request) {
                    commands.push(Command::LoadFonts { root: root.clone() });
                }
                if let Some((track, clip)) = self.editor.timeline.sky_request.take() {
                    commands.extend(self.ask_for_part(track, clip, Part::NoSky));
                }
                if let Some((track, clip)) = self.editor.timeline.sentence_request.take() {
                    self.open_ai(track, clip, &mut commands);
                }
                if let Some(id) = self.editor.timeline.beat_request.take() {
                    commands.push(Command::LoadBeats { root: root.clone(), id });
                }
                commands.extend(
                    self.editor
                        .timeline
                        .wave_requests
                        .drain(..)
                        .map(|id| Command::LoadPeaks { root: root.clone(), id }),
                );
                self.editor.assets.popover(&ctx, &mut asset_requests);
                self.editor.assets.drop_overlay(&ctx);
                match request {
                    Some(EditorRequest::Close) => self.act(Action::CloseProject, &mut commands),
                    Some(EditorRequest::Plugins) => {
                        self.plugins.open();
                        commands.push(Command::Plugins(PluginRequest::Scan));
                    }
                    Some(EditorRequest::Tempo(tempo)) => self.set_tempo(tempo),
                    Some(EditorRequest::CutsToBeats) => self.programme_action(true),
                    Some(EditorRequest::WhipAll) => self.programme_action(false),
                    Some(EditorRequest::Angle(angle)) => self.switch_angle(angle + 1),
                    Some(EditorRequest::SyncAll) => self.sync_takes(true, &mut commands),
                    Some(EditorRequest::ClearSong) => {
                        if let Some(session) = &mut self.session {
                            let id = session.project().sequences.first().map(|s| s.id);
                            if let Some(Err(e)) = id.map(|id| session.apply(Op::SetSong { id, song: None }, None)) {
                                self.error = Some(e.to_string());
                            }
                        }
                    }
                    Some(EditorRequest::RemoveAsset(id)) => self.remove_asset(id),
                    Some(EditorRequest::SetSize(width, height)) => {
                        if let Some(session) = &mut self.session {
                            self.editor.set_size(session, width, height, &mut self.error);
                        }
                    }
                    Some(EditorRequest::InstrumentTrack(plugin)) => self.instrument_track(plugin, &mut commands),
                    Some(EditorRequest::AddTrack(kind)) => {
                        if let Some(session) = &mut self.session {
                            self.editor.add_track(session, kind, &mut self.error);
                        }
                    }
                    None => {}
                }
                // Buttons and menu entries run what their keys run.
                let mut clicked = std::mem::take(&mut self.editor.actions);
                clicked.append(&mut self.editor.timeline.actions);
                for action in clicked {
                    self.act(action, &mut commands);
                }
            }
            None => {
                match self.library.ui(ui, now_unix) {
                    Some(LibraryRequest::Open(path)) => self.open(path, &mut commands),
                    Some(LibraryRequest::Create(name)) => self.create(name, &mut commands),
                    None => {}
                }
                commands.extend(self.library.poster_requests.drain(..).map(|path| Command::LoadProjectPoster { path }));
            }
        }
        for request in asset_requests {
            let Some(root) = self.session.as_ref().map(|s| s.root.clone()) else { break };
            match request {
                AssetRequest::LoadPoster(id) => commands.push(Command::LoadPoster { root, id }),
                AssetRequest::ImportLink(link, false) => commands.extend(self.incoming(Incoming::Text(link))),
                AssetRequest::ImportLink(link, true) => commands.extend(self.incoming(Incoming::Sound(link))),
                AssetRequest::Browse => commands.push(Command::PickFiles),
                AssetRequest::CancelJob(job) => commands.push(Command::CancelImport(job)),
                AssetRequest::Insert(only) => self.insert_selected(only, &mut commands),
                AssetRequest::UseAsSong => self.use_as_song(),
                AssetRequest::Scenario(choice) => {
                    if let Some(asset) = self.editor.assets.selected {
                        self.assign_scenario(asset, choice, &mut commands);
                    }
                }
                AssetRequest::Sync(all) => self.sync_takes(all, &mut commands),
                AssetRequest::AngleByHand => self.angle_by_hand(),
                AssetRequest::Remove => {
                    if let Some(id) = self.editor.assets.selected {
                        self.remove_asset(id);
                    }
                }
                AssetRequest::ShowFolder(file) => {
                    if let Some(session) = &self.session {
                        self.reveal = Some(session.root.join(file.as_deref().unwrap_or("assets")));
                    }
                }
                AssetRequest::Stretch(semitones, tempo) => {
                    let asset = self
                        .editor
                        .assets
                        .selected
                        .and_then(|id| self.session.as_ref()?.project().assets.get(&id).cloned());
                    if let (Some(asset), Some(session)) = (asset, &mut self.session) {
                        session.notice = Some(format!(
                            "Making a copy of {} at another pitch or tempo: it arrives as a new asset.",
                            asset.label
                        ));
                        commands.push(Command::Stretch { root: session.root.clone(), asset, semitones, tempo });
                    }
                }
                AssetRequest::Blurred => {
                    let asset = self
                        .editor
                        .assets
                        .selected
                        .and_then(|id| self.session.as_ref()?.project().assets.get(&id).cloned());
                    if let (Some(asset), Some(session)) = (asset, &mut self.session) {
                        session.notice =
                            Some(format!("Making a copy of {} out of focus: it arrives as a new asset.", asset.label));
                        commands.push(Command::Blurred { root: session.root.clone(), asset });
                    }
                }
                AssetRequest::FindKey => {
                    if let (Some(id), Some(session)) = (self.editor.assets.selected, &self.session) {
                        commands.push(Command::FindKey { root: session.root.clone(), id });
                    }
                }
                AssetRequest::CutOut => {
                    let asset = self
                        .editor
                        .assets
                        .selected
                        .and_then(|id| self.session.as_ref()?.project().assets.get(&id).cloned());
                    if let (Some(asset), Some(session)) = (asset, &mut self.session) {
                        session.notice = Some(format!(
                            "Cutting the person out of {}: it arrives as a new asset with a transparent background. \
                             This takes ten to twenty times as long as the clip; the first time, the model (15 MB) is fetched.",
                            asset.label
                        ));
                        commands.push(Command::CutOut { root: session.root.clone(), asset });
                    }
                }
                AssetRequest::ListGlobal => commands.push(Command::ListGlobal),
                AssetRequest::MakeGlobal => {
                    let asset = self
                        .editor
                        .assets
                        .selected
                        .and_then(|id| self.session.as_ref().and_then(|s| s.project().assets.get(&id).cloned()));
                    if let Some(asset) = asset {
                        commands.push(Command::MakeGlobal { root, asset });
                    }
                }
                AssetRequest::UseGlobal(path) => commands.extend(self.incoming(Incoming::Paths(vec![path]))),
                AssetRequest::RemoveGlobal(file) => commands.push(Command::RemoveGlobal { file }),
                AssetRequest::LoadGlobalPoster(file) => commands.push(Command::LoadGlobalPoster { file }),
            }
        }
        commands.extend(transport.into_iter().map(Command::Transport));
        if let Some((asset, choice)) = scenario_choice {
            self.assign_scenario(asset, choice, &mut commands);
        }
        self.colour_requests(colour_requests, &mut commands);
        self.hand_commands(&mut commands);
        self.relisten(&mut commands);
        for request in std::mem::take(&mut self.editor.input_requests) {
            commands.push(match request {
                editor::InputRequest::List => Command::ListInputs,
                editor::InputRequest::Choose(input) => Command::SaveInput(input),
            });
        }
        self.monitor_commands(&mut commands);
        let gif_range = self.gif_range();
        match self.plugins.ui(&ctx) {
            Some(PluginRequest::Play(name, path)) => self.instrument_track(Some((name, path)), &mut commands),
            Some(request) => {
                if let PluginRequest::Run(step) = request {
                    self.plugins.started(step);
                }
                commands.push(Command::Plugins(request));
            }
            None => {}
        }
        if let Some(panel) = &mut self.ai {
            match panel.ui(&ctx, self.ai_services.as_ref(), &self.ai_file) {
                Some(ai_panel::AiAction::Send(service, sentence)) => {
                    let (track, clip) = (panel.track, panel.clip);
                    self.ai = None;
                    commands.extend(self.ask_for_part(track, clip, Part::Sentence { service, sentence }));
                }
                Some(ai_panel::AiAction::Save(services)) => {
                    // (Shown at once; the file is written by a job, which says so if it cannot.)
                    self.ai_services = Some(Ok(services.clone()));
                    commands.push(Command::SaveAiServices(services));
                }
                Some(ai_panel::AiAction::Close) => {
                    if panel.scripted.is_some() {
                        self.error = Some("change: no such service in the file".to_owned());
                    }
                    self.ai = None;
                }
                None => {}
            }
        }
        match self.export.ui(&ctx, gif_range) {
            Some(ExportAction::Start(request)) => self.start_export(request, &mut commands),
            Some(ExportAction::Cancel) => commands.push(Command::CancelExport),
            Some(ExportAction::ShowFolder) => {
                if let Some(session) = &self.session {
                    self.reveal = Some(session.root.join("exports"));
                }
            }
            None => {}
        }
        for request in self.history.ui(&ctx, now_unix) {
            match request {
                HistoryRequest::Checkpoint(name) => {
                    if let Some(session) = &mut self.session {
                        session.checkpoint(name.clone());
                        session.notice = Some(format!("Checkpoint \"{name}\" saved."));
                    }
                }
                HistoryRequest::Restore { commit, description } => {
                    if let Some(session) = self.session.take() {
                        self.library.pending = Some("Restoring...".to_owned());
                        let root = session.root.clone();
                        commands.push(Command::Restore { root, writer: session.into_writer(), commit, description });
                    }
                }
            }
        }
        if self.help {
            help_panel(&ctx);
        }
        self.perf.ui(&ctx, self.session.as_ref(), &view.stats);
        commands
    }

    /// Errors and notices: a strip at the bottom, never a dialog.
    fn banner(&mut self, ui: &mut egui::Ui) {
        // A running export shows here while its popover is closed.
        let notice = self.export.status().or_else(|| self.session.as_ref().and_then(|s| s.notice.clone()));
        let (text, colour) = match (&self.error, notice) {
            (Some(e), _) => (e.clone(), theme::ERROR),
            (None, Some(n)) => (n, theme::ACCENT),
            (None, None) => return,
        };
        let frame = egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(12, 6));
        egui::Panel::bottom("banner").frame(frame).show(ui, |ui| {
            // (Over several lines if it is long: what "Voice" has to say does not fit one.)
            ui.horizontal_wrapped(|ui| {
                // Beside the news of an export: the way to the file.
                let file = self.exported.as_ref().filter(|(_, notice)| *notice == text).map(|(path, _)| path.clone());
                ui.label(egui::RichText::new(text).color(colour));
                if let Some(file) = file
                    && ui.button("Show in folder").on_hover_text("Opens the file manager where the file is").clicked()
                {
                    self.reveal = Some(PathBuf::from(file));
                }
                ui.label(egui::RichText::new("Esc to dismiss").color(theme::DIM));
            });
        });
    }

    fn open(&mut self, path: PathBuf, commands: &mut Vec<Command>) {
        self.library.pending = Some("Opening...".to_owned());
        commands.push(Command::Open { path });
    }

    fn create(&mut self, name: String, commands: &mut Vec<Command>) {
        self.library.pending = Some("Creating...".to_owned());
        commands.push(Command::Create { name });
    }

    fn act(&mut self, action: Action, commands: &mut Vec<Command>) {
        match action {
            Action::TogglePerf => self.perf.show = !self.perf.show,
            Action::Cancel => {
                // Dismiss one thing per press, most urgent first.
                if self.error.take().is_some()
                    || std::mem::take(&mut self.help)
                    || self.history.cancel()
                    || self.export.cancel()
                    || self.plugins.cancel()
                    || self.editor.cancel()
                {
                    return;
                }
                match &mut self.session {
                    Some(session) => session.notice = None,
                    None => self.library.filter.clear(),
                }
            }
            Action::NewProject => {
                if self.session.is_none() && self.library.pending.is_none() {
                    let name = self.library.filter.trim().to_owned();
                    self.create(name, commands);
                }
            }
            Action::CloseProject => {
                if let Some(session) = self.session.take() {
                    let (root, poster) = (session.root.clone(), poster_asset(session.project()));
                    // (A running export still writes into the folder; and the
                    // way to the last one's file would lead nowhere after a rename.)
                    let rename = !self.export.is_running() && self.editor.assets.jobs.is_empty();
                    self.exported = None;
                    // (Until the library has been read again, a tile may point at a folder that is being renamed.)
                    self.library.pending = Some("Closing...".to_owned());
                    commands.push(Command::Close { writer: session.into_writer(), root, poster, rename });
                }
            }
            Action::Undo | Action::Redo => {
                let Some(session) = &mut self.session else { return };
                let result = if action == Action::Undo { session.undo() } else { session.redo() };
                if let Err(e) = result {
                    self.error = Some(format!("History is damaged: {e}"));
                }
            }
            Action::Rename => {
                if let Some(session) = &self.session {
                    self.editor.begin_rename(session);
                }
            }
            Action::AddAsset => {
                if self.session.is_some() {
                    self.editor.assets.open_add();
                }
            }
            // While the piano roll is open these mean its notes.
            Action::Pitch(semitones) => {
                if let Some(session) = self.session.as_mut().filter(|_| self.editor.piano.is_open()) {
                    self.editor.piano.shift_selected(session, semitones, 0, &mut self.error);
                }
            }
            Action::Nudge(steps) if self.editor.piano.is_open() && self.editor.piano.has_selection() => {
                // A sixteenth; with Shift (ten steps) a beat.
                let ticks = if steps.abs() >= 10 { 960 } else { 240 } * steps.signum() as i64;
                if let Some(session) = &mut self.session {
                    self.editor.piano.shift_selected(session, 0, ticks, &mut self.error);
                }
            }
            Action::Paste | Action::PasteClips if self.editor.piano.is_open() => {
                if let Some(session) = &mut self.session {
                    self.editor.piano.paste(session, &mut self.error);
                }
            }
            Action::Copy | Action::Cut if self.editor.piano.is_open() => {
                let copied = self.editor.piano.copy();
                if let (true, Action::Cut, Some(session)) = (copied, action, &mut self.session) {
                    self.editor.piano.delete_selected(session, &mut self.error);
                }
            }
            Action::SelectAll if self.editor.piano.is_open() => {
                if let Some(session) = &self.session {
                    self.editor.piano.select_all(session);
                }
            }
            Action::Paste => {
                if self.session.is_some() {
                    // Copied clips are pasted, unless something was copied
                    // elsewhere since: the clipboard knows (F4.4). Without a
                    // clipboard to ask, the clips win.
                    if self.editor.timeline.has_copied() && !self.clips_offered {
                        self.timeline_action(Action::PasteClips, commands);
                    } else {
                        commands.push(Command::ReadClipboard);
                    }
                }
            }
            Action::Copy | Action::Cut => {
                // (Nothing selected: nothing copied, and the clipboard stays as it is.)
                if self.editor.timeline.has_selection() {
                    self.timeline_action(action, commands);
                    self.clips_offered = false;
                    commands.push(Command::OfferClips);
                }
            }
            Action::PlayPause => commands.push(Command::Transport(Transport::Toggle)),
            Action::StepBack => commands.push(Command::Transport(Transport::Step(-1))),
            Action::StepForward => commands.push(Command::Transport(Transport::Step(1))),
            Action::ToStart => commands.push(Command::Transport(Transport::Seek(0))),
            Action::ToEnd => commands.push(Command::Transport(Transport::Seek(i64::MAX))),
            Action::InsertAsset => self.insert_selected(None, commands),
            Action::Split
            | Action::RippleStart
            | Action::RippleEnd
            | Action::RippleDelete
            | Action::SelectUnder
            | Action::SelectAll
            | Action::Group
            | Action::PasteClips
            | Action::Marker
            | Action::Nudge(_)
            | Action::PrevCut
            | Action::NextCut => self.timeline_action(action, commands),
            Action::Delete => {
                // While the piano roll is open: its selected note. Otherwise
                // clips if any are selected, otherwise the selected asset.
                let note = match &mut self.session {
                    Some(session) => self.editor.piano.delete_selected(session, &mut self.error),
                    None => false,
                };
                if note || self.editor.piano.is_open() {
                    // (An open piano roll never lets the key reach the clip it shows.)
                } else if self.editor.timeline.has_selection() {
                    self.timeline_action(action, commands);
                } else if let Some(id) = self.editor.assets.selected {
                    self.remove_asset(id);
                }
            }
            Action::Metronome => {
                let tempo = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.project().sequences.first().is_some_and(|q| q.tempo.is_some()));
                if tempo || self.editor.metronome {
                    self.editor.metronome = !self.editor.metronome;
                    commands.push(Command::Metronome(self.editor.metronome));
                } else {
                    self.error = Some("The metronome needs a tempo: set one under Sequence, on the right.".to_owned());
                }
            }
            Action::Loop => {
                // (The application is told by `hand_commands`, with what else follows the open project.)
                if self.editor.timeline.loop_range.take().is_some() {
                } else if let Some(range) = self.loop_range() {
                    self.editor.timeline.loop_range = Some(range);
                } else if self.session.is_some() {
                    self.error = Some(
                        "Nothing to loop yet: open a note clip, select clips, or set a marker before and one after the \
                         playhead."
                            .to_owned(),
                    );
                }
            }
            Action::RecordSound => {
                if let Some(session) = &self.session {
                    if self.editor.recording_sound {
                        commands.push(Command::StopRecording);
                    } else {
                        self.editor.recording_sound = true;
                        let (root, source) = (session.root.clone(), self.editor.input.clone());
                        let armed = self.record_target(true).is_some();
                        // Where the take goes is settled now, so that it can be said and
                        // shown while it is made (feedback F16.10): the track marked "R",
                        // else the lowest audio track that is free from the playhead on,
                        // else a new one.
                        self.record_onto = self.record_target(false);
                        if self.record_onto.is_none()
                            && let Some(session) = &mut self.session
                        {
                            if let Err(e) = session.add_track(sg_core::TrackKind::Audio) {
                                self.error = Some(e.to_string());
                            }
                            self.record_onto = self.record_target(false);
                        }
                        // Punching in (`PROPOSED`): with a stretch marked to be looped
                        // that is not behind the playhead, only that stretch is
                        // replaced, however early the playing starts and however
                        // long it goes on. (The loop itself rests while recording.)
                        let stretch = self
                            .editor
                            .timeline
                            .loop_range
                            .filter(|(_, to)| self.record_onto.is_some_and(|(_, from)| from < *to));
                        self.record_until = stretch.map(|(_, to)| to);
                        if let (Some((from, _)), Some(onto)) = (stretch, &mut self.record_onto) {
                            onto.1 = onto.1.max(from);
                        }
                        let punched = stretch.is_some();
                        self.editor.timeline.live_take = self.record_onto.map(|(track, from)| {
                            crate::timeline::LiveTake { track, from, to: from, peaks: Vec::new() }
                        });
                        if let (Some((track, _)), Some(session)) = (self.record_onto, &mut self.session) {
                            let name =
                                session.project().sequences.first().and_then(|s| Some(s.track(track)?.name.clone()));
                            let name = name.unwrap_or_default();
                            session.notice = Some(if punched {
                                format!("Recording onto {name}: only the looped stretch is replaced.")
                            } else if armed {
                                format!("Recording onto {name}, from the playhead on.")
                            } else {
                                format!(
                                    "Recording onto {name}. To choose the track yourself, press R in its header first."
                                )
                            });
                        }
                        // (A lead-in only when punching into a track that was chosen for it.)
                        commands.push(Command::Record { root, source, lead_in: armed });
                    }
                }
            }
            Action::KeyframeSet | Action::KeyframeRemove | Action::AddTitle | Action::LiveRecord => {
                if let Some(session) = &mut self.session {
                    let transport =
                        self.editor.clip_action(action, session, self.playhead, self.playing, &mut self.error);
                    commands.extend(transport.into_iter().map(Command::Transport));
                }
            }
            Action::Export => {
                if self.session.is_some() {
                    self.export.open();
                }
            }
            Action::ExportAgain => match self.export.repeat() {
                Some(request) => self.start_export(request, commands),
                None => self.export.open(),
            },
            Action::ExportGif => {
                let range = self.gif_range();
                self.start_export(
                    ExportRequest {
                        preset: ExportPreset::Gif,
                        megabytes: 10,
                        compression: self.export.compression(),
                        range,
                        to_clipboard: true,
                        sound_format: 0,
                        stems: false,
                        loudness: None,
                    },
                    commands,
                );
            }
            Action::Checkpoint => {
                if self.session.is_some() {
                    self.history.begin_checkpoint();
                }
            }
            Action::Help => self.help = !self.help,
            Action::FitTimeline => self.editor.timeline.refit(),
            Action::Mixer => {
                self.editor.mixer.open = !self.editor.mixer.open;
                self.editor.mixer.chain = false;
                self.editor.colour.open = false;
            }
            Action::Colour => {
                self.editor.colour.open = !self.editor.colour.open;
                self.editor.mixer.open = false;
            }
            Action::ToggleLook | Action::ToggleMatch => {
                if let Some(session) = &mut self.session {
                    self.editor.colour.toggle(session, action == Action::ToggleMatch, &mut self.error);
                }
            }
            Action::Transition | Action::Blackout => {
                if let Some(session) = &mut self.session {
                    self.editor.mixer.open = false;
                    let kind = match action {
                        Action::Blackout => sg_core::TransitionKind::Black,
                        _ => sg_core::TransitionKind::Whip,
                    };
                    if self.editor.timeline.toggle_transition(session, self.playhead, kind, &mut self.error).is_some() {
                        // Its settings, where they can all be seen.
                        self.editor.colour.open = false;
                        self.editor.clip_panel.show_transition();
                    }
                }
            }
            Action::Angle(n) => self.switch_angle(n as usize),
            Action::History => {
                if let Some(session) = &self.session {
                    self.history.open();
                    commands.push(Command::LoadHistory { root: session.root.clone() });
                }
            }
        }
    }

    /// Runs a scripted step through the same paths as keys and clicks.
    pub fn dev(&mut self, step: DevStep) -> Vec<Command> {
        let mut commands = self.dev_step(step);
        // (Scripted runs draw no frames: what a frame would pass on is passed on here.)
        self.hand_commands(&mut commands);
        commands
    }

    fn dev_step(&mut self, step: DevStep) -> Vec<Command> {
        let mut commands = Vec::new();
        match step {
            DevStep::Filter(text) => self.library.filter = text,
            DevStep::New(name) => {
                self.library.filter = name;
                self.act(Action::NewProject, &mut commands);
            }
            DevStep::Open(slug) => match self.library.find_slug(&slug).map(|e| e.path.clone()) {
                Some(path) => self.open(path, &mut commands),
                None => self.error = Some(format!("No project folder named {slug}")),
            },
            DevStep::Rename(name) => {
                if let Some(session) = &mut self.session
                    && let Err(e) = session.apply(Op::SetProjectName { name }, None)
                {
                    self.error = Some(e.to_string());
                }
            }
            DevStep::Act(action) => self.act(action, &mut commands),
            DevStep::Transport(transport) => commands.push(Command::Transport(transport)),
            DevStep::Prop(name, value) => {
                if let Some(session) = &mut self.session {
                    self.editor.set_prop(session, self.playhead, &name, &value, &mut self.error);
                }
            }
            DevStep::AddTrack(kind) => {
                if let Some(session) = &mut self.session {
                    self.editor.add_track(session, kind, &mut self.error);
                }
            }
            DevStep::Drop(frame, row, only) => {
                if let Some(session) = &mut self.session
                    && let Some(asset) =
                        self.editor.assets.selected.and_then(|id| session.project().assets.get(&id).cloned())
                {
                    match session.drop_asset(&asset, frame, row, only) {
                        Ok(_) => self.editor.timeline.select_dropped(session, asset.id, frame),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
            }
            DevStep::Mix(which, field, value) => {
                if let Some(session) = &mut self.session {
                    let project = session.project().clone();
                    let sequence = project.sequences.first();
                    let audio =
                        |n: usize| sequence?.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n);
                    let op = match (sequence, which.parse().ok().and_then(audio)) {
                        // master:room=<seconds>, master:repeat=<percent>, master:note=<0 to 4>; else the fader.
                        (Some(seq), _) if which == "master" => {
                            let space = seq.space;
                            Some(match field.as_str() {
                                "room" => Op::SetSpace {
                                    id: seq.id,
                                    space: sg_core::Space { room_ds: (value * 10.0).round() as u8, ..space },
                                },
                                "repeat" => Op::SetSpace {
                                    id: seq.id,
                                    space: sg_core::Space { echo_repeat: value.round() as u8, ..space },
                                },
                                "note" => Op::SetSpace {
                                    id: seq.id,
                                    space: sg_core::Space {
                                        echo_note: sg_core::EchoNote::ALL[(value as usize).min(4)],
                                        ..space
                                    },
                                },
                                _ => Op::SetMasterGain { id: seq.id, db: value },
                            })
                        }
                        (Some(seq), Some(track)) => {
                            let mut mix = track.mix;
                            mixer::set_field(&mut mix, &field, value).then_some(Op::SetTrackMix {
                                seq: seq.id,
                                track: track.id,
                                mix,
                            })
                        }
                        _ => None,
                    };
                    match op.map(|op| session.apply(op, None)) {
                        Some(Ok(())) => {}
                        Some(Err(e)) => self.error = Some(e.to_string()),
                        None => self.error = Some(format!("mix: no track or field {which}:{field}")),
                    }
                }
            }
            DevStep::Scenario(which) => {
                let choice = match which.as_str() {
                    "new" => Some(session::ScenarioChoice::New),
                    "none" => Some(session::ScenarioChoice::None),
                    n => self
                        .session
                        .as_ref()
                        .and_then(|s| s.project().scenarios.keys().nth(n.parse().ok()?).copied())
                        .map(session::ScenarioChoice::Existing),
                };
                match (choice, self.editor.assets.selected) {
                    (Some(choice), Some(asset)) => self.assign_scenario(asset, choice, &mut commands),
                    _ => self.error = Some(format!("scenario: nothing selected or no scenario {which}")),
                }
            }
            DevStep::Look(what) => self.script_look(&what),
            DevStep::Lut(name) => {
                // The table of the first scenario.
                if let Some(session) = &mut self.session
                    && let Some(scenario) = session.project().scenarios.values().next().cloned()
                {
                    let edited = sg_core::Scenario { lut: name, ..sg_core::Scenario::clone(&scenario) };
                    if let Err(e) =
                        session.apply(Op::SetScenario { id: edited.id, scenario: Some(Box::new(edited)) }, None)
                    {
                        self.error = Some(e.to_string());
                    }
                }
            }
            DevStep::Lyrics(text) => {
                self.editor.lyrics.open = true;
                if let Some(session) = &mut self.session
                    && let Err(e) = session.apply(Op::SetLyrics { text: text.replace('|', "\n") }, None)
                {
                    self.error = Some(e.to_string());
                }
            }
            DevStep::Chain => {
                (self.editor.mixer.open, self.editor.mixer.chain) = (true, true);
                self.editor.colour.open = false;
            }
            DevStep::Stretch(semitones, tempo) => {
                let asset = self
                    .editor
                    .assets
                    .selected
                    .and_then(|id| self.session.as_ref()?.project().assets.get(&id).cloned());
                match (asset, &self.session) {
                    (Some(asset), Some(session)) => {
                        commands.push(Command::Stretch { root: session.root.clone(), asset, semitones, tempo })
                    }
                    _ => self.error = Some("stretch: no asset is selected".to_owned()),
                }
            }
            DevStep::Scale(root, minor) => self.editor.piano.scale = Some((root % 12, minor != 0)),
            DevStep::BuiltinLooks => {
                if let Some(session) = &self.session {
                    commands.push(Command::AddBuiltinLooks { root: session.root.clone() });
                }
            }
            DevStep::Blurred => {
                let asset = self
                    .editor
                    .assets
                    .selected
                    .and_then(|id| self.session.as_ref()?.project().assets.get(&id).cloned());
                match (asset, &self.session) {
                    (Some(asset), Some(session)) => {
                        commands.push(Command::Blurred { root: session.root.clone(), asset })
                    }
                    _ => self.error = Some("blurred: no asset is selected".to_owned()),
                }
            }
            DevStep::Sky | DevStep::Change(_) => {
                let chosen = self.session.as_ref().and_then(|session| {
                    let sequence = session.project().sequences.first()?.clone();
                    // (The picture of the selected take, not its sound.)
                    let (track, clip) = self.editor.timeline.selected_picture(&sequence)?;
                    Some((track.id, clip))
                });
                match (chosen, step) {
                    (Some(request), DevStep::Change(told)) => {
                        let (service, sentence) = told.split_once(':').unwrap_or((told.as_str(), ""));
                        self.open_ai(request.0, request.1, &mut commands);
                        // (Without a service's name the window only opens, to be looked at; `+`: on its fields for a new model.)
                        if let Some(panel) = self.ai.as_mut().filter(|_| service == "+") {
                            panel.add();
                        } else if let Some(panel) = self.ai.as_mut().filter(|_| !service.is_empty()) {
                            panel.scripted = Some(service.to_owned());
                            panel.sentence = sentence.to_owned();
                        }
                    }
                    (Some(request), _) => self.editor.timeline.sky_request = Some(request),
                    (None, _) => self.error = Some("no clip of a video is selected".to_owned()),
                }
            }
            DevStep::FindKey => {
                if let (Some(id), Some(session)) = (self.editor.assets.selected, &self.session) {
                    commands.push(Command::FindKey { root: session.root.clone(), id });
                }
            }
            DevStep::CutOut => {
                let asset = self
                    .editor
                    .assets
                    .selected
                    .and_then(|id| self.session.as_ref()?.project().assets.get(&id).cloned());
                match (asset, &self.session) {
                    (Some(asset), Some(session)) => {
                        commands.push(Command::CutOut { root: session.root.clone(), asset })
                    }
                    _ => self.error = Some("cutout: no asset is selected".to_owned()),
                }
            }
            DevStep::Stage(name) => match scope::Stage::named(&name) {
                Some(stage) => self.editor.mixer.stage = Some(stage),
                None => self.error = Some(format!("no stage called {name}")),
            },
            DevStep::Hero => {
                // The selected asset becomes what its scenario is matched to.
                if let (Some(session), Some(asset)) = (&mut self.session, self.editor.assets.selected)
                    && let Some(scenario) = session.project().scenario_of(asset).cloned()
                {
                    let edited = sg_core::Scenario { hero: Some(asset), ..sg_core::Scenario::clone(&scenario) };
                    if let Err(e) =
                        session.apply(Op::SetScenario { id: edited.id, scenario: Some(Box::new(edited)) }, None)
                    {
                        self.error = Some(e.to_string());
                    }
                }
            }
            DevStep::Song => self.use_as_song(),
            DevStep::Programme(beats) => self.programme_action(beats),
            DevStep::Sync(all) => self.sync_takes(all, &mut commands),
            DevStep::Tempo(tempo) => self.set_tempo(tempo),
            DevStep::NoteClip(n, frame) => {
                if let Some(session) = &mut self.session {
                    let project = session.project().clone();
                    let found = project.sequences.first().and_then(|s| {
                        Some((s, s.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n)?))
                    });
                    match found {
                        Some((sequence, track)) => {
                            self.editor.timeline.add_note_clip(session, sequence, track, frame as f64, &mut self.error);
                            if let Some((track, clip)) = self.editor.timeline.edit_notes.take() {
                                self.editor.piano.show(track, clip);
                            }
                        }
                        None => self.error = Some(format!("noteclip: no audio track {n}")),
                    }
                }
            }
            DevStep::Hand(pitch, on) => self.editor.piano.play(pitch, on),
            DevStep::RecordNotes => self.editor.piano.toggle_record(),
            DevStep::Arm(n) => {
                let track = self.session.as_ref().and_then(|s| {
                    let project = s.project();
                    let tracks = &project.sequences.first()?.tracks;
                    tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n).map(|t| t.id)
                });
                self.editor.timeline.record_track = track;
            }
            DevStep::Sampler(n, root) => {
                if let Some(session) = &mut self.session {
                    let project = session.project().clone();
                    let track = project.sequences.first().and_then(|s| {
                        let track = s.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n)?;
                        Some((s.id, track.id))
                    });
                    let asset = self.editor.assets.selected.and_then(|id| project.assets.get(&id).cloned());
                    match (track, asset) {
                        (Some((seq, track)), Some(asset)) => {
                            let seconds =
                                asset.audio.as_ref().map_or(0.0, |i| i.samples as f64 / i.sample_rate.max(1) as f64);
                            let sampler = Some(sg_core::Sampler { asset: asset.id, root, one_shot: seconds < 1.5 });
                            if let Err(e) = session.apply(Op::SetTrackSampler { seq, track, sampler }, None) {
                                self.error = Some(e.to_string());
                            }
                        }
                        _ => self.error = Some(format!("sampler: no audio track {n} or no asset selected")),
                    }
                }
            }
            DevStep::Monitor(hear) => {
                let flag = if hear { &mut self.editor.hear_input } else { &mut self.editor.monitor };
                *flag = !*flag;
                self.monitor_commands(&mut commands);
            }
            DevStep::Note(n, pitch, start, len) => {
                if let Some(session) = &mut self.session {
                    let project = session.project().clone();
                    let found = project.sequences.first().and_then(|s| {
                        let track = s.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n)?;
                        let clip = track.clips.iter().filter(|c| c.asset == AssetId(0)).last()?;
                        Some((
                            s.id,
                            track.id,
                            clip.id,
                            track.notes.get(&clip.id).map(|n| Vec::clone(n)).unwrap_or_default(),
                        ))
                    });
                    match found {
                        Some((seq, track, clip, mut notes)) => {
                            notes.push(sg_core::Note { start, len, pitch, velocity: 100 });
                            if let Err(e) =
                                session.apply(Op::SetClipNotes { seq, track, clip, notes: Some(notes) }, None)
                            {
                                self.error = Some(e.to_string());
                            }
                        }
                        None => self.error = Some(format!("note: no note clip on audio track {n}")),
                    }
                }
            }
            DevStep::Plugin(n, path) => {
                if let Some(session) = &mut self.session {
                    let project = session.project().clone();
                    let found = project.sequences.first().and_then(|s| {
                        let track = s.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n)?;
                        Some((s.id, track.clone()))
                    });
                    match found {
                        // `window`: of the plugin added last.
                        Some((_, track)) if path == "window" => {
                            if let Some(last) = track.plugins.last() {
                                commands.push(Command::PluginEditor { track: track.id, key: last.key(track.id) });
                            }
                        }
                        // A path adds that plugin after those there are; an empty one takes all off.
                        Some((seq, track)) => {
                            let name =
                                std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned());
                            let mut plugins = Vec::clone(&track.plugins);
                            match name.filter(|_| !path.is_empty()) {
                                Some(name) => {
                                    plugins.push(sg_core::PluginRef { name, path, id: session.new_plugin_id() });
                                }
                                None => plugins.clear(),
                            }
                            if let Err(e) = session.apply(Op::SetTrackPlugins { seq, track: track.id, plugins }, None) {
                                self.error = Some(e.to_string());
                            }
                        }
                        None => self.error = Some(format!("plugin: no audio track {n}")),
                    }
                }
            }
            DevStep::Plugins(what) => {
                let request = match what.as_str() {
                    "setup" => PluginRequest::Run(NiStep::Setup),
                    "launch" => PluginRequest::Run(NiStep::Launch),
                    "bridge" => PluginRequest::Run(NiStep::Bridge),
                    "check" => PluginRequest::Run(NiStep::Check),
                    _ => PluginRequest::Scan,
                };
                self.plugins.open();
                if let PluginRequest::Run(step) = request {
                    self.plugins.started(step);
                }
                commands.push(Command::Plugins(request));
            }
            DevStep::Voice(n) => {
                if let Some(session) = &self.session {
                    let track = session.project().sequences.first().and_then(|s| {
                        s.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).nth(n).map(|t| t.id)
                    });
                    match track {
                        Some(track) => commands.extend(Shell::measure_voice(
                            session,
                            &mut self.editor.mixer,
                            track,
                            None,
                            &mut self.error,
                        )),
                        None => self.error = Some(format!("voice: no audio track {n}")),
                    }
                }
            }
            DevStep::Bars(bpm) => {
                if let Some(session) = &mut self.session
                    && let Some(id) = session.project().sequences.first().map(|s| s.id)
                {
                    let tempo = (bpm > 0.0).then(|| sg_core::Tempo::new((bpm * 1000.0).round() as u32, 4));
                    if let Err(e) = session.apply(Op::SetTempo { id, tempo }, None) {
                        self.error = Some(e.to_string());
                    }
                }
            }
            DevStep::Global(what) => {
                let selected = self.editor.assets.selected;
                let session = self.session.as_ref();
                let asset = selected.and_then(|id| session.and_then(|s| s.project().assets.get(&id).cloned()));
                match (what.as_str(), asset, session.map(|s| s.root.clone())) {
                    ("make", Some(asset), Some(root)) => commands.push(Command::MakeGlobal { root, asset }),
                    ("tab", ..) => {
                        self.editor.assets.show_global();
                        commands.push(Command::ListGlobal);
                    }
                    (other, ..) => {
                        match other.strip_prefix("use:").and_then(|n| self.editor.assets.global_entry(n.parse().ok()?))
                        {
                            Some(path) => commands.extend(self.incoming(Incoming::Paths(vec![path]))),
                            None => self.error = Some(format!("global: cannot {what}")),
                        }
                    }
                }
            }
            DevStep::Size(width, height) => {
                if let Some(session) = &mut self.session {
                    self.editor.set_size(session, width, height, &mut self.error);
                }
            }
            DevStep::Export(what) => {
                // `gif:<0-4>` and `share:<MB>:<0-4>` name the compression; 1 (light) without.
                let parts: Vec<&str> = what.split(':').collect();
                let level = |part: Option<&&str>| part.and_then(|l| l.parse().ok()).unwrap_or(1);
                // `sound[:wav|flac|mp3][:stems][:<LUFS>]`
                let sound_format = ["wav", "flac", "mp3"].iter().position(|f| parts.contains(f)).unwrap_or(0) as u8;
                let stems = parts.contains(&"stems");
                let loudness =
                    parts.iter().skip(1).find_map(|part| part.parse::<f32>().ok()).filter(|_| parts[0] == "sound");
                let (preset, megabytes, compression) = match parts.as_slice() {
                    ["sound", ..] => (ExportPreset::Sound, 10, 1),
                    ["share", mb, rest @ ..] => (ExportPreset::Share, mb.parse().unwrap_or(10), level(rest.first())),
                    ["vertical"] => (ExportPreset::Vertical, 10, 1),
                    ["gif", rest @ ..] => (ExportPreset::Gif, 10, level(rest.first())),
                    _ => (ExportPreset::Master, 10, 1),
                };
                let range = if preset == ExportPreset::Gif { self.gif_range() } else { None };
                // Scripted exports never touch the clipboard: it belongs to the person at the desk.
                let request = ExportRequest {
                    preset,
                    megabytes,
                    compression,
                    range,
                    to_clipboard: false,
                    sound_format,
                    stems,
                    loudness,
                };
                self.start_export(request, &mut commands);
            }
            DevStep::Checkpoint(name) => {
                if let Some(session) = &self.session {
                    session.checkpoint(name);
                }
            }
            DevStep::Restore(n) => {
                let entry = self.history.entry(n);
                if let (Some((commit, description)), Some(session)) = (entry, self.session.take()) {
                    let root = session.root.clone();
                    commands.push(Command::Restore { root, writer: session.into_writer(), commit, description });
                }
            }
            DevStep::TimelineView(frames_per_point, left) => self.editor.timeline.set_view(frames_per_point, left),
            DevStep::TimelineBench(t) => {
                if let Some(sequence) = self.session.as_ref().and_then(|s| s.project().sequences.first().cloned()) {
                    self.editor.timeline.bench_view(t, &sequence);
                }
            }
            DevStep::Import(what) => {
                let incoming = if sg_project::fetch::looks_like_url(&what) {
                    Incoming::Text(what)
                } else {
                    Incoming::Paths(vec![what.into()])
                };
                commands.extend(self.incoming(incoming));
            }
            DevStep::ImportSound(link) => commands.extend(self.incoming(Incoming::Sound(link))),
            DevStep::SelectAsset(n) => {
                if let Some(session) = &self.session {
                    let mut assets: Vec<&Arc<Asset>> = session.project().assets.values().collect();
                    assets
                        .sort_by(|a, b| b.source.imported.cmp(&a.source.imported).then_with(|| a.label.cmp(&b.label)));
                    self.editor.assets.selected = assets.get(n).map(|a| a.id);
                }
            }
        }
        commands
    }
}

impl Shell {
    /// Hands over the writer of the open project at shutdown, if any.
    pub fn take_writer(&mut self) -> Option<ProjectWriter> {
        self.session.take().map(Session::into_writer)
    }
}

impl Shell {
    fn remove_asset(&mut self, id: AssetId) {
        let Some(session) = &mut self.session else { return };
        match session.apply(Op::RemoveAsset { id }, None) {
            Ok(()) => self.editor.assets.selected = None,
            Err(sg_core::OpError::AssetInUse(_)) => {
                self.error = Some("This asset is still used on the timeline. Remove its clips first.".to_owned());
            }
            Err(e) => self.error = Some(format!("Cannot remove: {e}")),
        }
    }

    /// What the global folder holds, or why it could not be read. `news`
    /// is what just happened there, for the notice line.
    pub fn global_listed(
        &mut self,
        result: Result<Vec<sg_project::global::GlobalEntry>, String>,
        news: Option<String>,
    ) {
        match result {
            Ok(entries) => {
                self.editor.assets.set_global(entries);
                if let (Some(news), Some(session)) = (news, &mut self.session) {
                    session.notice = Some(news);
                }
            }
            Err(e) => self.error = Some(format!("Global assets: {e}")),
        }
    }

    pub fn global_poster_loaded(&mut self, ctx: &egui::Context, file: PathBuf, rgba: Option<Vec<u8>>) {
        self.editor.assets.set_global_poster(ctx, file, rgba);
    }

    /// Asks for the voice on a track to be listened to: the stretches its
    /// clips play, in order (at most the first 512 clips).
    fn measure_voice(
        session: &Session,
        mixer: &mut mixer::Mixer,
        track: sg_core::TrackId,
        style: Option<sg_media::voice::VoiceStyle>,
        error: &mut Option<String>,
    ) -> Option<Command> {
        let project = session.project();
        let found = project.sequences.first().and_then(|s| s.track(track))?;
        let parts: Vec<(AssetId, i64, i64)> =
            found.clips.iter().take(512).map(|c| (c.asset, c.src_in, c.len)).collect();
        if parts.is_empty() {
            *error = Some(format!("There is nothing on {} to listen to yet.", found.name));
            return None;
        }
        mixer.listening = Some(track);
        let mut assets: Vec<AssetId> = found.clips.iter().take(512).map(|c| c.asset).collect();
        assets.sort_unstable();
        assets.dedup();
        mixer.listened.insert(track, mixer::Listened { assets, style, set: None, seen: found.clone(), stale: false });
        // (A clip turned up or down, e.g. to full level, reaches the stages so.)
        let gains: Vec<f32> = found
            .clips
            .iter()
            .take(512)
            .map(|c| found.props_of(c.id).map_or(1.0, |props| 10f32.powf(props.gain_db.at(0) / 20.0)))
            .collect();
        // The tracks that play under it (at most four, the lowest ones
        // first as the timeline shows them): their recordings, to tell
        // how they sit against the voice.
        let sequence = project.sequences.first()?;
        let beside = sequence
            .tracks
            .iter()
            .filter(|t| {
                t.kind == sg_core::TrackKind::Audio
                    && t.id != track
                    && !t.mix.voice
                    && !t.mix.mute
                    && !t.clips.is_empty()
            })
            .take(4)
            .map(|t| {
                let clips = || t.clips.iter().filter(|c| c.asset != AssetId(0)).take(64);
                let parts = clips().map(|c| (c.asset, c.src_in, c.len)).collect();
                let gains =
                    clips().map(|c| t.props_of(c.id).map_or(1.0, |p| 10f32.powf(p.gain_db.at(0) / 20.0))).collect();
                (t.id, parts, gains)
            })
            .filter(|(_, parts, _): &(_, Vec<(AssetId, i64, i64)>, Vec<f32>)| !parts.is_empty())
            .collect();
        Some(Command::MeasureVoice { root: session.root.clone(), track, style, parts, gains, beside })
    }

    /// A strip that was set up by listening follows its track: when other
    /// recordings are on it than the ones it was set up from, it listens
    /// again by itself, as long as nobody has changed its settings by hand
    /// since (then "Listen again" is offered on the strip instead).
    /// Looks only at the tracks that were listened to, and at their clips
    /// only when the track has changed.
    fn relisten(&mut self, commands: &mut Vec<Command>) {
        let Some(session) = &self.session else { return };
        let mixer = &mut self.editor.mixer;
        if mixer.listening.is_some() || self.editor.recording_sound || mixer.listened.is_empty() {
            return;
        }
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return };
        mixer.listened.retain(|id, _| sequence.track(*id).is_some());
        let mut again = None;
        for (id, heard) in mixer.listened.iter_mut() {
            let Some(track) = sequence.track(*id).filter(|t| !Arc::ptr_eq(t, &heard.seen)) else { continue };
            heard.seen = track.clone();
            let mut assets: Vec<AssetId> = track.clips.iter().take(512).map(|c| c.asset).collect();
            assets.sort_unstable();
            assets.dedup();
            // (Cut, moved or trimmed, the same recordings are the same voice.)
            if assets == heard.assets || assets.is_empty() {
                heard.stale = false;
            } else if heard.set == Some(track.mix) {
                again = Some((*id, heard.style));
                break;
            } else {
                heard.stale = true;
            }
        }
        if let Some((track, style)) = again {
            commands.extend(Shell::measure_voice(session, mixer, track, style, &mut self.error));
        }
    }

    /// How loud the recordings on the tracks beside a voice are (decibels,
    /// before their faders): told just before [`Shell::voice_measured`],
    /// which says how they sit against the voice.
    pub fn heard_beside(&mut self, levels: Vec<(sg_core::TrackId, f32)>) {
        self.beside_levels = levels;
    }

    /// The voice on a track was listened to: its strip is set for the
    /// style asked for, or the one that fits. One undo step.
    pub fn voice_measured(
        &mut self,
        root: &std::path::Path,
        track: sg_core::TrackId,
        style: Option<sg_media::voice::VoiceStyle>,
        result: Result<sg_media::voice::VoiceStats, String>,
        fitted: Option<sg_media::voice::Suggestion>,
    ) {
        self.editor.mixer.listening = None;
        let beside = std::mem::take(&mut self.beside_levels);
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        let stats = match result {
            Ok(stats) => stats,
            Err(e) => {
                self.error = Some(format!("Could not listen to the track: {e}"));
                return;
            }
        };
        let project = session.project().clone();
        let Some((seq, found)) = project.sequences.first().and_then(|s| Some((s.id, s.track(track)?))) else { return };
        let style = style.unwrap_or_else(|| sg_media::voice::VoiceStyle::guess(&stats));
        // The settings checked on the sound itself, if the listening
        // gave them; what the measurements alone propose otherwise.
        let set = fitted.filter(|set| set.style == style).unwrap_or_else(|| sg_media::voice::suggest(&stats, style));
        let mix = mixer::with_voice(found.mix, &set);
        if let Some(heard) = self.editor.mixer.listened.get_mut(&track) {
            heard.set = Some(mix);
        }
        let mut ops = vec![Op::SetTrackMix { seq, track, mix }];
        // The beat makes room: every other sounding track that is not a
        // voice itself and does not duck yet goes down a little under it.
        let mut ducking = 0;
        for other in project.sequences.first().into_iter().flat_map(|s| s.tracks.iter()) {
            let beat = other.kind == sg_core::TrackKind::Audio && other.id != track && !other.mix.voice;
            if beat && set.duck_db > 0.0 && other.mix.duck_db == 0.0 && !other.clips.is_empty() {
                let mix = sg_core::TrackSettings { duck_db: set.duck_db, ..other.mix };
                ops.push(Op::SetTrackMix { seq, track: other.id, mix });
                ducking += 1;
            }
        }
        match session.apply(Op::Batch(ops), None) {
            Ok(()) => {
                let room = match ducking {
                    0 => String::new(),
                    n => format!(" {n} other track(s) now duck {:.0} dB under it.", set.duck_db),
                };
                let pitch = stats.pitch_hz.map_or(String::new(), |hz| format!(", voice around {hz:.0} Hz"));
                let noise = match stats.noise_db {
                    Some(noise) => format!(", noise {:.0} dB under it", stats.loudness_db - noise),
                    None => String::new(),
                };
                // What no setting repairs is said, so that the next take is better.
                let mut advice = String::new();
                if stats.hum_db.is_some_and(|hum| stats.loudness_db - hum < 30.0) {
                    advice += " Much of the noise is mains hum from the input: the low cut takes it out.";
                }
                if stats.noise_db.is_some_and(|noise| stats.loudness_db - noise < 20.0) {
                    advice +=
                        " The voice is close to the noise: record nearer to the microphone, or with a better input.";
                }
                if stats.peak_db > -0.3 {
                    advice += " The take clips: turn the input down for the next one.";
                }
                // How the loudest track beside it sits against the voice as it
                // is set now: a beat is usually 3 to 6 dB under a rap voice.
                // (Said, not done: the faders of the other tracks are his.)
                let (_, voice_db) = style.aims();
                let voice_db = voice_db + (mix.gain_db - set.gain_db);
                let loudest = beside
                    .iter()
                    .filter_map(|(id, raw)| {
                        let other = project.sequences.first()?.track(*id)?;
                        // (While the voice sounds it is turned down by what it ducks:
                        // what it was set to, or what this set-up has just given it.)
                        let ducked = if other.mix.duck_db > 0.0 { other.mix.duck_db } else { set.duck_db };
                        Some((other.name.clone(), raw + other.mix.gain_db - ducked, *raw - ducked))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((name, now, raw)) = loudest {
                    let under = voice_db - now;
                    if !(1.0..=9.0).contains(&under) {
                        let fader = (voice_db - 4.0 - raw).clamp(-60.0, 6.0);
                        let how =
                            if under > 0.0 { format!("{under:.0} dB under") } else { format!("{:.0} dB over", -under) };
                        advice += &format!(
                            " {name} now plays {how} the voice: its fader at about {fader:.0} dB puts it 4 dB under."
                        );
                    }
                }
                session.notice = Some(format!(
                    "{} set up as {}: heard {:.0} s at {:.0} dB{pitch}{noise}.{room}{advice} Undo takes it back.",
                    found.name,
                    style.name().to_lowercase(),
                    stats.seconds,
                    stats.loudness_db,
                ));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// News from the plugin jobs: a line of a running step, what was found, the end of a step.
    pub fn plugins_update(&mut self, update: PluginUpdate) {
        if let PluginUpdate::Found { plugins, .. } = &update {
            // The mixer offers them on its strips.
            let listed = plugins.iter().map(|p| (p.name.clone(), p.path.display().to_string())).collect();
            self.editor.mixer.available = Some(listed);
        }
        self.plugins.update(update);
    }

    /// Whether the mark of copied clips could be put on the clipboard.
    pub fn clips_offered(&mut self, offered: bool) {
        self.clips_offered = offered;
    }

    /// Imports what was dropped, pasted or picked into the open project.
    pub fn incoming(&mut self, incoming: Incoming) -> Vec<Command> {
        self.editor.assets.drag_hover = false;
        // The clipboard still holds the mark of clips copied here: paste those.
        if matches!(&incoming, Incoming::Text(text) if text.trim() == CLIPS_ON_CLIPBOARD) {
            let mut commands = Vec::new();
            self.timeline_action(Action::PasteClips, &mut commands);
            return commands;
        }
        let Some(session) = &self.session else {
            self.error = Some("Open a project first, then drop or paste media into it.".to_owned());
            return Vec::new();
        };
        let sources = match incoming {
            Incoming::Paths(paths) => paths.into_iter().map(ImportSource::Path).collect(),
            Incoming::Image { data, mime } => vec![ImportSource::Bytes { data, mime }],
            Incoming::Sound(link) => vec![ImportSource::UrlSound(link.trim().to_owned())],
            Incoming::Text(text) if sg_project::fetch::looks_like_url(&text) => {
                vec![ImportSource::Url(text.trim().to_owned())]
            }
            Incoming::Text(_) => {
                self.error = Some("The clipboard holds text that is not a link.".to_owned());
                Vec::new()
            }
        };
        let (root, known) = (session.root.clone(), session.project().assets.clone());
        // Look-up tables are not media: they go to their own folder.
        let is_table = |source: &ImportSource| matches!(source, ImportSource::Path(p) if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("cube")));
        sources
            .into_iter()
            .map(|source| match source {
                ImportSource::Path(path) if is_table(&ImportSource::Path(path.clone())) => {
                    Command::ImportLut { root: root.clone(), path }
                }
                // Nor are notes: a MIDI file becomes note clips.
                ImportSource::Path(path)
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi")) =>
                {
                    Command::ReadMidi { root: root.clone(), path }
                }
                // Nor are fonts.
                ImportSource::Path(path)
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf")) =>
                {
                    Command::ImportFont { root: root.clone(), path }
                }
                source => Command::Import { root: root.clone(), known: known.clone(), source },
            })
            .collect()
    }

    /// A drag from another application entered or left the window.
    pub fn set_drag_hover(&mut self, hover: bool) {
        self.editor.assets.drag_hover = hover && self.session.is_some();
    }

    pub fn import_event(&mut self, event: ImportEvent) {
        let assets = &mut self.editor.assets;
        // Events of a project that was closed in the meantime are dropped.
        let current = |root: &PathBuf| self.session.as_ref().is_some_and(|s| s.root == *root);
        // An asset that arrived and may belong to a drop on the timeline.
        let mut placed = None;
        match event {
            ImportEvent::Progress { job, label, stage, fraction } => {
                assets.jobs.insert(job, JobView { label, stage, fraction });
            }
            ImportEvent::Finished { job } => {
                assets.jobs.remove(&job);
            }
            ImportEvent::Failed { label, error, .. } => {
                self.error = Some(format!("{label}: {error}"));
                // One file fewer to wait for at the place of a drop.
                self.dropped_at =
                    self.dropped_at.and_then(|(frame, row, left)| (left > 1).then_some((frame, row, left - 1)));
            }
            ImportEvent::Duplicate { label, id, .. } => {
                assets.selected = Some(id);
                placed = Some(id);
                if let Some(session) = &mut self.session {
                    session.notice = Some(format!("{label} is already in this project."));
                }
            }
            ImportEvent::Usable { root, asset, .. } if current(&root) => {
                let id = asset.id;
                if let Some(session) = &mut self.session {
                    match session.apply(Op::AddAsset { asset }, None) {
                        Ok(()) => {
                            assets.selected = Some(id);
                            placed = Some(id);
                        }
                        // Two imports of the same content raced: the first one won.
                        Err(sg_core::OpError::DuplicateId(_)) => assets.selected = Some(id),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
            }
            ImportEvent::Derived { root, id, what: sg_project::Derived::Peaks } if current(&root) => {
                self.editor.timeline.peaks_changed(id);
            }
            ImportEvent::Derived { root, id, what: sg_project::Derived::Poster } if current(&root) => {
                assets.poster_changed(id)
            }
            ImportEvent::Missing { root, id } if current(&root) => {
                assets.missing.insert(id);
            }
            ImportEvent::Usable { .. } | ImportEvent::Derived { .. } | ImportEvent::Missing { .. } => {}
        }
        // A recorded take goes where it was played; anything else where it was dropped.
        if let Some(id) = placed
            && !self.place_part(id)
            && !self.place_take(id)
        {
            self.place_dropped(id);
        }
    }

    pub fn poster_loaded(&mut self, ctx: &egui::Context, root: &std::path::Path, id: AssetId, rgba: Option<Vec<u8>>) {
        if self.session.as_ref().is_some_and(|s| s.root == root) {
            self.editor.assets.set_poster(ctx, id, rgba);
        }
    }
}

/// How the part of a video that a clip shows is made anew.
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    /// Its sky made transparent, for one's own on the track below.
    NoSky,
    /// Changed by a sentence, by a model at a service or a program.
    Sentence { service: Box<sg_project::ai::Service>, sentence: String },
}

impl Part {
    /// The name the part has as an asset: how it is known again.
    pub fn name(&self, asset: &Asset, src_in: i64, span: i64) -> String {
        match self {
            Part::NoSky => sg_project::analysis::sky_name(asset, src_in, span),
            Part::Sentence { service, sentence } => {
                sg_project::ai::change_name(asset, &service.name, sentence, src_in, span)
            }
        }
    }
}

/// A clip that waits for its part made anew: the name the part will have
/// as an asset, the clip, and where in the video the part begins.
struct PendingPart {
    name: String,
    seq: sg_core::SequenceId,
    track: sg_core::TrackId,
    clip: sg_core::ClipId,
    src_in: i64,
}

/// The asset that stands for a project in the library: what its topmost
/// shown video track begins with, or else any asset with a picture. Looks
/// at one clip per track, whatever the size of the project.
fn poster_asset(project: &sg_core::Project) -> Option<AssetId> {
    let pictured = |id: &AssetId| project.assets.get(id).is_some_and(|a| a.video.is_some());
    project
        .sequences
        .first()
        .into_iter()
        .flat_map(|s| s.tracks.iter())
        .filter(|t| t.kind == sg_core::TrackKind::Video && !t.mix.hidden)
        .filter_map(|t| t.clips.iter().next().map(|c| c.asset))
        .find(pictured)
        .or_else(|| project.assets.keys().find(|id| pictured(id)).copied())
}

impl Shell {
    /// The picture of a project in the library was read (or it has none).
    pub fn project_poster_loaded(&mut self, ctx: &egui::Context, path: PathBuf, rgba: Option<Vec<u8>>) {
        self.library.set_poster(ctx, path, rgba);
    }

    /// Appends the selected asset to the timeline and moves the playhead to it.
    fn insert_selected(&mut self, only: Option<sg_core::TrackKind>, commands: &mut Vec<Command>) {
        let Some(session) = &mut self.session else { return };
        let Some(asset) = self.editor.assets.selected.and_then(|id| session.project().assets.get(&id).cloned()) else {
            return;
        };
        match session.place_asset(&asset, self.playhead, only) {
            Ok(Some(start)) => commands.push(Command::Transport(Transport::Seek(start))),
            Ok(None) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

impl Shell {
    fn timeline_action(&mut self, action: Action, commands: &mut Vec<Command>) {
        let Some(session) = &mut self.session else { return };
        if let Some(frame) = self.editor.timeline.act(action, session, self.playhead, &mut self.error) {
            commands.push(Command::Transport(Transport::Seek(frame)));
        }
    }
}

impl Shell {
    /// Quads the timeline drew last frame.
    pub fn timeline_quads(&self) -> usize {
        self.editor.timeline.quads
    }
}

impl Shell {
    /// Tells the shell where the playhead is before a scripted step, which
    /// (unlike a key press) does not come through a frame.
    pub fn set_playhead(&mut self, frame: i64) {
        self.playhead = frame;
    }
}

impl Shell {
    pub fn set_history(&mut self, result: Result<Vec<sg_project::HistoryEntry>, String>) {
        match result {
            Ok(entries) => self.history.set_entries(entries),
            Err(e) => {
                self.history.cancel();
                self.error = Some(format!("Cannot read the history: {e}"));
            }
        }
    }
}

impl Shell {
    /// What "Loop" plays over and over, in timeline samples: the note clip
    /// whose piano roll is open, else the selected clips, else the stretch
    /// between the markers around the playhead.
    fn loop_range(&self) -> Option<(i64, i64)> {
        let session = self.session.as_ref()?;
        let project = session.project();
        let sequence = project.sequences.first()?;
        if let Some(span) = self.editor.piano.span(session) {
            return Some(span);
        }
        let (from, to) = self.gif_range()?;
        let samples = |frame: i64| sequence.rate.frames_to_samples(frame, 48_000);
        Some((samples(from), samples(to)))
    }

    /// What a GIF export covers: the selected clips, else the stretch
    /// between the markers around the playhead, else the whole sequence.
    fn gif_range(&self) -> Option<(i64, i64)> {
        let session = self.session.as_ref()?;
        let project = session.project();
        let sequence = project.sequences.first()?;
        if let Some(span) = self.editor.timeline.selection_span(sequence) {
            return Some(span);
        }
        let before = sequence.markers.iter().rev().find(|m| m.frame <= self.playhead).map(|m| m.frame);
        let after = sequence.markers.iter().find(|m| m.frame > self.playhead).map(|m| m.frame);
        match (before, after) {
            (Some(a), Some(b)) => Some((a, b)),
            _ => None,
        }
    }

    fn start_export(&mut self, request: ExportRequest, commands: &mut Vec<Command>) {
        let Some(session) = &self.session else { return };
        if self.export.is_running() {
            self.error = Some("An export is already running.".to_owned());
            return;
        }
        // The state that is exported is also a commit (03-project-bundle.md).
        session.commit();
        self.export.started(request.clone());
        commands.push(Command::Export { root: session.root.clone(), project: session.project().clone(), request });
    }

    pub fn export_update(&mut self, update: ExportUpdate) {
        if let ExportUpdate::Failed(error) = &update {
            self.error = Some(format!("Export failed: {error}"));
        }
        if let (ExportUpdate::Done { path, .. }, Some(session)) = (&update, &mut self.session) {
            let notice = format!("Exported {path}");
            self.exported = Some((path.clone(), notice.clone()));
            session.notice = Some(notice);
        }
        self.export.update(update);
    }

    /// Progress of a running export, for display outside the popover.
    pub fn export_status(&self) -> Option<String> {
        self.export.status()
    }
}

impl Shell {
    /// Imports what was dropped at a point of the window (in points). Over
    /// the timeline the imported media is also placed where it was dropped,
    /// one after the other if there are several.
    pub fn incoming_at(&mut self, incoming: Incoming, x: f32, y: f32) -> Vec<Command> {
        let target = self.session.as_ref().and_then(|session| {
            let project = session.project();
            let sequence = project.sequences.first()?;
            self.editor.timeline.drop_target(sequence, self.playhead, egui::pos2(x, y), true)
        });
        let commands = self.incoming(incoming);
        // Imports that were already running may finish first and take the
        // place: rare, and undo puts it right.
        // (Only media arrives as an asset to place: tables and fonts go to their folders.)
        let media = commands.iter().filter(|c| matches!(c, Command::Import { .. })).count();
        self.dropped_at = target.filter(|_| media > 0).map(|(frame, row)| (frame, row, media));
        commands
    }

    /// The track a recording started now goes onto, and the timeline sample
    /// it starts at: the one marked "R"; unless only that one is asked
    /// for, else the lowest audio track that sounds and has nothing from
    /// the playhead on.
    fn record_target(&self, only_armed: bool) -> Option<(sg_core::TrackId, i64)> {
        let project = self.session.as_ref()?.project().clone();
        let sequence = project.sequences.first()?;
        let from = sequence.rate.frames_to_samples(self.playhead.max(0), 48_000);
        let audio = |t: &&Arc<sg_core::Track>| t.kind == sg_core::TrackKind::Audio;
        let armed =
            self.editor.timeline.record_track.and_then(|id| sequence.tracks.iter().filter(audio).find(|t| t.id == id));
        if only_armed || armed.is_some() {
            return armed.map(|t| (t.id, from));
        }
        let free = |t: &&Arc<sg_core::Track>| {
            !t.mix.mute && t.clips.at(from).is_none() && t.clips.first_at_or_after(from).is_none()
        };
        sequence.tracks.iter().rev().filter(audio).find(free).map(|t| (t.id, from))
    }

    /// A recording has ended: its file is taken into the project, and laid
    /// on the timeline where it was played (`start`, in timeline samples)
    /// once it is there.
    pub fn take_recorded(&mut self, path: PathBuf, start: i64, frames: u64, lost: u64) -> Vec<Command> {
        self.editor.recording_sound = false;
        if frames == 0 {
            self.editor.timeline.live_take = None;
            self.error = Some("Nothing was recorded: no sound arrived from the input.".to_owned());
            return Vec::new();
        }
        if lost > 0 {
            self.error = Some(format!("{lost} samples of the take were lost: the disk could not keep up."));
        }
        self.takes.push((path.clone(), start));
        self.incoming(Incoming::Paths(vec![path]))
    }

    /// The monitored input follows the buttons, the chosen input and the open project.
    fn monitor_commands(&mut self, commands: &mut Vec<Command>) {
        let monitor = (self.editor.monitor && self.session.is_some())
            .then(|| (self.editor.input.clone(), self.editor.hear_input));
        if monitor != self.monitor {
            self.monitor = monitor.clone();
            commands.push(Command::Monitor(monitor));
        }
        // Heard through the stages of the track marked "R" (feedback N2.1,
        // M13's "the input through the track's own chain"): what is in the
        // ear while recording is then close to what the take will be.
        let chain = (self.editor.monitor && self.editor.hear_input)
            .then(|| {
                let sequence = self.session.as_ref()?.project().sequences.first()?.clone();
                let armed = self.editor.timeline.record_track?;
                Some(sequence.track(armed)?.mix)
            })
            .flatten();
        if chain != self.monitor_chain {
            self.monitor_chain = chain;
            commands.push(Command::MonitorChain(chain.map(Box::new)));
        }
    }

    /// "Play it" in the plugins panel: a new audio track with the plugin
    /// on it, a note clip at the playhead with its piano roll open and the
    /// computer keyboard as a piano, and the plugin's own window asked
    /// for. Everything a first note needs, in one step.
    ///
    /// Without a plugin ("Instrument track" under "+ Track"): the same
    /// with the built-in instrument, and no window to ask for.
    fn instrument_track(&mut self, plugin: Option<(String, String)>, commands: &mut Vec<Command>) {
        let Some(session) = &mut self.session else {
            self.error = Some("Open a project first: an instrument needs a track to play on.".to_owned());
            return;
        };
        if let Err(e) = session.add_track(sg_core::TrackKind::Audio) {
            self.error = Some(e.to_string());
            return;
        }
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return };
        let Some(track) = sequence.tracks.iter().rev().find(|t| t.kind == sg_core::TrackKind::Audio) else { return };
        let mut window = None;
        if let Some((name, path)) = &plugin {
            let plugin = sg_core::PluginRef { name: name.clone(), path: path.clone(), id: session.new_plugin_id() };
            window = Some(plugin.key(track.id));
            let op = Op::SetTrackPlugins { seq: sequence.id, track: track.id, plugins: vec![plugin] };
            if let Err(e) = session.apply(op, None) {
                self.error = Some(e.to_string());
                return;
            }
        }
        // (The note clip opens its piano roll with the next frame.)
        self.editor.timeline.add_note_clip(session, sequence, track, self.playhead.max(0) as f64, &mut self.error);
        self.editor.piano.set_keyboard(true);
        commands.extend(window.map(|key| Command::PluginEditor { track: track.id, key }));
        self.plugins.cancel();
        let keys = "play with the keys A S D F G H J (W E T Z U are the black ones), or draw notes";
        session.notice = Some(match &plugin {
            Some((name, _)) => format!(
                "{name} is on a new track ({}). Its window opens when it has started: load a sound there, then {keys}.",
                track.name
            ),
            None => format!(
                "{} is a new track for notes with the built-in instrument: {keys}. Another instrument: Mixer, on its strip.",
                track.name
            ),
        });
    }

    /// Tells the application which track's instrument is played by hand
    /// (that of the open note clip) and passes on the keys played since
    /// the last call.
    fn hand_commands(&mut self, commands: &mut Vec<Command>) {
        // The loop belongs to the open project: it ends with it.
        if self.session.is_none() {
            self.editor.timeline.loop_range = None;
        }
        if self.editor.timeline.loop_range != self.looping {
            self.looping = self.editor.timeline.loop_range;
            commands.push(Command::Loop(self.looping));
        }
        let hand = self.editor.piano.hand().filter(|_| self.session.is_some());
        if hand != self.hand {
            self.hand = hand;
            commands.push(Command::Hand(hand));
        }
        for played in std::mem::take(&mut self.editor.piano.played) {
            if self.hand.is_some() {
                commands.push(Command::Note { on: played.on, pitch: played.pitch, velocity: played.velocity });
            }
        }
    }

    /// A key played by hand was heard at this timeline sample (the
    /// timeline was playing): the piano roll writes it down if it records.
    pub fn note_heard(&mut self, on: bool, pitch: u8, velocity: u8, at: i64) {
        if let Some(session) = &mut self.session {
            self.editor.piano.heard(session, on, pitch, velocity, at, &mut self.error);
        }
    }

    /// The inputs sound can be recorded from, (PipeWire name, label).
    pub fn inputs_listed(&mut self, inputs: Vec<(String, String)>) {
        self.editor.inputs = Some(inputs);
    }

    /// The input remembered from the last run.
    pub fn set_input(&mut self, input: Option<String>) {
        self.editor.input = input;
    }

    /// The audio track (counted among the audio tracks) a stage of whose
    /// chain has its picture open, for the engine to report the level
    /// along it, and the point of the chain the spectrum is wanted at (and
    /// after) if the picture has one. `None` while no picture is open.
    pub fn watched(&self) -> Option<(usize, Option<usize>)> {
        let mixer = &self.editor.mixer;
        let open = self.session.is_some() && mixer.open && mixer.chain && mixer.stage.is_some();
        open.then_some(mixer.watching).flatten().map(|track| (track, mixer.spectrum_at))
    }

    /// The take being drawn while it is recorded: its track's name and the
    /// timeline samples it runs from and to so far.
    pub fn live_take(&self) -> Option<(String, i64, i64)> {
        let live = self.editor.timeline.live_take.as_ref()?;
        let project = self.session.as_ref()?.project().clone();
        let name = project.sequences.first()?.track(live.track)?.name.clone();
        Some((name, live.from, live.to))
    }

    /// A recording could not be started or was lost.
    pub fn recording_failed(&mut self, why: String) {
        self.editor.recording_sound = false;
        self.editor.timeline.live_take = None;
        self.error = Some(why);
    }

    /// If this asset is a take that was just recorded: puts it where it
    /// was played and says so.
    fn place_take(&mut self, id: AssetId) -> bool {
        let Some(session) = &mut self.session else { return false };
        let Some(asset) = session.project().assets.get(&id).cloned() else { return false };
        // The import names an asset after its file: that is how the take is known again.
        let Some(index) = self
            .takes
            .iter()
            .position(|(path, _)| path.file_stem().is_some_and(|stem| stem.to_string_lossy() == asset.label))
        else {
            return false;
        };
        let (path, start) = self.takes.remove(index);
        // (The file in the project is a copy; the one it was recorded into has done its work.)
        self.spent_takes.push(path);
        // (What was drawn while it was made gives way to the clip. How loud
        // it got is kept: a take that reached full scale is said to have.)
        let live = self.editor.timeline.live_take.take();
        match session.place_take(&asset, start, self.record_onto.take(), self.record_until.take()) {
            Ok(Some((track, clip))) => {
                self.editor.timeline.select_only(track, &clip);
                // (Of the stretch that became the clip: what was played before and after a punch does not count.)
                let loudest = live.map_or(0.0, |live| {
                    let bucket = |at: i64| ((at - live.from) / crate::timeline::LiveTake::BUCKET).max(0) as usize;
                    let (first, last) = (bucket(clip.start), bucket(clip.end()).min(live.peaks.len()));
                    live.peaks.get(first..last.max(first)).map_or(0.0, |part| part.iter().copied().fold(0.0, f32::max))
                });
                let clipped = if loudest >= 0.985 {
                    " It reached full scale and is cut off there: turn the input down and record it again."
                } else {
                    ""
                };
                session.notice = Some(format!("Recorded {:.1} s.{clipped}", clip.len as f64 / 48_000.0));
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
        true
    }

    /// Recorded files that are in the project now and can be deleted; taken by the application.
    pub fn take_spent_takes(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.spent_takes)
    }

    /// Places an asset that came from a drop on the timeline.
    fn place_dropped(&mut self, id: AssetId) {
        let (Some((frame, row, left)), Some(session)) = (self.dropped_at, &mut self.session) else { return };
        let Some(asset) = session.project().assets.get(&id).cloned() else { return };
        self.dropped_at = (left > 1).then_some((frame, row, left - 1));
        match session.drop_asset(&asset, frame, row, None) {
            Ok(Some(start)) => {
                self.editor.timeline.select_dropped(session, id, start);
                // The next file of the same drop follows this one.
                let length = sg_core::Asset::frames_at(&asset, session.project().sequences[0].rate, 1).max(1);
                if let Some(next) = &mut self.dropped_at {
                    next.0 = start + length;
                }
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

impl Shell {
    pub fn peaks_loaded(&mut self, root: &std::path::Path, id: AssetId, peaks: Option<sg_media::peaks::Peaks>) {
        if self.session.as_ref().is_some_and(|s| s.root == root) {
            self.editor.timeline.set_peaks(id, peaks);
        }
    }
}

impl Shell {
    /// Makes the selected asset the song of the sequence.
    fn angle_by_hand(&mut self) {
        let Some(session) = &mut self.session else { return };
        let Some(asset) = self.editor.assets.selected.and_then(|id| session.project().assets.get(&id).cloned()) else {
            return;
        };
        match session.place_angle_by_hand(&asset, self.playhead) {
            Ok(()) => {
                self.editor.timeline.select_dropped(session, asset.id, self.playhead);
                session.notice = Some(format!(
                    "{} is a camera angle. Drag it or use the arrow keys until its waveform lines up with the song's; then cut to it with its number.",
                    asset.label
                ));
            }
            Err(e) => self.error = Some(format!("{} was not placed: {e}.", asset.label)),
        }
    }

    fn use_as_song(&mut self) {
        let Some(session) = &mut self.session else { return };
        let Some(asset) = self.editor.assets.selected.and_then(|id| session.project().assets.get(&id).cloned()) else {
            return;
        };
        if asset.audio.is_none() {
            self.error = Some(format!("{} has no sound.", asset.label));
            return;
        }
        match session.set_song(&asset) {
            Ok(()) => session.notice = Some(format!("{} is the song. Finding its beats...", asset.label)),
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn set_tempo(&mut self, tempo: i8) {
        let Some(session) = &mut self.session else { return };
        let project = session.project().clone();
        let Some((sequence, song)) = project.sequences.first().and_then(|s| Some((s, s.song?))) else { return };
        let op = Op::SetSong { id: sequence.id, song: Some(sg_core::Song { tempo, ..song }) };
        if let Err(e) = session.apply(op, None) {
            self.error = Some(e.to_string());
        }
    }

    /// Starts the search for the selected take (or every video asset that
    /// is not on a camera track yet) in the song.
    fn sync_takes(&mut self, all: bool, commands: &mut Vec<Command>) {
        let Some(session) = &mut self.session else { return };
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return };
        let Some(song) = sequence.song else {
            self.error = Some("Choose the song first: right click on it among the assets.".to_owned());
            return;
        };
        if session::song_clip(sequence).is_none() {
            self.error = Some("The song is not on the timeline: put it on an audio track first.".to_owned());
            return;
        }
        let usable = |asset: &sg_core::Asset| asset.id != song.asset && asset.video.is_some() && asset.audio.is_some();
        let takes: Vec<AssetId> = if all {
            // Everything with picture and sound that is not an angle already.
            let placed: Vec<AssetId> = session::angle_rows(sequence)
                .iter()
                .flat_map(|row| sequence.tracks[*row].clips.iter().map(|c| c.asset))
                .collect();
            project.assets.values().filter(|a| usable(a) && !placed.contains(&a.id)).map(|a| a.id).collect()
        } else {
            self.editor
                .assets
                .selected
                .and_then(|id| project.assets.get(&id))
                .filter(|a| usable(a))
                .map(|a| a.id)
                .into_iter()
                .collect()
        };
        if takes.is_empty() {
            self.error = Some("Nothing to synchronise: a take needs picture and sound.".to_owned());
            return;
        }
        session.notice = Some(format!("Looking for {} take(s) in the song...", takes.len()));
        let root = session.root.clone();
        commands.extend(takes.into_iter().map(|take| Command::SyncTake { root: root.clone(), song: song.asset, take }));
    }

    pub fn beats_loaded(
        &mut self,
        root: &std::path::Path,
        id: AssetId,
        beats: Result<sg_media::analysis::Beats, String>,
    ) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        match beats {
            Ok(beats) => {
                let steady = if beats.steady { "steady" } else { "changing" };
                session.notice = Some(format!(
                    "Song: {:.1} beats per minute, {steady} tempo, {} beats.",
                    beats.bpm,
                    beats.beats.len()
                ));
                self.editor.timeline.set_beats(id, Some(beats));
            }
            Err(e) => {
                self.editor.timeline.set_beats(id, None);
                self.error = Some(format!("No beat grid: {e}."));
            }
        }
    }

    /// A take was looked up in the song: lay it on the timeline if the
    /// match can be trusted, say why not otherwise.
    pub fn synced(&mut self, root: &std::path::Path, take: AssetId, found: Result<sg_media::analysis::Offset, String>) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        let Some(asset) = session.project().assets.get(&take).cloned() else { return };
        let offset = match found {
            Ok(offset) => offset,
            Err(e) => {
                self.error = Some(format!("{}: {e}.", asset.label));
                return;
            }
        };
        if !offset.is_confident() {
            self.error = Some(format!(
                "{} was not placed: its sound does not clearly match one place in the song (likeness {:.2}, next best {:.2}). \
                 It may be too short, too noisy, or from another song. To place it yourself: right click it, \
                 \"Camera angle at the playhead, synced by hand\".",
                asset.label, offset.likeness, offset.runner_up
            ));
            return;
        }
        match session.place_synced(&asset, offset.lag) {
            Ok(start) => {
                let rate = session.project().sequences[0].rate;
                let seconds = start as f64 * rate.den() as f64 / rate.num() as f64;
                // Only worth a word when it adds up to a frame over a minute.
                let clock = if offset.drift_ppm.abs() >= 150.0 {
                    format!(
                        " Its clock differs by {:+.0} ppm: sync is exact in the middle and drifts towards the ends.",
                        offset.drift_ppm
                    )
                } else {
                    String::new()
                };
                session.notice =
                    Some(format!("{} placed at {seconds:.2} s (likeness {:.2}).{clock}", asset.label, offset.likeness));
            }
            Err(e) => self.error = Some(format!("{}: {e}.", asset.label)),
        }
    }

    /// Cuts the programme to camera angle `number` (1 is the first) at the
    /// playhead. While playing, all cuts of the pass undo together.
    fn switch_angle(&mut self, number: usize) {
        let Some(session) = &mut self.session else { return };
        if !self.playing {
            self.angle_group = None;
        }
        let group = self.playing.then(|| *self.angle_group.get_or_insert_with(|| session.new_group()));
        match session.switch_angle(number.saturating_sub(1), self.playhead, group) {
            Ok(true) => {}
            Ok(false) => session.notice = Some(format!("There is no camera angle {number} at this point.")),
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

impl Shell {
    /// Puts an asset into a scenario and has its picture measured.
    fn assign_scenario(&mut self, asset: AssetId, choice: session::ScenarioChoice, commands: &mut Vec<Command>) {
        let Some(session) = &mut self.session else { return };
        match session.assign_scenario(asset, choice) {
            Ok(Some((scenario, group))) => {
                self.editor.colour.current = Some(scenario);
                self.editor.colour.open = true;
                self.editor.mixer.open = false;
                if let Some(asset) = session.project().assets.get(&asset).cloned() {
                    commands.push(Command::AnalyseColour {
                        root: session.root.clone(),
                        scenario,
                        assets: vec![asset],
                        group: Some(group),
                    });
                }
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn colour_requests(&mut self, requests: Vec<colour_panel::ColourRequest>, commands: &mut Vec<Command>) {
        use colour_panel::ColourRequest;
        let Some(session) = &self.session else { return };
        let root = session.root.clone();
        for request in requests {
            commands.push(match request {
                ColourRequest::Analyse(scenario, ids) => {
                    let assets = ids.iter().filter_map(|id| session.project().assets.get(id).cloned()).collect();
                    Command::AnalyseColour { root: root.clone(), scenario, assets, group: None }
                }
                ColourRequest::SaveLook(name, modules) => Command::SaveLook { root: root.clone(), name, modules },
                ColourRequest::LoadLooks => Command::LoadLooks { root: root.clone() },
                ColourRequest::AddBuiltinLooks => Command::AddBuiltinLooks { root: root.clone() },
            });
        }
    }

    /// The picture of an asset was measured: note it in its scenario.
    pub fn colour_measured(
        &mut self,
        root: &std::path::Path,
        scenario: sg_core::ScenarioId,
        asset: AssetId,
        stats: Result<sg_core::ColourStats, String>,
        group: Option<u64>,
    ) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        let label = session.project().assets.get(&asset).map_or(String::new(), |a| a.label.clone());
        let stats = match stats {
            Ok(stats) => stats,
            Err(e) => {
                self.error = Some(format!("{label} could not be measured: {e}."));
                return;
            }
        };
        // The scenario as it is now; the asset may have left it meanwhile.
        let Some(current) = session.project().scenarios.get(&scenario).filter(|s| s.members.contains_key(&asset))
        else {
            return;
        };
        let mut edited = sg_core::Scenario::clone(current);
        edited.members.insert(asset, sg_core::Member { stats: Some(stats) });
        if let Err(e) = session.apply(Op::SetScenario { id: scenario, scenario: Some(Box::new(edited)) }, group) {
            self.error = Some(e.to_string());
        }
    }

    pub fn looks_loaded(
        &mut self,
        root: &std::path::Path,
        looks: Vec<(String, Vec<sg_core::Module>)>,
        luts: Vec<String>,
    ) {
        if self.session.as_ref().is_some_and(|s| s.root == root) {
            self.editor.colour.presets = Some(looks);
            self.editor.colour.luts = luts;
        }
    }

    pub fn look_saved(&mut self, root: &std::path::Path, result: Result<String, String>) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        match result {
            Ok(file) => session.notice = Some(format!("Look saved as {file}.")),
            Err(e) => self.error = Some(format!("The look could not be saved: {e}.")),
        }
    }
}

impl Shell {
    /// Scripted: adds a look module with the given knob values to the
    /// scenario the colour panel shows.
    fn script_look(&mut self, what: &str) {
        let Some(session) = &mut self.session else { return };
        let (kind, values) = what.split_once(':').unwrap_or((what, ""));
        let kind = sg_core::ModuleKind::ALL
            .into_iter()
            .find(|k| k.name().to_lowercase().replace(' ', "") == kind.to_lowercase());
        let project = session.project().clone();
        let scenario = self
            .editor
            .colour
            .current
            .and_then(|id| project.scenarios.get(&id))
            .or_else(|| project.scenarios.values().next());
        let (Some(kind), Some(scenario)) = (kind, scenario) else {
            self.error = Some(format!("look: no such module or no scenario ({what})"));
            return;
        };
        let mut module = sg_core::Module::new(kind);
        for (slot, value) in module.knobs.iter_mut().zip(values.split(',').filter_map(|v| v.trim().parse::<f32>().ok()))
        {
            *slot = value;
        }
        let mut edited = sg_core::Scenario::clone(scenario);
        edited.look.push(module);
        if let Err(e) = session.apply(Op::SetScenario { id: edited.id, scenario: Some(Box::new(edited)) }, None) {
            self.error = Some(e.to_string());
        }
    }
}

/// The key reference (F1): every key and what it does, in columns.
fn help_panel(ctx: &egui::Context) {
    use egui::{Align2, RichText, Stroke, vec2};
    egui::Area::new(egui::Id::unique("help")).fade_in(false).anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0)).order(egui::Order::Foreground).show(ctx, |ui| {
        egui::Frame::new().fill(theme::RAISED).stroke(Stroke::new(1.0, theme::LINE)).corner_radius(theme::RADIUS).inner_margin(16.0).show(ui, |ui| {
            ui.label(RichText::new("KEYS").size(11.0).strong().color(theme::DIM));
            ui.label(RichText::new("As printed on a Swiss German keyboard. Esc or F1 closes this.").color(theme::DIM));
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                // Three columns of two groups each.
                for column in keymap::reference().chunks(2) {
                    ui.vertical(|ui| {
                        for (group, lines) in column {
                            ui.label(RichText::new(*group).strong());
                            egui::Grid::new(*group).num_columns(2).spacing(vec2(14.0, 3.0)).show(ui, |ui| {
                                for (keys, what) in lines {
                                    ui.label(RichText::new(keys).monospace().color(theme::ACCENT));
                                    ui.label(*what);
                                    ui.end_row();
                                }
                            });
                            ui.add_space(10.0);
                        }
                    });
                    ui.add_space(18.0);
                }
            });
            ui.set_max_width(1180.0);
            if let Some((file, problems)) = keymap::source() {
                ui.label(RichText::new(format!("To change keys, edit {file} and start again.")).color(theme::DIM));
                for problem in problems {
                    ui.label(RichText::new(format!("Not understood in that file, {problem}")).color(theme::ACCENT));
                }
            }
            ui.label(RichText::new("Mouse: the wheel moves along the timeline, Ctrl + wheel zooms it; pull the line across a sound clip to change its volume; drag over empty timeline to select with a box; drag clips to move (with Ctrl: over what is in the way), their edges to trim, the mark at a selected clip's top corner to fade; right click for menus; drag assets onto the timeline (Shift: only picture or only sound).").color(theme::DIM));
        });
    });
}

impl Shell {
    /// "Cuts to the beat" (`true`) or "Whip every cut" on the programme track.
    fn programme_action(&mut self, to_beats: bool) {
        let Some(session) = &mut self.session else { return };
        let count = if to_beats {
            self.editor.timeline.cuts_to_beats(session, &mut self.error)
        } else {
            self.editor.timeline.whip_every_cut(session, &mut self.error)
        };
        let what = if to_beats { "cut(s) moved onto a beat" } else { "whip-pan(s) added" };
        session.notice = Some(format!("{count} {what}."));
    }
}

impl Shell {
    /// The fonts in the project's folder were listed.
    pub fn fonts_loaded(
        &mut self,
        root: &std::path::Path,
        fonts: Vec<String>,
        shipped: Vec<sg_project::analysis::ShippedFont>,
    ) {
        if self.session.as_ref().is_some_and(|s| s.root == root) {
            self.editor.clip_panel.fonts = Some(fonts);
            self.editor.clip_panel.shipped = shipped;
        }
    }

    /// A font was copied into the project (or could not be).
    pub fn font_imported(&mut self, root: &std::path::Path, result: Result<String, String>) -> Vec<Command> {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return Vec::new() };
        match result {
            Ok(name) => {
                // (One chosen in the menu is on the title already.)
                if self.editor.clip_panel.chosen_font.take().as_deref() != Some(name.as_str()) {
                    session.notice =
                        Some(format!("{name} is in the project: choose it under Font in a title's settings."));
                }
                vec![Command::LoadFonts { root: root.to_owned() }]
            }
            Err(e) => {
                self.error = Some(format!("The font was not taken: {e}."));
                Vec::new()
            }
        }
    }

    /// A MIDI file was read: each of its tracks that has notes becomes a
    /// new track with a note clip from the bar the playhead is in. A
    /// sequence without a tempo takes the file's.
    pub fn midi_read(&mut self, root: &std::path::Path, name: &str, result: Result<Vec<u8>, String>) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        let file = match result.and_then(|bytes| sg_core::midi::parse(&bytes).map_err(|e| e.to_string())) {
            Ok(file) if !file.tracks.is_empty() => file,
            Ok(_) => {
                self.error = Some(format!("{name} has no notes in it."));
                return;
            }
            Err(e) => {
                self.error = Some(format!("{name} could not be read: {e}."));
                return;
            }
        };
        let Some(seq) = session.project().sequences.first().map(|s| s.id) else { return };
        let had_tempo = session.project().sequences.first().is_some_and(|s| s.tempo.is_some());
        if let (false, Some(milli_bpm)) = (had_tempo, file.milli_bpm) {
            let tempo = sg_core::Tempo { milli_bpm: milli_bpm.clamp(20_000, 400_000), ..sg_core::Tempo::DEFAULT };
            if let Err(e) = session.apply(Op::SetTempo { id: seq, tempo: Some(tempo) }, None) {
                self.error = Some(e.to_string());
                return;
            }
        }
        let (tracks, length) = (file.tracks.len().min(16), file.length());
        let mut notes = 0;
        for part in file.tracks.into_iter().take(16) {
            if let Err(e) = session.add_track(sg_core::TrackKind::Audio) {
                self.error = Some(e.to_string());
                return;
            }
            let project = session.project().clone();
            let Some(sequence) = project.sequences.first() else { return };
            let Some(track) = sequence.tracks.iter().rev().find(|t| t.kind == sg_core::TrackKind::Audio) else {
                return;
            };
            if let Some(name) = part.name.filter(|name| name.chars().count() <= 40) {
                let _ = session.apply(Op::SetTrackName { seq, track: track.id, name }, None);
            }
            notes += part.notes.len();
            let at = self.playhead.max(0) as f64;
            // (All as long as the longest: they belong together.)
            self.editor.timeline.add_notes(session, sequence, track, at, Some(length), part.notes, &mut self.error);
        }
        let tempo = match (had_tempo, file.milli_bpm) {
            (false, Some(bpm)) => {
                format!(" The sequence now has its tempo, {:.0} beats a minute.", bpm as f32 / 1000.0)
            }
            (true, Some(_)) => " They play at the sequence's tempo, not the file's.".to_owned(),
            _ => String::new(),
        };
        session.notice = Some(format!(
            "{name}: {notes} notes on {tracks} new track(s), played by the built-in instrument until a plugin is put on \
             (Mixer, on the strip).{tempo}"
        ));
    }

    /// A file asked for with [`Command::WriteExport`] was written (or not).
    pub fn file_written(&mut self, root: &std::path::Path, result: Result<String, String>) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        match result {
            Ok(path) => {
                let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned());
                let notice = format!("Written: {} in the project's exports folder.", name.unwrap_or_default());
                session.notice = Some(notice.clone());
                self.exported = Some((path, notice));
            }
            Err(e) => self.error = Some(format!("The file could not be written: {e}.")),
        }
    }

    /// The window for changing a clip by a sentence opens, and which
    /// models there are is read again (the file may have been written since).
    fn open_ai(&mut self, track: sg_core::TrackId, clip: sg_core::Clip, commands: &mut Vec<Command>) {
        let Some(session) = &self.session else { return };
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return };
        let props = sequence.track(track).and_then(|track| track.props_of(clip.id));
        let span = props.map_or(clip.len, |props| props.source_offset(clip.len)).max(1);
        let seconds = span as f64 * sequence.rate.den() as f64 / sequence.rate.num() as f64;
        self.ai = Some(ai_panel::AiPanel::new(track, clip, seconds));
        self.ai_services = None;
        commands.push(Command::LoadAiServices);
    }

    /// Which models change a clip by a sentence was read from `file`.
    pub fn ai_services_loaded(&mut self, file: String, services: Result<Vec<sg_project::ai::Service>, String>) {
        self.ai_file = file;
        self.ai_services = Some(services);
    }

    /// The part of its video that a clip shows is to be made anew (a
    /// job); when that arrives the clip shows it.
    fn ask_for_part(&mut self, track: sg_core::TrackId, clip: sg_core::Clip, how: Part) -> Option<Command> {
        let session = self.session.as_mut()?;
        let project = session.project().clone();
        let sequence = project.sequences.first()?;
        let asset = project.assets.get(&clip.asset)?;
        let props = sequence.track(track)?.props_of(clip.id);
        // (As much of the source as the clip plays, at its speed.)
        let span = props.map_or(clip.len, |props| props.source_offset(clip.len)).max(1);
        let name = how.name(asset, clip.src_in, span);
        // Made before (the same part, the same change): it is there already.
        if let Some(made) = project.assets.values().find(|other| other.label == name) {
            let made = made.id;
            self.parts.push(PendingPart { name, seq: sequence.id, track, clip: clip.id, src_in: clip.src_in });
            self.place_part(made);
            return None;
        }
        let waiting = self.parts.iter().any(|pending| pending.name == name);
        self.parts.push(PendingPart { name, seq: sequence.id, track, clip: clip.id, src_in: clip.src_in });
        session.notice = Some(match &how {
            Part::NoSky => "The sky of this clip is being removed: see the jobs above the assets; the clip shows it when \
                            it is done, and what is on the track below then shows where the sky was. About a minute for \
                            four seconds of 1080p at 60 frames a second; the first time, the model (176 MB) is fetched."
                .to_owned(),
            Part::Sentence { service, .. } => format!(
                "This clip is being changed by {}: see the jobs above the assets. The clip shows the result when it is there.",
                service.name
            ),
        });
        (!waiting).then(|| Command::Part {
            root: session.root.clone(),
            asset: Box::new(Asset::clone(asset)),
            how,
            src_in: clip.src_in,
            span,
            rate: sequence.rate,
        })
    }

    /// An asset arrived: if it is a part made anew that clips wait
    /// for, they show it now (each one undo step). True if it was one.
    fn place_part(&mut self, id: AssetId) -> bool {
        let Some(session) = &mut self.session else { return false };
        let Some(label) = session.project().assets.get(&id).map(|asset| asset.label.clone()) else { return false };
        let (waiting, rest): (Vec<PendingPart>, Vec<PendingPart>) =
            std::mem::take(&mut self.parts).into_iter().partition(|pending| pending.name == label);
        self.parts = rest;
        if waiting.is_empty() {
            return false;
        }
        for pending in waiting {
            let project = session.project().clone();
            // The clip as it is now: it may have been moved or trimmed
            // meanwhile, and shows the new sky as far as the part reaches.
            let found = project.sequence(pending.seq).and_then(|sequence| {
                let track = sequence.track(pending.track)?;
                let clip = track.clips.iter().find(|clip| clip.id == pending.clip)?;
                Some((sequence, track, clip))
            });
            let outcome = match found {
                Some((sequence, track, clip)) if clip.src_in >= pending.src_in => {
                    crate::timeline::Timeline::swap_asset(
                        session,
                        sequence,
                        track,
                        clip,
                        id,
                        clip.src_in - pending.src_in,
                    )
                    .map_err(|e| e.to_string())
                }
                _ => Err("its clip is no longer where it was; the new part is among the assets".to_owned()),
            };
            match outcome {
                Ok(()) => {
                    session.notice = Some(
                        "The clip shows the new part. Its menu has \"The picture as it was\"; undo also takes it back."
                            .to_owned(),
                    )
                }
                Err(e) => self.error = Some(format!("{label}: {e}.")),
            }
        }
        true
    }

    /// A part could not be made anew: nothing waits for it any more.
    pub fn part_failed(&mut self, name: &str) {
        self.parts.retain(|pending| pending.name != name);
    }

    /// The key of an asset's music was found (or not): the piano roll
    /// shows its scale, and voice tracks whose pitch correction has no
    /// key yet (it corrects to every semitone) are given it, as one undo step.
    pub fn key_found(&mut self, root: &std::path::Path, id: AssetId, result: Result<(u8, bool), String>) {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return };
        let label = session.project().assets.get(&id).map_or(String::new(), |asset| asset.label.clone());
        let (key, minor) = match result {
            Ok(found) => found,
            Err(e) => {
                self.error = Some(format!("{label}: {e}."));
                return;
            }
        };
        self.editor.piano.scale = Some((key, minor));
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return };
        let ops: Vec<Op> = sequence
            .tracks
            .iter()
            .filter(|t| t.mix.voice && t.mix.tune_scale == 0)
            .map(|t| {
                let mix = sg_core::TrackSettings { tune_key: key, tune_scale: if minor { 2 } else { 1 }, ..t.mix };
                Op::SetTrackMix { seq: sequence.id, track: t.id, mix }
            })
            .collect();
        let voices = ops.len();
        if voices > 0
            && let Err(e) = session.apply(Op::Batch(ops), None)
        {
            self.error = Some(e.to_string());
            return;
        }
        const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        let name = format!("{} {}", NAMES[key as usize % 12], if minor { "minor" } else { "major" });
        let tuned = match voices {
            0 => String::new(),
            n => format!(" The pitch correction of {n} voice track(s) is set to it."),
        };
        session.notice = Some(format!(
            "{label} is in {name} (or its relative, which has the same notes). The piano roll shows that scale.{tuned}"
        ));
    }

    /// A look-up table was copied into the project (or could not be).
    pub fn lut_imported(&mut self, root: &std::path::Path, result: Result<String, String>) -> Vec<Command> {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return Vec::new() };
        match result {
            Ok(name) => {
                session.notice = Some(if name.is_empty() {
                    "The built-in looks are among the project's tables: choose one under Film look.".to_owned()
                } else {
                    format!(
                        "{name} is in the project: add a Film look module to a scenario (Colour) and choose it there."
                    )
                });
                // Read the folder again so that the panel lists it.
                vec![Command::LoadLooks { root: root.to_owned() }]
            }
            Err(e) => {
                self.error = Some(format!("The look-up table was not taken: {e}."));
                Vec::new()
            }
        }
    }
}
