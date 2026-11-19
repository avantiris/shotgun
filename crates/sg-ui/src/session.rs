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
            // Not a camera-angle track and not a muted one (a camera's guide
            // sound): what is placed should be seen and heard.
            let open = |t: &&std::sync::Arc<sg_core::Track>| match kind {
                TrackKind::Video => !t.mix.hidden,
                TrackKind::Audio => !t.mix.mute,
            };
            let mut tracks: Vec<&std::sync::Arc<sg_core::Track>> =
                seq.tracks.iter().filter(|t| t.kind == kind).filter(open).collect();
            if tracks.is_empty() {
                tracks = seq.tracks.iter().filter(|t| t.kind == kind).collect();
            }
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
        // Camera-angle tracks are only used when pointed at.
        let mut rows: Vec<usize> = (0..sequence.tracks.len())
            .filter(|i| {
                let mix = &sequence.tracks[*i].mix;
                // Nor muted sound tracks (a camera's guide sound).
                usable(*i) && !mix.hidden && !(kind == TrackKind::Audio && mix.mute)
            })
            .collect();
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
    /// A move that covers other clips: `removes` takes the moving clips
    /// out, then each of `clips` is put down over what is in its place.
    /// One undo step.
    pub fn move_over(&mut self, removes: Vec<Op>, clips: Vec<(sg_core::TrackId, Clip)>) -> Result<(), OpError> {
        self.move_over_in(removes, clips, None)
    }

    /// The same as part of an undo group (a paste that also sets properties).
    pub fn move_over_in(
        &mut self,
        removes: Vec<Op>,
        clips: Vec<(sg_core::TrackId, Clip)>,
        group: Option<u64>,
    ) -> Result<(), OpError> {
        // Each step is planned on the state the one before leaves behind.
        let (mut project, _) = sg_core::apply(self.project(), &Op::Batch(removes.clone()))?;
        let mut ops = removes;
        for (track, clip) in clips {
            let Some(sequence) = project.sequences.first() else { break };
            let Some(op) = Op::overwrite(sequence, track, clip, self.ids.clip()) else { continue };
            project = sg_core::apply(&project, &op)?.0;
            ops.push(op);
        }
        self.apply(Op::Batch(ops), group)
    }

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
        let placement = place_at(sequence, asset, frame, row, only);
        self.apply_placement(asset, placement, None)
    }

    /// Carries out a placement: makes the tracks it asks for and inserts
    /// the clips, as one operation.
    fn apply_placement(
        &mut self,
        asset: &Asset,
        placement: Placement,
        group: Option<u64>,
    ) -> Result<Option<i64>, OpError> {
        let project = self.project().clone();
        let Some(sequence) = project.sequences.first() else { return Ok(None) };
        let rate = sequence.rate;
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
        self.apply(Op::Batch(ops), group)?;
        Ok(Some(placement.start))
    }
}

/// Rows of the camera-angle tracks (hidden video tracks), top to bottom.
/// Angle `n` of the switching keys is the n-th of them.
pub fn angle_rows(sequence: &sg_core::Sequence) -> Vec<usize> {
    (0..sequence.tracks.len())
        .filter(|row| sequence.tracks[*row].kind == TrackKind::Video && sequence.tracks[*row].mix.hidden)
        .collect()
}

/// Row of the programme track: the visible video track nearest above the
/// angle tracks (the lowest visible video track if there are none).
pub fn program_row(sequence: &sg_core::Sequence) -> Option<usize> {
    let limit = angle_rows(sequence).first().copied().unwrap_or(sequence.tracks.len());
    (0..limit).rev().find(|row| sequence.tracks[*row].kind == TrackKind::Video && !sequence.tracks[*row].mix.hidden)
}

/// The clip of the song on the timeline: the first clip of the song's asset
/// on an audio track.
pub fn song_clip(sequence: &sg_core::Sequence) -> Option<Clip> {
    let song = sequence.song?;
    sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .find_map(|t| t.clips.iter().find(|c| c.asset == song.asset))
}

/// Why a synchronised take could not be laid on the timeline.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("no song is set")]
    NoSong,
    #[error("the song is not on the timeline: put it on an audio track first")]
    SongNotPlaced,
    #[error("the take has no picture")]
    NoPicture,
    #[error("the take lies outside the song")]
    Outside,
    #[error("{0}")]
    Op(#[from] OpError),
}

