//! Synthetic music and synthetic "camera recordings" of it, for measuring
//! the analysis code against a known truth (spike S6 and the unit tests).

use std::f32::consts::PI;

const RATE: usize = 48_000;

/// A small deterministic noise source in -1..1.
pub struct Noise(pub u32);

impl Noise {
    pub fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 8) as f32 / (1 << 23) as f32 - 1.0
    }
}

/// Drums, bass, chords and a wandering lead at `bpm`, `seconds` long, with
/// `lead_in` seconds of silence first. Returns the samples and the true beat
/// positions.
pub fn song(bpm: f32, seconds: f32, lead_in: f32, seed: u32) -> (Vec<f32>, Vec<i64>) {
    let total = (seconds * RATE as f32) as usize;
    let beat = 60.0 / bpm * RATE as f32;
    let start = (lead_in * RATE as f32) as usize;
    let mut out = vec![0.0f32; total];
    let mut noise = Noise(seed);
    // Musical choices come from their own source, so that no two bars are alike:
    // a song that repeats exactly has no single right answer.
    let mut choice = Noise(seed ^ 0x9E37_79B9);
    let mut pick = |n: usize| ((choice.next() * 0.5 + 0.5) * n as f32) as usize % n;
    let (mut root, mut section) = (55.0f32, 0usize);
    let mut beats = Vec::new();
    let roots = [55.0f32, 65.4, 49.0, 73.4];
    let count = ((total - start) as f32 / beat) as usize;
    for n in 0..count {
        let at = start + (n as f32 * beat) as usize;
        beats.push(at as i64);
        if n % 4 == 0 {
            root = roots[pick(roots.len())];
        }
        // Every eight bars the arrangement changes: full, no drums, no lead.
        if n % 32 == 0 {
            section = pick(4);
        }
        let (drums, lead_on) = (section != 1, section != 2);
        // Kick on every beat: a falling sine.
        for i in 0..if drums { (0.18 * RATE as f32) as usize } else { 0 } {
            let t = i as f32 / RATE as f32;
            let hz = 50.0 + 90.0 * (-t * 30.0).exp();
            if let Some(s) = out.get_mut(at + i) {
                *s += 0.6 * (2.0 * PI * hz * t).sin() * (-t * 18.0).exp();
            }
        }
        // Snare on two and four: a burst of noise.
        if n % 2 == 1 && drums {
            for i in 0..(0.12 * RATE as f32) as usize {
                let t = i as f32 / RATE as f32;
                if let Some(s) = out.get_mut(at + i) {
                    *s += 0.35 * noise.next() * (-t * 28.0).exp();
                }
            }
        }
        // Hi-hat on the off-beat, sometimes left out.
        if !(seed as usize + n * 7).is_multiple_of(5) {
            let off = at + (beat / 2.0) as usize;
            for i in 0..(0.03 * RATE as f32) as usize {
                let t = i as f32 / RATE as f32;
                if let Some(s) = out.get_mut(off + i) {
                    // Differenced noise: mostly high frequencies.
                    *s += 0.12 * (noise.next() - noise.next()) * (-t * 150.0).exp();
                }
            }
        }
        // Bass and a chord per bar, a lead note that changes irregularly.
        let lead = root * 4.0 * [1.0, 1.2, 1.5, 1.335, 1.78][pick(5)];
        for i in 0..beat as usize {
            let t = i as f32 / RATE as f32;
            let fade = (1.0 - i as f32 / beat).max(0.0);
            if let Some(s) = out.get_mut(at + i) {
                *s += 0.22 * (2.0 * PI * root * t).sin() * fade.sqrt();
                *s += 0.05 * ((2.0 * PI * root * 2.0 * t).sin() + (2.0 * PI * root * 3.0 * t).sin());
                *s += 0.1
                    * (2.0 * PI * lead * t + 2.0 * (2.0 * PI * 5.0 * t).sin()).sin()
                    * fade
                    * if lead_on { 1.0 } else { 0.0 };
            }
        }
    }
    (out, beats)
}

/// How a camera heard the song.
pub struct Camera {
    /// Song sample at which the recording starts (may be negative: the
    /// camera ran before the music).
    pub start: i64,
    pub seconds: f32,
    /// Level of the music over the level of the room noise, in decibels.
    pub snr_db: f32,
    /// The camera's clock against the song's, parts per million (positive:
    /// the camera samples slower, so its recording of the song is shorter).
    pub clock_ppm: f32,
    pub seed: u32,
}

/// A phone in a room with the song playing from a speaker: thin sound,
/// echoes, room noise that comes and goes, clipping.
pub fn record(song: &[f32], camera: &Camera) -> Vec<f32> {
    let len = (camera.seconds * RATE as f32) as usize;
    let mut noise = Noise(camera.seed);
    let step = 1.0 + camera.clock_ppm as f64 * 1e-6;
    // What reaches the microphone: the song, resampled by the clock error.
    let mut direct: Vec<f32> = (0..len)
        .map(|i| {
            let at = camera.start as f64 + i as f64 * step;
            let (whole, part) = (at.floor(), (at - at.floor()) as f32);
            let sample = |k: f64| if k >= 0.0 && (k as usize) < song.len() { song[k as usize] } else { 0.0 };
            sample(whole) * (1.0 - part) + sample(whole + 1.0) * part
        })
        .collect();
    // Echoes of the room.
    for (delay_ms, gain) in [(23.0f32, 0.45f32), (41.0, 0.32), (67.0, 0.25), (113.0, 0.18), (190.0, 0.1)] {
        let delay = (delay_ms * RATE as f32 / 1000.0) as usize;
        for i in (delay..len).rev() {
            direct[i] += gain * direct[i - delay];
        }
    }
    // A small speaker and a small microphone: no bass, little treble.
    let (mut low, mut high) = (0.0f32, 0.0f32);
    let (a_high, a_low) =
        (1.0 - (-2.0 * PI * 250.0 / RATE as f32).exp(), 1.0 - (-2.0 * PI * 4500.0 / RATE as f32).exp());
    for s in direct.iter_mut() {
        high += a_high * (*s - high);
        low += a_low * ((*s - high) - low);
        *s = low;
    }
    let power = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt().max(1e-6);
    let music = power(&direct);
    // Room noise: rumble and chatter with a slowly changing level.
    let mut rumble = 0.0f32;
    let room: Vec<f32> = (0..len)
        .map(|i| {
            rumble += 0.02 * (noise.next() - rumble);
            let swell = 0.6 + 0.4 * (2.0 * PI * 0.31 * i as f32 / RATE as f32 + camera.seed as f32).sin();
            (rumble * 6.0 + 0.3 * noise.next()) * swell
        })
        .collect();
    let scale = music / power(&room) / 10f32.powf(camera.snr_db / 20.0);
    direct
        .iter()
        .zip(&room)
        // The automatic gain of a phone, then its clipping.
        .map(|(s, n)| ((s + n * scale) / music * 0.5).tanh())
        .collect()
}
