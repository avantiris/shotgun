//! Audio analysis: where a take's camera sound sits in the song, and where
//! the song's beats are. Pure signal processing on mono 48 kHz samples; no
//! files, no threads. Method and measured accuracy: `docs/spikes/s6-sync-beats.md`.

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;
use serde::{Deserialize, Serialize};

/// Everything here works on samples at this rate.
pub const RATE: usize = 48_000;

/// Averages interleaved channels into one.
pub fn mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    let channels = channels.max(1);
    interleaved.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect()
}

const BANDS: usize = 32;

/// How strongly the sound changes over time, per frequency band: the rise
/// of the (log-compressed) energy in 32 bands, one row per `hop` samples.
/// Onsets of notes and drums show as peaks whatever the microphone did to
/// the timbre, which is what makes a phone recording comparable with the
/// studio file; keeping the bands apart keeps *which* notes they were.
pub fn band_flux(samples: &[f32], hop: usize) -> Vec<[f32; BANDS]> {
    const SIZE: usize = 1024;
    if samples.len() < SIZE {
        return Vec::new();
    }
    let fft = FftPlanner::<f32>::new().plan_fft_forward(SIZE);
    let window: Vec<f32> =
        (0..SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / SIZE as f32).cos()).collect();
    // Band edges from 60 Hz to 12 kHz, evenly spaced in pitch.
    let edge = |band: usize| {
        let hz = 60.0 * (12_000.0f32 / 60.0).powf(band as f32 / BANDS as f32);
        ((hz * SIZE as f32 / RATE as f32) as usize).clamp(1, SIZE / 2)
    };
    let frames = (samples.len() - SIZE) / hop + 1;
    let mut rows = Vec::with_capacity(frames);
    let mut buffer = vec![Complex32::default(); SIZE];
    let mut previous = [0.0f32; BANDS];
    for frame in 0..frames {
        let at = frame * hop;
        for (slot, (sample, w)) in buffer.iter_mut().zip(samples[at..at + SIZE].iter().zip(&window)) {
            *slot = Complex32::new(sample * w, 0.0);
        }
        fft.process(&mut buffer);
        let mut row = [0.0f32; BANDS];
        for (band, before) in previous.iter_mut().enumerate() {
            let (low, high) = (edge(band), edge(band + 1).max(edge(band) + 1));
            let energy: f32 = buffer[low..high.min(SIZE / 2)].iter().map(|c| c.norm()).sum();
            let level = (1.0 + 100.0 * energy / SIZE as f32).ln();
            row[band] = (level - *before).max(0.0);
            *before = level;
        }
        // The first frame has nothing before it to rise from.
        rows.push(if frame == 0 { [0.0; BANDS] } else { row });
    }
    rows
}

/// The bands of [`band_flux`] summed: one number per `hop` samples that
/// peaks where something starts.
pub fn onset_envelope(samples: &[f32], hop: usize) -> Vec<f32> {
    band_flux(samples, hop).iter().map(|row| row.iter().sum()).collect()
}

/// Subtracts the mean and scales to unit deviation, in place.
fn normalise(values: &mut [f32]) {
    if values.is_empty() {
        return;
    }
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let deviation = (values.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / values.len() as f32).sqrt();
    let scale = if deviation > 1e-9 { 1.0 / deviation } else { 0.0 };
    values.iter_mut().for_each(|v| *v = (*v - mean) * scale);
}

/// `c[k] = sum over i of b[i] * a[i + k - (b.len() - 1)]`: the correlation of
/// `b` slid along `a`, for every lag from `-(b.len() - 1)` to `a.len() - 1`.
fn correlate(a: &[f32], b: &[f32]) -> Vec<f32> {
    let size = (a.len() + b.len()).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let (forward, inverse) = (planner.plan_fft_forward(size), planner.plan_fft_inverse(size));
    let lift = |values: &[f32]| {
        let mut buffer = vec![Complex32::default(); size];
        for (slot, v) in buffer.iter_mut().zip(values) {
            *slot = Complex32::new(*v, 0.0);
        }
        forward.process(&mut buffer);
        buffer
    };
    let (fa, fb) = (lift(a), lift(b));
    let mut product: Vec<Complex32> = fa.iter().zip(&fb).map(|(x, y)| x * y.conj()).collect();
    inverse.process(&mut product);
    let lags = a.len() + b.len() - 1;
    // Negative lags wrap to the end of the buffer.
    (0..lags)
        .map(|k| {
            let lag = k as isize - (b.len() as isize - 1);
            product[lag.rem_euclid(size as isize) as usize].re / size as f32
        })
        .collect()
}

