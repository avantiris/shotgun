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
use crate::ids::{AssetId, ClipId, SequenceId, TrackId};
use crate::model::{Marker, Project, Sequence, SequenceHeader, Track, TrackHeader};
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
            let inverse = Op::AddSequence { index, sequence: seq.header() };
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
            Ok(Op::AddTrack { seq: *seq, index, track: track.header() })
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
        Op::SetSequenceSize { id, width, height } => with_sequence(p, *id, |s| {
            if *width < 16 || *height < 16 || *width > 16384 || *height > 16384 {
                return Err(OpError::BadSize(*width, *height));
            }
            let inverse = Op::SetSequenceSize { id: *id, width: s.width, height: s.height };
            (s.width, s.height) = (*width, *height);
            Ok(inverse)
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
        s.tracks[index] =
            Arc::new(Track { id: old.id, kind: old.kind, name: old.name.clone(), clips, props: old.props.clone() });
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
    fn round_trip(p: &Arc<Project>, op: &Op) -> Arc<Project> {
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
        (0u8..6, any::<bool>(), 0i64..2000, 1i64..60, any::<usize>(), 1u64..u64::MAX)
    }

    pub(crate) fn make_op(p: &Project, (kind, video, a, b, pick, id): (u8, bool, i64, i64, usize, u64)) -> Op {
        let track = if video { V1 } else { A1 };
        let clips = &p.sequence(SEQ).unwrap().track(track).unwrap().clips;
        let existing = (!clips.is_empty()).then(|| clips.iter().nth(pick % clips.len()).unwrap());
        match (kind, existing) {
            (0, _) | (_, None) => Op::InsertClip { seq: SEQ, track, clip: clip(id, a, b) },
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
