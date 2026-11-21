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
    // (The first export ends, so that the panel offers its button again.)
    harness.state_mut().shell.export_update(sg_ui::ExportUpdate::Failed("cancelled".to_owned()));
    harness.run_steps(2);
    // A share copy is compressed as chosen: "Strong" is level 3; light without a choice.
    click(&mut harness, "Share copy");
    assert!(harness.query_by_label("Compression").is_some());
    click(&mut harness, "Strong");
    let buttons: Vec<_> = harness.get_all_by_label("Export").collect();
    buttons.last().unwrap().click();
    harness.run_steps(3);
    let levels: Vec<u8> = harness
        .state()
        .commands
        .iter()
        .filter_map(|c| if let Command::Export { request, .. } = c { Some(request.compression) } else { None })
        .collect();
    assert_eq!(levels, [1, 3]);
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
    // Such a take is placed by hand: picture on a camera track, sound on a
    // muted track of its own, both selected so that the arrow keys move them.
    playhead(&mut harness, 90);
    right_click(&mut harness, "second take");
    click(&mut harness, "Camera angle at the playhead, synced by hand");
    let names: Vec<(String, usize)> = tracks(&harness).into_iter().map(|t| (t.1, t.2)).collect();
    assert_eq!(
        names,
        [
            ("V1".to_owned(), 1),
            ("Cam 1".to_owned(), 1),
            ("Cam 2".to_owned(), 1),
            ("A1".to_owned(), 1),
            ("Cam 2 sound".to_owned(), 1)
        ]
    );
    let project = harness.state().shell.session().unwrap().project().clone();
    let sequence = &project.sequences[0];
    assert!(sequence.tracks[2].mix.hidden && sequence.tracks[4].mix.mute);
    assert_eq!(sequence.tracks[2].clips.iter().next().map(|c| c.start), Some(90));
    harness.key_press(Key::ArrowRight);
    harness.run_steps(3);
    let project = harness.state().shell.session().unwrap().project().clone();
    let sequence = &project.sequences[0];
    assert_eq!(sequence.tracks[2].clips.iter().next().map(|c| c.start), Some(91));
    assert_eq!(sequence.tracks[4].clips.iter().next().map(|c| c.start), Some(91 * 800));
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
    // "Black" in the same row makes it a dip to black; its button and Shift+B put one on a cut directly.
    click(&mut harness, "Black");
    assert_eq!(transition(&harness).map(|t| t.kind), Some(sg_core::TransitionKind::Black));
    // It comes hard; "Dip" makes all of it a fade, which can then be shaped.
    assert!(harness.query_by_label("Fade").is_some() && harness.query_by_label("Blur").is_none());
    assert!(harness.query_by_label("Darkens").is_none());
    assert_eq!(transition(&harness).map(|t| t.amount), Some(0.0));
    click(&mut harness, "Dip");
    assert_eq!(transition(&harness).map(|t| t.amount), Some(1.0));
    assert!(harness.query_by_label("Darkens").is_some());
    harness.key_press_modifiers(Modifiers::SHIFT, Key::B);
    harness.run_steps(3);
    assert_eq!(transition(&harness), None);
    click(&mut harness, "Clip");
    click(&mut harness, "Black cut");
    assert_eq!(
        transition(&harness).map(|t| (t.kind, t.frames, t.amount)),
        Some((sg_core::TransitionKind::Black, 6, 0.0))
    );
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
    // The cursor is in its text with everything selected: typing replaces it.
    assert!(harness.query_by_label("TITLE").is_some());
    harness.event(egui::Event::Text("Hello".to_owned()));
    harness.run_steps(3);
    assert_eq!(title(&harness).map(|t| t.text), Some("Hello".to_owned()));
    // Escape lets go of the field; keys are keys again (G: a marker).
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    harness.key_press(Key::G);
    harness.run_steps(3);
    assert_eq!(title(&harness).map(|t| t.text), Some("Hello".to_owned()));
    let project = harness.state().shell.session().unwrap().project().clone();
    assert_eq!(project.sequences[0].markers.len(), 1);
    // With the mixer in the way, "Edit text" in the title's menu brings its text back.
    // (A double click on the title does the same; the test's clock cannot make one.)
    click(&mut harness, "Mixer");
    assert!(harness.query_by_label("TITLE").is_none());
    let rect = harness.get_by_label("Timeline").rect();
    let on_title = egui::pos2(rect.left() + 120.0 + 6.0, rect.top() + 22.0 + 20.0);
    pointer(&mut harness, on_title, egui::PointerButton::Secondary);
    click(&mut harness, "Edit text");
    assert!(harness.query_by_label("TITLE").is_some());
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

fn wheel(harness: &mut Harness<'_, App>, at: egui::Pos2, delta: egui::Vec2, modifiers: Modifiers) {
    harness.event(egui::Event::PointerMoved(at));
    harness.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        modifiers,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    harness.run_steps(6);
}

#[test]
fn the_wheel_moves_along_the_timeline_and_with_ctrl_zooms_around_the_pointer() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let at = on_timeline(&harness, 300.0, 0);
    // Where a click at this point seeks to.
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
    // Ctrl + wheel: the frame under the pointer stays; a point 200 to the right is now nearer in time.
    wheel(&mut harness, at, vec2(0.0, 240.0), Modifiers::CTRL);
    let (here_after, further_after) = (seek_at(&mut harness, at), seek_at(&mut harness, at + vec2(200.0, 0.0)));
    assert!((here_after - here).abs() <= 3, "{here} -> {here_after}");
    assert!(
        further_after - here_after < (further - here) * 3 / 4,
        "{} -> {}",
        further - here,
        further_after - here_after
    );
    // The wheel alone moves along the sequence at the same zoom: turned
    // down, what was to the right comes under the pointer.
    wheel(&mut harness, at, vec2(0.0, -120.0), Modifiers::NONE);
    let (here_moved, further_moved) = (seek_at(&mut harness, at), seek_at(&mut harness, at + vec2(200.0, 0.0)));
    assert!(here_moved > here_after + 10, "{here_after} -> {here_moved}");
    assert!(((further_moved - here_moved) - (further_after - here_after)).abs() <= 2);
    // H shows the whole sequence again.
    harness.key_press(Key::H);
    harness.run_steps(3);
    let (here_again, further_again) = (seek_at(&mut harness, at), seek_at(&mut harness, at + vec2(200.0, 0.0)));
    assert_eq!((here_again, further_again), (here, further));
}

#[test]
fn ctrl_c_x_and_v_copy_cut_and_paste_the_selected_clips() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let on_clip = on_timeline(&harness, 12.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    harness.key_press_modifiers(Modifiers::CTRL, Key::C);
    harness.run_steps(3);
    // The application is asked to mark the clipboard.
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::OfferClips)));
    // Without an answer from the clipboard, Ctrl+V pastes the clips: picture and sound.
    playhead(&mut harness, 900);
    harness.key_press_modifiers(Modifiers::CTRL, Key::V);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0), clips(&harness, 1).len()), (vec![(0, 600), (900, 600)], 2));
    assert!(!harness.state().commands.iter().any(|c| matches!(c, Command::ReadClipboard)));
    // Ctrl+X takes the pasted clips (still selected) out and leaves the gap.
    harness.key_press_modifiers(Modifiers::CTRL, Key::X);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0), clips(&harness, 1).len()), (vec![(0, 600)], 1));
    // With the mark on the clipboard, Ctrl+V asks what the clipboard holds now...
    harness.state_mut().shell.clips_offered(true);
    playhead(&mut harness, 2000);
    harness.key_press_modifiers(Modifiers::CTRL, Key::V);
    harness.run_steps(3);
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::ReadClipboard)));
    assert_eq!(clips(&harness, 0), vec![(0, 600)]);
    // ...the mark: the clips are pasted.
    let commands = harness.state_mut().shell.incoming(sg_ui::Incoming::Text(sg_ui::CLIPS_ON_CLIPBOARD.to_owned()));
    assert!(commands.is_empty());
    harness.run_steps(2);
    assert_eq!(clips(&harness, 0), vec![(0, 600), (2000, 600)]);
    // ...a link copied in a browser since: that is imported, and no clip is pasted.
    let commands = harness.state_mut().shell.incoming(sg_ui::Incoming::Text("https://example.org/clip.mp4".to_owned()));
    assert!(commands.iter().any(|c| matches!(c, Command::Import { .. })));
    assert_eq!(clips(&harness, 0).len(), 2);
    // While a text field has focus, the three keys belong to the text.
    harness.key_press(Key::F2);
    harness.run_steps(3);
    harness.key_press_modifiers(Modifiers::CTRL, Key::X);
    harness.run_steps(3);
    assert_eq!(clips(&harness, 0).len(), 2);
}

