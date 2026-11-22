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
use crate::scope::Probe;

/// Samples the noise reduction works on at a time.
const FRAME: usize = DenoiseState::FRAME_SIZE;
/// How far the network's output trails its input, in frames (measured by
/// the test `noise_reduction_delay_is_what_the_feed_compensates`).
pub const NETWORK_DELAY: usize = FRAME;
/// Frames the feed reads ahead so that the processed sound lines up: one
/// frame to collect a block for the network plus the network's own delay.
pub const LOOKAHEAD: usize = FRAME + NETWORK_DELAY;

/// The two sections of the low cut (a fourth-order Butterworth, 24 dB per
/// octave). It was one section, 12 dB per octave, until feedback F16.19:
/// the hum of the mains at 50 Hz under his voice was only 8 dB down with
/// the cut at 100 Hz, and getting rid of it took a cut at 300 Hz, which
/// takes the body of the voice along. Now it is 24 dB down at 100 Hz.
const CUT_Q: [f32; 2] = [0.541_196_1, 1.306_563];

/// Second-order high-pass (Butterworth), one channel.
#[derive(Clone, Copy, Default)]
struct HighPass {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl HighPass {
    /// A corner at `hz`, as flat as can be above it.
    fn set_at(&mut self, hz: f32) {
        self.set_q(hz, std::f32::consts::FRAC_1_SQRT_2);
    }

    /// With the sharpness of the corner chosen: two of these in a row
    /// with [`CUT_Q`] fall twice as steeply and are as flat above.
    fn set_q(&mut self, hz: f32, q: f32) {
        let w = 2.0 * std::f32::consts::PI * hz / SAMPLE_RATE as f32;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
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
    /// What the compressor gives back after it, in decibels.
    pub makeup_db: f32,
    /// De-esser, 0 (off) to 1: how much the S sounds are turned down.
    pub deess: f32,
    /// Pitch correction.
    pub tune: crate::tune::Tune,
    /// Leveller, 0 (off) to 1: how much phrases are pulled towards the
    /// track's own average level, loud ones down and quiet ones up.
    pub level: f32,
    /// The average the leveller levels to, in decibels, if it is known
    /// beforehand; 0: it is found while playing.
    pub level_ref_db: f32,
    /// Saturation, 0 (off) to 1: the peaks are rounded off, which adds
    /// harmonics and density.
    pub drive: f32,
    /// This track is a voice: tracks that duck go down while it sounds.
    pub voice: bool,
    /// How far this track is turned down while a voice track sounds, in
    /// decibels (0: not at all).
    pub duck_db: f32,
    /// The order of the stages, by number: 0 low cut, 1 noise reduction,
    /// 2 tune, 3 equaliser, 4 de-esser, 5 leveller, 6 compressor, 7
    /// saturation. Each once.
    pub order: [u8; 8],
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
            makeup_db: 0.0,
            deess: 0.0,
            tune: crate::tune::Tune { amount: 0.0, key: 0, scale: 0, speed: 0.2 },
            level: 0.0,
            level_ref_db: 0.0,
            drive: 0.0,
            voice: false,
            duck_db: 0.0,
            order: [0, 1, 2, 3, 4, 5, 6, 7],
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
    /// The strongest reduction of the last block, in decibels (0 or below).
    most_db: f32,
    /// What turns the top down: a shelf whose depth follows `gain_db`.
    shelf: Band,
}

impl DeEsser {
    fn new() -> DeEsser {
        let mut split = [HighPass::default(); 2];
        split.iter_mut().for_each(|f| f.set_at(SIBILANT_HZ));
        DeEsser { split, high: 0.0, whole: 0.0, gain_db: 0.0, most_db: 0.0, shelf: Band::default() }
    }

