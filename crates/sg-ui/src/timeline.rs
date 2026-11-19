//! The timeline: zoomable, drawn with the instanced renderer, and the place
//! where clips are selected, moved, trimmed and cut.
//!
//! Only what is visible is visited: per track one binary search finds the
//! first clip in view, and clips narrower than two pixels are merged into
//! runs (`ClipStore::visit`), so a frame costs the same for 10 or 50 000
//! clips (04-performance.md).
//!
//! Units: the view and the playhead are in sequence frames. Video tracks
//! store frames, audio tracks store samples; `Lane` converts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Align2, Color32, FontId, PointerButton, Rect, Sense, Stroke, StrokeKind, pos2};
use sg_core::time::SAMPLE_RATE;
use sg_core::{
    Asset, AssetId, Clip, ClipId, ClipProps, Marker, Op, Project, Rate, Sequence, Track, TrackId, TrackKind, Visible,
};
use sg_media::analysis::Beats;
use sg_media::peaks::Peaks;

use crate::keymap::Action;
use crate::session::{Placement, Session, Slot, place_at};
use crate::stage::{Transport, ViewerState, timecode};
use crate::theme;
use crate::timeline_gpu::{Instance, TimelineCallback};

const HEADER_W: f32 = 120.0;
const RULER_H: f32 = 22.0;
/// Track rows shrink to fit the panel, within these bounds.
const ROW_MAX: f32 = 40.0;
const ROW_MIN: f32 = 10.0;
/// Distance in points within which an edge is grabbed or snapped to.
const GRAB: f32 = 6.0;
const SNAP: f32 = 8.0;
/// Clips at least this wide (points) get a label.
const LABEL_MIN: f32 = 56.0;

/// A selected clip: where it is and which one, so that a stale entry (after
/// an undo, say) is recognised and dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sel {
    track: TrackId,
    start: i64,
    id: ClipId,
}

/// Unit conversion for one track.
#[derive(Clone, Copy)]
struct Lane {
    kind: TrackKind,
    rate: Rate,
}

impl Lane {
    fn of(track: &Track, sequence: &Sequence) -> Lane {
        Lane { kind: track.kind, rate: sequence.rate }
    }

    /// Track units at the start of sequence frame `frame`.
    fn units(self, frame: i64) -> i64 {
        match self.kind {
            TrackKind::Video => frame,
            TrackKind::Audio => self.rate.frames_to_samples(frame, SAMPLE_RATE),
        }
    }

    /// Track units per sequence frame, for drawing.
    fn per_frame(self) -> f64 {
        match self.kind {
            TrackKind::Video => 1.0,
            TrackKind::Audio => SAMPLE_RATE as f64 * self.rate.den() as f64 / self.rate.num() as f64,
        }
    }

    /// The frame a position falls in (for edges: the frame it starts).
    fn frame(self, units: i64) -> i64 {
        match self.kind {
            TrackKind::Video => units,
            TrackKind::Audio => self.rate.samples_to_frames(units, SAMPLE_RATE),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Start,
    End,
}

enum Drag {
    None,
    /// Moving the playhead.
    Scrub,
    /// Middle button: panning the view.
    Pan,
    /// Pressed on a clip; becomes a move once the pointer travels.
    Move {
        grab_frame: f64,
        grab_row: usize,
        primary: Sel,
    },
    Trim {
        sel: Sel,
        edge: Edge,
        ripple: bool,
    },
}

/// A clip on the internal clipboard, relative to the first copied frame.
struct Copied {
    row: usize,
    clip: Clip,
    /// Offset from the earliest copied clip, in the clip's track units.
    offset: i64,
}

pub struct Timeline {
    /// Sequence frames per point.
    frames_per_point: f64,
    /// Frame at the left edge of the lanes.
    left: f64,
    fitted: bool,
    selection: Vec<Sel>,
    drag: Drag,
    /// Where a move or trim in progress would end, for the ghost and the commit.
    pending: Option<Vec<Op>>,
    ghosts: Vec<(usize, i64, i64, bool)>,
    clipboard: Vec<Copied>,
    instances: Arc<Mutex<Vec<Instance>>>,
    /// Which track `W` selected last, to cycle upwards.
    w_row: usize,
    row_h: f32,
    lanes_w: f32,
    last_playhead: i64,
    /// Where the timeline was drawn last, for drops arriving from outside.
    rect: Rect,
    /// Where an asset being dragged over the timeline would land.
    drop_ghost: Option<Placement>,
    /// The beats of the song, once analysed (`None`: asked for, not there yet).
    song_beats: Option<(AssetId, Option<Arc<Beats>>)>,
    /// The grid made from them: beat positions in samples of the song, for
    /// the tempo choice in `grid_for`.
    grid: Arc<Vec<i64>>,
    grid_for: Option<(AssetId, i8)>,
    /// The song whose beats should be loaded; taken by the shell.
    pub beat_request: Option<AssetId>,
    /// Where posters of assets belong at the heads of clips this frame; the
    /// editor draws them (it has the textures) clipped to `lanes`.
    pub thumbs: Vec<(AssetId, Rect)>,
    pub lanes: Rect,
    /// Waveform data per asset, loaded off this thread on first sight.
    waves: HashMap<AssetId, Wave>,
    /// Assets whose peaks should be loaded; drained by the shell.
    pub wave_requests: Vec<AssetId>,
    /// An asset to move into a scenario, chosen in a clip's menu; taken by the shell.
    pub scenario_request: Option<(AssetId, crate::session::ScenarioChoice)>,
    /// The clip the context menu was opened on, with its row.
    menu_clip: Option<(usize, Clip)>,
    /// The track row the context menu was opened on.
    menu_row: Option<usize>,
    /// Quads drawn in the last frame, for the performance overlay.
    pub quads: usize,
}

impl Default for Timeline {
    fn default() -> Timeline {
        Timeline {
            frames_per_point: 1.0,
            left: 0.0,
            fitted: false,
            selection: Vec::new(),
            drag: Drag::None,
            pending: None,
            ghosts: Vec::new(),
            clipboard: Vec::new(),
            instances: Arc::default(),
            w_row: 0,
            row_h: ROW_MAX,
            lanes_w: 0.0,
            last_playhead: 0,
            rect: Rect::NOTHING,
            drop_ghost: None,
            menu_row: None,
            menu_clip: None,
            scenario_request: None,
            waves: HashMap::new(),
            thumbs: Vec::new(),
            lanes: Rect::NOTHING,
            song_beats: None,
            grid: Arc::default(),
            grid_for: None,
            beat_request: None,
            wave_requests: Vec::new(),
            quads: 0,
        }
    }
}

/// Waveform colour: dark on the clip's fill, like ink on the track colour.
const WAVE: Color32 = Color32::from_rgb(10, 38, 30);

enum Wave {
    /// Asked for; nothing to draw yet.
    Requested,
    Ready(Arc<Peaks>),
}

/// The clip a waveform is drawn for.
struct WaveSpan<'a> {
    clip: &'a Clip,
    /// Source samples per timeline sample.
    speed: f64,
    /// Top and bottom of the drawing area in points.
    y: (f32, f32),
}

/// One vertical line per physical pixel between `x.0` and `x.1`, from the
/// peak level whose blocks are about a pixel wide: the cost depends on the
/// width on screen, not on the length of the sound.
fn wave_columns(
    peaks: &Peaks,
    span: &WaveSpan<'_>,
    x: (f32, f32),
    ppp: f32,
    units_at: &dyn Fn(f32) -> f64,
    units_per_pixel: f64,
    line: &mut dyn FnMut(f32, f32, f32),
) {
    // Peaks are in the file's own rate; the timeline counts 48 kHz samples.
    let rate = peaks.sample_rate as f64 / SAMPLE_RATE as f64 * span.speed;
    let per_pixel = (units_per_pixel * rate).max(1e-9);
    let level = peaks.level_for(per_pixel);
    let block = peaks.block_len(level) as f64;
    let blocks = peaks.blocks(level);
    let (mid, half) = ((span.y.0 + span.y.1) / 2.0, ((span.y.1 - span.y.0) / 2.0).max(1.0));
    let (first, last) = ((x.0 * ppp).ceil() as i64, (x.1 * ppp).floor() as i64);
    for px in first..last {
        let at = px as f32 / ppp;
        let source = (span.clip.src_in as f64 + (units_at(at) - span.clip.start as f64)) * rate;
        if source < 0.0 {
            continue;
        }
        let (b0, b1) = ((source / block) as usize, ((source + per_pixel) / block) as usize);
        if b0 >= blocks {
            break;
        }
        let (mut low, mut high) = (f32::MAX, f32::MIN);
        // A pixel covers at most a few blocks of the chosen level.
        for b in b0..=b1.min(b0 + 8).min(blocks - 1) {
            for channel in 0..peaks.channels as usize {
                if let Some((min, max)) = peaks.get(level, channel, b) {
                    low = low.min(min);
                    high = high.max(max);
                }
            }
        }
        if low > high {
            continue;
        }
        let (top, bottom) = (mid - high.clamp(-1.0, 1.0) * half, mid - low.clamp(-1.0, 1.0) * half);
        // Silence still shows as a thin line.
        line(at, top.min(bottom - 1.0 / ppp), bottom);
    }
}

fn sequence_end(sequence: &Sequence) -> i64 {
    sequence.tracks.iter().map(|t| Lane::of(t, sequence).frame(t.clips.end())).max().unwrap_or(0)
}

fn kind_colour(kind: TrackKind) -> Color32 {
    match kind {
        TrackKind::Video => theme::VIDEO,
        TrackKind::Audio => theme::AUDIO,
    }
}

/// Length of an asset in a lane's units, for clamping trims. `None` for
/// stills, which can be any length.
fn asset_len(asset: &Asset, lane: Lane) -> Option<i64> {
    match lane.kind {
        TrackKind::Video => {
            let v = asset.video.as_ref()?;
            (!v.is_still()).then(|| asset.frames_at(lane.rate, 1))
        }
        TrackKind::Audio => {
            let a = asset.audio.as_ref()?;
            Some((a.samples as i128 * SAMPLE_RATE as i128 / a.sample_rate.max(1) as i128) as i64)
        }
    }
}

impl Timeline {
    pub fn has_selection(&self) -> bool {
        !self.selection.is_empty()
    }

    /// Drops selection entries that no longer point at their clip.
    fn validate(&mut self, sequence: &Sequence) {
        self.selection
            .retain(|s| sequence.track(s.track).and_then(|t| t.clips.get(s.start)).is_some_and(|c| c.id == s.id));
    }

    fn is_selected(&self, track: TrackId, clip: &Clip) -> bool {
        self.selection.iter().any(|s| s.track == track && s.start == clip.start && s.id == clip.id)
    }

    /// The clips on other tracks that belong to `clip`: same asset, same
    /// place in time, same place in the source. This is how picture and
    /// sound of one take stay together without a link stored in the model.
    fn partners(sequence: &Sequence, track: &Track, clip: &Clip) -> Vec<Sel> {
        let lane = Lane::of(track, sequence);
        let per = lane.per_frame();
        let (start, src, len) = (clip.start as f64 / per, clip.src_in as f64 / per, clip.len as f64 / per);
        let mut found = Vec::new();
        for other in sequence.tracks.iter().filter(|t| t.id != track.id) {
            let other_lane = Lane::of(other, sequence);
            let other_per = other_lane.per_frame();
            let probe = (start * other_per).round() as i64;
            let Some(candidate) = other.clips.at(probe).or_else(|| other.clips.first_at_or_after(probe)) else {
                continue;
            };
            let near = |a: f64, b: f64| (a - b).abs() < 1.0;
            if candidate.asset == clip.asset
                && near(candidate.start as f64 / other_per, start)
                && near(candidate.src_in as f64 / other_per, src)
                && near(candidate.len as f64 / other_per, len)
            {
                found.push(Sel { track: other.id, start: candidate.start, id: candidate.id });
            }
        }
        found
    }

    fn select(&mut self, sequence: &Sequence, track: &Track, clip: &Clip, add: bool, alone: bool) {
        let mut picked = vec![Sel { track: track.id, start: clip.start, id: clip.id }];
        if !alone {
            picked.extend(Timeline::partners(sequence, track, clip));
        }
        if !add {
            self.selection.clear();
        } else if self.selection.contains(&picked[0]) {
            // Shift-click on a selected clip takes it out again.
            self.selection.retain(|s| !picked.contains(s));
            return;
        }
        for sel in picked {
            if !self.selection.contains(&sel) {
                self.selection.push(sel);
            }
        }
    }

    /// Zooms so that the whole sequence (or twenty seconds) is in view.
    fn fit(&mut self, sequence: &Sequence, lanes_width: f32) {
        let fps = sequence.rate.num() as f64 / sequence.rate.den() as f64;
        let span = (sequence_end(sequence) as f64 * 1.15).max(fps * 20.0);
        self.frames_per_point = span / lanes_width.max(1.0) as f64;
        self.left = 0.0;
    }

