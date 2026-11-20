//! Loudness as listeners and streaming services measure it (ITU-R
//! BS.1770 / EBU R 128), in LUFS: the level after a filter that follows
//! the ear ("K-weighting"), averaged, with the quiet stretches left out.
//!
//! [`Loudness`] measures a whole piece (an export); [`ShortTerm`] the
//! last three seconds, on the realtime thread, for the mixer.

use crate::SAMPLE_RATE;

/// The K-weighting filter of one channel at 48 kHz: a high shelf, then a
/// low cut, with the coefficients the standard gives.
#[derive(Clone, Copy, Default)]
struct Weighting {
    state: [f64; 4],
}

impl Weighting {
    fn process(&mut self, x: f64) -> f64 {
        const SHELF: ([f64; 3], [f64; 2]) = (
            [1.535_124_859_586_97, -2.691_696_189_406_38, 1.198_392_810_852_85],
            [-1.690_659_293_182_41, 0.732_480_774_215_85],
        );
        const CUT: ([f64; 3], [f64; 2]) = ([1.0, -2.0, 1.0], [-1.990_047_454_833_98, 0.990_072_250_366_21]);
        // Two biquads, transposed direct form II.
        let [s1, s2, s3, s4] = &mut self.state;
        let y = SHELF.0[0] * x + *s1;
        *s1 = SHELF.0[1] * x - SHELF.1[0] * y + *s2;
        *s2 = SHELF.0[2] * x - SHELF.1[1] * y;
        let z = CUT.0[0] * y + *s3;
        *s3 = CUT.0[1] * y - CUT.1[0] * z + *s4;
        *s4 = CUT.0[2] * y - CUT.1[1] * z;
        z
    }
}

/// A tenth of a second: loudness is averaged over windows of four of these.
const HOP: usize = SAMPLE_RATE as usize / 10;

/// LUFS of a mean square (both channels summed).
fn lufs(mean_square: f64) -> f32 {
    if mean_square <= 1e-12 { -120.0 } else { (-0.691 + 10.0 * mean_square.log10()) as f32 }
}

/// Splits weighted sound into the energy of each tenth of a second.
#[derive(Clone, Copy, Default)]
struct Hops {
    filters: [Weighting; 2],
    sum: f64,
    filled: usize,
}

impl Hops {
    /// Adds a frame; a finished hop's mean square comes back.
    fn add(&mut self, frame: [f32; 2]) -> Option<f64> {
        let (l, r) = (self.filters[0].process(frame[0] as f64), self.filters[1].process(frame[1] as f64));
        self.sum += l * l + r * r;
        self.filled += 1;
        (self.filled == HOP).then(|| {
            let mean = self.sum / HOP as f64;
            (self.sum, self.filled) = (0.0, 0);
            mean
        })
    }
}

/// The loudness of a whole piece and its highest peak.
#[derive(Default)]
pub struct Loudness {
    hops: Hops,
    energies: Vec<f64>,
    peak: f32,
}

impl Loudness {
    /// Adds interleaved stereo.
    pub fn add(&mut self, samples: &[f32]) {
        for frame in samples.as_chunks::<2>().0 {
            self.peak = self.peak.max(frame[0].abs()).max(frame[1].abs());
            if let Some(energy) = self.hops.add(*frame) {
                self.energies.push(energy);
            }
        }
    }

    /// Integrated loudness in LUFS: the average over windows of 0.4 s,
    /// leaving out those below -70 LUFS and then those more than 10 LU
    /// below the average of the rest. `None` for (near) silence.
    pub fn integrated(&self) -> Option<f32> {
        let blocks: Vec<f64> = self.energies.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).collect();
        let mean = |above: f32| {
            let kept: Vec<f64> = blocks.iter().copied().filter(|b| lufs(*b) > above).collect();
            (!kept.is_empty()).then(|| kept.iter().sum::<f64>() / kept.len() as f64)
        };
        let first = mean(-70.0)?;
        mean((lufs(first) - 10.0).max(-70.0)).map(lufs)
    }

    /// The highest sample in decibels below full scale (0 is full scale).
    pub fn peak_db(&self) -> f32 {
        if self.peak <= 1e-6 { -120.0 } else { 20.0 * self.peak.log10() }
    }
}

