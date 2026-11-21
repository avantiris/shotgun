//! The project snapshot. Everything below `Project` is behind `Arc`, so a new
//! snapshot made by an edit shares every sequence and track it did not touch.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::asset::Asset;
use crate::clips::ClipStore;
use crate::colour::{Grade, Scenario};
use crate::ids::{AssetId, ClipId, ProjectId, ScenarioId, SequenceId, TrackId};
use crate::props::ClipProps;
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
    /// Groups of assets that are matched and given a look together.
    pub scenarios: Arc<BTreeMap<ScenarioId, Arc<Scenario>>>,
    /// Words kept with the project: lyrics, ideas, what is left to do.
    /// (Shared: every other edit copies the project, not this text.)
    pub lyrics: Arc<str>,
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
    /// The song this sequence is cut to, if any.
    pub song: Option<Song>,
    /// Master fader in decibels.
    pub master_db: f32,
    /// The tempo of the music made in this sequence: with one, the
    /// timeline counts bars and beats (09-daw.md).
    pub tempo: Option<Tempo>,
    /// Sorted by frame, at most one per frame.
    pub markers: Arc<Vec<Marker>>,
    /// The room and the echo the tracks of this sequence send to.
    pub space: Space,
    /// The groups A to D: tracks that name one are summed and go through
    /// its fader together before the master.
    pub groups: [Group; GROUPS],
}

/// How many groups a sequence has.
pub const GROUPS: usize = 4;

/// The stages a track's sound goes through before its plugins and fader,
/// by number: 0 low cut, 1 noise reduction, 2 tune, 3 equaliser, 4
/// de-esser, 5 leveller, 6 compressor, 7 saturation.
pub const STAGES: usize = 8;
/// The order they come in unless the track says otherwise.
pub const STAGE_ORDER: [u8; STAGES] = [0, 1, 2, 3, 4, 5, 6, 7];

/// The fader of a group of tracks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub gain_db: f32,
    pub mute: bool,
}

impl Group {
    pub fn clamped(self) -> Group {
        Group { gain_db: if self.gain_db.is_finite() { self.gain_db.clamp(-60.0, 12.0) } else { 0.0 }, ..self }
    }
}

/// How the shared reverb and echo of a sequence sound (09-daw.md, M15).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Space {
    /// How long the room rings, in tenths of a second (3 to 80).
    pub room_ds: u8,
    /// The time between the echo's repeats as a note value.
    pub echo_note: EchoNote,
    /// How much of each repeat is repeated again, in percent (0 to 85).
    pub echo_repeat: u8,
}

/// A note value for the echo; a beat is a quarter note.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EchoNote {
    Half,
    Quarter,
    DottedEighth,
    Eighth,
    Sixteenth,
}

impl EchoNote {
    pub const ALL: [EchoNote; 5] =
        [EchoNote::Half, EchoNote::Quarter, EchoNote::DottedEighth, EchoNote::Eighth, EchoNote::Sixteenth];

    /// Its length in beats, as a fraction.
    pub fn beats(self) -> (u32, u32) {
        match self {
            EchoNote::Half => (2, 1),
            EchoNote::Quarter => (1, 1),
            EchoNote::DottedEighth => (3, 4),
            EchoNote::Eighth => (1, 2),
            EchoNote::Sixteenth => (1, 4),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EchoNote::Half => "1/2",
            EchoNote::Quarter => "1/4",
            EchoNote::DottedEighth => "1/8.",
            EchoNote::Eighth => "1/8",
            EchoNote::Sixteenth => "1/16",
        }
    }

    pub fn from_label(label: &str) -> Option<EchoNote> {
        EchoNote::ALL.into_iter().find(|n| n.label() == label)
    }
}

impl Default for Space {
    fn default() -> Space {
        Space { room_ds: 15, echo_note: EchoNote::Quarter, echo_repeat: 38 }
    }
}

