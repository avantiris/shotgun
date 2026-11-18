//! Writing the tracked text files as one atomic group.
//!
//! A save may touch several files (project.toml, sequences) plus the marker
//! that says which journal position they reflect. A crash between two of
//! them would leave a state that matches no point in the journal. So the new
//! contents are first written completely to a pending directory, then a
//! manifest is committed with one rename, and only then are the files moved
//! into place. A crash before the manifest leaves the old state, a crash
//! after it is finished by [`recover`] on the next open.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sg_core::text::{self, PROJECT_FILE};

use crate::layout::journal_dir;
use crate::{ProjectError, at};

const MARK_PATH: &str = ".shotgun/journal/saved.json";

/// Which journal position the text files on disk reflect.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SavedMark {
    /// The files contain the effect of the first `records` journal records.
    pub records: u64,
    /// [`files_hash`] of the files as written. A different hash on open means
    /// they were changed from outside (git checkout, manual edit).
    pub hash: String,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    /// (file in the pending directory, destination relative to the bundle root)
    writes: Vec<(String, String)>,
    deletes: Vec<String>,
}

pub fn files_hash(files: &BTreeMap<String, String>) -> String {
    let mut hasher = blake3::Hasher::new();
    for (path, content) in files {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Reads `project.toml`, the sequence files it lists and every asset index file.
pub fn read_tracked(root: &Path) -> Result<BTreeMap<String, String>, ProjectError> {
    let project_file = root.join(PROJECT_FILE);
    let toml = match std::fs::read_to_string(&project_file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProjectError::NotAProject(root.to_owned()));
        }
        Err(e) => return Err(at(&project_file)(e)),
    };
    let mut files = BTreeMap::new();
    for rel in text::sequence_paths(&toml)? {
        let path = root.join(&rel);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                files.insert(rel, content);
            }
            // Reported by `from_files` with the proper message.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(at(&path)(e)),
        }
    }
    for rel in tracked_files_on_disk(root).into_iter().filter(|f| f.starts_with(text::ASSET_INDEX_DIR)) {
        let path = root.join(&rel);
        let content = std::fs::read_to_string(&path).map_err(at(&path))?;
        files.insert(rel, content);
    }
    files.insert(PROJECT_FILE.to_owned(), toml);
    Ok(files)
}

pub fn read_mark(root: &Path) -> Option<SavedMark> {
    let bytes = std::fs::read(root.join(MARK_PATH)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Sequence and asset index files present on disk, relative to the root.
/// A save deletes those that the new state no longer has.
fn tracked_files_on_disk(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for (dir, extension) in [("sequences", ".sg"), ("assets/index", ".toml")] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        found.extend(
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|name| name.ends_with(extension))
                .map(|name| format!("{dir}/{name}")),
        );
    }
    found
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let mut file = File::create(path).map_err(at(path))?;
    file.write_all(bytes).map_err(at(path))?;
    file.sync_all().map_err(at(path))
}

fn sync_dir(path: &Path) -> Result<(), ProjectError> {
    File::open(path).and_then(|d| d.sync_all()).map_err(at(path))
}

fn pending_dir(root: &Path) -> PathBuf {
    journal_dir(root).join("pending")
}

/// Makes the files on disk equal to `files` and records `mark`, atomically.
/// `unchanged` names files known to have the wanted content already.
pub fn commit(
    root: &Path,
    files: &BTreeMap<String, String>,
    unchanged: impl Fn(&str, &str) -> bool,
    mark: &SavedMark,
) -> Result<(), ProjectError> {
    let pending = pending_dir(root);
    match std::fs::remove_dir_all(&pending) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(at(&pending)(e)),
        _ => {}
    }
    std::fs::create_dir_all(&pending).map_err(at(&pending))?;

    let mut manifest = Manifest { writes: Vec::new(), deletes: Vec::new() };
    for (i, (rel, content)) in files.iter().enumerate() {
        if unchanged(rel, content) {
            continue;
        }
        let name = format!("f{i}");
        write_synced(&pending.join(&name), content.as_bytes())?;
        manifest.writes.push((name, rel.clone()));
    }
    // serde_json cannot fail on these plain structs.
    let mark_json = serde_json::to_vec(mark).unwrap_or_default();
    write_synced(&pending.join("mark"), &mark_json)?;
    manifest.writes.push(("mark".to_owned(), MARK_PATH.to_owned()));
    manifest.deletes = tracked_files_on_disk(root).into_iter().filter(|f| !files.contains_key(f)).collect();

    let tmp = pending.join("commit.json.tmp");
    write_synced(&tmp, &serde_json::to_vec(&manifest).unwrap_or_default())?;
    let commit_file = pending.join("commit.json");
    std::fs::rename(&tmp, &commit_file).map_err(at(&commit_file))?;
    sync_dir(&pending)?;
    apply(root, &manifest)
}

/// Finishes a save that was committed but not completed, or discards one
/// that was not committed. Call before reading a bundle.
pub fn recover(root: &Path) -> Result<(), ProjectError> {
    let pending = pending_dir(root);
    let commit_file = pending.join("commit.json");
    match std::fs::read(&commit_file) {
        Ok(bytes) => match serde_json::from_slice::<Manifest>(&bytes) {
            Ok(manifest) => {
                log::warn!("{}: finishing an interrupted save", root.display());
                apply(root, &manifest)
            }
            Err(e) => Err(at(&commit_file)(e.into())),
        },
        Err(_) if pending.exists() => std::fs::remove_dir_all(&pending).map_err(at(&pending)),
        Err(_) => Ok(()),
    }
}