    /// The edge nearest to `frame` among clip edges on all tracks, markers,
    /// the playhead and the start: what a dragged edge snaps to. O(log n) per track.
    fn snap(&self, sequence: &Sequence, playhead: i64, frame: f64, ignore: &[Sel]) -> Option<f64> {
        let reach = SNAP as f64 * self.frames_per_point;
        let mut best: Option<f64> = None;
        let mut consider = |candidate: f64| {
            let distance = (candidate - frame).abs();
            if distance <= reach && best.is_none_or(|b| distance < (b - frame).abs()) {
                best = Some(candidate);
            }
        };
        consider(0.0);
        consider(playhead as f64);
        if let Some(beat) = self.nearest_beat(sequence, frame) {
            consider(beat);
        }
        for marker in sequence.markers.iter() {
            consider(marker.frame as f64);
        }
        for track in &sequence.tracks {
            let lane = Lane::of(track, sequence);
            let per = lane.per_frame();
            let at = (frame * per) as i64;
            let near = [track.clips.last_before(at), track.clips.at(at), track.clips.first_at_or_after(at)];
            for clip in near.into_iter().flatten() {
                if ignore.iter().any(|s| s.track == track.id && s.id == clip.id) {
                    continue;
                }
                consider(clip.start as f64 / per);
                consider(clip.end() as f64 / per);
            }
        }
        best
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        view: &ViewerState,
        transport: &mut Vec<Transport>,
        error: &mut Option<String>,
    ) {
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first().cloned() else { return };
        self.validate(&sequence);
        self.follow_song(&sequence);
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Canvas, true, "Timeline"));
        let lanes = Rect::from_min_max(pos2(rect.left() + HEADER_W, rect.top()), rect.max);
        if !self.fitted && lanes.width() > 10.0 {
            self.fit(&sequence, lanes.width());
            self.fitted = true;
        }
        let row_h = ((rect.height() - RULER_H) / sequence.tracks.len().max(1) as f32).clamp(ROW_MIN, ROW_MAX);
        self.row_h = row_h;
        self.lanes_w = lanes.width();
        let row_at = move |y: f32| ((y - rect.top() - RULER_H) / row_h).floor();
        // Follow the playhead when it leaves the view (playback, jumps).
        if view.frame != self.last_playhead && matches!(self.drag, Drag::None) {
            self.reveal(view.frame, lanes.width());
        }
        self.last_playhead = view.frame;

        self.wheel_and_pan(ui, &response, lanes, &sequence);
        let fpp = self.frames_per_point;
        let left = self.left;
        let frame_at = move |x: f32| left + (x - lanes.left()) as f64 * fpp;
        let x_of = move |frame: f64| lanes.left() + ((frame - left) / fpp) as f32;

