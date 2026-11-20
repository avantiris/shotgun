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
            gain_keys: Vec::new(),
            warp: Vec::new(),
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
    let out = rendered(Mix { tracks: vec![track(vec![span])], master_gain: 1.0 }, 36_000);
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
    let mix =
        Mix { tracks: vec![TrackMix { plugin: Some(spec), notes: vec![note], ..track(Vec::new()) }], master_gain: 1.0 };
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
    // Pausing writes the plugin's state where the project keeps it.
    engine.pause();
    engine.settle();
    assert!(std::fs::metadata(&state_file).is_ok_and(|m| m.len() > 1000));
    assert!(engine.plugin_news().is_empty());
    // A plugin that is not there is said so, and the track is silent rather than broken.
    let missing = sg_audio::PluginSpec { key: 2, path: dir.path().join("nothing.vst3"), state_file };
    engine.set_mix(Mix {
        tracks: vec![TrackMix { plugin: Some(missing), notes: vec![note], ..track(Vec::new()) }],
        master_gain: 1.0,
    });
    engine.settle();
    let news = engine.plugin_news();
    assert!(news.len() == 1 && news[0].contains("nothing.vst3"), "{news:?}");
}