    fn process(&mut self, block: &mut [f32], amount: f32) {
        let rate = SAMPLE_RATE as f32;
        // The followers rise within a millisecond and fall within thirty;
        // the reduction comes on within two and lets go within sixty.
        let (rise, fall) = (1.0 - (-1.0 / (0.001 * rate)).exp(), 1.0 - (-1.0 / (0.03 * rate)).exp());
        let (on, off) = (1.0 - (-1.0 / (0.002 * rate)).exp(), 1.0 - (-1.0 / (0.06 * rate)).exp());
        self.most_db = 0.0;
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
            self.most_db = self.most_db.min(self.gain_db);
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
///
/// Built without noise reduction and pitch correction
/// (`TrackFx::new(false, false)`), `process` neither allocates nor waits:
/// that is what lets the monitored input run through it on the realtime
/// thread. Keep it so.
pub struct TrackFx {
    /// The low cut: per channel, its two sections.
    filters: [[HighPass; 2]; 2],
    cut_hz: f32,
    denoiser: Option<Denoiser>,
    bands: [Band; 3],
    /// The compressor's view of the level, in decibels.
    level_db: f32,
    deesser: DeEsser,
    corrector: Option<crate::tune::PitchCorrector>,
    /// The leveller's view of the phrase (fast) and of the track (slow), in decibels.
    phrase_db: f32,
    average_db: f32,
    /// What the leveller's hand is at, in decibels.
    ride_db: f32,
    /// Frames of sound the leveller's average has taken in.
    sounded: f32,
    /// What the stages did to the last block: the strongest reduction of
    /// the de-esser and of the compressor, in decibels (0 or below).
    pub work: [f32; 2],
}

impl TrackFx {
    /// `denoise` and `tune` decide whether the stream will carry the delay
    /// of the noise reduction and of the pitch correction; that cannot
    /// change afterwards.
    pub fn new(denoise: bool, tune: bool) -> TrackFx {
        TrackFx {
            filters: [[HighPass::default(); 2]; 2],
            cut_hz: 0.0,
            denoiser: denoise.then(Denoiser::new),
            bands: [Band::default(); 3],
            level_db: -120.0,
            deesser: DeEsser::new(),
            corrector: tune.then(crate::tune::PitchCorrector::new),
            phrase_db: -120.0,
            average_db: -120.0,
            ride_db: 0.0,
            sounded: 0.0,
            work: [0.0; 2],
        }
    }

    /// Frames the output trails the input.
    pub fn delay(&self) -> usize {
        let tune = if self.corrector.is_some() { crate::tune::TUNE_DELAY } else { 0 };
        tune + if self.denoiser.is_some() { LOOKAHEAD } else { 0 }
    }

    /// Treats interleaved stereo in place: low cut (`low_cut_hz` of 0 is
    /// off), noise reduction (`denoise` is the share of the treated sound),
    /// tune, equaliser, de-esser, leveller, compressor, saturation.
    pub fn process(&mut self, block: &mut [f32], low_cut_hz: f32, denoise: f32, tone: &Tone) {
        self.process_watched(block, low_cut_hz, denoise, tone, None);
    }

    /// [`TrackFx::process`], with the level after every stage noted for
    /// the picture of the chain if a `probe` is given.
    pub fn process_watched(
        &mut self,
        block: &mut [f32],
        low_cut_hz: f32,
        denoise: f32,
        tone: &Tone,
        mut probe: Option<Probe>,
    ) {
        // How far the sound trails the block that came in, in frames.
        let mut back = 0;
        let mut tap = |tap: usize, back: usize, block: &[f32]| {
            if let Some(probe) = &mut probe {
                probe.tap(tap, back, block);
            }
        };
        tap(0, back, block);
        self.work = [0.0; 2];
        // In the order the track has them in (feedback F16.22). As they
        // come unless changed: the tune on the cleaned voice, before tone
        // and level are shaped; the leveller before the compressor.
        for (place, stage) in tone.order.into_iter().enumerate() {
            match stage {
                0 => {
                    if low_cut_hz > 0.0 {
                        if low_cut_hz != self.cut_hz {
                            for channel in &mut self.filters {
                                for (section, q) in channel.iter_mut().zip(CUT_Q) {
                                    section.set_q(low_cut_hz.clamp(10.0, 2000.0), q);
                                }
                            }
                        }
                        for frame in block.as_chunks_mut::<2>().0 {
                            for (sample, channel) in frame.iter_mut().zip(&mut self.filters) {
                                let first = channel[0].run(*sample);
                                *sample = channel[1].run(first);
                            }
                        }
                    }
                    self.cut_hz = low_cut_hz;
                }
                1 => {
                    if let Some(denoiser) = &mut self.denoiser {
                        denoiser.process(block, denoise.clamp(0.0, 1.0));
                        back += LOOKAHEAD;
                    }
                }
                2 => {
                    if let Some(corrector) = &mut self.corrector {
                        corrector.process(block, &tone.tune);
                        back += crate::tune::TUNE_DELAY;
                    }
                }
                3 => {
                    for (band, (kind, hz, db)) in self.bands.iter_mut().zip(bands(tone)) {
                        if db != 0.0 {
                            band.set(kind, hz, db.clamp(-24.0, 24.0));
                            for frame in block.as_chunks_mut::<2>().0 {
                                frame[0] = band.run(0, frame[0]);
                                frame[1] = band.run(1, frame[1]);
                            }
                        }
                    }
                }
                4 => {
                    // After the equaliser (which may have added air and with it S), before
                    // the compressor (which would otherwise pump on every S).
                    if tone.deess > 0.0 {
                        self.deesser.process(block, tone.deess.clamp(0.0, 1.0));
                        self.work[0] = self.deesser.most_db;
                    }
                }
                5 => {
                    if tone.level > 0.0 {
                        // A hand on the fader: the phrase's level (about 80 ms) is
                        // compared with the track's own average while it sounds (about
                        // 10 s), and the difference is taken back, at most 12 dB either
                        // way. No threshold: it works the same on a quiet take.
                        // (It was 60 % of the difference against 3 s until feedback
                        // F16.20: a loud stretch of a few seconds became the average
                        // itself and passed. On his take the loudest moments came out
                        // 6 dB lower with this.)
                        // Before the compressor (feedback F16.16): that one then meets
                        // phrases of about one level and squeezes them alike, instead
                        // of working hard on the loud ones and not at all on the quiet.
                        //
                        // The phrase's level rises with the sound (5 ms) and falls
                        // slowly, and the hand moves down quickly (20 ms) but up slowly
                        // (300 ms): otherwise the first syllable after a pause or after
                        // a quiet phrase met a fader still pushed up and came out up to
                        // 9 dB too loud, a peak on every phrase (feedback F16.18).
                        let rate = SAMPLE_RATE as f32;
                        let time = |seconds: f32| 1.0 - (-1.0 / (seconds * rate)).exp();
                        let (rise, fast, slow) = (time(0.005), time(0.08), time(10.0));
                        let (down, up) = (time(0.02), time(0.3));
                        for frame in block.as_chunks_mut::<2>().0 {
                            let now_db = 20.0 * frame[0].abs().max(frame[1].abs()).max(1e-6).log10();
                            self.phrase_db +=
                                if now_db > self.phrase_db { rise } else { fast } * (now_db - self.phrase_db);
                            // While it is still getting to know the track (three seconds
                            // of sound), something far louder than what it has heard is
                            // the voice arriving after room noise or a breath: the
                            // average goes to it quickly, instead of holding the voice
                            // down for seconds (found in review: playback started in a pause).
                            let learning = tone.level_ref_db >= 0.0 && self.sounded < 3.0 * rate;
                            if learning && self.sounded > 0.0 && self.phrase_db > self.average_db + 3.0 {
                                self.average_db += time(0.1) * (self.phrase_db - self.average_db);
                            }
                            // (Not far below where sound counts: as for the compressor
                            // below, a word must not have to be climbed up to from the
                            // bottom of a pause.)
                            if self.sounded > 0.0 {
                                self.phrase_db = self.phrase_db.max(self.average_db - 25.0);
                            }
                            // An average that is known beforehand (from listening to the
                            // whole track) is the average: the same wherever playback
                            // starts, and a voice that gets louder over a take is brought
                            // back to where the take is on the whole (feedback F16.15, F16.20).
                            let known = tone.level_ref_db < 0.0;
                            if known {
                                self.average_db = tone.level_ref_db;
                                self.sounded = self.sounded.max(1.0);
                            }
                            // Pauses are neither part of the average nor raised.
                            let sounding = self.phrase_db > self.average_db - 25.0 && self.phrase_db > -70.0;
                            // Something that can be heard but lies far below the average
                            // for long (playback began on a loud stretch, the rest is
                            // quiet): the average comes down to it, slowly, instead of
                            // leaving the leveller idle for good.
                            if !known && !sounding && self.phrase_db > -60.0 && self.sounded > 0.0 {
                                self.average_db += slow * (self.phrase_db - self.average_db);
                            }
                            if sounding && !known {
                                // The plain mean of what has sounded so far until that
                                // is as long as the average's own memory: the track is
                                // known from its first phrase on, not after ten seconds.
                                self.sounded = (self.sounded + 1.0).min(1e9);
                                self.average_db += (1.0 / self.sounded).max(slow) * (self.phrase_db - self.average_db);
                            }
                            // (Not before it has heard a quarter of a second: the
                            // average of the first moments is the follower's own climb.)
                            let heard_enough = known || self.sounded > 0.25 * rate;
                            let wanted = if sounding && heard_enough {
                                let under = self.average_db - self.phrase_db;
                                // Pushed up in full only what is within 10 dB of the
                                // average, and less from there down to nothing at 20:
                                // further below it is breath, the end of a word or the
                                // noise of the room, which came out 12 dB louder (seen
                                // on a clean take of his, whose pauses were silent
                                // before the chain and 17 dB under the voice after it).
                                let share = if under > 0.0 { ((20.0 - under) / 10.0).clamp(0.0, 1.0) } else { 1.0 };
                                (under * tone.level.min(1.0)).clamp(-12.0, 12.0) * share
                            } else {
                                0.0
                            };
                            self.ride_db += if wanted < self.ride_db { down } else { up } * (wanted - self.ride_db);
                            let gain = 10f32.powf(self.ride_db / 20.0);
                            frame[0] *= gain;
                            frame[1] *= gain;
                        }
                    }
                }
                6 => {
                    if tone.threshold_db < 0.0 {
                        // Level follower: up within about 5 ms, down within about 120 ms.
                        // On a voice up within 1 ms: a voice is short spikes, one per
                        // cycle of its pitch, and at 5 ms the follower stayed so far
                        // under them that the loudest moments of his take passed
                        // (feedback F16.20). Drums keep the 5 ms that lets a hit through.
                        let up = if tone.voice { 0.001 } else { 0.005 };
                        let (attack, release) = (
                            1.0 - (-1.0 / (up * SAMPLE_RATE as f32)).exp(),
                            1.0 - (-1.0 / (0.12 * SAMPLE_RATE as f32)).exp(),
                        );
                        let slope = 1.0 - 1.0 / tone.ratio.max(1.0);
                        for frame in block.as_chunks_mut::<2>().0 {
                            let peak_db = 20.0 * frame[0].abs().max(frame[1].abs()).max(1e-6).log10();
                            let toward = if peak_db > self.level_db { attack } else { release };
                            self.level_db += toward * (peak_db - self.level_db);
                            // Never far below the threshold: the follower works in
                            // decibels, and from the -90 dB of a pause it took 10 to
                            // 20 ms to get up to a word, whose start went through
                            // untouched (feedback F16.18, seen on his take: the loudest
                            // moments left the compressor as they came in).
                            self.level_db = self.level_db.max(tone.threshold_db - 1.0);
                            // Both channels by the same amount: the stereo picture stays.
                            let over = (self.level_db - tone.threshold_db).max(0.0);
                            self.work[1] = self.work[1].min(-over * slope);
                            let gain = 10f32.powf(-over * slope / 20.0);
                            frame[0] *= gain;
                            frame[1] *= gain;
                        }
                    }
                    if tone.makeup_db > 0.0 {
                        let gain = 10f32.powf(tone.makeup_db.min(24.0) / 20.0);
                        block.iter_mut().for_each(|sample| *sample *= gain);
                    }
                }
                _ => {
                    if tone.drive > 0.0 {
                        // Quiet sound passes as it is; the louder, the rounder.
                        let push = 1.0 + 4.0 * tone.drive.min(1.0);
                        for sample in block.iter_mut() {
                            *sample = (*sample * push).tanh() / push;
                        }
                    }
                }
            }
            tap(place + 1, back, block);
        }
    }
}

/// A whole recording (interleaved stereo) through the stages, as the
/// track would sound before its plugins and fader, in place and in time
/// with what went in. For listening to what settings do to a take
/// without playing it (the automatic set-up of a voice).
pub fn process_offline(stereo: &mut [f32], low_cut_hz: f32, denoise: f32, tone: &Tone) {
    let mut fx = TrackFx::new(denoise > 0.0, tone.tune.amount > 0.0);
    let delay = fx.delay() * 2;
    let length = stereo.len();
    // The stages hand the sound back late: that much silence goes in
    // after it, and the result is moved back to where it belongs.
    let mut all = Vec::with_capacity(length + delay);
    all.extend_from_slice(stereo);
    all.resize(length + delay, 0.0);
    for block in all.chunks_mut(2 * 1024) {
        fx.process(block, low_cut_hz, denoise, tone);
    }
    stereo.copy_from_slice(&all[delay..]);
}

/// What the leveller of a track with these settings finds the average of
/// a whole recording (interleaved stereo) to be, in decibels: the level
/// it would level to by the end. For settings that give it beforehand
/// (`Tone::level_ref_db`). `None` if nothing in the recording sounds.
pub fn leveller_average(stereo: &[f32], low_cut_hz: f32, denoise: f32, tone: &Tone) -> Option<f32> {
    let mut fx = TrackFx::new(denoise > 0.0, false);
    // (The leveller must run to listen; what it is set to does not change what it hears.)
    let tone = Tone { level: tone.level.max(0.01), level_ref_db: 0.0, ..*tone };
    let mut all = stereo.to_vec();
    for block in all.chunks_mut(2 * 1024) {
        fx.process(block, low_cut_hz, denoise, &tone);
    }
    (fx.sounded > 0.0).then_some(fx.average_db.clamp(-90.0, -0.1))
}

/// The equaliser's three bands as (kind, hertz, decibels).
fn bands(tone: &Tone) -> [(u8, f32, f32); 3] {
    [(0, 120.0, tone.low_db), (1, tone.mid_hz, tone.mid_db), (2, 8000.0, tone.high_db)]
}

/// What the low cut and the equaliser together do to a steady tone of
/// `hz`, in decibels: the curve the mixer draws. From the same filters
/// that run on the sound.
pub fn response_db(low_cut_hz: f32, tone: &Tone, hz: f32) -> f32 {
    // |H| of a second-order section at `hz`.
    let gain = |b: [f32; 3], a: [f32; 2]| {
        let w = 2.0 * std::f32::consts::PI * hz / SAMPLE_RATE as f32;
        let (re1, im1) = (w.cos(), -w.sin());
        let (re2, im2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let top = (b[0] + b[1] * re1 + b[2] * re2, b[1] * im1 + b[2] * im2);
        let bottom = (1.0 + a[0] * re1 + a[1] * re2, a[0] * im1 + a[1] * im2);
        (top.0.hypot(top.1) / bottom.0.hypot(bottom.1).max(1e-9)).max(1e-9)
    };
    let mut all = 1.0;
    if low_cut_hz > 0.0 {
        for q in CUT_Q {
            let mut cut = HighPass::default();
            cut.set_q(low_cut_hz.clamp(10.0, 2000.0), q);
            all *= gain(cut.b, cut.a);
        }
    }
    for (kind, band_hz, db) in bands(tone) {
        if db != 0.0 {
            let mut band = Band::default();
            band.set(kind, band_hz, db.clamp(-24.0, 24.0));
            all *= gain(band.b, band.a);
        }
    }
    20.0 * all.log10()
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
    fn a_phrase_after_a_pause_does_not_come_in_louder_than_it_is() {
        // Quiet phrase, pause, loud phrase; again and again. The leveller
        // has pushed the quiet one up and must not meet the loud one so.
        let level_at = |t: f32| match (t % 3.0) as u32 {
            0 => 0.05,
            1 => 0.0,
            _ => 0.5,
        };
        let mut sound: Vec<f32> = (0..SAMPLE_RATE as usize * 9)
            .flat_map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let v = level_at(t) * (2.0 * std::f32::consts::PI * 200.0 * t).sin();
                [v, v]
            })
            .collect();
        let mut fx = TrackFx::new(false, false);
        let tone = Tone { level: 1.0, ..Tone::default() };
        for block in sound.chunks_mut(2 * 512) {
            fx.process(block, 0.0, 0.0, &tone);
        }
        // The loud phrase of the last round, from its first sample: never above what went in.
        let (from, to) = (SAMPLE_RATE as usize * 8 * 2, SAMPLE_RATE as usize * 9 * 2);
        let peak = sound[from..to].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 0.5 * 1.06, "{peak}");
    }