#[test]
fn the_less_obvious_keys_have_buttons_that_show_them() {
    let mut harness = editor();
    place(&mut harness, "first take");
    place(&mut harness, "second take");
    let markers = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        project.sequences[0].markers.iter().map(|m| m.frame).collect::<Vec<_>>()
    };
    let props = |harness: &Harness<'_, App>, start: i64| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = &project.sequences[0].tracks[0];
        track.props_of(track.clips.at(start).unwrap().id).map(|p| sg_core::ClipProps::clone(p))
    };
    // The key is on the button (here: as the map in force names it).
    assert_eq!(sg_ui::keymap::hint(sg_ui::Action::KeyframeSet), "F");
    // Above the timeline, as an icon named in its tooltip: the marker.
    playhead(&mut harness, 590);
    click(&mut harness, "Marker");
    assert_eq!(markers(&harness), [590]);
    // Under "Clip" in the top bar: whip-pan on the nearest cut, keyframes for the selected clip.
    click(&mut harness, "Clip");
    click(&mut harness, "Whip-pan");
    assert!(props(&harness, 600).is_some_and(|p| p.transition.is_some()));
    let first = on_timeline(&harness, 12.0, 0);
    pointer(&mut harness, first, egui::PointerButton::Primary);
    playhead(&mut harness, 100);
    click(&mut harness, "Clip");
    click(&mut harness, "Keyframe");
    assert_eq!(props(&harness, 0).unwrap().x.keys.iter().map(|k| k.frame).collect::<Vec<_>>(), [100]);
    click(&mut harness, "Clip");
    click(&mut harness, "Remove keyframe");
    assert!(props(&harness, 0).is_none_or(|p| p.x.keys.is_empty()));
    // The same in the clip's right-click menu.
    pointer(&mut harness, first, egui::PointerButton::Secondary);
    click(&mut harness, "Keyframe at the playhead");
    assert_eq!(props(&harness, 0).unwrap().x.keys.len(), 1);
}

#[test]
fn the_line_across_a_sound_clip_sets_its_volume() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let gain = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = &project.sequences[0].tracks[1];
        track.props_of(track.clips.iter().next().unwrap().id).map_or(0.0, |p| p.gain_db.at(0))
    };
    // The line of an untouched clip lies a quarter down the clip (0 dB):
    // pulled down a few points, the clip gets quieter; the clip itself stays.
    let rect = harness.get_by_label("Timeline").rect();
    let line = egui::pos2(rect.left() + 120.0 + 200.0, rect.top() + 22.0 + 40.0 + 12.75);
    drag(&mut harness, line, line + vec2(0.0, 9.0));
    let quieter = gain(&harness);
    assert!((-20.0..=-12.0).contains(&quieter), "{quieter}");
    assert_eq!(clips(&harness, 1), vec![(0, 480_000)]);
    // One undo for the whole pull.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(gain(&harness), 0.0);
    // The clip's menu has the volume too.
    let on_sound = on_timeline(&harness, 100.0, 1);
    pointer(&mut harness, on_sound, egui::PointerButton::Secondary);
    click(&mut harness, "Clip volume");
    click(&mut harness, "Quieter (-3 dB)");
    assert_eq!(gain(&harness), -3.0);
}

#[test]
fn a_mixer_preset_sets_the_strip_and_leaves_the_fader() {
    let mut harness = editor();
    click(&mut harness, "Mixer");
    assert!(harness.query_by_label("Preset: Flat").is_some());
    click(&mut harness, "Preset: Flat");
    click(&mut harness, "Voice, noisy room");
    let project = harness.state().shell.session().unwrap().project().clone();
    let mix = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().mix;
    assert_eq!((mix.low_cut_hz, mix.denoise, mix.comp_threshold_db, mix.gain_db), (100.0, 0.6, -20.0, 0.0));
    assert!(harness.query_by_label("Preset: Voice, noisy room").is_some());
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

#[test]
fn the_way_between_two_keyframes_can_be_made_smooth() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let on_clip = on_timeline(&harness, 100.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    assert!(harness.query_by_label("Smooth").is_none());
    // A keyframe at the start, then the picture dragged aside further on: a second one.
    click(&mut harness, "Keyframe (F)");
    playhead(&mut harness, 40);
    let middle = egui::pos2(810.0, 330.0);
    drag(&mut harness, middle, middle + vec2(120.0, 0.0));
    let x_at = |harness: &Harness<'_, App>, frame: i64| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = &project.sequences[0].tracks[0];
        track.props_of(track.clips.at(0).unwrap().id).unwrap().x.at(frame)
    };
    let end = x_at(&harness, 40);
    assert!(end > 0.05 && x_at(&harness, 0) == 0.0);
    playhead(&mut harness, 10);
    assert!((x_at(&harness, 10) - end / 4.0).abs() < 1e-5);
    // The clip's keyframes have a page of their own in the inspector.
    click(&mut harness, "Keyframes");
    click(&mut harness, "Smooth");
    assert!(x_at(&harness, 10) < end * 0.17, "{}", x_at(&harness, 10));
    assert!((x_at(&harness, 20) - end / 2.0).abs() < 1e-5);
    click(&mut harness, "Hold");
    assert_eq!((x_at(&harness, 39), x_at(&harness, 40)), (0.0, end));
    // The list names both keyframes by their time; a click on one moves the playhead there.
    click(&mut harness, "00:00:00:40");
    assert!(transport(&harness).contains(&sg_ui::Transport::Seek(40)));
    // Its values can be read and changed there; "x" removes it.
    let remove: Vec<_> = harness.get_all_by_label("x").collect();
    remove.last().unwrap().click();
    harness.run_steps(3);
    assert_eq!(x_at(&harness, 40), 0.0);
    assert!(harness.query_by_label("00:00:00:40").is_none());
}

#[test]
fn a_title_gets_a_font_from_the_project_folder() {
    let mut harness = editor();
    harness.key_press(Key::T);
    harness.run_steps(3);
    // The folder is asked for once a title is shown, and what it holds can be chosen.
    let root = harness.state().shell.session().unwrap().root.clone();
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::LoadFonts { .. })));
    harness.state_mut().shell.fonts_loaded(&root, vec!["poster-bold.ttf".to_owned()]);
    harness.run_steps(2);
    // (A combo box is known by what it shows, not by a label.)
    harness.get_by_value("Built-in").click();
    harness.run_steps(3);
    click(&mut harness, "poster-bold.ttf");
    let project = harness.state().shell.session().unwrap().project().clone();
    let track = &project.sequences[0].tracks[0];
    let title = track.props_of(track.clips.at(0).unwrap().id).and_then(|p| p.title.clone()).unwrap();
    assert_eq!(title.font, "poster-bold.ttf");
    // A dropped font file goes to the fonts folder, not to the media.
    let commands =
        harness.state_mut().shell.incoming(sg_ui::Incoming::Paths(vec!["/somewhere/Poster Bold.TTF".into()]));
    assert!(matches!(commands.as_slice(), [Command::ImportFont { .. }]));
}

#[test]
fn a_picture_of_another_shape_can_fill_the_frame() {
    let mut harness = editor();
    // A 4:3 take arrives.
    let root = harness.state().root.clone();
    let mut old_tv = take(3, "old tv");
    if let Some(video) = &mut old_tv.video {
        (video.width, video.height) = (480, 360);
    }
    harness.state_mut().shell.import_event(ImportEvent::Usable { job: 3, root, asset: Box::new(old_tv) });
    harness.run_steps(2);
    place(&mut harness, "old tv");
    let on_clip = on_timeline(&harness, 100.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    let scale = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = &project.sequences[0].tracks[0];
        track.props_of(track.clips.at(0).unwrap().id).map(|p| p.scale.at(0))
    };
    click(&mut harness, "Fill");
    // 16:9 over 4:3: a third larger.
    assert!((scale(&harness).unwrap() - 4.0 / 3.0).abs() < 1e-4);
    click(&mut harness, "Fit");
    assert_eq!(scale(&harness), None);
}

#[test]
fn the_mark_at_a_clips_top_corner_pulls_its_fade() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let on_clip = on_timeline(&harness, 100.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    let fades = |harness: &Harness<'_, App>| -> Vec<(i64, i64)> {
        let project = harness.state().shell.session().unwrap().project().clone();
        project.sequences[0]
            .tracks
            .iter()
            .map(|t| {
                t.clips.iter().next().and_then(|c| t.props_of(c.id).map(|p| (p.fade_in, p.fade_out))).unwrap_or((0, 0))
            })
            .collect()
    };
    // The mark sits just inside the clip's top left corner.
    let mark = on_timeline(&harness, 12.0, 0) - vec2(0.0, 14.0);
    drag(&mut harness, mark, mark + vec2(60.0, 0.0));
    let after = fades(&harness);
    // Picture and sound of the take fade in together, by the same time.
    assert!(after[0].0 > 0 && after[0] == after[1] && after[0].1 == 0, "{after:?}");
    // One pull, one undo step.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(fades(&harness), [(0, 0), (0, 0)]);
    // In the middle of the clip the same gesture moves it instead.
    drag(&mut harness, on_clip, on_clip + vec2(60.0, 0.0));
    assert_eq!(fades(&harness), [(0, 0), (0, 0)]);
}

#[test]
fn a_finished_export_offers_the_way_to_its_file() {
    let mut harness = editor();
    assert!(harness.query_by_label("Show in folder").is_none());
    harness.state_mut().shell.export_update(sg_ui::ExportUpdate::Done {
        path: "/library/flows/exports/flows-master-1.mp4".to_owned(),
        bytes: 1,
        seconds: 1.0,
        on_clipboard: false,
        note: String::new(),
    });
    harness.run_steps(2);
    click(&mut harness, "Show in folder");
    assert!(harness.state().commands.iter().any(
        |c| matches!(c, Command::Reveal { path } if path.to_str() == Some("/library/flows/exports/flows-master-1.mp4"))
    ));
}

