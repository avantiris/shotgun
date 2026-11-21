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
            gain_keys: Vec::new(),
            warp: Vec::new(),
        }])],
        ..Mix::default()
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
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    let mix = |pan: f32, mute: bool| Mix {
        tracks: vec![
            TrackMix { gain: 2.0, pan, mute, ..TrackMix::plain(vec![clip(&mono, 0.5)]) },
            track(vec![clip(&other, 1.0)]),
        ],
        master_gain: 1.0,
        ..Mix::default()
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
            gain_keys: Vec::new(),
            warp: Vec::new(),
        }])],
        ..Mix::default()
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
            gain_keys: Vec::new(),
            warp: Vec::new(),
        }])],
        ..Mix::default()
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
    let faded = ClipSpan {
        start: 0,
        len: 1000,
        src_in: 0,
        wav: flat,
        gain: 1.0,
        fade_in: 100,
        fade_out: 100,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    engine.set_mix(Mix { tracks: vec![track(vec![faded])], master_gain: 1.0, ..Mix::default() });
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
            gain_keys: Vec::new(),
            warp: Vec::new(),
        };
        engine.set_mix(Mix { tracks: vec![track(vec![clip])], master_gain: 1.0, ..Mix::default() });
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
    let span = ClipSpan {
        start: 0,
        len: 48_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let plain = rendered(Mix { tracks: vec![track(vec![span.clone()])], master_gain: 1.0, ..Mix::default() }, 48_000);
    // Blended half and half, the treated sound must line up with the plain
    // one: if it were late, the two halves would cancel in places.
    let half = TrackMix { denoise: 0.0001, ..track(vec![span]) };
    let treated = rendered(Mix { tracks: vec![half], master_gain: 1.0, ..Mix::default() }, 48_000);
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
    let span = ClipSpan {
        start: 0,
        len: 48_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let mix = |gain: f32, master: f32| Mix {
        tracks: vec![TrackMix { gain, ..track(vec![span.clone()]) }],
        master_gain: master,
        ..Mix::default()
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
    let span = ClipSpan {
        start: 0,
        len: 48_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix {
        tracks: vec![track(vec![span.clone()]), track(vec![span])],
        master_gain: 1.0,
        ..Mix::default()
    });
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

#[test]
fn volume_keyframes_move_the_level_along_the_clip() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav(dir.path(), "dc.wav", 1, 48_000, |_, _| 0.5);
    // Unity until a quarter second, down 20 dB by half a second, held after.
    let span = ClipSpan {
        start: 0,
        len: 48_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: vec![(12_000, 0.0), (24_000, -20.0)],
        warp: Vec::new(),
    };
    let out = rendered(Mix { tracks: vec![track(vec![span])], master_gain: 1.0, ..Mix::default() }, 36_000);
    let at = |sample: usize| out[sample * 2];
    assert!((at(6_000) - 0.5).abs() < 1e-4);
    // Half way down the slope: -10 dB.
    assert!((at(18_000) - 0.5 * 10f32.powf(-0.5)).abs() < 1e-3, "{}", at(18_000));
    assert!((at(30_000) - 0.05).abs() < 1e-4, "{}", at(30_000));
}

#[test]
fn a_grain_is_heard_at_the_playhead_and_the_clock_stays() {
    let dir = tempfile::tempdir().unwrap();
    let file = wav(dir.path(), "one.wav", 1, 48_000, |_, _| 0.5);
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
            gain_keys: Vec::new(),
            warp: Vec::new(),
        }])],
        ..Mix::default()
    });
    engine.grain(10_000);
    engine.settle();
    let out = pull(&engine, 6000);
    // Comes up without a click, holds, goes down, and is over after a tenth of a second.
    assert_eq!(out[0], 0.0);
    assert!(out[2 * 100] > 0.1 && out[2 * 100] < 0.4, "{}", out[2 * 100]);
    assert!(close(out[2 * 2400], 0.5));
    assert!(out[2 * 4700] > 0.1 && out[2 * 4700] < 0.4, "{}", out[2 * 4700]);
    assert!(out[2 * 4800..].iter().all(|s| *s == 0.0));
    assert!(pull(&engine, 512).iter().all(|s| *s == 0.0));
    // The playhead is where the grain was asked for; nothing is "playing".
    assert!(!engine.is_playing());
    assert_eq!(engine.position(), 10_000);
    // Playback afterwards starts as always.
    engine.play(0);
    engine.settle();
    assert!(close(pull(&engine, 256)[0], 0.5));
    assert!(engine.is_playing());
}