    #[test]
    fn the_low_cut_takes_mains_hum_away_and_leaves_the_voice_and_the_curve_says_so() {
        let tone_through = |hz: f32, cut: f32| {
            let mut block: Vec<f32> = (0..SAMPLE_RATE as usize)
                .flat_map(|i| {
                    let v = 0.5 * (2.0 * std::f32::consts::PI * hz * i as f32 / SAMPLE_RATE as f32).sin();
                    [v, v]
                })
                .collect();
            TrackFx::new(false, false).process(&mut block, cut, 0.0, &Tone::default());
            20.0 * (rms(block[block.len() / 2..].iter().copied()) / (0.5 * std::f32::consts::FRAC_1_SQRT_2)).log10()
        };
        // Cut at 100 Hz: the hum at 50 Hz is 24 dB down, a low voice at 200 Hz within half a decibel.
        let (hum, voice) = (tone_through(50.0, 100.0), tone_through(200.0, 100.0));
        assert!((-25.5..-23.0).contains(&hum) && voice > -0.5, "{hum} {voice}");
        // The curve the mixer draws is what the filters do.
        for (hz, measured) in [(50.0, hum), (200.0, voice), (100.0, tone_through(100.0, 100.0))] {
            let drawn = response_db(100.0, &Tone::default(), hz);
            assert!((drawn - measured).abs() < 0.2, "{hz}: {drawn} {measured}");
        }
        // And the equaliser's bell is on it: 6 dB up at its centre.
        let bell = Tone { mid_db: 6.0, mid_hz: 1000.0, ..Tone::default() };
        assert!((response_db(0.0, &bell, 1000.0) - 6.0).abs() < 0.1 && response_db(0.0, &bell, 60.0).abs() < 0.3);
    }

