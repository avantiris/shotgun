//! The built-in instrument: what the notes of a track sound like when no
//! plugin is on it, so that notes can be written and heard on any machine
//! (docs/spec/09-daw.md). A plain polyphonic synthesiser: two slightly
//! detuned sawtooth waves per note through a low-pass that closes as the
//! note decays. Runs on the disk thread like the plugins it stands in for.

use crate::SAMPLE_RATE;

const VOICES: usize = 16;

#[derive(Clone, Copy, Default)]
struct Voice {
    pitch: u8,
    /// 0: free. The level the note was struck at.
    velocity: f32,
    /// Position in the wave, 0 to 1, of the two oscillators.
    phase: [f32; 2],
    /// Loudness envelope, and whether the key is still down.
    level: f32,
    held: bool,
    /// Frames since the note began, for the attack.
    age: u32,
    filter: f32,
}

pub struct Synth {
    voices: [Voice; VOICES],
}

impl Default for Synth {
    fn default() -> Synth {
        Synth { voices: [Voice::default(); VOICES] }
    }
}

impl Synth {
    fn note(&mut self, on: bool, pitch: u8, velocity: u8) {
        if on {
            // A free voice, or the quietest one.
            let quietest = (0..VOICES).min_by(|a, b| self.voices[*a].level.total_cmp(&self.voices[*b].level));
            let slot = self.voices.iter().position(|v| v.velocity == 0.0).or(quietest).unwrap_or(0);
            self.voices[slot] =
                Voice { pitch, velocity: velocity.max(1) as f32 / 127.0, held: true, ..Voice::default() };
        } else {
            for voice in self.voices.iter_mut().filter(|v| v.pitch == pitch && v.held) {
                voice.held = false;
            }
        }
    }

    /// Adds the sounding notes to `block` (interleaved stereo). `events`
    /// are (frame in the block, on or off, pitch, velocity), in order.
    pub fn process(&mut self, block: &mut [f32], events: &[(u32, bool, u8, u8)]) {
        let rate = SAMPLE_RATE as f32;
        // Up in 4 ms; while held it sinks to 60 % over half a second; released it is gone in 150 ms.
        let (attack, sustain, release) =
            (1.0 / (0.004 * rate), 1.0 - (-1.0 / (0.5 * rate)).exp(), (-1.0 / (0.05 * rate)).exp());
        let mut next = 0;
        for (n, frame) in block.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            while let Some(&(at, on, pitch, velocity)) = events.get(next).filter(|e| e.0 as usize <= n) {
                let _ = at;
                self.note(on, pitch, velocity);
                next += 1;
            }
            let mut sum = 0.0f32;
            for voice in self.voices.iter_mut().filter(|v| v.velocity > 0.0) {
                let hz = 440.0 * 2f32.powf((voice.pitch as f32 - 69.0) / 12.0);
                let mut wave = 0.0;
                for (phase, detune) in voice.phase.iter_mut().zip([0.997f32, 1.003]) {
                    *phase = (*phase + hz * detune / rate).fract();
                    wave += 2.0 * *phase - 1.0;
                }
                if voice.held {
                    let target = if (voice.age as f32) * attack < 1.0 { (voice.age as f32) * attack } else { 0.6 };
                    voice.level += if target > voice.level { attack } else { sustain * (target - voice.level) };
                    voice.level = voice.level.min(1.0);
                } else {
                    voice.level *= release;
                    if voice.level < 1e-4 {
                        voice.velocity = 0.0;
                    }
                }
                voice.age = voice.age.saturating_add(1);
                // Brighter while loud: the low-pass follows the envelope.
                let open = (0.04 + 0.5 * voice.level * voice.velocity).min(0.9);
                voice.filter += open * (wave * 0.5 - voice.filter);
                sum += voice.filter * voice.level * voice.velocity;
            }
            frame[0] += sum * 0.25;
            frame[1] += sum * 0.25;
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

    #[test]
    fn a_note_sounds_from_its_start_and_dies_away_after_its_end() {
        let mut synth = Synth::default();
        let mut block = vec![0.0f32; 2 * 48_000];
        // Middle A from 0.1 s to 0.5 s.
        synth.process(&mut block, &[(4_800, true, 69, 100), (24_000, false, 69, 0)]);
        let level = |from: usize, to: usize| {
            let part = &block[from * 2..to * 2];
            10.0 * (part.iter().map(|s| s * s).sum::<f32>() / part.len() as f32).max(1e-12).log10()
        };
        assert!(level(0, 4_800) < -100.0);
        assert!(level(6_000, 24_000) > -35.0, "{}", level(6_000, 24_000));
        assert!(level(40_000, 48_000) < -80.0, "{}", level(40_000, 48_000));
        // Its pitch: the sound repeats every 109 frames (440 Hz), not every 80.
        let like = |lag: usize| -> f32 { (8_000..16_000).map(|i| block[i * 2] * block[(i + lag) * 2]).sum() };
        assert!(like(109) > like(0) * 0.8 && like(80) < like(0) * 0.5, "{} {} {}", like(0), like(109), like(80));
    }
}
