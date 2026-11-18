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
    TrackMix { clips, gain: 1.0, pan: 0.0, mute: false }
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
        tracks: vec![TrackMix { clips: vec![clip(&mono, 0.5)], gain: 2.0, pan, mute }, track(vec![clip(&other, 1.0)])],
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
    engine.set_mix(Mix { tracks: vec![track(vec![faded])] });
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
        engine.set_mix(Mix { tracks: vec![track(vec![clip])] });
        engine.play(0);
        engine.settle();
        let out = pull(&engine, 600);
        for n in [0usize, 1, 2, 3, 250, 599] {
            let expected = (100.0 + n as f32 * factor) / 100_000.0;
            assert!((out[n * 2] - expected).abs() < 1e-5, "speed {speed:?} sample {n}: {} vs {expected}", out[n * 2]);
        }
    }
}
