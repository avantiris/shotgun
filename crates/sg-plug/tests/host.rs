//! The plugin host with a real plugin: Surge XT, which the dev shell
//! provides (`SHOTGUN_TEST_VST3` names the folder its bundles are in).
//! Without it these tests say so and pass: there is nothing to host.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use sg_plug::{Client, NoteEvent, PlugError};

fn bundle(name: &str) -> Option<PathBuf> {
    // SAFETY: tests of this file all set the same value.
    unsafe { std::env::set_var("SHOTGUN_PLUGIN_HOST", env!("CARGO_BIN_EXE_shotgun-plugin")) };
    let Some(folder) = std::env::var_os("SHOTGUN_TEST_VST3") else {
        eprintln!("SHOTGUN_TEST_VST3 is not set: no plugin to test with");
        return None;
    };
    // (CLAP plugins lie in the folder beside the VST3 ones.)
    let folder = PathBuf::from(folder);
    Some(if name.ends_with(".clap") { folder.with_file_name("clap").join(name) } else { folder.join(name) })
}

fn level(block: &[f32]) -> f32 {
    let mean = block.iter().map(|s| s * s).sum::<f32>() / block.len().max(1) as f32;
    10.0 * mean.max(1e-12).log10()
}

#[test]
fn an_instrument_plays_the_notes_it_is_sent_and_keeps_its_state() {
    instrument("Surge XT.vst3");
}

#[test]
fn a_clap_instrument_plays_the_notes_it_is_sent_and_keeps_its_state() {
    instrument("Surge XT.clap");
}

fn instrument(name: &str) {
    let Some(path) = bundle(name) else { return };
    let mut plugin = Client::start(&path).unwrap();
    assert!(plugin.loaded.instrument && plugin.loaded.has_editor, "{:?}", plugin.loaded);
    assert_eq!(plugin.loaded.name, "Surge XT");
    // Silence until a note starts; the note starts where its offset says.
    let mut block = vec![0.0f32; 2 * 4800];
    assert!(plugin.process(&mut block, &[]));
    assert!(level(&block) < -100.0);
    let on = NoteEvent { offset: 2400, on: true, pitch: 60, velocity: 100 };
    assert!(plugin.process(&mut block, &[on]));
    assert!(level(&block[..2 * 2400]) < -100.0, "{}", level(&block[..2 * 2400]));
    assert!(level(&block[2 * 2400..]) > -40.0, "{}", level(&block[2 * 2400..]));
    // A block longer than one request carries is sent in pieces.
    let mut long = vec![0.0f32; 2 * 20_000];
    assert!(plugin.process(&mut long, &[]));
    assert!(level(&long[2 * 15_000..]) > -40.0);
    // It is told the tempo and where the song is, and goes on as before.
    plugin.set_tempo(93.5, 480_000);
    assert!(plugin.process(&mut long, &[]) && !plugin.is_dead());
    // A jump on the timeline ends what sounds.
    plugin.reset();
    let mut tail = vec![0.0f32; 2 * 96_000];
    plugin.process(&mut tail, &[]);
    assert!(level(&tail[2 * 90_000..]) < -90.0, "{}", level(&tail[2 * 90_000..]));
    // Its state comes out and goes into a second one of it.
    let state = plugin.state().unwrap();
    assert!(state.len() > 1000, "{}", state.len());
    let mut twin = Client::start(&path).unwrap();
    assert!(twin.set_state(&state));
    assert!(!twin.is_dead());
}

#[test]
fn an_effect_treats_sound_and_a_plugin_that_dies_costs_only_its_own_sound() {
    effect("Surge XT Effects.vst3");
}

#[test]
fn a_clap_effect_treats_sound_and_a_plugin_that_dies_costs_only_its_own_sound() {
    effect("Surge XT Effects.clap");
}

fn effect(name: &str) {
    let Some(path) = bundle(name) else { return };
    let mut plugin = Client::start(&path).unwrap();
    assert!(!plugin.loaded.instrument);
    let tone: Vec<f32> = (0..2 * 9600).map(|i| 0.2 * ((i / 2) as f32 * 0.05).sin()).collect();
    let mut block = tone.clone();
    assert!(plugin.process(&mut block, &[]));
    assert!((level(&block) - level(&tone)).abs() < 6.0, "{} {}", level(&block), level(&tone));
    // Its process is ended from outside: the next block passes as it is, and it says so.
    std::process::Command::new("kill").args(["-9", &plugin.pid().to_string()]).status().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut after = tone.clone();
    assert!(!plugin.process(&mut after, &[]));
    assert!(plugin.is_dead());
    assert_eq!(after, tone);
}

#[test]
fn what_is_not_a_plugin_is_refused_with_a_reason() {
    if bundle("x").is_none() {
        return;
    }
    let nothing = tempfile::tempdir().unwrap();
    match Client::start(nothing.path()) {
        Err(PlugError::Load(why)) => assert!(why.contains("no Linux plugin"), "{why}"),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("an empty folder was loaded as a plugin"),
    }
}

