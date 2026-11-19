//! The user interface: library, editor shell, key map, performance overlay.
//!
//! Everything here runs on the UI thread and therefore does no I/O: work
//! that touches the disk leaves as a [`Command`] for the application to run
//! on a worker thread, and comes back through [`Shell::set_library`],
//! [`Shell::opened`], [`Shell::import_event`] and [`Shell::poster_loaded`].

mod assets;
mod clip_panel;
mod colour_panel;
mod editor;
mod export_panel;
mod history;
pub mod keymap;
mod library;
mod mixer;
mod perf;
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
}

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
    playing: bool,
    /// Files dropped on the timeline: where the next imported assets go
    /// (frame, track row) and how many imports are still expected there.
    dropped_at: Option<(i64, Option<usize>, usize)>,
    /// The key reference is showing.
    help: bool,
    /// Undo group of the angle switches of the playback pass in progress.
    angle_group: Option<u64>,
    /// The playhead as of the last frame, for key actions.
    playhead: i64,
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
            Ok(entries) => self.library.set_entries(entries),
            Err(e) => self.error = Some(format!("Cannot read the library: {e}")),
        }
    }

    pub fn opened(&mut self, result: Result<Opened, String>) -> Vec<Command> {
        self.library.pending = None;
        match result {
            Ok(opened) => {
                self.library.filter.clear();
                self.editor = Editor::default();
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

    pub fn report_error(&mut self, error: String) {
        self.library.pending = None;
        self.error = Some(error);
    }

    /// Builds one frame.
    pub fn frame(&mut self, ui: &mut egui::Ui, now_unix: u64, view: &ViewerState) -> Vec<Command> {
        let ctx = ui.ctx().clone();
        let mut commands = Vec::new();
        for action in keymap::take_actions(&ctx, self.scope()) {
            self.act(action, &mut commands);
        }
        self.banner(ui);
        let mut asset_requests = Vec::new();
        let mut transport = Vec::new();
        let mut scenario_choice = None;
        let mut colour_requests = Vec::new();
        self.playhead = view.frame;
        self.playing = view.playing;
        match &mut self.session {
            Some(session) => {
                let request = self.editor.ui(ui, session, &mut self.error, &mut asset_requests, view, &mut transport);
                let root = session.root.clone();
                if let Some((asset, choice)) = self.editor.timeline.scenario_request.take() {
                    scenario_choice = Some((asset, choice));
                }
                colour_requests = std::mem::take(&mut self.editor.colour_requests);
                if let Some(asset) = self
                    .editor
                    .clip_panel
                    .stabilise_request
                    .take()
                    .and_then(|id| session.project().assets.get(&id).cloned())
                {
                    commands.push(Command::Stabilise { root: root.clone(), asset });
                }
                if std::mem::take(&mut self.editor.clip_panel.fonts_request) {
                    commands.push(Command::LoadFonts { root: root.clone() });
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
                    Some(EditorRequest::Export) => self.act(Action::Export, &mut commands),
                    Some(EditorRequest::Help) => self.act(Action::Help, &mut commands),
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
                    Some(EditorRequest::Undo) => self.act(Action::Undo, &mut commands),
                    Some(EditorRequest::Redo) => self.act(Action::Redo, &mut commands),
                    Some(EditorRequest::RemoveAsset(id)) => self.remove_asset(id),
                    Some(EditorRequest::SetSize(width, height)) => {
                        if let Some(session) = &mut self.session {
                            self.editor.set_size(session, width, height, &mut self.error);
                        }
                    }
                    Some(EditorRequest::AddTrack(kind)) => {
                        if let Some(session) = &mut self.session {
                            self.editor.add_track(session, kind, &mut self.error);
                        }
                    }
                    None => {}
                }
            }
            None => match self.library.ui(ui, now_unix) {
                Some(LibraryRequest::Open(path)) => self.open(path, &mut commands),
                Some(LibraryRequest::Create(name)) => self.create(name, &mut commands),
                None => {}
            },
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
                AssetRequest::Remove => {
                    if let Some(id) = self.editor.assets.selected {
                        self.remove_asset(id);
                    }
                }
            }
        }
        commands.extend(transport.into_iter().map(Command::Transport));
        if let Some((asset, choice)) = scenario_choice {
            self.assign_scenario(asset, choice, &mut commands);
        }
        self.colour_requests(colour_requests, &mut commands);
        let gif_range = self.gif_range();
        match self.export.ui(&ctx, gif_range) {
            Some(ExportAction::Start(request)) => self.start_export(request, &mut commands),
            Some(ExportAction::Cancel) => commands.push(Command::CancelExport),
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
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(text).color(colour));
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
                    commands.push(Command::Close { writer: session.into_writer() });
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
            Action::Paste => {
                if self.session.is_some() {
                    commands.push(Command::ReadClipboard);
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
            | Action::Copy
            | Action::PasteClips
            | Action::Marker
            | Action::PrevCut
            | Action::NextCut => self.timeline_action(action, commands),
            Action::Delete => {
                // Clips if any are selected, otherwise the selected asset.
                if self.editor.timeline.has_selection() {
                    self.timeline_action(action, commands);
                } else if let Some(id) = self.editor.assets.selected {
                    self.remove_asset(id);
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
                    ExportRequest { preset: ExportPreset::Gif, megabytes: 10, range, to_clipboard: true },
                    commands,
                );
            }
            Action::Checkpoint => {
                if self.session.is_some() {
                    self.history.begin_checkpoint();
                }
            }
            Action::Help => self.help = !self.help,
            Action::Mixer => {
                self.editor.mixer.open = !self.editor.mixer.open;
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
            Action::Transition => {
                if let Some(session) = &mut self.session {
                    self.editor.mixer.open = false;
                    self.editor.timeline.toggle_transition(session, self.playhead, &mut self.error);
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
                        (Some(seq), _) if which == "master" => Some(Op::SetMasterGain { id: seq.id, db: value }),
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
            DevStep::Size(width, height) => {
                if let Some(session) = &mut self.session {
                    self.editor.set_size(session, width, height, &mut self.error);
                }
            }
            DevStep::Export(what) => {
                let (preset, megabytes) = match what.split_once(':') {
                    Some(("share", mb)) => (ExportPreset::Share, mb.parse().unwrap_or(10)),
                    _ if what == "vertical" => (ExportPreset::Vertical, 10),
                    _ if what == "gif" => (ExportPreset::Gif, 10),
                    _ => (ExportPreset::Master, 10),
                };
                let range = if preset == ExportPreset::Gif { self.gif_range() } else { None };
                // Scripted exports never touch the clipboard: it belongs to the person at the desk.
                self.start_export(ExportRequest { preset, megabytes, range, to_clipboard: false }, &mut commands);
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

    /// Imports what was dropped, pasted or picked into the open project.
    pub fn incoming(&mut self, incoming: Incoming) -> Vec<Command> {
        self.editor.assets.drag_hover = false;
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
        if let Some(id) = placed {
            self.place_dropped(id);
        }
    }

    pub fn poster_loaded(&mut self, ctx: &egui::Context, root: &std::path::Path, id: AssetId, rgba: Option<Vec<u8>>) {
        if self.session.as_ref().is_some_and(|s| s.root == root) {
            self.editor.assets.set_poster(ctx, id, rgba);
        }
    }
}

impl Shell {
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
            session.notice = Some(format!("Exported {path}"));
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
        self.dropped_at = target.filter(|_| !commands.is_empty()).map(|(frame, row)| (frame, row, commands.len()));
        commands
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
                 It may be too short, too noisy, or from another song.",
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
        egui::Frame::new().fill(theme::RAISED).stroke(Stroke::new(1.0, theme::LINE)).corner_radius(6.0).inner_margin(16.0).show(ui, |ui| {
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
            ui.label(RichText::new("Mouse: wheel zooms the timeline, Shift + wheel scrolls it; drag clips to move (with Ctrl: over what is in the way), their edges to trim; right click for menus; drag assets onto the timeline (Shift: only picture or only sound).").color(theme::DIM));
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
    pub fn fonts_loaded(&mut self, root: &std::path::Path, fonts: Vec<String>) {
        if self.session.as_ref().is_some_and(|s| s.root == root) {
            self.editor.clip_panel.fonts = Some(fonts);
        }
    }

    /// A font was copied into the project (or could not be).
    pub fn font_imported(&mut self, root: &std::path::Path, result: Result<String, String>) -> Vec<Command> {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return Vec::new() };
        match result {
            Ok(name) => {
                session.notice = Some(format!("{name} is in the project: choose it under Font in a title's settings."));
                vec![Command::LoadFonts { root: root.to_owned() }]
            }
            Err(e) => {
                self.error = Some(format!("The font was not taken: {e}."));
                Vec::new()
            }
        }
    }

    /// A look-up table was copied into the project (or could not be).
    pub fn lut_imported(&mut self, root: &std::path::Path, result: Result<String, String>) -> Vec<Command> {
        let Some(session) = self.session.as_mut().filter(|s| s.root == root) else { return Vec::new() };
        match result {
            Ok(name) => {
                session.notice = Some(format!(
                    "{name} is in the project: add a Film look module to a scenario (Colour) and choose it there."
                ));
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
