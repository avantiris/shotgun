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
    pub fn place_asset(
        &mut self,
        asset: &Asset,
        playhead: i64,
        only: Option<TrackKind>,
    ) -> Result<Option<i64>, OpError> {
        let project = self.project().clone();
        let Some(seq) = project.sequences.first() else { return Ok(None) };
        let rate = seq.rate;
        // Bounded, so that the arithmetic below cannot overflow.
        let playhead = playhead.clamp(0, 1 << 40);
        let still = (5 * rate.num() as i64 / rate.den() as i64).max(1);
        let span = asset.frames_at(rate, still).max(1);
        let free = |t: &&std::sync::Arc<sg_core::Track>| match t.kind {
            TrackKind::Video => t.clips.range(playhead, playhead + span).next().is_none(),
            TrackKind::Audio => {
                let (a0, a1) = (
                    rate.frames_to_samples(playhead, SAMPLE_RATE),
                    rate.frames_to_samples(playhead + span, SAMPLE_RATE),
                );
                t.clips.range(a0, a1).next().is_none()
            }
        };
        // The lowest video track and the highest audio track with room at
        // the playhead, so that a second asset placed at the same spot
        // stacks on the next track; the first track of each kind otherwise.
        let pick = |kind: TrackKind, from_bottom: bool| {
            let mut tracks: Vec<&std::sync::Arc<sg_core::Track>> =
                seq.tracks.iter().filter(|t| t.kind == kind).collect();
            if from_bottom {
                tracks.reverse();
            }
            tracks.iter().find(|t| free(t)).or(tracks.first()).copied()
        };
        let video_track =
            pick(TrackKind::Video, true).filter(|_| asset.video.is_some() && only != Some(TrackKind::Audio));
        let audio_track =
            pick(TrackKind::Audio, false).filter(|_| asset.audio.is_some() && only != Some(TrackKind::Video));
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

impl Session {
    /// A fresh undo-group id for a gesture.
    pub fn new_group(&mut self) -> u64 {
        self.ids.next_u64()
    }
}

impl Session {
    pub fn new_track_id(&mut self) -> sg_core::TrackId {
        self.ids.track()
    }
}

impl Session {
    /// Saves and commits the current state to the project's git history.
    pub fn commit(&self) {
        self.writer.commit();
    }
}

/// The track a dropped asset's picture or sound goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// Row of an existing track (tracks are stored top first).
    Row(usize),
    /// A track that the drop creates: video on top, audio at the bottom.
    New,
}

/// Where an asset dropped on the timeline lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// First sequence frame and length in frames.
    pub start: i64,
    pub frames: i64,
    pub video: Option<Slot>,
    pub audio: Option<Slot>,
}

/// Decides where an asset dropped at `frame` over `row` (or outside every
/// track) goes. The track under the pointer is used for the part of the
/// asset it can hold if it is free there; otherwise the nearest free track
/// of the right kind; if there is none, a new track. A drop therefore never
/// fails for lack of room and nobody has to make tracks by hand first.
/// `only` limits the drop to the picture or the sound of the asset.
pub fn place_at(
    sequence: &sg_core::Sequence,
    asset: &Asset,
    frame: i64,
    row: Option<usize>,
    only: Option<TrackKind>,
) -> Placement {
    let rate = sequence.rate;
    let start = frame.clamp(0, 1 << 40);
    // A still gets five seconds.
    let still = (5 * rate.num() as i64 / rate.den() as i64).max(1);
    let frames = asset.frames_at(rate, still).max(1);
    let free = |track: &sg_core::Track| match track.kind {
        TrackKind::Video => track.clips.range(start, start + frames).next().is_none(),
        TrackKind::Audio => {
            let (a0, a1) =
                (rate.frames_to_samples(start, SAMPLE_RATE), rate.frames_to_samples(start + frames, SAMPLE_RATE));
            track.clips.range(a0, a1).next().is_none()
        }
    };
    let slot = |kind: TrackKind| {
        let usable = |index: usize| sequence.tracks.get(index).is_some_and(|t| t.kind == kind && free(t));
        if let Some(index) = row.filter(|r| usable(*r)) {
            return Slot::Row(index);
        }
        // Video stacks upwards from the bottom video track, sound downwards
        // from the top audio track: both start next to the other kind.
        let mut rows: Vec<usize> = (0..sequence.tracks.len()).filter(|i| usable(*i)).collect();
        if kind == TrackKind::Video {
            rows.reverse();
        }
        rows.first().map_or(Slot::New, |index| Slot::Row(*index))
    };
    Placement {
        start,
        frames,
        video: (asset.video.is_some() && only != Some(TrackKind::Audio)).then(|| slot(TrackKind::Video)),
        audio: (asset.audio.is_some() && only != Some(TrackKind::Video)).then(|| slot(TrackKind::Audio)),
    }
}