        self.pointer(ui, &response, session, &sequence, view, transport, error, rect, lanes, &frame_at, &row_at);
        self.rect = rect;
        self.asset_drop(ui, &response, session, &project, &sequence, view, error);
        self.paint(ui, rect, lanes, &project, &sequence, view, &x_of);
        self.track_controls(ui, &response, session, &sequence, rect, &row_at, error);
    }

    /// Where something dropped at `pos` lands: the frame (snapped like a
    /// moved clip unless `snap` is off) and the track row under it, if any.
    /// `None` if the point is outside the timeline.
    pub fn drop_target(
        &self,
        sequence: &Sequence,
        playhead: i64,
        pos: egui::Pos2,
        snap: bool,
    ) -> Option<(i64, Option<usize>)> {
        if !self.rect.contains(pos) {
            return None;
        }
        let raw = (self.left + (pos.x - self.rect.left() - HEADER_W) as f64 * self.frames_per_point).max(0.0);
        let frame = if snap { self.snap(sequence, playhead, raw, &[]).unwrap_or(raw) } else { raw };
        let row = ((pos.y - self.rect.top() - RULER_H) / self.row_h).floor();
        let row = (row >= 0.0 && (row as usize) < sequence.tracks.len()).then_some(row as usize);
        Some((frame.round() as i64, row))
    }

    /// An asset dragged here from the asset panel: shows where it would
    /// land and places it when the button is released.
    #[allow(clippy::too_many_arguments)]
    fn asset_drop(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        session: &mut Session,
        project: &Project,
        sequence: &Sequence,
        view: &ViewerState,
        error: &mut Option<String>,
    ) {
        self.drop_ghost = None;
        let released = response.dnd_release_payload::<AssetId>();
        let Some(id) = released.clone().or_else(|| response.dnd_hover_payload::<AssetId>()) else { return };
        let Some(asset) = project.assets.get(&*id) else { return };
        let (pos, alt, shift) = ui.input(|i| (i.pointer.interact_pos(), i.modifiers.alt, i.modifiers.shift));
        let Some((frame, row)) = pos.and_then(|p| self.drop_target(sequence, view.frame, p, !alt)) else { return };
        // Shift: only the part that belongs on the track under the pointer
        // (the sound of a video dropped on an audio track, say).
        let only = row.filter(|_| shift).and_then(|r| sequence.tracks.get(r)).map(|t| t.kind);
        if released.is_none() {
            self.drop_ghost = Some(place_at(sequence, asset, frame, row, only));
            return;
        }
        match session.drop_asset(asset, frame, row, only) {
            Ok(_) => self.select_dropped(session, asset.id, frame),
            Err(e) => *error = Some(e.to_string()),
        }
    }

    /// Selects the clips a drop just made, so that the next key acts on them.
    pub fn select_dropped(&mut self, session: &Session, asset: AssetId, frame: i64) {
        let project = session.project();
        let Some(sequence) = project.sequences.first() else { return };
        self.selection.clear();
        for track in &sequence.tracks {
            let start = Lane::of(track, sequence).units(frame);
            if let Some(clip) = track.clips.get(start).filter(|c| c.asset == asset) {
                self.selection.push(Sel { track: track.id, start: clip.start, id: clip.id });
            }
        }
    }

    /// The two add-track buttons in the corner above the track names, and
    /// the right-click menu of the track area.
    #[allow(clippy::too_many_arguments)]
    fn track_controls(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        session: &mut Session,
        sequence: &Sequence,
        rect: Rect,
        row_at: &dyn Fn(f32) -> f32,
        error: &mut Option<String>,
    ) {
        let mut add = None;
        // The switches in the header column are small: little padding, no wrapping.
        ui.spacing_mut().button_padding = egui::vec2(4.0, 0.0);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        let corner = Rect::from_min_size(rect.min, egui::vec2(HEADER_W, RULER_H)).shrink2(egui::vec2(4.0, 2.0));
        let builder = egui::UiBuilder::new().max_rect(corner).layout(egui::Layout::left_to_right(egui::Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (label, kind, tip) in [
                ("+ Video", TrackKind::Video, "Add a video track on top"),
                ("+ Audio", TrackKind::Audio, "Add an audio track at the bottom"),
            ] {
                let text = egui::RichText::new(label).size(10.5).color(kind_colour(kind));
                if ui.add(egui::Button::new(text).small()).on_hover_text(tip).clicked() {
                    add = Some(kind);
                }
            }
        });
        // Quick switches at the right of each track name: shown or hidden for
        // video, mute and solo for audio.
        let mut settings = None;
        if self.row_h >= 18.0 {
            for (row, track) in sequence.tracks.iter().enumerate() {
                let y = rect.top() + RULER_H + row as f32 * self.row_h + self.row_h / 2.0;
                if y + 9.0 > rect.bottom() {
                    break;
                }
                let mut mix = track.mix;
                let small =
                    |ui: &mut egui::Ui, right: f32, width: f32, label: &str, on: bool, colour: Color32, tip: &str| {
                        let area =
                            Rect::from_min_size(pos2(rect.left() + HEADER_W - right, y - 9.0), egui::vec2(width, 18.0));
                        let text = egui::RichText::new(label).size(10.0).color(if on { theme::BG } else { theme::DIM });
                        let button = egui::Button::new(text).small().fill(if on { colour } else { theme::RAISED });
                        ui.put(area, button).on_hover_text(tip).clicked()
                    };
                match track.kind {
                    TrackKind::Video => {
                        let label = if mix.hidden { "hidden" } else { "shown" };
                        if small(
                            ui,
                            50.0,
                            46.0,
                            label,
                            mix.hidden,
                            theme::DIM,
                            "A hidden track is not part of the picture: camera angles to cut from (keys 1 to 9)",
                        ) {
                            mix.hidden = !mix.hidden;
                        }
                    }
                    TrackKind::Audio => {
                        if small(ui, 50.0, 21.0, "M", mix.mute, theme::ERROR, "Mute") {
                            mix.mute = !mix.mute;
                        }
                        if small(ui, 25.0, 21.0, "S", mix.solo, theme::ACCENT, "Solo") {
                            mix.solo = !mix.solo;
                        }
                    }
                }
                if mix != track.mix {
                    settings = Some(Op::SetTrackMix { seq: sequence.id, track: track.id, mix });
                }
            }
        }
        if let Some(op) = settings
            && let Err(e) = session.apply(op, None)
        {
            *error = Some(e.to_string());
        }
        if response.secondary_clicked() {
            let pos = response.interact_pointer_pos();
            let row = pos.map_or(-1.0, |p| row_at(p.y));
            self.menu_row = (row >= 0.0 && (row as usize) < sequence.tracks.len()).then_some(row as usize);
            let frame = pos.map_or(0.0, |p| self.left + (p.x - rect.left() - HEADER_W) as f64 * self.frames_per_point);
            let on_lanes = pos.is_some_and(|p| p.x >= rect.left() + HEADER_W);
            self.menu_clip = self.hit(sequence, row, frame).filter(|_| on_lanes).map(|(row, clip, _)| (row, clip));
            // The menu acts on what is shown as selected.
            if let Some((row, clip)) = self.menu_clip {
                self.select(sequence, &sequence.tracks[row], &clip, false, false);
            }
        }
        let mut remove = None;
        let mut delete: Vec<Sel> = Vec::new();
        response.context_menu(|ui| {
            if let Some((track, clip)) = self.menu_clip.and_then(|(row, clip)| Some((sequence.tracks.get(row)?, clip)))
            {
                let this = Sel { track: track.id, start: clip.start, id: clip.id };
                let partners = Timeline::partners(sequence, track, &clip);
                let kind_of = |sel: &Sel| sequence.track(sel.track).map(|t| t.kind);
                let all: Vec<Sel> = std::iter::once(this).chain(partners.iter().copied()).collect();
                // A take with picture and sound: either part can go alone.
                if !partners.is_empty() {
                    for (label, kind) in [
                        ("Remove sound, keep picture", TrackKind::Audio),
                        ("Remove picture, keep sound", TrackKind::Video),
                    ] {
                        let part: Vec<Sel> = all.iter().copied().filter(|s| kind_of(s) == Some(kind)).collect();
                        if !part.is_empty() && part.len() < all.len() && ui.button(label).clicked() {
                            delete = part;
                            ui.close();
                        }
                    }
                }
                if track.kind == TrackKind::Video {
                    ui.menu_button("Scenario", |ui| {
                        use crate::session::ScenarioChoice;
                        let project = session.project().clone();
                        let current = project.scenario_of(clip.asset).map(|s| s.id);
                        let mut choose = |choice| self.scenario_request = Some((clip.asset, choice));
                        for scenario in project.scenarios.values() {
                            if ui.selectable_label(current == Some(scenario.id), &scenario.name).clicked() {
                                choose(ScenarioChoice::Existing(scenario.id));
                                ui.close();
                            }
                        }
                        if ui.button("New scenario").clicked() {
                            choose(ScenarioChoice::New);
                            ui.close();
                        }
                        if current.is_some() && ui.button("None").clicked() {
                            choose(ScenarioChoice::None);
                            ui.close();
                        }
                    });
                }
                if ui.button("Delete clip (Shift+X)").clicked() {
                    delete = all;
                    ui.close();
                }
                ui.separator();
            }
            if ui.button("Add video track").clicked() {
                add = Some(TrackKind::Video);
                ui.close();
            }
            if ui.button("Add audio track").clicked() {
                add = Some(TrackKind::Audio);
                ui.close();
            }
            if let Some(track) = self.menu_row.and_then(|row| sequence.tracks.get(row)) {
                ui.separator();
                let empty = track.clips.is_empty();
                let button = ui.add_enabled(empty, egui::Button::new(format!("Remove track {}", track.name)));
                if button.on_disabled_hover_text("Only an empty track can be removed.").clicked() {
                    remove = Some(track.id);
                    ui.close();
                }
            }
        });
        let result = match (add, remove) {
            _ if !delete.is_empty() => {
                let ops = delete
                    .iter()
                    .map(|s| Op::RemoveClip { seq: sequence.id, track: s.track, start: s.start })
                    .collect();
                self.selection.retain(|s| !delete.contains(s));
                session.apply(Op::Batch(ops), None)
            }
            (Some(kind), _) => session.add_track(kind),
            (None, Some(id)) => session.apply(Op::RemoveTrack { seq: sequence.id, id }, None),
            (None, None) => Ok(()),
        };
        if let Err(e) = result {
            *error = Some(e.to_string());
        }
    }

    fn wheel_and_pan(&mut self, ui: &egui::Ui, response: &egui::Response, lanes: Rect, sequence: &Sequence) {
        let end = sequence_end(sequence) as f64;
        if response.hovered() {
            let (scroll, pointer, shift) =
                ui.input(|i| (i.smooth_scroll_delta, i.pointer.hover_pos(), i.modifiers.shift));
            // Shift + wheel scrolls; egui already turns that into a horizontal delta.
            let sideways = if shift { scroll.x + scroll.y } else { scroll.x };
            if sideways != 0.0 {
                self.left -= sideways as f64 * self.frames_per_point;
            } else if scroll.y != 0.0 {
                // Zoom around the pointer: the frame under it stays put.
                let x = pointer.map_or(lanes.center().x, |p| p.x) - lanes.left();
                let anchor = self.left + x as f64 * self.frames_per_point;
                let widest = ((end * 2.0).max(600.0) / lanes.width().max(1.0) as f64).max(0.01);
                self.frames_per_point = (self.frames_per_point * (-scroll.y as f64 * 0.004).exp()).clamp(0.005, widest);
                self.left = anchor - x as f64 * self.frames_per_point;
            }
        }
        if response.dragged_by(PointerButton::Middle) {
            self.left -= response.drag_delta().x as f64 * self.frames_per_point;
        }
        // Keep some of the sequence in view.
        let visible = lanes.width() as f64 * self.frames_per_point;
        self.left = self.left.clamp(-visible * 0.1, (end - visible * 0.2).max(0.0));
    }

    /// The clip under a point and, if the point is near one of its ends, that end.
    fn hit(&self, sequence: &Sequence, row: f32, frame: f64) -> Option<(usize, Clip, Option<Edge>)> {
        if row < 0.0 {
            return None;
        }
        let index = row as usize;
        let track = sequence.tracks.get(index)?;
        let per = Lane::of(track, sequence).per_frame();
        let reach = GRAB as f64 * self.frames_per_point * per;
        let at = (frame * per) as i64;
        // An edge can be grabbed from just outside the clip too.
        let candidates = [track.clips.at(at), track.clips.at(at - reach as i64), track.clips.at(at + reach as i64)];
        let clip = candidates.into_iter().flatten().next()?;
        let width = clip.len as f64 / (per * self.frames_per_point);
        // Narrow clips are moved, not trimmed: there is no room to aim.
        let edge = if width < GRAB as f64 * 3.0 {
            None
        } else if ((at - clip.start) as f64) < reach {
            Some(Edge::Start)
        } else if ((clip.end() - at) as f64) < reach {
            Some(Edge::End)
        } else {
            None
        };
        (edge.is_some() || (clip.start <= at && at < clip.end())).then_some((index, clip, edge))
    }

    #[allow(clippy::too_many_arguments)]
    fn pointer(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        session: &mut Session,
        sequence: &Sequence,
        view: &ViewerState,
        transport: &mut Vec<Transport>,
        error: &mut Option<String>,
        rect: Rect,
        lanes: Rect,
        frame_at: &dyn Fn(f32) -> f64,
        row_at: &dyn Fn(f32) -> f32,
    ) {
        let end = sequence_end(sequence);
        let (press, pointer, modifiers) =
            ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos(), i.modifiers));
        let seek = |x: f32, transport: &mut Vec<Transport>| {
            let frame = (frame_at(x).round() as i64).clamp(0, end.max(0));
            if frame != view.frame {
                transport.push(Transport::Seek(frame));
            }
        };

        if let Some(pos) = pointer.filter(|p| response.hovered() && lanes.contains(*p)) {
            let over_edge =
                self.hit(sequence, row_at(pos.y), frame_at(pos.x)).is_some_and(|(_, _, edge)| edge.is_some());
            if over_edge && matches!(self.drag, Drag::None) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }

        if response.drag_started_by(PointerButton::Primary) {
            let origin = press.unwrap_or(rect.min);
            let on_ruler = origin.y < rect.top() + RULER_H || origin.x < lanes.left();
            self.drag = match self.hit(sequence, row_at(origin.y), frame_at(origin.x)).filter(|_| !on_ruler) {
                Some((row, clip, edge)) => {
                    let track = &sequence.tracks[row];
                    let sel = Sel { track: track.id, start: clip.start, id: clip.id };
                    if !self.selection.contains(&sel) {
                        self.select(sequence, track, &clip, modifiers.shift, modifiers.alt);
                    }
                    match edge {
                        Some(edge) => Drag::Trim { sel, edge, ripple: modifiers.ctrl },
                        None => Drag::Move { grab_frame: frame_at(origin.x), grab_row: row, primary: sel },
                    }
                }
                None => Drag::Scrub,
            };
        } else if response.drag_started_by(PointerButton::Middle) {
            self.drag = Drag::Pan;
        }

        if response.clicked_by(PointerButton::Primary)
            && let Some(pos) = pointer
        {
            let on_ruler = pos.y < rect.top() + RULER_H || pos.x < lanes.left();
            match self.hit(sequence, row_at(pos.y), frame_at(pos.x)).filter(|_| !on_ruler) {
                Some((row, clip, _)) => {
                    self.select(sequence, &sequence.tracks[row], &clip, modifiers.shift, modifiers.alt)
                }
                None => {
                    if !modifiers.shift {
                        self.selection.clear();
                    }
                    seek(pos.x, transport);
                }
            }
        }

        if response.dragged_by(PointerButton::Primary)
            && let Some(pos) = pointer
        {
            match &self.drag {
                Drag::Scrub => seek(pos.x, transport),
                Drag::Move { grab_frame, grab_row, primary } => {
                    let (grab_frame, grab_row, primary) = (*grab_frame, *grab_row, *primary);
                    let row_delta = (row_at(pos.y).max(0.0) as isize) - grab_row as isize;
                    self.plan_move(sequence, view, frame_at(pos.x) - grab_frame, row_delta, primary, !modifiers.alt);
                }
                Drag::Trim { sel, edge, ripple } => {
                    let (sel, edge, ripple) = (*sel, *edge, *ripple);
                    self.plan_trim(session, sequence, view, frame_at(pos.x), sel, edge, ripple, !modifiers.alt);
                }
                Drag::Pan | Drag::None => {}
            }
        }

        if response.drag_stopped() {
            if let Some(ops) = self.pending.take().filter(|ops| !ops.is_empty()) {
                // One gesture, one undo step.
                match session.apply(Op::Batch(ops), None) {
                    Ok(()) => self.reselect_after_gesture(session),
                    Err(_) => *error = Some("No room for that here.".to_owned()),
                }
            }
            self.ghosts.clear();
            self.drag = Drag::None;
        }
    }

    /// After a move or trim, the selected clips are found again by id near
    /// where the gesture put them.
    fn reselect_after_gesture(&mut self, session: &Session) {
        let project = session.project();
        let Some(sequence) = project.sequences.first() else { return };
        let moved: Vec<(usize, i64)> = self.ghosts.iter().map(|g| (g.0, g.1)).collect();
        let ids: Vec<ClipId> = self.selection.iter().map(|s| s.id).collect();
        self.selection.clear();
        for (row, start) in moved {
            let Some(track) = sequence.tracks.get(row) else { continue };
            if let Some(clip) = track.clips.get(start).filter(|c| ids.contains(&c.id)) {
                self.selection.push(Sel { track: track.id, start: clip.start, id: clip.id });
            }
        }
    }

    /// Works out where the selected clips would land and whether they fit.
    #[allow(clippy::too_many_arguments)]
    fn plan_move(
        &mut self,
        sequence: &Sequence,
        view: &ViewerState,
        delta: f64,
        row_delta: isize,
        primary: Sel,
        snapping: bool,
    ) {
        self.ghosts.clear();
        self.pending = None;
        let Some(primary_track) = sequence.track(primary.track) else { return };
        let Some(primary_clip) = primary_track.clips.get(primary.start) else { return };
        let per = Lane::of(primary_track, sequence).per_frame();
        let (start, end) = (primary_clip.start as f64 / per, primary_clip.end() as f64 / per);
        // Snap whichever end of the grabbed clip comes closest to an edge.
        let mut delta = delta;
        if snapping {
            let by_start = self.snap(sequence, view.frame, start + delta, &self.selection).map(|s| s - start);
            let by_end = self.snap(sequence, view.frame, end + delta, &self.selection).map(|s| s - end);
            delta = match (by_start, by_end) {
                (Some(a), Some(b)) => {
                    if (a - delta).abs() <= (b - delta).abs() {
                        a
                    } else {
                        b
                    }
                }
                (a, b) => a.or(b).unwrap_or(delta),
            };
        }
        let delta_frames = delta.round() as i64;
        if delta_frames == 0 && row_delta == 0 {
            return;
        }
        let mut removes = Vec::new();
        let mut inserts = Vec::new();
        let mut fits = true;
        for sel in &self.selection {
            let Some(row) = sequence.tracks.iter().position(|t| t.id == sel.track) else { continue };
            let track = &sequence.tracks[row];
            let Some(clip) = track.clips.get(sel.start) else { continue };
            let lane = Lane::of(track, sequence);
            let target_row = row as isize + row_delta;
            let target =
                usize::try_from(target_row).ok().and_then(|r| sequence.tracks.get(r)).filter(|t| t.kind == track.kind);
            // A whole number of frames, in this track's units.
            let shift = lane.units(delta_frames.abs()) * delta_frames.signum();
            let moved = Clip { start: clip.start + shift, ..clip };
            let Some(target) = target.filter(|_| moved.start >= 0) else {
                fits = false;
                self.ghosts.push((row, clip.start, clip.len, false));
                continue;
            };
            // Free if nothing but the moving clips themselves is in the way.
            let blocked = target
                .clips
                .range(moved.start, moved.end())
                .any(|c| !self.selection.iter().any(|s| s.track == target.id && s.id == c.id));
            fits &= !blocked;
            let target_row = sequence.tracks.iter().position(|t| t.id == target.id).unwrap_or(row);
            self.ghosts.push((target_row, moved.start, moved.len, !blocked));
            removes.push(Op::RemoveClip { seq: sequence.id, track: track.id, start: clip.start });
            inserts.push(Op::InsertClip { seq: sequence.id, track: target.id, clip: moved });
        }
        if fits {
            removes.extend(inserts);
            self.pending = Some(removes);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_trim(
        &mut self,
        session: &Session,
        sequence: &Sequence,
        view: &ViewerState,
        pointer_frame: f64,
        sel: Sel,
        edge: Edge,
        ripple: bool,
        snapping: bool,
    ) {
        self.ghosts.clear();
        self.pending = None;
        let project = session.project();
        let mut frame = pointer_frame;
        if snapping {
            frame = self.snap(sequence, view.frame, frame, &self.selection).unwrap_or(frame);
        }
        let frame = frame.round() as i64;
        let mut ops = Vec::new();
        // The grabbed clip and its partners are trimmed together.
        let targets = if self.selection.contains(&sel) { self.selection.clone() } else { vec![sel] };
        for target in targets {
            let Some(row) = sequence.tracks.iter().position(|t| t.id == target.track) else { continue };
            let track = &sequence.tracks[row];
            let Some(clip) = track.clips.get(target.start) else { continue };
            let lane = Lane::of(track, sequence);
            let at = lane.units(frame.max(0));
            let unit = lane.units(1).max(1);
            let source_len = project.assets.get(&clip.asset).and_then(|a| asset_len(a, lane));
            let trimmed = match edge {
                Edge::Start => {
                    // Not past the end, not before the source's first frame,
                    // not into the neighbour on the left.
                    let floor =
                        if ripple { i64::MIN } else { track.clips.last_before(clip.start).map_or(0, |p| p.end()) };
                    let new_start = at.clamp((clip.start - clip.src_in).max(floor).max(0), clip.end() - unit);
                    let cut = new_start - clip.start;
                    Clip {
                        start: if ripple { clip.start } else { new_start },
                        len: clip.len - cut,
                        src_in: clip.src_in + cut,
                        ..clip
                    }
                }
                Edge::End => {
                    let ceiling = if ripple {
                        i64::MAX
                    } else {
                        track.clips.first_at_or_after(clip.end()).map_or(i64::MAX, |n| n.start)
                    };
                    let longest = source_len.map_or(i64::MAX, |len| clip.start + (len - clip.src_in).max(unit));
                    let new_end = at.clamp(clip.start + unit, ceiling.min(longest));
                    Clip { len: new_end - clip.start, ..clip }
                }
            };
            if trimmed == clip {
                continue;
            }
            self.ghosts.push((row, trimmed.start, trimmed.len, true));
            let change = trimmed.len - clip.len;
            if ripple && change > 0 {
                // Make room first, then grow.
                ops.push(Op::ShiftFrom { seq: sequence.id, track: track.id, from: clip.end(), delta: change });
                ops.push(Op::SetClip { seq: sequence.id, track: track.id, start: clip.start, clip: trimmed });
            } else {
                ops.push(Op::SetClip { seq: sequence.id, track: track.id, start: clip.start, clip: trimmed });
                if ripple && change < 0 {
                    ops.push(Op::ShiftFrom { seq: sequence.id, track: track.id, from: clip.end(), delta: change });
                }
            }
        }
        self.pending = Some(ops);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        lanes: Rect,
        project: &Project,
        sequence: &Sequence,
        view: &ViewerState,
        x_of: &dyn Fn(f64) -> f32,
    ) {
        let ppp = ui.ctx().pixels_per_point();
        let row_h = self.row_h;
        let painter = ui.painter_at(rect);
        let fpp = self.frames_per_point;
        let visible = (self.left, self.left + lanes.width() as f64 * fpp);
        let px = |points: f32| points * ppp;
        let mut instances = self.instances.lock().unwrap_or_else(PoisonError::into_inner);
        instances.clear();
        // Instance coordinates are physical pixels relative to the rect.
        let quad = |x0: f32, y0: f32, x1: f32, y1: f32| {
            [px(x0 - rect.left()), px(y0 - rect.top()), px((x1 - x0).max(0.0)), px(y1 - y0)]
        };
        instances.push(Instance::new(quad(rect.left(), rect.top(), rect.right(), rect.bottom()), theme::BG, 0.0, 0.0));
        instances.push(Instance::new(
            quad(rect.left(), rect.top(), rect.right(), rect.top() + RULER_H),
            theme::PANEL,
            0.0,
            0.0,
        ));
        let mut labels: Vec<(f32, f32, f32, &str)> = Vec::new();
        let has_scenarios = !project.scenarios.is_empty();
        let mut thumbs: Vec<(AssetId, Rect)> = Vec::new();
        // Assets whose waveform is not loaded yet.
        let mut wanted: Vec<AssetId> = Vec::new();
        let left = self.left;

        for (row, track) in sequence.tracks.iter().enumerate() {
            let top = rect.top() + RULER_H + row as f32 * row_h;
            if top > rect.bottom() {
                break;
            }
            let shade = if row % 2 == 0 { theme::PANEL } else { theme::RAISED.gamma_multiply(0.8) };
            instances.push(Instance::new(quad(rect.left(), top, rect.right(), top + row_h - 1.0), shade, 0.0, 0.0));
            // A hidden camera angle is drawn faintly: it is material, not picture.
            let colour =
                if track.mix.hidden { kind_colour(track.kind).gamma_multiply(0.45) } else { kind_colour(track.kind) };
            instances.push(Instance::new(
                quad(rect.left(), top, rect.left() + 3.0, top + row_h - 1.0),
                colour,
                0.0,
                0.0,
            ));
            let lane = Lane::of(track, sequence);
            let per = lane.per_frame();
            // Two physical pixels, in this track's units.
            let min_len = ((2.0 / ppp as f64) * fpp * per).ceil() as i64;
            let (t0, t1) = ((visible.0 * per).floor() as i64, (visible.1 * per).ceil() as i64);
            // Small rows keep less padding so that the clips stay visible.
            let pad = if row_h < 20.0 { 1.0 } else { 3.0 };
            let (y0, y1) = (top + pad, top + row_h - pad - 1.0);
            track.clips.visit(t0, t1, min_len, |item| match item {
                Visible::Clip(clip) => {
                    let (x0, x1) = (
                        x_of(clip.start as f64 / per).max(lanes.left() - 4.0),
                        x_of(clip.end() as f64 / per).min(lanes.right() + 4.0),
                    );
                    let selected = self.is_selected(track.id, &clip);
                    let fill = if selected { colour } else { colour.gamma_multiply(0.62) };
                    instances.push(Instance::new(
                        quad(x0, y0, x1, y1),
                        fill,
                        px(3.0),
                        if selected { 0.0 } else { 1.0 },
                    ));
                    if track.kind == TrackKind::Audio && row_h >= 14.0 && x1 - x0 >= 4.0 {
                        match self.waves.get(&clip.asset) {
                            Some(Wave::Ready(peaks)) => {
                                let speed = track
                                    .props_of(clip.id)
                                    .filter(|p| p.speed_valid())
                                    .map_or(1.0, |p| p.speed.0 as f64 / p.speed.1 as f64);
                                let span = WaveSpan { clip: &clip, speed, y: (y0 + 2.0, y1 - 2.0) };
                                wave_columns(
                                    peaks,
                                    &span,
                                    (x0.max(lanes.left()), x1.min(lanes.right())),
                                    ppp,
                                    &|x| (left + (x - lanes.left()) as f64 * fpp) * per,
                                    fpp * per / ppp as f64,
                                    &mut |x, top, bottom| {
                                        instances.push(Instance::new(
                                            quad(x, top, x + 1.0 / ppp, bottom),
                                            WAVE,
                                            0.0,
                                            0.0,
                                        ));
                                    },
                                );
                            }
                            Some(Wave::Requested) => {}
                            None => wanted.push(clip.asset),
                        }
                    }
                    if selected {
                        instances.push(Instance::new(quad(x0, y1 - 2.0, x1, y1), theme::ACCENT, 0.0, 0.0));
                    }
                    // The scenario of the clip's asset: a stripe in its colour along the top.
                    if has_scenarios
                        && track.kind == TrackKind::Video
                        && let Some(scenario) = project.scenario_of(clip.asset)
                    {
                        let stripe = crate::colour_panel::stripe(scenario);
                        instances.push(Instance::new(quad(x0 + 1.0, y0, x1 - 1.0, y0 + 3.0), stripe, 0.0, 0.0));
                    }
                    // A transition into this clip: a mark across the cut.
                    if track.props_of(clip.id).is_some_and(|p| p.transition.is_some()) {
                        instances.push(Instance::new(
                            quad(x0 - 4.0, y0 - 1.0, x0 + 4.0, y1 + 1.0),
                            theme::ACCENT,
                            px(3.0),
                            0.0,
                        ));
                    }
                    // The poster of the asset at the head of a clip that has room for it.
                    let mut thumb_w = 0.0;
                    let is_title = track.props_of(clip.id).is_some_and(|p| p.title.is_some());
                    if track.kind == TrackKind::Video && !is_title && row_h >= 28.0 && thumbs.len() < 256 {
                        let (left, height) = (x0.max(lanes.left()) + 1.0, y1 - y0 - 5.0);
                        let width = height * 16.0 / 9.0;
                        if x1 - left >= width * 2.0 {
                            thumbs.push((
                                clip.asset,
                                Rect::from_min_size(pos2(left, y0 + 4.0), egui::vec2(width, height)),
                            ));
                            thumb_w = width;
                        }
                    }
                    if x1 - x0 >= LABEL_MIN && row_h >= 16.0 && labels.len() < 400 {
                        // A title shows its text, anything else its asset's label.
                        let title = track.props_of(clip.id).and_then(|p| p.title.as_ref()).map(|t| t.text.as_str());
                        let name = title
                            .or(project.assets.get(&clip.asset).map(|a| a.label.as_str()))
                            .unwrap_or("missing asset");
                        labels.push((
                            x0.max(lanes.left()) + 5.0 + thumb_w,
                            (y0 + y1) / 2.0,
                            x1 - x0.max(lanes.left()) - 8.0 - thumb_w,
                            name,
                        ));
                    }
                }
                Visible::Run { start, end } => {
                    let (x0, x1) = (x_of(start as f64 / per), x_of(end as f64 / per));
                    instances.push(Instance::new(
                        quad(x0, y0, x1.max(x0 + 1.0 / ppp), y1),
                        colour.gamma_multiply(0.45),
                        0.0,
                        0.0,
                    ));
                }
            });
        }
        // The header column covers whatever slid under it.
        instances.push(Instance::new(
            quad(rect.left() + 3.0, rect.top() + RULER_H, lanes.left(), rect.bottom()),
            theme::PANEL,
            0.0,
            0.0,
        ));
        self.quads = instances.len();
        drop(instances);
        self.thumbs = thumbs;
        self.lanes = lanes;
        for id in wanted {
            if self.waves.insert(id, Wave::Requested).is_none() {
                self.wave_requests.push(id);
            }
        }
        painter.add(egui_wgpu::Callback::new_paint_callback(
            rect,
            TimelineCallback { instances: self.instances.clone(), size: [px(rect.width()), px(rect.height())] },
        ));

        // Text and thin lines go through egui, on top of the quads.
        for (x, y, width, name) in labels {
            let fits = (width / 6.2) as usize;
            let text: String = if name.chars().count() > fits {
                name.chars().take(fits.saturating_sub(1)).chain(['…']).collect()
            } else {
                name.to_owned()
            };
            painter.text(pos2(x, y), Align2::LEFT_CENTER, text, FontId::proportional(11.0), theme::TEXT);
        }
        for (row, track) in sequence.tracks.iter().enumerate() {
            let y = rect.top() + RULER_H + row as f32 * row_h + row_h / 2.0;
            painter.text(
                pos2(rect.left() + 12.0, y),
                Align2::LEFT_CENTER,
                &track.name,
                FontId::proportional((row_h - 4.0).clamp(8.0, 13.0)),
                if track.mix.hidden || (track.kind == TrackKind::Audio && track.mix.mute) {
                    theme::DIM
                } else {
                    theme::TEXT
                },
            );
        }
        self.paint_ruler(&painter, rect, lanes, sequence, x_of);
        // The beat grid of the song: every beat a faint line, every fourth a clearer one.
        let rows_bottom = (rect.top() + RULER_H + sequence.tracks.len() as f32 * row_h).min(rect.bottom());
        for (frame, index) in self.grid_lines(sequence, visible.0, visible.1, 6.0) {
            let x = x_of(frame);
            if x < lanes.left() {
                continue;
            }
            let bar = index % 4 == 0;
            let colour = Color32::from_white_alpha(if bar { 46 } else { 18 });
            painter.line_segment([pos2(x, rect.top() + RULER_H), pos2(x, rows_bottom)], Stroke::new(1.0, colour));
            painter.line_segment(
                [pos2(x, rect.top() + RULER_H - if bar { 7.0 } else { 4.0 }), pos2(x, rect.top() + RULER_H)],
                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.8)),
            );
        }
        for &(row, start, len, fits) in &self.ghosts {
            let Some(track) = sequence.tracks.get(row) else { continue };
            let per = Lane::of(track, sequence).per_frame();
            let top = rect.top() + RULER_H + row as f32 * row_h;
            let ghost = Rect::from_min_max(
                pos2(x_of(start as f64 / per), top + 3.0),
                pos2(x_of((start + len) as f64 / per), top + row_h - 4.0),
            );
            let colour = if fits { theme::ACCENT } else { theme::ERROR };
            painter.rect_filled(ghost, 3.0, colour.gamma_multiply(0.25));
            painter.rect_stroke(ghost, 3.0, Stroke::new(1.5, colour), StrokeKind::Inside);
        }
        if let Some(drop) = self.drop_ghost {
            let lanes_painter = painter.with_clip_rect(lanes);
            let (x0, x1) =
                (x_of(drop.start as f64), x_of((drop.start + drop.frames) as f64).max(x_of(drop.start as f64) + 3.0));
            let rows_top = rect.top() + RULER_H;
            let rows_bottom = rows_top + sequence.tracks.len() as f32 * row_h;
            for (slot, kind) in [(drop.video, TrackKind::Video), (drop.audio, TrackKind::Audio)] {
                let colour = kind_colour(kind);
                match slot {
                    Some(Slot::Row(row)) => {
                        let top = rows_top + row as f32 * row_h;
                        let ghost = Rect::from_min_max(pos2(x0, top + 3.0), pos2(x1, top + row_h - 4.0));
                        lanes_painter.rect_filled(ghost, 3.0, colour.gamma_multiply(0.3));
                        lanes_painter.rect_stroke(ghost, 3.0, Stroke::new(1.5, theme::ACCENT), StrokeKind::Inside);
                    }
                    // A track that the drop will make: a bar where it appears.
                    Some(Slot::New) => {
                        let (y, label, align) = match kind {
                            TrackKind::Video => (rows_top, "new video track", Align2::LEFT_TOP),
                            TrackKind::Audio => {
                                (rows_bottom.min(rect.bottom() - 4.0), "new audio track", Align2::LEFT_BOTTOM)
                            }
                        };
                        let bar = Rect::from_min_max(pos2(x0, y - 2.0), pos2(x1, y + 2.0));
                        lanes_painter.rect_filled(bar, 2.0, theme::ACCENT);
                        let offset = if kind == TrackKind::Video { 4.0 } else { -4.0 };
                        lanes_painter.text(
                            pos2(x0 + 4.0, y + offset),
                            align,
                            label,
                            FontId::proportional(11.0),
                            colour,
                        );
                    }
                    None => {}
                }
            }
        }
        painter.line_segment(
            [pos2(lanes.left(), rect.top()), pos2(lanes.left(), rect.bottom())],
            Stroke::new(1.0, theme::LINE),
        );
        let x = x_of(view.frame as f64);
        if x >= lanes.left() - 1.0 {
            painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, theme::ACCENT));
        }
    }

    fn paint_ruler(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        lanes: Rect,
        sequence: &Sequence,
        x_of: &dyn Fn(f64) -> f32,
    ) {
        let rate = sequence.rate;
        let fps = rate.num() as f64 / rate.den() as f64;
        let fpp = self.frames_per_point;
        // A tick about every 90 points: a round number of frames or seconds.
        let frames_per_tick = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0]
            .into_iter()
            .chain([1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0].into_iter().map(|s| s * fps))
            .find(|f| *f / fpp >= 90.0)
            .unwrap_or(3600.0 * fps);
        let mut tick = (self.left / frames_per_tick).floor().max(0.0) * frames_per_tick;
        let right = self.left + lanes.width() as f64 * fpp;
        while tick <= right {
            let x = x_of(tick);
            if x >= lanes.left() {
                painter.line_segment(
                    [pos2(x, rect.top() + 13.0), pos2(x, rect.top() + RULER_H)],
                    Stroke::new(1.0, theme::LINE),
                );
                let code = timecode(tick.round() as i64, rate);
                let text = if frames_per_tick < fps { &code[..] } else { &code[..8] };
                painter.text(
                    pos2(x + 3.0, rect.top() + 2.0),
                    Align2::LEFT_TOP,
                    text,
                    FontId::monospace(10.0),
                    theme::DIM,
                );
            }
            tick += frames_per_tick;
        }
        for marker in sequence.markers.iter() {
            let x = x_of(marker.frame as f64);
            if x < lanes.left() || x > lanes.right() {
                continue;
            }
            let colour = Color32::from_rgb(240, 200, 60);
            painter.line_segment(
                [pos2(x, rect.top() + RULER_H - 8.0), pos2(x, rect.bottom())],
                Stroke::new(1.0, colour.gamma_multiply(0.5)),
            );
            painter.circle_filled(pos2(x, rect.top() + RULER_H - 5.0), 3.5, colour);
            if !marker.label.is_empty() {
                painter.text(
                    pos2(x + 6.0, rect.top() + RULER_H - 5.0),
                    Align2::LEFT_CENTER,
                    &marker.label,
                    FontId::proportional(10.0),
                    colour,
                );
            }
        }
    }

    /// The clips an edit at the playhead applies to: the selected ones that
    /// contain it, or every clip under it if none does.
    fn targets(&self, sequence: &Sequence, playhead: i64) -> Vec<(Arc<Track>, Clip)> {
        let under: Vec<(Arc<Track>, Clip)> = sequence
            .tracks
            .iter()
            .filter_map(|t| {
                let at = Lane::of(t, sequence).units(playhead);
                // A cut exactly on a clip's first frame cuts nothing.
                t.clips.at(at).filter(|c| c.start < at).map(|c| (t.clone(), c))
            })
            .collect();
        let chosen: Vec<(Arc<Track>, Clip)> =
            under.iter().filter(|(t, c)| self.is_selected(t.id, c)).cloned().collect();
        if chosen.is_empty() { under } else { chosen }
    }

    /// Runs a timeline key action. Returns where the playhead should go, if anywhere.
    pub fn act(
        &mut self,
        action: Action,
        session: &mut Session,
        playhead: i64,
        error: &mut Option<String>,
    ) -> Option<i64> {
        let project = session.project().clone();
        let sequence = project.sequences.first()?.clone();
        self.validate(&sequence);
        let seq = sequence.id;
        let mut seek = None;
        let mut ops: Vec<Op> = Vec::new();
        match action {
            Action::Split => {
                for (track, clip) in self.targets(&sequence, playhead) {
                    let at = Lane::of(&track, &sequence).units(playhead);
                    ops.extend(split_ops(seq, &track, Lane::of(&track, &sequence), &clip, at, session.new_clip_id()));
                }
                self.selection.clear();
            }
            Action::RippleStart | Action::RippleEnd => {
                let targets = self.targets(&sequence, playhead);
                let mut removed_frames = 0;
                for (track, clip) in &targets {
                    let lane = Lane::of(track, &sequence);
                    let at = lane.units(playhead);
                    let (kept, cut) = if action == Action::RippleStart {
                        let cut = at - clip.start;
                        (Clip { len: clip.len - cut, src_in: clip.src_in + cut, ..*clip }, cut)
                    } else {
                        (Clip { len: at - clip.start, ..*clip }, clip.end() - at)
                    };
                    ops.push(Op::SetClip { seq, track: track.id, start: clip.start, clip: kept });
                    ops.push(Op::ShiftFrom { seq, track: track.id, from: clip.end(), delta: -cut });
                    removed_frames = removed_frames.max(lane.frame(cut));
                }
                // The other tracks follow, so that everything stays in sync.
                let mut with_others = ops.clone();
                for track in sequence.tracks.iter().filter(|t| !targets.iter().any(|(x, _)| x.id == t.id)) {
                    let lane = Lane::of(track, &sequence);
                    with_others.push(Op::ShiftFrom {
                        seq,
                        track: track.id,
                        from: lane.units(playhead),
                        delta: -lane.units(removed_frames),
                    });
                }
                if !ops.is_empty() && session.apply(Op::Batch(with_others), None).is_ok() {
                    ops.clear();
                }
                if action == Action::RippleStart {
                    seek = targets.iter().map(|(t, c)| Lane::of(t, &sequence).frame(c.start)).min();
                }
                self.selection.clear();
            }
            Action::RippleDelete | Action::Delete => {
                // Right to left per track, so that earlier positions stay valid.
                let mut selected = self.selection.clone();
                selected.sort_by_key(|s| std::cmp::Reverse(s.start));
                for sel in selected {
                    let Some(clip) = sequence.track(sel.track).and_then(|t| t.clips.get(sel.start)) else { continue };
                    ops.push(Op::RemoveClip { seq, track: sel.track, start: clip.start });
                    if action == Action::RippleDelete {
                        ops.push(Op::ShiftFrom { seq, track: sel.track, from: clip.end(), delta: -clip.len });
                    }
                }
                self.selection.clear();
            }
            Action::SelectUnder => {
                let under: Vec<usize> = (0..sequence.tracks.len())
                    .rev()
                    .filter(|&row| {
                        let t = &sequence.tracks[row];
                        t.clips.at(Lane::of(t, &sequence).units(playhead)).is_some()
                    })
                    .collect();
                if let Some(&row) = under.iter().find(|&&row| row < self.w_row).or(under.first()) {
                    let track = &sequence.tracks[row];
                    if let Some(clip) = track.clips.at(Lane::of(track, &sequence).units(playhead)) {
                        self.select(&sequence, track, &clip, false, true);
                        self.w_row = row;
                    }
                }
            }
            Action::Copy => {
                let picked: Vec<(usize, Clip)> = self
                    .selection
                    .iter()
                    .filter_map(|s| {
                        let row = sequence.tracks.iter().position(|t| t.id == s.track)?;
                        Some((row, sequence.tracks[row].clips.get(s.start)?))
                    })
                    .collect();
                let first = picked
                    .iter()
                    .map(|(row, c)| Lane::of(&sequence.tracks[*row], &sequence).frame(c.start))
                    .min()
                    .unwrap_or(0);
                self.clipboard = picked
                    .into_iter()
                    .map(|(row, clip)| Copied {
                        row,
                        clip,
                        offset: clip.start - Lane::of(&sequence.tracks[row], &sequence).units(first),
                    })
                    .collect();
            }
            Action::PasteClips => {
                self.selection.clear();
                for copied in &self.clipboard {
                    let Some(track) = sequence.tracks.get(copied.row) else { continue };
                    let start = Lane::of(track, &sequence).units(playhead) + copied.offset;
                    let clip = Clip { id: session.new_clip_id(), start, ..copied.clip };
                    ops.push(Op::InsertClip { seq, track: track.id, clip });
                }
            }
            Action::Marker => {
                ops.push(match sequence.markers.iter().any(|m| m.frame == playhead) {
                    true => Op::RemoveMarker { seq, frame: playhead },
                    false => Op::AddMarker { seq, marker: Marker { frame: playhead, label: String::new() } },
                });
            }
            Action::PrevCut | Action::NextCut => {
                let forward = action == Action::NextCut;
                let mut edges: Vec<i64> = sequence.markers.iter().map(|m| m.frame).collect();
                for track in &sequence.tracks {
                    let lane = Lane::of(track, &sequence);
                    let at = lane.units(playhead);
                    let near = [track.clips.last_before(at), track.clips.at(at), track.clips.first_at_or_after(at + 1)];
                    for clip in near.into_iter().flatten() {
                        edges.extend([lane.frame(clip.start), lane.frame(clip.end())]);
                    }
                }
                seek = if forward {
                    edges.into_iter().filter(|e| *e > playhead).min()
                } else {
                    edges.into_iter().filter(|e| *e < playhead).max().or(Some(0))
                };
            }
            _ => {}
        }
        if !ops.is_empty()
            && let Err(e) = session.apply(Op::Batch(ops), None)
        {
            *error = Some(match action {
                Action::PasteClips => "No room to paste here.".to_owned(),
                _ => e.to_string(),
            });
        }
        seek
    }

    /// Scrolls so that `frame` is in view (after a jump).
    pub fn reveal(&mut self, frame: i64, lanes_width: f32) {
        let visible = lanes_width.max(1.0) as f64 * self.frames_per_point;
        if (frame as f64) < self.left || (frame as f64) > self.left + visible {
            self.left = (frame as f64 - visible * 0.3).max(0.0);
        }
    }
}

