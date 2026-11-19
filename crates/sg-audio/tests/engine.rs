//! The engine with the manual backend: blocks are pulled by the test, so
//! every sample that comes out can be checked against the files.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use sg_audio::{Backend, ClipSpan, Engine, Mix, TrackMix};

/// Writes a 32-bit float WAV whose sample at frame n is `f(n)` per channel.
fn wav(dir: &Path, name: &str, channels: u16, frames: usize, f: impl Fn(usize, usize) -> f32) -> PathBuf {
    let mut bytes = Vec::new();
    let data = frames * channels as usize * 4;
    bytes.extend(b"RIFF");
    bytes.extend(((36 + data) as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(48_000u32.to_le_bytes());
    bytes.extend((48_000 * channels as u32 * 4).to_le_bytes());
    bytes.extend((channels * 4).to_le_bytes());
    bytes.extend(32u16.to_le_bytes());
    // An odd-sized chunk before the data, as ffmpeg writes them.
    bytes.extend(b"LIST");
    bytes.extend(3u32.to_le_bytes());
    bytes.extend([1, 2, 3, 0]);
    bytes.extend(b"data");
    bytes.extend((data as u32).to_le_bytes());
    for n in 0..frames {
        for c in 0..channels as usize {
            bytes.extend(f(n, c).to_le_bytes());
        }
    }
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn track(clips: Vec<ClipSpan>) -> TrackMix {
    TrackMix::plain(clips)
}

fn pull(engine: &Engine, frames: usize) -> Vec<f32> {
    let mut out = vec![9.0; frames * 2];
    engine.pull(&mut out);
    out
}

/// Sample value that encodes its own frame number.
fn ramp(n: usize) -> f32 {
    n as f32 / 100_000.0
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn clips_land_on_their_timeline_samples() {
    let dir = tempfile::tempdir().unwrap();
    let stereo = wav(dir.path(), "s.wav", 2, 5000, |n, c| if c == 0 { ramp(n) } else { -ramp(n) });
    let engine = Engine::start(Backend::Manual).unwrap();
    // The clip sits at timeline sample 1000 and starts 200 samples into its file.
    engine.set_mix(Mix {
        master_gain: 1.0,
        tracks: vec![track(vec![ClipSpan {
            start: 1000,
            len: 600,
            src_in: 200,
            wav: stereo,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            speed: (1, 1),
        }])],
    });
    assert_eq!(engine.position(), 0);
    assert!(!engine.is_playing());
    // Not playing: silence, the position does not move.
    assert!(pull(&engine, 256).iter().all(|s| *s == 0.0));
    assert_eq!(engine.position(), 0);

    engine.play(900);
    engine.settle();
    let out = pull(&engine, 256);
    assert!(engine.is_playing());
    // Timeline 900..1000 is before the clip.
    assert!(out[..200].iter().all(|s| *s == 0.0));
    // Timeline sample 1000 + k is file frame 200 + k.
    for k in 0..156 {
        assert!(close(out[(100 + k) * 2], ramp(200 + k)), "left at {k}");
        assert!(close(out[(100 + k) * 2 + 1], -ramp(200 + k)), "right at {k}");
    }
    // The clip ends at timeline 1600; pull up to and past it.
    let out = pull(&engine, 512);
    assert!(close(out[(1599 - 1156) * 2], ramp(799)));
    assert_eq!(out[(1600 - 1156) * 2], 0.0);
}

#[test]
fn mono_gain_pan_mute_and_mixing() {
    let dir = tempfile::tempdir().unwrap();
    let mono = wav(dir.path(), "m.wav", 1, 4000, |_, _| 0.5);
    let other = wav(dir.path(), "o.wav", 2, 4000, |_, _| 0.25);
    let clip = |wav: &PathBuf, gain: f32| ClipSpan {
        start: 0,
        len: 4000,
        src_in: 0,
        wav: wav.clone(),
        gain,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    let mix = |pan: f32, mute: bool| Mix {
        tracks: vec![
            TrackMix { gain: 2.0, pan, mute, ..TrackMix::plain(vec![clip(&mono, 0.5)]) },
            track(vec![clip(&other, 1.0)]),
        ],
        master_gain: 1.0,
    };
    // Mono 0.5, clip gain 0.5, track gain 2, centre: 0.5 on both sides, plus 0.25.
    engine.set_mix(mix(0.0, false));
    engine.play(0);
    engine.settle();
    let out = pull(&engine, 64);
    assert!(close(out[0], 0.75) && close(out[1], 0.75), "{:?}", &out[..2]);

    // Hard left: the first track only reaches the left channel.
    engine.set_mix(mix(-1.0, false));
    engine.play(0);
    engine.settle();
    let out = pull(&engine, 64);
    assert!(out[0] > 0.9 && close(out[1], 0.25), "{:?}", &out[..2]);

    engine.set_mix(mix(0.0, true));
    engine.play(0);
    engine.settle();
    let out = pull(&engine, 64);
    assert!(close(out[0], 0.25) && close(out[1], 0.25));
}

#[test]
fn pause_seek_and_resume_are_sample_accurate() {
    let dir = tempfile::tempdir().unwrap();
    let file = wav(dir.path(), "r.wav", 1, 48_000, |n, _| ramp(n));
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix {
        master_gain: 1.0,
        tracks: vec![track(vec![ClipSpan {
            start: 0,
            len: 48_000,
            src_in: 0,
            wav: file,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            speed: (1, 1),
        }])],
    });
    engine.play(0);
    engine.settle();
    pull(&engine, 256);
    pull(&engine, 256);
    let paused_at = engine.pause();
    engine.settle();
    // The second block started at sample 256 and is being heard now.
    assert!((256..=256 + 12_000).contains(&paused_at), "{paused_at}");
    assert!(!engine.is_playing());
    assert!(pull(&engine, 256).iter().all(|s| *s == 0.0));

    engine.seek(10_000);
    assert_eq!(engine.position(), 10_000);
    engine.play(10_000);
    engine.settle();
    let out = pull(&engine, 256);
    assert!(close(out[0], ramp(10_000)) && close(out[255 * 2], ramp(10_255)));
    // A long pull crosses many ring refills without a gap.
    for block in 1..40 {
        engine.settle();
        let out = pull(&engine, 256);
        assert!(close(out[0], ramp(10_000 + block * 256)), "block {block}");
    }
    let stats = engine.stats();
    assert_eq!(stats.underruns, 0);
    assert_eq!(stats.quantum, 256);
}

#[test]
fn a_missing_file_is_silence_until_it_appears() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("later.wav");
    let engine = Engine::start(Backend::Manual).unwrap();
    let mix = Mix {
        master_gain: 1.0,
        tracks: vec![track(vec![ClipSpan {
            start: 0,
            len: 1000,
            src_in: 0,
            wav: path.clone(),
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            speed: (1, 1),
        }])],
    };
    engine.set_mix(mix.clone());
    engine.play(0);
    engine.settle();
    assert!(pull(&engine, 128).iter().all(|s| *s == 0.0));
    wav(dir.path(), "later.wav", 1, 1000, |_, _| 0.3);
    // The app sends the mix again when an import reports the audio ready.
    engine.set_mix(mix);
    engine.play(0);
    engine.settle();
    assert!(close(pull(&engine, 128)[0], 0.3));
}

#[test]
fn the_null_backend_keeps_time() {
    let engine = Engine::start(Backend::Null).unwrap();
    engine.set_mix(Mix::default());
    engine.play(48_000);
    let t = std::time::Instant::now();
    while !engine.is_playing() {
        assert!(t.elapsed().as_secs() < 2, "never started");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let (p0, t0) = (engine.position(), std::time::Instant::now());
    std::thread::sleep(std::time::Duration::from_millis(300));
    let advanced = engine.position() - p0;
    let expected = (t0.elapsed().as_secs_f64() * 48_000.0) as i64;
    assert!((advanced - expected).abs() < 2_400, "advanced {advanced}, expected {expected}");
    let at = engine.pause();
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(engine.position(), at, "paused: the position holds");
}

#[test]
fn fades_and_speed() {
    let dir = tempfile::tempdir().unwrap();
    let flat = wav(dir.path(), "flat.wav", 1, 4000, |_, _| 1.0);
    let ramp_file = wav(dir.path(), "ramp.wav", 1, 4000, |n, _| ramp(n));
    let engine = Engine::start(Backend::Manual).unwrap();

    // A 1000-sample clip of constant 1.0 with 100-sample fades.
    let faded =
        ClipSpan { start: 0, len: 1000, src_in: 0, wav: flat, gain: 1.0, fade_in: 100, fade_out: 100, speed: (1, 1) };
    engine.set_mix(Mix { tracks: vec![track(vec![faded])], master_gain: 1.0 });
    engine.play(0);
    engine.settle();
    let out = pull(&engine, 1024);
    assert_eq!(out[0], 0.0);
    assert!(
        close(out[50 * 2], 0.5) && close(out[500 * 2], 1.0) && close(out[950 * 2], 0.5),
        "{} {} {}",
        out[100],
        out[1000],
        out[1900]
    );
    assert_eq!(out[1000 * 2], 0.0);

    // Double speed: output sample n is source sample 2n; half speed: n/2.
    for (speed, factor) in [((2, 1), 2.0), ((1, 2), 0.5)] {
        let clip = ClipSpan {
            start: 0,
            len: 1000,
            src_in: 100,
            wav: ramp_file.clone(),
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            speed,
        };
        engine.set_mix(Mix { tracks: vec![track(vec![clip])], master_gain: 1.0 });
        engine.play(0);
        engine.settle();
        let out = pull(&engine, 600);
        for n in [0usize, 1, 2, 3, 250, 599] {
            let expected = (100.0 + n as f32 * factor) / 100_000.0;
            assert!((out[n * 2] - expected).abs() < 1e-5, "speed {speed:?} sample {n}: {} vs {expected}", out[n * 2]);
        }
    }
}

/// Renders `frames` frames of a mix from position 0 with the manual backend.
fn rendered(mix: Mix, frames: usize) -> Vec<f32> {
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(mix);
    engine.play(0);
    let mut out = vec![0.0f32; frames * 2];
    for block in out.chunks_mut(512) {
        engine.settle();
        engine.pull(block);
    }
    out
}

#[test]
fn noise_reduction_keeps_the_track_in_time() {
    let dir = tempfile::tempdir().unwrap();
    // Silence, then a click-free burst of a gliding tone starting at 0.5 s.
    let samples: Vec<f32> = (0..48_000)
        .map(|i| {
            let t = (i as f32 - 24_000.0) / 48_000.0;
            if t < 0.0 {
                0.0
            } else {
                0.5 * (2.0 * std::f32::consts::PI * (200.0 * t + 300.0 * t * t)).sin() * (t * 40.0).min(1.0)
            }
        })
        .collect();
    let path = wav(dir.path(), "burst.wav", 1, samples.len(), |i, _| samples[i]);
    let span =
        ClipSpan { start: 0, len: 48_000, src_in: 0, wav: path, gain: 1.0, fade_in: 0, fade_out: 0, speed: (1, 1) };
    let plain = rendered(Mix { tracks: vec![track(vec![span.clone()])], master_gain: 1.0 }, 48_000);
    // Blended half and half, the treated sound must line up with the plain
    // one: if it were late, the two halves would cancel in places.
    let half = TrackMix { denoise: 0.0001, ..track(vec![span]) };
    let treated = rendered(Mix { tracks: vec![half], master_gain: 1.0 }, 48_000);
    let worst = plain.iter().zip(&treated).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(worst < 0.01, "a nearly dry noise reduction changed the sound by {worst}");
    // And the burst starts where it starts in the file.
    let onset = |v: &[f32]| v.iter().step_by(2).position(|s| s.abs() > 0.05).unwrap();
    assert!(onset(&treated).abs_diff(onset(&plain)) <= 2);
}

#[test]
fn faders_move_without_a_new_graph_and_meters_follow() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav(dir.path(), "dc.wav", 1, 48_000, |_, _| 0.5);
    let span =
        ClipSpan { start: 0, len: 48_000, src_in: 0, wav: path, gain: 1.0, fade_in: 0, fade_out: 0, speed: (1, 1) };
    let mix = |gain: f32, master: f32| Mix {
        tracks: vec![TrackMix { gain, ..track(vec![span.clone()]) }],
        master_gain: master,
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(mix(1.0, 1.0));
    engine.play(0);
    engine.settle();
    let mut block = vec![0.0f32; 512];
    engine.pull(&mut block);
    assert!((block[500] - 0.5).abs() < 1e-4);
    let mut meters = [0.0f32; 2];
    assert!((engine.take_meters(&mut meters) - 0.5).abs() < 1e-4);
    assert!((meters[0] - 0.5).abs() < 1e-4 && meters[1] == 0.0);
    // Halve the track, double nothing: the position keeps counting on.
    engine.set_mix(mix(0.5, 0.5));
    engine.settle();
    engine.pull(&mut block);
    engine.pull(&mut block);
    assert!((block[500] - 0.125).abs() < 1e-4, "{}", block[500]);
    // The clock is where the third block began: nothing was restarted.
    assert_eq!(engine.position(), 256 * 2);
    assert!((engine.take_meters(&mut meters) - 0.5).abs() < 0.01, "the ramp starts at the old level");
}

#[test]
fn the_master_never_leaves_full_scale() {
    let dir = tempfile::tempdir().unwrap();
    // A tone at 0.8 on two tracks: 1.6 when summed.
    let path = wav(dir.path(), "loud.wav", 1, 48_000, |i, _| 0.8 * (i as f32 * 0.05).sin());
    let span =
        ClipSpan { start: 0, len: 48_000, src_in: 0, wav: path, gain: 1.0, fade_in: 0, fade_out: 0, speed: (1, 1) };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix { tracks: vec![track(vec![span.clone()]), track(vec![span])], master_gain: 1.0 });
    engine.play(0);
    let mut out = vec![0.0f32; 24_000 * 2];
    for block in out.chunks_mut(512) {
        engine.settle();
        engine.pull(block);
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= 1.0 && peak > 0.95, "peak {peak}");
    // 1.6 down to 1.0 is 4.1 dB.
    let taken = engine.meters().take_limiting();
    assert!((taken - 4.1).abs() < 0.3, "{taken} dB");
    assert_eq!(engine.meters().take_limiting(), 0.0);
}
