//! Edit operations. Every change to a project is an [`Op`]; applying one
//! yields the new snapshot and the op that undoes it. Ops are plain data so
//! they can be journalled and replayed.
//!
//! Clips are addressed by track and start position, which is unique on a
//! track and found in O(log n).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::asset::Asset;
use crate::clips::{Clip, ClipError, ClipStore};
use crate::colour::Scenario;
use crate::ids::{AssetId, ClipId, ScenarioId, SequenceId, TrackId};
use crate::model::{Marker, Project, Sequence, SequenceHeader, Song, Track, TrackHeader, TrackSettings};
use crate::props::ClipProps;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    SetProjectName {
        name: String,
    },
    /// Adds an empty sequence at `index`.
    AddSequence {
        index: usize,
        sequence: SequenceHeader,
    },
    /// Removes a sequence, which must have no tracks left.
    RemoveSequence {
        id: SequenceId,
    },
    SetSequenceName {
        id: SequenceId,
        name: String,
    },
    /// Adds an empty track at `index` (0 is the top of the timeline).
    AddTrack {
        seq: SequenceId,
        index: usize,
        track: TrackHeader,
    },
    /// Removes a track, which must have no clips left.
    RemoveTrack {
        seq: SequenceId,
        id: TrackId,
    },
    InsertClip {
        seq: SequenceId,
        track: TrackId,
        clip: Clip,
    },
    RemoveClip {
        seq: SequenceId,
        track: TrackId,
        start: i64,
    },
    /// Replaces the clip starting at `start` with `clip`: a move along the
    /// track, a trim, or both.
    SetClip {
        seq: SequenceId,
        track: TrackId,
        start: i64,
        clip: Clip,
    },
    /// Moves every clip on the track that starts at or after `from` by
    /// `delta` (ripple).
    ShiftFrom {
        seq: SequenceId,
        track: TrackId,
        from: i64,
        delta: i64,
    },
    /// Sets the mixer settings of a track.
    SetTrackMix {
        seq: SequenceId,
        track: TrackId,
        mix: TrackSettings,
    },
    /// Creates, replaces or (with `None`) removes a scenario as a whole.
    SetScenario {
        id: ScenarioId,
        scenario: Option<Box<Scenario>>,
    },
    /// Sets or clears the song a sequence is cut to.
    SetSong {
        id: SequenceId,
        song: Option<Song>,
    },
    /// Sets the master fader of a sequence, in decibels.
    SetMasterGain {
        id: SequenceId,
        db: f32,
    },
    /// Makes this the one plugin of a track, or takes all off. From before
    /// a track could have several; still understood in journals.
    SetTrackPlugin {
        seq: SequenceId,
        track: TrackId,
        plugin: Option<crate::model::PluginRef>,
    },
    /// Sets the plugins of a track as a whole: adding, removing and
    /// reordering are all this.
    SetTrackPlugins {
        seq: SequenceId,
        track: TrackId,
        plugins: Vec<crate::model::PluginRef>,
    },
    /// Sets the notes of a note clip as a whole (`None`: none recorded).
    SetClipNotes {
        seq: SequenceId,
        track: TrackId,
        clip: ClipId,
        notes: Option<Vec<crate::model::Note>>,
    },
    /// Sets how the shared room and echo of a sequence sound.
    SetSpace {
        id: SequenceId,
        space: crate::model::Space,
    },
    /// Gives a sequence a tempo (bars and beats), changes it or takes it away.
    SetTempo {
        id: SequenceId,
        tempo: Option<crate::model::Tempo>,
    },
    /// Changes the frame size of a sequence.
    SetSequenceSize {
        id: SequenceId,
        width: u32,
        height: u32,
    },
    /// Sets or clears (`None`) the properties of a clip.
    SetClipProps {
        seq: SequenceId,
        track: TrackId,
        clip: ClipId,
        props: Option<Box<ClipProps>>,
    },
    /// Adds a marker; a frame holds at most one.
    AddMarker {
        seq: SequenceId,
        marker: Marker,
    },
    RemoveMarker {
        seq: SequenceId,
        frame: i64,
    },
    /// Registers an imported asset. The media file is already in the bundle.
    AddAsset {
        asset: Box<Asset>,
    },
    /// Removes an asset that no clip uses any more. Its file moves to the
    /// bundle's trash when the project is next saved.
    RemoveAsset {
        id: AssetId,
    },
    SetAssetLabel {
        id: AssetId,
        label: String,
    },
    /// Several ops as one atomic change. Fails as a whole.
    Batch(Vec<Op>),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum OpError {
    #[error("sequence {0} does not exist")]
    NoSequence(SequenceId),
    #[error("track {0} does not exist")]
    NoTrack(TrackId),
    #[error("an item with id {0} already exists")]
    DuplicateId(String),
    #[error("index {index} is out of range (length {len})")]
    BadIndex { index: usize, len: usize },
    #[error("cannot remove {0}: it is not empty")]
    NotEmpty(&'static str),
    #[error("a frame of {0}x{1} is not possible")]
    BadSize(u32, u32),
    #[error("there is no marker at frame {0}")]
    NoMarker(i64),
    #[error("asset {0} does not exist")]
    NoAsset(AssetId),
    #[error("asset {0} is still used by a clip")]
    AssetInUse(AssetId),
    #[error(transparent)]
    Clip(#[from] ClipError),
}

impl Op {
    /// A few words for commit messages and the history list.
    pub fn label(&self) -> &'static str {
        match self {
            Op::SetProjectName { .. } => "rename project",
            Op::AddSequence { .. } => "add sequence",
            Op::RemoveSequence { .. } => "remove sequence",
            Op::SetSequenceName { .. } => "rename sequence",
            Op::AddTrack { .. } => "add track",
            Op::RemoveTrack { .. } => "remove track",
            Op::InsertClip { .. } => "add clip",
            Op::RemoveClip { .. } => "remove clip",
            Op::SetClip { .. } => "trim or move clip",
            Op::ShiftFrom { .. } => "ripple",
            Op::SetTrackMix { .. } => "adjust track",
            Op::SetScenario { .. } => "adjust colour",
            Op::SetSong { .. } => "set song",
            Op::SetMasterGain { .. } => "adjust master",
            Op::SetTempo { .. } => "set tempo",
            Op::SetSpace { .. } => "set room and echo",
            Op::SetTrackPlugin { .. } | Op::SetTrackPlugins { .. } => "change plugins",
            Op::SetClipNotes { .. } => "edit notes",
            Op::SetSequenceSize { .. } => "change frame size",
            Op::SetClipProps { .. } => "adjust clip",
            Op::AddMarker { .. } => "add marker",
            Op::RemoveMarker { .. } => "remove marker",
            Op::AddAsset { .. } => "import",
            Op::RemoveAsset { .. } => "remove asset",
            Op::SetAssetLabel { .. } => "rename asset",
            // A batch is named after what it mostly is.
            Op::Batch(ops) => match ops.as_slice() {
                [] => "edit",
                [Op::SetClip { .. }, Op::InsertClip { .. }, ..] => "split",
                [Op::RemoveClip { .. }, Op::ShiftFrom { .. }, ..] => "ripple delete",
                [Op::SetClip { .. }, Op::ShiftFrom { .. }, ..] | [Op::ShiftFrom { .. }, Op::SetClip { .. }, ..] => {
                    "ripple trim"
                }
                [Op::RemoveClip { .. }, .., Op::InsertClip { .. }] => "move clip",
                [first, ..] => first.label(),
            },
        }
    }

    /// Cuts `clip` in two at `at`. The left part keeps the clip's id, the
    /// right part gets `right_id`. `None` if `at` is not strictly inside.
    pub fn split(seq: SequenceId, track: TrackId, clip: &Clip, at: i64, right_id: ClipId) -> Option<Op> {
        if at <= clip.start || at >= clip.end() {
            return None;
        }
        let left = Clip { len: at - clip.start, ..*clip };
        let right = Clip { id: right_id, start: at, len: clip.end() - at, src_in: clip.src_in + left.len, ..*clip };
        Some(Op::Batch(vec![
            Op::SetClip { seq, track, start: clip.start, clip: left },
            Op::InsertClip { seq, track, clip: right },
        ]))
    }

    /// Puts `clip` on a track and clears what was in its place: clips
    /// inside its span go, clips reaching into it are shortened, a clip
    /// that surrounds it is cut in two (its right part gets `spare`).
    /// `None` if the track does not exist.
    pub fn overwrite(sequence: &Sequence, track: TrackId, clip: Clip, spare: ClipId) -> Option<Op> {
        let (seq, from, to) = (sequence.id, clip.start, clip.end());
        let mut ops = Vec::new();
        let lane = sequence.track(track)?;
        for old in lane.clips.range(from, to) {
            let props = lane.props_of(old.id);
            // How far into `old` its tail begins: in track units, and in
            // sequence frames (the unit of keyframes).
            let cut = to - old.start;
            let cut_frames = match lane.kind {
                crate::model::TrackKind::Video => cut,
                crate::model::TrackKind::Audio => sequence.rate.samples_to_frames(cut, crate::time::SAMPLE_RATE),
            };
            // The source goes on where the covered part would have stopped: speed and ramp counted.
            let consumed = match (props, lane.kind) {
                (Some(p), crate::model::TrackKind::Video) => p.source_offset(cut),
                (Some(p), crate::model::TrackKind::Audio) if p.speed_valid() => {
                    (cut as i128 * p.speed.0 as i128 / p.speed.1 as i128) as i64
                }
                _ => cut,
            };
            let tail = Clip { start: to, len: old.end() - to, src_in: old.src_in + consumed, ..old };
            let head = Clip { len: from - old.start, ..old };
            // The tail keeps its keyframes in place and the fade-out; the
            // head keeps the fade-in and the transition into the clip.
            let tail_props = |id: ClipId| {
                props.map(|p| {
                    let mut p = ClipProps::clone(p);
                    for param in p.picture_params() {
                        param.shift_keys(-cut_frames);
                    }
                    p.gain_db.shift_keys(-cut_frames);
                    (p.fade_in, p.transition) = (0, None);
                    Op::SetClipProps { seq, track, clip: id, props: Some(Box::new(p)) }
                })
            };
            let head_props = || {
                props.filter(|p| p.fade_out != 0).map(|p| {
                    let p = ClipProps { fade_out: 0, ..ClipProps::clone(p) };
                    let props = (p != ClipProps::default()).then(|| Box::new(p));
                    Op::SetClipProps { seq, track, clip: old.id, props }
                })
            };
            match (old.start < from, old.end() > to) {
                (false, false) => ops.push(Op::RemoveClip { seq, track, start: old.start }),
                (true, false) => {
                    ops.push(Op::SetClip { seq, track, start: old.start, clip: head });
                    ops.extend(head_props());
                }
                (false, true) => {
                    ops.push(Op::SetClip { seq, track, start: old.start, clip: tail });
                    ops.extend(tail_props(old.id));
                }
                (true, true) => {
                    ops.push(Op::SetClip { seq, track, start: old.start, clip: head });
                    ops.extend(head_props());
                    ops.push(Op::InsertClip { seq, track, clip: Clip { id: spare, ..tail } });
                    ops.extend(tail_props(spare));
                }
            }
        }
        ops.push(Op::InsertClip { seq, track, clip });
        Some(Op::Batch(ops))
    }

    /// Removes the clip at `start` and closes the gap on its track.
    pub fn ripple_delete(seq: SequenceId, track: TrackId, clip: &Clip) -> Op {
        Op::Batch(vec![
            Op::RemoveClip { seq, track, start: clip.start },
            Op::ShiftFrom { seq, track, from: clip.end(), delta: -clip.len },
        ])
    }
}

/// Applies `op` to `project`. Returns the new snapshot and the inverse op.
/// On error nothing has changed (the input snapshot is immutable).
pub fn apply(project: &Arc<Project>, op: &Op) -> Result<(Arc<Project>, Op), OpError> {
    let (next, inverse) = apply_inner(project, op)?;
    Ok((Arc::new(next), inverse))
}

fn apply_inner(p: &Project, op: &Op) -> Result<(Project, Op), OpError> {
    match op {
        Op::SetProjectName { name } => {
            let inverse = Op::SetProjectName { name: p.name.clone() };
            Ok((Project { name: name.clone(), ..p.clone() }, inverse))
        }
        Op::AddSequence { index, sequence } => {
            if p.sequence(sequence.id).is_some() {
                return Err(OpError::DuplicateId(sequence.id.to_string()));
            }
            check_index(*index, p.sequences.len())?;
            let mut next = p.clone();
            next.sequences.insert(*index, Arc::new(Sequence::from(sequence.clone())));
            Ok((next, Op::RemoveSequence { id: sequence.id }))
        }
        Op::RemoveSequence { id } => {
            let index = sequence_index(p, *id)?;
            let seq = &p.sequences[index];
            if !seq.tracks.is_empty() {
                return Err(OpError::NotEmpty("sequence"));
            }
            let add = Op::AddSequence { index, sequence: seq.header() };
            // The header carries neither master fader nor song: put them back separately.
            let mut restore = vec![add];
            if seq.master_db != 0.0 {
                restore.push(Op::SetMasterGain { id: *id, db: seq.master_db });
            }
            if seq.tempo.is_some() {
                restore.push(Op::SetTempo { id: *id, tempo: seq.tempo });
            }
            if seq.space != crate::model::Space::default() {
                restore.push(Op::SetSpace { id: *id, space: seq.space });
            }
            if seq.song.is_some() {
                restore.push(Op::SetSong { id: *id, song: seq.song });
            }
            let inverse = if restore.len() == 1 { restore.remove(0) } else { Op::Batch(restore) };
            let mut next = p.clone();
            next.sequences.remove(index);
            Ok((next, inverse))
        }
        Op::SetSequenceName { id, name } => with_sequence(p, *id, |s| {
            let old = std::mem::replace(&mut s.name, name.clone());
            Ok(Op::SetSequenceName { id: *id, name: old })
        }),
        Op::AddTrack { seq, index, track } => with_sequence(p, *seq, |s| {
            if s.track(track.id).is_some() {
                return Err(OpError::DuplicateId(track.id.to_string()));
            }
            check_index(*index, s.tracks.len())?;
            s.tracks.insert(*index, Arc::new(Track::from(track.clone())));
            Ok(Op::RemoveTrack { seq: *seq, id: track.id })
        }),
        Op::RemoveTrack { seq, id } => with_sequence(p, *seq, |s| {
            let index = s.tracks.iter().position(|t| t.id == *id).ok_or(OpError::NoTrack(*id))?;
            if !s.tracks[index].clips.is_empty() {
                return Err(OpError::NotEmpty("track"));
            }
            let track = s.tracks.remove(index);
            let add = Op::AddTrack { seq: *seq, index, track: track.header() };
            // A track's header carries neither mixer settings nor plugin: put them back separately.
            let mut restore = vec![add];
            if !track.mix.is_default() {
                restore.push(Op::SetTrackMix { seq: *seq, track: track.id, mix: track.mix });
            }
            if !track.plugins.is_empty() {
                restore.push(Op::SetTrackPlugins { seq: *seq, track: track.id, plugins: Vec::clone(&track.plugins) });
            }
            Ok(if restore.len() == 1 { restore.remove(0) } else { Op::Batch(restore) })
        }),
        Op::InsertClip { seq, track, clip } => with_clips(p, *seq, *track, |clips| {
            let inverse = Op::RemoveClip { seq: *seq, track: *track, start: clip.start };
            Ok((clips.insert(*clip)?, inverse))
        }),
        Op::RemoveClip { seq, track, start } => with_clips(p, *seq, *track, |clips| {
            let (next, clip) = clips.remove(*start)?;
            Ok((next, Op::InsertClip { seq: *seq, track: *track, clip }))
        }),
        Op::SetClip { seq, track, start, clip } => with_clips(p, *seq, *track, |clips| {
            let (without, old) = clips.remove(*start)?;
            let inverse = Op::SetClip { seq: *seq, track: *track, start: clip.start, clip: old };
            Ok((without.insert(*clip)?, inverse))
        }),
        Op::ShiftFrom { seq, track, from, delta } => with_clips(p, *seq, *track, |clips| {
            let (next, first_moved) = clips.shift_from(*from, *delta)?;
            // The inverse starts at the new position of the first moved clip,
            // not at `from + delta`: an unmoved clip may start in between.
            let inverse = match first_moved {
                Some(first) => Op::ShiftFrom { seq: *seq, track: *track, from: first + delta, delta: -delta },
                None => Op::Batch(Vec::new()),
            };
            Ok((next, inverse))
        }),
        Op::SetTrackMix { seq, track, mix } => with_sequence(p, *seq, |s| {
            let index = s.tracks.iter().position(|t| t.id == *track).ok_or(OpError::NoTrack(*track))?;
            let old = &s.tracks[index];
            let inverse = Op::SetTrackMix { seq: *seq, track: *track, mix: old.mix };
            // The clips and their properties stay shared.
            s.tracks[index] = Arc::new(Track { mix: mix.clamped(), ..Track::clone(old) });
            Ok(inverse)
        }),
        Op::SetScenario { id, scenario } => {
            if scenario.as_ref().is_some_and(|s| s.id != *id) {
                return Err(OpError::DuplicateId(id.to_string()));
            }
            let mut scenarios = BTreeMap::clone(&p.scenarios);
            let before = match scenario {
                Some(new) => scenarios.insert(*id, Arc::new(Scenario::clone(new).clamped())),
                None => scenarios.remove(id),
            };
            let inverse = Op::SetScenario { id: *id, scenario: before.map(|s| Box::new(Scenario::clone(&s))) };
            Ok((Project { scenarios: Arc::new(scenarios), ..p.clone() }, inverse))
        }
        Op::SetSong { id, song } => with_sequence(p, *id, |s| {
            let inverse = Op::SetSong { id: *id, song: s.song };
            s.song = song.map(|song| Song { tempo: song.tempo.clamp(-2, 2), ..song });
            Ok(inverse)
        }),
        Op::SetTempo { id, tempo } => with_sequence(p, *id, |s| {
            let inverse = Op::SetTempo { id: *id, tempo: s.tempo };
            s.tempo = tempo.map(crate::model::Tempo::clamped);
            Ok(inverse)
        }),
        Op::SetSpace { id, space } => with_sequence(p, *id, |s| {
            let inverse = Op::SetSpace { id: *id, space: s.space };
            s.space = space.clamped();
            Ok(inverse)
        }),
        Op::SetMasterGain { id, db } => with_sequence(p, *id, |s| {
            let inverse = Op::SetMasterGain { id: *id, db: s.master_db };
            s.master_db = if db.is_finite() { db.clamp(-60.0, 12.0) } else { 0.0 };
            Ok(inverse)
        }),
        Op::SetSequenceSize { id, width, height } => with_sequence(p, *id, |s| {
            if *width < 16 || *height < 16 || *width > 16384 || *height > 16384 {
                return Err(OpError::BadSize(*width, *height));
            }
            let inverse = Op::SetSequenceSize { id: *id, width: s.width, height: s.height };
            (s.width, s.height) = (*width, *height);
            Ok(inverse)
        }),
        Op::SetTrackPlugin { seq, track, plugin } => with_sequence(p, *seq, |s| {
            let index = s.tracks.iter().position(|t| t.id == *track).ok_or(OpError::NoTrack(*track))?;
            let old = &s.tracks[index];
            let before = Vec::clone(&old.plugins);
            let plugins = Arc::new(plugin.iter().cloned().collect());
            s.tracks[index] = Arc::new(Track { plugins, ..Track::clone(old) });
            Ok(Op::SetTrackPlugins { seq: *seq, track: *track, plugins: before })
        }),
        Op::SetTrackPlugins { seq, track, plugins } => with_sequence(p, *seq, |s| {
            let index = s.tracks.iter().position(|t| t.id == *track).ok_or(OpError::NoTrack(*track))?;
            let old = &s.tracks[index];
            let before = Vec::clone(&old.plugins);
            // (At most eight, and none twice under the same id.)
            let mut kept: Vec<crate::model::PluginRef> = Vec::new();
            for plugin in plugins.iter().take(8) {
                if !kept.iter().any(|k| k.id == plugin.id) {
                    kept.push(plugin.clone());
                }
            }
            s.tracks[index] = Arc::new(Track { plugins: Arc::new(kept), ..Track::clone(old) });
            Ok(Op::SetTrackPlugins { seq: *seq, track: *track, plugins: before })
        }),
        Op::SetClipNotes { seq, track, clip, notes } => with_sequence(p, *seq, |s| {
            let index = s.tracks.iter().position(|t| t.id == *track).ok_or(OpError::NoTrack(*track))?;
            let old = &s.tracks[index];
            let before = old.notes.get(clip).map(|n| Vec::clone(n));
            let mut map = BTreeMap::clone(&old.notes);
            match notes {
                // Kept in order of time, so that playing them is a walk through the list.
                Some(new) => {
                    let mut sorted = new.clone();
                    sorted.retain(|n| n.len > 0 && n.start >= 0 && n.pitch < 128);
                    sorted.sort_by_key(|n| (n.start, n.pitch));
                    map.insert(*clip, Arc::new(sorted))
                }
                None => map.remove(clip),
            };
            s.tracks[index] = Arc::new(Track { notes: Arc::new(map), ..Track::clone(old) });
            Ok(Op::SetClipNotes { seq: *seq, track: *track, clip: *clip, notes: before })
        }),
        Op::SetClipProps { seq, track, clip, props } => with_sequence(p, *seq, |s| {
            let index = s.tracks.iter().position(|t| t.id == *track).ok_or(OpError::NoTrack(*track))?;
            let old = &s.tracks[index];
            let before = old.props.get(clip).map(|p| Box::new(ClipProps::clone(p)));
            let mut map = BTreeMap::clone(&old.props);
            match props {
                Some(new) => map.insert(*clip, Arc::new(ClipProps::clone(new))),
                None => map.remove(clip),
            };
            // The clip arrays are shared, only the property map is new.
            s.tracks[index] = Arc::new(Track { props: Arc::new(map), ..Track::clone(old) });
            Ok(Op::SetClipProps { seq: *seq, track: *track, clip: *clip, props: before })
        }),
        Op::AddMarker { seq, marker } => with_sequence(p, *seq, |s| {
            let at = s.markers.partition_point(|m| m.frame < marker.frame);
            if s.markers.get(at).is_some_and(|m| m.frame == marker.frame) {
                return Err(OpError::DuplicateId(format!("marker at frame {}", marker.frame)));
            }
            let mut markers = Vec::clone(&s.markers);
            markers.insert(at, marker.clone());
            s.markers = Arc::new(markers);
            Ok(Op::RemoveMarker { seq: *seq, frame: marker.frame })
        }),
        Op::RemoveMarker { seq, frame } => with_sequence(p, *seq, |s| {
            let at = s.markers.binary_search_by_key(frame, |m| m.frame).map_err(|_| OpError::NoMarker(*frame))?;
            let mut markers = Vec::clone(&s.markers);
            let marker = markers.remove(at);
            s.markers = Arc::new(markers);
            Ok(Op::AddMarker { seq: *seq, marker })
        }),
        Op::AddAsset { asset } => {
            if p.assets.contains_key(&asset.id) {
                return Err(OpError::DuplicateId(asset.id.to_string()));
            }
            let mut assets = BTreeMap::clone(&p.assets);
            assets.insert(asset.id, Arc::new(Asset::clone(asset)));
            Ok((Project { assets: Arc::new(assets), ..p.clone() }, Op::RemoveAsset { id: asset.id }))
        }
        Op::RemoveAsset { id } => {
            let asset = p.assets.get(id).ok_or(OpError::NoAsset(*id))?;
            // Visits every clip; acceptable for an explicit, rare action.
            let used = p.sequences.iter().flat_map(|s| &s.tracks).any(|t| t.clips.iter().any(|c| c.asset == *id));
            if used {
                return Err(OpError::AssetInUse(*id));
            }
            let inverse = Op::AddAsset { asset: Box::new(Asset::clone(asset)) };
            let mut assets = BTreeMap::clone(&p.assets);
            assets.remove(id);
            Ok((Project { assets: Arc::new(assets), ..p.clone() }, inverse))
        }
        Op::SetAssetLabel { id, label } => {
            let old = p.assets.get(id).ok_or(OpError::NoAsset(*id))?;
            let inverse = Op::SetAssetLabel { id: *id, label: old.label.clone() };
            let mut assets = BTreeMap::clone(&p.assets);
            assets.insert(*id, Arc::new(Asset { label: label.clone(), ..Asset::clone(old) }));
            Ok((Project { assets: Arc::new(assets), ..p.clone() }, inverse))
        }
        Op::Batch(ops) => {
            let mut cur = p.clone();
            let mut inverses = Vec::with_capacity(ops.len());
            for op in ops {
                let (next, inverse) = apply_inner(&cur, op)?;
                cur = next;
                inverses.push(inverse);
            }
            inverses.reverse();
            Ok((cur, Op::Batch(inverses)))
        }
    }
}

fn check_index(index: usize, len: usize) -> Result<(), OpError> {
    if index > len { Err(OpError::BadIndex { index, len }) } else { Ok(()) }
}

fn sequence_index(p: &Project, id: SequenceId) -> Result<usize, OpError> {
    p.sequences.iter().position(|s| s.id == id).ok_or(OpError::NoSequence(id))
}

/// Copies the path project -> sequence, lets `f` edit the sequence copy and
/// shares everything else with `p`.
fn with_sequence(
    p: &Project,
    id: SequenceId,
    f: impl FnOnce(&mut Sequence) -> Result<Op, OpError>,
) -> Result<(Project, Op), OpError> {
    let index = sequence_index(p, id)?;
    let mut seq = Sequence::clone(&p.sequences[index]);
    let inverse = f(&mut seq)?;
    let mut next = p.clone();
    next.sequences[index] = Arc::new(seq);
    Ok((next, inverse))
}

fn with_clips(
    p: &Project,
    seq: SequenceId,
    track: TrackId,
    f: impl FnOnce(&ClipStore) -> Result<(ClipStore, Op), OpError>,
) -> Result<(Project, Op), OpError> {
    with_sequence(p, seq, |s| {
        let index = s.tracks.iter().position(|t| t.id == track).ok_or(OpError::NoTrack(track))?;
        let old = &s.tracks[index];
        let (clips, inverse) = f(&old.clips)?;
        s.tracks[index] = Arc::new(Track {
            id: old.id,
            kind: old.kind,
            name: old.name.clone(),
            clips,
            props: old.props.clone(),
            mix: old.mix,
            plugins: old.plugins.clone(),
            notes: old.notes.clone(),
        });
        Ok(inverse)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::ids::{AssetId, ProjectId};
    use crate::model::TrackKind;
    use crate::time::Rate;

    pub(crate) const SEQ: SequenceId = SequenceId(1);
    pub(crate) const V1: TrackId = TrackId(10);
    pub(crate) const A1: TrackId = TrackId(11);

    pub(crate) fn clip(id: u64, start: i64, len: i64) -> Clip {
        Clip { id: ClipId(id), asset: AssetId(0xA55E7), start, len, src_in: 0 }
    }

    /// A project with one sequence and an empty video and audio track.
    pub(crate) fn base() -> Arc<Project> {
        let mut p = Arc::new(Project::new(ProjectId(42), "Test"));
        let header = SequenceHeader { id: SEQ, name: "Main".into(), rate: Rate::FPS_60, width: 1920, height: 1080 };
        for op in [
            Op::AddSequence { index: 0, sequence: header },
            Op::AddTrack {
                seq: SEQ,
                index: 0,
                track: TrackHeader { id: V1, kind: TrackKind::Video, name: "V1".into() },
            },
            Op::AddTrack {
                seq: SEQ,
                index: 1,
                track: TrackHeader { id: A1, kind: TrackKind::Audio, name: "A1".into() },
            },
        ] {
            p = apply(&p, &op).unwrap().0;
        }
        p
    }

    fn starts(p: &Project, track: TrackId) -> Vec<(i64, i64)> {
        let t = p.sequence(SEQ).unwrap().track(track).unwrap();
        t.clips.iter().map(|c| (c.start, c.len)).collect()
    }

    /// Applies `op`, checks that the inverse restores the input exactly and
    /// that the inverse of the inverse redoes the op.
    pub(crate) fn round_trip(p: &Arc<Project>, op: &Op) -> Arc<Project> {
        let (after, inverse) = apply(p, op).unwrap();
        let (back, redo) = apply(&after, &inverse).unwrap();
        assert_eq!(&back, p, "inverse of {op:?}");
        let (again, _) = apply(&back, &redo).unwrap();
        assert_eq!(again, after, "redo of {op:?}");
        after
    }

    #[test]
    fn rename_project_and_sequence() {
        let p = base();
        let p2 = round_trip(&p, &Op::SetProjectName { name: "Other".into() });
        assert_eq!(p2.name, "Other");
        let p3 = round_trip(&p2, &Op::SetSequenceName { id: SEQ, name: "Cut 2".into() });
        assert_eq!(p3.sequences[0].name, "Cut 2");
    }

    #[test]
    fn structure_ops_validate() {
        let p = base();
        let dup = Op::AddTrack {
            seq: SEQ,
            index: 0,
            track: TrackHeader { id: V1, kind: TrackKind::Video, name: "x".into() },
        };
        assert!(matches!(apply(&p, &dup), Err(OpError::DuplicateId(_))));
        let far = Op::AddTrack {
            seq: SEQ,
            index: 9,
            track: TrackHeader { id: TrackId(99), kind: TrackKind::Video, name: "x".into() },
        };
        assert_eq!(apply(&p, &far).unwrap_err(), OpError::BadIndex { index: 9, len: 2 });
        assert_eq!(apply(&p, &Op::RemoveSequence { id: SEQ }).unwrap_err(), OpError::NotEmpty("sequence"));
        assert_eq!(
            apply(&p, &Op::RemoveTrack { seq: SequenceId(5), id: V1 }).unwrap_err(),
            OpError::NoSequence(SequenceId(5))
        );
        let p2 = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(1, 0, 10) });
        assert_eq!(apply(&p2, &Op::RemoveTrack { seq: SEQ, id: V1 }).unwrap_err(), OpError::NotEmpty("track"));
        let p3 = round_trip(&p, &Op::RemoveTrack { seq: SEQ, id: V1 });
        assert_eq!(p3.sequences[0].tracks.len(), 1);
    }

    #[test]
    fn clip_ops() {
        let p = base();
        let p = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(1, 0, 100) });
        let p = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(2, 100, 50) });
        // Trim the head of the second clip by 10 and move it right.
        let trimmed = Clip { start: 120, len: 40, src_in: 10, ..clip(2, 0, 0) };
        let p = round_trip(&p, &Op::SetClip { seq: SEQ, track: V1, start: 100, clip: trimmed });
        assert_eq!(starts(&p, V1), [(0, 100), (120, 40)]);
        let bad = Op::SetClip { seq: SEQ, track: V1, start: 120, clip: clip(2, 50, 40) };
        assert!(matches!(apply(&p, &bad), Err(OpError::Clip(ClipError::Overlap { .. }))));
        let p = round_trip(&p, &Op::RemoveClip { seq: SEQ, track: V1, start: 0 });
        assert_eq!(starts(&p, V1), [(120, 40)]);
        assert_eq!(starts(&p, A1), []);
    }

    #[test]
    fn split_keeps_source_continuity() {
        let p = base();
        let c = Clip { src_in: 30, ..clip(1, 100, 60) };
        let p = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: c });
        let op = Op::split(SEQ, V1, &c, 120, ClipId(2)).unwrap();
        let p = round_trip(&p, &op);
        let clips: Vec<Clip> = p.sequences[0].tracks[0].clips.iter().collect();
        assert_eq!((clips[0].start, clips[0].len, clips[0].src_in, clips[0].id), (100, 20, 30, ClipId(1)));
        assert_eq!((clips[1].start, clips[1].len, clips[1].src_in, clips[1].id), (120, 40, 50, ClipId(2)));
        assert!(Op::split(SEQ, V1, &c, 100, ClipId(3)).is_none());
        assert!(Op::split(SEQ, V1, &c, 160, ClipId(3)).is_none());
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let mut p = base();
        for (i, s) in [0, 100, 250].into_iter().enumerate() {
            p = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(i as u64 + 1, s, 100) });
        }
        let victim = p.sequences[0].tracks[0].clips.get(100).unwrap();
        let p = round_trip(&p, &Op::ripple_delete(SEQ, V1, &victim));
        assert_eq!(starts(&p, V1), [(0, 100), (150, 100)]);
    }

    #[test]
    fn shift_inverse_ignores_unmoved_clip_in_between() {
        // Clip at 70 stays; the clip at 200 moves to 100. A naive inverse
        // "shift from 100 - ..." must not pick up the clip at 70.
        let mut p = base();
        for (i, (s, l)) in [(70, 10), (200, 10)].into_iter().enumerate() {
            p = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(i as u64 + 1, s, l) });
        }
        let p2 = round_trip(&p, &Op::ShiftFrom { seq: SEQ, track: V1, from: 150, delta: -100 });
        assert_eq!(starts(&p2, V1), [(70, 10), (100, 10)]);
        let (same, inverse) = apply(&p, &Op::ShiftFrom { seq: SEQ, track: V1, from: 500, delta: 5 }).unwrap();
        assert_eq!(same, p);
        assert_eq!(inverse, Op::Batch(Vec::new()));
    }

    #[test]
    fn failed_batch_changes_nothing() {
        let p = base();
        let batch = Op::Batch(vec![
            Op::InsertClip { seq: SEQ, track: V1, clip: clip(1, 0, 10) },
            Op::InsertClip { seq: SEQ, track: V1, clip: clip(2, 5, 10) },
        ]);
        assert!(apply(&p, &batch).is_err());
        assert_eq!(starts(&p, V1), []);
    }

    #[test]
    fn untouched_tracks_are_shared() {
        let p = base();
        let (p2, _) = apply(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(1, 0, 10) }).unwrap();
        assert!(Arc::ptr_eq(&p.sequences[0].tracks[1], &p2.sequences[0].tracks[1]));
        assert!(!Arc::ptr_eq(&p.sequences[0].tracks[0], &p2.sequences[0].tracks[0]));
    }

    #[test]
    fn ops_survive_json() {
        let c = clip(1, 0, 10);
        let ops = vec![
            Op::SetProjectName { name: "a \"b\"\n".into() },
            Op::split(SEQ, V1, &Clip { len: 100, ..c }, 40, ClipId(u64::MAX)).unwrap(),
            Op::ShiftFrom { seq: SEQ, track: A1, from: 5, delta: -5 },
            Op::RemoveTrack { seq: SEQ, id: A1 },
        ];
        let json = serde_json::to_string(&ops).unwrap();
        assert_eq!(serde_json::from_str::<Vec<Op>>(&json).unwrap(), ops);
    }

    /// An op on the two test tracks, valid or not. `pick` selects an
    /// existing clip where one is needed.
    pub(crate) fn any_op() -> impl Strategy<Value = (u8, bool, i64, i64, usize, u64)> {
        (0u8..13, any::<bool>(), 0i64..2000, 1i64..60, any::<usize>(), 1u64..u64::MAX)
    }

    pub(crate) fn make_op(p: &Project, (kind, video, a, b, pick, id): (u8, bool, i64, i64, usize, u64)) -> Op {
        let track = if video { V1 } else { A1 };
        let clips = &p.sequence(SEQ).unwrap().track(track).unwrap().clips;
        let existing = (!clips.is_empty()).then(|| clips.iter().nth(pick % clips.len()).unwrap());
        match (kind, existing) {
            // Settings of tracks, the sequence and scenarios need no clip.
            (7, _) => {
                let mix = TrackSettings {
                    gain_db: -(b as f32) / 2.0,
                    mute: pick % 2 == 0,
                    hidden: pick % 3 == 0,
                    eq_mid_hz: 500.0 + a as f32,
                    comp_threshold_db: -(a % 40) as f32,
                    ..TrackSettings::default()
                };
                Op::SetTrackMix { seq: SEQ, track, mix }
            }
            (8, _) => Op::SetMasterGain { id: SEQ, db: (b - 30) as f32 / 4.0 },
            (9, _) => {
                let song = Song { asset: AssetId(id as u128 % 5), tempo: (b % 3) as i8 - 1 };
                Op::SetSong { id: SEQ, song: (pick % 4 != 0).then_some(song) }
            }
            (10, _) => {
                // One of three scenarios: made, changed or removed.
                let scenario_id = ScenarioId(1 + id % 3);
                let mut scenario = match p.scenarios.get(&scenario_id) {
                    Some(known) => Scenario::clone(known),
                    None => Scenario::new(scenario_id, format!("s{b}"), (a % 8) as u8),
                };
                scenario.strength = (b % 5) as f32 / 4.0;
                scenario.lut = if pick % 2 == 0 { "film.cube".to_owned() } else { String::new() };
                let stats = crate::colour::ColourStats {
                    log_lum: -(a as f32) / 512.0,
                    spread: 1.0,
                    wb: [0.25, -0.125],
                    saturation: 0.5,
                };
                let member = crate::colour::Member { stats: (pick % 3 != 0).then_some(stats) };
                scenario.members.insert(AssetId(a as u128 % 4), member);
                if pick % 4 == 0 && scenario.look.len() < crate::colour::MAX_MODULES {
                    scenario.look.push(crate::colour::Module::new(crate::colour::ModuleKind::ALL[b as usize % 9]));
                }
                Op::SetScenario { id: scenario_id, scenario: (pick % 7 != 0).then(|| Box::new(scenario)) }
            }
            (0, _) | (_, None) => Op::InsertClip { seq: SEQ, track, clip: clip(id, a, b) },
            (6, Some(c)) => {
                // Properties of every kind, in values that are exact in text.
                let transition = crate::props::Transition {
                    frames: 2 + 2 * (b % 4) as u8,
                    angle: (a % 4) as f32 * 90.0,
                    ..Default::default()
                };
                let props = ClipProps {
                    exposure: (b % 8) as f32 / 4.0,
                    stabilise: pick % 2 == 0,
                    ramp: crate::props::Param {
                        value: 1.0,
                        keys: vec![crate::props::Key {
                            frame: a % 50,
                            value: 0.5,
                            ease: crate::props::Ease::ALL[pick % 3],
                        }],
                    },
                    transition: (pick % 3 == 0).then_some(transition),
                    ..ClipProps::default()
                };
                Op::SetClipProps { seq: SEQ, track, clip: c.id, props: (pick % 5 != 0).then(|| Box::new(props)) }
            }
            (11 | 12, Some(_)) => {
                Op::overwrite(p.sequence(SEQ).unwrap(), track, clip(id, a, b), ClipId(id ^ 1)).unwrap()
            }
            (1, Some(c)) => Op::RemoveClip { seq: SEQ, track, start: c.start },
            (2, Some(c)) => Op::SetClip { seq: SEQ, track, start: c.start, clip: Clip { start: a, len: b, ..c } },
            (3, Some(c)) => Op::split(SEQ, track, &c, c.start + b % c.len, ClipId(id))
                .unwrap_or(Op::SetProjectName { name: format!("p{id}") }),
            (4, Some(c)) => Op::ripple_delete(SEQ, track, &c),
            (_, Some(_)) => Op::ShiftFrom { seq: SEQ, track, from: a, delta: b - 30 },
        }
    }

    proptest! {
        #[test]
        fn every_op_is_undone_by_its_inverse(seeds in prop::collection::vec(any_op(), 1..200)) {
            let mut p = base();
            for seed in seeds {
                let op = make_op(&p, seed);
                let Ok((after, inverse)) = apply(&p, &op) else { continue };
                let (back, redo) = apply(&after, &inverse).unwrap();
                prop_assert_eq!(&back, &p, "inverse of {:?}", op);
                let (again, _) = apply(&back, &redo).unwrap();
                prop_assert_eq!(&again, &after, "redo of {:?}", op);
                p = after;
            }
        }
    }
}

