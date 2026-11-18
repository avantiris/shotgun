//! The edit journal: one JSON line per record, appended as the edit happens.
//! It is the durable form of the undo history and of every edit made since
//! the text files were last written.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sg_core::Record;

use crate::{ProjectError, at, unix_now};

#[derive(Serialize)]
struct LineOut<'a> {
    /// Unix seconds; for commit messages and history compaction.
    t: u64,
    r: &'a Record,
}

#[derive(Deserialize)]
struct LineIn {
    r: Record,
}

pub struct Journal {
    file: File,
    buf: Vec<u8>,
    records: u64,
}

impl Journal {
    /// Opens the journal for appending and returns the records in it. A torn
    /// or corrupt tail (crash in the middle of a write) is cut off.
    pub fn open(path: &Path) -> Result<(Journal, Vec<Record>), ProjectError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(at(path)(e)),
        };
        let mut records = Vec::new();
        let mut valid = 0;
        for line in bytes.split_inclusive(|&b| b == b'\n') {
            let complete = line.last() == Some(&b'\n');
            match serde_json::from_slice::<LineIn>(line) {
                Ok(parsed) if complete => records.push(parsed.r),
                _ => break,
            }
            valid += line.len();
        }
        let file = OpenOptions::new().append(true).create(true).open(path).map_err(at(path))?;
        if valid < bytes.len() {
            let lost = &bytes[valid..];
            log::warn!("{}: dropping {} unreadable bytes at the end", path.display(), lost.len());
            // More than one line means damage in the middle, not a torn last
            // write: keep the original for inspection.
            if lost.iter().filter(|&&b| b == b'\n').count() > 1 {
                let copy = path.with_extension(format!("corrupt-{}", unix_now()));
                std::fs::write(&copy, &bytes).map_err(at(&copy))?;
            }
            file.set_len(valid as u64).map_err(at(path))?;
        }
        let journal = Journal { file, buf: Vec::with_capacity(512), records: records.len() as u64 };
        Ok((journal, records))
    }

    /// Starts an empty journal, keeping a non-empty old one as a backup.
    /// Used when the history no longer matches the files on disk.
    pub fn reset(path: &Path) -> Result<Journal, ProjectError> {
        if std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
            let backup = path.with_extension(format!("{}.bak", unix_now()));
            std::fs::rename(path, &backup).map_err(at(path))?;
        }
        Ok(Journal::open(path)?.0)
    }

    /// Appends one record with a single `write`. Durable after [`sync`](Self::sync).
    pub fn append(&mut self, record: &Record) -> std::io::Result<()> {
        self.buf.clear();
        serde_json::to_writer(&mut self.buf, &LineOut { t: unix_now(), r: record })?;
        self.buf.push(b'\n');
        self.file.write_all(&self.buf)?;
        self.records += 1;
        Ok(())
    }

    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file.sync_data()
    }

    /// Number of records in the file.
    pub fn records(&self) -> u64 {
        self.records
    }
}

#[cfg(test)]
mod tests {
    use sg_core::Op;

    use super::*;

    fn rec(name: &str) -> Record {
        Record::Do {
            op: Op::SetProjectName { name: name.into() },
            inv: Op::SetProjectName { name: "old".into() },
            group: None,
        }
    }

    #[test]
    fn append_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.log");
        let (mut j, records) = Journal::open(&path).unwrap();
        assert!(records.is_empty());
        j.append(&rec("a")).unwrap();
        j.append(&Record::Undo).unwrap();
        j.sync().unwrap();
        drop(j);
        let (j, records) = Journal::open(&path).unwrap();
        assert_eq!(records, [rec("a"), Record::Undo]);
        assert_eq!(j.records(), 2);
    }

    #[test]
    fn torn_tail_is_cut_and_appending_continues() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.log");
        let (mut j, _) = Journal::open(&path).unwrap();
        j.append(&rec("a")).unwrap();
        j.append(&rec("b")).unwrap();
        drop(j);
        let full = std::fs::read(&path).unwrap();
        // Cut in the middle of the second line, as a crash during write would.
        std::fs::write(&path, &full[..full.len() - 10]).unwrap();
        let (mut j, records) = Journal::open(&path).unwrap();
        assert_eq!(records, [rec("a")]);
        j.append(&rec("c")).unwrap();
        drop(j);
        assert_eq!(Journal::open(&path).unwrap().1, [rec("a"), rec("c")]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no corrupt copy for a torn tail");
    }

    #[test]
    fn damage_in_the_middle_keeps_a_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.log");
        let (mut j, _) = Journal::open(&path).unwrap();
        for name in ["a", "b", "c"] {
            j.append(&rec(name)).unwrap();
        }
        drop(j);
        let text = std::fs::read_to_string(&path).unwrap();
        let broken = text.replacen("\"b\"", "\"b", 1);
        std::fs::write(&path, broken).unwrap();
        assert_eq!(Journal::open(&path).unwrap().1, [rec("a")]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn reset_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.log");
        let (mut j, _) = Journal::open(&path).unwrap();
        j.append(&rec("a")).unwrap();
        drop(j);
        let j = Journal::reset(&path).unwrap();
        assert_eq!(j.records(), 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