/// Where a take sits in the song.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offset {
    /// Song sample at which the take's first sample belongs if the take is
    /// laid so that its middle is exactly right (negative: the take starts
    /// before the song).
    pub lag: i64,
    /// How alike the waveforms are at this position: the correlation of
    /// half-second stretches of the take with the song, averaged over the
    /// take. 1 is identical; a phone in a loud room reaches 0.2 to 0.6.
    pub likeness: f32,
    /// Likeness of the next best position, for comparison.
    pub runner_up: f32,
    /// The take's clock against the song's in parts per million. Positive:
    /// the take plays the song faster than the song file does (its
    /// recording of the song is shorter than the song).
    pub drift_ppm: f32,
}

impl Offset {
    /// Whether the match is good enough to place the take without asking
    /// (thresholds from spike S6).
    pub fn is_confident(&self) -> bool {
        self.likeness >= 0.15 && self.likeness >= self.runner_up * 1.4
    }
}

/// Rate of the waveform comparison: enough for the body of music and voice,
/// and a sixth of the work.
const FINE_RATE: usize = 8000;

/// Mono 48 kHz down to [`FINE_RATE`], without the bass (a phone hears none)
/// and without what would alias.
fn fine(samples: &[f32]) -> Vec<f32> {
    let step = RATE / FINE_RATE;
    let mut slow = 0.0f32;
    samples
        .chunks_exact(step)
        .map(|c| {
            let v = c.iter().sum::<f32>() / step as f32;
            // One-pole high-pass at about 150 Hz.
            slow += 0.11 * (v - slow);
            v - slow
        })
        .collect()
}

/// One stretch of the take compared with the song at every lag from
/// `guess - reach` to `guess + reach` (in fine samples): the normalised
/// correlation at each, 0 to 1. `None` where the song does not cover it.
fn probe(song: &[f32], take: &[f32], at: usize, len: usize, guess: i64, reach: i64) -> Option<Vec<f32>> {
    let piece = take.get(at..at + len)?;
    let energy: f32 = piece.iter().map(|v| v * v).sum();
    let first = at as i64 + guess - reach;
    let last = at as i64 + guess + reach;
    if energy < 1e-9 || first < 0 || last + len as i64 >= song.len() as i64 {
        return None;
    }
    let mut scores = Vec::with_capacity((2 * reach + 1) as usize);
    // Energy of the song under the sliding piece, kept up to date as it moves.
    let mut under: f32 = song[first as usize..first as usize + len].iter().map(|v| v * v).sum();
    for lag in first..=last {
        let window = &song[lag as usize..lag as usize + len];
        let dot: f32 = piece.iter().zip(window).map(|(x, y)| x * y).sum();
        // A microphone may invert the signal: the sign does not matter.
        scores.push(dot.abs() / (energy * under.max(1e-9)).sqrt());
        under += song[lag as usize + len] * song[lag as usize + len] - window[0] * window[0];
    }
    Some(scores)
}

/// A candidate position checked against the waveform.
struct Checked {
    /// Lag at the middle of the take, in fine samples.
    lag: f64,
    drift_ppm: f64,
    likeness: f32,
}

