//! Per-track clean-up that runs on the disk thread, ahead of playback: a
//! low cut and noise reduction. Not on the realtime thread because the
//! noise reduction is a neural network with a 20 ms delay and an FFT that
//! allocates; done here its delay is hidden by reading the source that much
//! earlier, and the realtime graph stays as simple as it was.
//!
//! A change of these settings is heard once the buffered half second has
//! played out (the fader, pan and mute act at once: they are in the graph).

use std::collections::VecDeque;

use nnnoiseless::DenoiseState;

use crate::SAMPLE_RATE;

/// Samples the noise reduction works on at a time.
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// How far the network's output trails its input, in frames (measured by
/// the test `noise_reduction_delay_is_what_the_feed_compensates`).
pub const NETWORK_DELAY: usize = FRAME;
/// Frames the feed reads ahead so that the processed sound lines up: one
/// frame to collect a block for the network plus the network's own delay.
pub const LOOKAHEAD: usize = FRAME + NETWORK_DELAY;

/// Second-order high-pass (Butterworth), one channel.
#[derive(Clone, Copy, Default)]
struct HighPass {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl HighPass {
    fn set(&mut self, hz: f32) {
        self.set_at(hz.clamp(10.0, 2000.0));
    }

    /// The same without the low cut's range: any corner frequency.
    fn set_at(&mut self, hz: f32) {
        let w = 2.0 * std::f32::consts::PI * hz / SAMPLE_RATE as f32;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        self.b = [(1.0 + cos) / 2.0 / a0, -(1.0 + cos) / a0, (1.0 + cos) / 2.0 / a0];
        self.a = [-2.0 * cos / a0, (1.0 - alpha) / a0];
    }

