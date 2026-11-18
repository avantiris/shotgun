//! The user interface: library, editor shell, key map, performance overlay.
//!
//! Everything here runs on the UI thread and therefore does no I/O: work
//! that touches the disk leaves as a [`Command`] for the application to run
//! on a worker thread, and comes back through [`Shell::set_library`] and
//! [`Shell::opened`].

mod editor;
pub mod keymap;
mod library;
mod perf;
mod session;
pub mod theme;

use std::path::PathBuf;

use sg_core::Op;
use sg_project::{Opened, ProjectEntry, ProjectWriter};

use crate::editor::{Editor, EditorRequest};
pub use crate::keymap::{Action, Scope};
use crate::library::{Library, LibraryRequest};
pub use crate::perf::Perf;
pub use crate::session::Session;

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
}

#[derive(Default)]
pub struct Shell {
    library: Library,
    editor: Editor,
    session: Option<Session>,
    perf: Perf,
    error: Option<String>,
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

    pub fn opened(&mut self, result: Result<Opened, String>) {
        self.library.pending = None;
        match result {
            Ok(opened) => {
                self.library.filter.clear();
                self.editor = Editor::default();
                self.session = Some(Session::new(opened));
            }
            Err(e) => self.error = Some(e),
        }
    }

    pub fn report_error(&mut self, error: String) {
        self.library.pending = None;
        self.error = Some(error);
    }

    /// Builds one frame.
    pub fn frame(&mut self, ui: &mut egui::Ui, now_unix: u64) -> Vec<Command> {
        let ctx = ui.ctx().clone();
        let mut commands = Vec::new();
        for action in keymap::take_actions(&ctx, self.scope()) {
            self.act(action, &mut commands);
        }
        self.banner(ui);
        match &mut self.session {
            Some(session) => match self.editor.ui(ui, session, &mut self.error) {
                Some(EditorRequest::Close) => self.act(Action::CloseProject, &mut commands),
                Some(EditorRequest::Undo) => self.act(Action::Undo, &mut commands),
                Some(EditorRequest::Redo) => self.act(Action::Redo, &mut commands),
                None => {}
            },
            None => match self.library.ui(ui, now_unix) {
                Some(LibraryRequest::Open(path)) => self.open(path, &mut commands),
                Some(LibraryRequest::Create(name)) => self.create(name, &mut commands),
                None => {}
            },
        }
        self.perf.ui(&ctx, self.session.as_ref());
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
                if self.error.take().is_some() || self.editor.cancel() {
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
