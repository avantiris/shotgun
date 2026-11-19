//! Whole flows through the shell, driven the way a person drives them:
//! clicks on what is labelled so, key presses, right-click menus. No GPU
//! and no window: egui runs headless and reports what is on screen
//! through its accessibility tree.

#![allow(clippy::unwrap_used)]

use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use sg_core::{Asset, AssetId, AssetKind, AudioInfo, Rate, Source, TrackKind, VideoInfo};
use sg_project::ImportEvent;
use sg_ui::{Command, Shell, ViewerState};

struct App {
    shell: Shell,
    view: ViewerState,
    commands: Vec<Command>,
    root: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

/// A take with picture and sound: 10 s at 60 fps.
fn take(id: u128, label: &str) -> Asset {
    Asset {
        id: AssetId(id),
        blake3: format!("{id:064x}"),
        size: 1,
        file: format!("assets/video/{label}.mp4"),
        kind: AssetKind::Video,
        label: label.to_owned(),
        tags: vec![],
        source: Source { origin: "test".into(), imported: id as u64, uploader: None },
        video: Some(VideoInfo {
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            rate: Rate::FPS_60,
            frames: 600,
            pix_fmt: "yuv420p".into(),
            bit_depth: 8,
            rotation: 0,
            alpha: false,
            header_vfr: false,
            color_primaries: None,
            color_transfer: None,
            color_space: None,
            color_range: None,
        }),
        audio: Some(AudioInfo { codec: "aac".into(), sample_rate: 48_000, channels: 2, samples: 480_000 }),
    }
}

/// The editor with an open, empty project and two assets in the panel.
fn editor<'a>() -> Harness<'a, App> {
    let dir = tempfile::tempdir().unwrap();
    let root = sg_project::create(dir.path(), "Flows").unwrap().path;
    let mut shell = Shell::new();
    shell.opened(sg_project::open(&root, || {}).map_err(|e| e.to_string()));
    for (id, label) in [(1, "first take"), (2, "second take")] {
        shell.import_event(ImportEvent::Usable {
            job: id as u64,
            root: root.clone(),
            asset: Box::new(take(id, label)),
        });
    }
    let app = App { shell, view: ViewerState::default(), commands: Vec::new(), root, _dir: dir };
    // (Some text fields have no accessible name yet; that is not what these tests are about.)
    let mut harness = Harness::builder().with_size(vec2(1600.0, 900.0)).with_accessibility_check(false).build_ui_state(
        |ui, app: &mut App| {
            let commands = app.shell.frame(ui, 0, &app.view);
            app.commands.extend(commands);
        },
        app,
    );
    harness.run_steps(3);
    harness
}

/// (kind, name, number of clips) of every track, top first.
fn tracks(harness: &Harness<'_, App>) -> Vec<(TrackKind, String, usize)> {
    let project = harness.state().shell.session().unwrap().project().clone();
    project.sequences[0].tracks.iter().map(|t| (t.kind, t.name.clone(), t.clips.len())).collect()
}

fn click(harness: &mut Harness<'_, App>, label: &str) {
    harness.get_by_label(label).click();
    harness.run_steps(3);
}

fn right_click(harness: &mut Harness<'_, App>, label: &str) {
    harness.get_by_label(label).click_secondary();
    harness.run_steps(3);
}

#[test]
fn track_buttons_add_tracks() {
    let mut harness = editor();
    assert_eq!(tracks(&harness).len(), 2);
    click(&mut harness, "+ Audio");
    click(&mut harness, "+ Video");
    let names: Vec<String> = tracks(&harness).into_iter().map(|t| t.1).collect();
    assert_eq!(names, ["V2", "V1", "A1", "A2"]);
}

#[test]
fn an_asset_is_placed_whole_or_as_sound_only_from_its_menu() {
    let mut harness = editor();
    right_click(&mut harness, "first take");
    click(&mut harness, "Place at the playhead (Enter)");
    assert_eq!(tracks(&harness), vec![(TrackKind::Video, "V1".into(), 1), (TrackKind::Audio, "A1".into(), 1)]);
    // The second one as sound only: no picture is added.
    right_click(&mut harness, "second take");
    click(&mut harness, "Place only the sound");
    let after = tracks(&harness);
    assert_eq!(after.iter().filter(|t| t.0 == TrackKind::Video).map(|t| t.2).sum::<usize>(), 1);
    assert_eq!(after.iter().filter(|t| t.0 == TrackKind::Audio).map(|t| t.2).sum::<usize>(), 2);
    // Undo by key (the cap Y, bottom left on the Swiss keyboard, is KeyZ).
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(tracks(&harness).iter().map(|t| t.2).sum::<usize>(), 2);
}

#[test]
fn the_mixer_opens_by_button_and_key_and_mutes_a_track() {
    let mut harness = editor();
    assert!(harness.query_by_label("MIXER").is_none());
    click(&mut harness, "Mixer");
    assert!(harness.query_by_label("MIXER").is_some());
    assert!(harness.query_by_label("Master").is_some());
    // The M in the strip (the track header has one too): either mutes the track.
    let mute = harness.get_all_by_label("M").next().unwrap();
    mute.click();
    harness.run_steps(3);
    let project = harness.state().shell.session().unwrap().project().clone();
    assert!(project.sequences[0].tracks.iter().any(|t| t.kind == TrackKind::Audio && t.mix.mute));
    harness.key_press(Key::M);
    harness.run_steps(3);
    assert!(harness.query_by_label("MIXER").is_none());
}

#[test]
fn a_scenario_is_made_from_the_asset_menu_and_gets_a_look_module() {
    let mut harness = editor();
    right_click(&mut harness, "first take");
    click(&mut harness, "Scenario");
    click(&mut harness, "New scenario");
    // The asset is in a scenario, its measurement was asked for, the panel is open.
    let app = harness.state();
    let project = app.shell.session().unwrap().project().clone();
    assert_eq!(project.scenario_of(AssetId(1)).map(|s| s.name.as_str()), Some("Scenario 1"));
    assert!(
        app.commands.iter().any(|c| matches!(c, Command::AnalyseColour { assets, .. } if assets[0].id == AssetId(1)))
    );
    assert!(harness.query_by_label("MATCH").is_some());
    click(&mut harness, "Add module");
    click(&mut harness, "Tone");
    let project = harness.state().shell.session().unwrap().project().clone();
    assert_eq!(project.scenarios.values().next().unwrap().look.len(), 1);
    // The A/B key switches the look off without losing it.
    harness.key_press(Key::J);
    harness.run_steps(3);
    let project = harness.state().shell.session().unwrap().project().clone();
    let scenario = project.scenarios.values().next().unwrap();
    assert!(!scenario.look_on && scenario.look.len() == 1);
}

#[test]
fn export_starts_from_the_panel() {
    let mut harness = editor();
    right_click(&mut harness, "first take");
    click(&mut harness, "Place at the playhead (Enter)");
    harness.key_press_modifiers(Modifiers::CTRL, Key::E);
    harness.run_steps(3);
    assert!(harness.query_by_label("EXPORT").is_some());
    // The button in the popover (the top bar has one of the same name).
    let buttons: Vec<_> = harness.get_all_by_label("Export").collect();
    buttons.last().unwrap().click();
    harness.run_steps(3);
    let app = harness.state();
    assert!(app.commands.iter().any(|c| matches!(c, Command::Export { root, .. } if *root == app.root)));
}

#[test]
fn the_song_and_sync_are_asked_for_from_the_asset_menu() {
    let mut harness = editor();
    right_click(&mut harness, "first take");
    click(&mut harness, "Use as the song");
    let project = harness.state().shell.session().unwrap().project().clone();
    assert_eq!(project.sequences[0].song.map(|s| s.asset), Some(AssetId(1)));
    // Its sound was put on the audio track, and its beats were asked for.
    assert_eq!(tracks(&harness)[1].2, 1);
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::LoadBeats { id, .. } if *id == AssetId(1))));
    right_click(&mut harness, "second take");
    click(&mut harness, "Sync to the song");
    assert!(
        harness.state().commands.iter().any(|c| matches!(c, Command::SyncTake { take, .. } if *take == AssetId(2)))
    );
    // The answer comes back: the take is laid 2 s into the song on a camera track.
    let root = harness.state().root.clone();
    let found = sg_media::analysis::Offset { lag: 96_000, likeness: 0.5, runner_up: 0.1, drift_ppm: 0.0 };
    harness.state_mut().shell.synced(&root, AssetId(2), Ok(found));
    harness.run_steps(3);
    let names: Vec<(String, usize)> = tracks(&harness).into_iter().map(|t| (t.1, t.2)).collect();
    assert_eq!(names, [("V1".to_owned(), 1), ("Cam 1".to_owned(), 1), ("A1".to_owned(), 1)]);
    // A match that is not clear places nothing.
    let unsure = sg_media::analysis::Offset { lag: 0, likeness: 0.2, runner_up: 0.19, drift_ppm: 0.0 };
    harness.state_mut().shell.synced(&root, AssetId(2), Ok(unsure));
    harness.run_steps(3);
    assert_eq!(tracks(&harness).len(), 3);
}