impl Timeline {
    /// Sets zoom and scroll directly (scripted runs and benchmarks).
    pub fn set_view(&mut self, frames_per_point: f64, left: f64) {
        self.frames_per_point = frames_per_point.max(0.001);
        self.left = left;
        self.fitted = true;
    }
}

impl Timeline {
    /// The view of spike S2's frame-time script at time `t` seconds: zoom
    /// swings on a log scale between "whole sequence" and 200 points per
    /// second (period 5 s) while the view sweeps across 90 % of the sequence
    /// (period 7 s).
    pub fn bench_view(&mut self, t: f64, sequence: &Sequence) {
        let width = self.lanes_w.max(100.0) as f64;
        let end = sequence_end(sequence).max(1) as f64;
        let fps = sequence.rate.num() as f64 / sequence.rate.den() as f64;
        let (wide, close) = ((end / width).ln(), (fps / 200.0).ln());
        let z = 0.5 - 0.5 * (std::f64::consts::TAU * t / 5.0).cos();
        let fpp = (wide + (close - wide) * z).exp();
        let visible = width * fpp;
        let centre = end * (0.5 + 0.45 * (std::f64::consts::TAU * t / 7.0).sin());
        self.set_view(fpp, (centre - visible / 2.0).clamp(0.0, (end - visible).max(0.0)));
    }
}

