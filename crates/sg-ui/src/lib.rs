//! The user interface: library, editor shell, key map, performance overlay.
//!
//! Everything here runs on the UI thread and therefore does no I/O: work
//! that touches the disk leaves as a [`Command`] for the application to run
//! on a worker thread, and comes back through [`Shell::set_library`],
//! [`Shell::opened`], [`Shell::import_event`] and [`Shell::poster_loaded`].

mod assets;
mod clip_panel;
mod editor;
mod history;
pub mod keymap;
mod library;
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
use crate::history::{HistoryPanel, HistoryRequest};
pub use crate::keymap::{Action, Scope};
use crate::library::{Library, LibraryRequest};
pub use crate::perf::Perf;
pub use crate::session::Session;
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
    LoadPoster {
        root: PathBuf,
        id: AssetId,
    },
    /// Show the system file picker; the chosen files are imported.
    PickFiles,
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
    Image { data: Vec<u8>, mime: String },
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
    /// Restore the n-th entry of the loaded history list (0 is the newest).
    Restore(usize),
    /// Zoom (frames per point) and scroll (frame at the left edge) of the timeline.
    TimelineView(f64, f64),
    /// The scripted zoom-and-scroll of the frame-time benchmark at this time in seconds.
    TimelineBench(f64),
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
    playing: bool,
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
        self.playhead = view.frame;
        self.playing = view.playing;
        match &mut self.session {
            Some(session) => {
                let request = self.editor.ui(ui, session, &mut self.error, &mut asset_requests, view, &mut transport);
                self.editor.assets.popover(&ctx, &mut asset_requests);
                self.editor.assets.drop_overlay(&ctx);
                match request {
                    Some(EditorRequest::Close) => self.act(Action::CloseProject, &mut commands),
                    Some(EditorRequest::Undo) => self.act(Action::Undo, &mut commands),
                    Some(EditorRequest::Redo) => self.act(Action::Redo, &mut commands),
                    Some(EditorRequest::RemoveAsset(id)) => self.remove_asset(id),
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
                AssetRequest::ImportLink(link) => commands.extend(self.incoming(Incoming::Text(link))),
                AssetRequest::Browse => commands.push(Command::PickFiles),
                AssetRequest::CancelJob(job) => commands.push(Command::CancelImport(job)),
                AssetRequest::Insert => self.insert_selected(&mut commands),
            }
        }
        commands.extend(transport.into_iter().map(Command::Transport));
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
        self.perf.ui(&ctx, self.session.as_ref(), &view.stats);
        commands
    }

    /// Errors and notices: a strip at the bottom, never a dialog.
    fn banner(&mut self, ui: &mut egui::Ui) {
        let notice = self.session.as_ref().and_then(|s| s.notice.clone());
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
                if self.error.take().is_some() || self.history.cancel() || self.editor.cancel() {
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
            Action::InsertAsset => self.insert_selected(commands),
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
            Action::Checkpoint => {
                if self.session.is_some() {
                    self.history.begin_checkpoint();
                }
            }
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
            Incoming::Text(text) if sg_project::fetch::looks_like_url(&text) => {
                vec![ImportSource::Url(text.trim().to_owned())]
            }
            Incoming::Text(_) => {
                self.error = Some("The clipboard holds text that is not a link.".to_owned());
                Vec::new()
            }
        };
        let (root, known) = (session.root.clone(), session.project().assets.clone());
        sources.into_iter().map(|source| Command::Import { root: root.clone(), known: known.clone(), source }).collect()
    }

    /// A drag from another application entered or left the window.
    pub fn set_drag_hover(&mut self, hover: bool) {
        self.editor.assets.drag_hover = hover && self.session.is_some();
    }

    pub fn import_event(&mut self, event: ImportEvent) {
        let assets = &mut self.editor.assets;
        // Events of a project that was closed in the meantime are dropped.
        let current = |root: &PathBuf| self.session.as_ref().is_some_and(|s| s.root == *root);
        match event {
            ImportEvent::Progress { job, label, stage, fraction } => {
                assets.jobs.insert(job, JobView { label, stage, fraction });
            }
            ImportEvent::Finished { job } => {
                assets.jobs.remove(&job);
            }
            ImportEvent::Failed { label, error, .. } => self.error = Some(format!("{label}: {error}")),
            ImportEvent::Duplicate { label, id, .. } => {
                assets.selected = Some(id);
                if let Some(session) = &mut self.session {
                    session.notice = Some(format!("{label} is already in this project."));
                }
            }
            ImportEvent::Usable { root, asset, .. } if current(&root) => {
                let id = asset.id;
                if let Some(session) = &mut self.session {
                    match session.apply(Op::AddAsset { asset }, None) {
                        Ok(()) => assets.selected = Some(id),
                        // Two imports of the same content raced: the first one won.
                        Err(sg_core::OpError::DuplicateId(_)) => assets.selected = Some(id),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
            }
            ImportEvent::Derived { root, id, what: sg_project::Derived::Poster } if current(&root) => {
                assets.poster_changed(id)
            }
            ImportEvent::Missing { root, id } if current(&root) => {
                assets.missing.insert(id);
            }
            ImportEvent::Usable { .. } | ImportEvent::Derived { .. } | ImportEvent::Missing { .. } => {}
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
    fn insert_selected(&mut self, commands: &mut Vec<Command>) {
        let Some(session) = &mut self.session else { return };
        let Some(asset) = self.editor.assets.selected.and_then(|id| session.project().assets.get(&id).cloned()) else {
            return;
        };
        match session.place_asset(&asset, self.playhead) {
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