/// Looks `probes` half-second stretches of the take up in the song near
/// `guess` and finds the line (a constant lag, or one that moves steadily
/// when the two clocks differ) along which they are most alike *together*.
/// One stretch alone often fits at several lags (a held note repeats every
/// few milliseconds, the same drum hit returns every bar); all of them at
/// once fit only where the take really belongs.
fn check(song: &[f32], take: &[f32], guess: i64, probes: usize) -> Checked {
    const PIECE: usize = FINE_RATE / 2;
    let count = (take.len() / FINE_RATE).clamp(1, probes);
    let stride = (take.len().saturating_sub(PIECE) / count).max(1);
    // The coarse answer is good to a few hundredths of a second; clocks of
    // phones and cameras differ by up to a few hundred parts per million.
    let reach = (FINE_RATE as f64 * 0.06 + take.len() as f64 * 400e-6) as i64;
    let middle = take.len() as f64 / 2.0;
    // (centre of the stretch relative to the middle of the take, scores)
    let found: Vec<(f64, Vec<f32>)> = (0..count)
        .filter_map(|n| {
            let at = n * stride;
            probe(song, take, at, PIECE, guess, reach).map(|scores| ((at + PIECE / 2) as f64 - middle, scores))
        })
        .collect();
    if found.is_empty() {
        return Checked { lag: guess as f64, drift_ppm: 0.0, likeness: 0.0 };
    }
    let width = 2 * reach + 1;
    // Sum of the scores along a line: `shift` at the middle of the take,
    // moving by `slope` fine samples per fine sample.
    let along = |shift: i64, slope: f64| -> f32 {
        found
            .iter()
            .map(|(t, scores)| {
                let index = shift + (slope * t).round() as i64;
                if index >= 0 && index < width { scores[index as usize] } else { 0.0 }
            })
            .sum()
    };
    // Short takes cannot show a clock difference: do not let noise invent one.
    let slopes: Vec<f64> =
        if take.len() >= FINE_RATE * 20 { (-20..=20).map(|s| s as f64 * 20e-6).collect() } else { vec![0.0] };
    let mut best = (f32::MIN, 0i64, 0.0f64);
    for slope in &slopes {
        for shift in 0..width {
            let sum = along(shift, *slope);
            if sum > best.0 {
                best = (sum, shift, *slope);
            }
        }
    }
    // The slope again in finer steps around the winner, then the fraction
    // of a sample from a parabola through the peak.
    if slopes.len() > 1 {
        for fine_step in -10..=10 {
            let slope = best.2 + fine_step as f64 * 2e-6;
            for shift in best.1 - 2..=best.1 + 2 {
                let sum = along(shift, slope);
                if sum > best.0 {
                    best = (sum, shift, slope);
                }
            }
        }
    }
    let (left, centre, right) = (along(best.1 - 1, best.2) as f64, best.0 as f64, along(best.1 + 1, best.2) as f64);
    let curve = left - 2.0 * centre + right;
    let fraction = if curve.abs() > 1e-12 { (0.5 * (left - right) / curve).clamp(-0.5, 0.5) } else { 0.0 };
    Checked {
        lag: (guess - reach + best.1) as f64 + fraction,
        // A lag that grows along the take: the take runs through the song faster.
        drift_ppm: best.2 * 1e6,
        likeness: best.0 / count as f32,
    }
}

