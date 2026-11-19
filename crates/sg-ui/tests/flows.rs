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

/// Puts the playhead at `frame`: the player reports it to the shell each
/// frame, and here the test stands in for the player.
fn playhead(harness: &mut Harness<'_, App>, frame: i64) {
    harness.state_mut().view.frame = frame;
    harness.state_mut().shell.set_playhead(frame);
    harness.run_steps(1);
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
    playhead(&mut harness, 600);
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

/// (start, length) of the clips on a track row, in the row's own units.
fn clips(harness: &Harness<'_, App>, row: usize) -> Vec<(i64, i64)> {
    let project = harness.state().shell.session().unwrap().project().clone();
    project.sequences[0].tracks[row].clips.iter().map(|c| (c.start, c.len)).collect()
}

fn drag(harness: &mut Harness<'_, App>, from: egui::Pos2, to: egui::Pos2) {
    let button = egui::PointerButton::Primary;
    harness.event(egui::Event::PointerMoved(from));
    harness.run_steps(1);
    harness.event(egui::Event::PointerButton { pos: from, button, pressed: true, modifiers: Modifiers::NONE });
    harness.run_steps(1);
    // In steps, as a hand moves: the first one only starts the drag.
    for step in 1..=4 {
        harness.event(egui::Event::PointerMoved(from + (to - from) * (step as f32 / 4.0)));
        harness.run_steps(1);
    }
    harness.event(egui::Event::PointerButton { pos: to, button, pressed: false, modifiers: Modifiers::NONE });
    harness.run_steps(3);
}

fn place(harness: &mut Harness<'_, App>, label: &str) {
    right_click(harness, label);
    click(harness, "Place at the playhead (Enter)");
}

fn transport(harness: &Harness<'_, App>) -> Vec<sg_ui::Transport> {
    harness
        .state()
        .commands
        .iter()
        .filter_map(|c| if let Command::Transport(t) = c { Some(t.clone()) } else { None })
        .collect()
}

#[test]
fn keys_cut_and_ripple_at_the_playhead() {
    let mut harness = editor();
    place(&mut harness, "first take");
    assert_eq!((clips(&harness, 0), clips(&harness, 1)), (vec![(0, 600)], vec![(0, 480_000)]));
    // S splits picture and sound at the playhead.
    playhead(&mut harness, 300);
    harness.key_press(Key::S);
    harness.run_steps(3);
    assert_eq!(
        (clips(&harness, 0), clips(&harness, 1)),
        (vec![(0, 300), (300, 300)], vec![(0, 240_000), (240_000, 240_000)])
    );
    // Q removes from the start of the clip under the playhead up to it, and closes the gap.
    playhead(&mut harness, 400);
    harness.key_press(Key::Q);
    harness.run_steps(3);
    assert_eq!(clips(&harness, 0), vec![(0, 300), (300, 200)]);
    // E removes from the playhead to the end of the clip.
    playhead(&mut harness, 100);
    harness.key_press(Key::E);
    harness.run_steps(3);
    assert_eq!(clips(&harness, 0), vec![(0, 100), (100, 200)]);
    assert_eq!(clips(&harness, 1), vec![(0, 80_000), (80_000, 160_000)]);
    // Three undos (the cap Y) bring the whole take back.
    for _ in 0..3 {
        harness.key_press(Key::Z);
        harness.run_steps(2);
    }
    assert_eq!((clips(&harness, 0), clips(&harness, 1)), (vec![(0, 600)], vec![(0, 480_000)]));
    // Space, A and D reach the player.
    harness.key_press(Key::Space);
    harness.key_press(Key::D);
    harness.run_steps(3);
    assert!(
        transport(&harness).contains(&sg_ui::Transport::Toggle)
            && transport(&harness).contains(&sg_ui::Transport::Step(1))
    );
}

#[test]
fn a_clip_is_moved_by_dragging_and_takes_its_sound_along() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let grab = on_timeline(&harness, 200.0, 0);
    drag(&mut harness, grab, grab + vec2(250.0, 0.0));
    let (picture, sound) = (clips(&harness, 0), clips(&harness, 1));
    // 250 points at about 0.8 frames per point (20 s across the lanes).
    assert!(picture[0].0 > 150 && picture[0].0 < 260 && picture[0].1 == 600, "{picture:?}");
    assert_eq!(sound[0].0, picture[0].0 * 800, "{sound:?}");
    // One undo step.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(clips(&harness, 0), vec![(0, 600)]);
    // X with the clip selected (the drag selected it) removes it and its sound.
    pointer(&mut harness, grab, egui::PointerButton::Primary);
    harness.key_press(Key::X);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0).len(), clips(&harness, 1).len()), (0, 0));
}

