//! Creating and opening project bundles.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sg_core::model::{SequenceHeader, TrackHeader, TrackKind};
use sg_core::text::{self, slug, unique_slug};
use sg_core::{Document, History, IdGen, Op, Project, Rate, Record, apply};

use crate::journal::Journal;
use crate::layout::{GITATTRIBUTES, GITIGNORE, ensure_dirs, journal_file};
use crate::save::{self, SavedMark};
use crate::writer::ProjectWriter;
use crate::{ProjectError, at, git, library, random_seed};

pub struct Created {
    pub path: PathBuf,
    /// The project exists and works, but has no git history yet.
    pub git_error: Option<String>,
}

pub struct Opened {
    pub root: PathBuf,
    pub document: Document,
    pub writer: ProjectWriter,
    /// Edits found in the journal that the text files did not contain yet
    /// (the app was killed before it saved them).
    pub recovered_edits: usize,
    /// The text files were changed outside the app, so the old undo history
    /// no longer applies and was set aside.
    pub history_reset: bool,
    /// Entropy for the ids of things created in this session, read here so
    /// that the UI thread never has to.
    pub seed: u64,
}

/// The state of a new project: one 1080p60 sequence with a video and an
/// audio track.
fn new_project(name: &str) -> Arc<Project> {
    let mut ids = IdGen::new(random_seed());
    let seq = ids.sequence();
    let ops = [
        Op::AddSequence {
            index: 0,
            sequence: SequenceHeader { id: seq, name: "Main".into(), rate: Rate::FPS_60, width: 1920, height: 1080 },
        },
        Op::AddTrack {
            seq,
            index: 0,
            track: TrackHeader { id: ids.track(), kind: TrackKind::Video, name: "V1".into() },
        },
        Op::AddTrack {
            seq,
            index: 1,
            track: TrackHeader { id: ids.track(), kind: TrackKind::Audio, name: "A1".into() },
        },
    ];
    let mut project = Arc::new(Project::new(ids.project(), name));
    for op in &ops {
        // These ops are valid on an empty project by construction.
        if let Ok((next, _)) = apply(&project, op) {
            project = next;
        }
    }
    project
}

/// Creates a project folder in `library` with the full layout and a git
/// repository, and returns its path.
pub fn create(library: &Path, name: &str) -> Result<Created, ProjectError> {
    let name = match name.trim() {
        "" => "Untitled",
        trimmed => trimmed,
    };
    std::fs::create_dir_all(library).map_err(at(library))?;
    let root = loop {
        let candidate = library.join(unique_slug(&slug(name), |s| library.join(s).exists()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            // Lost a race for the name: pick the next free one.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(at(&candidate)(e)),
        }
    };
    ensure_dirs(&root)?;
    let files = text::to_files(&new_project(name));
    let mark = SavedMark { records: 0, hash: save::files_hash(&files) };
    save::commit(&root, &files, |_, _| false, &mark)?;
    for (file, content) in [(".gitignore", GITIGNORE), (".gitattributes", GITATTRIBUTES)] {
        let path = root.join(file);
        std::fs::write(&path, content).map_err(at(&path))?;
    }
    let git_error = git::init(&root)
        .and_then(|()| git::commit_all(&root, &format!("Create project {name}")))
        .err()
        .map(|e| e.to_string());
    if let Some(e) = &git_error {
        log::warn!("{}: {e}", root.display());
    }
    Ok(Created { path: root, git_error })
}

/// Opens a bundle: finishes an interrupted save, loads the text files,
/// replays journal records the files do not contain yet, and starts the
/// writer thread. `notify` is called from the writer thread whenever its
/// status changes.
pub fn open(root: &Path, notify: impl Fn() + Send + 'static) -> Result<Opened, ProjectError> {
    save::recover(root)?;
    let files = save::read_tracked(root)?;
    let project = Arc::new(text::from_files(&files)?);
    ensure_dirs(root)?;
    let hash = save::files_hash(&files);
    let journal_path = journal_file(root);
    let (journal, records) = Journal::open(&journal_path)?;

    let in_sync = save::read_mark(root).filter(|m| m.hash == hash && m.records <= journal.records());
    let replayed = in_sync.and_then(|mark| {
        let recovered = records.len() - mark.records as usize;
        replay(&project, records, mark.records as usize).map(|doc| (doc, mark.records, recovered))
    });
    let (document, journal, saved_records, recovered_edits, history_reset) = match replayed {
        Some((document, saved, recovered)) => (document, journal, saved, recovered, false),
        None => {
            // No marker (fresh clone or unzip without .shotgun), or the files
            // differ from what the app wrote: the files are the truth.
            let had_history = journal.records() > 0;
            drop(journal);
            let journal = Journal::reset(&journal_path)?;
            save::commit(root, &files, |_, _| true, &SavedMark { records: 0, hash })?;
            (Document::new(project, History::default()), journal, 0, 0, had_history)
        }
    };
    if history_reset {
        log::warn!("{}: project files changed outside the app; undo history set aside", root.display());
    }
    library::touch_last_opened(root)?;
    let writer = ProjectWriter::spawn(
        root.to_owned(),
        journal,
        document.project().clone(),
        &files,
        saved_records,
        Box::new(notify),
    )
    .map_err(at(root))?;
    Ok(Opened { root: root.to_owned(), document, writer, recovered_edits, history_reset, seed: random_seed() })
}

/// Rebuilds the document: the first `saved` records are already in
/// `project`, so only the history follows them; the rest is re-executed.
fn replay(project: &Arc<Project>, records: Vec<Record>, saved: usize) -> Option<Document> {
    let mut history = History::default();
    let mut records = records.into_iter();
    for record in records.by_ref().take(saved) {
        history.note(record).ok()?;
    }
    let mut document = Document::new(project.clone(), history);
    for record in records {
        if let Err(e) = document.replay(record) {
            log::error!("journal does not apply to the saved project: {e}");
            return None;
        }
    }
    Some(document)
}
