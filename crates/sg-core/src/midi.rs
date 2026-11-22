//! Standard MIDI files: read into the notes of note clips, and written
//! from them. Only what a note clip holds is taken: which key, when, how
//! long, how hard; and the file's first tempo. Channels, controllers and
//! program changes are passed over.

use crate::model::{Note, TICKS_PER_BEAT};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MidiError {
    #[error("this is not a MIDI file")]
    NotMidi,
    #[error("the MIDI file ends in the middle of its data")]
    Cut,
    #[error("this MIDI file counts time in film frames, not in beats; that kind is not read")]
    FilmTime,
}

/// One track of a file: its name if it has one, and its notes in ticks
/// of [`TICKS_PER_BEAT`] from the start of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiTrack {
    pub name: Option<String>,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiFile {
    /// The first tempo the file sets, in thousandths of a beat per minute.
    pub milli_bpm: Option<u32>,
    /// The tracks that have notes.
    pub tracks: Vec<MidiTrack>,
}

impl MidiFile {
    /// The tick after the last note of any track.
    pub fn length(&self) -> i64 {
        self.tracks.iter().flat_map(|t| t.notes.iter()).map(|n| n.start + n.len).max().unwrap_or(0)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], MidiError> {
        let part = self.bytes.get(self.at..self.at + count).ok_or(MidiError::Cut)?;
        self.at += count;
        Ok(part)
    }

    fn byte(&mut self) -> Result<u8, MidiError> {
        Ok(self.take(1)?[0])
    }

    fn number(&mut self, bytes: usize) -> Result<u32, MidiError> {
        Ok(self.take(bytes)?.iter().fold(0, |all, b| all << 8 | *b as u32))
    }

    /// A length as MIDI files write it: seven bits a byte, the top bit says "more".
    fn variable(&mut self) -> Result<u32, MidiError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.byte()?;
            value = value << 7 | (byte & 0x7f) as u32;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(MidiError::Cut)
    }
}

/// Reads a standard MIDI file (format 0 or 1).
pub fn parse(bytes: &[u8]) -> Result<MidiFile, MidiError> {
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(4).map_err(|_| MidiError::NotMidi)? != b"MThd" {
        return Err(MidiError::NotMidi);
    }
    let header = reader.number(4)? as usize;
    let (_format, _tracks, division) = (reader.number(2)?, reader.number(2)?, reader.number(2)?);
    reader.take(header.saturating_sub(6))?;
    if division & 0x8000 != 0 || division == 0 {
        return Err(MidiError::FilmTime);
    }
    // The file's ticks to ours, in whole numbers.
    let scaled = |ticks: u64| (ticks as i128 * TICKS_PER_BEAT as i128 / division as i128) as i64;
    let mut file = MidiFile { milli_bpm: None, tracks: Vec::new() };
    while reader.at + 8 <= bytes.len() {
        let kind: [u8; 4] = reader.take(4)?.try_into().map_err(|_| MidiError::Cut)?;
        let length = reader.number(4)? as usize;
        // (A last chunk cut short by a careless writer is read as far as it goes.)
        let end = (reader.at + length).min(bytes.len());
        if &kind != b"MTrk" {
            reader.at = end;
            continue;
        }
        let mut track = Reader { bytes: &bytes[..end], at: reader.at };
        reader.at = end;
        let (mut now, mut status) = (0u64, 0u8);
        let mut name = None;
        let mut notes: Vec<Note> = Vec::new();
        // Keys that are down: (channel, key) to the tick they went down at and how hard.
        let mut down: Vec<((u8, u8), u64, u8)> = Vec::new();
        while track.at < end {
            let Ok(delta) = track.variable() else { break };
            now += delta as u64;
            let Ok(first) = track.byte() else { break };
            // A data byte where a status belongs: the last channel status
            // again. (Meta and system-exclusive events are not kept as that:
            // some programs go on with the status from before them.)
            let data = if first & 0x80 == 0 {
                Some(first)
            } else {
                if first < 0xf0 {
                    status = first;
                }
                None
            };
            let kind = if first >= 0xf0 { first } else { status };
            match kind {
                0xff => {
                    let (Ok(kind), Ok(length)) = (track.byte(), track.variable()) else { break };
                    let Ok(body) = track.take(length as usize) else { break };
                    match kind {
                        0x03 if name.is_none() => name = Some(String::from_utf8_lossy(body).trim().to_owned()),
                        // Microseconds per beat.
                        0x51 if body.len() == 3 && file.milli_bpm.is_none() => {
                            let micros = body.iter().fold(0u64, |all, b| all << 8 | *b as u64).max(1);
                            file.milli_bpm = Some((60_000_000_000 / micros) as u32);
                        }
                        0x2f => break,
                        _ => {}
                    }
                }
                0xf0 | 0xf7 => {
                    let Ok(length) = track.variable() else { break };
                    if track.take(length as usize).is_err() {
                        break;
                    }
                }
                _ => {
                    let kind = status & 0xf0;
                    let channel = status & 0x0f;
                    // The first data byte: the one that stood where the status would, or the next.
                    let Ok(one) = (match data {
                        Some(byte) => Ok(byte),
                        None => track.byte(),
                    }) else {
                        break;
                    };
                    // Program change and channel pressure have one data byte, the others two.
                    let two = if matches!(kind, 0xc0 | 0xd0) {
                        0
                    } else {
                        match track.byte() {
                            Ok(byte) => byte,
                            Err(_) => break,
                        }
                    };
                    let key = (channel, one & 0x7f);
                    let release = |down: &mut Vec<((u8, u8), u64, u8)>, notes: &mut Vec<Note>, now: u64| {
                        if let Some(at) = down.iter().position(|(k, ..)| *k == key) {
                            let (_, from, velocity) = down.remove(at);
                            let (start, end) = (scaled(from), scaled(now));
                            notes.push(Note { start, len: (end - start).max(1), pitch: key.1, velocity });
                        }
                    };
                    match (kind, two) {
                        // (A key struck again while down ends the note before.)
                        (0x90, velocity) if velocity > 0 => {
                            release(&mut down, &mut notes, now);
                            down.push((key, now, velocity.min(127)));
                        }
                        (0x90, _) | (0x80, _) => release(&mut down, &mut notes, now),
                        _ => {}
                    }
                }
            }
        }
        // Keys never let go end with the track.
        for (key, from, velocity) in down {
            let (start, end) = (scaled(from), scaled(now));
            notes.push(Note { start, len: (end - start).max(TICKS_PER_BEAT / 4), pitch: key.1, velocity });
        }
        notes.sort_by_key(|n| (n.start, n.pitch));
        if !notes.is_empty() {
            file.tracks.push(MidiTrack { name: name.filter(|n| !n.is_empty()), notes });
        }
    }
    Ok(file)
}