#[test]
fn a_clip_edge_is_trimmed_by_dragging() {
    let mut harness = editor();
    place(&mut harness, "first take");
    // The right edge: where the 600th frame is. Find it from the left: the
    // clip is under the pointer just before it and not just after.
    let rect = harness.get_by_label("Timeline").rect();
    let frames_per_point = 1200.0 / (rect.width() - 120.0);
    let edge = on_timeline(&harness, 600.0 / frames_per_point - 2.0, 0);
    drag(&mut harness, edge, edge - vec2(150.0, 0.0));
    let (picture, sound) = (clips(&harness, 0), clips(&harness, 1));
    assert!(picture[0].0 == 0 && picture[0].1 > 420 && picture[0].1 < 520, "{picture:?}");
    assert_eq!(sound[0].1, picture[0].1 * 800, "the sound is trimmed with the picture: {sound:?}");
}

#[test]
fn a_click_on_the_ruler_moves_the_playhead() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let rect = harness.get_by_label("Timeline").rect();
    pointer(&mut harness, egui::pos2(rect.left() + 120.0 + 370.0, rect.top() + 8.0), egui::PointerButton::Primary);
    let seeks: Vec<i64> = transport(&harness)
        .into_iter()
        .filter_map(|t| if let sg_ui::Transport::Seek(f) = t { Some(f) } else { None })
        .collect();
    assert!(seeks.last().is_some_and(|f| (250..=350).contains(f)), "{seeks:?}");
}

#[test]
fn a_title_is_added_and_its_text_edited() {
    let mut harness = editor();
    harness.key_press(Key::T);
    harness.run_steps(3);
    let title = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| !t.clips.is_empty())?.clone();
        let clip = track.clips.iter().next()?;
        track.props_of(clip.id)?.title.clone()
    };
    assert_eq!(title(&harness).map(|t| t.text), Some("Title".to_owned()));
    // The new title is selected: its position can be set in the inspector.
    assert!(harness.query_by_label("Position X").is_some());
}

#[test]
fn the_add_popover_imports_a_link_as_sound_only() {
    let mut harness = editor();
    harness.key_press_modifiers(Modifiers::CTRL, Key::Q);
    harness.run_steps(3);
    assert!(harness.query_by_label("ADD ASSET").is_some());
    harness.event(egui::Event::Text("https://example.org/a-song".to_owned()));
    harness.run_steps(2);
    click(&mut harness, "Sound only");
    click(&mut harness, "Import link");
    let imported = harness.state().commands.iter().any(|c| {
        matches!(c, Command::Import { source: sg_project::ImportSource::UrlSound(url), .. } if url == "https://example.org/a-song")
    });
    assert!(imported);
    assert!(harness.query_by_label("ADD ASSET").is_none());
}

#[test]
fn the_library_makes_a_project_from_what_was_typed() {
    let mut shell = Shell::new();
    shell.set_library(Ok(Vec::new()));
    let dir = tempfile::tempdir().unwrap();
    let app = App { shell, view: ViewerState::default(), commands: Vec::new(), root: dir.path().to_owned(), _dir: dir };
    let mut harness = Harness::builder().with_size(vec2(1600.0, 900.0)).with_accessibility_check(false).build_ui_state(
        |ui, app: &mut App| {
            let commands = app.shell.frame(ui, 0, &app.view);
            app.commands.extend(commands);
        },
        app,
    );
    harness.run_steps(3);
    harness.event(egui::Event::Text("Summer film".to_owned()));
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::CTRL, Key::N);
    harness.run_steps(3);
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::Create { name } if name == "Summer film")));
}

#[test]
fn dragging_in_the_viewer_moves_the_selected_picture() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let on_clip = on_timeline(&harness, 100.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    // The viewer is what is left between the asset panel, the inspector and the timeline.
    let middle = egui::pos2(810.0, 330.0);
    drag(&mut harness, middle, middle + vec2(120.0, -40.0));
    let project = harness.state().shell.session().unwrap().project().clone();
    let track = &project.sequences[0].tracks[0];
    let props = track.props_of(track.clips.at(0).unwrap().id).expect("the drag set a position");
    assert!(props.x.at(0) > 0.05 && props.y.at(0) < -0.02, "{:?} {:?}", props.x, props.y);
    // One drag, one undo step.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    let project = harness.state().shell.session().unwrap().project().clone();
    let track = &project.sequences[0].tracks[0];
    assert!(track.props_of(track.clips.at(0).unwrap().id).is_none());
}

#[test]
fn an_asset_dragged_from_the_panel_lands_on_the_timeline() {
    let mut harness = editor();
    let tile = harness.get_by_label("second take").rect().center();
    let target = on_timeline(&harness, 400.0, 0);
    drag(&mut harness, tile, target);
    let (picture, sound) = (clips(&harness, 0), clips(&harness, 1));
    assert_eq!((picture.len(), sound.len()), (1, 1));
    assert!(picture[0].0 > 250 && picture[0].0 < 400, "{picture:?}");
    assert_eq!(sound[0].0, picture[0].0 * 800);
}