impl Session {
    /// Makes `asset` the song of the sequence and puts its sound on an audio
    /// track at the start if it is not on the timeline yet. One undo step.
    pub fn set_song(&mut self, asset: &Asset) -> Result<(), OpError> {
        let project = self.project().clone();
        let Some(sequence) = project.sequences.first() else { return Ok(()) };
        let group = Some(self.new_group());
        let song = sg_core::Song { asset: asset.id, tempo: 0 };
        self.apply(Op::SetSong { id: sequence.id, song: Some(song) }, group)?;
        let placed =
            sequence.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.asset == asset.id));
        if !placed {
            let placement = place_at(sequence, asset, 0, None, Some(TrackKind::Audio));
            self.apply_placement(asset, placement, group)?;
        }
        Ok(())
    }

    /// Lays a take whose sound was found `lag` samples into the song: on a
    /// new hidden camera track, in time with the song's clip, and on the
    /// programme track too where that is still empty. Returns the frame the
    /// take starts at. One undo step.
    pub fn place_synced(&mut self, asset: &Asset, lag: i64) -> Result<i64, SyncError> {
        let project = self.project().clone();
        let sequence = project.sequences.first().ok_or(SyncError::NoSong)?;
        sequence.song.ok_or(SyncError::NoSong)?;
        let song = song_clip(sequence).ok_or(SyncError::SongNotPlaced)?;
        if asset.video.is_none() {
            return Err(SyncError::NoPicture);
        }
        let rate = sequence.rate;
        // Timeline sample of the take's first sample, then the nearest frame.
        let at = song.start + lag - song.src_in;
        let per_frame = rate.den() as i128 * SAMPLE_RATE as i128;
        let first = ((at as i128 * rate.num() as i128 * 2 + per_frame).div_euclid(per_frame * 2)) as i64;
        let total = asset.frames_at(rate, 1).max(1);
        // A take that starts before the timeline does loses its beginning.
        let (start, src_in) = (first.max(0), (-first).max(0));
        let len = total - src_in;
        if len <= 0 {
            return Err(SyncError::Outside);
        }
        let mut ops = Vec::new();
        let angles = angle_rows(sequence);
        let mut program = program_row(sequence).map(|row| sequence.tracks[row].clone());
        // Rows shift by one if a programme track has to be made on top.
        let mut shift = 0;
        let program_id = match &program {
            Some(track) => track.id,
            None => {
                let id = self.ids.track();
                let header = sg_core::TrackHeader { id, kind: TrackKind::Video, name: "Program".to_owned() };
                ops.push(Op::AddTrack { seq: sequence.id, index: 0, track: header });
                shift = 1;
                id
            }
        };
        // Below the angles there are; below every video track if this is the first.
        let below = angles
            .last()
            .copied()
            .or_else(|| (0..sequence.tracks.len()).rev().find(|r| sequence.tracks[*r].kind == TrackKind::Video));
        let index = below.map_or(0, |row| row + 1) + shift;
        let camera = self.ids.track();
        let header =
            sg_core::TrackHeader { id: camera, kind: TrackKind::Video, name: format!("Cam {}", angles.len() + 1) };
        ops.push(Op::AddTrack { seq: sequence.id, index, track: header });
        let hidden = sg_core::TrackSettings { hidden: true, ..Default::default() };
        ops.push(Op::SetTrackMix { seq: sequence.id, track: camera, mix: hidden });
        let clip = Clip { id: self.ids.clip(), asset: asset.id, start, len, src_in };
        ops.push(Op::InsertClip { seq: sequence.id, track: camera, clip });
        let free = program.take().is_none_or(|track| track.clips.range(start, start + len).next().is_none());
        if free {
            let clip = Clip { id: self.ids.clip(), ..clip };
            ops.push(Op::InsertClip { seq: sequence.id, track: program_id, clip });
        }
        self.apply(Op::Batch(ops), None)?;
        Ok(start)
    }

    /// Lays a take at `frame` as a camera angle to be put in time by hand:
    /// its picture on a new hidden camera track and, if it has sound, that
    /// on a new muted audio track, so that its waveform can be lined up
    /// with the song's (picture and sound move together). Nothing goes on
    /// the programme track: the angle is cut to once it sits right.
    /// One undo step.
    pub fn place_angle_by_hand(&mut self, asset: &Asset, frame: i64) -> Result<(), SyncError> {
        let project = self.project().clone();
        let sequence = project.sequences.first().ok_or(SyncError::NoSong)?;
        if asset.video.is_none() {
            return Err(SyncError::NoPicture);
        }
        let rate = sequence.rate;
        let start = frame.max(0);
        let len = asset.frames_at(rate, 1).max(1);
        let mut ops = Vec::new();
        let angles = angle_rows(sequence);
        // A programme track on top, if there is none yet, for the cuts to come.
        let mut shift = 0;
        if program_row(sequence).is_none() {
            let header =
                sg_core::TrackHeader { id: self.ids.track(), kind: TrackKind::Video, name: "Program".to_owned() };
            ops.push(Op::AddTrack { seq: sequence.id, index: 0, track: header });
            shift = 1;
        }
        let below = angles
            .last()
            .copied()
            .or_else(|| (0..sequence.tracks.len()).rev().find(|r| sequence.tracks[*r].kind == TrackKind::Video));
        let index = below.map_or(0, |row| row + 1) + shift;
        let name = format!("Cam {}", angles.len() + 1);
        let camera = self.ids.track();
        ops.push(Op::AddTrack {
            seq: sequence.id,
            index,
            track: sg_core::TrackHeader { id: camera, kind: TrackKind::Video, name: name.clone() },
        });
        let hidden = sg_core::TrackSettings { hidden: true, ..Default::default() };
        ops.push(Op::SetTrackMix { seq: sequence.id, track: camera, mix: hidden });
        let picture = Clip { id: self.ids.clip(), asset: asset.id, start, len, src_in: 0 };
        ops.push(Op::InsertClip { seq: sequence.id, track: camera, clip: picture });
        if asset.audio.is_some() {
            let sound = self.ids.track();
            // At the bottom: two tracks were added above at most.
            let bottom = sequence.tracks.len() + shift + 1;
            ops.push(Op::AddTrack {
                seq: sequence.id,
                index: bottom,
                track: sg_core::TrackHeader { id: sound, kind: TrackKind::Audio, name: format!("{name} sound") },
            });
            let muted = sg_core::TrackSettings { mute: true, ..Default::default() };
            ops.push(Op::SetTrackMix { seq: sequence.id, track: sound, mix: muted });
            let samples = |frames: i64| rate.frames_to_samples(frames, SAMPLE_RATE);
            let clip =
                Clip { id: self.ids.clip(), asset: asset.id, start: samples(start), len: samples(len), src_in: 0 };
            ops.push(Op::InsertClip { seq: sequence.id, track: sound, clip });
        }
        self.apply(Op::Batch(ops), None)?;
        Ok(())
    }

    /// Cuts the programme to camera angle `angle` (0 is the first) from
    /// `frame` to the end of that angle's take. `Ok(false)` if there is no
    /// such angle or it has no picture at this time. Switches made with
    /// the same `group` undo together.
    pub fn switch_angle(&mut self, angle: usize, frame: i64, group: Option<u64>) -> Result<bool, OpError> {
        let project = self.project().clone();
        let Some(sequence) = project.sequences.first() else { return Ok(false) };
        let Some(source) = angle_rows(sequence).get(angle).and_then(|row| sequence.tracks[*row].clips.at(frame)) else {
            return Ok(false);
        };
        let clip = Clip {
            id: self.ids.clip(),
            asset: source.asset,
            start: frame,
            len: source.end() - frame,
            src_in: source.src_in + (frame - source.start),
        };
        let mut ops = Vec::new();
        match program_row(sequence).map(|row| &sequence.tracks[row]) {
            Some(program) => {
                // Already showing this angle from here on: nothing to cut.
                let showing = program.clips.at(frame).is_some_and(|c| {
                    c.asset == clip.asset && c.src_in - c.start == clip.src_in - clip.start && c.end() >= clip.end()
                });
                if showing {
                    return Ok(true);
                }
                ops.extend(Op::overwrite(sequence, program.id, clip, self.ids.clip()));
            }
            None => {
                let id = self.ids.track();
                let header = sg_core::TrackHeader { id, kind: TrackKind::Video, name: "Program".to_owned() };
                ops.push(Op::AddTrack { seq: sequence.id, index: 0, track: header });
                ops.push(Op::InsertClip { seq: sequence.id, track: id, clip });
            }
        }
        self.apply(Op::Batch(ops), group)?;
        Ok(true)
    }
}