    #[test]
    fn the_picture_of_the_chain_has_the_level_after_each_stage_where_it_is_heard() {
        // A steady loud tone through noise reduction (which delays) and a compressor.
        let mut block: Vec<f32> = (0..SAMPLE_RATE as usize)
            .flat_map(|i| {
                let v = 0.5 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SAMPLE_RATE as f32).sin();
                [v, v]
            })
            .collect();
        let scope = crate::scope::Scope::default();
        let mut last = [i64::MIN; crate::SCOPE_TAPS];
        let mut fx = TrackFx::new(true, false);
        let tone = Tone { threshold_db: -18.0, ratio: 4.0, ..Tone::default() };
        let mut at = 96_000;
        for part in block.chunks_mut(2 * 1000) {
            let probe = Probe { scope: &scope, last: &mut last, at, analysers: None };
            fx.process_watched(part, 0.0, 0.0, &tone, Some(probe));
            at += 1000;
        }
        // Half a second in: the source at 0.5, the same until the compressor, about 9 dB less after it.
        let taps = scope.read((96_000 + 24_000) / crate::SCOPE_STEP).expect("laid down");
        assert!((taps[0] - 0.5).abs() < 0.01 && (taps[6] - 0.5).abs() < 0.02, "{taps:?}");
        let down = 20.0 * (taps[7] / taps[6]).log10();
        assert!((-10.5..-8.0).contains(&down) && taps[8] == taps[7], "{taps:?}");
        // What came out of the noise reduction late was put where it belongs:
        // the stretch before the tone began is silent at every tap.
        let before = scope.read(96_000 / crate::SCOPE_STEP - 1).expect("laid down by the late taps");
        assert!(before.iter().all(|level| *level < 1e-3), "{before:?}");
    }

