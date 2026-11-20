//! Pitch correction of a voice ("autotune", docs/spec/09-daw.md): the
//! pitch of what is sung is followed and moved to the nearest note of a
//! key, at once for the hard effect of modern rap hooks or gently for a
//! natural one.
//!
//! Method (`PROPOSED`): the classic time-domain one. The sound is read
//! from a short buffer faster or slower than it was written, which raises
//! or lowers its pitch; whenever the reading point has drifted a whole
//! period of the voice from where it should be, it jumps one period (the
//! sound repeats there, so the jump is inaudible) with a short crossfade.
//! Runs on the disk thread like the other track effects, not in the
//! realtime callback. Speech and noise, which have no pitch, pass as they are.

use crate::SAMPLE_RATE;

/// Frames the output trails the input, whatever is being corrected.
pub const TUNE_DELAY: usize = 1024;

const RING: usize = 8192;
/// A new pitch estimate this often (frames).
const ESTIMATE_EVERY: usize = 256;
/// The estimate looks at this many frames, at a quarter of the rate.
const WINDOW: usize = 2048;
const STEP: usize = 4;

/// Notes of a scale as semitones above its root.
const MAJOR: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];
const MINOR: [u8; 7] = [0, 2, 3, 5, 7, 8, 10];

/// How a track's pitch is corrected.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tune {
    /// 0 is off, 1 moves every note all the way onto the scale.
    pub amount: f32,
    /// Root of the key: 0 is C, 1 C sharp, ... 11 B.
    pub key: u8,
    /// 0: every semitone (chromatic), 1: major, 2: minor.
    pub scale: u8,
    /// How fast a note is pulled into place: 0 at once (the hard, robotic
    /// effect), 1 over about a tenth of a second (natural).
    pub speed: f32,
}

impl Default for Tune {
    fn default() -> Tune {
        Tune { amount: 0.0, key: 0, scale: 0, speed: 0.2 }
    }
}

impl Tune {
    /// The note (in semitones, 69 is the A of 440 Hz) of the scale nearest to `note`.
    pub fn nearest(&self, note: f32) -> f32 {
        let allowed: &[u8] = match self.scale {
            1 => &MAJOR,
            2 => &MINOR,
            _ => return note.round(),
        };
        let around = note.round() as i32;
        (around - 2..=around + 2)
            .filter(|n| allowed.contains(&((n - self.key as i32).rem_euclid(12) as u8)))
            .map(|n| n as f32)
            .min_by(|a, b| (a - note).abs().total_cmp(&(b - note).abs()))
            .unwrap_or(note.round())
    }
}

pub struct PitchCorrector {
    ring: Vec<[f32; 2]>,
    /// Frames written so far.
    written: u64,
    /// Where the output is read, in frames since the start (fractional).
    read: f64,
    /// A second reading point one period away, being faded to.
    jump: Option<(f64, f32)>,
    /// Frames a crossfade takes.
    fade: f32,
    /// Period of the voice in frames, as last measured; 0 if it has no pitch now.
    period: f32,
    /// The period last known, for jumps while the voice has none.
    last_period: f32,
    /// Reading speed in force and the one aimed for.
    ratio: f32,
    wanted: f32,
    since_estimate: usize,
    scratch: Vec<f32>,
}

impl Default for PitchCorrector {
    fn default() -> PitchCorrector {
        PitchCorrector::new()
    }
}

impl PitchCorrector {
    pub fn new() -> PitchCorrector {
        PitchCorrector {
            ring: vec![[0.0; 2]; RING],
            written: 0,
            read: -(TUNE_DELAY as f64),
            jump: None,
            fade: 128.0,
            period: 0.0,
            last_period: 240.0,
            ratio: 1.0,
            wanted: 1.0,
            since_estimate: 0,
            scratch: Vec::with_capacity(WINDOW / STEP),
        }
    }

    fn at(&self, index: i64) -> [f32; 2] {
        if index < 0 { [0.0; 2] } else { self.ring[index as usize % RING] }
    }

    /// The sound at a fractional position: a smooth curve through four frames.
    fn sample(&self, position: f64) -> [f32; 2] {
        let base = position.floor();
        let t = (position - base) as f32;
        let i = base as i64;
        let (a, b, c, d) = (self.at(i - 1), self.at(i), self.at(i + 1), self.at(i + 2));
        let mut out = [0.0; 2];
        for ch in 0..2 {
            let c0 = b[ch];
            let c1 = 0.5 * (c[ch] - a[ch]);
            let c2 = a[ch] - 2.5 * b[ch] + 2.0 * c[ch] - 0.5 * d[ch];
            let c3 = 0.5 * (d[ch] - a[ch]) + 1.5 * (b[ch] - c[ch]);
            out[ch] = ((c3 * t + c2) * t + c1) * t + c0;
        }
        out
    }