impl Timeline {
    /// The clip the inspector and the viewer act on: the first selected one.
    pub fn primary(&self, sequence: &Sequence) -> Option<(Arc<Track>, Clip)> {
        let sel = self.selection.first()?;
        let track = sequence.track(sel.track)?;
        let clip = track.clips.get(sel.start).filter(|c| c.id == sel.id)?;
        Some((track.clone(), clip))
    }

    /// First and last frame covered by the selected clips.
    pub fn selection_span(&self, sequence: &Sequence) -> Option<(i64, i64)> {
        let spans = self.selection.iter().filter_map(|s| {
            let track = sequence.track(s.track)?;
            let clip = track.clips.get(s.start)?;
            let lane = Lane::of(track, sequence);
            Some((lane.frame(clip.start), lane.frame(clip.end())))
        });
        spans.reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
    }

    /// Selects one clip by position, alone.
    pub fn select_only(&mut self, track: TrackId, clip: &Clip) {
        self.selection = vec![Sel { track, start: clip.start, id: clip.id }];
    }
}

impl Timeline {
    /// Peaks read from the cache; `None` if the file is not there yet, in
    /// which case the asset stays "requested" until the importer reports
    /// new peaks.
    pub fn set_peaks(&mut self, id: AssetId, peaks: Option<Peaks>) {
        if let Some(peaks) = peaks {
            self.waves.insert(id, Wave::Ready(Arc::new(peaks)));
        }
    }

    /// The peaks of this asset were (re)built: load them at the next sight.
    pub fn peaks_changed(&mut self, id: AssetId) {
        self.waves.remove(&id);
    }
}