impl Space {
    pub fn clamped(self) -> Space {
        Space { room_ds: self.room_ds.clamp(3, 80), echo_repeat: self.echo_repeat.min(85), ..self }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub clips: ClipStore,
    /// Properties of the clips that have any, by clip id. An entry may
    /// outlive its clip (a removed clip comes back with its properties on undo).
    pub props: Arc<BTreeMap<ClipId, Arc<ClipProps>>>,
    /// Mixer settings (audio tracks).
    pub mix: TrackSettings,
    /// The plugins on this track (audio tracks), in the order its sound
    /// runs through them. An instrument among them plays the notes of the
    /// track's note clips, before the strip's own stages; the others are
    /// effects after them.
    pub plugins: Arc<Vec<PluginRef>>,
    /// The recording the built-in instrument plays for this track's
    /// notes; `None`: its own tone (the built-in synthesiser).
    pub sampler: Option<Sampler>,
    /// Keys of the built-in instrument that have a recording of their
    /// own (a drum kit): (key, recording), by key. Such a key plays its
    /// recording as it is and to its end; the other keys play the
    /// sampler's recording or the synthesiser.
    pub pads: Arc<Vec<(u8, AssetId)>>,
    /// The notes of the note clips (clips without an asset on an audio
    /// track), by clip id; like `props`, an entry may outlive its clip.
    pub notes: Arc<BTreeMap<ClipId, Arc<Vec<Note>>>>,
}

/// The built-in sampler of a track: its notes play a recording of the
/// project, higher or lower with the key (09-daw.md, "Sound").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sampler {
    pub asset: AssetId,
    /// The key at which the recording plays as it is (60: middle C).
    pub root: u8,
    /// A hit (a drum, a stab): every note plays the recording to its end.
    pub one_shot: bool,
}

/// A plugin installed on the machine. Not media: the one thing a project
/// names that is not inside its bundle (09-daw.md). Found again by its
/// path, and by its name if it has moved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRef {
    pub name: String,
    pub path: String,
    /// Tells the plugins of a track apart (the same one can be on it
    /// twice) and names the file its settings are kept in. 0: the only
    /// plugin of a track written before a track could have several.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub id: u64,
}

fn is_zero(id: &u64) -> bool {
    *id == 0
}

impl PluginRef {
    /// What its running instance is known by in the engine.
    pub fn key(&self, track: TrackId) -> u64 {
        if self.id == 0 { track.0 } else { self.id }
    }

    /// The file in the project's `plugins` folder its settings are kept in.
    pub fn state_file(&self, track: TrackId) -> String {
        if self.id == 0 { format!("{track}.state") } else { format!("{track}-{:016x}.state", self.id) }
    }
}

/// Ticks to a beat: note positions are whole numbers of these, so that
/// they follow the tempo.
pub const TICKS_PER_BEAT: i64 = 960;

/// A note in a note clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Ticks from the start of the clip, and how long it sounds.
    pub start: i64,
    pub len: i64,
    /// MIDI note number: 60 is middle C.
    pub pitch: u8,
    /// 1 (softest) to 127.
    pub velocity: u8,
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
        Project {
            id,
            name: name.into(),
            sequences: Vec::new(),
            assets: Arc::default(),
            scenarios: Arc::default(),
            lyrics: Arc::from(""),
        }
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
            master_db: 0.0,
            tempo: None,
            song: None,
            markers: Arc::default(),
            space: Space::default(),
            groups: [Group::default(); GROUPS],
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
        Track {
            id: h.id,
            kind: h.kind,
            name: h.name,
            clips: ClipStore::new(),
            props: Arc::default(),
            mix: TrackSettings::default(),
            plugins: Arc::default(),
            sampler: None,
            pads: Arc::default(),
            notes: Arc::default(),
        }
    }
}

/// A named point on a sequence's timeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub frame: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Track {
    /// The properties of a clip, if it has any that differ from the defaults.
    pub fn props_of(&self, clip: ClipId) -> Option<&Arc<ClipProps>> {
        self.props.get(&clip)
    }
}

/// Mixer settings of a track. The default is a track that changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackSettings {
    /// Fader in decibels.
    pub gain_db: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    pub mute: bool,
    /// While any track is soloed, only soloed tracks are heard.
    pub solo: bool,
    /// Low cut in hertz; 0 is off.
    pub low_cut_hz: f32,
    /// Video tracks: not shown in the picture (a camera angle kept as a
    /// source for the programme track).
    pub hidden: bool,
    /// Noise reduction, 0 (off) to 1 (only the treated sound).
    pub denoise: f32,
    /// Equaliser: gain in decibels of the low shelf (120 Hz), the bell at
    /// `eq_mid_hz` and the high shelf (8 kHz).
    pub eq_low_db: f32,
    pub eq_mid_db: f32,
    pub eq_mid_hz: f32,
    pub eq_high_db: f32,
    /// Compressor: the level in decibels below full scale above which it
    /// turns the sound down (0 is off), and by what ratio.
    pub comp_threshold_db: f32,
    pub comp_ratio: f32,
    /// What the compressor gives back after it, in decibels (0 to 24):
    /// turning the loud parts down makes the whole quieter.
    pub comp_makeup_db: f32,
    /// De-esser: how much the S sounds of a voice are turned down, 0 (off)
    /// to 1 (by up to 12 dB). It finds them itself.
    pub deess: f32,
    /// Pitch correction: how much (0 off, 1 all the way onto the scale),
    /// the root of the key (0 is C ... 11 is B), the scale (0 every
    /// semitone, 1 major, 2 minor) and how fast a note is pulled into
    /// place (0 at once, 1 gently).
    pub tune: f32,
    pub tune_key: u8,
    pub tune_scale: u8,
    pub tune_speed: f32,
    /// Leveller, 0 to 1: phrases are pulled towards the track's average level.
    pub level: f32,
    /// Saturation, 0 to 1.
    pub drive: f32,
    /// A voice track: tracks with `duck_db` go down while it sounds.
    pub voice: bool,
    /// How far this track goes down under the voice tracks, in decibels.
    pub duck_db: f32,
    /// How much of the track is sent to the sequence's reverb and to its
    /// echo (repeats in time with the tempo), 0 (none) to 1.
    pub reverb: f32,
    pub echo: f32,
    /// The group this track's sound goes through on its way to the
    /// master: 0 none, 1 to 4 the groups A to D of the sequence.
    pub group: u8,
    /// The order the sound goes through the stages (feedback F16.22):
    /// each of the numbers of [`STAGES`] once.
    pub order: [u8; STAGES],
}

