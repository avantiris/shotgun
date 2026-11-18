//! The project snapshot. Everything below `Project` is behind `Arc`, so a new
//! snapshot made by an edit shares every sequence and track it did not touch.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::asset::Asset;
use crate::clips::ClipStore;
use crate::ids::{AssetId, ProjectId, SequenceId, TrackId};
use crate::time::Rate;

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    /// The first sequence is the one opened by default.
    pub sequences: Vec<Arc<Sequence>>,
    /// Shared as a whole: asset edits are rare, so they copy the map, while
    /// every other edit just bumps this reference.
    pub assets: Arc<BTreeMap<AssetId, Arc<Asset>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sequence {
    pub id: SequenceId,
    pub name: String,
    pub rate: Rate,
    pub width: u32,
    pub height: u32,
    /// Top of the timeline first.
    pub tracks: Vec<Arc<Track>>,
    /// Sorted by frame, at most one per frame.
    pub markers: Arc<Vec<Marker>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub clips: ClipStore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
}

/// A sequence without its tracks, as carried by ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceHeader {
    pub id: SequenceId,
    pub name: String,
    pub rate: Rate,
    pub width: u32,
    pub height: u32,
}

/// A track without its clips, as carried by ops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackHeader {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
}

impl Project {
    pub fn new(id: ProjectId, name: impl Into<String>) -> Project {
        Project { id, name: name.into(), sequences: Vec::new(), assets: Arc::default() }
    }

    pub fn sequence(&self, id: SequenceId) -> Option<&Arc<Sequence>> {
        self.sequences.iter().find(|s| s.id == id)
    }

    pub fn clip_count(&self) -> usize {
        self.sequences.iter().map(|s| s.clip_count()).sum()
    }
}

impl Sequence {
    pub fn header(&self) -> SequenceHeader {
        SequenceHeader { id: self.id, name: self.name.clone(), rate: self.rate, width: self.width, height: self.height }
    }

    pub fn track(&self, id: TrackId) -> Option<&Arc<Track>> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn clip_count(&self) -> usize {
        self.tracks.iter().map(|t| t.clips.len()).sum()
    }
}

impl From<SequenceHeader> for Sequence {
    fn from(h: SequenceHeader) -> Sequence {
        Sequence {
            id: h.id,
            name: h.name,
            rate: h.rate,
            width: h.width,
            height: h.height,
            tracks: Vec::new(),
            markers: Arc::default(),
        }
    }
}

impl Track {
    pub fn header(&self) -> TrackHeader {
        TrackHeader { id: self.id, kind: self.kind, name: self.name.clone() }
    }
}

impl From<TrackHeader> for Track {
    fn from(h: TrackHeader) -> Track {
        Track { id: h.id, kind: h.kind, name: h.name, clips: ClipStore::new() }
    }
}

/// A named point on a sequence's timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub frame: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}