#[cfg(test)]
mod asset_tests {
    use super::tests::{SEQ, V1, base};
    use super::*;
    use crate::asset::tests::sample;
    use crate::text::{from_files, to_files};

    fn add(id: u128) -> Op {
        Op::AddAsset { asset: Box::new(sample(id)) }
    }

    #[test]
    fn add_relabel_remove_round_trip() {
        let p0 = base();
        let (p1, undo_add) = apply(&p0, &add(7)).unwrap();
        assert_eq!(p1.assets.len(), 1);
        assert!(matches!(apply(&p1, &add(7)), Err(OpError::DuplicateId(_))));
        let (p2, undo_label) = apply(&p1, &Op::SetAssetLabel { id: AssetId(7), label: "B-roll".into() }).unwrap();
        assert_eq!(p2.assets[&AssetId(7)].label, "B-roll");
        assert_eq!(apply(&p2, &undo_label).unwrap().0, p1);
        assert_eq!(apply(&p1, &undo_add).unwrap().0, p0);
        let (p3, undo_remove) = apply(&p2, &Op::RemoveAsset { id: AssetId(7) }).unwrap();
        assert!(p3.assets.is_empty());
        assert_eq!(apply(&p3, &undo_remove).unwrap().0, p2);
        assert_eq!(apply(&p0, &Op::RemoveAsset { id: AssetId(9) }).unwrap_err(), OpError::NoAsset(AssetId(9)));
    }