/// A point on the timeline: `x` points right of the start of the lanes,
/// in the middle of track row `row`. (The track names take 120 points on
/// the left and the ruler 22 at the top; rows are 40 points high here.)
fn on_timeline(harness: &Harness<'_, App>, x: f32, row: usize) -> egui::Pos2 {
    let rect = harness.get_by_label("Timeline").rect();
    egui::pos2(rect.left() + 120.0 + x, rect.top() + 22.0 + 40.0 * row as f32 + 20.0)
}

fn pointer(harness: &mut Harness<'_, App>, pos: egui::Pos2, button: egui::PointerButton) {
    harness.event(egui::Event::PointerMoved(pos));
    harness.run_steps(1);
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE });
        harness.run_steps(1);
    }
    harness.run_steps(2);
}

#[test]
fn a_clip_is_selected_by_a_click_and_loses_its_sound_from_its_menu() {
    let mut harness = editor();
    right_click(&mut harness, "first take");
    click(&mut harness, "Place at the playhead (Enter)");
    // Nothing selected: the inspector shows no clip.
    assert!(harness.query_by_label("CLIP").is_none());
    let on_clip = on_timeline(&harness, 12.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    assert!(harness.query_by_label("CLIP").is_some());
    // Its menu: the sound goes, the picture stays.
    pointer(&mut harness, on_clip, egui::PointerButton::Secondary);
    click(&mut harness, "Remove sound, keep picture");
    assert_eq!(tracks(&harness), vec![(TrackKind::Video, "V1".into(), 1), (TrackKind::Audio, "A1".into(), 0)]);
    // "Stabilise" in the inspector asks for the stabilised copy.
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    click(&mut harness, "Stabilise");
    assert!(
        harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, Command::Stabilise { asset, .. } if asset.id == AssetId(1)))
    );
    let project = harness.state().shell.session().unwrap().project().clone();
    let track = &project.sequences[0].tracks[0];
    assert!(track.props_of(track.clips.at(0).unwrap().id).unwrap().stabilise);
}

