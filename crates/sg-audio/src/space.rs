//! What the tracks share on the way to the master: a reverb and an echo
//! that every track can send to (docs/spec/09-daw.md, M15), and the
//! metronome's click (M11).
//!
//! All of it runs in the realtime callback: buffers are allocated when a
//! graph is built, never while it plays.

use crate::SAMPLE_RATE;
use crate::graph::MAX_BLOCK;

/// A delay line read a fixed distance behind where it is written.
struct Line {
    buffer: Vec<f32>,
    at: usize,
}

impl Line {
    fn new(len: usize) -> Line {
        Line { buffer: vec![0.0; len.max(1)], at: 0 }
    }

    fn read(&self) -> f32 {
        self.buffer[self.at]
    }

    fn write(&mut self, value: f32) {
        self.buffer[self.at] = value;
        self.at = if self.at + 1 == self.buffer.len() { 0 } else { self.at + 1 };
    }
}

/// One channel of the reverb: eight damped comb filters side by side,
/// then four all-pass filters in a row (the well-known Schroeder/Moorer
/// arrangement with the "Freeverb" tunings, scaled to 48 kHz).
struct Tank {
    combs: Vec<(Line, f32)>,
    passes: Vec<Line>,
}

const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const PASSES: [usize; 4] = [556, 441, 341, 225];
/// How much of a comb's output goes round again: about 1.8 s of tail.
const ROOM: f32 = 0.86;
/// How much the highs are held back on each round: a tail that darkens.
const DAMP: f32 = 0.35;

impl Tank {
    fn new(spread: usize) -> Tank {
        let scale = |n: usize| (n + spread) * SAMPLE_RATE as usize / 44_100;
        Tank {
            combs: COMBS.iter().map(|n| (Line::new(scale(*n)), 0.0)).collect(),
            passes: PASSES.iter().map(|n| Line::new(scale(*n))).collect(),
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let mut out = 0.0;
        for (line, low) in &mut self.combs {
            let heard = line.read();
            *low = heard * (1.0 - DAMP) + *low * DAMP;
            line.write(input + *low * ROOM);
            out += heard;
        }
        for line in &mut self.passes {
            let held = line.read();
            line.write(out + held * 0.5);
            out = held - out;
        }
        out
    }
}

/// The shared reverb: what the tracks send comes back as a room, only
/// the room (the tracks themselves are heard directly).
pub struct Reverb {
    tanks: [Tank; 2],
    /// 20 ms before the room answers: the voice stays in front of it.
    early: [Line; 2],
    /// Low cut state: a room without rumble.
    low: [f32; 2],
}

impl Default for Reverb {
    fn default() -> Reverb {
        let early = SAMPLE_RATE as usize / 50;
        Reverb { tanks: [Tank::new(0), Tank::new(23)], early: [Line::new(early), Line::new(early)], low: [0.0; 2] }
    }
}

impl Reverb {
    /// Replaces what was sent (interleaved stereo) with the room's answer.
    pub fn process(&mut self, bus: &mut [f32]) {
        // A one-pole low cut at about 250 Hz.
        let cut = 1.0 - (-std::f32::consts::TAU * 250.0 / SAMPLE_RATE as f32).exp();
        for frame in bus.as_chunks_mut::<2>().0 {
            // The room is fed the middle of what was sent: its width is its own.
            let sent = (frame[0] + frame[1]) * 0.0075;
            for (side, out) in frame.iter_mut().enumerate() {
                self.low[side] += cut * (sent - self.low[side]);
                let delayed = self.early[side].read();
                self.early[side].write(sent - self.low[side]);
                *out = self.tanks[side].process(delayed);
            }
        }
    }
}

/// The shared echo: repeats in time with the song, each quieter and
/// duller than the one before, alternating a little between the sides.
pub struct Echo {
    lines: [Line; 2],
    low: [f32; 2],
    high: [f32; 2],
}

/// How much of each repeat is repeated again.
const FEEDBACK: f32 = 0.38;

impl Echo {
    /// `samples` between repeats (a beat of the song, usually).
    pub fn new(samples: usize) -> Echo {
        let samples = samples.clamp(480, SAMPLE_RATE as usize * 2);
        Echo { lines: [Line::new(samples), Line::new(samples)], low: [0.0; 2], high: [0.0; 2] }
    }