    #[test]
    fn an_asset_in_use_cannot_be_removed() {
        let (p, _) = apply(&base(), &add(7)).unwrap();
        let clip = Clip { id: ClipId(1), asset: AssetId(7), start: 0, len: 10, src_in: 0 };
        let (p, _) = apply(&p, &Op::InsertClip { seq: SEQ, track: V1, clip }).unwrap();
        assert_eq!(apply(&p, &Op::RemoveAsset { id: AssetId(7) }).unwrap_err(), OpError::AssetInUse(AssetId(7)));
    }

    #[test]
    fn clip_edits_share_the_asset_map() {
        let (p, _) = apply(&base(), &add(7)).unwrap();
        let clip = Clip { id: ClipId(1), asset: AssetId(7), start: 0, len: 10, src_in: 0 };
        let (p2, _) = apply(&p, &Op::InsertClip { seq: SEQ, track: V1, clip }).unwrap();
        assert!(Arc::ptr_eq(&p.assets, &p2.assets));
    }

    #[test]
    fn assets_are_index_files() {
        let (p, _) = apply(&base(), &add(0xABC)).unwrap();
        let (p, _) = apply(&p, &add(0x123)).unwrap();
        let files = to_files(&p);
        assert!(files.contains_key("assets/index/00000000000000000000000000000abc.toml"));
        assert_eq!(from_files(&files).unwrap(), *p);
        // An index entry may not point outside the bundle.
        let mut evil = files.clone();
        let key = "assets/index/00000000000000000000000000000abc.toml";
        evil.insert(key.into(), files[key].replace("assets/video/", "assets/../../"));
        assert!(from_files(&evil).is_err());
    }
}