/// Idempotent: a file already moved by an earlier, interrupted run is skipped.
fn apply(root: &Path, manifest: &Manifest) -> Result<(), ProjectError> {
    let pending = pending_dir(root);
    for (name, rel) in &manifest.writes {
        let (from, to) = (pending.join(name), root.join(rel));
        match std::fs::rename(&from, &to) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !from.exists() => {}
            Err(e) => return Err(at(&to)(e)),
            Ok(()) => {}
        }
    }
    for rel in &manifest.deletes {
        let path = root.join(rel);
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(at(&path)(e)),
            _ => {}
        }
    }
    for dir in [root.join("sequences"), root.join("assets/index"), root.to_owned(), journal_dir(root)] {
        sync_dir(&dir)?;
    }
    std::fs::remove_dir_all(&pending).map_err(at(&pending))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ensure_dirs;

    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn mark(records: u64) -> SavedMark {
        SavedMark { records, hash: "h".into() }
    }

    const TOML_A: &str = "format = 1\nid = \"0000000000000000000000000000002a\"\nname = \"T\"\nsequences = [\"a\"]\n";
    const TOML_B: &str = "format = 1\nid = \"0000000000000000000000000000002a\"\nname = \"T\"\nsequences = [\"b\"]\n";

    #[test]
    fn commit_writes_deletes_and_marks() {
        let dir = tempfile::tempdir().unwrap();
        ensure_dirs(dir.path()).unwrap();
        let first = files(&[("project.toml", TOML_A), ("sequences/a.sg", "one")]);
        commit(dir.path(), &first, |_, _| false, &mark(0)).unwrap();
        assert_eq!(read_tracked(dir.path()).unwrap(), first);
        assert_eq!(read_mark(dir.path()), Some(mark(0)));

        let second = files(&[("project.toml", TOML_B), ("sequences/b.sg", "two")]);
        commit(dir.path(), &second, |_, _| false, &mark(5)).unwrap();
        assert_eq!(read_tracked(dir.path()).unwrap(), second);
        assert!(!dir.path().join("sequences/a.sg").exists());
        assert_eq!(read_mark(dir.path()), Some(mark(5)));
        assert!(!pending_dir(dir.path()).exists());
    }

    #[test]
    fn unchanged_files_are_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        ensure_dirs(dir.path()).unwrap();
        let state = files(&[("project.toml", TOML_A), ("sequences/a.sg", "one")]);
        commit(dir.path(), &state, |_, _| false, &mark(0)).unwrap();
        let before = std::fs::metadata(dir.path().join("sequences/a.sg")).unwrap().modified().unwrap();
        commit(dir.path(), &state, |_, _| true, &mark(1)).unwrap();
        let after = std::fs::metadata(dir.path().join("sequences/a.sg")).unwrap().modified().unwrap();
        assert_eq!(before, after);
        assert_eq!(read_mark(dir.path()), Some(mark(1)));
    }

    /// Simulates a crash at every point of the move phase: the manifest is
    /// committed, some files are already in place.
    #[test]
    fn recover_finishes_a_committed_save() {
        for moved in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            ensure_dirs(root).unwrap();
            let old = files(&[("project.toml", TOML_A), ("sequences/a.sg", "one")]);
            commit(root, &old, |_, _| false, &mark(0)).unwrap();

            let pending = pending_dir(root);
            std::fs::create_dir_all(&pending).unwrap();
            let writes = [
                ("f0", "project.toml", TOML_B),
                ("f1", "sequences/b.sg", "two"),
                ("mark", MARK_PATH, "{\"records\":9,\"hash\":\"h\"}"),
            ];
            for (i, (name, rel, content)) in writes.iter().enumerate() {
                let target = if i < moved { root.join(rel) } else { pending.join(name) };
                std::fs::write(target, content).unwrap();
            }
            let manifest = Manifest {
                writes: writes.iter().map(|(n, r, _)| (n.to_string(), r.to_string())).collect(),
                deletes: vec!["sequences/a.sg".into()],
            };
            std::fs::write(pending.join("commit.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();

            recover(root).unwrap();
            let want = files(&[("project.toml", TOML_B), ("sequences/b.sg", "two")]);
            assert_eq!(read_tracked(root).unwrap(), want, "moved={moved}");
            assert_eq!(read_mark(root), Some(mark(9)));
            assert!(!root.join("sequences/a.sg").exists());
            assert!(!pending.exists());
        }
    }

    #[test]
    fn recover_discards_an_uncommitted_save() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        ensure_dirs(root).unwrap();
        let old = files(&[("project.toml", TOML_A), ("sequences/a.sg", "one")]);
        commit(root, &old, |_, _| false, &mark(0)).unwrap();
        let pending = pending_dir(root);
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("f0"), TOML_B).unwrap();
        std::fs::write(pending.join("commit.json.tmp"), "{").unwrap();
        recover(root).unwrap();
        assert_eq!(read_tracked(root).unwrap(), old);
        assert!(!pending.exists());
    }

    #[test]
    fn hash_depends_on_names_and_contents() {
        let a = files_hash(&files(&[("a", "bc")]));
        assert_ne!(a, files_hash(&files(&[("ab", "c")])));
        assert_ne!(a, files_hash(&files(&[("a", "bd")])));
        assert_eq!(a, files_hash(&files(&[("a", "bc")])));
    }
}