impl Session {
    /// Adds a track: video on top, audio at the bottom.
    pub fn add_track(&mut self, kind: TrackKind) -> Result<(), OpError> {
        let project = self.project().clone();
        let Some(sequence) = project.sequences.first() else { return Ok(()) };
        let op = self.add_track_op(sequence, kind, sequence.tracks.len());
        self.apply(op.0, None)
    }

    /// The op adding a track of `kind` to a sequence that has `len` tracks
    /// by then, and the new track's id.
    fn add_track_op(&mut self, sequence: &sg_core::Sequence, kind: TrackKind, len: usize) -> (Op, sg_core::TrackId) {
        let count = sequence.tracks.iter().filter(|t| t.kind == kind).count();
        let (index, name) = match kind {
            TrackKind::Video => (0, format!("V{}", count + 1)),
            TrackKind::Audio => (len, format!("A{}", count + 1)),
        };
        let id = self.ids.track();
        (Op::AddTrack { seq: sequence.id, index, track: sg_core::TrackHeader { id, kind, name } }, id)
    }

    /// Puts an asset where [`place_at`] says, creating the tracks it needs,
    /// as one undo step. Returns the frame it starts at.
    pub fn drop_asset(
        &mut self,
        asset: &Asset,
        frame: i64,
        row: Option<usize>,
        only: Option<TrackKind>,
    ) -> Result<Option<i64>, OpError> {
        let project = self.project().clone();
        let Some(sequence) = project.sequences.first() else { return Ok(None) };
        let rate = sequence.rate;
        let placement = place_at(sequence, asset, frame, row, only);
        let mut ops = Vec::new();
        let mut len = sequence.tracks.len();
        let mut track_of = |session: &mut Session, slot: Slot, kind: TrackKind, ops: &mut Vec<Op>| match slot {
            Slot::Row(index) => sequence.tracks.get(index).map(|t| t.id),
            Slot::New => {
                let (op, id) = session.add_track_op(sequence, kind, len);
                len += 1;
                ops.push(op);
                Some(id)
            }
        };
        let video = placement.video.and_then(|slot| track_of(self, slot, TrackKind::Video, &mut ops));
        let audio = placement.audio.and_then(|slot| track_of(self, slot, TrackKind::Audio, &mut ops));
        if let Some(track) = video {
            let clip =
                Clip { id: self.ids.clip(), asset: asset.id, start: placement.start, len: placement.frames, src_in: 0 };
            ops.push(Op::InsertClip { seq: sequence.id, track, clip });
        }
        if let (Some(track), Some(info)) = (audio, &asset.audio) {
            // The conformed audio is at the engine rate whatever the source was.
            let mut samples = (info.samples as i128 * SAMPLE_RATE as i128 / info.sample_rate.max(1) as i128) as i64;
            if video.is_some() {
                samples = samples.min(rate.frames_to_samples(placement.frames, SAMPLE_RATE));
            }
            let clip = Clip {
                id: self.ids.clip(),
                asset: asset.id,
                start: rate.frames_to_samples(placement.start, SAMPLE_RATE),
                len: samples.max(1),
                src_in: 0,
            };
            ops.push(Op::InsertClip { seq: sequence.id, track, clip });
        }
        if ops.is_empty() {
            return Ok(None);
        }
        self.apply(Op::Batch(ops), None)?;
        Ok(Some(placement.start))
    }
}