    #[test]
    fn a_leveller_that_knows_the_tracks_level_levels_from_the_first_sound_on() {
        // Playback starts in a loud stretch: two seconds at 0.5.
        let loud: Vec<f32> = (0..SAMPLE_RATE as usize * 2)
            .flat_map(|i| {
                let v = 0.5 * (2.0 * std::f32::consts::PI * 200.0 * i as f32 / SAMPLE_RATE as f32).sin();
                [v, v]
            })
            .collect();
        let peak_after = |tone: &Tone| {
            let mut sound = loud.clone();
            let mut fx = TrackFx::new(false, false);
            for block in sound.chunks_mut(2 * 512) {
                fx.process(block, 0.0, 0.0, tone);
            }
            sound[sound.len() / 2..].iter().fold(0.0f32, |m, s| m.max(s.abs()))
        };
        // Left to find the level itself, it takes the loud stretch for the track's level: untouched.
        let finding = peak_after(&Tone { level: 1.0, ..Tone::default() });
        assert!((finding - 0.5).abs() < 0.02, "{finding}");
        // What it finds by listening to all of it: the stretch's level as its
        // follower reads it (a little under the peaks of -6 dB).
        let found = leveller_average(&loud, 0.0, 0.0, &Tone::default()).unwrap();
        assert!((-8.5..-5.5).contains(&found), "{found}");
        // Told that the track is 12 dB below that on the whole, it brings the stretch down by those 12.
        let told = peak_after(&Tone { level: 1.0, level_ref_db: found - 12.0, ..Tone::default() });
        assert!((20.0 * (told / 0.5).log10() + 12.0).abs() < 0.7, "{told}");
        assert_eq!(leveller_average(&vec![0.0; 9600], 0.0, 0.0, &Tone::default()), None);
    }