/// Which scenario an asset should belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioChoice {
    None,
    Existing(sg_core::ScenarioId),
    /// A new scenario made for it.
    New,
}

impl Session {
    /// Moves an asset into a scenario (an asset is in one at most), making
    /// the scenario if asked. Returns the scenario it is in now and the
    /// undo group of the change, so that its measurement can join it.
    pub fn assign_scenario(
        &mut self,
        asset: sg_core::AssetId,
        choice: ScenarioChoice,
    ) -> Result<Option<(sg_core::ScenarioId, u64)>, OpError> {
        use sg_core::{Member, Scenario};
        let project = self.project().clone();
        let current = project.scenario_of(asset).cloned();
        let target = match choice {
            ScenarioChoice::None => None,
            ScenarioChoice::Existing(id) => project.scenarios.get(&id).map(|s| Scenario::clone(s)),
            ScenarioChoice::New => {
                let number = project.scenarios.len() + 1;
                Some(Scenario::new(self.ids.scenario(), format!("Scenario {number}"), (number - 1) as u8 % 8))
            }
        };
        if current.as_ref().map(|s| s.id) == target.as_ref().map(|s| s.id) {
            return Ok(None);
        }
        let mut ops = Vec::new();
        if let Some(old) = current {
            let mut old = Scenario::clone(&old);
            old.members.remove(&asset);
            if old.hero == Some(asset) {
                old.hero = None;
            }
            ops.push(Op::SetScenario { id: old.id, scenario: Some(Box::new(old)) });
        }
        let placed = target.map(|mut new| {
            new.members.insert(asset, Member::default());
            let id = new.id;
            ops.push(Op::SetScenario { id, scenario: Some(Box::new(new)) });
            id
        });
        let group = self.new_group();
        self.apply(Op::Batch(ops), Some(group))?;
        Ok(placed.map(|id| (id, group)))
    }
}