#[test]
fn an_asset_is_made_global_and_taken_into_a_project_from_the_global_tab() {
    let mut harness = editor();
    // From an asset's menu: the application is asked to copy it to the global folder.
    right_click(&mut harness, "first take");
    click(&mut harness, "Make global");
    assert!(
        harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, Command::MakeGlobal { asset, .. } if asset.id == AssetId(1)))
    );
    // The folder as the application reads it afterwards: that copy and a meme from another project.
    let entry = |name: &str, bytes: u64| sg_project::global::GlobalEntry {
        path: std::path::PathBuf::from(format!("/somewhere/_global/video/{name}.mp4")),
        kind: AssetKind::Video,
        label: name.to_owned(),
        bytes,
    };
    let listed = vec![entry("first take", 1), entry("meme", 5_000_000)];
    harness.state_mut().shell.global_listed(Ok(listed), Some("\"first take\" is global now.".to_owned()));
    harness.run_steps(3);
    // The Global tab shows both; the project's own assets are not in it.
    click(&mut harness, "Global");
    assert!(harness.query_by_label("meme").is_some() && harness.query_by_label("second take").is_none());
    // Taking the meme imports its file, as a copy, like a dropped file; the tab goes back to the project.
    right_click(&mut harness, "meme");
    click(&mut harness, "Add to this project");
    let imported = harness.state().commands.iter().any(|c| {
        matches!(c, Command::Import { source: sg_project::ImportSource::Path(path), .. } if path.ends_with("video/meme.mp4"))
    });
    assert!(imported);
    assert!(harness.query_by_label("second take").is_some());
    // Removing one from Global is asked of the application too.
    click(&mut harness, "Global");
    right_click(&mut harness, "meme");
    click(&mut harness, "Remove from Global");
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::RemoveGlobal { .. })));
}

#[test]
fn a_box_or_ctrl_a_selects_clips_and_backspace_deletes_them() {
    let mut harness = editor();
    place(&mut harness, "first take");
    playhead(&mut harness, 600);
    place(&mut harness, "second take");
    assert_eq!((clips(&harness, 0), clips(&harness, 1).len()), (vec![(0, 600), (600, 600)], 2));
    // A box pulled open from the empty space under the tracks up over the
    // start of the first take: its picture and its sound are inside.
    let (from, to) = (on_timeline(&harness, 20.0, 3), on_timeline(&harness, 4.0, 0));
    let sent = transport(&harness).len();
    drag(&mut harness, from, to);
    // The playhead did not follow (it does on the ruler only).
    assert_eq!(transport(&harness).len(), sent);
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0), clips(&harness, 1).len()), (vec![(600, 600)], 1));
    // Undo, then everything at once.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    harness.key_press_modifiers(Modifiers::CTRL, Key::A);
    harness.run_steps(3);
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0), clips(&harness, 1)), (vec![], vec![]));
    // Pulling along the ruler still moves the playhead.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    let rect = harness.get_by_label("Timeline").rect();
    let ruler = egui::pos2(rect.left() + 130.0, rect.top() + 8.0);
    let sent = transport(&harness).len();
    drag(&mut harness, ruler, ruler + vec2(120.0, 0.0));
    assert!(transport(&harness).len() > sent);
}

#[test]
fn shift_g_groups_the_selected_clips_and_again_ungroups_them() {
    let mut harness = editor();
    place(&mut harness, "first take");
    playhead(&mut harness, 600);
    place(&mut harness, "second take");
    assert_eq!((clips(&harness, 0).len(), clips(&harness, 1).len()), (2, 2));
    let end = clips(&harness, 0)[1].0 + 600;
    // Where the two takes lie on screen: the timeline shows a little more than the sequence.
    let rect = harness.get_by_label("Timeline").rect();
    let x_of = |frame: i64| frame as f32 / (end as f32 * 1.15).max(1200.0) * (rect.width() - 120.0);
    let (first, corner, empty) =
        (on_timeline(&harness, x_of(100), 0), on_timeline(&harness, x_of(900), 4), on_timeline(&harness, x_of(300), 4));
    let click_at = |harness: &mut Harness<'_, App>, pos: egui::Pos2, _: bool| {
        pointer(harness, pos, egui::PointerButton::Primary);
    };
    // Both takes selected with a box, then Shift+G.
    drag(&mut harness, corner, first);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::G);
    harness.run_steps(3);
    // A click on one of them now takes all four clips: Backspace empties the timeline.
    click_at(&mut harness, empty, false);
    click_at(&mut harness, first, false);
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0), clips(&harness, 1)), (vec![], vec![]));
    harness.key_press(Key::Z);
    harness.run_steps(3);
    // The group, selected by a click, is dissolved by the same key: a click takes one take again.
    click_at(&mut harness, first, false);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::G);
    harness.run_steps(3);
    click_at(&mut harness, empty, false);
    click_at(&mut harness, first, false);
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!((clips(&harness, 0).len(), clips(&harness, 1).len()), (1, 1));
}

#[test]
fn a_sequence_gets_a_tempo_and_clips_snap_to_its_beats() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let tempo = |harness: &Harness<'_, App>| harness.state().shell.session().unwrap().project().sequences[0].tempo;
    assert_eq!(tempo(&harness), None);
    // In the inspector, under SEQUENCE: bars and beats at 120 a minute, four to the bar.
    click(&mut harness, "Bars and beats");
    assert_eq!(tempo(&harness), Some(sg_core::Tempo::DEFAULT));
    // A beat is 30 frames at 60 a second. A clip dragged along lands on one.
    let on_clip = on_timeline(&harness, 60.0, 0);
    drag(&mut harness, on_clip, on_clip + vec2(103.0, 0.0));
    let start = clips(&harness, 0)[0].0;
    assert!(start > 0 && start % 30 == 0, "{start}");
    // Its sound came along, on the same beat.
    assert_eq!(clips(&harness, 1)[0].0, start * 800);
    // "Off" brings the ruler in time back; the clip stays where it is.
    // (Nothing selected, so that the sequence's settings are in view.)
    let empty = on_timeline(&harness, 300.0, 4);
    pointer(&mut harness, empty, egui::PointerButton::Primary);
    click(&mut harness, "Off");
    assert_eq!((tempo(&harness), clips(&harness, 0)[0].0), (None, start));
    // Undo steps back through all of it.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(tempo(&harness), Some(sg_core::Tempo::DEFAULT));
}

#[test]
fn a_strip_is_set_up_for_the_voice_on_its_track_by_listening() {
    let mut harness = editor();
    place(&mut harness, "first take");
    click(&mut harness, "Mixer");
    let strip = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        (track.id, track.mix)
    };
    let (track, before) = strip(&harness);
    // "Voice", "Listen and set up": the application is asked to listen to what the track's clips play.
    click(&mut harness, "Voice");
    click(&mut harness, "Listen and set up");
    let asked = harness.state().commands.iter().find_map(|c| match c {
        Command::MeasureVoice { track: asked, style: None, parts, .. } if *asked == track => Some(parts.clone()),
        _ => None,
    });
    assert_eq!(asked, Some(vec![(AssetId(1), 0, 480_000)]));
    assert!(harness.query_by_label("Voice").is_some());
    // What it heard: a low, dull, noisy speaking voice. The strip follows; pan stays.
    let heard = sg_media::voice::VoiceStats {
        loudness_db: -26.0,
        loud_db: -14.0,
        peak_db: -9.0,
        noise_db: Some(-58.0),
        pitch_hz: Some(112.0),
        steadiness: 0.2,
        bands_db: [-30.0, 6.0, 1.0, 0.0, -12.0, -27.0],
        sibilance_db: Some(-5.0),
        key: Some((9, true)),
        seconds: 42.0,
    };
    let root = harness.state().root.clone();
    harness.state_mut().shell.voice_measured(&root, track, None, Ok(heard));
    harness.run_steps(3);
    let (_, after) = strip(&harness);
    assert_eq!(after.pan, before.pan);
    assert_eq!((after.low_cut_hz, after.comp_ratio, after.eq_mid_hz), (75.0, 4.0, 220.0));
    assert!(after.denoise > 0.3 && after.eq_mid_db < -1.0 && after.eq_high_db > 3.0, "{after:?}");
    assert!(after.comp_threshold_db < -14.0 && after.gain_db > 3.0, "{after:?}");
    // Its S sounds were loud, and the added air makes them sharper: the de-esser is well in.
    assert!(after.deess >= 0.6, "{after:?}");
    assert!(harness.query_by_label("De-esser").is_some());
    // A rap verse is not pitch-corrected, but the key that was heard (A minor) is set for when it is turned up.
    assert_eq!((after.tune, after.tune_key, after.tune_scale), (0.0, 9, 2));
    // It is a voice track now, levelled and a little saturated.
    assert!(after.voice && after.level > 0.3 && after.drive > 0.1, "{after:?}");
    assert!(harness.query_by_label("Tune").is_some());
    // One undo brings the strip back as it was.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(strip(&harness).1, before);
    // Nothing to hear is said, not guessed.
    harness.state_mut().shell.voice_measured(&root, track, None, Err("there is too little sound".to_owned()));
    harness.run_steps(2);
    assert_eq!(strip(&harness).1, before);
}

