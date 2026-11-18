//! An open project as the UI thread holds it: the document plus the handle
//! of its writer thread. Edits are applied here in memory (well under a
//! millisecond) and handed to the writer without waiting.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sg_core::{Document, Op, OpError, Project};
use sg_project::{Opened, ProjectWriter, WriterStatus};

pub struct Session {
    pub root: PathBuf,
    doc: Document,
    writer: ProjectWriter,
    /// One-line message about how the project was opened, if noteworthy.
    pub notice: Option<String>,
    pub apply_last_ns: u64,
    pub apply_max_ns: u64,
}

impl Session {
    pub fn new(opened: Opened) -> Session {
        let notice = if opened.history_reset {
            Some(
                "Project files were changed outside Shotgun. Loaded them; the previous undo history was set aside."
                    .to_owned(),
            )
        } else if opened.recovered_edits > 0 {
            let plural = if opened.recovered_edits == 1 { "" } else { "s" };
            Some(format!("Recovered {} unsaved edit{plural} from the journal.", opened.recovered_edits))
        } else {
            None
        };
        Session {
            root: opened.root,
            doc: opened.document,
            writer: opened.writer,
            notice,
            apply_last_ns: 0,
            apply_max_ns: 0,
        }
    }

    pub fn project(&self) -> &Arc<Project> {
        self.doc.project()
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    pub fn writer_status(&self) -> &Arc<WriterStatus> {
        self.writer.status()
    }

    pub fn apply(&mut self, op: Op, group: Option<u64>) -> Result<(), OpError> {
        let start = Instant::now();
        let record = self.doc.apply(op, group)?;
        self.apply_last_ns = start.elapsed().as_nanos() as u64;
        self.apply_max_ns = self.apply_max_ns.max(self.apply_last_ns);
        self.writer.record(record, self.doc.project().clone());
        Ok(())
    }

    /// Returns false if there was nothing to undo.
    pub fn undo(&mut self) -> Result<bool, OpError> {
        let record = self.doc.undo()?;
        Ok(self.journal(record))
    }

    pub fn redo(&mut self) -> Result<bool, OpError> {
        let record = self.doc.redo()?;
        Ok(self.journal(record))
    }

    fn journal(&mut self, record: Option<sg_core::Record>) -> bool {
        let done = record.is_some();
        if let Some(record) = record {
            self.writer.record(record, self.doc.project().clone());
        }
        done
    }

    /// Ends the session. The caller closes the writer off the UI thread.
    pub fn into_writer(self) -> ProjectWriter {
        self.writer
    }
}