/// The loudness of the last three seconds, for a meter. No allocation:
/// made for the realtime thread.
pub struct ShortTerm {
    hops: Hops,
    last: [f64; 30],
    at: usize,
}

impl Default for ShortTerm {
    fn default() -> ShortTerm {
        ShortTerm { hops: Hops::default(), last: [0.0; 30], at: 0 }
    }
}

impl ShortTerm {
    /// Adds interleaved stereo; when another tenth of a second is
    /// complete, the loudness of the last three seconds comes back.
    pub fn add(&mut self, samples: &[f32]) -> Option<f32> {
        let mut fresh = None;
        for frame in samples.as_chunks::<2>().0 {
            if let Some(energy) = self.hops.add(*frame) {
                self.last[self.at] = energy;
                self.at = (self.at + 1) % self.last.len();
                fresh = Some(lufs(self.last.iter().sum::<f64>() / self.last.len() as f64));
            }
        }
        fresh
    }

    /// Forgets what was heard (playback starts somewhere else).
    pub fn clear(&mut self) {
        *self = ShortTerm::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hertz: f32, level: f32, seconds: f32) -> Vec<f32> {
        (0..(SAMPLE_RATE as f32 * seconds) as usize)
            .flat_map(|n| {
                let s = level * (std::f32::consts::TAU * hertz * n as f32 / SAMPLE_RATE as f32).sin();
                [s, s]
            })
            .collect()
    }

    #[test]
    fn a_full_scale_tone_at_997_hertz_measures_as_the_standard_says() {
        // The reference case of BS.1770: a 997 Hz sine at full scale in
        // one channel is -3.01 LUFS; the same in both adds 3.01.
        let mut loudness = Loudness::default();
        loudness.add(&tone(997.0, 1.0, 5.0));
        let measured = loudness.integrated().unwrap();
        assert!((measured - 0.0).abs() < 0.15, "{measured}");
        assert!(loudness.peak_db().abs() < 0.01);
        // 20 dB down is 20 LU down.
        let mut quiet = Loudness::default();
        quiet.add(&tone(997.0, 0.1, 5.0));
        assert!((quiet.integrated().unwrap() + 20.0).abs() < 0.15);
    }

    #[test]
    fn silence_between_does_not_pull_the_loudness_down_and_bass_counts_less() {
        let mut with_gaps = Loudness::default();
        with_gaps.add(&tone(997.0, 0.1, 3.0));
        with_gaps.add(&vec![0.0; SAMPLE_RATE as usize * 2 * 6]);
        with_gaps.add(&tone(997.0, 0.1, 3.0));
        assert!((with_gaps.integrated().unwrap() + 20.0).abs() < 0.3, "{:?}", with_gaps.integrated());
        // The ear's filter: the same level at 50 Hz reads several LU lower.
        let mut bass = Loudness::default();
        bass.add(&tone(50.0, 0.1, 5.0));
        assert!(bass.integrated().unwrap() < -23.0, "{:?}", bass.integrated());
        // Nothing at all has no loudness.
        let mut nothing = Loudness::default();
        nothing.add(&vec![0.0; 96_000 * 2]);
        assert_eq!(nothing.integrated(), None);
    }

    #[test]
    fn the_short_term_meter_follows_within_three_seconds() {
        let mut meter = ShortTerm::default();
        let reading = meter.add(&tone(997.0, 0.1, 3.5)).unwrap();
        assert!((reading + 20.0).abs() < 0.2, "{reading}");
        let after = meter.add(&vec![0.0; SAMPLE_RATE as usize * 2 * 4]).unwrap();
        assert!(after < -100.0, "{after}");
    }
}
