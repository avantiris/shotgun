//! Listening to a recorded voice and proposing settings for it: the
//! measuring half of the vocal chain (docs/spec/09-daw.md). Pure signal
//! processing on mono 48 kHz samples; no files, no threads.
//!
//! The numbers a style aims for were measured on the finished vocals of
//! six German rap records on 2026-11-20 (docs/spec/09-daw.md, "Reference
//! measurements"): how bright, how much body, how loud the S sounds, how
//! far the loud phrases stand above the average. They have not been judged
//! by ear (`PROPOSED`).

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;

use crate::analysis::RATE;

const SIZE: usize = 1024;
const HOP: usize = 512;
/// Band edges in hertz: rumble, body, box, middle, presence, air.
const EDGES: [f32; 7] = [40.0, 110.0, 300.0, 700.0, 2500.0, 6000.0, 11_000.0];

/// What a voice recording was measured to be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceStats {
    /// Average level of the parts where the voice sounds, decibels below full scale.
    pub loudness_db: f32,
    /// Level the loud phrases reach (nine tenths of the sounding parts are quieter).
    pub loud_db: f32,
    pub peak_db: f32,
    /// Level between the phrases; `None` if the recording never pauses.
    pub noise_db: Option<f32>,
    /// Typical pitch of the voice in hertz; `None` if none was found.
    pub pitch_hz: Option<f32>,
    /// Share of the voiced moments at which the pitch holds still (0 to 1):
    /// high for singing, low for speech and rap.
    pub steadiness: f32,
    /// Energy of the six bands (rumble, body, box, middle, presence, air)
    /// relative to the middle band, in decibels.
    pub bands_db: [f32; 6],
    /// The key the held notes fit best: root (0 is C, ... 11 is B) and
    /// whether it is minor. `None` if too little is held to tell.
    pub key: Option<(u8, bool)>,
    /// How loud the S sounds are against the voice as a whole, in
    /// decibels; `None` if none were found.
    pub sibilance_db: Option<f32>,
    /// Seconds of sounding voice that were measured.
    pub seconds: f32,
}

/// What kind of vocal a track is treated as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceStyle {
    /// The main voice of a rap track: close, bright, levelled hard.
    RapLead,
    /// Doubles and ad-libs: thinner and further back than the lead.
    Doubles,
    /// A sung hook: fuller and less squeezed.
    SungHook,
}

impl VoiceStyle {
    pub const ALL: [VoiceStyle; 3] = [VoiceStyle::RapLead, VoiceStyle::Doubles, VoiceStyle::SungHook];

    pub fn name(self) -> &'static str {
        match self {
            VoiceStyle::RapLead => "Rap lead",
            VoiceStyle::Doubles => "Doubles and ad-libs",
            VoiceStyle::SungHook => "Sung hook",
        }
    }

    /// The style a recording most likely wants: sung if the pitch holds
    /// still most of the time, otherwise the lead of a rap.
    pub fn guess(stats: &VoiceStats) -> VoiceStyle {
        // Measured: rapped verses hold their pitch at 17 to 28 % of the
        // voiced moments, sung and pitch-corrected hooks at 40 to 51 %.
        if stats.pitch_hz.is_some() && stats.steadiness >= 0.35 { VoiceStyle::SungHook } else { VoiceStyle::RapLead }
    }

    /// (air against the middle in dB, body or box allowed above the middle
    /// in dB, how far the loud phrases may stand above the average in dB,
    /// ratio, level after the chain in dB, low cut as a share of the
    /// voice's pitch)
    fn target(self) -> (f32, f32, f32, f32, f32, f32) {
        match self {
            // The references: air -10 to -2 (median -8.7), body and box
            // around the middle (-2 and +0.6), loud phrases 12 dB above
            // the average (10.9 to 13.4).
            VoiceStyle::RapLead => (-8.5, 0.5, 12.0, 4.0, -17.0, 0.6),
            VoiceStyle::Doubles => (-7.0, -2.0, 10.0, 6.0, -23.0, 0.85),
            VoiceStyle::SungHook => (-9.5, 1.5, 12.5, 3.0, -17.0, 0.55),
        }
    }
}

/// How loud the S sounds of a finished vocal are against the voice, in
/// decibels (the references: -5.1 to -0.1, four of six between -4.3 and -5.1).
const SIBILANCE_TARGET_DB: f32 = -4.5;