#[test]
fn setting_a_voice_up_makes_the_other_tracks_duck_under_it() {
    let mut harness = editor();
    place(&mut harness, "first take");
    // The second take over the first, in time: it gets a pair of tracks of its own.
    right_click(&mut harness, "second take");
    harness.run_steps(1);
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    let dropped = harness.state_mut().shell.dev(sg_ui::DevStep::Drop(0, None, None));
    assert!(dropped.is_empty());
    harness.run_steps(2);
    let audio = |harness: &Harness<'_, App>| -> Vec<(sg_core::TrackId, sg_core::TrackSettings)> {
        let project = harness.state().shell.session().unwrap().project().clone();
        project.sequences[0].tracks.iter().filter(|t| t.kind == TrackKind::Audio).map(|t| (t.id, t.mix)).collect()
    };
    let before = audio(&harness);
    assert_eq!(before.len(), 2);
    let heard = sg_media::voice::VoiceStats {
        loudness_db: -24.0,
        loud_db: -8.0,
        peak_db: -3.0,
        noise_db: None,
        pitch_hz: Some(180.0),
        steadiness: 0.2,
        bands_db: [-30.0, -2.0, 0.0, 0.0, -8.0, -14.0],
        key: None,
        sibilance_db: Some(-6.0),
        seconds: 60.0,
    };
    let root = harness.state().root.clone();
    harness.state_mut().shell.voice_measured(&root, before[0].0, None, Ok(heard));
    harness.run_steps(3);
    let after = audio(&harness);
    // The voice track is one; the other, which has sound on it, goes down 3 dB under it.
    assert!(after[0].1.voice && after[0].1.duck_db == 0.0);
    assert!(!after[1].1.voice && after[1].1.duck_db == 3.0, "{:?}", after[1].1);
    // The mixer shows each strip what belongs to it.
    click(&mut harness, "Mixer");
    assert!(harness.query_by_label("Duck under voice").is_some() && harness.query_by_label("De-esser").is_some());
    // All of it is one step.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(audio(&harness), before);
}

#[test]
fn a_title_is_turned_sized_and_moved_with_its_handles_in_the_viewer() {
    let mut harness = editor();
    harness.key_press(Key::T);
    harness.run_steps(3);
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    let props = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| !t.clips.is_empty()).unwrap().clone();
        let p = track.props_of(track.clips.iter().next().unwrap().id).unwrap().clone();
        (p.scale.at(0), p.rotation.at(0), p.x.at(0), p.y.at(0))
    };
    assert_eq!(props(&harness), (1.0, 0.0, 0.0, 0.0));
    // The title has an outline of its own size, with the same handles as a picture.
    let corners: Vec<egui::Rect> = harness.get_all_by_label("Size handle").map(|n| n.rect()).collect();
    assert_eq!(corners.len(), 4);
    let middle = (corners.iter().fold(egui::Vec2::ZERO, |sum, r| sum + r.center().to_vec2()) / 4.0).to_pos2();
    // Its box is as wide as the word, not as the frame.
    let width = corners.iter().map(|r| r.center().x).fold(f32::MIN, f32::max)
        - corners.iter().map(|r| r.center().x).fold(f32::MAX, f32::min);
    assert!((60.0..500.0).contains(&width), "{width}");
    // The knob pulled to the right turns it clockwise.
    let knob = harness.get_by_label("Turn handle").rect().center();
    drag(&mut harness, knob, knob + vec2(40.0, 0.0));
    let (_, rotation, ..) = props(&harness);
    assert!(rotation > 10.0, "{rotation}");
    // A corner pulled away from the middle makes it larger.
    let corner = harness.get_all_by_label("Size handle").next().unwrap().rect().center();
    drag(&mut harness, corner, corner + (corner - middle) * 0.5);
    assert!(props(&harness).0 > 1.2, "{:?}", props(&harness));
    // Dragged by its middle it moves.
    drag(&mut harness, middle, middle + vec2(90.0, 40.0));
    let (_, _, x, y) = props(&harness);
    assert!(x > 0.03 && y > 0.02, "{x} {y}");
}

#[test]
fn the_plugins_popover_lists_plugins_and_walks_through_the_native_instruments_set_up() {
    use sg_project::plugins::{Format, NiState, Plugin};
    use sg_ui::{NiStep, PluginRequest, PluginUpdate};
    let mut harness = editor();
    let asked = |harness: &Harness<'_, App>| -> Vec<PluginRequest> {
        harness
            .state()
            .commands
            .iter()
            .filter_map(|c| if let Command::Plugins(r) = c { Some(*r) } else { None })
            .collect()
    };
    // The button in the top bar opens it and has the folders searched.
    click(&mut harness, "Plugins");
    assert_eq!(asked(&harness), [PluginRequest::Scan]);
    let state = |access: bool, installed: &[&str], bridged: &[&str]| NiState {
        tools: true,
        prefix: "/disk/shotgun/_wine-ni".into(),
        prefix_ready: access,
        access,
        installed: installed.iter().map(|s| (*s).to_owned()).collect(),
        bridged: bridged.iter().map(|s| (*s).to_owned()).collect(),
    };
    let plugin = |name: &str, bridged: bool| Plugin {
        name: name.to_owned(),
        path: format!("/home/x/.vst3/{name}.vst3").into(),
        format: Format::Vst3,
        bridged,
    };
    let found = |harness: &mut Harness<'_, App>, plugins: Vec<Plugin>, ni: NiState| {
        harness.state_mut().shell.plugins_update(PluginUpdate::Found { plugins, ni });
        harness.run_steps(3);
    };
    // Nothing set up yet: one native plugin is listed; step one can be started, the others not yet.
    found(&mut harness, vec![plugin("Surge XT", false)], state(false, &[], &[]));
    assert!(harness.query_by_label("Surge XT").is_some());
    assert!(harness.query_by_label("NATIVE INSTRUMENTS (KONTAKT)").is_some());
    click(&mut harness, "Open Native Access");
    assert_eq!(asked(&harness).len(), 1, "a step whose turn has not come does nothing");
    click(&mut harness, "Install Native Instruments");
    assert_eq!(asked(&harness).last(), Some(&PluginRequest::Run(NiStep::Setup)));
    // While it runs its lines are shown and it can be stopped.
    harness.state_mut().shell.plugins_update(PluginUpdate::Line("Creating the Wine prefix...".to_owned()));
    harness.run_steps(3);
    assert!(harness.query_by_label("Creating the Wine prefix...").is_some());
    click(&mut harness, "Stop");
    assert_eq!(asked(&harness).last(), Some(&PluginRequest::Stop));
    // Done: step two opens Native Access; after it, what was installed is named and step three bridges it.
    harness.state_mut().shell.plugins_update(PluginUpdate::Finished(Ok(())));
    found(&mut harness, vec![plugin("Surge XT", false)], state(true, &[], &[]));
    click(&mut harness, "Open Native Access");
    assert_eq!(asked(&harness).last(), Some(&PluginRequest::Run(NiStep::Launch)));
    harness.state_mut().shell.plugins_update(PluginUpdate::Finished(Ok(())));
    found(&mut harness, vec![plugin("Surge XT", false)], state(true, &["Kontakt 8"], &[]));
    click(&mut harness, "Find the installed plugins");
    assert_eq!(asked(&harness).last(), Some(&PluginRequest::Run(NiStep::Bridge)));
    harness.state_mut().shell.plugins_update(PluginUpdate::Finished(Ok(())));
    found(
        &mut harness,
        vec![plugin("Kontakt 8", true), plugin("Surge XT", false)],
        state(true, &["Kontakt 8"], &["Kontakt 8"]),
    );
    assert!(harness.query_by_label("Kontakt 8").is_some());
    assert!(harness.query_by_label("VST3, Windows through the bridge").is_some());
    // A step that fails says why; Esc closes the popover.
    harness.state_mut().shell.plugins_update(PluginUpdate::Finished(Err("ni failed: no network".to_owned())));
    harness.run_steps(3);
    assert!(harness.query_by_label("ni failed: no network").is_some());
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label("PLUGINS").is_none());
}