impl Default for TrackSettings {
    fn default() -> TrackSettings {
        TrackSettings {
            gain_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            low_cut_hz: 0.0,
            hidden: false,
            denoise: 0.0,
            eq_low_db: 0.0,
            eq_mid_db: 0.0,
            eq_mid_hz: 1000.0,
            eq_high_db: 0.0,
            comp_threshold_db: 0.0,
            comp_ratio: 3.0,
            comp_makeup_db: 0.0,
            deess: 0.0,
            tune: 0.0,
            tune_key: 0,
            tune_scale: 0,
            tune_speed: 0.2,
            level: 0.0,
            drive: 0.0,
            voice: false,
            duck_db: 0.0,
            reverb: 0.0,
            echo: 0.0,
            group: 0,
            order: STAGE_ORDER,
        }
    }
}

impl TrackSettings {
    pub fn is_default(&self) -> bool {
        *self == TrackSettings::default()
    }

    /// Values a hand-edited file or a script may not have kept in range.
    pub fn clamped(self) -> TrackSettings {
        let finite = |v: f32, low: f32, high: f32| if v.is_finite() { v.clamp(low, high) } else { 0.0 };
        TrackSettings {
            gain_db: finite(self.gain_db, -60.0, 12.0),
            pan: finite(self.pan, -1.0, 1.0),
            low_cut_hz: finite(self.low_cut_hz, 0.0, 1000.0),
            denoise: finite(self.denoise, 0.0, 1.0),
            eq_low_db: finite(self.eq_low_db, -18.0, 18.0),
            eq_mid_db: finite(self.eq_mid_db, -18.0, 18.0),
            eq_mid_hz: if self.eq_mid_hz.is_finite() { self.eq_mid_hz.clamp(100.0, 10_000.0) } else { 1000.0 },
            eq_high_db: finite(self.eq_high_db, -18.0, 18.0),
            comp_threshold_db: finite(self.comp_threshold_db, -60.0, 0.0),
            comp_ratio: if self.comp_ratio.is_finite() { self.comp_ratio.clamp(1.0, 20.0) } else { 3.0 },
            comp_makeup_db: finite(self.comp_makeup_db, 0.0, 24.0),
            deess: finite(self.deess, 0.0, 1.0),
            tune: finite(self.tune, 0.0, 1.0),
            tune_key: self.tune_key % 12,
            tune_scale: self.tune_scale.min(2),
            tune_speed: finite(self.tune_speed, 0.0, 1.0),
            level: finite(self.level, 0.0, 1.0),
            drive: finite(self.drive, 0.0, 1.0),
            duck_db: finite(self.duck_db, 0.0, 24.0),
            reverb: finite(self.reverb, 0.0, 1.0),
            echo: finite(self.echo, 0.0, 1.0),
            group: self.group.min(GROUPS as u8),
            // Every stage once, or the usual order.
            order: if (0..STAGES as u8).all(|stage| self.order.contains(&stage)) { self.order } else { STAGE_ORDER },
            ..self
        }
    }
}

/// A tempo: how fast the beats come and how many make a bar. Whole
/// numbers, like every time in the model: thousandths of a beat per minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tempo {
    pub milli_bpm: u32,
    pub beats_per_bar: u8,
    /// Where the first bar begins on the timeline, in samples at 48 kHz:
    /// 0 unless the music does not start with the timeline (a beat that
    /// was made elsewhere and begins with a pause or an upbeat).
    #[serde(default)]
    pub origin: i64,
}

impl Tempo {
    /// 120 beats a minute in four-beat bars.
    pub const DEFAULT: Tempo = Tempo { milli_bpm: 120_000, beats_per_bar: 4, origin: 0 };