#[cfg(test)]
mod marker_tests {
    use super::tests::{SEQ, base};
    use super::*;
    use crate::text::{from_files, to_files};

    #[test]
    fn markers_stay_sorted_unique_and_round_trip() {
        let mut p = base();
        for (frame, label) in [(300, "chorus"), (10, ""), (120, "drop \"in\"")] {
            let op = Op::AddMarker { seq: SEQ, marker: Marker { frame, label: label.into() } };
            let (next, inverse) = apply(&p, &op).unwrap();
            assert_eq!(apply(&next, &inverse).unwrap().0, p);
            p = next;
        }
        let frames: Vec<i64> = p.sequences[0].markers.iter().map(|m| m.frame).collect();
        assert_eq!(frames, [10, 120, 300]);
        let dup = Op::AddMarker { seq: SEQ, marker: Marker { frame: 120, label: String::new() } };
        assert!(matches!(apply(&p, &dup), Err(OpError::DuplicateId(_))));
        assert_eq!(apply(&p, &Op::RemoveMarker { seq: SEQ, frame: 11 }).unwrap_err(), OpError::NoMarker(11));

        let files = to_files(&p);
        assert!(files["sequences/main.sg"].contains("marker 120 \"drop \\\"in\\\"\"\n"));
        assert_eq!(from_files(&files).unwrap(), *p);

        let (removed, inverse) = apply(&p, &Op::RemoveMarker { seq: SEQ, frame: 120 }).unwrap();
        assert_eq!(removed.sequences[0].markers.len(), 2);
        assert_eq!(apply(&removed, &inverse).unwrap().0, p);
    }
}