/// The plugin's own window. Not run by default: it opens a window on the
/// desktop. Run it with the rules of `scripts/run-hidden.sh` in force and
/// `SHOTGUN_APP_ID=shotgun-dev`, which keeps it on a hidden workspace:
/// `cargo nextest run -p sg-plug --run-ignored only the_editor`.
#[test]
#[ignore = "opens a window"]
fn the_editor_window_opens_and_closes() {
    editor("Surge XT.vst3");
}

#[test]
#[ignore = "opens a window"]
fn the_editor_window_of_a_clap_plugin_opens_and_closes() {
    editor("Surge XT.clap");
}

fn editor(name: &str) {
    let Some(path) = bundle(name) else { return };
    let mut plugin = Client::start(&path).unwrap();
    assert!(plugin.open_editor());
    // The plugin keeps working while its window is open, and the window is there.
    let mut block = vec![0.0f32; 2 * 4800];
    for _ in 0..40 {
        assert!(plugin.process(&mut block, &[]));
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let clients = std::process::Command::new("hyprctl").args(["clients", "-j"]).output().unwrap();
    let clients = String::from_utf8_lossy(&clients.stdout).into_owned();
    assert!(clients.contains("Surge XT"), "no window titled Surge XT among the desktop's windows");
    plugin.close_editor();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let clients = std::process::Command::new("hyprctl").args(["clients", "-j"]).output().unwrap();
    assert!(!String::from_utf8_lossy(&clients.stdout).contains("\"title\": \"Surge XT\""));
    assert!(plugin.process(&mut block, &[]));
}

/// A Windows plugin behind the bridge (yabridge under Wine). Not part of
/// the usual run: it needs a Wine prefix with a plugin in it, bridged
/// into a `.vst3` bundle that `SHOTGUN_TEST_BRIDGED` names, and a display
/// for Wine (a virtual X server will do). docs/spikes has the steps.
#[test]
#[ignore = "needs a bridged Windows plugin"]
fn a_bridged_windows_instrument_plays_and_keeps_its_state() {
    // SAFETY: as in `bundle`.
    unsafe { std::env::set_var("SHOTGUN_PLUGIN_HOST", env!("CARGO_BIN_EXE_shotgun-plugin")) };
    let path = PathBuf::from(std::env::var_os("SHOTGUN_TEST_BRIDGED").unwrap());
    let started = std::time::Instant::now();
    let mut plugin = Client::start(&path).unwrap();
    println!("loaded {:?} in {:.1} s", plugin.loaded, started.elapsed().as_secs_f64());
    assert!(plugin.loaded.instrument, "{:?}", plugin.loaded);
    let mut block = vec![0.0f32; 2 * 4800];
    assert!(plugin.process(&mut block, &[]));
    assert!(level(&block) < -90.0, "{}", level(&block));
    let on = NoteEvent { offset: 2400, on: true, pitch: 60, velocity: 100 };
    assert!(plugin.process(&mut block, &[on]));
    assert!(level(&block[..2 * 2400]) < -90.0, "{}", level(&block[..2 * 2400]));
    // (A sampler with nothing loaded in it, as Kontakt is when it is new, plays nothing: said, not failed.)
    println!("a note sounds at {:.0} dB", level(&block[2 * 2400..]));
    // How long a block takes through the bridge: it has to stay well under real time.
    let started = std::time::Instant::now();
    let mut blocks = vec![0.0f32; 2 * 512];
    for _ in 0..200 {
        assert!(plugin.process(&mut blocks, &[]));
    }
    println!("512 frames take {:.0} us", started.elapsed().as_secs_f64() * 1e6 / 200.0);
    let state = plugin.state().unwrap();
    assert!(!state.is_empty());
    let mut twin = Client::start(&path).unwrap();
    assert!(twin.set_state(&state));
    assert!(!twin.is_dead());
    // (`SHOTGUN_TEST_NO_WINDOW`: leave the plugin's window shut, on a desktop someone is working at.)
    if plugin.loaded.has_editor && std::env::var_os("SHOTGUN_TEST_NO_WINDOW").is_none() {
        let at = std::time::Instant::now();
        assert!(plugin.open_editor());
        println!("window open after {:.1} s", at.elapsed().as_secs_f64());
        // (`SHOTGUN_TEST_WINDOW_SECS`: how long it stays open, to have a look at it.)
        let open_for = std::env::var("SHOTGUN_TEST_WINDOW_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        std::thread::sleep(std::time::Duration::from_secs(open_for));
        // With its window open it still plays, takes a reset and gives its state.
        for _ in 0..200 {
            assert!(plugin.process(&mut blocks, &[]));
        }
        println!("blocks with the window open: ok after {:.1} s", at.elapsed().as_secs_f64());
        plugin.reset();
        plugin.set_tempo(120.0, 0);
        println!("reset and tempo: ok after {:.1} s", at.elapsed().as_secs_f64());
        let size = plugin.state().map(|s| s.len());
        println!("state with the window open: {size:?} after {:.1} s", at.elapsed().as_secs_f64());
        plugin.close_editor();
        println!("window closed after {:.1} s", at.elapsed().as_secs_f64());
        assert!(!plugin.is_dead());
    }
}