    /// A tempo whose first bar begins with the timeline.
    pub const fn new(milli_bpm: u32, beats_per_bar: u8) -> Tempo {
        Tempo { milli_bpm, beats_per_bar, origin: 0 }
    }

    /// Where the first bar begins, in frames of `rate` (not a whole number in general).
    pub fn origin_frames(self, rate: crate::Rate) -> f64 {
        self.origin as f64 * rate.num() as f64 / (rate.den() as f64 * crate::time::SAMPLE_RATE as f64)
    }

    /// Brought into the range the program works with: 20 to 400 beats a
    /// minute, 1 to 16 beats to the bar.
    pub fn clamped(self) -> Tempo {
        Tempo {
            milli_bpm: self.milli_bpm.clamp(20_000, 400_000),
            beats_per_bar: self.beats_per_bar.clamp(1, 16),
            // (Within a day either way.)
            origin: self.origin.clamp(-4_147_200_000, 4_147_200_000),
        }
    }

    pub fn bpm(self) -> f64 {
        self.milli_bpm as f64 / 1000.0
    }

    /// Length of a beat in frames of `rate`. Not a whole number in general:
    /// for drawing and for the engine, never stored.
    pub fn frames_per_beat(self, rate: crate::Rate) -> f64 {
        60_000.0 / self.milli_bpm.max(1) as f64 * rate.num() as f64 / rate.den() as f64
    }

    /// Bar and beat of a frame, both counted from 1, as a ruler names them.
    pub fn bar_and_beat(self, frame: i64, rate: crate::Rate) -> (i64, i64) {
        // (Before the first bar the bars count down: 0, -1, ...)
        let beat =
            ((frame.max(0) as f64 - self.origin_frames(rate)) / self.frames_per_beat(rate) + 1e-6).floor() as i64;
        let per_bar = self.beats_per_bar.max(1) as i64;
        (beat.div_euclid(per_bar) + 1, beat.rem_euclid(per_bar) + 1)
    }
}

/// The song a sequence is cut to: the asset whose beats make the grid and
/// to which takes are synchronised.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Song {
    pub asset: AssetId,
    /// Detected tempo taken at half (-1), as found (0) or double (1): the
    /// detector cannot know whether a song counts in fast or slow beats.
    #[serde(default)]
    pub tempo: i8,
}

impl Project {
    /// The scenario an asset belongs to, if any.
    pub fn scenario_of(&self, asset: AssetId) -> Option<&Arc<Scenario>> {
        self.scenarios.values().find(|s| s.members.contains_key(&asset))
    }

    /// The colour treatment of pictures of `asset`: `None` if it has none.
    pub fn grade_of(&self, asset: AssetId) -> Option<Grade> {
        let grade = self.scenario_of(asset)?.grade(asset);
        (!grade.is_neutral()).then_some(grade)
    }
}

#[cfg(test)]
mod tempo_tests {
    use super::*;
    use crate::Rate;

    #[test]
    fn a_tempo_counts_bars_and_beats() {
        let tempo = Tempo::DEFAULT;
        // 120 a minute at 60 frames a second: a beat every 30 frames, a bar every 120.
        assert_eq!(tempo.frames_per_beat(Rate::FPS_60), 30.0);
        assert_eq!(tempo.bar_and_beat(0, Rate::FPS_60), (1, 1));
        assert_eq!(tempo.bar_and_beat(29, Rate::FPS_60), (1, 1));
        assert_eq!(tempo.bar_and_beat(30, Rate::FPS_60), (1, 2));
        assert_eq!(tempo.bar_and_beat(120, Rate::FPS_60), (2, 1));
        assert_eq!(tempo.bar_and_beat(7 * 120 + 95, Rate::FPS_60), (8, 4));
        // Three to the bar, and a tempo that does not divide the frame rate.
        // A first bar that begins two seconds in: before it the bars count down.
        let late = Tempo { origin: 96_000, ..tempo };
        assert_eq!(late.bar_and_beat(120, Rate::FPS_60), (1, 1));
        assert_eq!(late.bar_and_beat(150, Rate::FPS_60), (1, 2));
        assert_eq!(late.bar_and_beat(0, Rate::FPS_60), (0, 1));
        let waltz = Tempo::new(93_500, 3);
        assert!((waltz.frames_per_beat(Rate::FPS_60) - 38.502_67).abs() < 1e-3);
        assert_eq!(waltz.bar_and_beat(116, Rate::FPS_60), (2, 1));
        // Out of range values are brought in.
        assert_eq!(Tempo::new(1, 0).clamped(), Tempo::new(20_000, 1));
    }
}