/// Writes notes (ticks of [`TICKS_PER_BEAT`]) as a MIDI file of one
/// track, with the tempo if there is one.
pub fn write(notes: &[Note], milli_bpm: Option<u32>, name: &str) -> Vec<u8> {
    fn variable(out: &mut Vec<u8>, value: u32) {
        let mut parts = vec![(value & 0x7f) as u8];
        let mut rest = value >> 7;
        while rest > 0 {
            parts.push((rest & 0x7f) as u8 | 0x80);
            rest >>= 7;
        }
        out.extend(parts.iter().rev());
    }
    // (tick, on, key, velocity); at one tick the releases come first.
    let mut events: Vec<(i64, bool, u8, u8)> = Vec::with_capacity(notes.len() * 2);
    for note in notes {
        events.push((note.start.max(0), true, note.pitch.min(127), note.velocity.clamp(1, 127)));
        events.push((note.start.max(0) + note.len.max(1), false, note.pitch.min(127), 0));
    }
    events.sort_by_key(|(tick, on, key, _)| (*tick, *on, *key));
    let mut track = Vec::new();
    variable(&mut track, 0);
    track.extend([0xff, 0x03]);
    variable(&mut track, name.len() as u32);
    track.extend(name.as_bytes());
    if let Some(milli_bpm) = milli_bpm.filter(|bpm| *bpm > 0) {
        let micros = (60_000_000_000u64 / milli_bpm as u64).min(0xff_ffff) as u32;
        variable(&mut track, 0);
        track.extend([0xff, 0x51, 0x03, (micros >> 16) as u8, (micros >> 8) as u8, micros as u8]);
    }
    let mut now = 0i64;
    for (tick, on, key, velocity) in events {
        variable(&mut track, (tick - now) as u32);
        now = tick;
        track.extend([if on { 0x90 } else { 0x80 }, key, velocity]);
    }
    variable(&mut track, 0);
    track.extend([0xff, 0x2f, 0x00]);
    let mut out = Vec::with_capacity(track.len() + 22);
    out.extend(b"MThd");
    out.extend(6u32.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend(1u16.to_be_bytes());
    out.extend((TICKS_PER_BEAT as u16).to_be_bytes());
    out.extend(b"MTrk");
    out.extend((track.len() as u32).to_be_bytes());
    out.extend(track);
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn notes_written_are_the_notes_read() {
        let notes = vec![
            Note { start: 0, len: 480, pitch: 60, velocity: 100 },
            Note { start: 0, len: 960, pitch: 64, velocity: 80 },
            Note { start: 960, len: 240, pitch: 60, velocity: 1 },
            // (Long enough for the length to need three bytes.)
            Note { start: 1920, len: 40_000, pitch: 127, velocity: 127 },
        ];
        let file = parse(&write(&notes, Some(92_500), "Keys")).unwrap();
        assert_eq!(file.milli_bpm, Some(92_500));
        assert_eq!(file.tracks, vec![MidiTrack { name: Some("Keys".to_owned()), notes }]);
        assert_eq!(file.length(), 41_920);
    }

    #[test]
    fn a_file_of_another_program_is_read() {
        // Format 1, 96 ticks a beat: a tempo track (120 bpm), then a track
        // that uses running status, a note-on with velocity 0 as the
        // release, a controller in between and a key that is never let go.
        let mut bytes = Vec::new();
        bytes.extend(b"MThd\x00\x00\x00\x06\x00\x01\x00\x02\x00\x60");
        let tempo = b"\x00\xff\x51\x03\x07\xa1\x20\x00\xff\x2f\x00";
        bytes.extend(b"MTrk");
        bytes.extend((tempo.len() as u32).to_be_bytes());
        bytes.extend(tempo);
        let track: &[u8] = &[
            0x00, 0xff, 0x03, 0x04, b'B', b'a', b's', b's', // name
            0x00, 0x91, 36, 90, // key 36 down on channel 2
            0x30, 36, 0, // half a beat later, running status: velocity 0 lets it go
            0x00, 0xb1, 7, 100, // a controller
            0x00, 0x91, 40, 1, // a key down, to set the status...
            0x00, 0xff, 0x01, 0x02, b'h', b'i', // ...a text event in between...
            0x00, 40, 0, // ...and its release by the status from before the text
            0x30, 0x91, 38, 70, // at one beat: key 38 down
            0x81, 0x40, 0xc1, 5, // two beats later a program change (one data byte)
            0x00, 0xff, 0x2f, 0x00,
        ];
        bytes.extend(b"MTrk");
        bytes.extend((track.len() as u32).to_be_bytes());
        bytes.extend(track);
        let file = parse(&bytes).unwrap();
        assert_eq!(file.milli_bpm, Some(120_000));
        assert_eq!(file.tracks.len(), 1);
        assert_eq!(file.tracks[0].name.as_deref(), Some("Bass"));
        assert_eq!(
            file.tracks[0].notes,
            vec![
                Note { start: 0, len: 480, pitch: 36, velocity: 90 },
                Note { start: 480, len: 1, pitch: 40, velocity: 1 },
                Note { start: 960, len: 1920, pitch: 38, velocity: 70 },
            ]
        );
    }

    proptest::proptest! {
        /// Whatever bytes follow a MIDI header: an answer, never a panic,
        /// and notes that a clip can hold.
        #[test]
        fn any_bytes_are_read_without_harm(
            division in 1u16..0x7fff,
            body in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400),
            claimed in 0u32..600,
        ) {
            let mut bytes = b"MThd\x00\x00\x00\x06\x00\x01\x00\x01".to_vec();
            bytes.extend(division.to_be_bytes());
            bytes.extend(b"MTrk");
            bytes.extend(claimed.to_be_bytes());
            bytes.extend(&body);
            if let Ok(file) = parse(&bytes) {
                for note in file.tracks.iter().flat_map(|track| track.notes.iter()) {
                    proptest::prop_assert!(note.start >= 0 && note.len >= 1 && note.pitch <= 127 && (1..=127).contains(&note.velocity));
                }
            }
        }

        /// What is written is read back as it was, whatever the notes.
        #[test]
        fn any_notes_survive_the_file(
            notes in proptest::collection::vec((0i64..200_000, 1i64..50_000, 0u8..128, 1u8..128), 0..40),
            tempo in proptest::option::of(20_000u32..400_000),
        ) {
            let mut notes: Vec<Note> =
                notes.into_iter().map(|(start, len, pitch, velocity)| Note { start, len, pitch, velocity }).collect();
            // (A file cannot hold the same key down twice at once: one of two that overlap is left out.)
            notes.sort_by_key(|n| (n.pitch, n.start));
            let mut plain: Vec<Note> = Vec::new();
            for note in notes {
                if plain.last().is_none_or(|last| last.pitch != note.pitch || last.start + last.len <= note.start) {
                    plain.push(note);
                }
            }
            plain.sort_by_key(|n| (n.start, n.pitch));
            let file = parse(&write(&plain, tempo, "t")).unwrap();
            let read = file.tracks.first().map_or(Vec::new(), |track| track.notes.clone());
            proptest::prop_assert_eq!(read, plain);
        }
    }

    #[test]
    fn what_is_not_a_midi_file_says_so() {
        assert_eq!(parse(b"RIFF....WAVE"), Err(MidiError::NotMidi));
        assert_eq!(parse(b""), Err(MidiError::NotMidi));
        assert_eq!(parse(b"MThd\x00\x00\x00\x06\x00\x00\x00\x01\xe7\x28"), Err(MidiError::FilmTime));
        // Cut off inside a track: what was complete is kept.
        let whole = write(&[Note { start: 0, len: 100, pitch: 60, velocity: 64 }], None, "");
        assert!(parse(&whole[..whole.len() - 6]).is_ok());
    }
}
