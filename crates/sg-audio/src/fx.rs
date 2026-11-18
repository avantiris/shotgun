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
        let w = 2.0 * std::f32::consts::PI * hz.clamp(10.0, 2000.0) / SAMPLE_RATE as f32;
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

/// The clean-up of one track.
pub struct TrackFx {
    filters: [HighPass; 2],
    cut_hz: f32,
    denoiser: Option<Denoiser>,
}

impl TrackFx {
    /// `denoise` decides whether the stream will carry the noise
    /// reduction's delay; it cannot change afterwards.
    pub fn new(denoise: bool) -> TrackFx {
        TrackFx { filters: [HighPass::default(); 2], cut_hz: 0.0, denoiser: denoise.then(Denoiser::new) }
    }

    /// Frames the output trails the input.
    pub fn delay(&self) -> usize {
        if self.denoiser.is_some() { LOOKAHEAD } else { 0 }
    }

    /// Treats interleaved stereo in place. `low_cut_hz` of 0 switches the
    /// filter off; `denoise` is the share of the treated sound, 0 to 1.
    pub fn process(&mut self, block: &mut [f32], low_cut_hz: f32, denoise: f32) {
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
    fn noise_reduction_delay_is_what_the_feed_compensates() {
        let input = voice(48_000 * 2);
        let mut output = input.clone();
        let mut fx = TrackFx::new(true);
        // Odd block sizes: the stream must not care.
        for block in output.chunks_mut(2 * 333) {
            fx.process(block, 0.0, 1.0);
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
        let mut fx = TrackFx::new(true);
        fx.process(&mut treated, 0.0, 1.0);
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
    fn low_cut_removes_rumble_and_keeps_the_rest() {
        let tone = |hz: f32| -> Vec<f32> {
            (0..48_000).flat_map(|i| [(2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin(); 2]).collect()
        };
        let level = |hz: f32| {
            let mut block = tone(hz);
            TrackFx::new(false).process(&mut block, 100.0, 0.0);
            rms(block[24_000..].iter().copied())
        };
        assert!(level(25.0) < 0.06, "{}", level(25.0));
        assert!((level(100.0) - 0.5).abs() < 0.03, "{}", level(100.0));
        assert!(level(1000.0) > 0.7);
    }
}
