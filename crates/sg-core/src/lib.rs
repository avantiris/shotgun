//! Project model: pure data and edit operations. No I/O, no GPU.
//!
//! A project is an immutable snapshot (`Arc<Project>`). Every change is an
//! [`Op`] applied with [`apply`], which returns a new snapshot sharing all
//! untouched data with the old one, plus the inverse op.

pub mod asset;
pub mod clips;
pub mod colour;
pub mod history;
pub mod ids;
pub mod model;
pub mod ops;
pub mod props;
pub mod text;
pub mod time;

pub use asset::{Asset, AssetKind, AudioInfo, Source, VideoInfo};
pub use clips::{Clip, ClipError, ClipStore, Visible};
pub use colour::{ColourStats, Correction, Grade, Lut, Member, Module, ModuleKind, Scenario, Transfer};
pub use history::{Document, History, HistoryError, Record};
pub use ids::ScenarioId;
pub use ids::{AssetId, ClipId, IdGen, ProjectId, SequenceId, TrackId};
pub use model::{
    EchoNote, GROUPS, Group, Marker, Note, PluginRef, Project, STAGE_ORDER, STAGES, Sampler, Sequence, SequenceHeader,
    Song, Space, TICKS_PER_BEAT, Tempo, Track, TrackHeader, TrackKind, TrackSettings,
};
pub use ops::{Op, OpError, apply};
pub use props::{ClipProps, Ease, Key, Param, Shift, Title, Transition, TransitionKind};
pub use time::Rate;
