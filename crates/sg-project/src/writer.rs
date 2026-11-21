//! The writer thread of an open project. It owns the journal file and the
//! text files; the editing thread hands it records and snapshots through a
//! channel and never waits for it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use sg_core::{Asset, AssetId, Project, Record, text};

use crate::git;
use crate::journal::Journal;
use crate::save::{self, SavedMark};

/// The journal is fsynced at most this long after an append.
const SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// Text files are rewritten after this much quiet...
const SAVE_IDLE: Duration = Duration::from_secs(2);
/// ...or at the latest this long after the first unsaved edit.
const SAVE_MAX: Duration = Duration::from_secs(10);
/// While there are changes, they are committed to git this often.
const COMMIT_INTERVAL: Duration = Duration::from_secs(300);

enum Msg {
    // (Boxed: a record is far larger than the other messages.)
    Record(Box<Record>, Arc<Project>),
    /// Save and commit the current state.
    Commit,
    /// Save, commit and tag the current state with this name.
    Checkpoint(String),
    Close,
}

/// Progress of the writer, readable from any thread without waiting on I/O.
#[derive(Default)]
pub struct WriterStatus {
    /// Records in the journal.
    pub appended: AtomicU64,
    /// Records whose effect is in the text files.
    pub saved: AtomicU64,
    pub append_last_ns: AtomicU64,
    pub append_max_ns: AtomicU64,
    error: Mutex<Option<String>>,
}

impl WriterStatus {
    /// The last write error, if autosave is failing.
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set_error(&self, error: Option<String>) {
        *self.error.lock().unwrap_or_else(PoisonError::into_inner) = error;
    }
}

pub struct ProjectWriter {
    tx: Sender<Msg>,
    status: Arc<WriterStatus>,
    thread: Option<JoinHandle<()>>,
}

impl ProjectWriter {
    pub(crate) fn spawn(
        root: PathBuf,
        journal: Journal,
        snapshot: Arc<Project>,
        on_disk: &BTreeMap<String, String>,
        saved_records: u64,
        notify: Box<dyn Fn() + Send>,
    ) -> std::io::Result<ProjectWriter> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let status = Arc::new(WriterStatus::default());
        status.appended.store(journal.records(), Ordering::Relaxed);
        status.saved.store(saved_records, Ordering::Relaxed);
        let now = Instant::now();
        let unsaved = journal.records() > saved_records;
        let worker = Worker {
            root,
            journal,
            saved_assets: snapshot.assets.clone(),
            summary: BTreeMap::new(),
            last_commit: now,
            snapshot,
            on_disk: on_disk.iter().map(|(k, v)| (k.clone(), blake3::hash(v.as_bytes()))).collect(),
            saved_records,
            sync_at: None,
            // Edits recovered from the journal on open are written out soon.
            last_edit: now,
            first_unsaved: unsaved.then_some(now),
            status: status.clone(),
            notify,
        };
        let thread = std::thread::Builder::new().name("sg-project-writer".into()).spawn(move || worker.run(rx))?;
        Ok(ProjectWriter { tx, status, thread: Some(thread) })
    }

    /// Queues a record for the journal together with the snapshot it leads
    /// to. Returns immediately.
    pub fn record(&self, record: Record, snapshot: Arc<Project>) {
        // The receiver only goes away if the writer thread panicked; the
        // status then already carries the reason.
        let _ = self.tx.send(Msg::Record(Box::new(record), snapshot));
    }

    pub fn status(&self) -> &Arc<WriterStatus> {
        &self.status
    }

    /// Asks the writer to save everything and stop. Join the returned handle
    /// (off the UI thread) before opening the same project again.
    pub fn close(mut self) -> Option<JoinHandle<()>> {
        let _ = self.tx.send(Msg::Close);
        self.thread.take()
    }
}

impl Drop for ProjectWriter {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Close);
    }
}