impl Timeline {
    /// Puts a whip-pan on the cut nearest to the playhead (on the selected
    /// clip's track, else on the lowest shown video track that has a cut
    /// nearby), or removes the transition that is there. The direction
    /// alternates with the transition before it. Returns the frame of the cut.
    pub fn toggle_transition(
        &mut self,
        session: &mut Session,
        playhead: i64,
        error: &mut Option<String>,
    ) -> Option<i64> {
        let project = session.project().clone();
        let sequence = project.sequences.first()?;
        let selected =
            self.selection.iter().find_map(|s| sequence.track(s.track).filter(|t| t.kind == TrackKind::Video));
        let shown = sequence.tracks.iter().rev().filter(|t| t.kind == TrackKind::Video && !t.mix.hidden);
        // The incoming clip of the nearest cut on a track: a clip that
        // starts where another one ends.
        let nearest = |track: &Arc<Track>| -> Option<Clip> {
            let here = track.clips.at(playhead).or_else(|| track.clips.last_before(playhead))?;
            let after = track.clips.at(here.end());
            [Some(here), after]
                .into_iter()
                .flatten()
                .filter(|c| c.start > 0 && track.clips.at(c.start - 1).is_some_and(|p| p.end() == c.start))
                .min_by_key(|c| (c.start - playhead).abs())
        };
        let (track, clip) = selected
            .into_iter()
            .chain(shown)
            .filter_map(|t| nearest(t).map(|c| (t.clone(), c)))
            .min_by_key(|(_, c)| (c.start - playhead).abs())
            .or_else(|| {
                *error = Some("There is no cut here: a transition sits between two clips that touch.".to_owned());
                None
            })?;
        let mut props = track.props_of(clip.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
        props.transition = match props.transition {
            Some(_) => None,
            None => {
                // Opposite to the transition before this one on the track.
                let mut earlier = track.clips.last_before(clip.start);
                let mut previous = None;
                for _ in 0..256 {
                    let Some(c) = earlier else { break };
                    previous = track.props_of(c.id).and_then(|p| p.transition);
                    if previous.is_some() {
                        break;
                    }
                    earlier = track.clips.last_before(c.start);
                }
                let angle = previous.map_or(0.0, |p| (p.angle + 180.0).rem_euclid(360.0));
                Some(sg_core::Transition { angle, ..Default::default() })
            }
        };
        let props = (props != ClipProps::default()).then(|| Box::new(props));
        let op = Op::SetClipProps { seq: sequence.id, track: track.id, clip: clip.id, props };
        match session.apply(op, None) {
            Ok(()) => {
                self.selection = vec![Sel { track: track.id, start: clip.start, id: clip.id }];
                Some(clip.start)
            }
            Err(e) => {
                *error = Some(e.to_string());
                None
            }
        }
    }
}

/// Cuts `clip` at `at` (track units) and gives both halves what belongs to
/// them of its properties: the right half continues the source where the
/// left one stops (speed and ramp counted), keeps its keyframes in place
/// and the fade-out; the left half keeps the fade-in and the transition
/// into the clip.
fn split_ops(seq: sg_core::SequenceId, track: &Track, lane: Lane, clip: &Clip, at: i64, right_id: ClipId) -> Vec<Op> {
    if at <= clip.start || at >= clip.end() {
        return Vec::new();
    }
    let Some(props) = track.props_of(clip.id) else {
        return Op::split(seq, track.id, clip, at, right_id).into_iter().collect();
    };
    // How far into the clip the cut is, in sequence frames (the unit of keyframes).
    let cut = lane.frame(at) - lane.frame(clip.start);
    let consumed = match lane.kind {
        TrackKind::Video => props.source_offset(cut),
        // Sound has no ramp: the set speed times the samples played.
        TrackKind::Audio => {
            let (num, den) = if props.speed_valid() { props.speed } else { (1, 1) };
            ((at - clip.start) as i128 * num as i128 / den as i128) as i64
        }
    };
    let (mut left, mut right) = (ClipProps::clone(props), ClipProps::clone(props));
    (left.fade_out, right.fade_in, right.transition) = (0, 0, None);
    for param in right.picture_params() {
        param.shift_keys(-cut);
    }
    right.gain_db.shift_keys(-cut);
    // A ramp continues from its value at the cut, not from its start.
    let left_clip = Clip { len: at - clip.start, ..*clip };
    let right_clip = Clip { id: right_id, start: at, len: clip.end() - at, src_in: clip.src_in + consumed, ..*clip };
    let stored = |props: ClipProps| (props != ClipProps::default()).then(|| Box::new(props));
    vec![
        Op::SetClip { seq, track: track.id, start: clip.start, clip: left_clip },
        Op::InsertClip { seq, track: track.id, clip: right_clip },
        Op::SetClipProps { seq, track: track.id, clip: clip.id, props: stored(left) },
        Op::SetClipProps { seq, track: track.id, clip: right_id, props: stored(right) },
    ]
}

impl Timeline {
    /// Moves every cut of the programme track onto the nearest line of the
    /// beat grid, if one lies within a third of a beat: the clip before the
    /// cut gets longer or shorter and the one after it starts later or
    /// earlier in its source by as much, so nothing slips out of sync.
    /// Returns how many cuts moved. One undo step.
    pub fn cuts_to_beats(&mut self, session: &mut Session, error: &mut Option<String>) -> usize {
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return 0 };
        let Some(track) = crate::session::program_row(sequence).map(|row| sequence.tracks[row].clone()) else {
            return 0;
        };
        // Beats that arrived since the last frame was drawn count too.
        self.follow_song(sequence);
        if self.grid.len() < 2 {
            *error = Some("There is no beat grid yet: choose the song first.".to_owned());
            return 0;
        }
        // A third of a beat, in frames.
        let reach = (self.grid[1] - self.grid[0]) as f64 / SAMPLE_RATE as f64 * sequence.rate.num() as f64
            / sequence.rate.den() as f64
            / 3.0;
        // The clips as they are after the cuts moved so far: each one is the
        // clip after one cut and the clip before the next.
        let mut clips: Vec<Clip> = track.clips.iter().collect();
        let mut ops = Vec::new();
        for i in 1..clips.len() {
            let (before, after) = (clips[i - 1], clips[i]);
            if before.end() != after.start {
                continue;
            }
            let cut = after.start;
            let Some(beat) = self.nearest_beat(sequence, cut as f64).map(|b| b.round() as i64) else { continue };
            let by = beat - cut;
            // Not past the middle of either neighbour, and only with source to show.
            let room = by.abs() as f64 <= reach && by.abs() * 2 < before.len.min(after.len) && after.src_in + by >= 0;
            let long_enough = project.assets.get(&before.asset).is_none_or(|a| {
                a.video
                    .as_ref()
                    .is_none_or(|v| v.is_still() || before.src_in + before.len + by <= a.frames_at(sequence.rate, 1))
            });
            if by == 0 || !room || !long_enough {
                continue;
            }
            let (longer, later) = (
                Clip { len: before.len + by, ..before },
                Clip { start: beat, src_in: after.src_in + by, len: after.len - by, ..after },
            );
            let set =
                |old: Clip, new: Clip| Op::SetClip { seq: sequence.id, track: track.id, start: old.start, clip: new };
            // Make room first, then fill it.
            ops.extend(if by > 0 {
                [set(after, later), set(before, longer)]
            } else {
                [set(before, longer), set(after, later)]
            });
            (clips[i - 1], clips[i]) = (longer, later);
            // Keyframes of the clip after the cut stay where they were on the timeline.
            if let Some(props) = track.props_of(after.id) {
                let mut moved = ClipProps::clone(props);
                moved.picture_params().into_iter().for_each(|p| p.shift_keys(-by));
                if moved != **props {
                    ops.push(Op::SetClipProps {
                        seq: sequence.id,
                        track: track.id,
                        clip: after.id,
                        props: Some(Box::new(moved)),
                    });
                }
            }
        }
        let moved = ops.iter().filter(|op| matches!(op, Op::SetClip { .. })).count() / 2;
        if moved > 0 {
            match session.apply(Op::Batch(ops), None) {
                Ok(()) => self.selection.clear(),
                Err(e) => {
                    *error = Some(e.to_string());
                    return 0;
                }
            }
        }
        moved
    }

    /// Puts a whip-pan on every cut of the programme track that has no
    /// transition yet, left and right in turn. Returns how many. One undo step.
    pub fn whip_every_cut(&mut self, session: &mut Session, error: &mut Option<String>) -> usize {
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else { return 0 };
        let Some(track) = crate::session::program_row(sequence).map(|row| sequence.tracks[row].clone()) else {
            return 0;
        };
        let clips: Vec<Clip> = track.clips.iter().collect();
        let mut ops = Vec::new();
        let mut angle = 0.0f32;
        for pair in clips.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            if before.end() != after.start {
                continue;
            }
            let mut props = track.props_of(after.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
            match props.transition {
                // Carry on from the direction of the ones that are there.
                Some(existing) => angle = (existing.angle + 180.0).rem_euclid(360.0),
                None => {
                    props.transition = Some(sg_core::Transition { angle, ..Default::default() });
                    ops.push(Op::SetClipProps {
                        seq: sequence.id,
                        track: track.id,
                        clip: after.id,
                        props: Some(Box::new(props)),
                    });
                    angle = (angle + 180.0).rem_euclid(360.0);
                }
            }
        }
        let count = ops.len();
        if count > 0
            && let Err(e) = session.apply(Op::Batch(ops), None)
        {
            *error = Some(e.to_string());
            return 0;
        }
        count
    }
}

/// Beat positions for a tempo choice: as found (0), with the beats between
/// them (1, 2: doubled, quadrupled) or only every second or fourth (-1, -2).
fn grid_of(beats: &[i64], tempo: i8) -> Vec<i64> {
    match tempo {
        t if t > 0 => {
            let parts = 1i64 << t.min(2);
            let mut out = Vec::with_capacity(beats.len() * parts as usize);
            for pair in beats.windows(2) {
                out.extend((0..parts).map(|k| pair[0] + (pair[1] - pair[0]) * k / parts));
            }
            out.extend(beats.last());
            out
        }
        t if t < 0 => beats.iter().step_by(1 << (-t).min(2)).copied().collect(),
        _ => beats.to_vec(),
    }
}

impl Timeline {
    /// Keeps the grid in step with the sequence's song and tempo choice.
    fn follow_song(&mut self, sequence: &Sequence) {
        let Some(song) = sequence.song else {
            self.song_beats = None;
            self.grid_for = None;
            self.grid = Arc::default();
            return;
        };
        if self.song_beats.as_ref().is_none_or(|(id, _)| *id != song.asset) {
            self.song_beats = Some((song.asset, None));
            self.beat_request = Some(song.asset);
        }
        if self.grid_for != Some((song.asset, song.tempo))
            && let Some((_, Some(beats))) = &self.song_beats
        {
            self.grid = Arc::new(grid_of(&beats.beats, song.tempo));
            self.grid_for = Some((song.asset, song.tempo));
        }
    }

    /// The analysed beats of an asset arrived (`None`: it has no usable rhythm).
    pub fn set_beats(&mut self, id: AssetId, beats: Option<Beats>) {
        if let Some((song, slot)) = &mut self.song_beats
            && *song == id
        {
            *slot = beats.map(Arc::new);
            self.grid_for = None;
        }
    }

    /// Tempo of the song as the grid shows it, in beats per minute.
    pub fn bpm(&self, sequence: &Sequence) -> Option<f32> {
        let song = sequence.song?;
        let (_, beats) = self.song_beats.as_ref().filter(|(id, _)| *id == song.asset)?;
        Some(beats.as_ref()?.bpm * 2f32.powi(song.tempo as i32))
    }

    /// The clips of the song on audio tracks that touch `f0..f1` (frames).
    fn song_clips(&self, sequence: &Sequence, f0: f64, f1: f64) -> Vec<(Clip, f64)> {
        let Some(song) = sequence.song else { return Vec::new() };
        let mut found = Vec::new();
        for track in sequence.tracks.iter().filter(|t| t.kind == TrackKind::Audio) {
            let per = Lane::of(track, sequence).per_frame();
            let range = track.clips.range((f0 * per).floor() as i64, (f1 * per).ceil() as i64);
            // A view crowded with thousands of clips is not searched for the song.
            found.extend(range.take(64).filter(|c| c.asset == song.asset).map(|c| (c, per)));
        }
        found
    }

    /// Grid lines between frames `f0` and `f1`, at least `min_points` apart
    /// on screen: (frame, running beat number).
    fn grid_lines(&self, sequence: &Sequence, f0: f64, f1: f64, min_points: f32) -> Vec<(f64, usize)> {
        if self.grid.len() < 2 {
            return Vec::new();
        }
        let mut lines = Vec::new();
        for (clip, per) in self.song_clips(sequence, f0, f1) {
            // Thin the grid out by powers of two until the lines have room.
            let gap = (self.grid[1] - self.grid[0]) as f64 / per / self.frames_per_point;
            let mut stride = 1usize;
            while gap * (stride as f64) < min_points as f64 && stride < 1 << 12 {
                stride *= 2;
            }
            // Song samples visible through this clip.
            let from = clip.src_in + ((f0 * per) as i64 - clip.start).max(0);
            let to = clip.src_in + ((f1 * per) as i64 - clip.start).min(clip.len);
            let first = self.grid.partition_point(|b| *b < from).next_multiple_of(stride);
            for index in (first..self.grid.len()).step_by(stride) {
                let beat = self.grid[index];
                if beat >= to {
                    break;
                }
                lines.push(((clip.start + beat - clip.src_in) as f64 / per, index / stride));
            }
        }
        lines
    }

    /// The grid line nearest to `frame`, for snapping.
    fn nearest_beat(&self, sequence: &Sequence, frame: f64) -> Option<f64> {
        let (clip, per) = self.song_clips(sequence, frame - 1.0, frame + 1.0).into_iter().next()?;
        let at = clip.src_in + (frame * per) as i64 - clip.start;
        let after = self.grid.partition_point(|b| *b < at);
        let near = [after.checked_sub(1), Some(after)]
            .into_iter()
            .flatten()
            .filter_map(|i| self.grid.get(i))
            .min_by_key(|b| (**b - at).abs())?;
        Some((clip.start + near - clip.src_in) as f64 / per)
    }
}

#[cfg(test)]
mod tests {
    use sg_core::{AssetId, AssetKind, AudioInfo, Source, VideoInfo};

    use super::*;