    #[test]
    fn a_leveller_that_starts_in_a_pause_or_on_a_loud_stretch_finds_the_voice() {
        let run = |levels: &[(f32, f32)]| -> Vec<f32> {
            let mut seed = 0x1234_5678u32;
            let mut sound: Vec<f32> = Vec::new();
            for (seconds, level) in levels {
                for i in 0..(seconds * SAMPLE_RATE as f32) as usize {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    // (Noise for the quiet parts, a tone for the voice.)
                    let v = if *level < 0.01 { level * noise } else { level * (i as f32 * 0.03).sin() };
                    sound.extend([v, v]);
                }
            }
            let mut fx = TrackFx::new(false, false);
            let tone = Tone { level: 1.0, ..Tone::default() };
            for block in sound.chunks_mut(2 * 512) {
                fx.process(block, 0.0, 0.0, &tone);
            }
            sound
        };
        let peak = |sound: &[f32], from: f32, to: f32| {
            let (a, b) = ((from * SAMPLE_RATE as f32) as usize * 2, (to * SAMPLE_RATE as f32) as usize * 2);
            20.0 * sound[a..b].iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6).log10()
        };
        // A second of room noise at -55 dB, then the voice at -15: half a
        // second in, the voice is within 3 dB of itself, not held down by 12.
        let after_noise = run(&[(1.0, 0.0018), (2.0, 0.18)]);
        assert!(peak(&after_noise, 1.5, 2.0) > -18.5, "{}", peak(&after_noise, 1.5, 2.0));
        // A second at -6 dB, then a voice 29 dB below it for a long while:
        // the leveller comes round to it and does not stay idle.
        let after_loud = run(&[(1.0, 0.5), (30.0, 0.018), (1.0, 0.07)]);
        // (The last second is 12 dB above the quiet voice: by then that is loud, and it is pulled down.)
        assert!(peak(&after_loud, 31.5, 32.0) < -25.0, "{}", peak(&after_loud, 31.5, 32.0));
    }