    /// Replaces what was sent with its repeats.
    pub fn process(&mut self, bus: &mut [f32]) {
        let rate = SAMPLE_RATE as f32;
        // Repeats keep what lies between about 300 Hz and 4 kHz: they sit behind the voice.
        let top = 1.0 - (-std::f32::consts::TAU * 4000.0 / rate).exp();
        let bottom = 1.0 - (-std::f32::consts::TAU * 300.0 / rate).exp();
        for frame in bus.as_chunks_mut::<2>().0 {
            let heard = [self.lines[0].read(), self.lines[1].read()];
            for (side, out) in frame.iter_mut().enumerate() {
                // What goes round crosses over: the repeats wander from side to side.
                let round = *out + heard[1 - side] * FEEDBACK;
                self.high[side] += top * (round - self.high[side]);
                self.low[side] += bottom * (self.high[side] - self.low[side]);
                self.lines[side].write(self.high[side] - self.low[side]);
                *out = heard[side];
            }
        }
    }
}

/// Reverb and echo with the buses the tracks send into.
pub struct Space {
    pub reverb: Reverb,
    pub echo: Echo,
    pub to_reverb: Vec<f32>,
    pub to_echo: Vec<f32>,
}

impl Space {
    pub fn new(echo_samples: usize) -> Space {
        Space {
            reverb: Reverb::default(),
            echo: Echo::new(echo_samples),
            to_reverb: vec![0.0; MAX_BLOCK * 2],
            to_echo: vec![0.0; MAX_BLOCK * 2],
        }
    }
}

/// The metronome: where the beats are, in samples as a fraction (a beat
/// is rarely a whole number of samples; the clicks must not drift).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Click {
    /// A beat lasts `beat.0 / beat.1` samples.
    pub beat: (u64, u64),
    pub beats_per_bar: u32,
}

/// Length of a click in frames (30 ms).
const CLICK_FRAMES: i64 = SAMPLE_RATE as i64 * 3 / 100;

impl Click {
    /// Adds the clicks that fall into `block`, which starts at timeline
    /// sample `position` (before the timeline's start too: the count-in).
    pub fn add(&self, block: &mut [f32], position: i64) {
        let (num, den) = (self.beat.0.max(1) as i128, self.beat.1.max(1) as i128);
        for (n, frame) in block.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let at = position + n as i64;
            // The beat this sample belongs to, and where that beat began.
            let beat = (at as i128 * den).div_euclid(num);
            let began = (beat * num + den - 1).div_euclid(den) as i64;
            let into = at - began;
            if into >= CLICK_FRAMES {
                continue;
            }
            // The first beat of a bar is higher and a little louder.
            let first = beat.rem_euclid(self.beats_per_bar.max(1) as i128) == 0;
            let (hertz, level) = if first { (1760.0, 0.5) } else { (1175.0, 0.35) };
            let t = into as f32 / SAMPLE_RATE as f32;
            let sound = (std::f32::consts::TAU * hertz * t).sin() * (-t * 180.0).exp() * level;
            frame[0] += sound;
            frame[1] += sound;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt()
    }

    #[test]
    fn the_room_answers_after_the_sound_and_dies_away() {
        let mut reverb = Reverb::default();
        let mut bus = vec![0.0f32; 48_000 * 2 * 4];
        // A short burst of tone, then nothing.
        for (n, frame) in bus.as_chunks_mut::<2>().0.iter_mut().take(4800).enumerate() {
            *frame = [(n as f32 * 0.09).sin(); 2];
        }
        reverb.process(&mut bus);
        // Nothing before the 20 ms are over; a tail well after the burst; gone after some seconds.
        assert!(rms(&bus[..900 * 2]) < 1e-6);
        let (tail, late) = (rms(&bus[24_000 * 2..36_000 * 2]), rms(&bus[180_000 * 2..]));
        assert!(tail > 0.002, "{tail}");
        assert!(late < tail * 0.05, "{late} against {tail}");
        // The sides differ: the room has width.
        assert!(bus.as_chunks::<2>().0[10_000..20_000].iter().any(|f| (f[0] - f[1]).abs() > 1e-4));
        let peak = bus.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak.is_finite() && peak < 1.0, "{peak} {tail}");
    }

    #[test]
    fn the_echo_repeats_at_its_time_each_time_quieter() {
        let mut echo = Echo::new(12_000);
        let mut bus = vec![0.0f32; 48_000 * 2];
        for (n, frame) in bus.as_chunks_mut::<2>().0.iter_mut().take(2400).enumerate() {
            *frame = [(n as f32 * 0.13).sin(); 2];
        }
        echo.process(&mut bus);
        let part = |from: usize| rms(&bus[from * 2..(from + 2400) * 2]);
        // Not the sound itself; its first repeat a beat later, the second weaker.
        assert!(part(0) < 1e-6 && part(6000) < 1e-3);
        assert!(part(12_000) > 0.2, "{}", part(12_000));
        assert!(part(24_000) < part(12_000) * 0.6 && part(24_000) > 0.01);
    }

    #[test]
    fn clicks_fall_on_the_beats_without_drifting() {
        // 93 beats a minute: a beat is 30967.74... samples.
        let click = Click { beat: (2_880_000_000, 93_000), beats_per_bar: 4 };
        let onset = |position: i64| {
            let mut block = vec![0.0f32; 4096 * 2];
            click.add(&mut block, position);
            block.as_chunks::<2>().0.iter().position(|f| f[0].abs() > 1e-4).map(|n| position + n as i64)
        };
        // (The sine starts at zero: the first audible sample is the one after the beat.)
        assert_eq!(onset(-100), Some(1));
        assert_eq!(onset(30_900), Some(30_969));
        // Beat 1000 lies at 30 967 741.9: no error has added up.
        assert_eq!(onset(30_967_700), Some(30_967_743));
        // Between beats there is nothing.
        assert_eq!(onset(10_000), None);
        // Before the start of the timeline the beats go on backwards.
        assert_eq!(onset(-31_000), Some(-30_966));
    }
}