#[test]
fn plugins_are_put_on_a_track_from_its_mixer_strip_in_an_order() {
    use sg_project::plugins::{Format, NiState, Plugin};
    let mut harness = editor();
    click(&mut harness, "Mixer");
    let plugins_of = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        (track.id, track.plugins.iter().map(|p| p.name.clone()).collect::<Vec<_>>())
    };
    // The strip's menu has the folders searched the first time it is opened.
    click(&mut harness, "Add plugin");
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::Plugins(sg_ui::PluginRequest::Scan))));
    let plugin = |name: &str| Plugin {
        name: name.to_owned(),
        path: format!("/lib/vst3/{name}.vst3").into(),
        format: Format::Vst3,
        bridged: false,
    };
    let ni = NiState {
        tools: true,
        prefix: "/x".into(),
        prefix_ready: false,
        access: false,
        installed: vec![],
        bridged: vec![],
    };
    let found = vec![plugin("Surge XT Effects"), plugin("Wide")];
    harness.state_mut().shell.plugins_update(sg_ui::PluginUpdate::Found { plugins: found, ni });
    harness.run_steps(3);
    click(&mut harness, "Surge XT Effects");
    assert_eq!(plugins_of(&harness).1, ["Surge XT Effects"]);
    // A second one comes after the first: the sound runs through them in this order.
    click(&mut harness, "Add plugin");
    click(&mut harness, "Wide");
    let (track, names) = plugins_of(&harness);
    assert_eq!(names, ["Surge XT Effects", "Wide"]);
    // "Up" moves one earlier.
    click(&mut harness, "Move Wide up");
    assert_eq!(plugins_of(&harness).1, ["Wide", "Surge XT Effects"]);
    // Its name shows its window: asked for by the track and by which of its plugins it is.
    let project = harness.state().shell.session().unwrap().project().clone();
    let wide = project.sequences[0].track(track).unwrap().plugins[0].clone();
    assert!(wide.id != 0);
    click(&mut harness, "Wide");
    assert!(
        harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, Command::PluginEditor { track: t, key } if *t == track && *key == wide.id))
    );
    // "Off" takes one off again; undo brings it back where it was.
    click(&mut harness, "Take Wide off");
    assert_eq!(plugins_of(&harness).1, ["Surge XT Effects"]);
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(plugins_of(&harness).1, ["Wide", "Surge XT Effects"]);
}

#[test]
fn a_note_clip_is_made_on_an_audio_track_and_its_notes_drawn_in_the_piano_roll() {
    let mut harness = editor();
    let notes = |harness: &Harness<'_, App>| -> Option<Vec<sg_core::Note>> {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio)?.clone();
        let clip = track.clips.iter().next()?;
        assert_eq!(clip.asset, AssetId(0));
        track.notes.get(&clip.id).map(|n| Vec::clone(n))
    };
    // Right click on the empty audio track (row 1): a note clip, two bars long, and its piano roll opens.
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    assert_eq!(notes(&harness), Some(vec![]));
    assert_eq!(clips(&harness, 1).len(), 1);
    // Two bars at 120 a minute are four seconds of sound.
    assert_eq!(clips(&harness, 1)[0].1, 4 * 48_000);
    let roll = harness.get_by_label("Piano roll").rect();
    // A click puts a note there: a sixteenth long, on the grid.
    let spot = egui::pos2(roll.left() + 46.0 + 100.0, roll.top() + 90.0);
    pointer(&mut harness, spot, egui::PointerButton::Primary);
    let one = notes(&harness).unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!((one[0].len, one[0].start % 240, one[0].velocity), (240, 0, 100));
    // A drag on empty space draws one as long as the drag; it is higher up, so higher in pitch.
    let from = egui::pos2(roll.left() + 46.0 + 300.0, roll.top() + 40.0);
    drag(&mut harness, from, from + vec2(160.0, 0.0));
    let two = notes(&harness).unwrap();
    assert_eq!(two.len(), 2);
    let drawn = two.iter().find(|n| n.pitch != one[0].pitch).unwrap();
    assert!(drawn.len > 240 && drawn.pitch > one[0].pitch, "{drawn:?}");
    // The delete key takes the note that was just drawn, not the clip.
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!(notes(&harness).unwrap(), one);
    assert_eq!(clips(&harness, 1).len(), 1);
    // A right click on a note removes it; undo brings it back.
    pointer(&mut harness, spot, egui::PointerButton::Secondary);
    assert_eq!(notes(&harness), Some(vec![]));
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(notes(&harness).unwrap(), one);
    // Esc closes the piano roll; a double click on the clip would open it again.
    assert_eq!(harness.state().shell.error_shown(), None);
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(harness.query_by_label("Piano roll").is_none());
}

#[test]
fn sound_is_recorded_while_playing_and_the_take_laid_where_it_was_played() {
    let mut harness = editor();
    place(&mut harness, "first take");
    // Shift+R (or the button): the application is asked to play and record.
    harness.key_press_modifiers(Modifiers::SHIFT, Key::R);
    harness.run_steps(3);
    let root = harness.state().root.clone();
    assert!(
        harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, Command::Record { root: r, source: None, lead_in: false } if *r == root))
    );
    // While it runs the button ends it.
    click(&mut harness, "Stop recording");
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::StopRecording)));
    // The take arrives: its file is imported like any other...
    let file = root.join(".shotgun/takes/take-5.wav");
    let imports = harness.state_mut().shell.take_recorded(file.clone(), 12_345, 96_000, 0);
    assert!(imports.iter().any(|c| {
        matches!(c, Command::Import { source: sg_project::ImportSource::Path(path), .. } if *path == file)
    }));
    harness.run_steps(2);
    assert!(harness.query_by_label("Record sound").is_some());
    // ...and once it is an asset it lies on a free audio track, to the sample where it was played.
    let mut asset = take(9, "take-5");
    asset.kind = AssetKind::Audio;
    asset.video = None;
    asset.audio = Some(AudioInfo { codec: "pcm".into(), sample_rate: 48_000, channels: 2, samples: 96_000 });
    harness.state_mut().shell.import_event(ImportEvent::Usable { job: 9, root: root.clone(), asset: Box::new(asset) });
    harness.run_steps(3);
    let project = harness.state().shell.session().unwrap().project().clone();
    let placed: Vec<(String, i64, i64)> = project.sequences[0]
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().filter(|c| c.asset == AssetId(9)).map(|c| (t.name.clone(), c.start, c.len)))
        .collect();
    // (A1 holds the first take's sound there: the take gets a track of its own.)
    assert_eq!(placed, [("A2".to_owned(), 12_345, 96_000)]);
    assert_eq!(harness.state_mut().shell.take_spent_takes(), [file]);
    // One undo takes the clip and its track away again.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(tracks(&harness).len(), 2);
}

#[test]
fn tempo_and_grid_are_set_above_the_timeline_and_clips_snap_to_the_chosen_grid() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let tempo = |harness: &Harness<'_, App>| harness.state().shell.session().unwrap().project().sequences[0].tempo;
    // "Set tempo" above the timeline does what "Bars and beats" in the inspector does.
    click(&mut harness, "Set tempo");
    assert_eq!(tempo(&harness), Some(sg_core::Tempo::DEFAULT));
    // The grid follows the zoom until one is chosen: whole bars here (120 frames at 60 a second).
    click(&mut harness, "Grid: auto");
    click(&mut harness, "bar");
    assert!(harness.query_by_label("Grid: bar").is_some());
    let on_clip = on_timeline(&harness, 60.0, 0);
    // (Edges are pulled to a line from a few points away: this lets go near the second bar.)
    drag(&mut harness, on_clip, on_clip + vec2(143.0, 0.0));
    let start = clips(&harness, 0)[0].0;
    assert!(start > 0 && start % 120 == 0, "{start}");
}

#[test]
fn the_input_to_record_from_is_chosen_in_the_settings() {
    let mut harness = editor();
    click(&mut harness, "Settings");
    // Opening the menu asks which inputs there are now.
    click(&mut harness, "Input: default");
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::ListInputs)));
    harness.state_mut().shell.inputs_listed(vec![("alsa_input.usb-scarlett".to_owned(), "Scarlett 2i2".to_owned())]);
    harness.run_steps(3);
    click(&mut harness, "Scarlett 2i2");
    // The choice is remembered, shown on the button, and the next take comes from there.
    assert!(
        harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, Command::SaveInput(Some(name)) if name == "alsa_input.usb-scarlett"))
    );
    assert!(harness.query_by_label("Input: Scarlett 2i2").is_some());
    harness.key_press_modifiers(Modifiers::SHIFT, Key::R);
    harness.run_steps(3);
    assert!(
        harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, Command::Record { source: Some(name), .. } if name == "alsa_input.usb-scarlett"))
    );
}

#[test]
fn the_metronome_needs_a_tempo_and_is_switched_with_its_button() {
    let mut harness = editor();
    // Without a tempo there are no beats to click on: the program says what to do.
    harness.key_press_modifiers(Modifiers::SHIFT, Key::K);
    harness.run_steps(3);
    assert!(harness.state().shell.error_shown().is_some_and(|e| e.contains("tempo")));
    assert!(!harness.state().commands.iter().any(|c| matches!(c, Command::Metronome(_))));
    click(&mut harness, "Bars and beats");
    click(&mut harness, "Metronome");
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::Metronome(true))));
    // The button says that it is on; again turns it off.
    click(&mut harness, "Metronome on");
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::Metronome(false))));
    assert!(harness.query_by_label("Metronome").is_some());
}

#[test]
fn a_song_is_exported_as_sound_only_with_its_tracks_and_a_loudness() {
    let mut harness = editor();
    place(&mut harness, "first take");
    harness.key_press_modifiers(Modifiers::CTRL, Key::E);
    harness.run_steps(3);
    click(&mut harness, "Sound only");
    click(&mut harness, "FLAC");
    click(&mut harness, "every track as a file of its own too");
    click(&mut harness, "-14 LUFS");
    // (The button in the popover: the top bar has one of the same name.)
    let buttons: Vec<_> = harness.get_all_by_label("Export").collect();
    buttons.last().unwrap().click();
    harness.run_steps(3);
    let asked = harness.state().commands.iter().find_map(|c| match c {
        Command::Export { request, .. } => Some(request.clone()),
        _ => None,
    });
    let asked = asked.expect("an export was asked for");
    assert_eq!(
        (asked.preset, asked.sound_format, asked.stems, asked.loudness),
        (sg_ui::ExportPreset::Sound, 1, true, Some(-14.0))
    );
    // What was measured is shown with the file.
    harness.state_mut().shell.export_update(sg_ui::ExportUpdate::Done {
        path: "/library/flows/exports/flows-sound-1.flac".to_owned(),
        bytes: 3_000_000,
        seconds: 2.0,
        on_clipboard: false,
        note: "Loudness -14.0 LUFS, highest peak -1.2 dB".to_owned(),
    });
    harness.run_steps(2);
    assert!(harness.query_by_label_contains("Loudness -14.0 LUFS").is_some());
}