/// Settings of a track's strip proposed for a voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suggestion {
    pub style: VoiceStyle,
    pub gain_db: f32,
    pub low_cut_hz: f32,
    pub denoise: f32,
    pub eq_low_db: f32,
    pub eq_mid_db: f32,
    pub eq_mid_hz: f32,
    pub eq_high_db: f32,
    pub comp_threshold_db: f32,
    pub comp_ratio: f32,
    /// De-esser, 0 to 1.
    pub deess: f32,
    /// Pitch correction: how much (0 is off), and the key it corrects to
    /// (root, and scale: 0 every semitone, 1 major, 2 minor).
    pub tune: f32,
    pub tune_key: u8,
    pub tune_scale: u8,
    pub tune_speed: f32,
    /// Leveller and saturation, 0 to 1.
    pub level: f32,
    pub drive: f32,
    /// How far the other tracks should go down under this voice, in decibels.
    pub duck_db: f32,
}

fn db(power: f64) -> f32 {
    (10.0 * power.max(1e-12).log10()) as f32
}

fn percentile(sorted: &[f32], share: f32) -> f32 {
    sorted[((sorted.len() - 1) as f32 * share).round() as usize]
}

/// Measures a recording. `None` if less than half a second of it sounds.
pub fn measure(samples: &[f32]) -> Option<VoiceStats> {
    if samples.len() < SIZE * 4 {
        return None;
    }
    let fft = FftPlanner::<f32>::new().plan_fft_forward(SIZE);
    let window: Vec<f32> =
        (0..SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / SIZE as f32).cos()).collect();
    let bin = |hz: f32| ((hz * SIZE as f32 / RATE as f32) as usize).clamp(1, SIZE / 2);
    let frames = (samples.len() - SIZE) / HOP + 1;
    let level: Vec<f32> = (0..frames)
        .map(|f| {
            let part = &samples[f * HOP..f * HOP + SIZE];
            db(part.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / SIZE as f64)
        })
        .collect();
    let loudest = level.iter().copied().fold(f32::MIN, f32::max);
    // The voice sounds where the level is within 35 dB of its loudest.
    let floor = (loudest - 35.0).max(-70.0);
    let sounding: Vec<usize> = (0..frames).filter(|f| level[*f] > floor).collect();
    let seconds = sounding.len() as f32 * HOP as f32 / RATE as f32;
    if seconds < 0.5 {
        return None;
    }
    let mut power = 0.0f64;
    let mut bands = [0.0f64; 6];
    let mut peaks: Vec<f32> = Vec::with_capacity(sounding.len());
    // Power of the frames that are S sounds: most of their energy lies above 4.5 kHz.
    let (mut sharp_power, mut sharp_frames) = (0.0f64, 0usize);
    let mut buffer = vec![Complex32::default(); SIZE];
    for &f in &sounding {
        let part = &samples[f * HOP..f * HOP + SIZE];
        power += part.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / SIZE as f64;
        peaks.push(20.0 * part.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6).log10());
        for (slot, (sample, w)) in buffer.iter_mut().zip(part.iter().zip(&window)) {
            *slot = Complex32::new(sample * w, 0.0);
        }
        fft.process(&mut buffer);
        let above: f64 = buffer[bin(4500.0)..SIZE / 2].iter().map(|c| c.norm_sqr() as f64).sum();
        let all: f64 = buffer[1..SIZE / 2].iter().map(|c| c.norm_sqr() as f64).sum();
        if above > all * 0.5 {
            sharp_power += part.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / SIZE as f64;
            sharp_frames += 1;
        }
        for (band, energy) in bands.iter_mut().enumerate() {
            let (low, high) = (bin(EDGES[band]), bin(EDGES[band + 1]));
            *energy += buffer[low..high.max(low + 1)].iter().map(|c| c.norm_sqr() as f64).sum::<f64>();
        }
    }
    peaks.sort_by(f32::total_cmp);
    // Between the phrases: what is clearly below the voice. Too little of it says nothing.
    let mut gaps: Vec<f32> = level.iter().copied().filter(|l| *l <= floor).collect();
    gaps.sort_by(f32::total_cmp);
    let noise_db = (gaps.len() >= 20).then(|| percentile(&gaps, 0.5).max(-110.0));
    let (pitch_hz, steadiness, key) = pitch(samples, &sounding);
    let middle = bands[3].max(1e-12);
    let loudness_db = db(power / sounding.len() as f64);
    Some(VoiceStats {
        key,
        sibilance_db: (sharp_frames >= 5).then(|| db(sharp_power / sharp_frames as f64) - loudness_db),
        loudness_db,
        loud_db: percentile(&peaks, 0.9),
        peak_db: *peaks.last()?,
        noise_db,
        pitch_hz,
        steadiness,
        bands_db: bands.map(|b| db(b / middle)),
        seconds,
    })
}

