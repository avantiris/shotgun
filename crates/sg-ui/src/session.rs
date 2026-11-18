//! An open project as the UI thread holds it: the document plus the handle
//! of its writer thread. Edits are applied here in memory (well under a
//! millisecond) and handed to the writer without waiting.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use sg_core::time::SAMPLE_RATE;
use sg_core::{Asset, Clip, Document, IdGen, Op, OpError, Project, TrackKind};
use sg_project::{Opened, ProjectWriter, WriterStatus};

pub struct Session {
    pub root: PathBuf,
    doc: Document,
    writer: ProjectWriter,
    ids: IdGen,
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
            ids: IdGen::new(opened.seed),
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

impl Session {
    /// Puts the whole asset on the first video and audio track, at the
    /// playhead if there is room for it there and otherwise at the end:
    /// its picture on the video track, its sound on the audio track, both
    /// starting at the same time. Returns the frame it starts at.
    pub fn place_asset(&mut self, asset: &Asset, playhead: i64) -> Result<Option<i64>, OpError> {
        let project = self.project().clone();
        let Some(seq) = project.sequences.first() else { return Ok(None) };
        let rate = seq.rate;
        let video_track = seq.tracks.iter().find(|t| t.kind == TrackKind::Video).filter(|_| asset.video.is_some());
        let audio_track = seq.tracks.iter().find(|t| t.kind == TrackKind::Audio).filter(|_| asset.audio.is_some());
        // After everything that is already on the tracks this asset uses.
        let video_end = video_track.map_or(0, |t| t.clips.end());
        let audio_end = audio_track.map_or(0, |t| {
            let samples = t.clips.end();
            let frames = rate.samples_to_frames(samples, SAMPLE_RATE);
            if rate.frames_to_samples(frames, SAMPLE_RATE) < samples { frames + 1 } else { frames }
        });
        let at_end = video_end.max(audio_end);
        // A still gets five seconds.
        let still_frames = (5 * rate.num() as i64 / rate.den() as i64).max(1);
        let frames = asset.frames_at(rate, still_frames).max(1);
        // Room at the playhead on every track this asset needs?
        let free_at = |start: i64| {
            let video_free = video_track.is_none_or(|t| t.clips.range(start, start + frames).next().is_none());
            let (a0, a1) =
                (rate.frames_to_samples(start, SAMPLE_RATE), rate.frames_to_samples(start + frames, SAMPLE_RATE));
            video_free && audio_track.is_none_or(|t| t.clips.range(a0, a1).next().is_none())
        };
        // Bounded, so that the arithmetic below cannot overflow.
        let playhead = playhead.clamp(0, 1 << 40);
        let start = if free_at(playhead) { playhead } else { at_end };
        let mut ops = Vec::new();
        if let Some(track) = video_track {
            let clip = Clip { id: self.ids.clip(), asset: asset.id, start, len: frames, src_in: 0 };
            ops.push(Op::InsertClip { seq: seq.id, track: track.id, clip });
        }
        if let (Some(track), Some(audio)) = (audio_track, &asset.audio) {
            // The conformed audio is at the engine rate whatever the source was.
            let mut len = (audio.samples as i128 * SAMPLE_RATE as i128 / audio.sample_rate.max(1) as i128) as i64;
            if video_track.is_some() {
                len = len.min(rate.frames_to_samples(frames, SAMPLE_RATE));
            }
            let clip = Clip {
                id: self.ids.clip(),
                asset: asset.id,
                start: rate.frames_to_samples(start, SAMPLE_RATE),
                len: len.max(1),
                src_in: 0,
            };
            ops.push(Op::InsertClip { seq: seq.id, track: track.id, clip });
        }
        if ops.is_empty() {
            return Ok(None);
        }
        self.apply(Op::Batch(ops), None)?;
        Ok(Some(start))
    }
}

impl Session {
    pub fn new_clip_id(&mut self) -> sg_core::ClipId {
        self.ids.clip()
    }
}

impl Session {
    /// Names the current state (saved, committed and tagged by the writer).
    pub fn checkpoint(&self, name: String) {
        self.writer.checkpoint(name);
    }
}