struct Worker {
    root: PathBuf,
    journal: Journal,
    snapshot: Arc<Project>,
    on_disk: BTreeMap<String, blake3::Hash>,
    saved_records: u64,
    /// The asset set as of the last save, to move media in and out of the trash.
    saved_assets: Assets,
    /// What was done since the last commit, for its message.
    summary: BTreeMap<&'static str, u32>,
    last_commit: Instant,
    sync_at: Option<Instant>,
    last_edit: Instant,
    first_unsaved: Option<Instant>,
    status: Arc<WriterStatus>,
    notify: Box<dyn Fn() + Send>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Msg>) {
        loop {
            let msg = match self.next_deadline() {
                Some(deadline) => match rx.recv_deadline(deadline) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => Some(Msg::Close),
                },
                None => Some(rx.recv().unwrap_or(Msg::Close)),
            };
            match msg {
                Some(Msg::Record(record, snapshot)) => self.append(&record, snapshot),
                Some(Msg::Close) => {
                    if self.first_unsaved.is_some() {
                        self.save();
                    }
                    self.sync();
                    // Closing a project is one of the moments it is committed (03).
                    self.commit();
                    return;
                }
                Some(Msg::Commit) => {
                    if self.first_unsaved.is_some() {
                        self.save();
                    }
                    self.commit();
                }
                Some(Msg::Checkpoint(name)) => {
                    if self.first_unsaved.is_some() {
                        self.save();
                    }
                    self.commit();
                    match git::checkpoint(&self.root, &name) {
                        Ok(tag) => log::info!("checkpoint {tag}"),
                        Err(e) => self.fail(format!("checkpoint failed: {e}")),
                    }
                    (self.notify)();
                }
                None => {}
            }
            let now = Instant::now();
            if self.sync_at.is_some_and(|t| now >= t) {
                self.sync();
            }
            if self.save_at().is_some_and(|t| now >= t) {
                self.save();
            }
        }
    }

    fn save_at(&self) -> Option<Instant> {
        self.first_unsaved.map(|first| (self.last_edit + SAVE_IDLE).min(first + SAVE_MAX))
    }

    fn next_deadline(&self) -> Option<Instant> {
        match (self.sync_at, self.save_at()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn append(&mut self, record: &Record, snapshot: Arc<Project>) {
        let start = Instant::now();
        let result = self.journal.append(record);
        *self.summary.entry(record_label(record)).or_default() += 1;
        let ns = start.elapsed().as_nanos() as u64;
        self.snapshot = snapshot;
        self.last_edit = start;
        self.first_unsaved.get_or_insert(start);
        self.sync_at.get_or_insert(start + SYNC_INTERVAL);
        self.status.append_last_ns.store(ns, Ordering::Relaxed);
        self.status.append_max_ns.fetch_max(ns, Ordering::Relaxed);
        self.status.appended.store(self.journal.records(), Ordering::Relaxed);
        if let Err(e) = result {
            self.fail(format!("journal write failed: {e}"));
        }
    }

    fn sync(&mut self) {
        self.sync_at = None;
        if let Err(e) = self.journal.sync() {
            self.fail(format!("journal sync failed: {e}"));
        }
    }

    fn save(&mut self) {
        self.first_unsaved = None;
        // The journal must be durable before files that claim to include it.
        self.sync();
        let files = text::to_files(&self.snapshot);
        let hashes: BTreeMap<String, blake3::Hash> =
            files.iter().map(|(k, v)| (k.clone(), blake3::hash(v.as_bytes()))).collect();
        let mark = SavedMark { records: self.journal.records(), hash: save::files_hash(&files) };
        let unchanged = |rel: &str, _: &str| self.on_disk.get(rel) == hashes.get(rel);
        match save::commit(&self.root, &files, unchanged, &mark) {
            Ok(()) => {
                self.on_disk = hashes;
                self.saved_records = mark.records;
                if !Arc::ptr_eq(&self.saved_assets, &self.snapshot.assets) {
                    reconcile_media(&self.root, &self.saved_assets, &self.snapshot.assets);
                    self.saved_assets = self.snapshot.assets.clone();
                }
                self.status.saved.store(mark.records, Ordering::Relaxed);
                self.status.set_error(None);
                // While editing, a commit every few minutes (03: proposed 5).
                if self.last_commit.elapsed() >= COMMIT_INTERVAL {
                    self.commit();
                }
                (self.notify)();
            }
            Err(e) => {
                // Try again after the next edit or at the next deadline.
                self.first_unsaved = Some(Instant::now());
                self.last_edit = Instant::now() + SAVE_MAX;
                self.fail(format!("save failed: {e}"));
            }
        }
    }

    fn fail(&self, message: String) {
        log::error!("{}: {message}", self.root.display());
        self.status.set_error(Some(message));
        (self.notify)();
    }
}

type Assets = Arc<BTreeMap<AssetId, Arc<Asset>>>;

fn trash_path(root: &std::path::Path, asset: &Asset) -> PathBuf {
    let name = std::path::Path::new(&asset.file).file_name().unwrap_or_default();
    root.join(".shotgun/trash").join(name)
}

/// Media of removed assets moves to the trash and comes back when the asset
/// does (undo, restore of an older state). Renames on one filesystem, so
/// both directions are instant and nothing is ever deleted here (spike S5).
fn reconcile_media(root: &std::path::Path, old: &Assets, new: &Assets) {
    let moves =
        old.values().filter(|a| !new.contains_key(&a.id)).map(|a| (root.join(&a.file), trash_path(root, a))).chain(
            new.values().filter(|a| !old.contains_key(&a.id)).map(|a| (trash_path(root, a), root.join(&a.file))),
        );
    for (from, to) in moves {
        // A freshly imported asset is already in place: only move what is there.
        if from.exists()
            && !to.exists()
            && let Err(e) = std::fs::rename(&from, &to)
        {
            log::error!("cannot move {} to {}: {e}", from.display(), to.display());
        }
    }
}

fn record_label(record: &Record) -> &'static str {
    match record {
        Record::Do { op, .. } => op.label(),
        Record::Undo => "undo",
        Record::Redo => "redo",
    }
}

impl Worker {
    /// Commits the saved files, with a message that says what was done.
    fn commit(&mut self) {
        self.last_commit = Instant::now();
        let mut parts: Vec<String> = self
            .summary
            .iter()
            .map(|(label, n)| if *n == 1 { (*label).to_owned() } else { format!("{n} x {label}") })
            .collect();
        parts.truncate(6);
        let message = if parts.is_empty() { "Save".to_owned() } else { parts.join(", ") };
        match git::commit_if_changed(&self.root, &message) {
            Ok(_) => self.summary.clear(),
            // No repository (a bundle copied without .git) is not an error worth a banner.
            Err(e) => log::warn!("{}: no commit: {e}", self.root.display()),
        }
    }
}

impl ProjectWriter {
    /// Names the current state. Saved and committed first; returns at once.
    pub fn checkpoint(&self, name: String) {
        let _ = self.tx.send(Msg::Checkpoint(name));
    }
}

impl ProjectWriter {
    /// Saves and commits now (before an export, for instance).
    pub fn commit(&self) {
        let _ = self.tx.send(Msg::Commit);
    }
}