    /// A take with picture and sound: 10 s at 60 fps, 48 kHz.
    fn take(id: u128) -> Asset {
        Asset {
            id: AssetId(id),
            blake3: format!("{id:064x}"),
            size: 1,
            file: format!("assets/video/take-{id}.mp4"),
            kind: AssetKind::Video,
            label: format!("take {id}"),
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

    /// A project with two takes placed back to back (frames 0..600 and 600..1200).
    fn session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().unwrap();
        let root = sg_project::create(dir.path(), "T").unwrap().path;
        let mut session = Session::new(sg_project::open(&root, || {}).unwrap());
        for id in [1, 2] {
            let asset = take(id);
            session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
            // The playhead is at 0: the first take lands there, the second
            // finds it taken and goes to the end.
            session.place_asset(&asset, 0, None).unwrap();
        }
        (dir, session)
    }

    type Spans = Vec<(i64, i64)>;

    /// (start, length) of every clip, video in frames and audio in frames too.
    fn layout(session: &Session) -> (Spans, Spans) {
        let project = session.project();
        let seq = &project.sequences[0];
        let v = seq.tracks[0].clips.iter().map(|c| (c.start, c.len)).collect();
        let a = seq.tracks[1].clips.iter().map(|c| (c.start / 800, c.len / 800)).collect();
        (v, a)
    }

    fn act(timeline: &mut Timeline, session: &mut Session, action: Action, playhead: i64) -> Option<i64> {
        let mut error = None;
        let seek = timeline.act(action, session, playhead, &mut error);
        assert_eq!(error, None, "{action:?}");
        seek
    }

    #[test]
    fn split_cuts_picture_and_sound_together() {
        let (_dir, mut session) = session();
        let mut timeline = Timeline::default();
        act(&mut timeline, &mut session, Action::Split, 200);
        assert_eq!(layout(&session), (vec![(0, 200), (200, 400), (600, 600)], vec![(0, 200), (200, 400), (600, 600)]));
        // The right halves continue where the left ones stop in the source.
        let project = session.project();
        let right = project.sequences[0].tracks[0].clips.get(200).unwrap();
        assert_eq!(right.src_in, 200);
        assert_eq!(project.sequences[0].tracks[1].clips.get(200 * 800).unwrap().src_in, 200 * 800);
        // On a cut: nothing to split.
        act(&mut timeline, &mut session, Action::Split, 600);
        assert_eq!(layout(&session).0.len(), 3);
        // One undo step per split.
        session.undo().unwrap();
        assert_eq!(layout(&session).0, [(0, 600), (600, 600)]);
    }

    #[test]
    fn ripple_trims_keep_both_tracks_in_sync() {
        let (_dir, mut session) = session();
        let mut timeline = Timeline::default();
        // Q at 2 s: the first two seconds go, everything moves left.
        let seek = act(&mut timeline, &mut session, Action::RippleStart, 120);
        assert_eq!(seek, Some(0));
        assert_eq!(layout(&session), (vec![(0, 480), (480, 600)], vec![(0, 480), (480, 600)]));
        assert_eq!(session.project().sequences[0].tracks[0].clips.get(0).unwrap().src_in, 120);
        // E at 5 s of the timeline (inside the second take): its tail goes.
        act(&mut timeline, &mut session, Action::RippleEnd, 540);
        assert_eq!(layout(&session), (vec![(0, 480), (480, 60)], vec![(0, 480), (480, 60)]));
        session.undo().unwrap();
        session.undo().unwrap();
        assert_eq!(layout(&session).0, [(0, 600), (600, 600)]);
    }

    #[test]
    fn selecting_a_clip_takes_its_partner_and_ripple_delete_closes_the_gap() {
        let (_dir, mut session) = session();
        let mut timeline = Timeline::default();
        let project = session.project().clone();
        let seq = &project.sequences[0];
        let first = seq.tracks[0].clips.get(0).unwrap();
        timeline.select(seq, &seq.tracks[0], &first, false, false);
        assert_eq!(timeline.selection.len(), 2, "picture and sound of the take");
        act(&mut timeline, &mut session, Action::RippleDelete, 0);
        assert_eq!(layout(&session), (vec![(0, 600)], vec![(0, 600)]));
        assert!(!timeline.has_selection());

        // Alone: only the clicked clip; delete leaves a gap.
        let project = session.project().clone();
        let seq = &project.sequences[0];
        let clip = seq.tracks[0].clips.get(0).unwrap();
        timeline.select(seq, &seq.tracks[0], &clip, false, true);
        assert_eq!(timeline.selection.len(), 1);
        act(&mut timeline, &mut session, Action::Delete, 0);
        assert_eq!(layout(&session), (vec![], vec![(0, 600)]));
    }

    #[test]
    fn copy_paste_markers_and_jumps() {
        let (_dir, mut session) = session();
        let mut timeline = Timeline::default();
        act(&mut timeline, &mut session, Action::SelectUnder, 700);
        assert_eq!(timeline.selection.len(), 1);
        act(&mut timeline, &mut session, Action::Copy, 700);
        act(&mut timeline, &mut session, Action::PasteClips, 1500);
        let (video, audio) = layout(&session);
        // W picked the lowest track under the playhead: the audio clip.
        assert_eq!((video.len(), audio), (2, vec![(0, 600), (600, 600), (1500, 600)]));
        // Pasting onto something is refused, with a message.
        let mut error = None;
        timeline.act(Action::PasteClips, &mut session, 1600, &mut error);
        assert_eq!(error.as_deref(), Some("No room to paste here."));

        act(&mut timeline, &mut session, Action::Marker, 300);
        assert_eq!(session.project().sequences[0].markers.len(), 1);
        assert_eq!(act(&mut timeline, &mut session, Action::NextCut, 0), Some(300));
        assert_eq!(act(&mut timeline, &mut session, Action::NextCut, 300), Some(600));
        assert_eq!(act(&mut timeline, &mut session, Action::PrevCut, 650), Some(600));
        assert_eq!(act(&mut timeline, &mut session, Action::PrevCut, 5), Some(0));
        // G again on the same frame removes the marker.
        act(&mut timeline, &mut session, Action::Marker, 300);
        assert!(session.project().sequences[0].markers.is_empty());
    }

    #[test]
    fn moving_and_trimming_plan_ops_that_fit() {
        let (_dir, mut session) = session();
        let mut timeline = Timeline { frames_per_point: 1.0, ..Timeline::default() };
        let project = session.project().clone();
        let seq = project.sequences[0].clone();
        let view = ViewerState::default();
        let second = seq.tracks[0].clips.get(600).unwrap();
        timeline.select(&seq, &seq.tracks[0], &second, false, false);
        let primary = timeline.selection[0];

        // Onto the first take: no room.
        timeline.plan_move(&seq, &view, -300.0, 0, primary, false);
        assert!(timeline.pending.is_none());
        assert!(timeline.ghosts.iter().any(|g| !g.3));
        // To the right by 100 frames: both clips move, as one undo step.
        timeline.plan_move(&seq, &view, 100.0, 0, primary, false);
        let ops = timeline.pending.take().unwrap();
        session.apply(Op::Batch(ops), None).unwrap();
        assert_eq!(layout(&session), (vec![(0, 600), (700, 600)], vec![(0, 600), (700, 600)]));
        // Snapping pulls a near miss onto the neighbour's edge.
        let seq = session.project().sequences[0].clone();
        let moved = seq.tracks[0].clips.get(700).unwrap();
        timeline.select(&seq, &seq.tracks[0], &moved, false, false);
        timeline.plan_move(&seq, &view, -96.0, 0, timeline.selection[0], true);
        session.apply(Op::Batch(timeline.pending.take().unwrap()), None).unwrap();
        assert_eq!(layout(&session).0, [(0, 600), (600, 600)]);

        // Trim the end of the first take to frame 400; the sound follows.
        let seq = session.project().sequences[0].clone();
        let first = seq.tracks[0].clips.get(0).unwrap();
        timeline.select(&seq, &seq.tracks[0], &first, false, false);
        let sel = timeline.selection[0];
        timeline.plan_trim(&session, &seq, &view, 400.0, sel, Edge::End, false, false);
        session.apply(Op::Batch(timeline.pending.take().unwrap()), None).unwrap();
        assert_eq!(layout(&session), (vec![(0, 400), (600, 600)], vec![(0, 400), (600, 600)]));
        // It cannot be dragged out beyond the end of its source or into the neighbour.
        let seq = session.project().sequences[0].clone();
        let first = seq.tracks[0].clips.get(0).unwrap();
        timeline.select(&seq, &seq.tracks[0], &first, false, false);
        timeline.plan_trim(&session, &seq, &view, 5000.0, timeline.selection[0], Edge::End, false, false);
        session.apply(Op::Batch(timeline.pending.take().unwrap()), None).unwrap();
        assert_eq!(layout(&session).0, [(0, 600), (600, 600)]);
        // A rippling head trim keeps the clip in place and pulls the rest in.
        let seq = session.project().sequences[0].clone();
        let first = seq.tracks[0].clips.get(0).unwrap();
        timeline.select(&seq, &seq.tracks[0], &first, false, false);
        timeline.plan_trim(&session, &seq, &view, 150.0, timeline.selection[0], Edge::Start, true, false);
        session.apply(Op::Batch(timeline.pending.take().unwrap()), None).unwrap();
        assert_eq!(layout(&session), (vec![(0, 450), (450, 600)], vec![(0, 450), (450, 600)]));
    }

    /// (kind, name, clip starts in frames) of every track, top first.
    fn tracks(session: &Session) -> Vec<(TrackKind, String, Vec<i64>)> {
        let project = session.project();
        let seq = &project.sequences[0];
        let starts = |t: &Track| t.clips.iter().map(|c| Lane::of(t, seq).frame(c.start)).collect();
        seq.tracks.iter().map(|t| (t.kind, t.name.clone(), starts(t))).collect()
    }

    #[test]
    fn drop_on_free_space_uses_the_tracks_that_exist() {
        let (_dir, mut session) = session();
        let asset = take(3);
        session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
        assert_eq!(session.drop_asset(&asset, 1500, Some(0), None).unwrap(), Some(1500));
        assert_eq!(
            tracks(&session),
            vec![
                (TrackKind::Video, "V1".to_owned(), vec![0, 600, 1500]),
                (TrackKind::Audio, "A1".to_owned(), vec![0, 600, 1500]),
            ]
        );
    }

    #[test]
    fn drop_on_taken_space_makes_tracks_and_undoes_in_one_step() {
        let (_dir, mut session) = session();
        let before = session.project().clone();
        let asset = take(3);
        session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
        let with_asset = session.project().clone();
        // Over the first take, on the video track, and again outside every track.
        for row in [Some(0), None] {
            let seq = session.project().sequences[0].clone();
            let planned = place_at(&seq, &asset, 100, row, None);
            assert_eq!((planned.video, planned.audio), (Some(Slot::New), Some(Slot::New)));
            session.drop_asset(&asset, 100, row, None).unwrap();
            assert_eq!(
                tracks(&session),
                vec![
                    (TrackKind::Video, "V2".to_owned(), vec![100]),
                    (TrackKind::Video, "V1".to_owned(), vec![0, 600]),
                    (TrackKind::Audio, "A1".to_owned(), vec![0, 600]),
                    (TrackKind::Audio, "A2".to_owned(), vec![100]),
                ]
            );
            assert!(session.undo().unwrap());
            assert_eq!(*session.project(), with_asset);
        }
        assert!(session.undo().unwrap());
        assert_eq!(*session.project(), before);
    }

    #[test]
    fn drop_stacks_on_a_free_track_before_making_one() {
        let (_dir, mut session) = session();
        session.add_track(TrackKind::Video).unwrap();
        let asset = take(3);
        session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
        // Dropped on the taken V1 (row 1 now): the picture goes up to the free V2.
        let seq = session.project().sequences[0].clone();
        let planned = place_at(&seq, &asset, 100, Some(1), None);
        assert_eq!((planned.video, planned.audio), (Some(Slot::Row(0)), Some(Slot::New)));
        // A sound-only asset dropped on a video track still finds an audio track.
        let mut sound = take(4);
        sound.video = None;
        let planned = place_at(&seq, &sound, 2000, Some(0), None);
        assert_eq!((planned.video, planned.audio), (None, Some(Slot::Row(2))));
    }

    #[test]
    fn drop_target_maps_points_to_frames_and_rows() {
        let (_dir, session) = session();
        let seq = session.project().sequences[0].clone();
        let rect = Rect::from_min_size(pos2(0.0, 500.0), egui::vec2(1000.0, 200.0));
        let mut timeline = Timeline { rect, row_h: 40.0, ..Timeline::default() };
        timeline.set_view(2.0, 0.0);
        let x = HEADER_W + 400.0;
        assert_eq!(timeline.drop_target(&seq, 0, pos2(x, 400.0), false), None);
        assert_eq!(timeline.drop_target(&seq, 0, pos2(x, 505.0), false), Some((800, None)));
        assert_eq!(timeline.drop_target(&seq, 0, pos2(x, 500.0 + RULER_H + 50.0), false), Some((800, Some(1))));
        assert_eq!(timeline.drop_target(&seq, 0, pos2(x, 690.0), false), Some((800, None)));
        // Near the end of the first take (frame 600), snapping pulls it there.
        assert_eq!(timeline.drop_target(&seq, 0, pos2(HEADER_W + 303.0, 560.0), true), Some((600, Some(0))));
    }

    /// One second of a full-scale square wave, then one second at a quarter.
    fn square_peaks() -> Peaks {
        let mut sent = false;
        Peaks::build(48_000, 1, |buf| {
            if std::mem::replace(&mut sent, true) {
                return Ok(false);
            }
            buf.extend((0..96_000).map(|i| {
                let level = if i < 48_000 { 1.0 } else { 0.25 };
                if (i / 100) % 2 == 0 { level } else { -level }
            }));
            Ok(true)
        })
        .unwrap()
    }

    #[test]
    fn waveform_has_one_column_per_pixel_whatever_the_zoom() {
        let peaks = square_peaks();
        let clip = Clip { id: ClipId(1), asset: AssetId(1), start: 0, len: 96_000, src_in: 0 };
        let span = WaveSpan { clip: &clip, speed: 1.0, y: (0.0, 40.0) };
        // 200 points wide at 480 samples per point, then zoomed out tenfold.
        for (width, per_point) in [(200.0f32, 480.0f64), (20.0, 4800.0)] {
            let mut columns = Vec::new();
            wave_columns(
                &peaks,
                &span,
                (0.0, width),
                1.0,
                &|x| x as f64 * per_point,
                per_point,
                &mut |x, top, bottom| columns.push((x, top, bottom)),
            );
            assert_eq!(columns.len(), width as usize);
            // Loud half: the full height. Quiet half: a quarter of it.
            let (_, top, bottom) = columns[columns.len() / 4];
            assert!(top < 1.0 && bottom > 39.0, "{top} {bottom}");
            let (_, top, bottom) = columns[columns.len() * 3 / 4];
            assert!((top - 15.0).abs() < 1.0 && (bottom - 25.0).abs() < 1.0, "{top} {bottom}");
        }
        // A clip that starts later in its source shows that part.
        let late = Clip { src_in: 48_000, len: 48_000, ..clip };
        let span = WaveSpan { clip: &late, speed: 1.0, y: (0.0, 40.0) };
        // (The very first column still touches the block that ends the loud half.)
        let mut tops = Vec::new();
        wave_columns(&peaks, &span, (0.0, 100.0), 1.0, &|x| x as f64 * 480.0, 480.0, &mut |_, top, _| {
            tops.push(top);
        });
        assert!((tops[5] - 15.0).abs() < 1.0, "{tops:?}");
    }

    #[test]
    fn a_drop_can_take_only_the_sound_or_only_the_picture() {
        let (_dir, mut session) = session();
        let asset = take(3);
        session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
        session.drop_asset(&asset, 1500, Some(1), Some(TrackKind::Audio)).unwrap();
        session.drop_asset(&asset, 2500, Some(0), Some(TrackKind::Video)).unwrap();
        assert_eq!(
            tracks(&session),
            vec![
                (TrackKind::Video, "V1".to_owned(), vec![0, 600, 2500]),
                (TrackKind::Audio, "A1".to_owned(), vec![0, 600, 1500]),
            ]
        );
    }

    /// The song (sound only, 60 s) on A1 from frame 0, and nothing else.
    fn with_song() -> (tempfile::TempDir, Session, Asset) {
        let dir = tempfile::tempdir().unwrap();
        let root = sg_project::create(dir.path(), "T").unwrap().path;
        let mut session = Session::new(sg_project::open(&root, || {}).unwrap());
        let mut song = take(50);
        song.video = None;
        song.audio.as_mut().unwrap().samples = 48_000 * 60;
        session.apply(Op::AddAsset { asset: Box::new(song.clone()) }, None).unwrap();
        session.set_song(&song).unwrap();
        (dir, session, song)
    }

    #[test]
    fn a_synced_take_lands_in_time_on_a_camera_track_and_fills_the_programme() {
        use crate::session::{angle_rows, program_row};
        let (_dir, mut session, song) = with_song();
        assert_eq!(session.project().sequences[0].song.map(|s| s.asset), Some(song.id));
        assert_eq!(tracks(&session)[1], (TrackKind::Audio, "A1".to_owned(), vec![0]));
        let before = session.project().clone();
        // Found 12.5 s into the song: frame 750 at 60 fps.
        let first = take(1);
        session.apply(Op::AddAsset { asset: Box::new(first.clone()) }, None).unwrap();
        assert_eq!(session.place_synced(&first, 48_000 * 25 / 2).unwrap(), 750);
        // A second one that began 2 s before the song: its first 120 frames are cut off.
        let second = take(2);
        session.apply(Op::AddAsset { asset: Box::new(second.clone()) }, None).unwrap();
        assert_eq!(session.place_synced(&second, -48_000 * 2).unwrap(), 0);
        assert_eq!(
            tracks(&session),
            vec![
                (TrackKind::Video, "V1".to_owned(), vec![0, 750]),
                (TrackKind::Video, "Cam 1".to_owned(), vec![750]),
                (TrackKind::Video, "Cam 2".to_owned(), vec![0]),
                (TrackKind::Audio, "A1".to_owned(), vec![0]),
            ]
        );
        let project = session.project().clone();
        let seq = &project.sequences[0];
        assert_eq!((program_row(seq), angle_rows(seq)), (Some(0), vec![1, 2]));
        let cam2 = seq.tracks[2].clips.at(0).unwrap();
        assert_eq!((cam2.src_in, cam2.len), (120, 480));
        // The programme got the second take only where the first left room.
        let programme: Vec<(i64, i64)> = seq.tracks[0].clips.iter().map(|c| (c.start, c.len)).collect();
        assert_eq!(programme, vec![(0, 480), (750, 600)]);
        // Each placement is one undo step (plus the two imports).
        for _ in 0..4 {
            assert!(session.undo().unwrap());
        }
        assert_eq!(*session.project(), before);
    }

    #[test]
    fn switching_angles_cuts_the_programme_from_the_playhead() {
        let (_dir, mut session, _song) = with_song();
        for (id, lag) in [(1, 0i64), (2, 48_000)] {
            let asset = take(id);
            session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
            session.place_synced(&asset, lag).unwrap();
        }
        // Programme: take 1 on 0..600. Cam 2 holds take 2 on 60..660.
        let programme = |session: &Session| -> Vec<(u128, i64, i64, i64)> {
            session.project().sequences[0].tracks[0]
                .clips
                .iter()
                .map(|c| (c.asset.0, c.start, c.len, c.src_in))
                .collect()
        };
        assert_eq!(programme(&session), vec![(1, 0, 600, 0)]);
        let group = Some(session.new_group());
        assert!(session.switch_angle(1, 200, group).unwrap());
        assert_eq!(programme(&session), vec![(1, 0, 200, 0), (2, 200, 460, 140)]);
        assert!(session.switch_angle(0, 300, group).unwrap());
        // (Take 1 ends at 600; behind it the programme keeps what it had: take 2.)
        assert_eq!(
            programme(&session),
            vec![(1, 0, 200, 0), (2, 200, 100, 140), (1, 300, 300, 300), (2, 600, 60, 540)]
        );
        // Asking for what is already shown changes nothing; no such angle says so.
        let unchanged = session.project().clone();
        assert!(session.switch_angle(0, 400, group).unwrap());
        assert_eq!(*session.project(), unchanged);
        assert!(!session.switch_angle(5, 400, group).unwrap());
        assert!(!session.switch_angle(1, 2000, group).unwrap());
        // One pass of switching is one undo step.
        assert!(session.undo().unwrap());
        assert_eq!(programme(&session), vec![(1, 0, 600, 0)]);
    }

    #[test]
    fn splitting_a_clip_shares_out_its_properties() {
        use sg_core::{Key, Param};
        let (_dir, mut session) = session();
        let project = session.project().clone();
        let seq = &project.sequences[0];
        let (track, clip) = (seq.tracks[0].clone(), seq.tracks[0].clips.at(0).unwrap());
        // Moving across the frame over the whole clip, slowing down to half
        // speed, fading in and out.
        let props = ClipProps {
            x: Param { value: 0.0, keys: vec![Key::new(0, -0.5), Key::new(600, 0.5)] },
            ramp: Param { value: 1.0, keys: vec![Key::new(0, 1.0), Key::new(600, 0.5)] },
            fade_in: 30,
            fade_out: 40,
            ..ClipProps::default()
        };
        let before_at = |frame: i64| (props.x.at(frame), props.source_offset(frame));
        let set =
            Op::SetClipProps { seq: seq.id, track: track.id, clip: clip.id, props: Some(Box::new(props.clone())) };
        session.apply(set, None).unwrap();
        let mut timeline = Timeline::default();
        timeline.select_only(track.id, &clip);
        act(&mut timeline, &mut session, Action::Split, 200);
        let project = session.project().clone();
        let track = &project.sequences[0].tracks[0];
        let (left, right) = (track.clips.at(0).unwrap(), track.clips.at(200).unwrap());
        let (lp, rp) = (track.props_of(left.id).unwrap(), track.props_of(right.id).unwrap());
        assert_eq!((left.len, right.start, right.len), (200, 200, 400));
        assert_eq!((lp.fade_in, lp.fade_out, rp.fade_in, rp.fade_out), (30, 0, 0, 40));
        // Every frame shows what it showed before the cut: same place, same source frame.
        for frame in [0, 100, 199] {
            assert_eq!((lp.x.at(frame), left.src_in + lp.source_offset(frame)), before_at(frame));
        }
        for frame in [200, 201, 450, 599] {
            let (x, source) = (rp.x.at(frame - 200), right.src_in + rp.source_offset(frame - 200));
            let (was_x, was_source) = before_at(frame);
            assert!(
                (x - was_x).abs() < 1e-5 && (source - was_source).abs() <= 1,
                "frame {frame}: {x} {source} vs {was_x} {was_source}"
            );
        }
        // One undo step.
        assert!(session.undo().unwrap());
        assert_eq!(session.project().sequences[0].tracks[0].clips.at(0).unwrap().len, 600);
    }

    #[test]
    fn cuts_move_onto_beats_without_losing_sync_and_get_whips() {
        let (_dir, mut session, _song) = with_song();
        for (id, lag) in [(1, 0i64), (2, 0)] {
            let asset = take(id);
            session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
            session.place_synced(&asset, lag).unwrap();
        }
        // Cuts by hand, a little off the beats: at frames 97, 208 and 331.
        for (angle, frame) in [(1, 97), (0, 208), (1, 331)] {
            session.switch_angle(angle, frame, None).unwrap();
        }
        let programme = |session: &Session| -> Vec<(u128, i64, i64, i64)> {
            session.project().sequences[0].tracks[0]
                .clips
                .iter()
                .map(|c| (c.asset.0, c.start, c.len, c.src_in))
                .collect()
        };
        assert_eq!(programme(&session), vec![(1, 0, 97, 0), (2, 97, 111, 97), (1, 208, 123, 208), (2, 331, 269, 331)]);
        // A beat every half second: every 30 frames.
        let mut timeline = Timeline { grid: Arc::new((0..120).map(|n| n * 24_000).collect()), ..Timeline::default() };
        let mut error = None;
        assert_eq!(timeline.cuts_to_beats(&mut session, &mut error), 3);
        assert_eq!(error, None);
        // 97 -> 90, 208 -> 210, 331 -> 330; every clip still shows the frame of
        // its take that belongs at that moment (source position = start).
        assert_eq!(programme(&session), vec![(1, 0, 90, 0), (2, 90, 120, 90), (1, 210, 120, 210), (2, 330, 270, 330)]);
        assert_eq!(timeline.cuts_to_beats(&mut session, &mut error), 0);
        // A whip on each of the three cuts, left and right in turn.
        assert_eq!(timeline.whip_every_cut(&mut session, &mut error), 3);
        let project = session.project().clone();
        let track = &project.sequences[0].tracks[0];
        let angles: Vec<f32> = track
            .clips
            .iter()
            .filter_map(|c| track.props_of(c.id).and_then(|p| p.transition).map(|t| t.angle))
            .collect();
        assert_eq!(angles, [0.0, 180.0, 0.0]);
        assert_eq!(timeline.whip_every_cut(&mut session, &mut error), 0);
        // Two undo steps bring the hand-made cuts back.
        assert!(session.undo().unwrap() && session.undo().unwrap());
        assert_eq!(programme(&session)[1], (2, 97, 111, 97));
    }
    /// The whole gesture through egui's own input handling: press on an
    /// asset tile, move over the timeline, let go.
    #[test]
    fn dragging_an_asset_tile_onto_the_timeline_places_it() {
        use egui::{Event, Modifiers, vec2};

        let (_dir, mut session) = session();
        let asset = take(3);
        session.apply(Op::AddAsset { asset: Box::new(asset.clone()) }, None).unwrap();
        let ctx = egui::Context::default();
        let mut assets = crate::assets::AssetsPanel::default();
        let mut timeline = Timeline::default();
        let view = ViewerState::default();
        let mut frame = |events: Vec<Event>, session: &mut Session, timeline: &mut Timeline| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 800.0))),
                events,
                ..Default::default()
            };
            let (mut transport, mut error, mut requests) = (Vec::new(), None, Vec::new());
            let mut output = ctx.run_ui(input, |ui| {
                let project = session.project().clone();
                egui::Panel::bottom("timeline")
                    .default_size(230.0)
                    .show(ui, |ui| timeline.ui(ui, session, &view, &mut transport, &mut error));
                egui::Panel::left("assets").default_size(320.0).show(ui, |ui| assets.ui(ui, &project, &mut requests));
            });
            // No renderer takes the font atlas here.
            output.textures_delta.clear();
            assert_eq!(error, None);
            assets.selected
        };
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        // The newest asset is the first tile, below the panel's header rows.
        let tile = pos2(60.0, 120.0);
        let over = pos2(700.0, 800.0 - 230.0 + RULER_H + 60.0);
        frame(vec![], &mut session, &mut timeline);
        frame(vec![Event::PointerMoved(tile)], &mut session, &mut timeline);
        frame(vec![button(tile, true)], &mut session, &mut timeline);
        frame(vec![Event::PointerMoved(tile + vec2(30.0, 30.0))], &mut session, &mut timeline);
        let selected = frame(vec![Event::PointerMoved(over)], &mut session, &mut timeline);
        assert_eq!(selected, Some(asset.id), "the press must have landed on the tile of the newest asset");
        frame(vec![Event::PointerMoved(over)], &mut session, &mut timeline);
        let ghost = timeline.drop_ghost.expect("a ghost shows where the asset would land");
        let before = tracks(&session);
        frame(vec![button(over, false)], &mut session, &mut timeline);
        let project = session.project().clone();
        let seq = &project.sequences[0];
        let placed: Vec<(TrackKind, i64)> = seq
            .tracks
            .iter()
            .flat_map(|t| {
                t.clips.iter().filter(|c| c.asset == asset.id).map(|c| (t.kind, Lane::of(t, seq).frame(c.start)))
            })
            .collect();
        assert_eq!(placed, vec![(TrackKind::Video, ghost.start), (TrackKind::Audio, ghost.start)], "was {before:?}");
        assert_eq!(timeline.selection.len(), 2);
        // Letting go ends the drag: nothing more is placed on the next frames.
        frame(vec![Event::PointerMoved(over)], &mut session, &mut timeline);
        assert!(timeline.drop_ghost.is_none());
        assert!(session.undo().unwrap());
        assert_eq!(tracks(&session), before);
    }
}