    /// Measures the period of the newest sound: where it resembles itself
    /// most when laid over itself shifted, between 70 and 400 Hz.
    fn estimate(&mut self) {
        let rate = SAMPLE_RATE as usize / STEP;
        let (short, long) = (rate / 400, rate / 70);
        let newest = self.written as i64;
        // Only the lowest harmonics tell the pitch (see sg-media's voice.rs).
        let ease = 1.0 - (-2.0 * std::f32::consts::PI * 900.0 / SAMPLE_RATE as f32).exp();
        let (mut one, mut two) = (0.0f32, 0.0f32);
        self.scratch.clear();
        for n in 0..WINDOW as i64 {
            let frame = self.at(newest - WINDOW as i64 + n);
            one += ease * ((frame[0] + frame[1]) * 0.5 - one);
            two += ease * (one - two);
            if n % STEP as i64 == 0 {
                self.scratch.push(two);
            }
        }
        let small = &self.scratch;
        let span = small.len() - long - 1;
        let energy: f32 = small[..span].iter().map(|s| s * s).sum();
        if energy < 1e-6 {
            self.period = 0.0;
            return;
        }
        let score = |lag: usize| -> f32 {
            let cross: f32 = small[..span].iter().zip(&small[lag..lag + span]).map(|(a, b)| a * b).sum();
            let other: f32 = small[lag..lag + span].iter().map(|s| s * s).sum();
            cross / (energy * other).sqrt().max(1e-9)
        };
        let scores: Vec<f32> = (short - 1..=long + 1).map(score).collect();
        let best = scores[1..scores.len() - 1].iter().copied().fold(0.0f32, f32::max);
        if best < 0.7 {
            self.period = 0.0;
            return;
        }
        // The shortest lag nearly as good as the best, where the likeness peaks: the pitch, not an octave below it.
        let found = (1..scores.len() - 1)
            .find(|i| scores[*i] >= best * 0.93 && scores[*i] >= scores[*i - 1] && scores[*i] >= scores[*i + 1]);
        let Some(i) = found else {
            self.period = 0.0;
            return;
        };
        // Between the reduced samples: the top of the curve through three scores.
        let (l, m, r) = (scores[i - 1], scores[i], scores[i + 1]);
        let bend = l - 2.0 * m + r;
        let offset = if bend.abs() > 1e-9 { (0.5 * (l - r) / bend).clamp(-0.5, 0.5) } else { 0.0 };
        self.period = ((short - 1 + i) as f32 + offset) * STEP as f32;
        self.last_period = self.period;
    }

    /// Corrects interleaved stereo in place. The output is [`TUNE_DELAY`]
    /// frames late (give or take a period of the voice while it corrects).
    pub fn process(&mut self, block: &mut [f32], tune: &Tune) {
        // 0: at once. 1: about 120 ms to arrive.
        let glide = 1.0 - (-1.0 / (tune.speed.clamp(0.0, 1.0) * 0.12 * SAMPLE_RATE as f32).max(1.0)).exp();
        for frame in block.as_chunks_mut::<2>().0 {
            self.ring[self.written as usize % RING] = *frame;
            self.written += 1;
            self.since_estimate += 1;
            if self.since_estimate >= ESTIMATE_EVERY {
                self.since_estimate = 0;
                self.estimate();
                self.wanted = if self.period > 0.0 && tune.amount > 0.0 {
                    let note = 69.0 + 12.0 * (SAMPLE_RATE as f32 / self.period / 440.0).log2();
                    let off = tune.nearest(note) - note;
                    2f32.powf(off * tune.amount.clamp(0.0, 1.0) / 12.0).clamp(0.7, 1.4)
                } else {
                    1.0
                };
            }
            self.ratio += glide * (self.wanted - self.ratio);
            let mut out = self.sample(self.read);
            self.read += self.ratio as f64;
            if let Some((other, progress)) = self.jump {
                let incoming = self.sample(other);
                out[0] += (incoming[0] - out[0]) * progress;
                out[1] += (incoming[1] - out[1]) * progress;
                let (other, progress) = (other + self.ratio as f64, progress + 1.0 / self.fade);
                if progress >= 1.0 {
                    self.read = other;
                    self.jump = None;
                } else {
                    self.jump = Some((other, progress));
                }
            } else {
                // Drifted a period from where the reading point belongs: one period over.
                let behind = self.written as f64 - self.read;
                let period = (if self.period > 0.0 { self.period } else { self.last_period }) as f64;
                let to = if behind > TUNE_DELAY as f64 + period * 0.5 {
                    Some(self.read + period)
                } else if behind < TUNE_DELAY as f64 - period * 0.5 {
                    Some(self.read - period)
                } else {
                    None
                };
                if let Some(to) = to {
                    self.fade = (period as f32 * 0.5).clamp(64.0, 400.0);
                    self.jump = Some((to, 0.0));
                }
            }
            *frame = out;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A sung note: harmonics of `hz`, interleaved stereo, starting after `silence` frames.
    fn note(hz: f32, frames: usize, silence: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i.saturating_sub(silence) as f32 / SAMPLE_RATE as f32;
                let phase = 2.0 * std::f32::consts::PI * hz * t;
                let v: f32 = (1..=12).map(|h| (phase * h as f32).sin() / h as f32).sum::<f32>() * 0.2;
                let v = if i < silence { 0.0 } else { v };
                [v, v]
            })
            .collect()
    }

