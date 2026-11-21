//! What one track's sound looks like along its chain: the level after
//! every stage, per short stretch of the timeline, for the picture the
//! mixer draws of a stage at work (feedback F16.17).
//!
//! The stages run on the disk thread half a second before their sound is
//! heard, so the readings are kept by timeline position, as the stage
//! readings are, and looked up around the position that is heard. Only
//! one track is watched at a time: the one whose chain is on screen.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

use rustfft::num_complex::Complex;

/// The points along the chain: the source, then after low cut, noise
/// reduction, tune, equaliser, de-esser, leveller, compressor and
/// saturation, and last what leaves for the fader (plugins and ducking done).
pub const SCOPE_TAPS: usize = 10;
/// Samples per stretch (5 ms: a syllable is a dozen of them).
pub const SCOPE_STEP: i64 = 256;
/// Stretches kept (11 s).
const SLOTS: usize = 2048;
/// Per slot: which stretch it holds, then a peak per tap (f32 bits).
const WIDTH: usize = 1 + SCOPE_TAPS;

/// The bands of the spectrum, evenly spaced in pitch from [`SPECTRUM_HZ`].
pub const SPECTRUM_BANDS: usize = 64;
pub const SPECTRUM_HZ: (f32, f32) = (20.0, 20_000.0);
/// The quietest a band is told apart from, in decibels.
pub const SPECTRUM_FLOOR_DB: f32 = -96.0;
/// Samples between two spectra (21 ms), the samples each one is taken
/// from, and how many are kept (5 s).
const SPECTRUM_STEP: i64 = 1024;
const SPECTRUM_WINDOW: usize = 2048;
const SPECTRUM_SLOTS: usize = 256;

pub(crate) struct Scope {
    /// The track that is watched (`usize::MAX`: none).
    track: AtomicUsize,
    slots: Box<[AtomicU32]>,
    /// The point along the chain whose spectrum is wanted, with that of
    /// the point after it: what goes into a stage and what comes out
    /// (`usize::MAX`: none).
    spectrum_at: AtomicUsize,
    /// Per slot which stretch it holds, and for both points a byte per band.
    spectrum_tags: Box<[AtomicU32]>,
    spectrum: Box<[AtomicU8]>,
}

impl Default for Scope {
    fn default() -> Scope {
        Scope {
            track: AtomicUsize::new(usize::MAX),
            slots: (0..SLOTS * WIDTH).map(|_| AtomicU32::new(0)).collect(),
            spectrum_at: AtomicUsize::new(usize::MAX),
            spectrum_tags: (0..SPECTRUM_SLOTS * 2).map(|_| AtomicU32::new(0)).collect(),
            spectrum: (0..SPECTRUM_SLOTS * 2 * SPECTRUM_BANDS).map(|_| AtomicU8::new(0)).collect(),
        }
    }
}

impl Scope {
    pub(crate) fn watch(&self, track: Option<usize>, spectrum_at: Option<usize>) {
        self.track.store(track.unwrap_or(usize::MAX), Ordering::Relaxed);
        self.spectrum_at.store(spectrum_at.unwrap_or(usize::MAX), Ordering::Relaxed);
    }

    /// The spectra of the stretch that ends at timeline sample `heard`:
    /// decibels per band, going into the watched stage and coming out.
    pub(crate) fn spectrum(&self, heard: i64) -> Option<[[f32; SPECTRUM_BANDS]; 2]> {
        let step = heard.div_euclid(SPECTRUM_STEP) - 1;
        let slot = step.rem_euclid(SPECTRUM_SLOTS as i64) as usize;
        let mut out = [[SPECTRUM_FLOOR_DB; SPECTRUM_BANDS]; 2];
        for (which, bands) in out.iter_mut().enumerate() {
            if self.spectrum_tags[slot * 2 + which].load(Ordering::Relaxed) != tag(step) {
                return None;
            }
            for (band, db) in bands.iter_mut().enumerate() {
                let byte = self.spectrum[(slot * 2 + which) * SPECTRUM_BANDS + band].load(Ordering::Relaxed);
                *db = SPECTRUM_FLOOR_DB * (1.0 - byte as f32 / 255.0);
            }
        }
        Some(out)
    }

    pub(crate) fn watches(&self, track: usize) -> bool {
        self.track.load(Ordering::Relaxed) == track
    }

    /// The peaks of the stretch `step` at every tap; `None` if it was not
    /// laid down (or has been overwritten since).
    pub(crate) fn read(&self, step: i64) -> Option<[f32; SCOPE_TAPS]> {
        let base = step.rem_euclid(SLOTS as i64) as usize * WIDTH;
        (self.slots[base].load(Ordering::Relaxed) == tag(step))
            .then(|| std::array::from_fn(|tap| f32::from_bits(self.slots[base + 1 + tap].load(Ordering::Relaxed))))
    }
}

/// A stretch's mark in its slot (never 0, which is an empty slot).
fn tag(step: i64) -> u32 {
    (step as u32) | 1 << 31
}