    #[test]
    fn compressor_and_leveller_turn_a_steady_tone_down_without_bending_it() {
        // A follower that rode each cycle of a low voice would add harmonics:
        // what is not the tone itself, after a second, as a share of it.
        let distortion = |tone: &Tone, hz: f32| {
            let n = SAMPLE_RATE as usize * 2;
            let mut block: Vec<f32> = (0..n)
                .flat_map(|i| {
                    let v = 0.3 * (2.0 * std::f32::consts::PI * hz * i as f32 / SAMPLE_RATE as f32).sin();
                    [v, v]
                })
                .collect();
            let mut fx = TrackFx::new(false, false);
            for part in block.chunks_mut(1024) {
                fx.process(part, 0.0, 0.0, tone);
            }
            let w = 2.0 * std::f64::consts::PI * hz as f64 / SAMPLE_RATE as f64;
            let (mut s, mut c, mut total, mut len) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for (i, v) in block[n..].iter().step_by(2).enumerate() {
                s += *v as f64 * (w * i as f64).sin();
                c += *v as f64 * (w * i as f64).cos();
                total += (*v as f64) * (*v as f64);
                len += 1.0;
            }
            let tone_itself = 2.0 * (s * s + c * c) / len;
            ((total - tone_itself).max(0.0) / tone_itself.max(1e-12)).sqrt() as f32
        };
        let squeezed = Tone { threshold_db: -24.0, ratio: 4.0, voice: true, ..Tone::default() };
        let levelled = Tone { level: 1.0, level_ref_db: -24.0, ..Tone::default() };
        for hz in [100.0, 170.0, 400.0] {
            // (Measured 2026-11-22: 0.7 %, 0.4 %, 0.2 % for the compressor on a voice track; none for the leveller.)
            assert!(distortion(&squeezed, hz) < 0.01, "compressor at {hz} Hz: {}", distortion(&squeezed, hz));
            assert!(distortion(&levelled, hz) < 0.002, "leveller at {hz} Hz: {}", distortion(&levelled, hz));
        }
    }