#[cfg(test)]
mod props_tests {
    use super::tests::{SEQ, V1, base, clip};
    use super::*;
    use crate::props::Param;
    use crate::text::{from_files, to_files};

    #[test]
    fn properties_are_set_cleared_undone_and_saved() {
        let (p, _) = apply(&base(), &Op::InsertClip { seq: SEQ, track: V1, clip: clip(7, 0, 100) }).unwrap();
        let mut props = ClipProps { speed: (2, 1), crop: [0.1, 0.0, 0.1, 0.0], ..ClipProps::default() };
        props.opacity = Param::constant(0.5);
        props.x.set_key(0, -0.25);
        props.x.set_key(30, 0.25);
        let set = Op::SetClipProps { seq: SEQ, track: V1, clip: ClipId(7), props: Some(Box::new(props.clone())) };
        let (p2, undo) = apply(&p, &set).unwrap();
        let track = &p2.sequences[0].tracks[0];
        assert_eq!(track.props_of(ClipId(7)).map(|x| &**x), Some(&props));
        assert!(Arc::ptr_eq(&p.sequences[0].tracks[1], &p2.sequences[0].tracks[1]));
        assert_eq!(apply(&p2, &undo).unwrap().0, p);

        let files = to_files(&p2);
        let line = files["sequences/main.sg"].lines().find(|l| l.starts_with("props ")).unwrap();
        assert!(line.starts_with("props 0000000000000007 {"), "{line}");
        assert_eq!(from_files(&files).unwrap(), *p2);

        let clear = Op::SetClipProps { seq: SEQ, track: V1, clip: ClipId(7), props: None };
        let (p3, undo) = apply(&p2, &clear).unwrap();
        assert!(p3.sequences[0].tracks[0].props.is_empty());
        assert_eq!(apply(&p3, &undo).unwrap().0, p2);
        // Removing the clip keeps its properties for the undo.
        let (p4, undo) = apply(&p2, &Op::RemoveClip { seq: SEQ, track: V1, start: 0 }).unwrap();
        assert_eq!(apply(&p4, &undo).unwrap().0, p2);
    }
}