/// The writing side, on the disk thread: where each tap got to, so that a
/// stretch played again (a loop, a jump back) is written anew, not merged
/// with what it was the last time.
pub struct Probe<'a> {
    pub(crate) scope: &'a Scope,
    pub(crate) last: &'a mut [i64; SCOPE_TAPS],
    /// Timeline sample of the first frame of the block as it went into the chain.
    pub(crate) at: i64,
    /// What takes the spectra, if any are wanted.
    pub(crate) analysers: Option<&'a mut [Analyser; 2]>,
}

/// Takes the spectrum of a sound as it passes: every [`SPECTRUM_STEP`]
/// samples of the timeline, of the [`SPECTRUM_WINDOW`] samples before.
pub struct Analyser {
    /// The last samples (both sides summed), a ring.
    recent: Vec<f32>,
    next: usize,
    /// Samples taken in since the last jump.
    filled: usize,
    /// Where the sample after the last one belongs on the timeline.
    expect: i64,
    fft: Arc<dyn rustfft::Fft<f32>>,
    work: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}

impl Default for Analyser {
    fn default() -> Analyser {
        let fft = rustfft::FftPlanner::new().plan_fft_forward(SPECTRUM_WINDOW);
        let scratch = vec![Complex::default(); fft.get_inplace_scratch_len()];
        Analyser {
            recent: vec![0.0; SPECTRUM_WINDOW],
            next: 0,
            filled: 0,
            expect: i64::MIN,
            fft,
            work: vec![Complex::default(); SPECTRUM_WINDOW],
            scratch,
        }
    }
}

impl Analyser {
    /// Takes `block` (interleaved stereo) in, whose first frame is
    /// timeline sample `start`, and writes a spectrum for point `which`
    /// (0: into the stage, 1: out of it) whenever a stretch is complete.
    fn feed(&mut self, scope: &Scope, which: usize, start: i64, block: &[f32]) {
        if start != self.expect {
            // A jump: what was heard before does not belong to what comes now.
            self.filled = 0;
        }
        for (n, frame) in block.as_chunks::<2>().0.iter().enumerate() {
            self.recent[self.next] = (frame[0] + frame[1]) * 0.5;
            self.next = (self.next + 1) % SPECTRUM_WINDOW;
            self.filled = (self.filled + 1).min(SPECTRUM_WINDOW);
            let after = start + n as i64 + 1;
            if after.rem_euclid(SPECTRUM_STEP) == 0 && self.filled == SPECTRUM_WINDOW {
                self.write(scope, which, after.div_euclid(SPECTRUM_STEP) - 1);
            }
        }
        self.expect = start + (block.len() / 2) as i64;
    }

    fn write(&mut self, scope: &Scope, which: usize, step: i64) {
        let size = SPECTRUM_WINDOW as f32;
        for (n, slot) in self.work.iter_mut().enumerate() {
            // A Hann window, so that a steady tone is one peak and not a smear.
            let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * n as f32 / size).cos();
            *slot = Complex::new(self.recent[(self.next + n) % SPECTRUM_WINDOW] * hann, 0.0);
        }
        self.fft.process_with_scratch(&mut self.work, &mut self.scratch);
        // A full-scale sine reads 0 dB: the window halves it, and half of it is in the mirrored bin.
        let level = |bin: usize| self.work[bin].norm() * 4.0 / size;
        let hz_per_bin = crate::SAMPLE_RATE as f32 / size;
        let slot = step.rem_euclid(SPECTRUM_SLOTS as i64) as usize * 2 + which;
        let ratio = (SPECTRUM_HZ.1 / SPECTRUM_HZ.0).powf(1.0 / SPECTRUM_BANDS as f32);
        for band in 0..SPECTRUM_BANDS {
            let low = SPECTRUM_HZ.0 * ratio.powi(band as i32);
            let (first, last) = ((low / hz_per_bin).ceil() as usize, (low * ratio / hz_per_bin).ceil() as usize);
            let peak = if first < last {
                (first..last.min(SPECTRUM_WINDOW / 2)).map(level).fold(0.0f32, f32::max)
            } else {
                // Down low a band is narrower than a bin: between the two around its middle.
                let at = low * ratio.sqrt() / hz_per_bin;
                let below = at.floor() as usize;
                level(below) + (level(below + 1) - level(below)) * at.fract()
            };
            let db = 20.0 * peak.max(1e-6).log10();
            let byte = ((1.0 - db / SPECTRUM_FLOOR_DB).clamp(0.0, 1.0) * 255.0) as u8;
            scope.spectrum[slot * SPECTRUM_BANDS + band].store(byte, Ordering::Relaxed);
        }
        scope.spectrum_tags[slot].store(tag(step), Ordering::Relaxed);
    }
}