#[test]
fn the_instrument_is_played_with_the_keyboard_and_what_is_played_is_recorded() {
    let mut harness = editor();
    let notes = |harness: &Harness<'_, App>| -> Vec<sg_core::Note> {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        let clip = track.clips.iter().next().unwrap();
        track.notes.get(&clip.id).map(|n| Vec::clone(n)).unwrap_or_default()
    };
    let played = |harness: &Harness<'_, App>| -> Vec<(bool, u8)> {
        let all = harness.state().commands.iter();
        all.filter_map(|c| if let Command::Note { on, pitch, .. } = c { Some((*on, *pitch)) } else { None }).collect()
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    // With the piano roll open, its track's instrument is the one played by hand.
    let track = harness.state().shell.session().unwrap().project().sequences[0].tracks[1].id;
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::Hand(Some(t)) if *t == track)));
    // Without "Keyboard", the letter keys stay shortcuts and play nothing.
    harness.key_press(Key::A);
    harness.run_steps(2);
    assert_eq!(played(&harness), []);
    // With it: the home row is the white keys from middle C, the row above the black ones.
    click(&mut harness, "Keyboard");
    harness.key_down(Key::A);
    harness.run_steps(2);
    harness.key_down(Key::W);
    harness.run_steps(2);
    harness.key_up(Key::A);
    harness.key_up(Key::W);
    harness.run_steps(2);
    assert_eq!(played(&harness), [(true, 60), (true, 61), (false, 60), (false, 61)]);
    // X moves the keyboard up an octave; S would split a clip otherwise, here it is a D.
    harness.key_press(Key::X);
    harness.key_press(Key::S);
    harness.run_steps(2);
    assert_eq!(played(&harness)[4..], [(true, 74), (false, 74)]);
    assert_eq!(clips(&harness, 1).len(), 1);
    // A key at the left of the roll plays its pitch while the button is down.
    let roll = harness.get_by_label("Piano roll").rect();
    let key = egui::pos2(roll.left() + 10.0, roll.top() + 90.0);
    harness.hover_at(key);
    harness.event(egui::Event::PointerButton {
        pos: key,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(2);
    assert_eq!(played(&harness).len(), 7);
    harness.event(egui::Event::PointerButton {
        pos: key,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run_steps(2);
    let by_mouse = played(&harness)[6..].to_vec();
    assert_eq!(by_mouse, [(true, by_mouse[0].1), (false, by_mouse[0].1)]);
    // Nothing of this was written down: "Record" is off, and nothing plays.
    assert_eq!(notes(&harness), []);
    // Recording: the application says where each key was heard while the timeline played.
    click(&mut harness, "Record");
    harness.state_mut().view.playing = true;
    harness.run_steps(2);
    let start = clips(&harness, 1)[0].0;
    // (At 120 a minute a tick is 25 samples.) Half a beat in, a little late, held for a quarter of a beat.
    harness.state_mut().shell.note_heard(true, 62, 100, start + 12_000 + 100);
    harness.state_mut().shell.note_heard(false, 62, 0, start + 18_000 + 100);
    // One played before the clip begins is not part of it.
    harness.state_mut().shell.note_heard(true, 64, 100, start - 5_000);
    harness.state_mut().shell.note_heard(false, 64, 0, start - 1_000);
    harness.run_steps(2);
    assert_eq!(notes(&harness), [sg_core::Note { start: 484, len: 240, pitch: 62, velocity: 100 }]);
    // "Quantise" puts it on the grid (sixteenths).
    harness.state_mut().view.playing = false;
    harness.run_steps(2);
    click(&mut harness, "Quantise");
    assert_eq!(notes(&harness), [sg_core::Note { start: 480, len: 240, pitch: 62, velocity: 100 }]);
    // Undo: the quantising, then the whole pass.
    click(&mut harness, "Keyboard");
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(notes(&harness)[0].start, 484);
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(notes(&harness), []);
    // Closing the roll gives the instrument back to the timeline alone.
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(matches!(
        harness.state().commands.iter().rfind(|c| matches!(c, Command::Hand(_))),
        Some(Command::Hand(None))
    ));
    assert_eq!(harness.state().shell.error_shown(), None);
}

#[test]
fn the_input_level_is_shown_and_the_input_heard_only_when_asked() {
    let mut harness = editor();
    let monitor = |harness: &Harness<'_, App>| {
        let all = harness.state().commands.iter();
        all.filter_map(|c| if let Command::Monitor(m) = c { Some(m.clone()) } else { None }).next_back()
    };
    assert_eq!(monitor(&harness), None);
    // In the settings, "Level" opens the input: measured, not heard.
    click(&mut harness, "Settings");
    click(&mut harness, "Level");
    assert_eq!(monitor(&harness), Some(Some((None, false))));
    harness.state_mut().view.input_level = 0.5;
    harness.run_steps(2);
    assert!(harness.query_by_label("-6 dB").is_some());
    // "Hear" is a button of its own, and another input is followed.
    click(&mut harness, "Hear");
    assert_eq!(monitor(&harness), Some(Some((None, true))));
    click(&mut harness, "Input: default");
    harness.state_mut().shell.inputs_listed(vec![("alsa_input.usb".to_owned(), "Interface".to_owned())]);
    harness.run_steps(3);
    click(&mut harness, "Interface");
    assert_eq!(monitor(&harness), Some(Some((Some("alsa_input.usb".to_owned()), true))));
    // Off again: the input is closed.
    click(&mut harness, "Level");
    assert_eq!(monitor(&harness), Some(None));
    assert!(harness.query_by_label("Hear").is_none());
}

#[test]
fn the_room_and_the_echo_are_set_on_the_master_strip() {
    let mut harness = editor();
    let space = |harness: &Harness<'_, App>| harness.state().shell.session().unwrap().project().sequences[0].space;
    assert_eq!(space(&harness), sg_core::Space::default());
    harness.key_press(Key::M);
    harness.run_steps(3);
    // The echo's time is chosen as a note value of the tempo.
    harness.get_by_value("1/4").click();
    harness.run_steps(3);
    click(&mut harness, "1/8.");
    assert_eq!(space(&harness).echo_note, sg_core::EchoNote::DottedEighth);
    // One undo step back.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(space(&harness), sg_core::Space::default());
}

#[test]
fn a_take_goes_onto_the_track_marked_for_recording_over_what_is_there() {
    let mut harness = editor();
    place(&mut harness, "first take");
    // A1 holds ten seconds of sound. "R" in its header: recordings go there.
    click(&mut harness, "R");
    playhead(&mut harness, 120);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::R);
    harness.run_steps(3);
    // (Onto a track that has sound already: the application is asked for a way in before the playhead.)
    assert!(harness.state().commands.iter().any(|c| matches!(c, Command::Record { lead_in: true, .. })));
    // The take began a second before the playhead (the way in) and is three seconds long.
    let root = harness.state().root.clone();
    let file = root.join(".shotgun/takes/take-7.wav");
    harness.state_mut().shell.take_recorded(file, 96_000 - 48_000, 144_000, 0);
    let mut asset = take(9, "take-7");
    asset.kind = AssetKind::Audio;
    asset.video = None;
    asset.audio = Some(AudioInfo { codec: "pcm".into(), sample_rate: 48_000, channels: 2, samples: 144_000 });
    harness.state_mut().shell.import_event(ImportEvent::Usable { job: 9, root, asset: Box::new(asset) });
    harness.run_steps(3);
    let on_a1 = |harness: &Harness<'_, App>| -> Vec<(u128, i64, i64, i64)> {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        track.clips.iter().map(|c| (c.asset.0, c.start, c.len, c.src_in)).collect()
    };
    // It lies on A1 from the playhead on (what was counted in stays in front of the clip's
    // start, in the file), and what was there makes room: before it and after it.
    assert_eq!(on_a1(&harness), [(1, 0, 96_000, 0), (9, 96_000, 96_000, 48_000), (1, 192_000, 288_000, 192_000)]);
    assert_eq!(tracks(&harness).len(), 2);
    // One undo: the track is as it was.
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(on_a1(&harness), [(1, 0, 480_000, 0)]);
}

