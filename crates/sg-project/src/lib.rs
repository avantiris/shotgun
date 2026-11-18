//! The project bundle on disk: fixed layout, library scan, journal, atomic
//! saves and git. Everything here does blocking I/O and must be called from
//! the job pool or a project's writer thread, never from the UI thread.

mod bundle;
pub mod fetch;
mod git;
pub mod import;
mod journal;
mod layout;
mod library;
mod save;
mod writer;

use std::path::{Path, PathBuf};

pub use bundle::{Created, Opened, create, open};
pub use git::{HistoryEntry, history, restore};
pub use import::{Derived, ImportEvent, ImportSource, Importer, read_poster};
pub use journal::Journal;
pub use layout::cache;
pub use library::{ProjectEntry, library_dir, scan};
pub use writer::{ProjectWriter, WriterStatus};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Text(#[from] sg_core::text::TextError),
    #[error("{} is not a project (no project.toml)", .0.display())]
    NotAProject(PathBuf),
    #[error("git {command} failed: {stderr}")]
    Git { command: String, stderr: String },
}

/// Attaches the path to an I/O error.
pub(crate) fn at(path: &Path) -> impl FnOnce(std::io::Error) -> ProjectError + '_ {
    move |source| ProjectError::Io { path: path.to_owned(), source }
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Seed for id generation, from the kernel.
pub fn random_seed() -> u64 {
    use std::io::Read;
    let mut buf = [0u8; 8];
    match std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)) {
        Ok(()) => u64::from_le_bytes(buf),
        // Only reachable in a sandbox without /dev; time and pid are unique enough for ids.
        Err(_) => {
            let nanos =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
            nanos ^ (std::process::id() as u64) << 32
        }
    }
}
