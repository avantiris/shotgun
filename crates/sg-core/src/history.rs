//! Undo/redo. The history is the list of applied ops with their inverses and
//! a cursor; it is rebuilt from the journal on open, so it survives restarts.
//!
//! A [`Record`] is what the journal stores. `Undo` and `Redo` are records
//! too: replaying the journal in order reproduces both the project and the
//! position in the history.

use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::Project;
use crate::ops::{Op, OpError, apply};

// `Do` holds two operations and is by far the largest variant, but it is
// also nearly every record there is: boxing it would cost an allocation
// per edit to save space only in the rare undo and redo records.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Record {
    Do {
        op: Op,
        inv: Op,
        /// Consecutive ops with the same group (one drag, one knob turn) are
        /// undone and redone as a single step.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<u64>,
    },
    Undo,
    Redo,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HistoryError {
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("history does not match the project: {0}")]
    Op(#[from] OpError),
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    op: Op,
    inv: Op,
    group: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    entries: Vec<Entry>,
    /// Entries before the cursor are applied; those after it can be redone.
    cursor: usize,
}

impl History {
    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor < self.entries.len()
    }

    /// Number of applied ops that can be undone.
    pub fn undo_len(&self) -> usize {
        self.cursor
    }

    pub fn redo_len(&self) -> usize {
        self.entries.len() - self.cursor
    }

    fn undo_span(&self) -> Option<Range<usize>> {
        let last = self.cursor.checked_sub(1)?;
        let group = self.entries[last].group;
        let mut lo = last;
        while group.is_some() && lo > 0 && self.entries[lo - 1].group == group {
            lo -= 1;
        }
        Some(lo..self.cursor)
    }

    fn redo_span(&self) -> Option<Range<usize>> {
        let group = self.entries.get(self.cursor)?.group;
        let mut hi = self.cursor + 1;
        while group.is_some() && self.entries.get(hi).is_some_and(|e| e.group == group) {
            hi += 1;
        }
        Some(self.cursor..hi)
    }

    fn push(&mut self, op: Op, inv: Op, group: Option<u64>) {
        self.entries.truncate(self.cursor);
        self.entries.push(Entry { op, inv, group });
        self.cursor = self.entries.len();
    }

    /// Follows a journal record without touching any project: used for the
    /// part of the journal whose effect is already in the saved files.
    pub fn note(&mut self, record: Record) -> Result<(), HistoryError> {
        match record {
            Record::Do { op, inv, group } => self.push(op, inv, group),
            Record::Undo => self.cursor = self.undo_span().ok_or(HistoryError::NothingToUndo)?.start,
            Record::Redo => self.cursor = self.redo_span().ok_or(HistoryError::NothingToRedo)?.end,
        }
        Ok(())
    }
}

/// A project snapshot together with its history: the editing state of one
/// open project. Lives on the thread that edits; readers get `Arc` clones of
/// the snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    project: Arc<Project>,
    history: History,
}

impl Document {
    pub fn new(project: Arc<Project>, history: History) -> Document {
        Document { project, history }
    }

    pub fn project(&self) -> &Arc<Project> {
        &self.project
    }

    pub fn history(&self) -> &History {
        &self.history
    }

    /// Applies `op` and returns the record to append to the journal.
    pub fn apply(&mut self, op: Op, group: Option<u64>) -> Result<Record, OpError> {
        let (next, inv) = apply(&self.project, &op)?;
        self.project = next;
        self.history.push(op.clone(), inv.clone(), group);
        Ok(Record::Do { op, inv, group })
    }

    /// Undoes the last step. `Ok(None)` if there is nothing to undo.
    pub fn undo(&mut self) -> Result<Option<Record>, OpError> {
        let Some(span) = self.history.undo_span() else { return Ok(None) };
        let mut project = self.project.clone();
        for entry in self.history.entries[span.clone()].iter().rev() {
            project = apply(&project, &entry.inv)?.0;
        }
        self.project = project;
        self.history.cursor = span.start;
        Ok(Some(Record::Undo))
    }

    /// Redoes the last undone step. `Ok(None)` if there is nothing to redo.
    pub fn redo(&mut self) -> Result<Option<Record>, OpError> {
        let Some(span) = self.history.redo_span() else { return Ok(None) };
        let mut project = self.project.clone();
        for entry in &self.history.entries[span.clone()] {
            project = apply(&project, &entry.op)?.0;
        }
        self.project = project;
        self.history.cursor = span.end;
        Ok(Some(Record::Redo))
    }