impl Probe<'_> {
    /// Notes the level of `block` (interleaved stereo) at `tap`. `back` is
    /// how many frames the sound trails the chain's input by now.
    pub(crate) fn tap(&mut self, tap: usize, back: usize, block: &[f32]) {
        let start = self.at - back as i64;
        let wanted = self.scope.spectrum_at.load(Ordering::Relaxed);
        if let Some(analysers) = &mut self.analysers
            && let Some(which @ (0 | 1)) = tap.checked_sub(wanted)
        {
            analysers[which].feed(self.scope, which, start, block);
        }
        let frames = block.as_chunks::<2>().0;
        let mut n = 0;
        while n < frames.len() {
            let step = (start + n as i64).div_euclid(SCOPE_STEP);
            let until = (((step + 1) * SCOPE_STEP - start) as usize).min(frames.len());
            let mut peak = frames[n..until].iter().fold(0.0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
            let base = step.rem_euclid(SLOTS as i64) as usize * WIDTH;
            if self.scope.slots[base].load(Ordering::Relaxed) != tag(step) {
                for other in 0..SCOPE_TAPS {
                    self.scope.slots[base + 1 + other].store(0, Ordering::Relaxed);
                }
                self.scope.slots[base].store(tag(step), Ordering::Relaxed);
            } else if self.last[tap] == step {
                peak = peak.max(f32::from_bits(self.scope.slots[base + 1 + tap].load(Ordering::Relaxed)));
            }
            self.scope.slots[base + 1 + tap].store(peak.to_bits(), Ordering::Relaxed);
            self.last[tap] = step;
            n = until;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_keeps_the_peak_of_each_stretch_and_a_replay_writes_it_anew() {
        let scope = Scope::default();
        let mut last = [i64::MIN; SCOPE_TAPS];
        // Two blocks that share a stretch: 300 frames at 0.2, then 300 at 0.5.
        let block = |level: f32| vec![level; 600];
        Probe { scope: &scope, last: &mut last, at: 0, analysers: None }.tap(0, 0, &block(0.2));
        Probe { scope: &scope, last: &mut last, at: 300, analysers: None }.tap(0, 0, &block(0.5));
        assert_eq!(scope.read(0).map(|taps| taps[0]), Some(0.2));
        assert_eq!(scope.read(1).map(|taps| taps[0]), Some(0.5));
        assert_eq!(scope.read(2).map(|taps| taps[0]), Some(0.5));
        assert_eq!(scope.read(3), None);
        // A later tap of the same sound, trailing by 256 frames, lands one stretch earlier.
        Probe { scope: &scope, last: &mut last, at: 512, analysers: None }.tap(3, 256, &block(0.1));
        assert_eq!(scope.read(1).map(|taps| (taps[0], taps[3])), Some((0.5, 0.1)));
        // Played again, quieter: the old peak does not stay.
        Probe { scope: &scope, last: &mut last, at: 0, analysers: None }.tap(0, 0, &block(0.05));
        assert_eq!(scope.read(0).map(|taps| taps[0]), Some(0.05));
        // A stretch a round of the slots later takes the slot over.
        Probe { scope: &scope, last: &mut last, at: SLOTS as i64 * SCOPE_STEP, analysers: None }.tap(0, 0, &block(0.3));
        assert_eq!(scope.read(0), None);
        assert_eq!(scope.read(SLOTS as i64).map(|taps| taps[0]), Some(0.3));
    }

    #[test]
    fn the_spectrum_of_a_tone_is_a_peak_at_its_pitch_at_its_level() {
        let scope = Scope::default();
        scope.watch(Some(0), Some(3));
        let mut last = [i64::MIN; SCOPE_TAPS];
        let mut analysers = [Analyser::default(), Analyser::default()];
        // 1 kHz at a tenth of full scale (-20 dB) into the stage, a quarter of that out of it.
        let tone = |gain: f32, from: usize, frames: usize| -> Vec<f32> {
            (from..from + frames)
                .flat_map(|i| {
                    let v = gain * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / crate::SAMPLE_RATE as f32).sin();
                    [v, v]
                })
                .collect()
        };
        for block in 0..12 {
            let at = 48_000 + block * 500;
            let mut probe = Probe { scope: &scope, last: &mut last, at, analysers: Some(&mut analysers) };
            probe.tap(3, 0, &tone(0.1, block as usize * 500, 500));
            probe.tap(4, 0, &tone(0.025, block as usize * 500, 500));
            // (Another point of the chain is not looked at.)
            probe.tap(5, 0, &tone(0.9, block as usize * 500, 500));
        }
        let [into, out] = scope.spectrum(48_000 + 5_500).expect("a stretch is complete");
        let loudest = (0..SPECTRUM_BANDS).max_by(|a, b| into[*a].total_cmp(&into[*b])).unwrap();
        let hz = SPECTRUM_HZ.0 * (SPECTRUM_HZ.1 / SPECTRUM_HZ.0).powf((loudest as f32 + 0.5) / SPECTRUM_BANDS as f32);
        assert!((900.0..1100.0).contains(&hz), "{hz}");
        assert!(
            (into[loudest] + 20.0).abs() < 1.0 && (out[loudest] + 32.0).abs() < 1.0,
            "{} {}",
            into[loudest],
            out[loudest]
        );
        // Far from the tone there is nothing.
        assert!(into[8] < -80.0 && into[60] < -80.0, "{} {}", into[8], into[60]);
        // Nothing is known of a stretch that was not laid down.
        assert_eq!(scope.spectrum(480_000), None);
    }
}