#[test]
fn b_puts_a_whip_on_the_cut_and_the_inspector_changes_its_kind() {
    let mut harness = editor();
    for label in ["first take", "second take"] {
        right_click(&mut harness, label);
        click(&mut harness, "Place at the playhead (Enter)");
    }
    // Two clips back to back on V1; the playhead is at the start of the second.
    harness.state_mut().shell.set_playhead(600);
    harness.key_press(Key::B);
    harness.run_steps(3);
    let transition = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = &project.sequences[0].tracks[0];
        track.props_of(track.clips.at(600).unwrap().id).and_then(|p| p.transition)
    };
    assert_eq!(transition(&harness).map(|t| t.kind), Some(sg_core::TransitionKind::Whip));
    // The clip is selected: its transition can be changed in the inspector.
    click(&mut harness, "Slide");
    assert_eq!(transition(&harness).map(|t| (t.kind, t.frames)), Some((sg_core::TransitionKind::Slide, 8)));
    click(&mut harness, "Remove transition");
    assert_eq!(transition(&harness), None);
}

#[test]
fn right_click_on_the_empty_timeline_adds_and_removes_tracks() {
    let mut harness = editor();
    let empty = on_timeline(&harness, 300.0, 1);
    pointer(&mut harness, empty, egui::PointerButton::Secondary);
    // (The inspector has a button of the same name; the menu was made last.)
    let entries: Vec<_> = harness.get_all_by_label("Add video track").collect();
    entries.last().unwrap().click();
    harness.run_steps(3);
    assert_eq!(tracks(&harness).len(), 3);
    // The audio track is now row 2; it is empty and can go.
    let audio_row = on_timeline(&harness, 300.0, 2);
    pointer(&mut harness, audio_row, egui::PointerButton::Secondary);
    click(&mut harness, "Remove track A1");
    let names: Vec<String> = tracks(&harness).into_iter().map(|t| t.1).collect();
    assert_eq!(names, ["V2", "V1"]);
}