/// The typical pitch and how still it holds, from the autocorrelation of
/// the sounding frames at a quarter of the sample rate.
fn pitch(samples: &[f32], sounding: &[usize]) -> (Option<f32>, f32, Option<(u8, bool)>) {
    const STEP: usize = 4;
    let rate = RATE / STEP;
    // 70 to 400 Hz, in samples of the reduced rate.
    let (short, long) = (rate / 400, rate / 70);
    let span = long * 3;
    let mut found: Vec<(usize, f32)> = Vec::new();
    // Every fourth frame: about 43 ms apart.
    for &f in sounding.iter().step_by(4) {
        let start = f * HOP;
        if start + (span + long) * STEP > samples.len() {
            continue;
        }
        // Only the lowest harmonics tell the pitch; the bright ones would
        // punish a lag that is half a (reduced) sample off and favour the
        // octave below, whose lag is twice as fine. Two gentle low-passes
        // at about 900 Hz, then every fourth sample.
        let ease = 1.0 - (-2.0 * std::f32::consts::PI * 900.0 / RATE as f32).exp();
        let (mut one, mut two) = (0.0f32, 0.0f32);
        let small: Vec<f32> = samples[start..start + (span + long) * STEP]
            .iter()
            .map(|sample| {
                one += ease * (sample - one);
                two += ease * (one - two);
                two
            })
            .step_by(STEP)
            .collect();
        let energy: f32 = small[..span].iter().map(|s| s * s).sum();
        if energy < 1e-7 {
            continue;
        }
        let score = |lag: usize| -> f32 {
            let cross: f32 = small[..span].iter().zip(&small[lag..lag + span]).map(|(a, b)| a * b).sum();
            let other: f32 = small[lag..lag + span].iter().map(|s| s * s).sum();
            cross / (energy * other).sqrt().max(1e-9)
        };
        // (One lag more at each end, to see whether an end is a peak. The
        // window has `long` samples to spare, so the short end is the only bound.)
        let scores: Vec<f32> = (short - 1..=long).map(score).collect();
        let best = scores[1..].iter().copied().fold(0.0f32, f32::max);
        if best < 0.6 {
            continue;
        }
        // The shortest lag nearly as good as the best at which the likeness
        // peaks: the pitch, not one of its octaves below, and not the slope
        // just before the peak, which would read every note sharp.
        let peak = (1..scores.len() - 1)
            .find(|i| scores[*i] >= best * 0.93 && scores[*i] >= scores[*i - 1] && scores[*i] >= scores[*i + 1]);
        if let Some(i) = peak {
            // Between the reduced samples: the top of the curve through three scores.
            let (l, m, r) = (scores[i - 1], scores[i], scores[i + 1]);
            let bend = l - 2.0 * m + r;
            let offset = if bend.abs() > 1e-9 { (0.5 * (l - r) / bend).clamp(-0.5, 0.5) } else { 0.0 };
            found.push((f, rate as f32 / ((short - 1 + i) as f32 + offset)));
        }
    }
    if found.len() < 5 {
        return (None, 0.0, None);
    }
    let mut pitches: Vec<f32> = found.iter().map(|(_, hz)| *hz).collect();
    pitches.sort_by(f32::total_cmp);
    let neighbours: Vec<(f32, f32)> =
        found.windows(2).filter(|w| w[1].0 - w[0].0 <= 4).map(|w| (w[0].1, w[1].1)).collect();
    // Still: within a third of a semitone of the moment before.
    let still = neighbours.iter().filter(|(a, b)| (a / b).ln().abs() < 0.02).count();
    // The notes that are held say which key it is in.
    let mut held = [0.0f32; 12];
    for (a, b) in neighbours.iter().filter(|(a, b)| (a / b).ln().abs() < 0.02) {
        let note = 69.0 + 12.0 * ((a * b).sqrt() / 440.0).log2();
        // (A note sung between two semitones counts for neither.)
        if (note - note.round()).abs() < 0.35 {
            held[(note.round() as i32).rem_euclid(12) as usize] += 1.0;
        }
    }
    (Some(percentile(&pitches, 0.5)), still as f32 / neighbours.len().max(1) as f32, key_of(&held))
}

