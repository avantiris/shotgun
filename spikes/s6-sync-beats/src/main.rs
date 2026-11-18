//! Spike S6: can a take be placed in the song from its camera sound, and
//! can the beats of the song be found, well enough to build M7 on?
//! Runs the product's analysis code on synthetic material with known truth.
//!   CARGO_TARGET_DIR=/disk/shotgun-dev/target-s6 cargo run --release --manifest-path spikes/s6-sync-beats/Cargo.toml

#[path = "../../../crates/sg-media/src/analysis.rs"]
mod analysis;
#[path = "../../../crates/sg-media/src/analysis_testdata.rs"]
mod testdata;

use std::time::Instant;

use testdata::{Camera, record, song};

const RATE: f32 = 48_000.0;

/// Writes mono 16-bit WAV.
fn write_wav(path: &std::path::Path, samples: &[f32]) {
    let data = samples.len() as u32 * 2;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend(b"RIFF");
    bytes.extend((36 + data).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(48_000u32.to_le_bytes());
    bytes.extend(96_000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data.to_le_bytes());
    for s in samples {
        bytes.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write wav");
}

/// `write <dir>`: a song and three camera recordings of it as WAV files,
/// for trying the feature in the app. Prints where each camera starts.
fn write_media(dir: &std::path::Path) {
    let (music, _) = song(128.0, 120.0, 2.0, 2);
    write_wav(&dir.join("song.wav"), &music);
    for (n, start_s, seconds, snr_db, ppm) in [(1, 31.37f32, 40.0f32, 6.0f32, 0.0f32), (2, 10.0, 60.0, 0.0, 50.0), (3, -2.0, 30.0, 10.0, -30.0)] {
        let camera = Camera { start: (start_s * RATE) as i64, seconds, snr_db, clock_ppm: ppm, seed: n };
        write_wav(&dir.join(format!("cam{n}.wav")), &record(&music, &camera));
        println!("cam{n} starts {start_s} s into the song, {seconds} s long, music {snr_db} dB over the room, clock {ppm} ppm");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("write") {
        return write_media(std::path::Path::new(args.get(2).map_or(".", String::as_str)));
    }
    println!("== sync: error of the found position, in milliseconds");
    println!("{:>5} {:>6} {:>7} {:>7} {:>9} {:>7} {:>7} {:>9} {:>8}", "bpm", "take s", "snr dB", "ppm", "error ms", "alike", "next", "drift ppm", "time ms");
    let mut worst = 0.0f64;
    let mut lowest = (f32::MAX, f32::MAX);
    let mut unsure = 0;
    for (bpm, seed) in [(96.0, 1u32), (128.0, 2), (174.0, 3)] {
        let (music, _) = song(bpm, 200.0, 2.0, seed);
        for seconds in [12.0f32, 45.0, 150.0] {
            for snr_db in [20.0f32, 6.0, 0.0, -6.0] {
                for (ppm, start_s) in [(0.0f32, 31.37f32), (120.0, 17.011), (-300.0, -3.2)] {
                    let start = (start_s * RATE) as i64;
                    let take = record(&music, &Camera { start, seconds, snr_db, clock_ppm: ppm, seed: seed * 31 + snr_db as u32 });
                    let started = Instant::now();
                    let found = analysis::find_offset(&music, &take);
                    let ms = started.elapsed().as_secs_f64() * 1e3;
                    match found {
                        Some(o) => {
                            // With a clock error the take has no single position: compare at its middle.
                            let middle = seconds as f64 / 2.0 * ppm as f64 * 1e-6 * RATE as f64;
                            let error = (o.lag as f64 - (start as f64 + middle)) / RATE as f64 * 1e3;
                            worst = worst.max(error.abs());
                            lowest = (lowest.0.min(o.likeness), lowest.1.min(o.likeness - o.runner_up));
                            if !o.is_confident() {
                                unsure += 1;
                            }
                            let drift = format!("{:.0}", o.drift_ppm);
                            println!("{bpm:>5} {seconds:>6} {snr_db:>7} {ppm:>7} {error:>9.2} {:>7.2} {:>7.2} {drift:>9} {ms:>8.0}", o.likeness, o.runner_up);
                        }
                        None => println!("{bpm:>5} {seconds:>6} {snr_db:>7} {ppm:>7}   NOT FOUND"),
                    }
                }
            }
        }
    }
    println!("worst error {worst:.2} ms; lowest likeness {:.2}, smallest lead over the next position {:.2}; {unsure} of the true matches not confident", lowest.0, lowest.1);

    println!("\n== sync: takes that do not belong to the song (must not be confident)");
    let (music, _) = song(128.0, 200.0, 2.0, 2);
    let mut highest = (0.0f32, 0.0f32);
    for (bpm, seed) in [(128.0, 9u32), (96.0, 5), (140.0, 6), (128.0, 7)] {
        let (other, _) = song(bpm, 120.0, 0.0, seed);
        for seconds in [12.0f32, 45.0] {
            let take = record(&other, &Camera { start: 20 * 48_000, seconds, snr_db: 10.0, clock_ppm: 0.0, seed });
            if let Some(o) = analysis::find_offset(&music, &take) {
                highest = (highest.0.max(o.likeness), highest.1.max(o.likeness - o.runner_up));
                println!("other song at {bpm} bpm, {seconds} s: likeness {:.2}, next {:.2}, confident: {}", o.likeness, o.runner_up, o.is_confident());
            }
        }
    }
    // Pure room noise.
    let silence = vec![0.0f32; 48_000 * 60];
    let take = record(&silence, &Camera { start: 0, seconds: 30.0, snr_db: 0.0, clock_ppm: 0.0, seed: 4 });
    if let Some(o) = analysis::find_offset(&music, &take) {
        highest = (highest.0.max(o.likeness), highest.1.max(o.likeness - o.runner_up));
        println!("room noise only: likeness {:.2}, next {:.2}, confident: {}", o.likeness, o.runner_up, o.is_confident());
    }
    println!("highest likeness {:.2}, largest lead {:.2} among false matches", highest.0, highest.1);

    println!("\n== beats");
    println!("{:>6} {:>9} {:>7} {:>8} {:>12} {:>10} {:>8}", "bpm", "found", "steady", "beats", "median ms", "worst ms", "time ms");
    for (bpm, seed) in [(72.0f32, 1u32), (96.0, 2), (110.0, 3), (128.0, 4), (140.0, 5), (174.0, 6), (87.5, 7)] {
        let (music, truth) = song(bpm, 180.0, 1.5, seed);
        let started = Instant::now();
        let found = analysis::find_beats(&music);
        let ms = started.elapsed().as_secs_f64() * 1e3;
        match found {
            Some(b) => {
                // Distance of every found beat to the nearest true beat or half beat.
                // (One more true beat than was generated: the last one rings on.)
                let step = truth[1] - truth[0];
                let all: Vec<i64> = truth.iter().copied().chain([truth[truth.len() - 1] + step, truth[truth.len() - 1] + 2 * step]).collect();
                let half: Vec<i64> = all.windows(2).flat_map(|w| [w[0], (w[0] + w[1]) / 2]).collect();
                let mut errors: Vec<f64> = b
                    .beats
                    .iter()
                    .map(|beat| half.iter().map(|t| (t - beat).abs()).min().unwrap_or(0) as f64 / RATE as f64 * 1e3)
                    .collect();
                errors.sort_by(f64::total_cmp);
                println!(
                    "{bpm:>6} {:>9.2} {:>7} {:>4}/{:<4} {:>11.1} {:>10.1} {ms:>8.0}",
                    b.bpm,
                    b.steady,
                    b.beats.len(),
                    truth.len(),
                    errors[errors.len() / 2],
                    errors[errors.len() - 1]
                );
            }
            None => println!("{bpm:>6}   NOT FOUND"),
        }
    }
}