/// Finds where `take` (a camera's sound) sits in `song`. Both mono at
/// [`RATE`]. `None` if either is too short to say anything.
///
/// Two stages. The rise of the energy in 32 frequency bands, 100 times a
/// second, is compared at every possible position: cheap, and it survives
/// what a phone in a room does to the sound, but music repeats, so several
/// positions can look alike. The best few are then checked against the
/// waveform itself, where only the right one fits to a fraction of a
/// millisecond over the whole take.
pub fn find_offset(song: &[f32], take: &[f32]) -> Option<Offset> {
    const HOP: usize = RATE / 100;
    let bands = |samples: &[f32]| -> Vec<Vec<f32>> {
        let rows = band_flux(samples, HOP);
        (0..BANDS)
            .map(|band| {
                let mut values: Vec<f32> = rows.iter().map(|row| row[band]).collect();
                normalise(&mut values);
                values
            })
            .collect()
    };
    let (song_bands, take_bands) = (bands(song), bands(take));
    let (a_len, b_len) = (song_bands[0].len(), take_bands[0].len());
    if a_len < 100 || b_len < 100 {
        return None;
    }
    let mut correlation = vec![0.0f32; a_len + b_len - 1];
    for (x, y) in song_bands.iter().zip(&take_bands) {
        for (total, part) in correlation.iter_mut().zip(correlate(x, y)) {
            *total += part;
        }
    }
    // Positions where the two overlap by less than a second mean nothing.
    let lag_of = |k: usize| k as i64 - (b_len as i64 - 1);
    let usable = |k: usize| {
        let lag = lag_of(k);
        (a_len as i64).min(lag + b_len as i64) - lag.max(0) >= 100
    };
    // The best few positions, at least a quarter of a second apart.
    let mut order: Vec<usize> = (0..correlation.len()).filter(|k| usable(*k)).collect();
    order.sort_by(|x, y| correlation[*y].total_cmp(&correlation[*x]));
    let mut candidates: Vec<usize> = Vec::new();
    for k in order {
        if candidates.iter().all(|c| c.abs_diff(k) > 25) {
            candidates.push(k);
            if candidates.len() == 12 {
                break;
            }
        }
    }
    let (song_fine, take_fine) = (fine(song), fine(take));
    let to_fine = (FINE_RATE / 100) as i64;
    // A quick look at every candidate, a thorough one at the best three.
    let mut quick: Vec<(f32, i64)> = candidates
        .iter()
        .map(|k| {
            let guess = lag_of(*k) * to_fine;
            (check(&song_fine, &take_fine, guess, 6).likeness, guess)
        })
        .collect();
    quick.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut checked: Vec<Checked> =
        quick.iter().take(3).map(|(_, guess)| check(&song_fine, &take_fine, *guess, 40)).collect();
    checked.sort_by(|x, y| y.likeness.total_cmp(&x.likeness));
    let best = checked.first()?;
    // The same position reached from a neighbouring candidate is not a rival.
    let rival = |lag: f64| (lag - best.lag).abs() > FINE_RATE as f64 * 0.05;
    let runner_up = checked
        .iter()
        .skip(1)
        .filter(|c| rival(c.lag))
        .map(|c| c.likeness)
        .chain(quick.iter().skip(3).map(|(likeness, _)| *likeness))
        .fold(0.0, f32::max);
    Some(Offset {
        lag: (best.lag * (RATE / FINE_RATE) as f64).round() as i64,
        likeness: best.likeness,
        runner_up,
        drift_ppm: best.drift_ppm as f32,
    })
}

/// The beats of a piece of music.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beats {
    pub bpm: f32,
    /// Sample positions, ascending.
    pub beats: Vec<i64>,
    /// The tempo does not change: the beats were put on an even grid.
    pub steady: bool,
}