#[test]
fn a_changing_speed_reads_the_source_along_its_curve() {
    let dir = tempfile::tempdir().unwrap();
    let file = wav(dir.path(), "r.wav", 1, 48_000, |n, _| ramp(n));
    // Normal speed for 1000 samples, then half speed.
    let warp = vec![(0, 0.0), (1000, 1000.0), (5000, 3000.0)];
    let out = rendered(
        Mix {
            master_gain: 1.0,
            tracks: vec![track(vec![ClipSpan {
                start: 0,
                len: 5000,
                src_in: 200,
                wav: file,
                gain: 1.0,
                fade_in: 0,
                fade_out: 0,
                speed: (1, 1),
                gain_keys: Vec::new(),
                warp,
            }])],
            ..Mix::default()
        },
        5000,
    );
    // Each output sample holds the source position it was read from.
    for (n, source) in [(0, 200.0), (999, 1199.0), (1000, 1200.0), (1001, 1200.5), (3000, 2200.0), (4999, 3199.5)] {
        assert!(
            (out[2 * n] * 100_000.0 - source).abs() < 0.2,
            "sample {n}: {} instead of {source}",
            out[2 * n] * 100_000.0
        );
    }
}

#[test]
fn a_track_that_ducks_goes_down_while_the_voice_sounds() {
    let dir = tempfile::tempdir().unwrap();
    // The beat: a steady level. The voice: silent, then sounding from 0.5 s to 1.2 s, then silent.
    let beat = wav(dir.path(), "beat.wav", 1, 96_000, |_, _| 0.25);
    let voice = wav(dir.path(), "voice.wav", 1, 96_000, |n, _| if (24_000..57_600).contains(&n) { 0.3 } else { 0.0 });
    let span = |path: &PathBuf| ClipSpan {
        start: 0,
        len: 96_000,
        src_in: 0,
        wav: path.clone(),
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let tone = |voice: bool, duck_db: f32| sg_audio::Tone { voice, duck_db, ..sg_audio::Tone::default() };
    let mix = |duck_db: f32, voice_muted: bool| Mix {
        tracks: vec![
            TrackMix { tone: tone(false, duck_db), ..track(vec![span(&beat)]) },
            TrackMix { tone: tone(true, 0.0), mute: voice_muted, ..track(vec![span(&voice)]) },
        ],
        master_gain: 1.0,
        ..Mix::default()
    };
    // What the beat contributes at a frame: the sum less the voice (0.3 while it sounds).
    let beat_at = |v: &[f32], frame: usize| v[frame * 2] - if (24_000..57_600).contains(&frame) { 0.3 } else { 0.0 };
    // Six decibels of ducking: before the voice the beat is as it is, under
    // it half as loud, and a second after it has come back.
    let ducked = rendered(mix(6.0, false), 96_000);
    assert!((beat_at(&ducked, 12_000) - 0.25).abs() < 0.003, "{}", beat_at(&ducked, 12_000));
    assert!((beat_at(&ducked, 48_000) - 0.125).abs() < 0.01, "{}", beat_at(&ducked, 48_000));
    assert!((beat_at(&ducked, 95_000) - 0.25).abs() < 0.02, "{}", beat_at(&ducked, 95_000));
    // It goes down within a tenth of a second of the voice coming in.
    assert!(beat_at(&ducked, 24_000 + 4_800) < 0.14, "{}", beat_at(&ducked, 24_000 + 4_800));
    // No ducking set: the beat is untouched under the voice. The voice muted: it ducks nothing.
    let plain = rendered(mix(0.0, false), 96_000);
    assert!((beat_at(&plain, 48_000) - 0.25).abs() < 0.003, "{}", beat_at(&plain, 48_000));
    let muted = rendered(mix(6.0, true), 96_000);
    assert!((muted[48_000 * 2] - 0.25).abs() < 0.003, "{}", muted[48_000 * 2]);
}

/// Where the dev shell keeps a native plugin to test with, and the host
/// program built beside the tests. `None`: nothing to test with here.
fn test_plugin(name: &str) -> Option<PathBuf> {
    let host = std::env::current_exe().ok()?.parent()?.parent()?.join("shotgun-plugin");
    let folder = std::env::var_os("SHOTGUN_TEST_VST3")?;
    if !host.is_file() {
        eprintln!("{} is not built: no plugin host to test with", host.display());
        return None;
    }
    // SAFETY: every test of this file that sets it sets the same value.
    unsafe { std::env::set_var("SHOTGUN_PLUGIN_HOST", host) };
    Some(PathBuf::from(folder).join(name))
}

#[test]
fn an_instrument_plugin_plays_a_tracks_notes_and_its_state_is_kept() {
    let Some(plugin) = test_plugin("Surge XT.vst3") else { return };
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("plugins/1.state");
    let spec = sg_audio::PluginSpec { key: 1, path: plugin, state_file: state_file.clone() };
    // One note from 0.5 s to 1.0 s.
    let note = sg_audio::NoteSpan { start: 24_000, len: 24_000, pitch: 60, velocity: 100 };
    let mix = Mix {
        tracks: vec![TrackMix { plugins: vec![spec], notes: vec![note], ..track(Vec::new()) }],
        master_gain: 1.0,
        ..Mix::default()
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(mix.clone());
    engine.play(0);
    let mut out = vec![0.0f32; 96_000 * 2];
    for block in out.chunks_mut(512) {
        engine.settle();
        engine.pull(block);
    }
    let level = |from: usize, to: usize| {
        let part = &out[from * 2..to * 2];
        10.0 * (part.iter().map(|s| s * s).sum::<f32>() / part.len() as f32).max(1e-12).log10()
    };
    // Silent before the note, sounding during it, and it starts where the note starts.
    assert!(level(0, 23_900) < -100.0, "{}", level(0, 23_900));
    assert!(level(26_000, 46_000) > -45.0, "{}", level(26_000, 46_000));
    let first = out.iter().step_by(2).position(|s| s.abs() > 1e-4).unwrap();
    assert!((24_000..24_600).contains(&first), "{first}");
    // After the note has ended it dies away.
    assert!(level(90_000, 96_000) < level(26_000, 46_000) - 20.0);
    // Started in the middle before the plugin has loaded (an export of a marked stretch):
    // it joins where playback is, not at the top of the timeline.
    let late = Engine::start(Backend::Manual).unwrap();
    late.set_mix(mix.clone());
    late.play(12_000);
    let mut later = vec![0.0f32; 24_000 * 2];
    for block in later.chunks_mut(512) {
        late.settle();
        late.pull(block);
    }
    let first = later.iter().step_by(2).position(|s| s.abs() > 1e-4).unwrap();
    assert!((12_000..12_600).contains(&first), "{first}");
    drop(late);
    // Pausing writes the plugin's state where the project keeps it.
    engine.pause();
    engine.settle();
    assert!(std::fs::metadata(&state_file).is_ok_and(|m| m.len() > 1000));
    assert!(engine.plugin_news().is_empty());
    // A plugin that is not there is said so, and the track is silent rather than broken.
    let missing = sg_audio::PluginSpec { key: 2, path: dir.path().join("nothing.vst3"), state_file };
    engine.set_mix(Mix {
        tracks: vec![TrackMix { plugins: vec![missing], notes: vec![note], ..track(Vec::new()) }],
        master_gain: 1.0,
        ..Mix::default()
    });
    engine.settle();
    let news = engine.plugin_news();
    assert!(news.len() == 1 && news[0].contains("nothing.vst3"), "{news:?}");
}

/// Recording against the real sound server. Not run by default: it adds a
/// null sink to the session for its duration (nothing is played through
/// the speakers or taken from a microphone). Run it with
/// `cargo nextest run -p sg-audio --run-ignored only a_take_lands`.
#[test]
#[ignore = "adds a null sink to the session's PipeWire graph"]
fn a_take_lands_on_the_timeline_where_it_was_heard() {
    let pactl = |args: &[&str]| std::process::Command::new("pactl").args(args).output().unwrap();
    let loaded = pactl(&["load-module", "module-null-sink", "sink_name=shotgun-test-record"]);
    let module = String::from_utf8_lossy(&loaded.stdout).trim().to_owned();
    assert!(loaded.status.success() && !module.is_empty(), "no null sink: {loaded:?}");
    let result = std::panic::catch_unwind(|| {
        let dir = tempfile::tempdir().unwrap();
        // Silence, and a burst of tone from 1.0 s to 1.2 s.
        let played = wav(dir.path(), "burst.wav", 1, 144_000, |n, _| {
            if (48_000..57_600).contains(&n) { 0.5 * ((n as f32) * 0.2).sin() } else { 0.0 }
        });
        let span = ClipSpan {
            start: 0,
            len: 144_000,
            src_in: 0,
            wav: played,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            speed: (1, 1),
            gain_keys: Vec::new(),
            warp: Vec::new(),
        };
        let engine =
            Engine::start(Backend::PipeWire { target: Some("shotgun-test-record".into()), quantum: 256 }).unwrap();
        engine.set_mix(Mix { tracks: vec![track(vec![span])], master_gain: 1.0, ..Mix::default() });
        engine.play(0);
        // What the sink plays is recorded: a loop without air in it.
        let recording = engine.record(dir.path().join("take.wav"), Some("shotgun-test-record.monitor".into())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let take = recording.stop().unwrap();
        engine.pause();
        let bytes = std::fs::read(&take.path).unwrap();
        let samples: Vec<f32> =
            bytes[44..].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        assert_eq!(samples.len() as u64, take.frames * 2);
        assert!(take.frames > 48_000 && take.lost == 0, "{take:?}");
        // Where the burst begins in the take, put on the timeline, against where it was played from.
        let first = samples.iter().step_by(2).position(|s| s.abs() > 0.05).expect("the burst was recorded") as i64;
        let landed = take.start + first;
        eprintln!(
            "burst played at 48000, recorded at {landed}: {} frames = {:.1} ms off",
            landed - 48_000,
            (landed - 48_000) as f64 / 48.0
        );
        assert!((landed - 48_000).abs() < 1_024, "{landed}");
    });
    pactl(&["unload-module", &module]);
    result.unwrap();
}

#[test]
fn sends_add_a_room_and_repeats_and_the_click_is_heard_only_when_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    // A tenth of a second of tone, then silence.
    let path =
        wav(dir.path(), "burst.wav", 1, 96_000, |n, _| if n < 4800 { 0.5 * (n as f32 * 0.1).sin() } else { 0.0 });
    let span = ClipSpan {
        start: 0,
        len: 96_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let rms = |samples: &[f32]| (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let dry = rendered(Mix { tracks: vec![track(vec![span.clone()])], ..Mix::default() }, 96_000);
    assert!(rms(&dry[20_000 * 2..]) < 1e-6);
    // With the reverb, sound goes on after the burst; the burst itself is still there.
    let room = TrackMix { reverb: 0.5, ..track(vec![span.clone()]) };
    let wet = rendered(Mix { tracks: vec![room], ..Mix::default() }, 96_000);
    assert!(rms(&wet[20_000 * 2..40_000 * 2]) > 0.001);
    assert!((rms(&wet[..4800 * 2]) - rms(&dry[..4800 * 2])).abs() < 0.02);
    // With the echo, the burst comes again after `echo_samples`.
    let again = TrackMix { echo: 0.5, ..track(vec![span.clone()]) };
    let echoed = rendered(Mix { tracks: vec![again.clone()], echo_samples: 24_000, ..Mix::default() }, 96_000);
    assert!(rms(&echoed[12_000 * 2..22_000 * 2]) < 1e-4);
    assert!(rms(&echoed[24_000 * 2..28_800 * 2]) > 0.02);
    // A muted track sends nothing.
    let muted = TrackMix { mute: true, ..again };
    let silent = rendered(Mix { tracks: vec![muted], echo_samples: 24_000, ..Mix::default() }, 96_000);
    assert!(rms(&silent) < 1e-6);
    // The metronome: a click at the start of every beat, here every 12 000 samples.
    let click = sg_audio::space::Click { beat: (12_000, 1), beats_per_bar: 4 };
    let clicked = rendered(Mix { tracks: vec![track(vec![span])], click: Some(click), ..Mix::default() }, 96_000);
    assert!(rms(&clicked[36_000 * 2..36_500 * 2]) > 0.05 && rms(&clicked[40_000 * 2..47_000 * 2]) < 1e-4);
}

/// Asks the running PipeWire daemon (read only): `cargo test -p sg-audio -- --ignored inputs --nocapture`.
#[test]
#[ignore = "needs a running PipeWire daemon"]
fn the_inputs_of_this_machine_are_listed() {
    let inputs = sg_audio::record::inputs().unwrap();
    for input in &inputs {
        println!("{} = {}", input.name, input.label);
    }
    assert!(inputs.iter().all(|i| !i.name.is_empty() && !i.label.is_empty()));
}

#[test]
fn a_track_played_by_hand_sounds_within_milliseconds_playing_or_not() {
    let hand = TrackMix { by_hand: true, ..track(Vec::new()) };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix { tracks: vec![hand.clone()], ..Mix::default() });
    engine.settle();
    // Pulls `blocks` device blocks and returns them, the buffer being topped up as while idling.
    let pull = |blocks: usize| {
        let mut out = vec![0.0f32; blocks * 256 * 2];
        for block in out.chunks_mut(512) {
            engine.settle();
            engine.pull(block);
        }
        out
    };
    let first = |out: &[f32]| out.iter().step_by(2).position(|s| s.abs() > 1e-4);
    // Nothing plays and no key is down: silence, and the clock stands still.
    assert_eq!(first(&pull(8)), None);
    assert!(!engine.is_playing());
    // A key: heard after what was already buffered, which is less than 25 ms.
    engine.note(0, true, 60, 100);
    let out = pull(40);
    let after = first(&out).unwrap();
    assert!(after <= 1200, "{after}");
    // Let go, it dies away.
    engine.note(0, false, 60, 0);
    let out = pull(80);
    assert!(out[out.len() - 2048..].iter().all(|s| s.abs() < 1e-3));
    assert!(!engine.is_playing());
    // The same while the timeline plays, where the track also has a note of its own.
    let written = sg_audio::NoteSpan { start: 48_000, len: 4_800, pitch: 72, velocity: 100 };
    engine.set_mix(Mix { tracks: vec![TrackMix { notes: vec![written], ..hand }], ..Mix::default() });
    engine.play(0);
    assert_eq!(first(&pull(8)), None);
    engine.note(0, true, 64, 100);
    let after = first(&pull(8)).unwrap();
    assert!(after <= 1200, "{after}");
    engine.note(0, false, 64, 0);
    // (4096 frames into the timeline by now; the written note starts at one second.)
    let out = pull(190);
    assert!(out[(40_000 - 4096) * 2..(46_000 - 4096) * 2].iter().all(|s| s.abs() < 1e-3));
    let written_at = out[(46_000 - 4096) * 2..].iter().step_by(2).position(|s| s.abs() > 1e-3).unwrap() + 46_000;
    assert!((48_000..48_200).contains(&written_at), "{written_at}");
    // A track that is not played by hand takes no keys.
    engine.note(1, true, 60, 100);
    engine.settle();
}

/// Monitoring against the real sound server, with two null sinks: one
/// engine plays a burst into the first; a second engine monitors that
/// sink and plays into the second, which the first engine records. So the
/// take shows how late the monitored input is heard. Not run by default.
#[test]
#[ignore = "adds two null sinks to the session's PipeWire graph"]
fn a_monitored_input_is_measured_and_heard_a_few_milliseconds_late() {
    let pactl = |args: &[&str]| std::process::Command::new("pactl").args(args).output().unwrap();
    let load = |name: &str| {
        let loaded = pactl(&["load-module", "module-null-sink", &format!("sink_name={name}")]);
        let module = String::from_utf8_lossy(&loaded.stdout).trim().to_owned();
        assert!(loaded.status.success() && !module.is_empty(), "no null sink: {loaded:?}");
        module
    };
    let modules = [load("shotgun-test-source"), load("shotgun-test-heard")];
    let result = std::panic::catch_unwind(|| {
        let dir = tempfile::tempdir().unwrap();
        let played = wav(dir.path(), "burst.wav", 1, 144_000, |n, _| {
            if (48_000..57_600).contains(&n) { 0.5 * ((n as f32) * 0.2).sin() } else { 0.0 }
        });
        let span = ClipSpan {
            start: 0,
            len: 144_000,
            src_in: 0,
            wav: played,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            speed: (1, 1),
            gain_keys: Vec::new(),
            warp: Vec::new(),
        };
        let player =
            Engine::start(Backend::PipeWire { target: Some("shotgun-test-source".into()), quantum: 256 }).unwrap();
        let listener =
            Engine::start(Backend::PipeWire { target: Some("shotgun-test-heard".into()), quantum: 256 }).unwrap();
        // Nothing plays on the listening engine: the device wakes for the monitor alone.
        let monitor = listener.monitor(Some("shotgun-test-source.monitor".into()), true).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut none = [0.0f32; 1];
        listener.take_meters(&mut none);
        assert!(listener.meters().take_input() < 1e-4);
        player.set_mix(Mix { tracks: vec![track(vec![span])], master_gain: 1.0, ..Mix::default() });
        player.play(0);
        let recording = player.record(dir.path().join("take.wav"), Some("shotgun-test-heard.monitor".into())).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2500));
        let take = recording.stop().unwrap();
        player.pause();
        // The meter saw the burst at its level.
        let level = listener.meters().take_input();
        assert!((0.4..0.6).contains(&level), "{level}");
        let bytes = std::fs::read(&take.path).unwrap();
        let samples: Vec<f32> =
            bytes[44..].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let first = samples.iter().step_by(2).position(|s| s.abs() > 0.05).expect("the burst was heard") as i64;
        let late = take.start + first - 48_000;
        eprintln!("monitored input heard {late} frames = {:.1} ms late", late as f64 / 48.0);
        assert!((0..2_048).contains(&late), "{late}");
        // Measured only, not heard: nothing arrives any more.
        listener.hear_monitor(false);
        drop(monitor);
        let stats = listener.stats();
        assert_eq!((stats.xruns, stats.underruns), (0, 0));
    });
    for module in &modules {
        pactl(&["unload-module", module]);
    }
    result.unwrap();
}

#[test]
fn what_the_compressor_takes_off_is_read_when_its_stretch_is_heard() {
    let dir = tempfile::tempdir().unwrap();
    // Quiet for a second (-40 dB), then loud (-6 dB) for a second.
    let path =
        wav(dir.path(), "steps.wav", 1, 96_000, |n, _| (if n < 48_000 { 0.01 } else { 0.5 }) * (n as f32 * 0.1).sin());
    let span = ClipSpan {
        start: 0,
        len: 96_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let squeezed = TrackMix {
        tone: sg_audio::Tone { threshold_db: -20.0, ratio: 4.0, ..sg_audio::Tone::default() },
        ..track(vec![span.clone()])
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix { tracks: vec![squeezed, track(vec![span])], ..Mix::default() });
    engine.play(0);
    let mut stages = [[0.0f32; 3]; 2];
    let mut play = |frames: usize| {
        for _ in 0..frames / 480 {
            engine.settle();
            pull(&engine, 480);
        }
        engine.meters().stages(&mut stages);
        stages
    };
    // The loud second is long since worked out (the stages run half a second
    // ahead), but while the quiet one is heard nothing is shown.
    let quiet = play(24_000);
    assert_eq!(quiet, [[0.0; 3]; 2]);
    // In the loud second: 14 dB over the threshold at 4 : 1 is 10.5 dB off, on the squeezed track only.
    let loud = play(48_000);
    assert!((-11.5..=-9.5).contains(&loud[0][1]), "{loud:?}");
    assert_eq!((loud[0][0], loud[0][2], loud[1]), (0.0, 0.0, [0.0; 3]));
    // Paused, nothing is at work.
    engine.pause();
    engine.meters().stages(&mut stages);
    assert_eq!(stages, [[0.0; 3]; 2]);
}

#[test]
fn a_tracks_notes_play_a_recording_when_it_has_one_for_its_sampler() {
    let dir = tempfile::tempdir().unwrap();
    // Half a second whose every frame says where in the recording it is.
    let path = wav(dir.path(), "hit.wav", 1, 24_000, |n, _| ramp(n));
    let sampler = sg_audio::SamplerSpec { wav: path, root: 60, one_shot: true };
    // The root key at 0.1 s, and an octave above it at 1 s; short notes, played to the end.
    let note = |start: i64, pitch: u8| sg_audio::NoteSpan { start, len: 480, pitch, velocity: 127 };
    let sampled =
        TrackMix { notes: vec![note(4_800, 60), note(48_000, 72)], sampler: Some(sampler), ..track(Vec::new()) };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix { tracks: vec![sampled], ..Mix::default() });
    engine.play(0);
    let mut out = vec![0.0f32; 72_000 * 2];
    for block in out.chunks_mut(960) {
        engine.settle();
        engine.pull(block);
    }
    let left = |frame: usize| out[frame * 2];
    // Nothing before the note; then the recording as it is, long after the short note has ended.
    assert_eq!(left(4_799), 0.0);
    assert!(close(left(4_800 + 1_000), ramp(1_000)), "{}", left(5_800));
    assert!(close(left(4_800 + 20_000), ramp(20_000)));
    assert_eq!(left(4_800 + 24_100), 0.0);
    // An octave up it runs through the recording twice as fast.
    assert!(close(left(48_000 + 1_000), ramp(2_000)), "{}", left(49_000));
    assert_eq!(left(48_000 + 12_100), 0.0);
}

#[test]
fn a_stretch_is_played_over_and_over_to_the_sample_and_no_note_hangs_at_its_end() {
    let dir = tempfile::tempdir().unwrap();
    let path = wav(dir.path(), "ramp.wav", 1, 96_000, |n, _| ramp(n));
    let span = ClipSpan {
        start: 0,
        len: 96_000,
        src_in: 0,
        wav: path,
        gain: 1.0,
        fade_in: 0,
        fade_out: 0,
        speed: (1, 1),
        gain_keys: Vec::new(),
        warp: Vec::new(),
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(Mix { tracks: vec![track(vec![span])], ..Mix::default() });
    // From half a second to one second, started from the top.
    engine.set_loop(Some((24_000, 48_000)));
    engine.play(0);
    let mut out = vec![0.0f32; 120_000 * 2];
    for block in out.chunks_mut(1000) {
        engine.settle();
        engine.pull(block);
    }
    // Up to the end of the stretch as it lies, then its start again, every round, to the sample.
    let expected = |frame: usize| if frame < 48_000 { frame } else { 24_000 + (frame - 48_000) % 24_000 };
    for frame in [0, 23_999, 24_000, 47_999, 48_000, 48_001, 71_999, 72_000, 95_999, 96_000, 119_999] {
        assert!(close(out[frame * 2], ramp(expected(frame))), "frame {frame}: {}", out[frame * 2]);
    }
    // The clock goes round with it.
    let position = engine.position();
    assert!((24_000..48_000).contains(&position), "{position}");
    // Started after the stretch, playback goes on as usual.
    engine.play(60_000);
    engine.settle();
    assert!(close(pull(&engine, 500)[0], ramp(60_000)));
    // A note held across the end of the stretch is let go there, not left sounding.
    let held = sg_audio::NoteSpan { start: 40_000, len: 30_000, pitch: 60, velocity: 100 };
    engine.set_mix(Mix { tracks: vec![TrackMix { notes: vec![held], ..track(Vec::new()) }], ..Mix::default() });
    engine.play(36_000);
    let mut out = vec![0.0f32; 36_000 * 2];
    for block in out.chunks_mut(1000) {
        engine.settle();
        engine.pull(block);
    }
    let level = |from: usize, to: usize| out[from * 2..to * 2].iter().fold(0.0f32, |m, s| m.max(s.abs()));
    // (Sounding from 40 000; the stretch ends 12 000 frames in; half a second later it has died away,
    // until the next round reaches the note again 16 000 frames after the wrap.)
    assert!(level(5_000, 11_000) > 0.01);
    assert!(level(24_000, 27_000) < level(5_000, 11_000) * 0.05, "{}", level(24_000, 27_000));
    assert!(level(29_000, 35_000) > 0.01);
    // Without the loop the clock runs on.
    engine.set_loop(None);
    engine.play(47_000);
    for _ in 0..4 {
        engine.settle();
        pull(&engine, 1000);
    }
    assert!(engine.position() > 48_000);
}

/// Through the ALSA sequencer of this machine (no sound device, no sound):
/// a second client stands in for a keyboard and sends to the listening port.
#[test]
fn notes_from_a_midi_keyboard_arrive_as_keys() {
    use alsa::seq::{Addr, EvNote, Event, EventType, PortCap, PortType, Seq};
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = heard.clone();
    let listening = match sg_audio::midi::MidiIn::start("Shotgun test", move |down, pitch, velocity| {
        sink.lock().unwrap().push((down, pitch, velocity));
    }) {
        Ok(listening) => listening,
        Err(e) => {
            eprintln!("no MIDI sequencer on this machine ({e}): nothing to test");
            return;
        }
    };
    let keyboard = Seq::open(None, Some(alsa::Direction::Playback), false).unwrap();
    let name = std::ffi::CString::new("Shotgun test keyboard").unwrap();
    keyboard.set_client_name(&name).unwrap();
    let port = keyboard
        .create_simple_port(&name, PortCap::READ | PortCap::SUBS_READ, PortType::MIDI_GENERIC | PortType::APPLICATION)
        .unwrap();
    let to = Addr { client: listening.address.0, port: listening.address.1 };
    let send = |kind: EventType, note: u8, velocity: u8| {
        let data = EvNote { channel: 0, note, velocity, off_velocity: 0, duration: 0 };
        let mut event = Event::new(kind, &data);
        event.set_source(port);
        event.set_dest(to);
        event.set_direct();
        keyboard.event_output(&mut event).unwrap();
        keyboard.drain_output().unwrap();
    };
    send(EventType::Noteon, 64, 90);
    send(EventType::Noteoff, 64, 0);
    // (Keyboards also end a note with a key-down at velocity 0.)
    send(EventType::Noteon, 67, 1);
    send(EventType::Noteon, 67, 0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while heard.lock().unwrap().len() < 4 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(*heard.lock().unwrap(), [(true, 64, 90), (false, 64, 0), (true, 67, 1), (false, 67, 0)]);
}

#[test]
fn notes_written_or_taken_away_while_playing_need_no_new_graph_and_leave_nothing_hanging() {
    let note = |start: i64, len: i64, pitch: u8| sg_audio::NoteSpan { start, len, pitch, velocity: 100 };
    // Played by hand, so that what changes is heard within milliseconds; a long note from the start.
    let with = |notes: Vec<sg_audio::NoteSpan>| Mix {
        tracks: vec![TrackMix { notes, by_hand: true, ..track(Vec::new()) }],
        ..Mix::default()
    };
    let engine = Engine::start(Backend::Manual).unwrap();
    engine.set_mix(with(vec![note(0, 480_000, 60)]));
    engine.play(0);
    let pull_for = |frames: usize| {
        let mut out = vec![0.0f32; frames * 2];
        for block in out.chunks_mut(512) {
            engine.settle();
            engine.pull(block);
        }
        out.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    };
    assert!(pull_for(9_600) > 0.01);
    // The note is taken away while it sounds: it is let go, and the clock runs on undisturbed.
    let before = engine.position();
    engine.set_mix(with(Vec::new()));
    pull_for(14_400);
    assert!(pull_for(4_800) < 1e-3);
    let after = engine.position();
    // (The position is reckoned on from the last block by the clock on the wall: about, not to the sample.)
    assert!((after - before - 19_200).abs() < 1_000, "{}", after - before);
    // A note written ahead of the playhead sounds when the playhead gets there.
    engine.set_mix(with(vec![note(after + 4_800, 4_800, 67)]));
    assert!(pull_for(4_000) < 1e-3);
    assert!(pull_for(4_000) > 0.01);
}