#[test]
fn several_notes_are_selected_moved_together_copied_and_pasted_after_themselves() {
    let mut harness = editor();
    let notes = |harness: &Harness<'_, App>| -> Vec<sg_core::Note> {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        let clip = track.clips.iter().next().unwrap();
        track.notes.get(&clip.id).map(|n| Vec::clone(n)).unwrap_or_default()
    };
    let ctrl_click = |harness: &mut Harness<'_, App>, pos: egui::Pos2| {
        // (Held over the frames of the click, as a hand holds it.)
        harness.event(egui::Event::ModifiersChanged(Modifiers::CTRL));
        harness.hover_at(pos);
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::CTRL,
            });
            harness.run_steps(2);
        }
        harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
        harness.run_steps(2);
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    let roll = harness.get_by_label("Piano roll").rect();
    // Two notes, one after the other and a third apart in pitch.
    let (first, second) = (
        egui::pos2(roll.left() + 46.0 + 130.0, roll.top() + 90.0),
        egui::pos2(roll.left() + 46.0 + 190.0, roll.top() + 54.0),
    );
    pointer(&mut harness, first, egui::PointerButton::Primary);
    pointer(&mut harness, second, egui::PointerButton::Primary);
    let two = notes(&harness);
    assert_eq!(two.len(), 2);
    assert!(two[0].start < two[1].start, "{two:?}");
    // Ctrl+click adds the first to the selection (the second is selected from being placed):
    // dragging one now moves both, by the same way along and up.
    ctrl_click(&mut harness, first + vec2(2.0, 0.0));
    drag(&mut harness, second + vec2(2.0, 0.0), second + vec2(2.0, 0.0) + vec2(120.0, -24.0));
    let moved = notes(&harness);
    let (along, up) = (moved[0].start - two[0].start, moved[0].pitch as i32 - two[0].pitch as i32);
    assert!(along > 0 && up == 2, "{two:?} {moved:?}");
    assert_eq!((moved[1].start - two[1].start, moved[1].pitch as i32 - two[1].pitch as i32), (along, up));
    // Ctrl+A, Ctrl+C, Ctrl+V: the same two again, after themselves by the whole beats they span.
    harness.key_press_modifiers(Modifiers::CTRL, Key::A);
    harness.key_press_modifiers(Modifiers::CTRL, Key::C);
    harness.key_press_modifiers(Modifiers::CTRL, Key::V);
    harness.run_steps(3);
    let four = notes(&harness);
    assert_eq!(four.len(), 4);
    let by = four[2].start - four[0].start;
    assert!(by > 0 && by % 960 == 0, "{four:?}");
    assert_eq!((four[3].start - four[1].start, four[2].pitch, four[3].pitch), (by, four[0].pitch, four[1].pitch));
    // The clipboard of the desktop was not asked: the notes are the roll's own.
    assert!(!harness.state().commands.iter().any(|c| matches!(c, Command::ReadClipboard)));
    // The pasted ones are selected: again, and they repeat once more; the delete key takes those away.
    harness.key_press_modifiers(Modifiers::CTRL, Key::V);
    harness.run_steps(3);
    assert_eq!(notes(&harness).len(), 6);
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!(notes(&harness), four);
    assert_eq!(harness.state().shell.error_shown(), None);
}

#[test]
fn a_box_drawn_with_ctrl_selects_the_notes_it_touches() {
    let mut harness = editor();
    let notes = |harness: &Harness<'_, App>| -> usize {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        let clip = track.clips.iter().next().unwrap();
        track.notes.get(&clip.id).map_or(0, |n| n.len())
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    let roll = harness.get_by_label("Piano roll").rect();
    let at = |x: f32, y: f32| egui::pos2(roll.left() + 46.0 + x, roll.top() + y);
    for spot in [at(130.0, 90.0), at(190.0, 54.0), at(400.0, 150.0)] {
        pointer(&mut harness, spot, egui::PointerButton::Primary);
    }
    assert_eq!(notes(&harness), 3);
    // A box over the first two (the third lies outside it), then the delete key.
    harness.event(egui::Event::ModifiersChanged(Modifiers::CTRL));
    drag(&mut harness, at(100.0, 40.0), at(260.0, 110.0));
    harness.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    harness.run_steps(2);
    assert_eq!(notes(&harness), 3);
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!(notes(&harness), 1);
}

#[test]
fn a_strip_set_up_by_listening_listens_again_when_another_take_is_on_its_track() {
    let mut harness = editor();
    place(&mut harness, "first take");
    click(&mut harness, "Mixer");
    let strip = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        (track.id, track.mix)
    };
    let asked = |harness: &Harness<'_, App>| -> Vec<Vec<AssetId>> {
        let all = harness.state().commands.iter();
        all.filter_map(|c| match c {
            Command::MeasureVoice { parts, .. } => Some(parts.iter().map(|p| p.0).collect()),
            _ => None,
        })
        .collect()
    };
    let heard = sg_media::voice::VoiceStats {
        loudness_db: -26.0,
        loud_db: -14.0,
        peak_db: -9.0,
        noise_db: Some(-58.0),
        pitch_hz: Some(112.0),
        steadiness: 0.2,
        bands_db: [-30.0, 6.0, 1.0, 0.0, -12.0, -27.0],
        sibilance_db: Some(-5.0),
        key: Some((9, true)),
        seconds: 42.0,
    };
    let (track, _) = strip(&harness);
    let root = harness.state().root.clone();
    click(&mut harness, "Voice");
    click(&mut harness, "Listen and set up");
    harness.state_mut().shell.voice_measured(&root, track, None, Ok(heard));
    harness.run_steps(3);
    assert_eq!(asked(&harness), [vec![AssetId(1)]]);
    // Cutting the take in two leaves the same recording on the track: nothing is listened to again.
    playhead(&mut harness, 120);
    let on_clip = on_timeline(&harness, 30.0, 1);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    harness.key_press(Key::S);
    harness.run_steps(3);
    assert_eq!(asked(&harness).len(), 1);
    // Another take recorded onto the track: the strip, untouched since, listens again by itself.
    click(&mut harness, "R");
    playhead(&mut harness, 300);
    harness.key_press_modifiers(Modifiers::SHIFT, Key::R);
    harness.run_steps(3);
    harness.state_mut().shell.take_recorded(root.join(".shotgun/takes/take-3.wav"), 240_000, 96_000, 0);
    let mut asset = take(9, "take-3");
    asset.kind = AssetKind::Audio;
    asset.video = None;
    asset.audio = Some(AudioInfo { codec: "pcm".into(), sample_rate: 48_000, channels: 2, samples: 96_000 });
    harness.state_mut().shell.import_event(ImportEvent::Usable { job: 9, root: root.clone(), asset: Box::new(asset) });
    harness.run_steps(3);
    let again = asked(&harness);
    assert_eq!(again.len(), 2);
    assert!(again[1].contains(&AssetId(9)) && again[1].contains(&AssetId(1)), "{again:?}");
    harness.state_mut().shell.voice_measured(&root, track, None, Ok(heard));
    harness.run_steps(3);
    // With a setting changed by hand it does not: it offers to, on the strip.
    let (_, mix) = strip(&harness);
    harness.state_mut().shell.dev(sg_ui::DevStep::Mix("0".into(), "threshold".into(), mix.comp_threshold_db - 3.0));
    harness.run_steps(2);
    assert!(harness.query_by_label("Listen again").is_none());
    // (The new take is selected from being placed: the delete key takes it off the track again.)
    harness.key_press(Key::Backspace);
    harness.run_steps(3);
    assert_eq!(asked(&harness).len(), 2);
    click(&mut harness, "Listen again");
    assert_eq!(asked(&harness).len(), 3);
}

#[test]
fn the_notes_of_a_track_are_given_a_recording_to_play() {
    let mut harness = editor();
    let sampler = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().sampler
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    // A track with notes says on its strip what plays them: the synthesiser until told otherwise.
    click(&mut harness, "Mixer");
    click(&mut harness, "Instrument: Synthesiser");
    // The recordings of the project are offered (the one in the menu comes after the one in the asset panel).
    harness.get_all_by_label("second take").last().unwrap().click();
    harness.run_steps(3);
    // Ten seconds long: held as long as the key, at middle C as recorded.
    assert_eq!(sampler(&harness), Some(sg_core::Sampler { asset: AssetId(2), root: 60, one_shot: false }));
    assert!(harness.query_by_label("Instrument: second take").is_some());
    click(&mut harness, "To its end");
    assert_eq!(sampler(&harness).map(|s| s.one_shot), Some(true));
    // Undo, twice: the synthesiser again.
    harness.key_press(Key::Z);
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(sampler(&harness), None);
    assert_eq!(harness.state().shell.error_shown(), None);
}

#[test]
fn a_stretch_is_looped_the_open_note_clip_the_selected_clips_or_between_two_markers() {
    let mut harness = editor();
    let looped = |harness: &Harness<'_, App>| {
        let all = harness.state().commands.iter();
        all.filter_map(|c| if let Command::Loop(range) = c { Some(*range) } else { None }).next_back()
    };
    // Nothing to loop yet: the program says what would be.
    harness.key_press(Key::L);
    harness.run_steps(3);
    assert!(harness.state().shell.error_shown().is_some_and(|e| e.contains("marker")));
    assert_eq!(looped(&harness), None);
    // A selected clip: its stretch, in samples (ten seconds at 60 frames a second).
    place(&mut harness, "first take");
    let on_clip = on_timeline(&harness, 30.0, 0);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    click(&mut harness, "Loop");
    assert_eq!(looped(&harness), Some(Some((0, 480_000))));
    // The button says it is on, and switches it off again.
    click(&mut harness, "Loop on");
    assert_eq!(looped(&harness), Some(None));
    // With a piano roll open: its note clip, whatever is selected.
    let on_audio = on_timeline(&harness, 1000.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    let clip = clips(&harness, 1).into_iter().find(|c| c.1 == 4 * 48_000).unwrap();
    harness.key_press(Key::L);
    harness.run_steps(3);
    assert_eq!(looped(&harness), Some(Some((clip.0, clip.0 + clip.1))));
}