/// Finds tempo and beats (the dynamic-programming beat tracker of Ellis,
/// 2007). `None` for sound too short or without any rhythm.
pub fn find_beats(samples: &[f32]) -> Option<Beats> {
    const HOP: usize = 256;
    let rate = RATE as f32 / HOP as f32;
    let mut envelope = onset_envelope(samples, HOP);
    if envelope.len() < (rate * 4.0) as usize {
        return None;
    }
    normalise(&mut envelope);
    if envelope.iter().all(|v| *v == 0.0) {
        return None;
    }
    envelope.iter_mut().for_each(|v| *v = v.max(0.0));

    // Tempo: the lag at which the envelope repeats best, preferring tempi
    // around 120 beats per minute over their halves and doubles.
    let (shortest, longest) = ((rate * 60.0 / 240.0) as usize, (rate * 60.0 / 50.0) as usize);
    let window = envelope.len().min((rate * 90.0) as usize);
    let middle = &envelope[(envelope.len() - window) / 2..(envelope.len() - window) / 2 + window];
    let repeat = |lag: usize| -> f32 {
        middle.iter().zip(&middle[lag..]).map(|(x, y)| x * y).sum::<f32>() / (window - lag) as f32
    };
    let prefer = |lag: usize| {
        let octaves = (lag as f32 / (rate * 0.5)).log2();
        (-0.5 * octaves * octaves).exp()
    };
    let period = (shortest..=longest.min(window / 2))
        .max_by(|x, y| (repeat(*x) * prefer(*x)).total_cmp(&(repeat(*y) * prefer(*y))))?;

    // Beats: the sequence of onsets that is both strong and evenly spaced.
    let width = (period as f32 / 32.0).max(1.0);
    let reach = (width * 3.0) as isize;
    let local: Vec<f32> = (0..envelope.len() as isize)
        .map(|t| {
            (-reach..=reach)
                .filter(|d| t + d >= 0 && t + d < envelope.len() as isize)
                .map(|d| envelope[(t + d) as usize] * (-0.5 * (d as f32 / width).powi(2)).exp())
                .sum()
        })
        .collect();
    const TIGHTNESS: f32 = 100.0;
    let mut score = local.clone();
    let mut from = vec![usize::MAX; local.len()];
    for t in 0..local.len() {
        let (low, high) = (t.saturating_sub(2 * period), t.saturating_sub(period / 2));
        let mut best = (f32::MIN, usize::MAX);
        for (p, before) in score.iter().enumerate().take(high).skip(low) {
            let off = ((t - p) as f32 / period as f32).ln();
            let candidate = before - TIGHTNESS * off * off;
            if candidate > best.0 {
                best = (candidate, p);
            }
        }
        if best.1 != usize::MAX && best.0 > 0.0 {
            score[t] += best.0;
            from[t] = best.1;
        }
    }
    let last = (local.len().saturating_sub(period)..local.len()).max_by(|x, y| score[*x].total_cmp(&score[*y]))?;
    let mut frames = vec![last];
    while let Some(&t) = frames.last() {
        if from[t] == usize::MAX {
            break;
        }
        frames.push(from[t]);
    }
    frames.reverse();
    // Beats tracked through silence before and after the music are dropped.
    let strength: Vec<f32> = frames.iter().map(|t| local[*t]).collect();
    let mut sorted = strength.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[sorted.len() / 2] * 0.1;
    let first = strength.iter().position(|s| *s > floor)?;
    let last = strength.iter().rposition(|s| *s > floor)?;
    let frames = &frames[first..=last];
    if frames.len() < 4 {
        return None;
    }
    // The envelope frame at `t` is centred half a window after `t * HOP`.
    // A steady tempo: slide an evenly spaced comb over the envelope, in
    // steps much finer than a frame, and keep the spacing and position that
    // collect the most. Where the tracker lost the beat for a while (a
    // passage without drums), the comb simply runs through.
    let at = |position: f64| -> f32 {
        let (whole, part) = (position.floor(), (position - position.floor()) as f32);
        let value = |i: f64| if i >= 0.0 && (i as usize) < local.len() { local[i as usize] } else { 0.0 };
        value(whole) * (1.0 - part) + value(whole + 1.0) * part
    };
    let (first, last) = (frames[0] as f64, frames[frames.len() - 1] as f64);
    let rough = (last - first) / (frames.len() - 1) as f64;
    let mut comb = (f32::MIN, rough, first);
    for step in -150i32..=150 {
        let spacing = rough * (1.0 + step as f64 * 1e-4);
        let teeth = ((last - first) / spacing) as usize + 1;
        // Four positions per frame across one spacing.
        for shift in 0..(spacing * 4.0) as usize {
            let start = first - spacing / 2.0 + shift as f64 / 4.0;
            let sum: f32 = (0..teeth).map(|k| at(start + k as f64 * spacing)).sum();
            if sum > comb.0 {
                comb = (sum, spacing, start);
            }
        }
    }
    let tracked: f32 = frames.iter().map(|t| local[*t]).sum();
    // The frame at `t` is centred half a window after `t * HOP`.
    let to_sample = |frame: f64| (frame * HOP as f64 + 512.0).round() as i64;
    let steady = comb.0 >= 0.85 * tracked;
    let (bpm, beats) = if steady {
        let teeth = ((last - comb.2) / comb.1).round() as usize + 1;
        (60.0 * rate as f64 / comb.1, (0..teeth).map(|k| to_sample(comb.2 + k as f64 * comb.1)).collect())
    } else {
        let mut gaps: Vec<usize> = frames.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_unstable();
        (
            60.0 * rate as f64 / gaps[gaps.len() / 2] as f64,
            frames.iter().map(|t| to_sample(*t as f64)).collect::<Vec<i64>>(),
        )
    };
    Some(Beats { bpm: bpm as f32, beats, steady })
}