#[cfg(test)]
mod size_tests {
    use super::tests::{SEQ, base};
    use super::*;

    #[test]
    fn frame_size_changes_and_undoes() {
        let p = base();
        let (p2, undo) = apply(&p, &Op::SetSequenceSize { id: SEQ, width: 1080, height: 1920 }).unwrap();
        assert_eq!((p2.sequences[0].width, p2.sequences[0].height), (1080, 1920));
        assert_eq!(apply(&p2, &undo).unwrap().0, p);
        assert_eq!(
            apply(&p, &Op::SetSequenceSize { id: SEQ, width: 0, height: 100 }).unwrap_err(),
            OpError::BadSize(0, 100)
        );
    }
}

#[cfg(test)]
mod mix_tests {
    use super::tests::{A1, SEQ, base};
    use super::*;

    #[test]
    fn track_settings_and_master_change_and_undo() {
        let p = base();
        let mix = TrackSettings {
            gain_db: -6.0,
            pan: 0.5,
            mute: true,
            solo: false,
            low_cut_hz: 80.0,
            denoise: 0.7,
            eq_mid_db: -3.0,
            comp_threshold_db: -18.0,
            ..TrackSettings::default()
        };
        let (p2, undo) = apply(&p, &Op::SetTrackMix { seq: SEQ, track: A1, mix }).unwrap();
        assert_eq!(p2.sequences[0].track(A1).unwrap().mix, mix);
        assert_eq!(apply(&p2, &undo).unwrap().0, p);
        // A plugin and the notes of a clip come and go the same way.
        let surge = crate::model::PluginRef { name: "Surge XT".into(), path: "/lib/vst3/Surge XT.vst3".into(), id: 0 };
        let (plugged, unplug) =
            apply(&p2, &Op::SetTrackPlugin { seq: SEQ, track: A1, plugin: Some(surge.clone()) }).unwrap();
        let a1 = |p: &Project| p.sequences[0].tracks.iter().find(|t| t.id == A1).unwrap().clone();
        assert_eq!(*a1(&plugged).plugins, std::slice::from_ref(&surge));
        assert_eq!(apply(&plugged, &unplug).unwrap().0, p2);
        // Several on a track, in an order; the same id is not taken twice.
        let named = |name: &str, id| crate::model::PluginRef { name: name.into(), path: format!("/{name}.vst3"), id };
        let chain = vec![named("Synth", 7), named("Squash", 8), named("Squash again", 8), named("Wide", 9)];
        let (chained, unchain) = apply(&plugged, &Op::SetTrackPlugins { seq: SEQ, track: A1, plugins: chain }).unwrap();
        let names: Vec<String> = a1(&chained).plugins.iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, ["Synth", "Squash", "Wide"]);
        assert_eq!(apply(&chained, &unchain).unwrap().0, plugged);
        // Each has a file of its own for its settings; the one from before keeps its old name.
        assert_eq!(surge.state_file(A1), format!("{A1}.state"));
        assert_eq!(named("Wide", 9).state_file(A1), format!("{A1}-0000000000000009.state"));
        assert_eq!((surge.key(A1), named("Wide", 9).key(A1)), (A1.0, 9));
        let note = |start, pitch| crate::model::Note { start, len: 480, pitch, velocity: 100 };
        let played = vec![note(960, 64), note(0, 60), note(0, 55), note(-5, 60)];
        let set = Op::SetClipNotes { seq: SEQ, track: A1, clip: ClipId(77), notes: Some(played) };
        let (noted, unnote) = apply(&p2, &set).unwrap();
        // Sorted by time, and what cannot be a note is left out.
        assert_eq!(**a1(&noted).notes.get(&ClipId(77)).unwrap(), vec![note(0, 55), note(0, 60), note(960, 64)]);
        assert_eq!(apply(&noted, &unnote).unwrap().0, p2);
        // So do the room and the echo, brought into range.
        let wide =
            crate::model::Space { room_ds: 200, echo_note: crate::model::EchoNote::DottedEighth, echo_repeat: 60 };
        let (spaced, unspace) = apply(&p2, &Op::SetSpace { id: SEQ, space: wide }).unwrap();
        assert_eq!(spaced.sequences[0].space, crate::model::Space { room_ds: 80, ..wide });
        assert_eq!(apply(&spaced, &unspace).unwrap().0, p2);
        // A tempo comes and goes like any other setting.
        let waltz = crate::model::Tempo { milli_bpm: 93_500, beats_per_bar: 3 };
        let (timed, untime) = apply(&p2, &Op::SetTempo { id: SEQ, tempo: Some(waltz) }).unwrap();
        assert_eq!(timed.sequences[0].tempo, Some(waltz));
        assert_eq!(apply(&timed, &untime).unwrap().0, p2);
        let (p3, undo) = apply(&p2, &Op::SetMasterGain { id: SEQ, db: -3.0 }).unwrap();
        assert_eq!(p3.sequences[0].master_db, -3.0);
        assert_eq!(apply(&p3, &undo).unwrap().0, p2);
        // Out-of-range and non-numbers are brought back into range.
        let wild = TrackSettings { gain_db: 99.0, pan: f32::NAN, denoise: 7.0, ..mix };
        let (p4, _) = apply(&p, &Op::SetTrackMix { seq: SEQ, track: A1, mix: wild }).unwrap();
        let kept = p4.sequences[0].track(A1).unwrap().mix;
        assert_eq!((kept.gain_db, kept.pan, kept.denoise), (12.0, 0.0, 1.0));
    }

    #[test]
    fn removing_a_track_remembers_its_settings() {
        let p = base();
        let mix = TrackSettings { gain_db: -12.0, ..TrackSettings::default() };
        let (with_mix, _) = apply(&p, &Op::SetTrackMix { seq: SEQ, track: A1, mix }).unwrap();
        let (without, undo) = apply(&with_mix, &Op::RemoveTrack { seq: SEQ, id: A1 }).unwrap();
        assert!(without.sequences[0].track(A1).is_none());
        assert_eq!(apply(&without, &undo).unwrap().0, with_mix);
    }
}