/// The key whose scale holds most of the held notes; its root and its
/// fifth count extra, which tells a minor key from the major one with the
/// same notes. `None` below twenty held moments or if a fifth of them
/// would lie outside every scale.
fn key_of(held: &[f32; 12]) -> Option<(u8, bool)> {
    let total: f32 = held.iter().sum();
    if total < 20.0 {
        return None;
    }
    let mut best: Option<(f32, f32, u8, bool)> = None;
    for root in 0..12usize {
        for (minor, scale) in [(false, [0, 2, 4, 5, 7, 9, 11]), (true, [0, 2, 3, 5, 7, 8, 10])] {
            let inside: f32 = scale.iter().map(|step| held[(root + step) % 12]).sum();
            let score = inside + 0.5 * held[root] + 0.25 * held[(root + 7) % 12];
            if best.is_none_or(|(top, ..)| score > top) {
                best = Some((score, inside, root as u8, minor));
            }
        }
    }
    best.filter(|(_, inside, ..)| *inside >= total * 0.8).map(|(_, _, root, minor)| (root, minor))
}

/// The strip settings that bring a measured voice to a style.
pub fn suggest(stats: &VoiceStats, style: VoiceStyle) -> Suggestion {
    let (air_target, body_allowed, spread, ratio, level_target, cut_share) = style.target();
    // Well below the voice's own pitch there is only rumble.
    let low_cut_hz = stats.pitch_hz.map_or(90.0, |hz| (hz * cut_share).clamp(75.0, 140.0)).round();
    // The loud phrases are brought down to where they stand on the records.
    let reduction = (stats.loud_db - stats.loudness_db - spread).clamp(2.0, 14.0);
    // The quieter the pauses against the voice, the less there is to remove.
    let denoise = match stats.noise_db.map(|noise| stats.loudness_db - noise) {
        // (Gently: the noise reduction is made for speech and costs a little of the voice.)
        Some(clear) if clear < 45.0 => ((45.0 - clear) / 25.0 * 0.7).clamp(0.1, 0.7),
        _ => 0.0,
    };
    // Too much body (mud, about 200 Hz) or box (about 450 Hz) against the
    // middle: the worse of the two is turned down, half of the excess.
    let (body, boxy, air) = (stats.bands_db[1], stats.bands_db[2], stats.bands_db[5]);
    let (excess, eq_mid_hz) = if body >= boxy { (body - body_allowed, 220.0) } else { (boxy - body_allowed, 450.0) };
    let eq_mid_db = -(excess / 2.0).clamp(0.0, 6.0);
    // Dull against the style: air is added, most of the difference. Sharp: a little is taken.
    let eq_high_db = ((air_target - air) * 0.6).clamp(-4.0, 9.0);
    let eq_low_db = if stats.bands_db[0] > -12.0 { -2.0 } else { 0.0 };
    // The threshold at which the loud phrases are turned down by `reduction`.
    let slope = 1.0 - 1.0 / ratio;
    let comp_threshold_db = (stats.loud_db - reduction / slope).clamp(-45.0, -6.0);
    // What is left after the compressor (it has no make-up gain), brought to the style's level.
    let average_loss = ((stats.loudness_db + 3.0 - comp_threshold_db).max(0.0) * slope).min(reduction);
    let gain_db = (level_target - (stats.loudness_db - average_loss)).clamp(-12.0, 12.0);
    let round = |v: f32| (v * 10.0).round() / 10.0;
    // The S sounds are brought to where they are on the records. Added air
    // makes them sharper by about as much; the de-esser takes off up to 12 dB.
    let deess =
        stats.sibilance_db.map_or(0.0, |s| ((s + eq_high_db.max(0.0) - SIBILANCE_TARGET_DB) / 12.0).clamp(0.0, 1.0));
    Suggestion {
        style,
        gain_db: round(gain_db),
        low_cut_hz,
        denoise: (denoise * 20.0).round() / 20.0,
        eq_low_db,
        eq_mid_db: round(eq_mid_db),
        eq_mid_hz,
        eq_high_db: round(eq_high_db),
        comp_threshold_db: round(comp_threshold_db),
        comp_ratio: ratio,
        deess: (deess * 20.0).round() / 20.0,
        // A sung hook is corrected, fast (the sound of the records); a rap
        // verse is left alone, but the key is set so that turning the
        // correction up does the right thing. Without a key: every semitone.
        tune: if style == VoiceStyle::SungHook { 1.0 } else { 0.0 },
        tune_key: stats.key.map_or(0, |(root, _)| root),
        tune_scale: stats.key.map_or(0, |(_, minor)| if minor { 2 } else { 1 }),
        tune_speed: 0.15,
        // (Choices, not measurements: how even, how dense, and how much room the beat makes.)
        level: match style {
            VoiceStyle::RapLead => 0.6,
            VoiceStyle::Doubles => 0.7,
            VoiceStyle::SungHook => 0.5,
        },
        drive: match style {
            VoiceStyle::RapLead => 0.25,
            VoiceStyle::Doubles => 0.35,
            VoiceStyle::SungHook => 0.15,
        },
        duck_db: match style {
            VoiceStyle::RapLead => 3.0,
            VoiceStyle::Doubles => 0.0,
            VoiceStyle::SungHook => 2.0,
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Phrases of a voice-like sound with pauses: harmonics of `pitch`
    /// falling off by `tilt` decibels per octave, hiss at `hiss` (linear).
    /// `glide` makes the pitch wander as in speech.
    fn take(pitch: f32, tilt: f32, hiss: f32, glide: bool) -> Vec<f32> {
        take_with_s(pitch, tilt, hiss, glide, 0.0)
    }

    /// The same with S sounds of amplitude `sharp` at the end of each phrase.
    fn take_with_s(pitch: f32, tilt: f32, hiss: f32, glide: bool, sharp: f32) -> Vec<f32> {
        let mut seed = 0x2545_f491u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        let mut phase = 0.0f32;
        (0..RATE * 6)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                // Three quarters of a second of voice, a quarter of pause.
                let sounding = t.fract() < 0.75;
                let hz = if glide { pitch * (1.0 + 0.25 * (t * 7.0).sin() + 0.1 * (t * 23.0).sin()) } else { pitch };
                phase += 2.0 * std::f32::consts::PI * hz / RATE as f32;
                let voice: f32 = (1..=40)
                    .filter(|h| hz * (*h as f32) < 10_000.0)
                    .map(|h| (phase * h as f32).sin() * 10f32.powf(-tilt * (h as f32).log2() / 20.0))
                    .sum();
                // The last tenth of a second of each phrase is an S, if asked for.
                let s = sounding && t.fract() >= 0.65;
                if s && sharp > 0.0 {
                    // (The difference of two noise samples has its energy at the top.)
                    return sharp * (noise() - noise()) + hiss * noise();
                }
                (if sounding { 0.12 * voice } else { 0.0 }) + hiss * noise()
            })
            .collect()
    }

    #[test]
    fn a_take_is_measured_and_a_rap_lead_set_up_from_it() {
        // A man's speaking voice, rather dull, with hiss behind it.
        let stats = measure(&take(115.0, 9.0, 0.003, true)).unwrap();
        assert!((stats.pitch_hz.unwrap() - 115.0).abs() < 25.0, "{stats:?}");
        assert!(stats.steadiness < 0.6, "{stats:?}");
        assert!((3.5..5.5).contains(&stats.seconds), "{stats:?}");
        let clear = stats.loudness_db - stats.noise_db.unwrap();
        assert!((20.0..45.0).contains(&clear), "{stats:?}");
        assert_eq!(VoiceStyle::guess(&stats), VoiceStyle::RapLead);
        let set = suggest(&stats, VoiceStyle::RapLead);
        // Low cut under the voice, noise reduction because of the hiss, air because it is dull.
        assert!((70.0..=120.0).contains(&set.low_cut_hz), "{set:?}");
        assert!(set.denoise >= 0.2, "{set:?}");
        assert!(set.eq_high_db > 2.0, "{set:?}");
        // This made-up voice is even already (its loud phrases 3 dB above its
        // average, where a record has 12): the compressor only touches it, 2 dB.
        assert!((stats.loud_db - set.comp_threshold_db - 2.0 / 0.75).abs() < 0.2, "{set:?} {stats:?}");
        // A take that swings 20 dB is brought to the records' 12: 8 dB off its loud phrases.
        let swinging = VoiceStats { loud_db: -6.0, loudness_db: -26.0, ..stats };
        let squeezed = suggest(&swinging, VoiceStyle::RapLead);
        assert!((-6.0 - squeezed.comp_threshold_db - 8.0 / 0.75).abs() < 0.2, "{squeezed:?}");
        assert_eq!(set.comp_ratio, 4.0);
        // Doubles sit further back and thinner than the lead.
        let doubles = suggest(&stats, VoiceStyle::Doubles);
        assert!(doubles.gain_db < set.gain_db && doubles.low_cut_hz > set.low_cut_hz, "{doubles:?}");
    }

    #[test]
    fn a_clean_bright_sung_take_needs_little() {
        // Held notes, bright, no hiss.
        let stats = measure(&take(220.0, 4.0, 0.0, false)).unwrap();
        assert!((stats.pitch_hz.unwrap() - 220.0).abs() < 8.0, "{stats:?}");
        assert!(stats.steadiness > 0.8, "{stats:?}");
        assert_eq!(VoiceStyle::guess(&stats), VoiceStyle::SungHook);
        let set = suggest(&stats, VoiceStyle::SungHook);
        assert_eq!(set.denoise, 0.0, "{set:?}");
        assert!(set.eq_high_db <= 0.0, "{set:?}");
        assert_eq!(set.comp_ratio, 3.0);
    }

    #[test]
    fn sharp_s_sounds_are_found_and_the_de_esser_set_by_how_loud_they_are() {
        // No S at all: nothing found, de-esser off.
        let plain = measure(&take(115.0, 9.0, 0.0, true)).unwrap();
        assert_eq!(plain.sibilance_db, None, "{plain:?}");
        assert_eq!(suggest(&plain, VoiceStyle::RapLead).deess, 0.0);
        // Mild ones, well under the voice: little; as loud as the voice: a lot.
        let mild = measure(&take_with_s(115.0, 9.0, 0.0, true, 0.02)).unwrap();
        let harsh = measure(&take_with_s(115.0, 9.0, 0.0, true, 0.2)).unwrap();
        let (m, h) = (mild.sibilance_db.unwrap(), harsh.sibilance_db.unwrap());
        assert!(m < -8.0 && h > -3.0, "{m} {h}");
        let (little, much) = (suggest(&mild, VoiceStyle::RapLead).deess, suggest(&harsh, VoiceStyle::RapLead).deess);
        assert!(little < much && much >= 0.5, "{little} {much}");
    }

    #[test]
    fn the_key_is_told_from_the_notes_that_are_held() {
        // A melody in A minor that keeps coming home to A: A C D E G, half a second each.
        let melody = [220.0, 261.63, 293.66, 329.63, 220.0, 392.0, 329.63, 220.0, 261.63, 220.0, 293.66, 220.0];
        let mut phase = 0.0f32;
        let sung: Vec<f32> = (0..RATE * 6)
            .map(|i| {
                let hz = melody[(i / (RATE / 2)) % melody.len()];
                phase += 2.0 * std::f32::consts::PI * hz / RATE as f32;
                (1..=10).map(|h| (phase * h as f32).sin() / h as f32).sum::<f32>() * 0.15
            })
            .collect();
        let stats = measure(&sung).unwrap();
        assert_eq!(stats.key, Some((9, true)), "{stats:?}");
        let set = suggest(&stats, VoiceStyle::guess(&stats));
        assert_eq!((set.style, set.tune, set.tune_key, set.tune_scale), (VoiceStyle::SungHook, 1.0, 9, 2));
        // A rap verse is not corrected, and a voice that wanders has no key.
        let spoken = measure(&take(115.0, 9.0, 0.0, true)).unwrap();
        assert_eq!(spoken.key, None);
        assert_eq!(suggest(&spoken, VoiceStyle::RapLead).tune, 0.0);
    }

    #[test]
    fn silence_and_scraps_are_not_measured() {
        assert_eq!(measure(&vec![0.0; RATE * 2]), None);
        assert_eq!(measure(&[0.1; 100]), None);
    }
}
