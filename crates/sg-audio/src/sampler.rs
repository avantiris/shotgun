//! The built-in sampler: the notes of a track play a recording, higher
//! or lower with the key (docs/spec/09-daw.md, "Sound"): a bass note, a
//! drum hit, a chopped phrase. Runs on the disk thread like the built-in
//! synthesiser it takes the place of.
//!
//! The recording is the project's conformed sound of an asset, read whole
//! into memory when the graph is built (at most [`MOST_FRAMES`] of it).

use std::sync::Arc;

const VOICES: usize = 16;
/// The longest recording a sampler holds: half a minute.
pub const MOST_FRAMES: usize = crate::SAMPLE_RATE as usize * 30;
/// Frames over which a voice that ends is faded out (20 ms): no click.
const RELEASE: f32 = 960.0;

#[derive(Clone, Copy, Default)]
struct Voice {
    pitch: u8,
    /// 0: free.
    velocity: f32,
    /// Where in the recording the voice is, in frames.
    at: f64,
    /// Frames of recording per frame of output: 1 at the root key, 2 an octave above.
    step: f64,
    held: bool,
    /// 1 while it sounds, falling to 0 once it has been let go.
    fade: f32,
}

pub struct Sampler {
    /// Interleaved stereo at the engine's rate.
    sound: Arc<Vec<f32>>,
    /// The key at which the recording plays as it is.
    root: u8,
    /// A hit: every note plays the recording to its end, however short the note.
    one_shot: bool,
    voices: [Voice; VOICES],
}

impl Sampler {
    pub fn new(sound: Arc<Vec<f32>>, root: u8, one_shot: bool) -> Sampler {
        Sampler { sound, root, one_shot, voices: [Voice::default(); VOICES] }
    }

    fn note(&mut self, on: bool, pitch: u8, velocity: u8) {
        if on {
            // A free voice, or the one furthest along.
            let furthest = (0..VOICES).max_by(|a, b| self.voices[*a].at.total_cmp(&self.voices[*b].at));
            let slot = self.voices.iter().position(|v| v.velocity == 0.0).or(furthest).unwrap_or(0);
            self.voices[slot] = Voice {
                pitch,
                velocity: velocity.max(1) as f32 / 127.0,
                at: 0.0,
                step: 2f64.powf((pitch as f64 - self.root as f64) / 12.0),
                held: true,
                fade: 1.0,
            };
        } else if !self.one_shot {
            for voice in self.voices.iter_mut().filter(|v| v.pitch == pitch && v.held) {
                voice.held = false;
            }
        }
    }

    /// Adds the sounding notes to `block` (interleaved stereo). `events`
    /// are (frame in the block, on or off, pitch, velocity), in order.
    pub fn process(&mut self, block: &mut [f32], events: &[(u32, bool, u8, u8)]) {
        let frames = self.sound.len() / 2;
        let mut next = 0;
        for (n, frame) in block.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            while let Some(&(_, on, pitch, velocity)) = events.get(next).filter(|e| e.0 as usize <= n) {
                self.note(on, pitch, velocity);
                next += 1;
            }
            for voice in self.voices.iter_mut().filter(|v| v.velocity > 0.0) {
                let index = voice.at as usize;
                if index + 1 >= frames {
                    voice.velocity = 0.0;
                    continue;
                }
                // Between two frames of the recording: a straight line.
                let t = (voice.at - index as f64) as f32;
                let gain = voice.velocity * voice.fade;
                for (side, out) in frame.iter_mut().enumerate() {
                    let (a, b) = (self.sound[index * 2 + side], self.sound[index * 2 + 2 + side]);
                    *out += (a + (b - a) * t) * gain;
                }
                voice.at += voice.step;
                if !voice.held {
                    voice.fade -= 1.0 / RELEASE;
                    if voice.fade <= 0.0 {
                        voice.velocity = 0.0;
                    }
                }
            }
        }
        // (Events past the block's end still count.)
        for &(_, on, pitch, velocity) in &events[next..] {
            self.note(on, pitch, velocity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of a ramp: each frame's value says where in the recording it is.
    fn ramp() -> Arc<Vec<f32>> {
        Arc::new((0..48_000).flat_map(|n| [n as f32 / 48_000.0; 2]).collect())
    }

    #[test]
    fn a_key_plays_the_recording_faster_the_higher_it_is_and_ends_with_the_note() {
        let mut sampler = Sampler::new(ramp(), 60, false);
        let mut block = vec![0.0f32; 2 * 9_600];
        // The root key from frame 100: the recording as it is, at the note's level.
        sampler.process(&mut block, &[(100, true, 60, 127)]);
        assert_eq!(block[2 * 99], 0.0);
        assert!((block[2 * 1_100] - 1_000.0 / 48_000.0).abs() < 1e-5, "{}", block[2 * 1_100]);
        // Let go, it is gone within the release.
        let mut after = vec![0.0f32; 2 * 4_800];
        sampler.process(&mut after, &[(0, false, 60, 0)]);
        assert!(after[2 * 100] > 0.0 && after[2 * 2_000..].iter().all(|s| *s == 0.0));
        // An octave up it runs through the recording twice as fast.
        let mut high = vec![0.0f32; 2 * 4_800];
        sampler.process(&mut high, &[(0, true, 72, 127)]);
        assert!((high[2 * 1_000] - 2_000.0 / 48_000.0).abs() < 1e-5, "{}", high[2 * 1_000]);
    }

    #[test]
    fn a_hit_plays_to_its_end_however_short_the_note() {
        let mut sampler = Sampler::new(ramp(), 60, true);
        let mut block = vec![0.0f32; 2 * 60_000];
        sampler.process(&mut block, &[(0, true, 60, 64), (480, false, 60, 0)]);
        // Still sounding long after the note ended, and silent once the recording is over.
        assert!(block[2 * 40_000] > 0.3);
        assert!(block[2 * 48_100..].iter().all(|s| *s == 0.0));
    }
}