    /// The pitch of the last second of interleaved stereo, by where it resembles itself.
    fn pitch_of(sound: &[f32]) -> f32 {
        let left: Vec<f32> = sound[sound.len() - SAMPLE_RATE as usize * 2..].iter().step_by(2).copied().collect();
        let span = 8192;
        let score = |lag: usize| -> f64 { (0..span).map(|i| (left[i] * left[i + lag]) as f64).sum() };
        let lags: Vec<usize> = (80..600).collect();
        let top = lags.iter().map(|lag| score(*lag)).fold(f64::MIN, f64::max);
        // The first peak that high: the period itself, not a multiple of it.
        let best = lags
            .iter()
            .copied()
            .find(|lag| score(*lag) >= top * 0.97 && score(*lag) >= score(lag - 1) && score(*lag) >= score(lag + 1))
            .unwrap();
        let (l, m, r) = (score(best - 1), score(best), score(best + 1));
        let offset = 0.5 * (l - r) / (l - 2.0 * m + r);
        SAMPLE_RATE as f32 / (best as f64 + offset) as f32
    }

    fn run(input: &[f32], tune: Tune) -> Vec<f32> {
        let mut output = input.to_vec();
        let mut corrector = PitchCorrector::new();
        // Odd block sizes: the stream must not care.
        for block in output.chunks_mut(2 * 333) {
            corrector.process(block, &tune);
        }
        output
    }

    #[test]
    fn a_flat_note_is_pulled_onto_the_nearest_semitone() {
        // 225 Hz lies between A (220) and B flat (233.1), nearer to A.
        let sung = note(225.0, SAMPLE_RATE as usize * 3, 0);
        assert!((pitch_of(&sung) - 225.0).abs() < 0.5);
        let tuned = run(&sung, Tune { amount: 1.0, key: 0, scale: 0, speed: 0.0 });
        assert!((pitch_of(&tuned) - 220.0).abs() < 1.0, "{}", pitch_of(&tuned));
        // Half the amount: half the way, in pitch.
        let half = run(&sung, Tune { amount: 0.5, key: 0, scale: 0, speed: 0.0 });
        assert!((pitch_of(&half) - 222.5).abs() < 1.0, "{}", pitch_of(&half));
        // As loud as before, and no louder: the jumps leave no holes and no clicks to speak of.
        let level = |v: &[f32]| (v[96_000..].iter().map(|s| s * s).sum::<f32>() / (v.len() - 96_000) as f32).sqrt();
        assert!((level(&tuned) / level(&sung) - 1.0).abs() < 0.06, "{} {}", level(&tuned), level(&sung));
    }

    #[test]
    fn a_note_outside_the_key_goes_to_the_nearest_one_inside() {
        // 365 Hz is nearly F sharp, which C major does not have: F (349.2) is nearer than G (392).
        let sung = note(365.0, SAMPLE_RATE as usize * 3, 0);
        let tuned = run(&sung, Tune { amount: 1.0, key: 0, scale: 1, speed: 0.0 });
        assert!((pitch_of(&tuned) - 349.23).abs() < 1.5, "{}", pitch_of(&tuned));
        // In G major (root 7) F sharp belongs: it is only brought exactly onto it (370 Hz).
        let in_g = run(&sung, Tune { amount: 1.0, key: 7, scale: 1, speed: 0.0 });
        assert!((pitch_of(&in_g) - 369.99).abs() < 1.5, "{}", pitch_of(&in_g));
        let tune = Tune { amount: 1.0, key: 9, scale: 2, speed: 0.0 };
        // A minor has the same notes as C major.
        assert_eq!((tune.nearest(66.0), tune.nearest(61.2), tune.nearest(69.4)), (65.0, 62.0, 69.0));
    }

    #[test]
    fn what_has_no_pitch_passes_and_everything_is_as_late_as_promised() {
        // Noise: nothing to correct. It comes out as it went in, TUNE_DELAY later.
        let mut seed = 0x1234_5678u32;
        let noise: Vec<f32> = (0..SAMPLE_RATE as usize)
            .flat_map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let v = seed as f32 / u32::MAX as f32 - 0.5;
                [v, v]
            })
            .collect();
        let out = run(&noise, Tune { amount: 1.0, key: 0, scale: 0, speed: 0.0 });
        for i in (0..40_000).step_by(997) {
            assert!((out[(i + TUNE_DELAY) * 2] - noise[i * 2]).abs() < 1e-4, "frame {i}");
        }
        // A note that starts after a silence starts in the output TUNE_DELAY later, within a period.
        let sung = note(225.0, SAMPLE_RATE as usize, 10_000);
        let tuned = run(&sung, Tune { amount: 1.0, key: 0, scale: 0, speed: 0.0 });
        let first = tuned.iter().step_by(2).position(|s| s.abs() > 0.01).unwrap();
        assert!(first.abs_diff(10_000 + TUNE_DELAY) < 260, "{first}");
    }
}