    #[test]
    fn the_stages_run_in_the_order_the_track_has_them_in() {
        // A loud tone through compressor and saturation: squeezed first it
        // reaches the saturation quieter and is rounded less than the other way round.
        let through = |order: [u8; 8]| {
            let mut block: Vec<f32> = (0..SAMPLE_RATE as usize / 2)
                .flat_map(|i| {
                    let v = 0.8 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SAMPLE_RATE as f32).sin();
                    [v, v]
                })
                .collect();
            let tone = Tone { threshold_db: -20.0, ratio: 4.0, drive: 1.0, order, ..Tone::default() };
            TrackFx::new(false, false).process(&mut block, 0.0, 0.0, &tone);
            block[block.len() / 2..].iter().fold(0.0f32, |m, s| m.max(s.abs()))
        };
        let (usual, turned) = (through([0, 1, 2, 3, 4, 5, 6, 7]), through([0, 1, 2, 3, 4, 5, 7, 6]));
        assert!((usual - turned).abs() > 0.02, "{usual} {turned}");
        // The same stages in the same order give the same sound.
        assert_eq!(usual, through([1, 0, 2, 4, 3, 5, 6, 7]));
    }

    #[test]
    fn the_leveller_evens_out_phrases_and_saturation_rounds_the_peaks() {
        // A loud phrase, a quiet one, a pause; again and again. 14 dB between the two.
        let phrases: Vec<f32> = (0..SAMPLE_RATE as usize * 12)
            .flat_map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let level = match (t % 3.0) as u32 {
                    0 => 0.5,
                    1 => 0.1,
                    _ => 0.0,
                };
                let v = level * (2.0 * std::f32::consts::PI * 200.0 * t).sin();
                [v, v]
            })
            .collect();
        let mut evened = phrases.clone();
        let mut fx = TrackFx::new(false, false);
        let tone = Tone { level: 1.0, ..Tone::default() };
        for block in evened.chunks_mut(2 * 512) {
            fx.process(block, 0.0, 0.0, &tone);
        }
        let level = |v: &[f32], from: f32, to: f32| {
            let (a, b) = ((from * SAMPLE_RATE as f32) as usize * 2, (to * SAMPLE_RATE as f32) as usize * 2);
            20.0 * rms(v[a..b].iter().copied()).max(1e-9).log10()
        };
        // The 14 dB between loud and quiet are gone but for a little, in
        // the first round already (the track is known from its first phrase on).
        // (In the first round it knows only the loud phrase, and does not
        // push something 14 dB below that up in full; later it knows both.)
        for (round, left) in [(0.0, 7.0), (9.0, 4.0)] {
            let before = level(&phrases, round + 0.5, round + 0.9) - level(&phrases, round + 1.5, round + 1.9);
            let after = level(&evened, round + 0.5, round + 0.9) - level(&evened, round + 1.5, round + 1.9);
            assert!((13.5..14.5).contains(&before) && (-1.0..left).contains(&after), "{round}: {before} {after}");
        }
        // The pause stays a pause.
        assert!(level(&evened, 11.3, 11.9) < -100.0);
        // Saturation: a quiet tone passes nearly as it is, a loud one loses its peaks.
        let tone = Tone { drive: 1.0, ..Tone::default() };
        for (amplitude, kept) in [(0.02f32, 0.99..1.001), (0.9, 0.2..0.4)] {
            let mut block: Vec<f32> = (0..4800).map(|i| amplitude * (i as f32 * 0.05).sin()).collect();
            TrackFx::new(false, false).process(&mut block, 0.0, 0.0, &tone);
            let peak = block.iter().fold(0.0f32, |m, s| m.max(s.abs())) / amplitude;
            assert!(kept.contains(&peak), "{amplitude}: {peak}");
        }
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