#[cfg(test)]
mod song_tests {
    use super::tests::{SEQ, V1, base, clip, round_trip};
    use super::*;
    use crate::ids::AssetId;

    fn starts(p: &Project) -> Vec<(u128, i64, i64, i64)> {
        p.sequences[0].track(V1).unwrap().clips.iter().map(|c| (c.id.0 as u128, c.start, c.len, c.src_in)).collect()
    }

    #[test]
    fn song_is_set_and_undone() {
        let p = base();
        let song = Song { asset: AssetId(7), tempo: 1 };
        let (p2, undo) = apply(&p, &Op::SetSong { id: SEQ, song: Some(song) }).unwrap();
        assert_eq!(p2.sequences[0].song, Some(song));
        assert_eq!(apply(&p2, &undo).unwrap().0, p);
    }

    #[test]
    fn overwrite_leaves_the_pieces_their_properties() {
        use crate::props::{Key, Param};
        let mut p = base();
        p = round_trip(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(1, 0, 100) });
        // Twice the speed, moving across the frame, fading in and out.
        let props = ClipProps {
            speed: (2, 1),
            x: Param { value: 0.0, keys: vec![Key::new(0, 0.0), Key::new(100, 1.0)] },
            fade_in: 5,
            fade_out: 7,
            ..ClipProps::default()
        };
        p = round_trip(&p, &Op::SetClipProps { seq: SEQ, track: V1, clip: ClipId(1), props: Some(Box::new(props)) });
        // Something else lands on frames 40 to 60.
        let op = Op::overwrite(&p.sequences[0], V1, clip(9, 40, 20), ClipId(99)).unwrap();
        let p = round_trip(&p, &op);
        let track = &p.sequences[0].tracks[0];
        let (head, tail) = (track.clips.get(0).unwrap(), track.clips.get(60).unwrap());
        let (hp, tp) = (track.props_of(head.id).unwrap(), track.props_of(tail.id).unwrap());
        // The tail goes on at source frame 120 (60 frames at twice the speed), where it would have been.
        assert_eq!((tail.id, tail.len, tail.src_in), (ClipId(99), 40, 120));
        assert_eq!((tp.speed, tp.fade_in, tp.fade_out), ((2, 1), 0, 7));
        assert!((tp.x.at(0) - 0.6).abs() < 1e-6 && (tp.x.at(40) - 1.0).abs() < 1e-6);
        assert_eq!((head.len, hp.fade_in, hp.fade_out), (40, 5, 0));
        assert!((hp.x.at(20) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn overwrite_clears_exactly_its_span() {
        // Three clips: 0..100, 100..200, 300..400 (source positions 0 each).
        let mut p = base();
        for (id, start) in [(1u64, 0i64), (2, 100), (3, 300)] {
            p = apply(&p, &Op::InsertClip { seq: SEQ, track: V1, clip: clip(id, start, 100) }).unwrap().0;
        }
        let before = p.clone();
        // 50..350 swallows the middle clip and shortens both neighbours.
        let new = Clip { src_in: 7, ..clip(9, 50, 300) };
        let op = Op::overwrite(&p.sequences[0], V1, new, ClipId(99)).unwrap();
        let (after, undo) = apply(&p, &op).unwrap();
        assert_eq!(starts(&after), vec![(1, 0, 50, 0), (9, 50, 300, 7), (3, 350, 50, 50)]);
        assert_eq!(apply(&after, &undo).unwrap().0, before);
        // Inside one clip: that clip is cut in two around it.
        let op = Op::overwrite(&p.sequences[0], V1, clip(9, 120, 30), ClipId(99)).unwrap();
        let (after, undo) = apply(&p, &op).unwrap();
        assert_eq!(
            starts(&after),
            vec![(1, 0, 100, 0), (2, 100, 20, 0), (9, 120, 30, 0), (99, 150, 50, 50), (3, 300, 100, 0)]
        );
        assert_eq!(apply(&after, &undo).unwrap().0, before);
        // On free space it is a plain insert.
        let op = Op::overwrite(&p.sequences[0], V1, clip(9, 200, 100), ClipId(99)).unwrap();
        assert_eq!(apply(&p, &op).unwrap().0.sequences[0].track(V1).unwrap().clips.len(), 4);
    }
}

#[cfg(test)]
mod scenario_tests {
    use super::tests::base;
    use super::*;
    use crate::colour::{Member, Module, ModuleKind};

    #[test]
    fn scenarios_are_set_replaced_and_removed_with_exact_inverses() {
        let p = base();
        let id = ScenarioId(5);
        let mut scenario = Scenario::new(id, "Stage", 2);
        scenario.members.insert(AssetId(7), Member::default());
        let (p1, undo1) = apply(&p, &Op::SetScenario { id, scenario: Some(Box::new(scenario.clone())) }).unwrap();
        assert_eq!(p1.scenario_of(AssetId(7)).map(|s| s.name.as_str()), Some("Stage"));
        scenario.look.push(Module::new(ModuleKind::Tone));
        let (p2, undo2) = apply(&p1, &Op::SetScenario { id, scenario: Some(Box::new(scenario)) }).unwrap();
        assert_eq!(p2.scenarios[&id].look.len(), 1);
        assert_eq!(apply(&p2, &undo2).unwrap().0, p1);
        let (p3, undo3) = apply(&p2, &Op::SetScenario { id, scenario: None }).unwrap();
        assert!(p3.scenarios.is_empty());
        assert_eq!(apply(&p3, &undo3).unwrap().0, p2);
        assert_eq!(apply(&p1, &undo1).unwrap().0, p);
        // The id in the op and in the scenario must agree.
        let wrong = Scenario::new(ScenarioId(6), "x", 0);
        assert!(apply(&p, &Op::SetScenario { id, scenario: Some(Box::new(wrong)) }).is_err());
    }
}
