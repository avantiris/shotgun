//! The piano roll: the notes of one note clip, drawn and edited with the
//! mouse (docs/spec/09-daw.md). It takes the place of the viewer while a
//! note clip is open.
//!
//! Several notes can be selected (Ctrl+click adds one, Ctrl+drag draws a
//! box, Ctrl+A takes all): they move, stretch and go together, and
//! Ctrl+C / Ctrl+V repeat them after themselves.
//!
//! Every gesture (draw, move, stretch, delete) is one operation that
//! replaces the clip's notes as a whole, so undo steps back gesture by
//! gesture. While a gesture is in progress the notes shown are a working
//! copy; nothing reaches the project until the button is let go.
//!
//! The instrument of the open clip's track can be played by hand: with
//! the mouse on the keys at the left, and with the computer keyboard
//! while "Keyboard" is on (the home row is the white keys from C, the row
//! above the black ones; Z and X move an octave). What is played while
//! the timeline plays and "Record" is on is written into the clip.

use egui::{Align2, Color32, FontId, PointerButton, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::time::SAMPLE_RATE;
use std::sync::Arc;

use sg_core::{ClipId, Note, Op, Sequence, TICKS_PER_BEAT, TrackId};

use crate::session::Session;
use crate::theme;

const KEYS_W: f32 = 46.0;
const ROW_H: f32 = 17.0;
const HEAD_H: f32 = 30.0;
/// The strip above the rows with the bar numbers: a press there moves the playhead.
const RULER_H: f32 = 16.0;
/// The lane under the keys that shows how hard each note is struck.
const VELOCITY_H: f32 = 44.0;
/// Distance from a note's right end within which it is stretched, not moved.
const EDGE: f32 = 8.0;
const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

enum Gesture {
    /// A new note being drawn: its index in the working copy.
    Draw(usize),
    /// Notes being moved: the one under the pointer (its index, and the
    /// pointer's offset inside it in ticks) and all that move with it, as
    /// (index, the note as it was).
    Move(usize, i64, Vec<(usize, Note)>),
    /// Notes being stretched at their ends: the one under the pointer and all that follow it.
    Stretch(usize, Vec<(usize, Note)>),
    /// A box being drawn from this point: what it touches gets selected.
    Box(egui::Pos2),
    /// The right button is down: every note the pointer passes over goes.
    Erase,
}

#[derive(Default)]
pub struct PianoRoll {
    /// The clip whose notes are shown.
    open: Option<(TrackId, ClipId)>,
    /// The highest pitch in view.
    top: Option<f32>,
    /// Grid in ticks: notes start and end on it.
    grid: Option<i64>,
    /// How far the clip is spread out along the roll (1: all of it in
    /// view), and the tick at the left edge.
    zoom: Option<f32>,
    left: f32,
    /// Height of a key's row in points (Alt + wheel).
    row_h: Option<f32>,
    /// A drag on empty space draws a box that selects, not a note.
    select: bool,
    /// The length a clicked note gets: that of the last one drawn or stretched.
    last_len: Option<i64>,
    /// The selected notes, as what they are (their places change when the notes are sorted).
    selected: Vec<Note>,
    /// Notes copied with Ctrl+C, their starts counted from the beat the
    /// first began in, and how many ticks (whole beats) they span.
    copied: Vec<Note>,
    copied_span: i64,
    gesture: Option<Gesture>,
    /// The notes while a gesture is in progress.
    working: Option<Vec<Note>>,
    /// The computer keyboard plays the instrument.
    keys: bool,
    /// What is played while the timeline plays is written into the clip.
    record: bool,
    /// Octave of the keyboard's first C, as a MIDI octave (5: middle C).
    octave: Option<u8>,
    /// Keys held on the computer keyboard and the pitch each started.
    held: Vec<(egui::Key, u8)>,
    /// The pitch sounding from the mouse: a key at the left held down, or
    /// a note just placed (until the time given).
    mouse: Option<(u8, Option<f64>)>,
    /// Notes to start or stop on the instrument; taken by the shell.
    pub played: Vec<Played>,
    /// The playhead should go to this sequence frame (a press or a drag in
    /// the ruler); taken by the editor.
    pub seek_request: Option<i64>,
    /// The undo group of the pull through the velocity lane that is in progress.
    velocity_group: Option<u64>,
    /// The notes of the open clip as a MIDI file to be written: a name
    /// for it and its bytes; taken by the shell.
    pub midi_request: Option<(String, Vec<u8>)>,
    /// Notes being recorded: pitch, the timeline sample its key went down at, and how hard.
    sounding: Vec<(u8, i64, u8)>,
    /// Undo group of the recording pass in progress.
    pass: Option<u64>,
}

/// A key going down or up on the instrument played by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Played {
    pub on: bool,
    pub pitch: u8,
    pub velocity: u8,
}

/// The computer keyboard as a piano, by position (named as on the US
/// layout): semitones above the C the home row starts on.
const PIANO_KEYS: [(egui::Key, u8); 17] = {
    use egui::Key;
    [
        (Key::A, 0),
        (Key::W, 1),
        (Key::S, 2),
        (Key::E, 3),
        (Key::D, 4),
        (Key::F, 5),
        (Key::T, 6),
        (Key::G, 7),
        (Key::Y, 8),
        (Key::H, 9),
        (Key::U, 10),
        (Key::J, 11),
        (Key::K, 12),
        (Key::O, 13),
        (Key::L, 14),
        (Key::P, 15),
        (Key::Semicolon, 16),
    ]
};

/// The notes of a clip as its piano roll shows them, and those it does
/// not show. A clip that was cut or trimmed at its start begins `offset`
/// ticks into its notes, and is `length` ticks long: notes are shown
/// from the clip's start, one that lies across the start begins there,
/// and what lies outside the clip altogether is kept but not shown.
fn in_view(raw: &[Note], offset: i64, length: i64) -> (Vec<Note>, Vec<Note>) {
    let (mut shown, mut hidden) = (Vec::new(), Vec::new());
    for note in raw {
        let (start, end) = (note.start - offset, note.start - offset + note.len);
        if end <= 0 || start >= length {
            hidden.push(*note);
        } else {
            shown.push(Note { start: start.max(0), len: end - start.max(0), ..*note });
        }
    }
    (shown, hidden)
}

/// How many ticks into its notes a clip begins, and how many ticks long it is.
fn span_of(sequence: &Sequence, clip: &sg_core::Clip) -> (i64, i64) {
    let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
    let samples_per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * TICKS_PER_BEAT as f64);
    let length = ((clip.len as f64 / samples_per_tick).round() as i64).max(TICKS_PER_BEAT);
    ((clip.src_in as f64 / samples_per_tick).round() as i64, length)
}

/// How far from the clip's start the track is free, in ticks and in
/// samples: up to the next clip on it, or without end.
fn room_of(sequence: &Sequence, track: &sg_core::Track, clip: &sg_core::Clip) -> (i64, i64) {
    let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
    let samples_per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * TICKS_PER_BEAT as f64);
    match track.clips.first_at_or_after(clip.end()) {
        Some(next) => {
            let samples = next.start - clip.start;
            ((samples as f64 / samples_per_tick).floor() as i64, samples)
        }
        None => (i64::MAX / 4, i64::MAX / 4),
    }
}