#[test]
fn the_wheel_zooms_the_timeline_around_the_pointer() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let at = on_timeline(&harness, 300.0, 0);
    // Where a click at this point seeks to, before and after zooming in.
    let seek_at = |harness: &mut Harness<'_, App>, pos: egui::Pos2| {
        let rect = harness.get_by_label("Timeline").rect();
        pointer(harness, egui::pos2(pos.x, rect.top() + 8.0), egui::PointerButton::Primary);
        transport(harness)
            .into_iter()
            .rev()
            .find_map(|t| if let sg_ui::Transport::Seek(f) = t { Some(f) } else { None })
            .unwrap()
    };
    let here = seek_at(&mut harness, at);
    let further = seek_at(&mut harness, at + vec2(200.0, 0.0));
    harness.event(egui::Event::PointerMoved(at));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 240.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    harness.run_steps(6);
    // The frame under the pointer stays; a point 200 to the right is now nearer in time.
    let (here_after, further_after) = (seek_at(&mut harness, at), seek_at(&mut harness, at + vec2(200.0, 0.0)));
    assert!((here_after - here).abs() <= 3, "{here} -> {here_after}");
    assert!(
        further_after - here_after < (further - here) * 3 / 4,
        "{} -> {}",
        further - here,
        further_after - here_after
    );
}

#[test]
fn f1_shows_every_key_and_esc_closes_it() {
    let mut harness = editor();
    harness.key_press(Key::F1);
    harness.run_steps(3);
    assert!(harness.query_by_label("KEYS").is_some());
    // The Swiss caps: undo is on the key that reads Y.
    assert!(harness.query_by_label("Y, Ctrl+Z").is_some());
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label("KEYS").is_none());
}

#[test]
fn f_on_a_sound_clip_keys_its_volume() {
    let mut harness = editor();
    place(&mut harness, "first take");
    // Click the sound clip (row 1): it is the one the keys act on.
    let on_sound = on_timeline(&harness, 100.0, 1);
    pointer(&mut harness, on_sound, egui::PointerButton::Primary);
    for frame in [60, 240] {
        playhead(&mut harness, frame);
        harness.run_steps(1);
        harness.key_press(Key::F);
        harness.run_steps(3);
    }
    let project = harness.state().shell.session().unwrap().project().clone();
    let track = &project.sequences[0].tracks[1];
    let props = track.props_of(track.clips.iter().next().unwrap().id).expect("keys were set");
    assert_eq!(props.gain_db.keys.iter().map(|k| k.frame).collect::<Vec<_>>(), [60, 240]);
    // The picture clip got none of it.
    let picture = &project.sequences[0].tracks[0];
    assert!(picture.props_of(picture.clips.iter().next().unwrap().id).is_none());
}

#[test]
fn a_marked_stretch_can_be_exported_alone() {
    let mut harness = editor();
    place(&mut harness, "first take");
    for frame in [120, 360] {
        playhead(&mut harness, frame);
        harness.run_steps(1);
        harness.key_press(Key::G);
        harness.run_steps(2);
    }
    playhead(&mut harness, 200);
    harness.key_press_modifiers(Modifiers::CTRL, Key::E);
    harness.run_steps(3);
    click(&mut harness, "Only the selected clips, or the stretch between the markers around the playhead");
    let buttons: Vec<_> = harness.get_all_by_label("Export").collect();
    buttons.last().unwrap().click();
    harness.run_steps(3);
    let range = harness
        .state()
        .commands
        .iter()
        .find_map(|c| if let Command::Export { request, .. } = c { Some(request.range) } else { None });
    assert_eq!(range, Some(Some((120, 360))));
}

#[test]
fn handles_in_the_viewer_size_and_turn_the_selected_picture() {
    let mut harness = editor();
    place(&mut harness, "first take");
    assert!(harness.query_by_label("Turn handle").is_none());
    let on_clip = on_timeline(&harness, 100.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    let props = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = &project.sequences[0].tracks[0];
        track.props_of(track.clips.at(0).unwrap().id).map(|p| (p.scale.at(0), p.rotation.at(0), p.x.at(0)))
    };
    // A corner pulled towards the middle of the picture: smaller.
    let corners: Vec<egui::Rect> = harness.get_all_by_label("Size handle").map(|n| n.rect()).collect();
    assert_eq!(corners.len(), 4);
    let middle = corners.iter().fold(egui::Vec2::ZERO, |sum, r| sum + r.center().to_vec2()) / 4.0;
    let corner = corners[0].center();
    drag(&mut harness, corner, corner + (middle.to_pos2() - corner) * 0.5);
    let (scale, rotation, x) = props(&harness).expect("the handle changed the clip");
    assert!((scale - 0.5).abs() < 0.08 && rotation == 0.0 && x == 0.0, "{scale} {rotation} {x}");
    // The turning handle pulled to the right: clockwise.
    let knob = harness.get_by_label("Turn handle").rect().center();
    drag(&mut harness, knob, knob + vec2(60.0, 0.0));
    let (scale_after, rotation, _) = props(&harness).unwrap();
    assert!(rotation > 10.0 && rotation < 60.0 && (scale_after - scale).abs() < 1e-4, "{rotation}");
    // Two gestures, two undo steps.
    harness.key_press(Key::Z);
    harness.run_steps(2);
    harness.key_press(Key::Z);
    harness.run_steps(2);
    assert_eq!(props(&harness), None);
}