/// The key a piece of music is in: its root (0 is C, ... 11 is B) and
/// whether it is minor. From how much of each of the twelve notes sounds
/// over the piece (up to three minutes of it), held against how much of
/// each note a major or a minor key has (Krumhansl and Kessler's
/// profiles). `None` for sound that fits no key clearly: speech, drums
/// alone, silence.
///
/// A key and its relative (C major and A minor have the same notes) are
/// told apart only by which notes weigh more, so that pair can be taken
/// for each other; the notes shown or corrected to are the same either way.
pub fn find_key(samples: &[f32]) -> Option<(u8, bool)> {
    const SIZE: usize = 8192;
    const HOP: usize = 4096;
    if samples.len() < SIZE * 4 {
        return None;
    }
    let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(SIZE);
    let window: Vec<f32> =
        (0..SIZE).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / SIZE as f32).cos()).collect();
    // The note of each bin between 55 Hz and 2 kHz (below, a bin is wider
    // than a semitone; above, it is mostly overtones and noise).
    let hz_per_bin = RATE as f32 / SIZE as f32;
    let (low, high) = ((55.0 / hz_per_bin).ceil() as usize, (2000.0 / hz_per_bin) as usize);
    let note_of: Vec<usize> = (0..=high)
        .map(|bin| {
            let hz = (bin.max(1) as f32) * hz_per_bin;
            ((69.0 + 12.0 * (hz / 440.0).log2()).round() as i32).rem_euclid(12) as usize
        })
        .collect();
    let most = samples.len().min(RATE * 180);
    let mut notes = [0.0f64; 12];
    let mut buffer = vec![rustfft::num_complex::Complex32::default(); SIZE];
    for start in (0..most - SIZE).step_by(HOP) {
        for (slot, (sample, w)) in buffer.iter_mut().zip(samples[start..start + SIZE].iter().zip(&window)) {
            *slot = rustfft::num_complex::Complex32::new(sample * w, 0.0);
        }
        fft.process(&mut buffer);
        // (Each frame counts by what it holds, loud or not: a quiet bridge is as much of the key as a loud chorus.)
        let mut frame = [0.0f64; 12];
        for bin in low..=high {
            frame[note_of[bin]] += buffer[bin].norm() as f64;
        }
        let sum: f64 = frame.iter().sum();
        if sum > 1e-3 {
            for (all, own) in notes.iter_mut().zip(frame) {
                *all += own / sum;
            }
        }
    }
    if notes.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
    // How alike two rows of twelve numbers are, from -1 to 1.
    let alike = |profile: &[f64; 12], root: usize| {
        let mean = |v: &[f64; 12]| v.iter().sum::<f64>() / 12.0;
        let (a, b) = (mean(profile), mean(&notes));
        let (mut both, mut one, mut other) = (0.0, 0.0, 0.0);
        for step in 0..12 {
            let (x, y) = (profile[step] - a, notes[(root + step) % 12] - b);
            both += x * y;
            one += x * x;
            other += y * y;
        }
        both / (one * other).sqrt().max(1e-12)
    };
    let mut best: Option<(f64, u8, bool)> = None;
    for root in 0..12 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let score = alike(profile, root);
            if best.is_none_or(|(top, ..)| score > top) {
                best = Some((score, root as u8, minor));
            }
        }
    }
    best.filter(|(score, ..)| *score > 0.5).map(|(_, root, minor)| (root, minor))
}

#[cfg(test)]
mod tests {
    /// Chords, a second each: every note with a few overtones.
    fn chords(progression: &[&[f32]]) -> Vec<f32> {
        let mut out = Vec::new();
        for round in 0..3 {
            for chord in progression {
                for i in 0..super::RATE {
                    let t = (round * progression.len() * super::RATE + i) as f32 / super::RATE as f32;
                    let fade = (i as f32 / 2000.0).min(1.0) * ((super::RATE - i) as f32 / 2000.0).min(1.0);
                    let v: f32 = chord
                        .iter()
                        .map(|hz| {
                            (1..=4)
                                .map(|h| (2.0 * std::f32::consts::PI * hz * h as f32 * t).sin() / h as f32)
                                .sum::<f32>()
                        })
                        .sum();
                    out.push(0.08 * fade * v);
                }
            }
        }
        out
    }