    /// Re-executes a journal record.
    pub fn replay(&mut self, record: Record) -> Result<(), HistoryError> {
        match record {
            Record::Do { op, inv, group } => {
                self.project = apply(&self.project, &op)?.0;
                self.history.push(op, inv, group);
            }
            Record::Undo => {
                self.undo()?.ok_or(HistoryError::NothingToUndo)?;
            }
            Record::Redo => {
                self.redo()?.ok_or(HistoryError::NothingToRedo)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::ops::tests::{SEQ, V1, any_op, base, clip, make_op};

    fn insert(id: u64, start: i64) -> Op {
        Op::InsertClip { seq: SEQ, track: V1, clip: clip(id, start, 10) }
    }

    fn count(doc: &Document) -> usize {
        doc.project().clip_count()
    }

    #[test]
    fn undo_redo_walk() {
        let mut doc = Document::new(base(), History::default());
        assert_eq!(doc.undo().unwrap(), None);
        doc.apply(insert(1, 0), None).unwrap();
        doc.apply(insert(2, 10), None).unwrap();
        assert_eq!(doc.undo().unwrap(), Some(Record::Undo));
        assert_eq!(count(&doc), 1);
        assert_eq!(doc.redo().unwrap(), Some(Record::Redo));
        assert_eq!(count(&doc), 2);
        assert_eq!(doc.redo().unwrap(), None);
    }

    #[test]
    fn new_edit_discards_redo() {
        let mut doc = Document::new(base(), History::default());
        doc.apply(insert(1, 0), None).unwrap();
        doc.undo().unwrap();
        doc.apply(insert(2, 50), None).unwrap();
        assert!(!doc.history().can_redo());
        assert_eq!(doc.history().undo_len(), 1);
    }

    #[test]
    fn failed_op_leaves_history_alone() {
        let mut doc = Document::new(base(), History::default());
        doc.apply(insert(1, 0), None).unwrap();
        assert!(doc.apply(insert(2, 5), None).is_err());
        assert_eq!(doc.history().undo_len(), 1);
        assert_eq!(count(&doc), 1);
    }

    #[test]
    fn a_group_is_one_step() {
        let mut doc = Document::new(base(), History::default());
        doc.apply(insert(1, 0), None).unwrap();
        for i in 0..5 {
            doc.apply(insert(10 + i, 100 + i as i64 * 10), Some(7)).unwrap();
        }
        doc.apply(insert(2, 500), Some(8)).unwrap();
        doc.undo().unwrap();
        assert_eq!(count(&doc), 6);
        doc.undo().unwrap();
        assert_eq!(count(&doc), 1);
        doc.redo().unwrap();
        assert_eq!(count(&doc), 6);
        doc.undo().unwrap();
        doc.undo().unwrap();
        assert_eq!(count(&doc), 0);
    }

    #[test]
    fn record_json_shape() {
        let rec = Record::Do {
            op: Op::SetProjectName { name: "a".into() },
            inv: Op::SetProjectName { name: "b".into() },
            group: None,
        };
        assert_eq!(
            serde_json::to_string(&rec).unwrap(),
            r#"{"do":{"op":{"set_project_name":{"name":"a"}},"inv":{"set_project_name":{"name":"b"}}}}"#
        );
        assert_eq!(serde_json::to_string(&Record::Undo).unwrap(), r#""undo""#);
        assert_eq!(serde_json::from_str::<Record>(r#""redo""#).unwrap(), Record::Redo);
    }

    proptest! {
        /// Replaying the records of a session reproduces the project and the
        /// history; undoing everything returns to the start.
        #[test]
        fn replay_reproduces_the_session(steps in prop::collection::vec((0u8..10, any_op(), prop::option::of(0u64..3)), 1..150)) {
            let start = base();
            let mut doc = Document::new(start.clone(), History::default());
            let mut records = Vec::new();
            for (what, seed, group) in steps {
                let record = match what {
                    0 | 1 => doc.undo().unwrap(),
                    2 => doc.redo().unwrap(),
                    _ => doc.apply(make_op(doc.project(), seed), group).ok(),
                };
                records.extend(record);
            }

            let mut replayed = Document::new(start.clone(), History::default());
            let mut noted = History::default();
            for r in &records {
                replayed.replay(r.clone()).unwrap();
                noted.note(r.clone()).unwrap();
            }
            prop_assert_eq!(&replayed, &doc);
            prop_assert_eq!(&noted, doc.history());

            while doc.undo().unwrap().is_some() {}
            prop_assert_eq!(doc.project(), &start);
        }
    }
}