#[test]
fn the_piano_roll_zooms_along_the_clip_around_the_pointer() {
    let mut harness = editor();
    let notes = |harness: &Harness<'_, App>| -> Vec<i64> {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        let clip = track.clips.iter().next().unwrap();
        track.notes.get(&clip.id).map(|n| n.iter().map(|n| n.start).collect()).unwrap_or_default()
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    let roll = harness.get_by_label("Piano roll").rect();
    let at = egui::pos2(roll.left() + 46.0 + 300.0, roll.top() + 90.0);
    // Two notes 200 points apart, with the whole clip in view.
    pointer(&mut harness, at, egui::PointerButton::Primary);
    pointer(&mut harness, at + vec2(200.0, 12.0), egui::PointerButton::Primary);
    let wide = notes(&harness);
    assert_eq!(wide.len(), 2);
    // Ctrl + wheel over the first: zoomed in around it, a point 200 to the right is nearer in time,
    // and the place under the pointer is still the first note's.
    wheel(&mut harness, at, vec2(0.0, 240.0), Modifiers::CTRL);
    pointer(&mut harness, at + vec2(200.0, 24.0), egui::PointerButton::Primary);
    let near = notes(&harness);
    assert_eq!(near.len(), 3);
    let added = *near.iter().find(|start| !wide.contains(start)).unwrap();
    assert!(added > wide[0] && added - wide[0] < (wide[1] - wide[0]) * 3 / 4, "{wide:?} {near:?}");
    // Right click where the first note was drawn: it is still there to be removed.
    pointer(&mut harness, at + vec2(2.0, 0.0), egui::PointerButton::Secondary);
    assert_eq!(notes(&harness).len(), 2);
    assert!(!notes(&harness).contains(&wide[0]));
}

#[test]
fn a_track_is_put_into_a_group_which_gets_a_level_of_its_own_beside_the_master() {
    let mut harness = editor();
    place(&mut harness, "first take");
    click(&mut harness, "Mixer");
    let group = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().mix.group
    };
    // No group has a fader until a track goes through it.
    assert!(harness.query_by_label("Group B").is_none() || harness.query_all_by_label("Group B").count() == 1);
    click(&mut harness, "Group B");
    assert_eq!(group(&harness), 2);
    // Now there are two things called so: the choice on the strip and the group's own level beside the master.
    assert_eq!(harness.query_all_by_label("Group B").count(), 2);
    click(&mut harness, "No group");
    assert_eq!(group(&harness), 0);
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(group(&harness), 2);
}

#[test]
fn a_note_clip_keeps_its_notes_when_it_is_copied_cut_in_two_or_moved_to_another_track() {
    let mut harness = editor();
    // (track name, clip start, source offset, notes as the file has them) of every note clip.
    let note_clips = |harness: &Harness<'_, App>| -> Vec<(String, i64, i64, Vec<i64>)> {
        let project = harness.state().shell.session().unwrap().project().clone();
        let mut found = Vec::new();
        for track in project.sequences[0].tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
            for clip in track.clips.iter().filter(|c| c.asset == AssetId(0)) {
                let notes = track.notes.get(&clip.id).map(|n| n.iter().map(|n| n.start).collect()).unwrap_or_default();
                found.push((track.name.clone(), clip.start, clip.src_in, notes));
            }
        }
        found
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    // One note in the first bar, one in the second.
    for (pitch, start) in [(60, 0), (64, 4800)] {
        harness.state_mut().shell.dev(sg_ui::DevStep::Note(0, pitch, start, 480));
    }
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    let first = note_clips(&harness);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].3, [0, 4800]);
    // Copied and pasted at the playhead: the copy has the notes too.
    let on_clip = on_timeline(&harness, first[0].1 as f32 / 800.0 / 0.82 + 20.0, 1);
    pointer(&mut harness, on_clip, egui::PointerButton::Primary);
    harness.key_press_modifiers(Modifiers::CTRL, Key::C);
    playhead(&mut harness, 900);
    harness.key_press_modifiers(Modifiers::CTRL, Key::V);
    harness.run_steps(3);
    let two = note_clips(&harness);
    assert_eq!(two.len(), 2, "{two:?}");
    assert!(two.iter().all(|c| c.3 == [0, 4800]), "{two:?}");
    // Cut in two in the middle of the copy (two bars at 120 a minute are 240 frames):
    // both halves have the notes, and the second begins a bar into them.
    let copy = two.iter().map(|c| c.1).max().unwrap();
    playhead(&mut harness, copy / 800 + 120);
    harness.key_press(Key::S);
    harness.run_steps(3);
    let three = note_clips(&harness);
    assert_eq!(three.len(), 3, "{three:?}");
    assert!(three.iter().all(|c| c.3 == [0, 4800]), "{three:?}");
    assert!(three.iter().any(|c| c.1 == copy + 96_000 && c.2 == 96_000), "{three:?}");
    // Dragged down to a second audio track, the first clip takes its notes along.
    click(&mut harness, "+ Audio");
    let from = on_timeline(&harness, first[0].1 as f32 / 800.0 / 0.82 + 20.0, 1);
    let to = on_timeline(&harness, first[0].1 as f32 / 800.0 / 0.82 + 20.0, 2);
    drag(&mut harness, from, to);
    let moved = note_clips(&harness);
    assert_eq!(moved.iter().filter(|c| c.0 == "A2").map(|c| c.3.clone()).collect::<Vec<_>>(), [vec![0, 4800]]);
    assert_eq!(moved.len(), 3);
    assert_eq!(harness.state().shell.error_shown(), None);
}

#[test]
fn the_songs_tempo_becomes_the_tempo_of_the_sequence() {
    let mut harness = editor();
    right_click(&mut harness, "first take");
    click(&mut harness, "Use as the song");
    // Its beats were found: 139.9 a minute, which is a song made at 140.
    let root = harness.state().root.clone();
    let beats = sg_media::analysis::Beats { bpm: 139.9, beats: (0..40).map(|n| n * 20_586).collect(), steady: true };
    harness.state_mut().shell.beats_loaded(&root, AssetId(1), Ok(beats));
    // (Nothing selected, so that the sequence's and the song's settings are in view.)
    let empty = on_timeline(&harness, 300.0, 4);
    pointer(&mut harness, empty, egui::PointerButton::Primary);
    harness.run_steps(3);
    click(&mut harness, "Bars at this tempo");
    let tempo = harness.state().shell.session().unwrap().project().sequences[0].tempo;
    // (The song lies at the start of the timeline and its first beat at its start: so does the first bar.)
    assert_eq!(tempo, Some(sg_core::Tempo::new(140_000, 4)));
}

#[test]
fn the_first_bar_can_begin_at_the_playhead_and_the_grid_moves_with_it() {
    let mut harness = editor();
    place(&mut harness, "first take");
    let tempo = |harness: &Harness<'_, App>| harness.state().shell.session().unwrap().project().sequences[0].tempo;
    click(&mut harness, "Set tempo");
    // The music begins 20 frames in (not on a beat of a grid that starts with the timeline).
    playhead(&mut harness, 20);
    click(&mut harness, "Bar 1 here");
    assert_eq!(tempo(&harness).map(|t| t.origin), Some(20 * 800));
    // A clip dragged along now lands on a beat counted from there: 20 + a whole number of 30 frames.
    let on_clip = on_timeline(&harness, 60.0, 0);
    drag(&mut harness, on_clip, on_clip + vec2(103.0, 0.0));
    let start = clips(&harness, 0)[0].0;
    assert!(start > 20 && (start - 20) % 30 == 0, "{start}");
    // Undo, twice: the clip back, then the first bar back at the start.
    harness.key_press(Key::Z);
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(tempo(&harness).map(|t| t.origin), Some(0));
}

#[test]
fn keys_of_a_track_get_recordings_of_their_own_as_a_drum_kit() {
    let mut harness = editor();
    let pads = |harness: &Harness<'_, App>| {
        let project = harness.state().shell.session().unwrap().project().clone();
        let track = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clone();
        Vec::clone(&track.pads)
    };
    let on_audio = on_timeline(&harness, 40.0, 1);
    pointer(&mut harness, on_audio, egui::PointerButton::Secondary);
    click(&mut harness, "Add note clip here");
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    click(&mut harness, "Mixer");
    // The first pad goes on C1, the next on the key above.
    for name in ["first take", "second take"] {
        click(&mut harness, "Add pad");
        harness.get_all_by_label(name).last().unwrap().click();
        harness.run_steps(3);
    }
    assert_eq!(pads(&harness), [(36, AssetId(1)), (37, AssetId(2))]);
    click(&mut harness, "Remove pad first take");
    assert_eq!(pads(&harness), [(37, AssetId(2))]);
    harness.key_press(Key::Z);
    harness.run_steps(3);
    assert_eq!(pads(&harness).len(), 2);
}