/// The pitch whose row lies `down` points below the top of the lanes,
/// when `top` is the pitch drawn at their top edge. The row of pitch p
/// spans (top - p) to (top - p + 1) rows down, and `top` is seldom a
/// whole number once the roll was scrolled: rounding the rows down and
/// then the pitch down put the lower part of every row onto the note
/// below (feedback F16.6).
fn pitch_under(top: f32, row_h: f32, down: f32) -> u8 {
    (top - down / row_h).ceil().clamp(0.0, 127.0) as u8
}

fn name_of(pitch: u8) -> String {
    format!("{}{}", NAMES[pitch as usize % 12], pitch as i32 / 12 - 1)
}

impl PianoRoll {
    pub fn show(&mut self, track: TrackId, clip: ClipId) {
        let mut played = self.silence();
        played.append(&mut self.played);
        *self = PianoRoll {
            open: Some((track, clip)),
            grid: self.grid,
            row_h: self.row_h,
            select: self.select,
            last_len: self.last_len,
            copied: std::mem::take(&mut self.copied),
            copied_span: self.copied_span,
            keys: self.keys,
            octave: self.octave,
            played,
            ..PianoRoll::default()
        };
    }

    /// Where the open clip lies on the timeline: its first sample and the one after its last.
    pub fn span(&self, session: &Session) -> Option<(i64, i64)> {
        let (track, clip) = self.open?;
        let project = session.project();
        let clip = project.sequences.first()?.track(track)?.clips.iter().find(|c| c.id == clip)?;
        Some((clip.start, clip.start + clip.len))
    }

    /// The track whose instrument is played by hand: that of the open clip.
    pub fn hand(&self) -> Option<TrackId> {
        self.open.map(|(track, _)| track)
    }

    /// Whether the computer keyboard plays notes now.
    pub fn takes_keys(&self) -> bool {
        self.open.is_some() && self.keys
    }

    /// Switches the computer keyboard as a piano on or off.
    pub fn set_keyboard(&mut self, on: bool) {
        self.keys = on;
    }

    /// A key going down or up as if played by hand (scripted runs).
    pub fn play(&mut self, pitch: u8, on: bool) {
        if self.open.is_some() {
            self.played.push(Played { on, pitch, velocity: if on { 100 } else { 0 } });
        }
    }

    pub fn toggle_record(&mut self) {
        self.record = !self.record;
        self.sounding.clear();
        self.pass = None;
    }

    /// Lets go of everything that is held: the notes to stop.
    fn silence(&mut self) -> Vec<Played> {
        let mut stops: Vec<Played> =
            self.held.drain(..).map(|(_, pitch)| Played { on: false, pitch, velocity: 0 }).collect();
        stops.extend(self.mouse.take().map(|(pitch, _)| Played { on: false, pitch, velocity: 0 }));
        stops
    }

