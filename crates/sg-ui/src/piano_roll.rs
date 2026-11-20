//! The piano roll: the notes of one note clip, drawn and edited with the
//! mouse (docs/spec/09-daw.md). It takes the place of the viewer while a
//! note clip is open.
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
use sg_core::{ClipId, Note, Op, Sequence, TICKS_PER_BEAT, TrackId};

use crate::session::Session;
use crate::theme;

const KEYS_W: f32 = 46.0;
const ROW_H: f32 = 12.0;
const HEAD_H: f32 = 30.0;
/// Distance from a note's right end within which it is stretched, not moved.
const EDGE: f32 = 6.0;
const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

enum Gesture {
    /// A new note being drawn: its index in the working copy.
    Draw(usize),
    /// A note being moved: index, and the pointer's offset inside it in ticks.
    Move(usize, i64),
    Stretch(usize),
}

#[derive(Default)]
pub struct PianoRoll {
    /// The clip whose notes are shown.
    open: Option<(TrackId, ClipId)>,
    /// The highest pitch in view.
    top: Option<f32>,
    /// Grid in ticks: notes start and end on it.
    grid: Option<i64>,
    /// The length a clicked note gets: that of the last one drawn or stretched.
    last_len: Option<i64>,
    selected: Option<usize>,
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
    /// Notes being recorded: pitch, and the timeline sample its key went down at.
    sounding: Vec<(u8, i64)>,
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
            last_len: self.last_len,
            keys: self.keys,
            octave: self.octave,
            played,
            ..PianoRoll::default()
        };
    }

    /// The track whose instrument is played by hand: that of the open clip.
    pub fn hand(&self) -> Option<TrackId> {
        self.open.map(|(track, _)| track)
    }

    /// Whether the computer keyboard plays notes now.
    pub fn takes_keys(&self) -> bool {
        self.open.is_some() && self.keys
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
    pub fn heard(&mut self, session: &mut Session, on: bool, pitch: u8, at: i64, error: &mut Option<String>) {
        if !self.record {
            return;
        }
        if on {
            self.sounding.retain(|(p, _)| *p != pitch);
            self.sounding.push((pitch, at));
            return;
        }
        let Some(index) = self.sounding.iter().position(|(p, _)| *p == pitch) else { return };
        let (_, began) = self.sounding.remove(index);
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
        let note = Note { start, len: (end - start).clamp(TICKS_PER_BEAT / 16, length - start), pitch, velocity: 100 };
        let mut notes: Vec<Note> = track.notes.get(&clip_id).map(|n| Vec::clone(n)).unwrap_or_default();
        notes.push(note);
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

    /// Removes the selected note. Returns whether there was one: the
    /// delete key means the note while the piano roll is open.
    pub fn delete_selected(&mut self, session: &mut Session, error: &mut Option<String>) -> bool {
        let (Some((track, clip)), Some(index)) = (self.open, self.selected.take()) else { return false };
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return false };
        let Some(notes) = sequence.track(track).and_then(|t| t.notes.get(&clip)) else { return false };
        let mut left = Vec::clone(notes);
        if index < left.len() {
            left.remove(index);
            Self::commit(session, sequence, track, clip, left, error);
        }
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
        let op = Op::SetClipNotes { seq: sequence.id, track, clip, notes: Some(notes) };
        if let Err(e) = session.apply(op, None) {
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
        let stored: Vec<Note> = track.notes.get(&clip_id).map(|n| Vec::clone(n)).unwrap_or_default();
        let grid = *self.grid.get_or_insert(TICKS_PER_BEAT / 4);

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
                    .button("Quantise")
                    .on_hover_text("Moves the start of every note to the nearest line of the grid")
                    .clicked()
                {
                    let mut notes = stored.clone();
                    for note in &mut notes {
                        let start = (note.start as f64 / grid as f64).round() as i64 * grid;
                        note.start = start.clamp(0, (length - note.len).max(0));
                    }
                    if notes != stored {
                        self.selected = None;
                        Self::commit(session, sequence, track_id, clip_id, notes, error);
                    }
                }
                if let Some(mut note) = self.selected.and_then(|i| stored.get(i).copied()) {
                    ui.add_space(12.0);
                    ui.label(RichText::new("Velocity").color(theme::DIM));
                    let mut velocity = note.velocity as i32;
                    let field = egui::DragValue::new(&mut velocity).range(1..=127).speed(0.5);
                    if ui.add(field).on_hover_text("How hard the selected note is struck, 1 to 127").changed() {
                        note.velocity = velocity.clamp(1, 127) as u8;
                        let mut notes = stored.clone();
                        if let Some(index) = self.selected.filter(|i| *i < notes.len()) {
                            notes[index] = note;
                            Self::commit(session, sequence, track_id, clip_id, notes, error);
                        }
                    }
                }
                ui.add_space(12.0);
                ui.label(
                    RichText::new("Click: a note. Drag: draw, move, or stretch at its end. Right click: remove.")
                        .color(theme::DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").on_hover_text("Back to the picture (Esc)").clicked() {
                        close = true;
                    }
                });
            });
        });
        let body = Rect::from_min_max(pos2(area.left(), area.top() + HEAD_H), area.max);
        let response = ui.allocate_rect(body, Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, "Piano roll"));
        let lanes = Rect::from_min_max(pos2(body.left() + KEYS_W, body.top()), body.max);
        let rows = (lanes.height() / ROW_H).floor().max(1.0);
        // Starts around the notes there are, or around middle C.
        let top = *self.top.get_or_insert_with(|| {
            let highest = stored.iter().map(|n| n.pitch as f32).fold(f32::MIN, f32::max);
            if stored.is_empty() { 60.0 + rows / 2.0 } else { highest + rows / 3.0 }.clamp(rows, 127.0)
        });
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            self.top = Some((top + scroll / ROW_H).clamp(rows.min(127.0), 127.0));
        }
        let per_tick = lanes.width() / length as f32;
        let x_of = |tick: i64| lanes.left() + tick as f32 * per_tick;
        let y_of = |pitch: f32| lanes.top() + (top - pitch) * ROW_H;
        let tick_at = |x: f32| ((x - lanes.left()) / per_tick) as i64;
        let pitch_at = |y: f32| (top - ((y - lanes.top()) / ROW_H).floor()).clamp(0.0, 127.0) as u8;
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
        // The keys at the left play the instrument while the button is down.
        let keys_column = Rect::from_min_max(body.min, pos2(lanes.left(), body.bottom()));
        let down = ui.input(|i| i.pointer.primary_down());
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
            self.gesture = Some(match hit(&notes, origin) {
                Some((index, true)) => Gesture::Stretch(index),
                Some((index, false)) => Gesture::Move(index, tick_at(origin.x) - notes[index].start),
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
            match *gesture {
                Gesture::Draw(index) | Gesture::Stretch(index) if index < notes.len() => {
                    // (A drawn note keeps the start it was given when the drag began.)
                    let start = notes[index].start;
                    let end = snap(tick_at(pos.x)).max(start + grid).min(length);
                    notes[index].len = (end - start).max(grid.min(length));
                }
                Gesture::Move(index, inside) if index < notes.len() => {
                    let len = notes[index].len;
                    notes[index].start = snap(tick_at(pos.x) - inside).clamp(0, (length - len).max(0));
                    if notes[index].pitch != pitch_at(pos.y) {
                        notes[index].pitch = pitch_at(pos.y);
                        carried = Some(pitch_at(pos.y));
                    }
                }
                _ => {}
            }
        }
        if let Some(pitch) = carried {
            audition(self, pitch);
        }
        if response.drag_stopped() {
            if let (Some(notes), Some(gesture)) = (self.working.take(), self.gesture.take()) {
                let index = match gesture {
                    Gesture::Draw(i) | Gesture::Stretch(i) | Gesture::Move(i, _) => i,
                };
                if let Some(note) = notes.get(index) {
                    self.last_len = Some(note.len);
                }
                if notes != stored {
                    // After sorting the note is found again by what it is.
                    let touched = notes.get(index).copied();
                    Self::commit(session, sequence, track_id, clip_id, notes, error);
                    let now = session.project().clone();
                    let fresh = now
                        .sequences
                        .first()
                        .and_then(|s| s.track(track_id))
                        .and_then(|t| t.notes.get(&clip_id).cloned());
                    self.selected = touched.and_then(|t| fresh?.iter().position(|n| *n == t));
                }
            }
        } else if response.clicked_by(PointerButton::Primary)
            && let Some(pos) = pointer.filter(|p| lanes.contains(*p))
        {
            match hit(&stored, pos) {
                Some((index, _)) => self.selected = Some(index),
                None => {
                    let len = self.last_len.unwrap_or(grid).max(grid);
                    let start = floor(tick_at(pos.x));
                    let note =
                        Note { start, len: len.min(length - start).max(1), pitch: pitch_at(pos.y), velocity: 100 };
                    let mut notes = stored.clone();
                    notes.push(note);
                    audition(self, note.pitch);
                    Self::commit(session, sequence, track_id, clip_id, notes, error);
                    let now = session.project().clone();
                    let fresh = now
                        .sequences
                        .first()
                        .and_then(|s| s.track(track_id))
                        .and_then(|t| t.notes.get(&clip_id).cloned());
                    self.selected = fresh.and_then(|n| n.iter().position(|n| *n == note));
                }
            }
        } else if response.secondary_clicked()
            && let Some((index, _)) = pointer.and_then(|p| hit(&stored, p))
        {
            let mut notes = stored.clone();
            notes.remove(index);
            self.selected = None;
            Self::commit(session, sequence, track_id, clip_id, notes, error);
        }

        // Painting: the keys, the rows, the beats, the notes, the playhead.
        let shown: Vec<Note> = match &self.working {
            Some(notes) => notes.clone(),
            None => {
                let now = session.project().clone();
                now.sequences
                    .first()
                    .and_then(|s| s.track(track_id))
                    .and_then(|t| t.notes.get(&clip_id).map(|n| Vec::clone(n)))
                    .unwrap_or_default()
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
            let line = Rect::from_min_size(pos2(body.left(), y), vec2(body.width(), ROW_H));
            painter.rect_filled(line.with_min_x(lanes.left()), 0.0, if black { theme::BG } else { theme::PANEL });
            let key = Rect::from_min_size(pos2(body.left(), y), vec2(KEYS_W - 2.0, ROW_H - 1.0));
            painter.rect_filled(key, 1.0, if black { Color32::from_gray(24) } else { Color32::from_gray(200) });
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
        let mut tick = 0;
        while tick <= length {
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
                    lanes_painter.text(
                        pos2(x + 3.0, lanes.top() + 1.0),
                        Align2::LEFT_TOP,
                        number,
                        FontId::monospace(9.0),
                        theme::DIM,
                    );
                }
            }
            tick += grid;
        }
        for (index, note) in shown.iter().enumerate() {
            let rect = Rect::from_min_max(
                pos2(x_of(note.start), y_of(note.pitch as f32) + 1.0),
                pos2(x_of(note.start + note.len) - 1.0, y_of(note.pitch as f32) + ROW_H - 1.0),
            );
            let chosen = self.selected == Some(index) && self.working.is_none();
            lanes_painter.rect_filled(rect, 2.0, if chosen { theme::TEXT } else { theme::ACCENT });
            lanes_painter.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::BG), StrokeKind::Inside);
        }
        // The playhead, where it lies inside the clip.
        let at = sequence.rate.frames_to_samples(playhead, SAMPLE_RATE) - clip.start;
        if (0..=clip.len).contains(&at) {
            let x = x_of((at as f64 / samples_per_tick) as i64);
            lanes_painter
                .line_segment([pos2(x, lanes.top()), pos2(x, lanes.bottom())], Stroke::new(1.5, theme::ACCENT));
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