    fn run(&mut self, x: f32) -> f32 {
        // Transposed direct form II.
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// RNNoise per channel, as a stream: frames go in and come out in any
/// block size, [`LOOKAHEAD`] frames late.
struct Denoiser {
    states: [Box<DenoiseState<'static>>; 2],
    /// Interleaved stereo waiting for a full network frame.
    pending: Vec<f32>,
    /// The untreated sound, delayed like the network delays, for blending.
    dry: VecDeque<[f32; 2]>,
    ready: VecDeque<[f32; 2]>,
    scratch_in: [Vec<f32>; 2],
    scratch_out: [Vec<f32>; 2],
}

impl Denoiser {
    fn new() -> Denoiser {
        Denoiser {
            states: [DenoiseState::new(), DenoiseState::new()],
            pending: Vec::with_capacity(FRAME * 2),
            dry: std::iter::repeat_n([0.0; 2], NETWORK_DELAY).collect(),
            // One frame of silence first: output can be taken from the start.
            ready: std::iter::repeat_n([0.0; 2], FRAME).collect(),
            scratch_in: [vec![0.0; FRAME], vec![0.0; FRAME]],
            scratch_out: [vec![0.0; FRAME], vec![0.0; FRAME]],
        }
    }

    fn process(&mut self, block: &mut [f32], amount: f32) {
        // The network expects 16-bit sample values.
        const SCALE: f32 = 32768.0;
        for frame in block.as_chunks_mut::<2>().0 {
            self.pending.extend_from_slice(frame);
            self.dry.push_back(*frame);
            if self.pending.len() == FRAME * 2 {
                for (i, pair) in self.pending.as_chunks::<2>().0.iter().enumerate() {
                    self.scratch_in[0][i] = pair[0] * SCALE;
                    self.scratch_in[1][i] = pair[1] * SCALE;
                }
                for channel in 0..2 {
                    self.states[channel].process_frame(&mut self.scratch_out[channel], &self.scratch_in[channel]);
                }
                for i in 0..FRAME {
                    let dry = self.dry.pop_front().unwrap_or([0.0; 2]);
                    let wet = [self.scratch_out[0][i] / SCALE, self.scratch_out[1][i] / SCALE];
                    self.ready.push_back([dry[0] + (wet[0] - dry[0]) * amount, dry[1] + (wet[1] - dry[1]) * amount]);
                }
                self.pending.clear();
            }
            *frame = self.ready.pop_front().unwrap_or([0.0; 2]);
        }
    }
}

/// Tone shaping of a track after its clean-up: a three-band equaliser and
/// a compressor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    /// Gain in decibels of the low shelf (120 Hz), the bell and the high shelf (8 kHz).
    pub low_db: f32,
    pub mid_db: f32,
    /// Centre of the bell in hertz.
    pub mid_hz: f32,
    pub high_db: f32,
    /// Level above which the compressor turns the sound down, in decibels
    /// below full scale. 0 is off.
    pub threshold_db: f32,
    /// How strongly: 4 lets one decibel through for every four above the threshold.
    pub ratio: f32,
    /// De-esser, 0 (off) to 1: how much the S sounds are turned down.
    pub deess: f32,
    /// Pitch correction.
    pub tune: crate::tune::Tune,
}

impl Default for Tone {
    fn default() -> Tone {
        Tone {
            low_db: 0.0,
            mid_db: 0.0,
            mid_hz: 1000.0,
            high_db: 0.0,
            threshold_db: 0.0,
            ratio: 3.0,
            deess: 0.0,
            tune: crate::tune::Tune { amount: 0.0, key: 0, scale: 0, speed: 0.2 },
        }
    }
}

/// One second-order filter section (Audio EQ Cookbook), stereo.
#[derive(Clone, Copy, Default)]
struct Band {
    b: [f32; 3],
    a: [f32; 2],
    z: [[f32; 2]; 2],
    /// What it is set to: (kind, hertz, decibels).
    set: (u8, f32, f32),
}

impl Band {
    /// `kind`: 0 low shelf, 1 bell, 2 high shelf.
    fn set(&mut self, kind: u8, hz: f32, db: f32) {
        if self.set == (kind, hz, db) {
            return;
        }
        self.set = (kind, hz, db);
        let amp = 10f32.powf(db / 40.0);
        let w = 2.0 * std::f32::consts::PI * hz.clamp(20.0, 20_000.0) / SAMPLE_RATE as f32;
        let (sin, cos) = w.sin_cos();
        let (b, a) = if kind == 1 {
            // A bell one octave wide.
            let alpha = sin * (std::f32::consts::LN_2 / 2.0 * w / sin).sinh();
            ([1.0 + alpha * amp, -2.0 * cos, 1.0 - alpha * amp], [1.0 + alpha / amp, -2.0 * cos, 1.0 - alpha / amp])
        } else {
            let alpha = sin / 2.0 * std::f32::consts::SQRT_2;
            let root = 2.0 * amp.sqrt() * alpha;
            // The high shelf is the low shelf with the cosine terms' signs turned.
            let s = if kind == 0 { 1.0 } else { -1.0 };
            (
                [
                    amp * ((amp + 1.0) - s * (amp - 1.0) * cos + root),
                    s * 2.0 * amp * ((amp - 1.0) - s * (amp + 1.0) * cos),
                    amp * ((amp + 1.0) - s * (amp - 1.0) * cos - root),
                ],
                [
                    (amp + 1.0) + s * (amp - 1.0) * cos + root,
                    -s * 2.0 * ((amp - 1.0) + s * (amp + 1.0) * cos),
                    (amp + 1.0) + s * (amp - 1.0) * cos - root,
                ],
            )
        };
        self.b = b.map(|v| v / a[0]);
        self.a = [a[1] / a[0], a[2] / a[0]];
    }

    fn run(&mut self, channel: usize, x: f32) -> f32 {
        let z = &mut self.z[channel];
        let y = self.b[0] * x + z[0];
        z[0] = self.b[1] * x - self.a[0] * y + z[1];
        z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Where the S sounds of a voice live: above this.
const SIBILANT_HZ: f32 = 4500.0;
/// The most the de-esser takes off them, in decibels.
const DEESS_MOST_DB: f32 = 12.0;

/// A de-esser that finds the S sounds itself: they are the moments at
/// which most of the sound lies above [`SIBILANT_HZ`], whatever the level
/// of the recording, so there is no threshold to set. Only the part above
/// that frequency is turned down, and only during those moments; vowels,
/// however bright, keep their top.
struct DeEsser {
    split: [HighPass; 2],
    /// Followers of the level above the split and of the whole, on the sum of the channels.
    high: f32,
    whole: f32,
    /// The reduction in force, in decibels (0 or below).
    gain_db: f32,
    /// What turns the top down: a shelf whose depth follows `gain_db`.
    shelf: Band,
}

impl DeEsser {
    fn new() -> DeEsser {
        let mut split = [HighPass::default(); 2];
        split.iter_mut().for_each(|f| f.set_at(SIBILANT_HZ));
        DeEsser { split, high: 0.0, whole: 0.0, gain_db: 0.0, shelf: Band::default() }
    }

    fn process(&mut self, block: &mut [f32], amount: f32) {
        let rate = SAMPLE_RATE as f32;
        // The followers rise within a millisecond and fall within thirty;
        // the reduction comes on within two and lets go within sixty.
        let (rise, fall) = (1.0 - (-1.0 / (0.001 * rate)).exp(), 1.0 - (-1.0 / (0.03 * rate)).exp());
        let (on, off) = (1.0 - (-1.0 / (0.002 * rate)).exp(), 1.0 - (-1.0 / (0.06 * rate)).exp());
        for (n, frame) in block.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let top = [self.split[0].run(frame[0]), self.split[1].run(frame[1])];
            let (h, w) = ((top[0] + top[1]).abs() * 0.5, (frame[0] + frame[1]).abs() * 0.5);
            self.high += if h > self.high { rise } else { fall } * (h - self.high);
            self.whole += if w > self.whole { rise } else { fall } * (w - self.whole);
            // An S: more than a third of the sound is up there (a vowel has
            // a few percent), and it is not just the hiss of a pause.
            let share = if self.whole > 1e-4 { self.high / self.whole } else { 0.0 };
            let sharp = ((share - 0.35) / 0.3).clamp(0.0, 1.0);
            let wanted = -DEESS_MOST_DB * amount * sharp;
            self.gain_db += if wanted < self.gain_db { on } else { off } * (wanted - self.gain_db);
            // The shelf follows in steps of a third of a millisecond. It
            // runs all the time, at 0 dB between the S sounds, so that it
            // comes in without a click.
            if n % 16 == 0 {
                self.shelf.set(2, SIBILANT_HZ, (self.gain_db * 4.0).round() / 4.0);
            }
            frame[0] = self.shelf.run(0, frame[0]);
            frame[1] = self.shelf.run(1, frame[1]);
        }
    }
}

/// The clean-up and tone of one track.
pub struct TrackFx {
    filters: [HighPass; 2],
    cut_hz: f32,
    denoiser: Option<Denoiser>,
    bands: [Band; 3],
    /// The compressor's view of the level, in decibels.
    level_db: f32,
    deesser: DeEsser,
    corrector: Option<crate::tune::PitchCorrector>,
}

impl TrackFx {
    /// `denoise` and `tune` decide whether the stream will carry the delay
    /// of the noise reduction and of the pitch correction; that cannot
    /// change afterwards.
    pub fn new(denoise: bool, tune: bool) -> TrackFx {
        TrackFx {
            filters: [HighPass::default(); 2],
            cut_hz: 0.0,
            denoiser: denoise.then(Denoiser::new),
            bands: [Band::default(); 3],
            level_db: -120.0,
            deesser: DeEsser::new(),
            corrector: tune.then(crate::tune::PitchCorrector::new),
        }
    }

    /// Frames the output trails the input.
    pub fn delay(&self) -> usize {
        let tune = if self.corrector.is_some() { crate::tune::TUNE_DELAY } else { 0 };
        tune + if self.denoiser.is_some() { LOOKAHEAD } else { 0 }
    }

    /// Treats interleaved stereo in place: low cut (`low_cut_hz` of 0 is
    /// off), noise reduction (`denoise` is the share of the treated sound),
    /// equaliser, compressor.
    pub fn process(&mut self, block: &mut [f32], low_cut_hz: f32, denoise: f32, tone: &Tone) {
        if low_cut_hz > 0.0 {
            if low_cut_hz != self.cut_hz {
                self.filters.iter_mut().for_each(|f| f.set(low_cut_hz));
            }
            for frame in block.as_chunks_mut::<2>().0 {
                frame[0] = self.filters[0].run(frame[0]);
                frame[1] = self.filters[1].run(frame[1]);
            }
        }
        self.cut_hz = low_cut_hz;
        if let Some(denoiser) = &mut self.denoiser {
            denoiser.process(block, denoise.clamp(0.0, 1.0));
        }
        // On the cleaned voice, before tone and level are shaped.
        if let Some(corrector) = &mut self.corrector {
            corrector.process(block, &tone.tune);
        }
        for (band, (kind, hz, db)) in self.bands.iter_mut().zip([
            (0u8, 120.0, tone.low_db),
            (1, tone.mid_hz, tone.mid_db),
            (2, 8000.0, tone.high_db),
        ]) {
            if db != 0.0 {
                band.set(kind, hz, db.clamp(-24.0, 24.0));
                for frame in block.as_chunks_mut::<2>().0 {
                    frame[0] = band.run(0, frame[0]);
                    frame[1] = band.run(1, frame[1]);
                }
            }
        }
        // After the equaliser (which may have added air and with it S), before
        // the compressor (which would otherwise pump on every S).
        if tone.deess > 0.0 {
            self.deesser.process(block, tone.deess.clamp(0.0, 1.0));
        }
        if tone.threshold_db < 0.0 {
            // Level follower: up within about 5 ms, down within about 120 ms.
            let (attack, release) =
                (1.0 - (-1.0 / (0.005 * SAMPLE_RATE as f32)).exp(), 1.0 - (-1.0 / (0.12 * SAMPLE_RATE as f32)).exp());
            let slope = 1.0 - 1.0 / tone.ratio.max(1.0);
            for frame in block.as_chunks_mut::<2>().0 {
                let peak_db = 20.0 * frame[0].abs().max(frame[1].abs()).max(1e-6).log10();
                let toward = if peak_db > self.level_db { attack } else { release };
                self.level_db += toward * (peak_db - self.level_db);
                // Both channels by the same amount: the stereo picture stays.
                let over = (self.level_db - tone.threshold_db).max(0.0);
                let gain = 10f32.powf(-over * slope / 20.0);
                frame[0] *= gain;
                frame[1] *= gain;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Something voice-like: harmonics of a gliding pitch that come and go.
    /// (A steady pitch would repeat every few milliseconds and hide the delay.)
    fn voice(frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let envelope = (2.0 * std::f32::consts::PI * 3.0 * t).sin().max(0.0);
                let phase = 2.0 * std::f32::consts::PI * (110.0 * t + 20.0 * t * t);
                let s: f32 = (1..=12).map(|h| (phase * h as f32).sin() / h as f32).sum();
                let v = 0.3 * envelope * s;
                [v, v]
            })
            .collect()
    }

    fn rms(samples: impl Iterator<Item = f32>) -> f32 {
        let (sum, n) = samples.fold((0.0f64, 0usize), |(s, n), v| (s + (v * v) as f64, n + 1));
        (sum / n.max(1) as f64).sqrt() as f32
    }

    #[test]
    fn the_de_esser_finds_the_s_sounds_and_leaves_the_vowels() {
        // Half a second of a bright vowel, then a fifth of a second of S
        // (noise above 5 kHz), again and again; the whole thing 20 dB quieter
        // the second time: the de-esser must not care about the level.
        let mut seed = 0x9e37_79b9u32;
        let mut sharp = HighPass::default();
        sharp.set_at(5500.0);
        let dry: Vec<f32> = (0..SAMPLE_RATE as usize * 4)
            .flat_map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let hiss = sharp.run(seed as f32 / u32::MAX as f32 * 2.0 - 1.0);
                let phase = 2.0 * std::f32::consts::PI * 140.0 * t;
                let vowel: f32 = (1..=30).map(|h| (phase * h as f32).sin() / h as f32).sum();
                let s = (t % 0.7) >= 0.5;
                let quiet = if t >= 2.1 { 0.1 } else { 1.0 };
                let v = quiet * if s { 0.25 * hiss } else { 0.2 * vowel };
                [v, v]
            })
            .collect();
        let mut wet = dry.clone();
        let mut fx = TrackFx::new(false, false);
        let tone = Tone { deess: 1.0, ..Tone::default() };
        for block in wet.chunks_mut(2 * 480) {
            fx.process(block, 0.0, 0.0, &tone);
        }
        let level = |v: &[f32], from: f32, to: f32| {
            let (a, b) = ((from * SAMPLE_RATE as f32) as usize * 2, (to * SAMPLE_RATE as f32) as usize * 2);
            20.0 * rms(v[a..b].iter().copied()).max(1e-9).log10()
        };
        // The S sounds lose most of the twelve decibels, loud and quiet alike...
        for (from, to) in [(0.55, 0.69), (1.25, 1.39), (2.65, 2.79), (3.35, 3.49)] {
            let lost = level(&dry, from, to) - level(&wet, from, to);
            assert!((8.0..=12.5).contains(&lost), "S at {from}: {lost} dB");
        }
        // ...the vowels nothing to speak of (after the moment the reduction needs to let go).
        for (from, to) in [(0.15, 0.45), (0.85, 1.15), (2.25, 2.55)] {
            let lost = level(&dry, from, to) - level(&wet, from, to);
            assert!(lost.abs() < 0.3, "vowel at {from}: {lost} dB");
        }
        // Off is off: not a sample changes.
        let mut untouched = dry.clone();
        TrackFx::new(false, false).process(&mut untouched, 0.0, 0.0, &Tone::default());
        assert_eq!(untouched, dry);
    }

    #[test]
    fn noise_reduction_delay_is_what_the_feed_compensates() {
        let input = voice(48_000 * 2);
        let mut output = input.clone();
        let mut fx = TrackFx::new(true, false);
        // Odd block sizes: the stream must not care.
        for block in output.chunks_mut(2 * 333) {
            fx.process(block, 0.0, 1.0, &Tone::default());
        }
        let left = |v: &[f32]| v.iter().step_by(2).copied().collect::<Vec<f32>>();
        let (a, b) = (left(&input), left(&output));
        // The lag at which the output matches the input best.
        let best = (0..2000)
            .max_by(|&x, &y| {
                let score = |lag: usize| (20_000..60_000).map(|i| (a[i] * b[i + lag]) as f64).sum::<f64>();
                score(x).total_cmp(&score(y))
            })
            .unwrap();
        // The glide shifts the best match by a sample or two.
        assert!(best.abs_diff(LOOKAHEAD) <= 4, "measured {best}, compensated {LOOKAHEAD}");
        assert_eq!(fx.delay(), LOOKAHEAD);
    }

    #[test]
    fn hiss_goes_down_and_the_voice_stays() {
        let frames = 48_000 * 3;
        let clean = voice(frames);
        // A fixed pseudo-random hiss at about -34 dB.
        let mut seed = 0x2545F491u32;
        let mut noisy = clean.clone();
        for pair in noisy.as_chunks_mut::<2>().0 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let n = ((seed >> 8) as f32 / (1 << 24) as f32 - 0.5) * 0.04;
            pair[0] += n;
            pair[1] += n;
        }
        let mut treated = noisy.clone();
        let mut fx = TrackFx::new(true, false);
        fx.process(&mut treated, 0.0, 1.0, &Tone::default());
        // The stretches where the voice is silent hold only hiss: second
        // half of every third of a second. Compare them after the delay.
        let quiet = |v: &[f32], shift: usize| {
            rms((48_000..frames - 2000)
                .filter(|i| (i * 3 / 48_000) % 2 == 1 && (i * 6 / 48_000) % 2 == 1)
                .map(|i| v[(i + shift) * 2]))
        };
        let (before, after) = (quiet(&noisy, 0), quiet(&treated, LOOKAHEAD));
        assert!(after < before * 0.35, "hiss {before} -> {after}");
        let loud = |v: &[f32], shift: usize| {
            rms((48_000..frames - 2000).filter(|i| (i * 3 / 48_000) % 2 == 0).map(|i| v[(i + shift) * 2]))
        };
        let (before, after) = (loud(&clean, 0), loud(&treated, LOOKAHEAD));
        assert!(after > before * 0.6, "voice {before} -> {after}");
    }

    #[test]
    fn equaliser_bands_act_where_they_should() {
        let tone_at = |hz: f32| -> Vec<f32> {
            (0..48_000)
                .flat_map(|i| [0.25 * (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin(); 2])
                .collect()
        };
        // Level in decibels relative to the untreated tone.
        let change = |hz: f32, tone: Tone| {
            let mut block = tone_at(hz);
            TrackFx::new(false, false).process(&mut block, 0.0, 0.0, &tone);
            20.0 * (rms(block[24_000..].iter().copied()) / rms(tone_at(hz)[24_000..].iter().copied())).log10()
        };
        let low = Tone { low_db: 6.0, ..Tone::default() };
        assert!(
            (change(40.0, low) - 6.0).abs() < 0.7 && change(2000.0, low).abs() < 0.3,
            "{} {}",
            change(40.0, low),
            change(2000.0, low)
        );
        let mid = Tone { mid_db: -9.0, mid_hz: 1000.0, ..Tone::default() };
        assert!(
            (change(1000.0, mid) + 9.0).abs() < 0.5
                && change(100.0, mid).abs() < 0.5
                && change(10_000.0, mid).abs() < 0.5
        );
        let high = Tone { high_db: -6.0, ..Tone::default() };
        assert!((change(16_000.0, high) + 6.0).abs() < 0.7 && change(300.0, high).abs() < 0.3);
        assert_eq!(change(1000.0, Tone::default()), 0.0);
    }

    #[test]
    fn compressor_holds_loud_parts_back_by_its_ratio() {
        let tone = |amplitude: f32| -> Vec<f32> {
            (0..48_000)
                .flat_map(|i| [amplitude * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin(); 2])
                .collect()
        };
        let squeeze = Tone { threshold_db: -20.0, ratio: 4.0, ..Tone::default() };
        let peak_db = |amplitude: f32| {
            let mut block = tone(amplitude);
            TrackFx::new(false, false).process(&mut block, 0.0, 0.0, &squeeze);
            20.0 * block[40_000..].iter().fold(0.0f32, |m, s| m.max(s.abs())).log10()
        };
        // -8 dB in is 12 dB over the threshold: 3 dB of that are let through.
        assert!((peak_db(10f32.powf(-8.0 / 20.0)) + 17.0).abs() < 1.0, "{}", peak_db(10f32.powf(-8.0 / 20.0)));
        // Below the threshold nothing happens.
        assert!((peak_db(10f32.powf(-30.0 / 20.0)) + 30.0).abs() < 0.2);
    }

    #[test]
    fn low_cut_removes_rumble_and_keeps_the_rest() {
        let tone = |hz: f32| -> Vec<f32> {
            (0..48_000).flat_map(|i| [(2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin(); 2]).collect()
        };
        let level = |hz: f32| {
            let mut block = tone(hz);
            TrackFx::new(false, false).process(&mut block, 100.0, 0.0, &Tone::default());
            rms(block[24_000..].iter().copied())
        };
        assert!(level(25.0) < 0.06, "{}", level(25.0));
        assert!((level(100.0) - 0.5).abs() < 0.03, "{}", level(100.0));
        assert!(level(1000.0) > 0.7);
    }
}