    /// Takes the presses and releases of the piano keys out of this
    /// frame's input (so that they are not shortcuts as well) and turns
    /// them into notes. Only while "Keyboard" is on and no text is being typed.
    pub fn take_keys(&mut self, ctx: &egui::Context) {
        if !self.takes_keys() || ctx.text_edit_focused() {
            return;
        }
        // A window that is no longer in front gets no releases: let go of everything.
        if !ctx.input(|i| i.focused) {
            let stops = self.silence();
            self.played.extend(stops);
            return;
        }
        let base = self.octave.unwrap_or(5) * 12;
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::Key { key, physical_key, pressed, repeat, modifiers } = event else { return true };
                if modifiers.ctrl || modifiers.alt || modifiers.shift || modifiers.command {
                    return true;
                }
                let key = physical_key.unwrap_or(*key);
                if matches!(key, egui::Key::Z | egui::Key::X) {
                    if *pressed && !*repeat {
                        let octave = self.octave.unwrap_or(5);
                        self.octave = Some(if key == egui::Key::Z { octave.max(1) - 1 } else { (octave + 1).min(8) });
                    }
                    return false;
                }
                let Some(&(_, semitone)) = PIANO_KEYS.iter().find(|(k, _)| *k == key) else { return true };
                if *pressed && !*repeat && !self.held.iter().any(|(k, _)| *k == key) {
                    let pitch = (base + semitone).min(127);
                    self.held.push((key, pitch));
                    self.played.push(Played { on: true, pitch, velocity: 100 });
                } else if !*pressed && let Some(at) = self.held.iter().position(|(k, _)| *k == key) {
                    // (The pitch it started, whatever the octave is by now.)
                    let (_, pitch) = self.held.remove(at);
                    self.played.push(Played { on: false, pitch, velocity: 0 });
                }
                false
            });
        });
    }

    /// The application says where on the timeline a played key was heard
    /// (`at`, in timeline samples; only while something plays). With
    /// "Record" on, the note is written into the open clip when its key is
    /// let go: where it was played, as long as it was held.
    pub fn heard(
        &mut self,
        session: &mut Session,
        on: bool,
        pitch: u8,
        velocity: u8,
        at: i64,
        error: &mut Option<String>,
    ) {
        if !self.record {
            return;
        }
        if on {
            self.sounding.retain(|(p, ..)| *p != pitch);
            self.sounding.push((pitch, at, velocity.clamp(1, 127)));
            return;
        }
        let Some(index) = self.sounding.iter().position(|(p, ..)| *p == pitch) else { return };
        let (_, began, struck) = self.sounding.remove(index);
        let Some((track_id, clip_id)) = self.open else { return };
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return };
        let Some(track) = sequence.track(track_id) else { return };
        let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) else { return };
        let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
        let samples_per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * TICKS_PER_BEAT as f64);
        let length = ((clip.len as f64 / samples_per_tick).round() as i64).max(TICKS_PER_BEAT);
        let start = ((began - clip.start) as f64 / samples_per_tick).round() as i64;
        let end = ((at - clip.start) as f64 / samples_per_tick).round() as i64;
        // (What was played before or after the clip is not part of it.)
        if start < 0 || start >= length || end <= start {
            return;
        }
        let note = Note {
            start,
            len: (end - start).max(TICKS_PER_BEAT / 16).min(length - start).max(1),
            pitch,
            velocity: struck,
        };
        let mut notes: Vec<Note> = track.notes.get(&clip_id).map(|n| Vec::clone(n)).unwrap_or_default();
        notes.push(Note { start: note.start + span_of(sequence, &clip).0, ..note });
        // One pass of playing is one undo step.
        let group = *self.pass.get_or_insert_with(|| session.new_group());
        let op = Op::SetClipNotes { seq: sequence.id, track: track_id, clip: clip_id, notes: Some(notes) };
        if let Err(e) = session.apply(op, Some(group)) {
            *error = Some(e.to_string());
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Closes it. Returns whether it was open.
    pub fn close(&mut self) -> bool {
        self.working = None;
        self.gesture = None;
        let stops = self.silence();
        self.played.extend(stops);
        self.sounding.clear();
        self.pass = None;
        self.open.take().is_some()
    }

    /// The notes of the open clip, and what is needed to change them.
    fn stored(&self, session: &Session) -> Option<(Arc<sg_core::Project>, TrackId, ClipId, Vec<Note>, i64)> {
        let (track_id, clip_id) = self.open?;
        let project = session.project().clone();
        let sequence = project.sequences.first()?;
        let track = sequence.track(track_id)?;
        let clip = track.clips.iter().find(|c| c.id == clip_id)?;
        let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
        let samples_per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * TICKS_PER_BEAT as f64);
        let length = ((clip.len as f64 / samples_per_tick).round() as i64).max(TICKS_PER_BEAT);
        let raw = track.notes.get(&clip_id).map(|n| Vec::clone(n)).unwrap_or_default();
        let (notes, _) = in_view(&raw, span_of(sequence, &clip).0, length);
        Some((project, track_id, clip_id, notes, length))
    }

    /// Removes the selected notes. Returns whether there were any: the
    /// delete key means the notes while the piano roll is open.
    pub fn delete_selected(&mut self, session: &mut Session, error: &mut Option<String>) -> bool {
        let gone = std::mem::take(&mut self.selected);
        let Some((project, track, clip, mut notes, _)) = self.stored(session).filter(|_| !gone.is_empty()) else {
            return false;
        };
        for note in &gone {
            if let Some(index) = notes.iter().position(|n| n == note) {
                notes.remove(index);
            }
        }
        if let Some(sequence) = project.sequences.first() {
            Self::commit(session, sequence, track, clip, notes, error);
        }
        true
    }

    pub fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }

    /// Moves the selected notes up or down the keys and along the time
    /// (the arrow keys), all by the same amount or, if one of them would
    /// leave the keyboard or go before the clip's start, not at all.
    /// One undo step. Returns whether notes were selected.
    pub fn shift_selected(
        &mut self,
        session: &mut Session,
        semitones: i8,
        ticks: i64,
        error: &mut Option<String>,
    ) -> bool {
        let Some((project, track, clip, mut notes, _)) = self.stored(session).filter(|_| !self.selected.is_empty())
        else {
            return false;
        };
        let fits = |n: &Note| (0..=127).contains(&(n.pitch as i16 + semitones as i16)) && n.start + ticks >= 0;
        if !self.selected.iter().all(fits) {
            return true;
        }
        let shifted =
            |n: &Note| Note { pitch: (n.pitch as i16 + semitones as i16) as u8, start: n.start + ticks, ..*n };
        let mut moved = Vec::with_capacity(self.selected.len());
        for note in &self.selected {
            if let Some(index) = notes.iter().position(|n| n == note) {
                notes[index] = shifted(note);
                moved.push(notes[index]);
            }
        }
        self.selected = moved;
        if let Some(sequence) = project.sequences.first() {
            Self::commit(session, sequence, track, clip, notes, error);
        }
        true
    }

    /// Selects every note of the clip (Ctrl+A).
    pub fn select_all(&mut self, session: &Session) {
        if let Some((.., notes, _)) = self.stored(session) {
            self.selected = notes;
        }
    }

    /// Remembers the selected notes for pasting (Ctrl+C). Returns whether there were any.
    pub fn copy(&mut self) -> bool {
        let Some(first) = self.selected.iter().map(|n| n.start).min() else { return false };
        // Counted from the beat the first one began in, and spanning whole
        // beats: pasted after themselves, a bar repeats as a bar.
        let from = first.div_euclid(TICKS_PER_BEAT) * TICKS_PER_BEAT;
        let end = self.selected.iter().map(|n| n.start + n.len).max().unwrap_or(from);
        self.copied = self.selected.iter().map(|n| Note { start: n.start - from, ..*n }).collect();
        self.copied_span = ((end - from) as f64 / TICKS_PER_BEAT as f64).ceil().max(1.0) as i64 * TICKS_PER_BEAT;
        true
    }

    /// Puts the copied notes after the selected ones (after where they were
    /// copied from, the first time; after the last paste, the next), and
    /// selects them. Past the clip's end they make the clip longer; what
    /// would lie in the next clip on the track is left out.
    /// Returns whether there was something to paste.
    pub fn paste(&mut self, session: &mut Session, error: &mut Option<String>) -> bool {
        if self.copied.is_empty() {
            return false;
        }
        let Some((project, track, clip, mut notes, length)) = self.stored(session) else { return false };
        let room = project.sequences.first().and_then(|sequence| {
            let on = sequence.track(track)?;
            let found = on.clips.iter().find(|c| c.id == clip)?;
            Some(room_of(sequence, on, &found).0)
        });
        let length = room.unwrap_or(length).max(length);
        let after = self
            .selected
            .iter()
            .map(|n| n.start)
            .min()
            .map_or(0, |first| first.div_euclid(TICKS_PER_BEAT) * TICKS_PER_BEAT + self.copied_span);
        let pasted: Vec<Note> = self
            .copied
            .iter()
            .map(|n| Note { start: n.start + after, ..*n })
            .filter(|n| n.start < length)
            .map(|n| Note { len: n.len.min(length - n.start), ..n })
            .collect();
        if pasted.is_empty() {
            *error = Some("There is no room after these notes: the next clip on the track is in the way.".to_owned());
            return true;
        }
        notes.extend(pasted.iter().copied());
        if let Some(sequence) = project.sequences.first() {
            Self::commit(session, sequence, track, clip, notes, error);
        }
        self.selected = pasted;
        true
    }

    fn commit(
        session: &mut Session,
        sequence: &Sequence,
        track: TrackId,
        clip: ClipId,
        notes: Vec<Note>,
        error: &mut Option<String>,
    ) {
        Self::commit_in(session, sequence, track, clip, notes, None, error);
    }

    /// [`PianoRoll::commit`] as part of an undo group, if one is given.
    fn commit_in(
        session: &mut Session,
        sequence: &Sequence,
        track: TrackId,
        clip: ClipId,
        notes: Vec<Note>,
        group: Option<u64>,
        error: &mut Option<String>,
    ) {
        // `notes` are as the roll shows them: put back where they lie in
        // the clip's notes, beside those the clip does not reach.
        let found = sequence.track(track).and_then(|t| {
            let raw = t.notes.get(&clip).map(|n| Vec::clone(n)).unwrap_or_default();
            let (offset, length) = span_of(sequence, t.clips.iter().find(|c| c.id == clip).as_ref()?);
            // Notes that begin before the clip does are shown from its start:
            // as shown, and as they really are.
            let across: Vec<(Note, Note)> = raw
                .iter()
                .filter(|n| n.start < offset && n.start + n.len > offset)
                .map(|n| (Note { start: 0, len: n.start + n.len - offset, ..*n }, *n))
                .collect();
            // Notes written after the clip's end make it longer: to the
            // end of their bar, as far as the track is free (feedback F16.1).
            let found = t.clips.iter().find(|c| c.id == clip)?;
            let bar = TICKS_PER_BEAT * sequence.tempo.map_or(4, |tempo| tempo.beats_per_bar as i64);
            let needed = notes.iter().map(|n| n.start + n.len).max().unwrap_or(0);
            let longer = (needed > length).then(|| {
                let (room, room_samples) = room_of(sequence, t, &found);
                let ticks = ((needed + bar - 1) / bar * bar).min(room).max(needed.min(room));
                let samples = (ticks as f64 * found.len as f64 / length as f64).round() as i64;
                sg_core::Clip { len: samples.min(room_samples).max(found.len), ..found }
            });
            Some((offset, in_view(&raw, offset, length).1, across, longer))
        });
        let (offset, mut all, mut across, longer) = found.unwrap_or((0, Vec::new(), Vec::new(), None));
        for note in notes {
            // (One that was not touched keeps the part of it the clip does not show.)
            match across.iter().position(|(shown, _)| *shown == note) {
                Some(at) => all.push(across.remove(at).1),
                None => all.push(Note { start: note.start + offset, ..note }),
            }
        }
        let mut op = Op::SetClipNotes { seq: sequence.id, track, clip, notes: Some(all) };
        if let Some(longer) = longer {
            let grow = Op::SetClip { seq: sequence.id, track, start: longer.start, clip: longer };
            op = Op::Batch(vec![grow, op]);
        }
        if let Err(e) = session.apply(op, group) {
            *error = Some(e.to_string());
        }
    }

    /// Draws the piano roll into the whole of `ui`. `playhead` is in
    /// sequence frames.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        playhead: i64,
        playing: bool,
        error: &mut Option<String>,
    ) {
        let Some((track_id, clip_id)) = self.open else { return };
        if !playing {
            // The next pass of recording is an undo step of its own.
            self.pass = None;
            self.sounding.clear();
        }
        let project = session.project().clone();
        let found = project.sequences.first().and_then(|sequence| {
            let track = sequence.track(track_id)?;
            let clip = track.clips.iter().find(|c| c.id == clip_id)?;
            Some((sequence, track, clip))
        });
        // The clip is gone (deleted, or an undo): nothing to show any more.
        let Some((sequence, track, clip)) = found else {
            self.close();
            return;
        };
        let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
        let beats_per_bar = sequence.tempo.map_or(4, |t| t.beats_per_bar as i64);
        let samples_per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * TICKS_PER_BEAT as f64);
        let length = ((clip.len as f64 / samples_per_tick).round() as i64).max(TICKS_PER_BEAT);
        let raw: Vec<Note> = track.notes.get(&clip_id).map(|n| Vec::clone(n)).unwrap_or_default();
        let (stored, _) = in_view(&raw, span_of(sequence, &clip).0, length);
        let grid = *self.grid.get_or_insert(TICKS_PER_BEAT / 4);
        // The roll goes on after the clip's end, as far as the track is
        // free (and not without end: sixteen bars at a time). What is
        // written there makes the clip longer.
        let extent = room_of(sequence, track, &clip).0.min(length + 16 * TICKS_PER_BEAT * beats_per_bar).max(length);
        let row_h = self.row_h.unwrap_or(ROW_H).clamp(6.0, 30.0);

        let area = ui.available_rect_before_wrap();
        // The line above the grid: what this is, the grid, and the way out.
        let head = Rect::from_min_size(area.min, vec2(area.width(), HEAD_H));
        let mut close = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(head.shrink2(vec2(4.0, 2.0))), |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(RichText::new(format!("NOTES  {}", track.name)).size(11.0).strong().color(theme::DIM));
                ui.add_space(12.0);
                ui.label(RichText::new("Grid").color(theme::DIM));
                for (label, ticks) in
                    [("1/4", 960), ("1/8", 480), ("1/8T", 320), ("1/16", 240), ("1/16T", 160), ("1/32", 120)]
                {
                    if ui.selectable_label(grid == ticks, label).clicked() {
                        self.grid = Some(ticks);
                    }
                }
                ui.add_space(12.0);
                let select = ui.selectable_label(self.select, "Select");
                if select
                    .on_hover_text(
                        "A drag on empty space draws a box and selects the notes it touches, and a click there \
                         selects none; no note is made. Off: a drag or a click draws a note, and Ctrl + drag \
                         draws the box.",
                    )
                    .clicked()
                {
                    self.select = !self.select;
                }
                let keys = ui.selectable_label(self.keys, "Keyboard");
                let octave = self.octave.unwrap_or(5);
                if keys
                    .on_hover_text(format!(
                        "Play the instrument with the computer keyboard: the home row (A S D F ...) is the white \
                         keys from C{}, the row above (W E T ...) the black ones; Z and X move an octave. While \
                         this is on, those keys are not shortcuts.",
                        octave as i32 - 1
                    ))
                    .clicked()
                {
                    self.keys = !self.keys;
                    let stops = self.silence();
                    self.played.extend(stops);
                }
                let record = ui.selectable_label(self.record, "Record");
                if record
                    .on_hover_text(
                        "While the timeline plays, what is played (keyboard or the keys at the left) is written \
                         into this clip. One pass is one undo step.",
                    )
                    .clicked()
                {
                    self.toggle_record();
                }
                if ui
                    .button("MIDI")
                    .on_hover_text("Writes the notes of this clip as a MIDI file into the project's exports folder")
                    .clicked()
                {
                    let name = sequence.track(track_id).map_or("notes".to_owned(), |t| sg_core::text::slug(&t.name));
                    let tempo = sequence.tempo.map(|tempo| tempo.milli_bpm);
                    self.midi_request = Some((name.clone(), sg_core::midi::write(&stored, tempo, &name)));
                }
                if ui
                    .button("Quantise")
                    .on_hover_text(
                        "Moves the start of every note (of the selected ones, if some are) to the nearest line \
                         of the grid",
                    )
                    .clicked()
                {
                    let mut notes = stored.clone();
                    let mut moved = Vec::new();
                    for note in &mut notes {
                        if self.selected.is_empty() || self.selected.contains(note) {
                            let start = (note.start as f64 / grid as f64).round() as i64 * grid;
                            note.start = start.clamp(0, (extent - note.len).max(0));
                            moved.push(*note);
                        }
                    }
                    if notes != stored {
                        if !self.selected.is_empty() {
                            self.selected = moved;
                        }
                        Self::commit(session, sequence, track_id, clip_id, notes, error);
                    }
                }
                if let Some(first) = self.selected.first().copied().filter(|n| stored.contains(n)) {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Velocity").color(theme::DIM));
                    let mut velocity = first.velocity as i32;
                    let field = egui::DragValue::new(&mut velocity).range(1..=127).speed(0.5);
                    if ui.add(field).on_hover_text("How hard the selected notes are struck, 1 to 127").changed() {
                        let velocity = velocity.clamp(1, 127) as u8;
                        let mut notes = stored.clone();
                        for note in notes.iter_mut().filter(|n| self.selected.contains(n)) {
                            note.velocity = velocity;
                        }
                        self.selected.iter_mut().for_each(|n| n.velocity = velocity);
                        Self::commit(session, sequence, track_id, clip_id, notes, error);
                    }
                }
                ui.add_space(12.0);
                // (Short: the head must have room for its buttons and "Close" in a narrow roll.)
                ui.label(RichText::new("?").color(theme::DIM)).on_hover_text(
                    "Click: a note. Drag: draw one, move one, or stretch it at its end. Right button: removes \
                     every note it is drawn over. A note after the clip's end makes the clip longer.\n\
                     Ctrl + click adds to the selection, Ctrl + drag (or \"Select\") draws a box, Ctrl + A takes \
                     all; Ctrl + C and Ctrl + V repeat the selected notes after themselves.\n\
                     Arrow keys move the selected notes: up and down a semitone (Shift: an octave), left and \
                     right a sixteenth (Shift: a beat).\n\
                     Wheel: up and down the keys. Shift + wheel: along the time. Ctrl + wheel: zoom along the \
                     time. Alt + wheel: higher or lower rows. Middle button: drag the view.\n\
                     A press or a drag in the strip with the bar numbers moves the playhead.",
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").on_hover_text("Back to the picture (Esc)").clicked() {
                        close = true;
                    }
                });
            });
        });
        // Under the keys, if the roll is high enough for both: the velocities.
        let lane_h = if area.height() > HEAD_H + RULER_H + VELOCITY_H + 120.0 { VELOCITY_H } else { 0.0 };
        let body =
            Rect::from_min_max(pos2(area.left(), area.top() + HEAD_H), pos2(area.right(), area.bottom() - lane_h));
        let velocities = (lane_h > 0.0).then(|| Rect::from_min_max(pos2(area.left(), body.bottom()), area.max));
        let response = ui.allocate_rect(body, Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, "Piano roll"));
        let lanes = Rect::from_min_max(pos2(body.left() + KEYS_W, body.top() + RULER_H), body.max);
        let ruler = Rect::from_min_max(pos2(lanes.left(), body.top()), pos2(body.right(), lanes.top()));
        let rows = (lanes.height() / row_h).floor().max(1.0);
        // Starts around the notes there are, or around middle C.
        let top = *self.top.get_or_insert_with(|| {
            let highest = stored.iter().map(|n| n.pitch as f32).fold(f32::MIN, f32::max);
            if stored.is_empty() { 60.0 + rows / 2.0 } else { highest + rows / 3.0 }.clamp(rows, 127.0)
        });
        // The wheel moves up and down the keys; with Shift along the time;
        // with Ctrl it zooms along the time around the pointer (as on the
        // timeline); with Alt the rows get higher. Zoom 1 is the clip filling the roll.
        let widest = (length as f32 / extent as f32).min(1.0);
        // (At first with a bar of what comes after the clip in view: it shows that there is more.)
        let bar = (TICKS_PER_BEAT * beats_per_bar) as f32;
        let zoom = self.zoom.unwrap_or(length as f32 / (length as f32 + bar)).clamp(widest, 64.0);
        let mut per_tick = lanes.width() / length as f32 * zoom;
        let mut left = self.left.clamp(0.0, (extent as f32 - lanes.width() / per_tick).max(0.0));
        let mut top = top;
        if response.dragged_by(PointerButton::Middle) {
            // The view follows the hand, both ways (as on the timeline).
            let by = response.drag_delta();
            left -= by.x / per_tick;
            top += by.y / row_h;
        }
        if response.hovered() {
            let (scroll, mods, factor) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers, i.zoom_delta()));
            let under = ui.input(|i| i.pointer.hover_pos()).map_or(lanes.center().x, |p| p.x);
            if factor != 1.0 {
                let anchor = left + (under - lanes.left()) / per_tick;
                let zoomed = (zoom * factor).clamp(widest, 64.0);
                per_tick = lanes.width() / length as f32 * zoomed;
                left = anchor - (under - lanes.left()) / per_tick;
                self.zoom = Some(zoomed);
            } else if mods.alt {
                let turned = scroll.x + scroll.y;
                self.row_h = Some((row_h * (1.0 + turned * 0.004)).clamp(6.0, 30.0));
            } else if mods.shift || scroll.x != 0.0 {
                let along = if scroll.x != 0.0 { scroll.x } else { scroll.y };
                left -= along / per_tick;
            } else {
                top += scroll.y / row_h;
            }
        }
        let left = left.clamp(0.0, (extent as f32 - lanes.width() / per_tick).max(0.0));
        let top = top.clamp(rows.min(127.0), 127.0);
        (self.left, self.top) = (left, Some(top));
        let x_of = |tick: i64| lanes.left() + (tick as f32 - left) * per_tick;
        let y_of = |pitch: f32| lanes.top() + (top - pitch) * row_h;
        let tick_at = |x: f32| ((x - lanes.left()) / per_tick + left) as i64;
        let pitch_at = |y: f32| pitch_under(top, row_h, y - lanes.top());
        // The velocity lane (feedback N2.4): a bar per note where the note
        // begins, as high as it is struck hard. A press or a drag in the
        // lane sets the bars it passes: of the selected notes if some of
        // those are there, else of all that sound there.
        if let Some(lane) = velocities {
            let bars = Rect::from_min_max(pos2(lanes.left(), lane.top() + 1.0), lane.max);
            let pressed = ui.allocate_rect(bars, Sense::click_and_drag());
            pressed.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Velocities"));
            if !pressed.is_pointer_button_down_on() {
                self.velocity_group = None;
            }
            let mut shown_notes = stored.clone();
            if pressed.is_pointer_button_down_on()
                && let Some(pos) = pressed.interact_pointer_pos()
            {
                let share = (bars.bottom() - 2.0 - pos.y) / (bars.height() - 4.0);
                let velocity = (share * 127.0).round().clamp(1.0, 127.0) as u8;
                // (Under its bar, or anywhere under the note itself.)
                let near =
                    |n: &Note| pos.x >= x_of(n.start) - 5.0 && pos.x <= x_of(n.start + n.len).max(x_of(n.start) + 6.0);
                let among_chosen = stored.iter().any(|n| near(n) && self.selected.contains(n));
                let mut changed = false;
                for note in shown_notes.iter_mut().filter(|n| near(n)) {
                    if (among_chosen && !self.selected.contains(note)) || note.velocity == velocity {
                        continue;
                    }
                    if let Some(chosen) = self.selected.iter_mut().find(|n| *n == note) {
                        chosen.velocity = velocity;
                    }
                    note.velocity = velocity;
                    changed = true;
                }
                if changed {
                    // (One pull through the lane is one step to undo.)
                    let group = *self.velocity_group.get_or_insert_with(|| session.new_group());
                    Self::commit_in(session, sequence, track_id, clip_id, shown_notes.clone(), Some(group), error);
                }
            }
            let painter = ui.painter_at(lane);
            painter.rect_filled(lane, 0.0, theme::BG);
            painter.hline(lane.x_range(), lane.top(), Stroke::new(1.0, theme::LINE));
            painter.text(
                pos2(lane.left() + 4.0, lane.center().y),
                egui::Align2::LEFT_CENTER,
                "Velocity",
                FontId::proportional(9.5),
                theme::DIM,
            );
            let painter = painter.with_clip_rect(bars);
            for note in &shown_notes {
                let x = x_of(note.start);
                let top = bars.bottom() - 2.0 - (bars.height() - 4.0) * note.velocity as f32 / 127.0;
                let colour = if self.selected.contains(note) { theme::TEXT } else { theme::ACCENT };
                painter.rect_filled(Rect::from_min_max(pos2(x, top), pos2(x + 3.0, bars.bottom() - 1.0)), 0.0, colour);
                painter.rect_filled(
                    Rect::from_min_max(pos2(x - 1.5, top - 1.0), pos2(x + 4.5, top + 2.0)),
                    0.0,
                    colour,
                );
            }
        }
        // (From here on the roll's end is what bounds a note, not the clip's.)
        let clip_length = length;
        let length = extent;
        let snap = |tick: i64| ((tick as f64 / grid as f64).round() as i64 * grid).clamp(0, length);
        let floor = |tick: i64| (tick.div_euclid(grid) * grid).clamp(0, length - grid.min(length));
        let hit = |notes: &[Note], pos: egui::Pos2| -> Option<(usize, bool)> {
            let (tick, pitch) = (tick_at(pos.x), pitch_at(pos.y));
            let index = notes.iter().rposition(|n| n.pitch == pitch && tick >= n.start && tick < n.start + n.len)?;
            let at_end = x_of(notes[index].start + notes[index].len) - pos.x <= EDGE;
            Some((index, at_end))
        };

        let pointer = ui.input(|i| i.pointer.interact_pos());
        let origin = ui.input(|i| i.pointer.press_origin());
        // The pointer says what a press does here: carry the note, or pull its end.
        let over = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| lanes.contains(*p) && response.hovered())
            .and_then(|p| hit(self.working.as_deref().unwrap_or(&stored), p));
        match (&self.gesture, over) {
            (Some(Gesture::Stretch(..) | Gesture::Draw(_)), _) | (None, Some((_, true))) => {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal)
            }
            (Some(Gesture::Move(..)), _) => ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing),
            (None, Some((_, false))) => ui.ctx().set_cursor_icon(egui::CursorIcon::Grab),
            _ => {}
        }
        // The keys at the left play the instrument while the button is down.
        let keys_column = Rect::from_min_max(pos2(body.left(), lanes.top()), pos2(lanes.left(), body.bottom()));
        let down = ui.input(|i| i.pointer.primary_down());
        // A press or a drag in the ruler moves the playhead, as on the timeline (feedback F16.8).
        if down
            && origin.is_some_and(|p| ruler.contains(p))
            && let Some(pos) = pointer
        {
            let tick = tick_at(pos.x).clamp(0, length);
            let sample = clip.start + (tick as f64 * samples_per_tick).round() as i64;
            let frame = sequence.rate.samples_to_frames(sample, SAMPLE_RATE);
            if frame != playhead {
                self.seek_request = Some(frame);
            }
        }
        if ui.rect_contains_pointer(ruler) && response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let now = ui.input(|i| i.time);
        let on_key = origin.filter(|p| down && keys_column.contains(*p)).and(pointer).map(|p| pitch_at(p.y));
        match (self.mouse, on_key) {
            (Some((pitch, None)), Some(wanted)) if pitch == wanted => {}
            (held, Some(wanted)) => {
                // (Sliding along the keys plays each in turn.)
                self.played.extend(held.map(|(pitch, _)| Played { on: false, pitch, velocity: 0 }));
                self.played.push(Played { on: true, pitch: wanted, velocity: 100 });
                self.mouse = Some((wanted, None));
            }
            (Some((pitch, until)), None) if until.is_none_or(|until| now >= until) => {
                self.played.push(Played { on: false, pitch, velocity: 0 });
                self.mouse = None;
            }
            (Some(_), None) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(30)),
            (None, None) => {}
        }
        // A note that is placed, or carried to another pitch, is heard for a moment.
        let audition = |roll: &mut PianoRoll, pitch: u8| {
            roll.played.extend(roll.mouse.take().map(|(pitch, _)| Played { on: false, pitch, velocity: 0 }));
            roll.played.push(Played { on: true, pitch, velocity: 100 });
            roll.mouse = Some((pitch, Some(now + 0.25)));
        };
        if response.drag_started_by(PointerButton::Primary)
            && let Some(origin) = origin.filter(|p| lanes.contains(*p))
        {
            let mut notes = stored.clone();
            let ctrl = ui.input(|i| i.modifiers.ctrl);
            // What moves or stretches together: the selected notes if the
            // one under the pointer is among them, else that one alone.
            let group = |index: usize, selected: &[Note]| -> Vec<(usize, Note)> {
                if selected.contains(&notes[index]) {
                    let mut left = selected.to_vec();
                    let mut group = Vec::new();
                    for (i, note) in notes.iter().enumerate() {
                        if let Some(at) = left.iter().position(|s| s == note) {
                            left.remove(at);
                            group.push((i, *note));
                        }
                    }
                    group
                } else {
                    vec![(index, notes[index])]
                }
            };
            self.gesture = Some(match hit(&notes, origin) {
                _ if ctrl => Gesture::Box(origin),
                None if self.select => Gesture::Box(origin),
                Some((index, true)) => Gesture::Stretch(index, group(index, &self.selected)),
                Some((index, false)) => {
                    Gesture::Move(index, tick_at(origin.x) - notes[index].start, group(index, &self.selected))
                }
                None => {
                    let start = floor(tick_at(origin.x));
                    notes.push(Note { start, len: grid, pitch: pitch_at(origin.y), velocity: 100 });
                    audition(self, pitch_at(origin.y));
                    Gesture::Draw(notes.len() - 1)
                }
            });
            self.working = Some(notes);
        }
        let mut carried = None;
        if response.dragged_by(PointerButton::Primary)
            && let (Some(pos), Some(notes), Some(gesture)) = (pointer, &mut self.working, &self.gesture)
        {
            match gesture {
                Gesture::Draw(index) if *index < notes.len() => {
                    // (A drawn note keeps the start it was given when the drag began.)
                    let start = notes[*index].start;
                    let end = snap(tick_at(pos.x)).max(start + grid).min(length);
                    notes[*index].len = (end - start).max(grid.min(length));
                }
                Gesture::Stretch(index, group) if *index < notes.len() => {
                    // The one under the pointer follows it; the others grow or shrink by as much.
                    let was = group.iter().find(|(i, _)| i == index).map_or(notes[*index], |(_, n)| *n);
                    let end = snap(tick_at(pos.x)).max(was.start + grid).min(length);
                    let by = end - (was.start + was.len);
                    for &(i, before) in group.iter() {
                        if let Some(note) = notes.get_mut(i) {
                            // (Not `clamp`: near the clip's end the room left is less than a grid step.)
                            note.len = (before.len + by).max(grid.min(length)).min(length - before.start).max(1);
                        }
                    }
                }
                Gesture::Move(index, inside, group) if *index < notes.len() => {
                    let was = group.iter().find(|(i, _)| i == index).map_or(notes[*index], |(_, n)| *n);
                    // As far as the one under the pointer goes, kept inside
                    // the clip and the keyboard for every note of the group.
                    let low = group.iter().map(|(_, n)| -n.start).max().unwrap_or(0);
                    let high = group.iter().map(|(_, n)| length - n.len - n.start).min().unwrap_or(0).max(low);
                    let by = (snap(tick_at(pos.x) - *inside) - was.start).clamp(low, high);
                    let down = group.iter().map(|(_, n)| -(n.pitch as i32)).max().unwrap_or(0);
                    let up = group.iter().map(|(_, n)| 127 - n.pitch as i32).min().unwrap_or(0);
                    let pitch_by = (pitch_at(pos.y) as i32 - was.pitch as i32).clamp(down, up);
                    let pitch_now = (was.pitch as i32 + pitch_by) as u8;
                    if notes[*index].pitch != pitch_now {
                        carried = Some(pitch_now);
                    }
                    for &(i, before) in group.iter() {
                        if let Some(note) = notes.get_mut(i) {
                            note.start = before.start + by;
                            note.pitch = (before.pitch as i32 + pitch_by) as u8;
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(pitch) = carried {
            audition(self, pitch);
        }
        // The box of a selection in progress, for the painting below.
        let boxed = match (&self.gesture, pointer) {
            (Some(Gesture::Box(from)), Some(to)) if response.dragged_by(PointerButton::Primary) => {
                Some(Rect::from_two_pos(*from, to))
            }
            _ => None,
        };
        // The right button removes the note under it at once, and every
        // note it is then drawn over: one undo step when it is let go. (Not
        // egui's "clicked": a hand that moves a little made it a drag, and
        // the note stayed, feedback F16.2.)
        let (pressed, held, at) = ui.input(|i| {
            (i.pointer.button_pressed(PointerButton::Secondary), i.pointer.secondary_down(), i.pointer.latest_pos())
        });
        if pressed && self.gesture.is_none() && at.is_some_and(|p| lanes.contains(p)) && response.contains_pointer() {
            self.gesture = Some(Gesture::Erase);
            self.working = Some(stored.clone());
        }
        if matches!(self.gesture, Some(Gesture::Erase)) {
            if let (Some(notes), Some(at)) = (&mut self.working, at)
                && let Some((index, _)) = hit(notes, at)
            {
                notes.remove(index);
            }
            if !held {
                self.gesture = None;
                if let Some(notes) = self.working.take().filter(|notes| *notes != stored) {
                    self.selected.clear();
                    Self::commit(session, sequence, track_id, clip_id, notes, error);
                }
            }
        }
        if response.drag_stopped() {
            if let (Some(notes), Some(gesture)) = (self.working.take(), self.gesture.take()) {
                let touched: Vec<usize> = match &gesture {
                    Gesture::Draw(i) => vec![*i],
                    Gesture::Stretch(_, group) | Gesture::Move(_, _, group) => group.iter().map(|(i, _)| *i).collect(),
                    Gesture::Box(_) | Gesture::Erase => Vec::new(),
                };
                if let Gesture::Box(from) = gesture {
                    // Every note the box touches, and only those.
                    let area = Rect::from_two_pos(from, pointer.unwrap_or(from));
                    self.selected.clear();
                    for note in &stored {
                        let rect = Rect::from_min_max(
                            pos2(x_of(note.start), y_of(note.pitch as f32)),
                            pos2(x_of(note.start + note.len), y_of(note.pitch as f32) + row_h),
                        );
                        if area.intersects(rect) && !self.selected.contains(note) {
                            self.selected.push(*note);
                        }
                    }
                } else {
                    let after: Vec<Note> = touched.iter().filter_map(|i| notes.get(*i).copied()).collect();
                    if let Some(note) = after.first() {
                        self.last_len = Some(note.len);
                    }
                    if notes != stored {
                        Self::commit(session, sequence, track_id, clip_id, notes, error);
                        // (Found again by what they are: the notes are sorted when stored.)
                        self.selected = after;
                    }
                }
            }
        } else if response.clicked_by(PointerButton::Primary)
            && let Some(pos) = pointer.filter(|p| lanes.contains(*p))
        {
            let ctrl = ui.input(|i| i.modifiers.ctrl);
            match hit(&stored, pos) {
                // Ctrl+click adds a note to the selection or takes it out.
                Some((index, _)) if ctrl => match self.selected.iter().position(|n| *n == stored[index]) {
                    Some(at) => {
                        self.selected.remove(at);
                    }
                    None => self.selected.push(stored[index]),
                },
                Some((index, _)) => self.selected = vec![stored[index]],
                None if ctrl || self.select => self.selected.clear(),
                None => {
                    let len = self.last_len.unwrap_or(grid).max(grid);
                    let start = floor(tick_at(pos.x));
                    let note =
                        Note { start, len: len.min(length - start).max(1), pitch: pitch_at(pos.y), velocity: 100 };
                    let mut notes = stored.clone();
                    notes.push(note);
                    audition(self, note.pitch);
                    Self::commit(session, sequence, track_id, clip_id, notes, error);
                    self.selected = vec![note];
                }
            }
        }

        // Painting: the keys, the rows, the beats, the notes, the playhead.
        let shown: Vec<Note> = match &self.working {
            Some(notes) => notes.clone(),
            None => {
                let now = session.project().clone();
                let raw = now
                    .sequences
                    .first()
                    .and_then(|s| s.track(track_id))
                    .and_then(|t| t.notes.get(&clip_id).map(|n| Vec::clone(n)))
                    .unwrap_or_default();
                in_view(&raw, span_of(sequence, &clip).0, length).0
            }
        };
        let painter = ui.painter_at(body);
        painter.rect_filled(body, 0.0, theme::BG);
        let first_row = top.ceil() as i32;
        for row in 0..=rows as i32 {
            let pitch = first_row - row;
            if !(0..=127).contains(&pitch) {
                continue;
            }
            let y = y_of(pitch as f32);
            let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
            let line = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), row_h));
            painter.rect_filled(line.with_min_x(lanes.left()), 0.0, if black { theme::BG } else { theme::PANEL });
            let key = Rect::from_min_size(pos2(body.left(), y), vec2(KEYS_W - 2.0, row_h - 1.0));
            painter.rect_filled(key, 1.0, if black { Color32::from_gray(24) } else { Color32::from_gray(200) });
            // A key with a recording of its own says which (a drum kit reads by name, not by pitch).
            if let Some((_, asset)) = track.pads.iter().find(|(key, _)| *key as i32 == pitch) {
                let name = project.assets.get(asset).map_or("?", |a| a.label.as_str());
                painter.text(
                    pos2(lanes.left() + 4.0, y + row_h / 2.0),
                    Align2::LEFT_CENTER,
                    name,
                    FontId::proportional(9.0),
                    theme::DIM,
                );
            }
            if pitch % 12 == 0 {
                painter.text(
                    key.right_center() - vec2(3.0, 0.0),
                    Align2::RIGHT_CENTER,
                    name_of(pitch as u8),
                    FontId::proportional(9.0),
                    Color32::from_gray(40),
                );
            }
        }
        let lanes_painter = painter.with_clip_rect(lanes);
        let ruler_painter = painter.with_clip_rect(ruler);
        painter.rect_filled(Rect::from_min_max(body.min, pos2(body.right(), lanes.top())), 0.0, theme::PANEL);
        // After the clip's end: darker, it is not part of the clip until a note is written there.
        if clip_length < length {
            let after = lanes.with_min_x(x_of(clip_length).max(lanes.left()));
            lanes_painter.rect_filled(after, 0.0, Color32::from_black_alpha(90));
        }
        // (Only the lines in view: the roll may be much longer than what is seen.)
        let mut tick = (tick_at(lanes.left()).max(0) / grid) * grid;
        let last = tick_at(lanes.right()).min(length);
        while tick <= last {
            let beat = tick / TICKS_PER_BEAT;
            let (bar, on_beat) = (tick % (TICKS_PER_BEAT * beats_per_bar) == 0, tick % TICKS_PER_BEAT == 0);
            // (Grid lines closer than four points would only smear.)
            if on_beat || grid as f32 * per_tick >= 4.0 {
                let alpha = if bar {
                    70
                } else if on_beat {
                    34
                } else {
                    14
                };
                let x = x_of(tick);
                lanes_painter.line_segment(
                    [pos2(x, lanes.top()), pos2(x, lanes.bottom())],
                    Stroke::new(1.0, Color32::from_white_alpha(alpha)),
                );
                if bar {
                    let number = format!("{}", beat / beats_per_bar + 1);
                    ruler_painter.line_segment(
                        [pos2(x, ruler.top() + 3.0), pos2(x, ruler.bottom())],
                        Stroke::new(1.0, Color32::from_white_alpha(70)),
                    );
                    ruler_painter.text(
                        pos2(x + 3.0, ruler.top() + 2.0),
                        Align2::LEFT_TOP,
                        number,
                        FontId::monospace(9.0),
                        theme::DIM,
                    );
                }
            }
            tick += grid;
        }
        for note in shown.iter() {
            let rect = Rect::from_min_max(
                pos2(x_of(note.start), y_of(note.pitch as f32) + 1.0),
                pos2(x_of(note.start + note.len) - 1.0, y_of(note.pitch as f32) + row_h - 1.0),
            );
            if !rect.intersects(lanes) {
                continue;
            }
            let chosen = self.working.is_none() && self.selected.contains(note);
            let under = over.is_some_and(|(index, _)| shown.get(index) == Some(note));
            // The harder a note is struck, the stronger its colour.
            let strength = 0.5 + 0.5 * note.velocity as f32 / 127.0;
            let body = theme::ACCENT.gamma_multiply(if under { (strength + 0.15).min(1.0) } else { strength });
            lanes_painter.rect_filled(rect.translate(vec2(0.0, 1.0)), theme::RADIUS, Color32::from_black_alpha(70));
            lanes_painter.rect_filled(rect, theme::RADIUS, body);
            // A lighter upper half: the note reads as a key cap, not as a bar of colour.
            let cap = Rect::from_min_max(rect.min + vec2(1.0, 1.0), pos2(rect.right() - 1.0, rect.center().y));
            lanes_painter.rect_filled(cap, theme::RADIUS, Color32::from_white_alpha(26));
            let edge =
                if chosen { Stroke::new(2.0, theme::TEXT) } else { Stroke::new(1.0, Color32::from_black_alpha(140)) };
            lanes_painter.rect_stroke(rect, theme::RADIUS, edge, StrokeKind::Inside);
            // The end that is pulled to make the note longer or shorter.
            if rect.width() >= 14.0 {
                let x = rect.right() - 4.0;
                let grip = if under { Color32::from_white_alpha(230) } else { Color32::from_white_alpha(120) };
                lanes_painter
                    .line_segment([pos2(x, rect.top() + 4.0), pos2(x, rect.bottom() - 4.0)], Stroke::new(1.5, grip));
            }
            // Its name, where there is room for it.
            if rect.width() >= 34.0 && row_h >= 13.0 {
                lanes_painter.with_clip_rect(rect.intersect(lanes)).text(
                    rect.left_center() + vec2(5.0, 0.0),
                    Align2::LEFT_CENTER,
                    name_of(note.pitch),
                    FontId::proportional((row_h - 6.0).clamp(8.5, 11.0)),
                    Color32::from_black_alpha(210),
                );
            }
        }
        if let Some(area) = boxed {
            lanes_painter.rect_filled(area, 0.0, Color32::from_white_alpha(18));
            lanes_painter.rect_stroke(area, 0.0, Stroke::new(1.0, theme::TEXT), StrokeKind::Inside);
        }
        // The playhead, where the roll reaches it, with its handle in the ruler.
        let at = sequence.rate.frames_to_samples(playhead, SAMPLE_RATE) - clip.start;
        if at >= 0 && (at as f64 / samples_per_tick) <= length as f64 {
            let x = x_of((at as f64 / samples_per_tick) as i64);
            lanes_painter
                .line_segment([pos2(x, lanes.top()), pos2(x, lanes.bottom())], Stroke::new(1.5, theme::ACCENT));
            let handle =
                vec![pos2(x - 5.0, ruler.bottom() - 9.0), pos2(x + 5.0, ruler.bottom() - 9.0), pos2(x, ruler.bottom())];
            ruler_painter.add(egui::Shape::convex_polygon(handle, theme::ACCENT, Stroke::NONE));
        }
        if let Some(pos) = pointer.filter(|p| lanes.contains(*p) && response.hovered()) {
            let label = name_of(pitch_at(pos.y));
            painter.text(pos + vec2(12.0, -12.0), Align2::LEFT_BOTTOM, label, FontId::proportional(10.0), theme::TEXT);
        }
        if close {
            self.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_anywhere_in_a_row_is_that_rows_pitch_however_the_roll_is_scrolled() {
        for top in [72.0, 72.5, 71.25, 60.9, 127.0] {
            for row_h in [6.0, 17.0, 30.0] {
                for pitch in 40..=(top as u8) {
                    // The row as it is drawn: from (top - pitch) rows down, one row high.
                    let y = (top - pitch as f32) * row_h;
                    for inside in [0.05, 0.5, 0.95] {
                        assert_eq!(pitch_under(top, row_h, y + inside * row_h), pitch, "top {top}, row {row_h}");
                    }
                }
            }
        }
    }
}