    #[test]
    fn the_key_of_a_piece_is_found_from_its_notes() {
        let note = |semitones_from_a4: i32| 440.0 * 2f32.powf(semitones_from_a4 as f32 / 12.0);
        // C major: C, F, G, C (C4 is nine semitones under A4).
        let (c, e, g, f, a, b, d) = (note(-9), note(-5), note(-2), note(-4), note(0), note(2), note(-7));
        let major = chords(&[&[c, e, g], &[f, a, note(3)], &[g, b, note(5)], &[c, e, g]]);
        assert_eq!(super::find_key(&major), Some((0, false)));
        // A minor with its leading note: Am, Dm, E, Am.
        let (a3, c4, e4, d4, f4, gs) = (note(-12), c, e, d, f, note(-1));
        let minor = chords(&[&[a3, c4, e4], &[d4, f4, a], &[e4, gs, b], &[a3, c4, e4]]);
        assert_eq!(super::find_key(&minor), Some((9, true)));
        // The same piece three semitones up is in C minor.
        let shift = |chord: &[f32]| chord.iter().map(|hz| hz * 2f32.powf(3.0 / 12.0)).collect::<Vec<f32>>();
        let (i, iv, v) = (shift(&[a3, c4, e4]), shift(&[d4, f4, a]), shift(&[e4, gs, b]));
        assert_eq!(super::find_key(&chords(&[&i, &iv, &v, &i])), Some((0, true)));
        // Noise and silence are in no key.
        let mut seed = 0x1357_9bdfu32;
        let noise: Vec<f32> = (0..super::RATE * 4)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as f32 / u32::MAX as f32 - 0.5
            })
            .collect();
        assert_eq!(super::find_key(&noise), None);
        assert_eq!(super::find_key(&vec![0.0; super::RATE * 4]), None);
    }

    use super::*;
    use crate::analysis_testdata::{Camera, record, song};

    #[test]
    fn a_noisy_phone_recording_is_placed_within_a_millisecond() {
        let (music, _) = song(128.0, 120.0, 2.0, 2);
        let start = (31.37 * RATE as f64) as i64;
        let take = record(&music, &Camera { start, seconds: 40.0, snr_db: 0.0, clock_ppm: 0.0, seed: 5 });
        let found = find_offset(&music, &take).unwrap();
        assert!((found.lag - start).abs() < 48, "{} samples off", found.lag - start);
        assert!(found.is_confident(), "{found:?}");
        assert!(found.drift_ppm.abs() < 5.0);
    }

    #[test]
    fn a_clock_difference_is_measured_and_split_evenly() {
        let (music, _) = song(96.0, 200.0, 2.0, 1);
        let start = (17.0 * RATE as f64) as i64;
        let take = record(&music, &Camera { start, seconds: 150.0, snr_db: 6.0, clock_ppm: 120.0, seed: 8 });
        let found = find_offset(&music, &take).unwrap();
        assert!((found.drift_ppm - 120.0).abs() < 6.0, "{found:?}");
        // Laid by its middle: 75 s into a take that gains 120 millionths.
        let expected = start + (75.0 * 120e-6 * RATE as f64) as i64;
        assert!((found.lag - expected).abs() < 72, "{} samples off", found.lag - expected);
        assert!(found.is_confident());
    }

    #[test]
    fn a_recording_of_another_song_is_not_trusted() {
        let (music, _) = song(128.0, 120.0, 2.0, 2);
        let (other, _) = song(128.0, 90.0, 0.0, 9);
        let take = record(&other, &Camera { start: 20 * 48_000, seconds: 30.0, snr_db: 10.0, clock_ppm: 0.0, seed: 3 });
        assert!(!find_offset(&music, &take).unwrap().is_confident());
        assert_eq!(find_offset(&music, &[0.0; 1000]), None);
    }

    #[test]
    fn beats_of_a_steady_song_are_found_to_a_few_milliseconds() {
        for (bpm, seed) in [(96.0f32, 2u32), (128.0, 4)] {
            let (music, truth) = song(bpm, 90.0, 1.5, seed);
            let found = find_beats(&music).unwrap();
            assert!(found.steady);
            assert!((found.bpm - bpm).abs() < 0.05, "{} instead of {bpm}", found.bpm);
            // Every found beat is on a true beat (the last true one rings on a little).
            let step = truth[1] - truth[0];
            for beat in &found.beats {
                let nearest =
                    truth.iter().chain([&(truth[truth.len() - 1] + step)]).map(|t| (t - beat).abs()).min().unwrap();
                assert!(nearest < 48 * 8, "beat {beat} is {nearest} samples from a true one");
            }
            assert!(found.beats.len().abs_diff(truth.len()) <= 2);
        }
        assert_eq!(find_beats(&vec![0.0; 48_000 * 10]), None);
    }
}
