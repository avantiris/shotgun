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

/// A recording while it is made: the audio track it goes onto, the
/// timeline samples it runs from and to so far, and its level, one peak
/// (linear) for every [`LiveTake::BUCKET`] samples.
pub struct LiveTake {
    pub track: TrackId,
    pub from: i64,
    pub to: i64,
    pub peaks: Vec<f32>,
}

impl LiveTake {
    /// A hundredth of a second: finer than a column of the timeline at any useful zoom.
    pub const BUCKET: i64 = 480;

    /// The playback has come to timeline sample `now`, and the input was
    /// this loud since the last call.
    pub fn heard(&mut self, now: i64, level: f32) {
        if now <= self.from {
            return;
        }
        let bucket = ((now - self.from) / Self::BUCKET) as usize;
        if self.peaks.len() <= bucket {
            self.peaks.resize(bucket + 1, level);
        } else if let Some(last) = self.peaks.last_mut() {
            *last = last.max(level);
        }
        self.to = self.to.max(now);
    }
}

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
    /// Pulling the fade mark at a top corner of a selected clip: the fade
    /// in (start) or out (end) of every selected clip follows, as one
    /// undo step.
    Fade {
        sel: Sel,
        edge: Edge,
        group: u64,
    },
    /// A box pulled open over empty timeline: the clips it touches are
    /// selected (with Shift: added to what was selected before, kept here).
    Marquee {
        from: egui::Pos2,
        before: Vec<Sel>,
    },
    /// Pulling the volume line of a sound clip up or down; the other
    /// selected sound clips change by as much. One undo step.
    Gain {
        sel: Sel,
        group: u64,
    },
}

/// A clip on the internal clipboard, relative to the first copied frame.
struct Copied {
    row: usize,
    clip: Clip,
    /// Offset from the earliest copied clip, in the clip's track units.
    offset: i64,
    /// What the clip had been given: volume, fades, placement, title.
    props: Option<Box<ClipProps>>,
    /// The notes of a note clip.
    notes: Option<Vec<sg_core::Note>>,
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
    /// With `pending` holding the removals of a move: the clips to put
    /// down over whatever is in their place (a move with Ctrl held).
    pending_over: Vec<(TrackId, Clip)>,
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
    /// What the grid of a sequence with a tempo is made of.
    pub grid_step: GridStep,
    /// The audio track recordings go onto ("R" in its header), if one is marked.
    pub record_track: Option<TrackId>,
    /// The track whose name is being typed (a double click on it, or its
    /// menu), the text so far, and whether the field still has to take the keyboard.
    renaming: Option<(TrackId, String, bool)>,
    /// The recording in progress, drawn where it will lie (feedback F16.10).
    pub live_take: Option<LiveTake>,
    /// The stretch that is played over and over, in timeline samples.
    pub loop_range: Option<(i64, i64)>,
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
    /// Undo group of the volume field in the clip menu while that menu is open.
    menu_group: Option<u64>,
    /// Actions chosen in the menus this frame; the shell runs them like keys.
    pub actions: Vec<Action>,
    /// The selected title's text was asked for (double click, or its menu); taken by the editor.
    pub edit_title: bool,
    /// A note clip was asked for its notes (double click, or just made); taken by the editor.
    pub edit_notes: Option<(TrackId, ClipId)>,
    /// The sequence frame the context menu was opened at.
    menu_frame: f64,
    /// The volume being pulled or pointed at: where to write it, and its decibels.
    gain_label: Option<(egui::Pos2, f32)>,
    /// The selection box being pulled open, to draw it.
    marquee: Option<Rect>,
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
            pending_over: Vec::new(),
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
            menu_group: None,
            actions: Vec::new(),
            edit_title: false,
            edit_notes: None,
            menu_frame: 0.0,
            gain_label: None,
            marquee: None,
            menu_clip: None,
            scenario_request: None,
            waves: HashMap::new(),
            thumbs: Vec::new(),
            lanes: Rect::NOTHING,
            song_beats: None,
            grid: Arc::default(),
            grid_for: None,
            beat_request: None,
            grid_step: GridStep::Auto,
            record_track: None,
            renaming: None,
            live_take: None,
            loop_range: None,
            wave_requests: Vec::new(),
            quads: 0,
        }
    }
}

/// The volume line of a sound clip: its height in the clip says how loud
/// the clip is. +12 dB at the top, 0 dB a quarter down, -36 dB (and
/// anything quieter) at the bottom.
const GAIN_TOP_DB: f32 = 12.0;
const GAIN_SPAN_DB: f32 = 48.0;
/// The range a clip's volume can take, as in the inspector.
const GAIN_RANGE: std::ops::RangeInclusive<f32> = -60.0..=24.0;
/// Rows and clips smaller than this (points) carry no volume line.
const GAIN_MIN_ROW: f32 = 24.0;
const GAIN_MIN_WIDTH: f32 = 36.0;

/// Top and bottom of a clip inside a row that starts at `top`.
fn clip_span(top: f32, row_h: f32) -> (f32, f32) {
    // Small rows keep less padding so that the clips stay visible.
    let pad = if row_h < 20.0 { 1.0 } else { 3.0 };
    (top + pad, top + row_h - pad - 1.0)
}

/// Where the volume line of a clip between `y0` and `y1` lies.
fn gain_y(db: f32, y0: f32, y1: f32) -> f32 {
    let down = ((GAIN_TOP_DB - db) / GAIN_SPAN_DB).clamp(0.0, 1.0);
    // (Kept off the very edges, where the selection bar and the fade marks are.)
    y0 + 3.0 + down * (y1 - y0 - 6.0)
}

/// The volume a line at height `y` stands for; at the bottom, silence.
fn gain_at(y: f32, y0: f32, y1: f32) -> f32 {
    let down = ((y - y0 - 3.0) / (y1 - y0 - 6.0).max(1.0)).clamp(0.0, 1.0);
    if down >= 1.0 { *GAIN_RANGE.start() } else { GAIN_TOP_DB - down * GAIN_SPAN_DB }
}

/// The most clips a box or "select all" takes. Each drawn clip is looked
/// up in the selection, so an unbounded one would cost time every frame
/// in very large projects.
const SELECT_MOST: usize = 2000;

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

/// The frames (from the clip's start) at which any value of a clip has a
/// keyframe, in order, each once.
fn key_frames(props: &sg_core::ClipProps) -> Vec<i64> {
    let animated = [&props.x, &props.y, &props.scale, &props.rotation, &props.opacity, &props.ramp, &props.gain_db];
    let mut frames: Vec<i64> = animated.iter().flat_map(|p| p.keys.iter().map(|k| k.frame)).collect();
    frames.sort_unstable();
    frames.dedup();
    frames
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
            // The groups these clips are in come along (feedback F10.1).
            // This looks at every clip once, on a click only, and only when
            // a clicked clip is grouped at all.
            let group_of =
                |sel: &Sel| sequence.track(sel.track).and_then(|t| t.props_of(sel.id)).map_or(0, |p| p.group);
            let groups: Vec<u64> = picked.iter().map(group_of).filter(|g| *g != 0).collect();
            if !groups.is_empty() {
                for other in &sequence.tracks {
                    for member in other.clips.iter() {
                        if other.props_of(member.id).is_some_and(|p| groups.contains(&p.group)) {
                            let sel = Sel { track: other.id, start: member.start, id: member.id };
                            if !picked.contains(&sel) && picked.len() < SELECT_MOST {
                                picked.push(sel);
                            }
                        }
                    }
                }
            }
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
        // Rows from the top of the lanes, with the fraction: how far down the row.
        let row_at = move |y: f32| (y - rect.top() - RULER_H) / row_h;
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

        self.gain_label = None;
        self.marquee = None;
        self.pointer(ui, &response, session, &sequence, view, transport, error, rect, lanes, &frame_at, &row_at);
        self.rect = rect;
        self.asset_drop(ui, &response, session, &project, &sequence, view, error);
        self.paint(ui, rect, lanes, &project, &sequence, view, &x_of);
        self.track_controls(ui, &response, session, &sequence, rect, &row_at, error);
        if let Some(area) = self.marquee {
            let painter = ui.painter_at(rect);
            painter.rect_filled(area, 0.0, theme::ACCENT.gamma_multiply(0.12));
            painter.rect_stroke(area, 0.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
        }
        // The volume under the pointer, in figures, beside it.
        if let Some((at, db)) = self.gain_label {
            let text = if db <= *GAIN_RANGE.start() { "silent".to_owned() } else { format!("{db:+.1} dB") };
            let painter = ui.painter_at(rect);
            let galley = painter.layout_no_wrap(text, FontId::proportional(11.0), theme::TEXT);
            let plate = Rect::from_min_size(at + egui::vec2(12.0, -22.0), galley.size() + egui::vec2(10.0, 6.0));
            painter.rect_filled(plate, theme::RADIUS, theme::BG);
            painter.galley(plate.min + egui::vec2(5.0, 3.0), galley, theme::TEXT);
        }
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
        // A double click on a track's name: type another (feedback F16.11).
        if response.double_clicked_by(PointerButton::Primary)
            && let Some(pos) = response.interact_pointer_pos()
            && pos.x < rect.left() + HEADER_W - 80.0
            && pos.y >= rect.top() + RULER_H
            && let Some(track) = sequence.tracks.get(((pos.y - rect.top() - RULER_H) / self.row_h) as usize)
        {
            self.renaming = Some((track.id, track.name.clone(), true));
        }
        if let Some((id, name, focus)) = &mut self.renaming
            && let Some(row) = sequence.tracks.iter().position(|t| t.id == *id)
        {
            let y = rect.top() + RULER_H + row as f32 * self.row_h + self.row_h / 2.0;
            let area = Rect::from_min_size(pos2(rect.left() + 6.0, y - 10.0), egui::vec2(HEADER_W - 12.0, 20.0));
            let field = ui.put(area, egui::TextEdit::singleline(name).font(FontId::proportional(12.0)));
            field.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Track name"));
            if std::mem::take(focus) {
                field.request_focus();
            }
            if field.lost_focus() {
                let wanted = name.trim().to_owned();
                let kept = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let id = *id;
                self.renaming = None;
                if !kept
                    && !wanted.is_empty()
                    && wanted != sequence.tracks[row].name
                    && let Err(e) = session.apply(Op::SetTrackName { seq: sequence.id, track: id, name: wanted }, None)
                {
                    *error = Some(e.to_string());
                }
            }
        } else {
            self.renaming = None;
        }
        // Quick switches at the right of each track name: shown or hidden for
        // video, mute and solo for audio.
        let mut settings = None;
        if self.row_h >= 18.0 {
            for (row, track) in sequence.tracks.iter().enumerate() {
                // (The name being typed has the header to itself.)
                if self.renaming.as_ref().is_some_and(|(id, ..)| *id == track.id) {
                    continue;
                }
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
                        let takes = self.record_track == Some(track.id);
                        if small(
                            ui,
                            75.0,
                            21.0,
                            "R",
                            takes,
                            theme::ERROR,
                            "Recordings go onto this track, over what is there, from the playhead on. Without a \
                             track marked so, a take gets a free track.",
                        ) {
                            self.record_track = (!takes).then_some(track.id);
                        }
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
            self.menu_frame = frame;
            self.menu_clip = self.hit(sequence, row, frame).filter(|_| on_lanes).map(|(row, clip, _)| (row, clip));
            // The menu acts on what is shown as selected.
            if let Some((row, clip)) = self.menu_clip {
                self.select(sequence, &sequence.tracks[row], &clip, false, false);
            }
        }
        let mut remove = None;
        let mut delete: Vec<Sel> = Vec::new();
        if !response.context_menu_opened() {
            self.menu_group = None;
        }
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
                // The sound of the take, whichever of its clips was clicked.
                let sound: Vec<Sel> = all.iter().copied().filter(|s| kind_of(s) == Some(TrackKind::Audio)).collect();
                if let Some(first) = sound.first() {
                    ui.menu_button("Clip volume", |ui| {
                        let now = sequence
                            .track(first.track)
                            .and_then(|t| t.props_of(first.id))
                            .map_or(0.0, |p| p.gain_db.at(0));
                        let mut db = now;
                        let group = *self.menu_group.get_or_insert_with(|| session.new_group());
                        ui.add(
                            egui::DragValue::new(&mut db).range(GAIN_RANGE).speed(0.1).fixed_decimals(1).suffix(" dB"),
                        )
                        .on_hover_text("Drag, or click and type. The line across a sound clip does the same.");
                        for (label, to) in
                            [("Quieter (-3 dB)", now - 3.0), ("Louder (+3 dB)", now + 3.0), ("Back to 0 dB", 0.0)]
                        {
                            if ui.button(label).clicked() {
                                db = to;
                            }
                        }
                        if db != now {
                            Timeline::shift_gain(session, sequence, &sound, db - now, Some(group));
                        }
                    });
                }
                ui.separator();
                if track.props_of(clip.id).is_some_and(|p| p.title.is_some()) && ui.button("Edit text").clicked() {
                    self.edit_title = true;
                    ui.close();
                }
                for (action, label) in [
                    (Action::KeyframeSet, "Keyframe at the playhead"),
                    (Action::KeyframeRemove, "Remove the keyframe here"),
                    (Action::Transition, "Whip-pan on this clip's cut"),
                    (Action::Blackout, "Black on this clip's cut"),
                    (Action::Group, "Group the selected clips, or ungroup"),
                    (Action::Delete, "Delete clip"),
                ] {
                    if crate::actions::entry(ui, action, label) {
                        self.actions.push(action);
                    }
                }
                ui.separator();
            }
            for (action, label) in [(Action::Marker, "Marker at the playhead"), (Action::AddTitle, "Add a title")] {
                if crate::actions::entry(ui, action, label) {
                    self.actions.push(action);
                }
            }
            ui.separator();
            if ui.button("Add video track").clicked() {
                add = Some(TrackKind::Video);
                ui.close();
            }
            // Notes: on an audio track, where nothing lies yet. Played by the
            // track's plugin, or by the built-in instrument if it has none.
            if let Some(track) = self.menu_row.and_then(|row| sequence.tracks.get(row)).filter(|t| t.kind == TrackKind::Audio)
                && self.menu_clip.is_none()
                && ui
                    .button("Add note clip here")
                    .on_hover_text("Two bars to write notes into (double-click it). An instrument plugin on the track plays them; without one, a built-in instrument.")
                    .clicked()
            {
                let (track, frame) = (track.clone(), self.menu_frame);
                self.add_note_clip(session, sequence, &track, frame, error);
                ui.close();
            }
            if ui.button("Add audio track").clicked() {
                add = Some(TrackKind::Audio);
                ui.close();
            }
            if let Some(track) = self.menu_row.and_then(|row| sequence.tracks.get(row)) {
                ui.separator();
                if ui.button(format!("Rename track {}", track.name)).clicked() {
                    self.renaming = Some((track.id, track.name.clone(), true));
                    ui.close();
                }
                // Up and down among the tracks of its kind (pictures stay above sound).
                let at = sequence.tracks.iter().position(|t| t.id == track.id).unwrap_or(0);
                for (label, to) in [("up", at.checked_sub(1)), ("down", Some(at + 1))] {
                    let neighbour = to.and_then(|to| sequence.tracks.get(to)).filter(|other| other.kind == track.kind);
                    let button = ui.add_enabled(neighbour.is_some(), egui::Button::new(format!("Move track {} {label}", track.name)));
                    if let (true, Some(to)) = (button.clicked(), to) {
                        if let Err(e) = session.apply(Op::MoveTrack { seq: sequence.id, id: track.id, index: to }, None) {
                            *error = Some(e.to_string());
                        }
                        ui.close();
                    }
                }
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
            // The wheel moves along the sequence, whichever way it turns
            // (feedback F4.5); with Ctrl it zooms, which egui reports as a
            // zoom factor, the same as a pinch on a touchpad.
            let (scroll, zoom, pointer) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            if zoom != 1.0 {
                // Around the pointer: the frame under it stays put.
                let x = pointer.map_or(lanes.center().x, |p| p.x) - lanes.left();
                self.zoom_around(1.0 / zoom as f64, x, lanes.width(), end);
            } else if scroll != egui::Vec2::ZERO {
                self.left -= (scroll.x + scroll.y) as f64 * self.frames_per_point;
            }
        }
        if response.dragged_by(PointerButton::Middle) {
            self.left -= response.drag_delta().x as f64 * self.frames_per_point;
        }
        // Keep some of the sequence in view.
        let visible = lanes.width() as f64 * self.frames_per_point;
        self.left = self.left.clamp(-visible * 0.1, (end - visible * 0.2).max(0.0));
    }

    /// Changes the zoom by `factor` (above 1: further out) keeping the
    /// frame at `x` points from the left of the lanes where it is.
    fn zoom_around(&mut self, factor: f64, x: f32, lanes_width: f32, end: f64) {
        let anchor = self.left + x as f64 * self.frames_per_point;
        // Out to four times the sequence, and to ten minutes whatever its
        // length: it was twice the sequence or 600 frames, which in a new
        // or short project is less than what it opens with, so that Ctrl +
        // wheel did nothing outwards (feedback F16.21).
        let widest = ((end * 4.0).max(36_000.0) / lanes_width.max(1.0) as f64).max(0.01);
        self.frames_per_point = (self.frames_per_point * factor).clamp(0.005, widest);
        self.left = anchor - x as f64 * self.frames_per_point;
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

    /// The fade mark under the pointer: selected clips wide enough to aim
    /// at carry one near each top corner, just inside where the fade ends.
    fn fade_hit(&self, sequence: &Sequence, row: f32, frame: f64) -> Option<(Sel, Edge)> {
        if row < 0.0 || row.fract() * self.row_h > 11.0 || self.row_h < 24.0 {
            return None;
        }
        let track = sequence.tracks.get(row as usize)?;
        let per = Lane::of(track, sequence).per_frame();
        let clip = track.clips.at((frame * per) as i64)?;
        if !self.is_selected(track.id, &clip) {
            return None;
        }
        let (start, end) = (clip.start as f64 / per, clip.end() as f64 / per);
        let reach = GRAB as f64 * self.frames_per_point;
        if end - start < reach * 10.0 {
            return None;
        }
        let (fade_in, fade_out) = track.props_of(clip.id).map_or((0, 0), |p| (p.fade_in, p.fade_out));
        let sel = Sel { track: track.id, start: clip.start, id: clip.id };
        let after_in = frame - (start + fade_in as f64);
        let before_out = (end - fade_out as f64) - frame;
        if (reach..reach * 3.0).contains(&after_in) {
            Some((sel, Edge::Start))
        } else if (reach..reach * 3.0).contains(&before_out) {
            Some((sel, Edge::End))
        } else {
            None
        }
    }

    /// The sound clip whose volume line is under the pointer. `row` counts
    /// rows from the top of the lanes, fraction included.
    fn gain_hit(&self, sequence: &Sequence, row: f32, frame: f64) -> Option<(Sel, f32)> {
        if row < 0.0 || self.row_h < GAIN_MIN_ROW {
            return None;
        }
        let track = sequence.tracks.get(row as usize).filter(|t| t.kind == TrackKind::Audio)?;
        let per = Lane::of(track, sequence).per_frame();
        let clip = track.clips.at((frame * per) as i64)?;
        let (start, end) = (clip.start as f64 / per, clip.end() as f64 / per);
        // The ends belong to trimming.
        let reach = GRAB as f64 * self.frames_per_point;
        if (end - start) / self.frames_per_point < GAIN_MIN_WIDTH as f64 || frame - start < reach || end - frame < reach
        {
            return None;
        }
        let db = track.props_of(clip.id).map_or(0.0, |p| p.gain_db.at(0));
        let (y0, y1) = clip_span(0.0, self.row_h);
        let y = row.fract() * self.row_h;
        ((y - gain_y(db, y0, y1)).abs() <= 3.5).then_some((Sel { track: track.id, start: clip.start, id: clip.id }, db))
    }

    /// Makes these clips louder or quieter by `delta` decibels, keyframes
    /// and all. Clips on video tracks are left alone.
    fn shift_gain(session: &mut Session, sequence: &Sequence, clips: &[Sel], delta: f32, group: Option<u64>) {
        for sel in clips {
            let Some(track) = sequence.track(sel.track).filter(|t| t.kind == TrackKind::Audio) else { continue };
            let Some(clip) = track.clips.get(sel.start) else { continue };
            let before = track.props_of(clip.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
            let mut props = before.clone();
            let moved = |db: f32| ((db + delta).clamp(*GAIN_RANGE.start(), *GAIN_RANGE.end()) * 10.0).round() / 10.0;
            props.gain_db.value = moved(props.gain_db.value);
            for key in &mut props.gain_db.keys {
                key.value = moved(key.value);
            }
            if props == before {
                continue;
            }
            let props = (props != ClipProps::default()).then(|| Box::new(props));
            let op = Op::SetClipProps { seq: sequence.id, track: track.id, clip: clip.id, props };
            // (A clip that cannot take it is left as it is.)
            let _ = session.apply(op, group);
        }
    }

    /// Sets the fade of every selected clip so that the grabbed one's ends
    /// (or begins) at `frame`.
    fn drag_fade(&mut self, session: &mut Session, sequence: &Sequence, sel: Sel, edge: Edge, group: u64, frame: f64) {
        let Some(track) = sequence.track(sel.track) else { return };
        let Some(clip) = track.clips.get(sel.start) else { return };
        let per = Lane::of(track, sequence).per_frame();
        let wanted = match edge {
            Edge::Start => frame - clip.start as f64 / per,
            Edge::End => clip.end() as f64 / per - frame,
        }
        .round() as i64;
        for target in self.selection.clone() {
            let Some(track) = sequence.track(target.track) else { continue };
            let Some(clip) = track.clips.get(target.start) else { continue };
            let frames = (clip.len as f64 / Lane::of(track, sequence).per_frame()) as i64;
            let mut props = track.props_of(clip.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
            // The two fades of a clip do not cross.
            match edge {
                Edge::Start => props.fade_in = wanted.clamp(0, (frames - props.fade_out).max(0)),
                Edge::End => props.fade_out = wanted.clamp(0, (frames - props.fade_in).max(0)),
            }
            // Nothing changed (a clip without properties counts as the resting state).
            let before = track.props_of(clip.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
            if before == props {
                continue;
            }
            let props = (props != ClipProps::default()).then(|| Box::new(props));
            let op = Op::SetClipProps { seq: sequence.id, track: track.id, clip: clip.id, props };
            // (A clip that cannot take it is left as it is.)
            let _ = session.apply(op, Some(group));
        }
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
            let over_fade = self.fade_hit(sequence, row_at(pos.y), frame_at(pos.x)).is_some();
            if (over_edge || over_fade) && matches!(self.drag, Drag::None) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            } else if let (Drag::None, Some((_, db))) =
                (&self.drag, self.gain_hit(sequence, row_at(pos.y), frame_at(pos.x)))
            {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                self.gain_label = Some((pos, db));
            }
        }

        if response.drag_started_by(PointerButton::Primary) {
            let origin = press.unwrap_or(rect.min);
            let on_ruler = origin.y < rect.top() + RULER_H || origin.x < lanes.left();
            let fade = self.fade_hit(sequence, row_at(origin.y), frame_at(origin.x)).filter(|_| !on_ruler);
            let gain = self.gain_hit(sequence, row_at(origin.y), frame_at(origin.x)).filter(|_| !on_ruler);
            self.drag = match self.hit(sequence, row_at(origin.y), frame_at(origin.x)).filter(|_| !on_ruler) {
                _ if fade.is_some() => match fade {
                    Some((sel, edge)) => Drag::Fade { sel, edge, group: session.new_group() },
                    None => Drag::None,
                },
                Some((row, clip, None)) if gain.is_some() => {
                    let sel = Sel { track: sequence.tracks[row].id, start: clip.start, id: clip.id };
                    // The line of a clip outside the selection moves that clip alone.
                    if !self.selection.contains(&sel) {
                        self.selection = vec![sel];
                    }
                    Drag::Gain { sel, group: session.new_group() }
                }
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
                // On the ruler the playhead follows; on empty lanes a box opens.
                None if on_ruler => Drag::Scrub,
                None => {
                    let before = if modifiers.shift { self.selection.clone() } else { Vec::new() };
                    Drag::Marquee { from: origin, before }
                }
            };
        } else if response.drag_started_by(PointerButton::Middle) {
            self.drag = Drag::Pan;
        }

        // A double click on a title goes to its text, on a note clip to its notes.
        if response.double_clicked_by(PointerButton::Primary)
            && let Some(pos) = pointer
            && let Some((row, clip, _)) = self.hit(sequence, row_at(pos.y), frame_at(pos.x))
        {
            let track = &sequence.tracks[row];
            if track.props_of(clip.id).is_some_and(|p| p.title.is_some()) {
                self.edit_title = true;
            } else if track.kind == TrackKind::Audio && clip.asset == AssetId(0) {
                self.edit_notes = Some((track.id, clip.id));
            }
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
                Drag::Marquee { from, before } => {
                    let (from, mut picked) = (*from, before.clone());
                    let (f0, f1) = (frame_at(from.x.min(pos.x)), frame_at(from.x.max(pos.x)));
                    let (r0, r1) = (row_at(from.y.min(pos.y)).max(0.0) as usize, row_at(from.y.max(pos.y)));
                    if r1 >= 0.0 {
                        for track in sequence.tracks.iter().take(r1 as usize + 1).skip(r0) {
                            let per = Lane::of(track, sequence).per_frame();
                            let touched = track.clips.range((f0 * per).floor() as i64, (f1 * per).ceil() as i64 + 1);
                            for clip in touched.take(SELECT_MOST.saturating_sub(picked.len())) {
                                let sel = Sel { track: track.id, start: clip.start, id: clip.id };
                                if !picked.contains(&sel) {
                                    picked.push(sel);
                                }
                            }
                        }
                    }
                    self.selection = picked;
                    self.marquee = Some(Rect::from_two_pos(from, pos).intersect(lanes));
                }
                Drag::Move { grab_frame, grab_row, primary } => {
                    let (grab_frame, grab_row, primary) = (*grab_frame, *grab_row, *primary);
                    let row_delta = (row_at(pos.y).max(0.0) as isize) - grab_row as isize;
                    self.plan_move(
                        sequence,
                        view,
                        frame_at(pos.x) - grab_frame,
                        row_delta,
                        primary,
                        !modifiers.alt,
                        modifiers.ctrl,
                    );
                }
                Drag::Trim { sel, edge, ripple } => {
                    let (sel, edge, ripple) = (*sel, *edge, *ripple);
                    self.plan_trim(session, sequence, view, frame_at(pos.x), sel, edge, ripple, !modifiers.alt);
                }
                Drag::Fade { sel, edge, group } => {
                    let (sel, edge, group) = (*sel, *edge, *group);
                    self.drag_fade(session, sequence, sel, edge, group, frame_at(pos.x));
                }
                Drag::Gain { sel, group } => {
                    let (sel, group) = (*sel, *group);
                    let row = sequence.tracks.iter().position(|t| t.id == sel.track).unwrap_or(0);
                    let (y0, y1) = clip_span(rect.top() + RULER_H + row as f32 * self.row_h, self.row_h);
                    let now =
                        sequence.track(sel.track).and_then(|t| t.props_of(sel.id)).map_or(0.0, |p| p.gain_db.at(0));
                    // The line follows the pointer in whole decibels; with
                    // Shift it creeps, a tenth of a decibel per point.
                    let wanted = if modifiers.shift {
                        now - response.drag_delta().y * 0.1
                    } else {
                        gain_at(pos.y, y0, y1).round()
                    };
                    Timeline::shift_gain(session, sequence, &self.selection.clone(), wanted - now, Some(group));
                    self.gain_label = Some((pos, wanted.clamp(*GAIN_RANGE.start(), *GAIN_RANGE.end())));
                }
                Drag::Pan | Drag::None => {}
            }
        }

        if response.drag_stopped() {
            let over = std::mem::take(&mut self.pending_over);
            if let Some(ops) = self.pending.take().filter(|ops| !ops.is_empty()) {
                // One gesture, one undo step.
                let done =
                    if over.is_empty() { session.apply(Op::Batch(ops), None) } else { session.move_over(ops, over) };
                match done {
                    Ok(()) => self.reselect_after_gesture(session),
                    Err(_) => *error = Some("No room for that here.".to_owned()),
                }
            } else if matches!(self.drag, Drag::Move { .. }) && self.ghosts.iter().any(|g| !g.3) {
                *error =
                    Some("No room there. Hold Ctrl while dropping to put the clip over what is in the way.".to_owned());
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
        over: bool,
    ) {
        self.ghosts.clear();
        self.pending = None;
        self.pending_over.clear();
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
        let mut covers = false;
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
            // With Ctrl the clip goes over what is in the way.
            fits &= !blocked || over;
            covers |= blocked;
            let target_row = sequence.tracks.iter().position(|t| t.id == target.id).unwrap_or(row);
            self.ghosts.push((target_row, moved.start, moved.len, !blocked || over));
            removes.push(Op::RemoveClip { seq: sequence.id, track: track.id, start: clip.start });
            // A note clip takes its notes along to another track (they are kept per track).
            if target.id != track.id
                && let Some(notes) = track.notes.get(&clip.id)
            {
                let seq = sequence.id;
                removes.push(Op::SetClipNotes { seq, track: target.id, clip: clip.id, notes: Some(Vec::clone(notes)) });
                removes.push(Op::SetClipNotes { seq, track: track.id, clip: clip.id, notes: None });
            }
            inserts.push((target.id, moved));
        }
        if fits && covers {
            // What has to give way is worked out when the button is released.
            self.pending = Some(removes);
            self.pending_over = inserts;
        } else if fits {
            removes.extend(inserts.into_iter().map(|(track, clip)| Op::InsertClip { seq: sequence.id, track, clip }));
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
            let (y0, y1) = clip_span(top, row_h);
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
                    if track.kind == TrackKind::Audio && clip.asset == AssetId(0) {
                        // A note clip: its notes as small bars, high ones above low ones.
                        if let Some(notes) = track.notes.get(&clip.id).filter(|n| !n.is_empty()) {
                            let (low, high) =
                                notes.iter().fold((127u8, 0u8), |(l, h), n| (l.min(n.pitch), h.max(n.pitch)));
                            let span = (high - low).max(11) as f32 + 1.0;
                            let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
                            let per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * sg_core::TICKS_PER_BEAT as f64);
                            for note in notes.iter().take(256) {
                                // (From where the clip begins in its notes, as it plays them.)
                                let from = (clip.start - clip.src_in) as f64;
                                let a = x_of((from + note.start as f64 * per_tick) / per);
                                let b = x_of((from + (note.start + note.len) as f64 * per_tick) / per);
                                let y = y1 - 3.0 - (note.pitch - low) as f32 / span * (y1 - y0 - 8.0);
                                let (a, b) = (a.max(x0), b.min(x1));
                                if b > a {
                                    instances.push(Instance::new(
                                        quad(a, y - 2.0, b.max(a + 1.5), y),
                                        theme::TEXT,
                                        0.0,
                                        0.0,
                                    ));
                                }
                            }
                        }
                    } else if track.kind == TrackKind::Audio && row_h >= 14.0 && x1 - x0 >= 4.0 {
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
                    // Fades: the clip darkens towards its end in steps (the
                    // lanes are drawn with rectangles only), and a selected
                    // clip carries a mark to pull at each top corner.
                    let (fade_in, fade_out) = track.props_of(clip.id).map_or((0, 0), |p| (p.fade_in, p.fade_out));
                    let (start_x, end_x) = (x_of(clip.start as f64 / per), x_of(clip.end() as f64 / per));
                    let (in_x, out_x) = (
                        x_of(clip.start as f64 / per + fade_in as f64),
                        x_of(clip.end() as f64 / per - fade_out as f64),
                    );
                    const STEPS: usize = 8;
                    for (from, to) in [(start_x, in_x), (end_x, out_x)] {
                        if (to - from).abs() < 3.0 {
                            continue;
                        }
                        for step in 0..STEPS {
                            // Full height at the clip's end, nothing where the fade is over.
                            let (a, b) = (
                                from + (to - from) * step as f32 / STEPS as f32,
                                from + (to - from) * (step + 1) as f32 / STEPS as f32,
                            );
                            let depth = (y1 - y0) * (1.0 - (step as f32 + 0.5) / STEPS as f32);
                            let (left, right) = (a.min(b).max(x0), a.max(b).min(x1));
                            if right > left {
                                instances.push(Instance::new(
                                    quad(left, y0, right, y0 + depth),
                                    egui::Color32::from_black_alpha(110),
                                    0.0,
                                    0.0,
                                ));
                            }
                        }
                    }
                    if selected && row_h >= 24.0 && end_x - start_x >= GRAB * 10.0 {
                        for x in [in_x + GRAB, out_x - GRAB * 3.0] {
                            if x > x0 && x + GRAB * 2.0 < x1 {
                                instances.push(Instance::new(
                                    quad(x, y0 + 2.0, x + GRAB * 2.0, y0 + 8.0),
                                    theme::TEXT,
                                    px(2.0),
                                    0.0,
                                ));
                            }
                        }
                    }
                    // Keyframes: a small mark on the clip's lower edge at each.
                    if x1 - x0 >= 12.0
                        && let Some(props) = track.props_of(clip.id)
                    {
                        let mut last = f32::MIN;
                        for frame in key_frames(props).into_iter().take(64) {
                            let x = x_of(clip.start as f64 / per + frame as f64);
                            // (Marks closer than their own width would only smear.)
                            if x > x0 && x < x1 && x - last >= 7.0 {
                                instances.push(Instance::new(
                                    quad(x - 3.0, y1 - 7.0, x + 3.0, y1 - 1.0),
                                    theme::TEXT,
                                    px(1.0),
                                    0.0,
                                ));
                                last = x;
                            }
                        }
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
                    let mut name_w = 0.0;
                    if x1 - x0 >= LABEL_MIN && row_h >= 16.0 && labels.len() < 400 {
                        // A title shows its text, anything else its asset's label.
                        let title = track.props_of(clip.id).and_then(|p| p.title.as_ref()).map(|t| t.text.as_str());
                        let notes = (track.kind == TrackKind::Audio && clip.asset == AssetId(0)).then_some("notes");
                        let name = title
                            .or(notes)
                            .or(project.assets.get(&clip.asset).map(|a| a.label.as_str()))
                            .unwrap_or("missing asset");
                        labels.push((
                            x0.max(lanes.left()) + 5.0 + thumb_w,
                            (y0 + y1) / 2.0,
                            x1 - x0.max(lanes.left()) - 8.0 - thumb_w,
                            name,
                        ));
                        // (Roughly the room the name takes, so that the volume line starts after it.)
                        name_w = (name.chars().count() as f32 * 6.5 + 12.0).min((x1 - x0) * 0.5);
                    }
                    // In a group: a light bar down the clip's left edge.
                    if track.props_of(clip.id).is_some_and(|p| p.group != 0) && x1 - x0 >= 6.0 {
                        instances.push(Instance::new(quad(x0, y0 + 2.0, x0 + 2.5, y1 - 2.0), theme::TEXT, 0.0, 0.0));
                    }
                    // The volume line: pulled up or down to make the clip louder or quieter.
                    if track.kind == TrackKind::Audio && row_h >= GAIN_MIN_ROW && x1 - x0 >= GAIN_MIN_WIDTH {
                        let db = track.props_of(clip.id).map_or(0.0, |p| p.gain_db.at(0));
                        let y = gain_y(db, y0, y1);
                        let line = if db == 0.0 { theme::TEXT.gamma_multiply(0.45) } else { theme::TEXT };
                        let from = x0.max(lanes.left()) + 2.0 + name_w;
                        instances.push(Instance::new(quad(from, y - 0.75, x1 - 2.0, y + 0.75), line, 0.0, 0.0));
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
            // (A long name ends where the track's switches begin.)
            let room = Rect::from_min_max(
                pos2(rect.left(), y - row_h / 2.0),
                pos2(rect.left() + HEADER_W - if row_h >= 18.0 { 78.0 } else { 4.0 }, y + row_h / 2.0),
            );
            painter.with_clip_rect(room).text(
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
        // The stretch that loops: a bar along the bottom of the ruler.
        if let Some((from, to)) = self.loop_range {
            let frame = |samples: i64| {
                samples as f64 * sequence.rate.num() as f64 / (sequence.rate.den() as f64 * SAMPLE_RATE as f64)
            };
            let (a, b) = (x_of(frame(from)).max(lanes.left()), x_of(frame(to)).min(lanes.right()));
            if b > a {
                let bar = Rect::from_min_max(pos2(a, rect.top() + RULER_H - 4.0), pos2(b, rect.top() + RULER_H));
                painter.rect_filled(bar, 1.0, theme::ACCENT);
            }
        }
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
        if let Some(live) = &self.live_take
            && let Some(row) = sequence.tracks.iter().position(|t| t.id == live.track)
        {
            let per = Lane::of(&sequence.tracks[row], sequence).per_frame();
            let top = rect.top() + RULER_H + row as f32 * row_h;
            let lanes_painter = painter.with_clip_rect(lanes);
            let (x0, x1) = (x_of(live.from as f64 / per), x_of(live.to as f64 / per));
            let take = Rect::from_min_max(pos2(x0, top + 3.0), pos2(x1.max(x0 + 2.0), top + row_h - 4.0));
            lanes_painter.rect_filled(take, theme::RADIUS, theme::ERROR.gamma_multiply(0.3));
            lanes_painter.rect_stroke(take, theme::RADIUS, Stroke::new(1.5, theme::ERROR), StrokeKind::Inside);
            // Its level as it came in: only the columns in view are drawn.
            let (first, last) = (take.left().max(lanes.left()), take.right().min(lanes.right()));
            let mut x = first;
            while x < last {
                let sample = (self.left + ((x - lanes.left()) as f64) * fpp) * per;
                let bucket = ((sample - live.from as f64) / LiveTake::BUCKET as f64).max(0.0) as usize;
                // (As wide as the column is: the loudest of what falls into it.)
                let wide = ((2.0 * fpp * per / LiveTake::BUCKET as f64).ceil() as usize).max(1);
                let end = (bucket + wide).min(live.peaks.len());
                let peak = live.peaks.get(bucket..end).map_or(0.0, |p| p.iter().copied().fold(0.0, f32::max));
                let half = (peak.min(1.0).sqrt() * (take.height() / 2.0 - 2.0)).max(0.5);
                lanes_painter.line_segment(
                    [pos2(x, take.center().y - half), pos2(x, take.center().y + half)],
                    Stroke::new(1.5, theme::ERROR),
                );
                x += 2.0;
            }
            let seconds = (live.to - live.from).max(0) as f64 / SAMPLE_RATE as f64;
            let label = if live.to > live.from { format!("REC {seconds:.1} s") } else { "REC from here".to_owned() };
            lanes_painter.text(
                pos2(take.left().max(lanes.left()) + 5.0, take.top() + 2.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(10.0),
                theme::TEXT,
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
            painter.rect_filled(ghost, theme::RADIUS, colour.gamma_multiply(0.25));
            painter.rect_stroke(ghost, theme::RADIUS, Stroke::new(1.5, colour), StrokeKind::Inside);
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
                        lanes_painter.rect_filled(ghost, theme::RADIUS, colour.gamma_multiply(0.3));
                        lanes_painter.rect_stroke(
                            ghost,
                            theme::RADIUS,
                            Stroke::new(1.5, theme::ACCENT),
                            StrokeKind::Inside,
                        );
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
        // With a tempo the ruler counts bars (09-daw.md): every bar that has
        // room for its number, thinned out by twos when zoomed far out.
        if let Some(tempo) = sequence.tempo {
            let bar = tempo.frames_per_beat(rate) * tempo.beats_per_bar as f64;
            // (Bars are counted from where the first one begins, which need not be the start.)
            let origin = tempo.origin_frames(rate);
            let mut stride = 1.0;
            while bar * stride / fpp < 46.0 && stride < 65_536.0 {
                stride *= 2.0;
            }
            let mut n = ((self.left - origin) / (bar * stride)).floor();
            while origin + n * bar * stride <= right {
                let x = x_of(origin + n * bar * stride);
                if x >= lanes.left() {
                    painter.line_segment(
                        [pos2(x, rect.top() + 13.0), pos2(x, rect.top() + RULER_H)],
                        Stroke::new(1.0, theme::LINE),
                    );
                    let number = format!("{}", (n * stride) as i64 + 1);
                    painter.text(
                        pos2(x + 3.0, rect.top() + 2.0),
                        Align2::LEFT_TOP,
                        number,
                        FontId::monospace(10.0),
                        theme::DIM,
                    );
                }
                n += 1.0;
            }
            tick = f64::INFINITY;
        }
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
                    let lane = Lane::of(&track, &sequence);
                    let at = lane.units(playhead);
                    let ramp = picture_ramp(&sequence, &track, &clip);
                    ops.extend(split_ops(seq, &track, lane, &clip, at, session.new_clip_id(), ramp.as_ref()));
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
            Action::RippleDelete | Action::Delete | Action::Cut => {
                if action == Action::Cut {
                    self.copy(&sequence);
                }
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
            Action::SelectAll => {
                let all = sequence.tracks.iter().flat_map(|track| {
                    track.clips.iter().map(|clip| Sel { track: track.id, start: clip.start, id: clip.id })
                });
                self.selection = all.take(SELECT_MOST).collect();
                if sequence.clip_count() > SELECT_MOST {
                    *error = Some(format!("Selected the first {SELECT_MOST} clips: more cannot be selected at once."));
                }
            }
            Action::Group => {
                let members: Vec<(Sel, ClipProps)> = self
                    .selection
                    .iter()
                    .filter_map(|sel| {
                        let track = sequence.track(sel.track)?;
                        track.clips.get(sel.start)?;
                        Some((*sel, track.props_of(sel.id).map(|p| ClipProps::clone(p)).unwrap_or_default()))
                    })
                    .collect();
                // Already one group, all of them: the key dissolves it.
                let one = members.first().map_or(0, |(_, p)| p.group);
                let grouped = one != 0 && members.iter().all(|(_, p)| p.group == one);
                if members.len() < 2 && !grouped {
                    *error = Some("Select two or more clips to make a group of them.".to_owned());
                } else {
                    // (Any number no other group has; clip ids are handed out once.)
                    let group = if grouped { 0 } else { session.new_clip_id().0.max(1) };
                    for (sel, mut props) in members {
                        props.group = group;
                        let props = (props != ClipProps::default()).then(|| Box::new(props));
                        ops.push(Op::SetClipProps { seq, track: sel.track, clip: sel.id, props });
                    }
                }
            }
            Action::Copy => self.copy(&sequence),
            Action::PasteClips => {
                self.selection.clear();
                // Nothing is covered by a paste (feedback F7.2): the clips
                // copied from one track stay together and go to that track
                // if they fit there at the playhead, otherwise to the
                // nearest track of the kind that has the room, otherwise to
                // a new one.
                let mut rows: Vec<usize> = self.clipboard.iter().map(|c| c.row).collect();
                rows.dedup();
                rows.sort_unstable();
                rows.dedup();
                let mut placed: Vec<(TrackId, Clip)> = Vec::new();
                // Copies of grouped clips form groups of their own, not one with the originals.
                let mut regrouped: HashMap<u64, u64> = HashMap::new();
                let mut taken: Vec<TrackId> = Vec::new();
                let mut inserts: Vec<Op> = Vec::new();
                let mut added = [0usize; 2];
                for row in rows {
                    let Some(home) = sequence.tracks.get(row) else { continue };
                    let at = Lane::of(home, &sequence).units(playhead);
                    let group: Vec<&Copied> = self.clipboard.iter().filter(|c| c.row == row).collect();
                    let fits = |track: &Track| {
                        group
                            .iter()
                            .all(|c| track.clips.range(at + c.offset, at + c.offset + c.clip.len).next().is_none())
                    };
                    // Nearest first; for pictures the track above before the one below (it shows on top).
                    let mut others: Vec<(usize, &Arc<Track>)> = sequence
                        .tracks
                        .iter()
                        .enumerate()
                        .filter(|(index, t)| t.kind == home.kind && (*index == row || !t.mix.hidden))
                        .collect();
                    others.sort_by_key(|(index, _)| (index.abs_diff(row), *index));
                    let free = others.into_iter().map(|(_, t)| t).find(|t| !taken.contains(&t.id) && fits(t));
                    let track = match free {
                        Some(track) => track.id,
                        None => {
                            let kind = home.kind as usize;
                            let count = sequence.tracks.iter().filter(|t| t.kind == home.kind).count() + added[kind];
                            let (index, name) = match home.kind {
                                TrackKind::Video => (0, format!("V{}", count + 1)),
                                // Right under the video track this paste made, if it made one
                                // (picture and sound of a take stay together); else at the bottom.
                                TrackKind::Audio if added[0] > 0 && added[1] == 0 => (1, format!("A{}", count + 1)),
                                TrackKind::Audio => {
                                    (sequence.tracks.len() + added[0] + added[1], format!("A{}", count + 1))
                                }
                            };
                            added[kind] += 1;
                            let id = session.new_track_id();
                            let track = sg_core::TrackHeader { id, kind: home.kind, name };
                            ops.push(Op::AddTrack { seq, index, track });
                            id
                        }
                    };
                    taken.push(track);
                    for copied in group {
                        let clip = Clip { id: session.new_clip_id(), start: at + copied.offset, ..copied.clip };
                        inserts.push(Op::InsertClip { seq, track, clip });
                        // The copy keeps what the original was given: volume, fades, placement.
                        if let Some(props) = &copied.props {
                            let mut props = props.clone();
                            if props.group != 0 {
                                props.group =
                                    *regrouped.entry(props.group).or_insert_with(|| session.new_clip_id().0.max(1));
                            }
                            inserts.push(Op::SetClipProps { seq, track, clip: clip.id, props: Some(props) });
                        }
                        if let Some(notes) = &copied.notes {
                            inserts.push(Op::SetClipNotes { seq, track, clip: clip.id, notes: Some(notes.clone()) });
                        }
                        placed.push((track, clip));
                    }
                }
                ops.append(&mut inserts);
                if !ops.is_empty() {
                    match session.apply(Op::Batch(std::mem::take(&mut ops)), None) {
                        Ok(()) => {
                            self.selection = placed
                                .iter()
                                .map(|(track, clip)| Sel { track: *track, start: clip.start, id: clip.id })
                                .collect()
                        }
                        Err(_) => *error = Some("No room to paste here.".to_owned()),
                    }
                }
            }
            Action::Nudge(frames) => {
                // The same as dragging the selection that far, without snapping.
                if let Some(primary) = self.selection.first().copied() {
                    let view = ViewerState::default();
                    self.plan_move(&sequence, &view, frames as f64, 0, primary, false, false);
                    let blocked = self.ghosts.iter().any(|g| !g.3);
                    self.ghosts.clear();
                    match self.pending.take() {
                        Some(moves) => {
                            if session.apply(Op::Batch(moves), None).is_ok() {
                                // The selection follows its clips.
                                for sel in &mut self.selection {
                                    if let Some(track) = sequence.track(sel.track) {
                                        let lane = Lane::of(track, &sequence);
                                        sel.start += lane.units(i64::from(frames).abs()) * i64::from(frames).signum();
                                    }
                                }
                            }
                        }
                        None if blocked => *error = Some("No room to move the selection there.".to_owned()),
                        None => {}
                    }
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

    /// Makes a note clip of two bars on an audio track at `frame` (on a
    /// bar line if the sequence has a tempo; shorter if another clip
    /// comes sooner) and asks for its notes to be shown.
    pub fn add_note_clip(
        &mut self,
        session: &mut Session,
        sequence: &Sequence,
        track: &Arc<Track>,
        frame: f64,
        error: &mut Option<String>,
    ) {
        self.add_notes(session, sequence, track, frame, None, Vec::new(), error);
    }

    /// A note clip with these notes in it, as long as they need (in whole
    /// bars; `None`: two bars) from the bar `frame` lies in.
    #[allow(clippy::too_many_arguments)]
    pub fn add_notes(
        &mut self,
        session: &mut Session,
        sequence: &Sequence,
        track: &Arc<Track>,
        frame: f64,
        ticks: Option<i64>,
        notes: Vec<sg_core::Note>,
        error: &mut Option<String>,
    ) {
        let lane = Lane::of(track, sequence);
        let fps = sequence.rate.num() as f64 / sequence.rate.den() as f64;
        let beat = sequence.tempo.map_or(fps / 2.0, |t| t.frames_per_beat(sequence.rate));
        let per_bar = sequence.tempo.map_or(4.0, |t| t.beats_per_bar as f64);
        let bar = beat * per_bar;
        let origin = sequence.tempo.map_or(0.0, |t| t.origin_frames(sequence.rate));
        let from = if sequence.tempo.is_some() { ((frame - origin) / bar).floor() * bar + origin } else { frame };
        let bars =
            ticks.map_or(2.0, |ticks| (ticks as f64 / (sg_core::TICKS_PER_BEAT as f64 * per_bar)).ceil().max(1.0));
        let (start, mut len) = (lane.units(from.max(0.0).round() as i64), lane.units((bar * bars).round() as i64));
        if let Some(next) = track.clips.first_at_or_after(start) {
            len = len.min(next.start - start);
        }
        if len <= 0 || track.clips.at(start).is_some() {
            *error = Some("There is no room for a note clip here.".to_owned());
            return;
        }
        let clip = Clip { id: session.new_clip_id(), asset: AssetId(0), start, len, src_in: 0 };
        let made = Op::Batch(vec![
            Op::InsertClip { seq: sequence.id, track: track.id, clip },
            Op::SetClipNotes { seq: sequence.id, track: track.id, clip: clip.id, notes: Some(notes) },
        ]);
        match session.apply(made, None) {
            Ok(()) => {
                self.selection = vec![Sel { track: track.id, start: clip.start, id: clip.id }];
                self.edit_notes = Some((track.id, clip.id));
            }
            Err(e) => *error = Some(e.to_string()),
        }
    }

    /// Puts the selected clips on the internal clipboard.
    fn copy(&mut self, sequence: &Sequence) {
        let picked: Vec<(usize, Clip)> = self
            .selection
            .iter()
            .filter_map(|s| {
                let row = sequence.tracks.iter().position(|t| t.id == s.track)?;
                Some((row, sequence.tracks[row].clips.get(s.start)?))
            })
            .collect();
        if picked.is_empty() {
            return;
        }
        let first =
            picked.iter().map(|(row, c)| Lane::of(&sequence.tracks[*row], sequence).frame(c.start)).min().unwrap_or(0);
        self.clipboard = picked
            .into_iter()
            .map(|(row, clip)| Copied {
                row,
                clip,
                offset: clip.start - Lane::of(&sequence.tracks[row], sequence).units(first),
                props: sequence.tracks[row].props_of(clip.id).map(|p| Box::new(ClipProps::clone(p))),
                notes: sequence.tracks[row].notes.get(&clip.id).map(|n| Vec::clone(n)),
            })
            .collect();
    }

    /// Whether clips have been copied and can be pasted.
    pub fn has_copied(&self) -> bool {
        !self.clipboard.is_empty()
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
    /// Shows the whole sequence again, as when the project was opened.
    pub fn refit(&mut self) {
        self.fitted = false;
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
    /// Puts a transition of `kind` on the cuts of the selected clips
    /// (feedback F12.1), or takes it off if they all have it already.
    /// Which cuts: those between two selected clips; if no two selected
    /// clips touch, the cut at the start of each selected clip, or at its
    /// end if nothing touches its start. With nothing selected: the cut
    /// nearest to the playhead on the lowest shown video track that has
    /// one nearby. A whip's direction alternates with the transition
    /// before it. Returns the frame of the first cut.
    pub fn toggle_transition(
        &mut self,
        session: &mut Session,
        playhead: i64,
        kind: sg_core::TransitionKind,
        error: &mut Option<String>,
    ) -> Option<i64> {
        let project = session.project().clone();
        let sequence = project.sequences.first()?;
        // A cut is named by its incoming clip: one that starts where another ends.
        let follows =
            |track: &Track, c: &Clip| c.start > 0 && track.clips.at(c.start - 1).is_some_and(|p| p.end() == c.start);
        let chosen: Vec<(Arc<Track>, Clip)> = self
            .selection
            .iter()
            .filter_map(|s| {
                let track = sequence.tracks.iter().find(|t| t.id == s.track && t.kind == TrackKind::Video)?;
                Some((track.clone(), track.clips.get(s.start)?))
            })
            .collect();
        let from_selection = !chosen.is_empty();
        let mut targets: Vec<(Arc<Track>, Clip)> = if from_selection {
            let is_chosen = |track: &Track, c: &Clip| chosen.iter().any(|(t, x)| t.id == track.id && x.id == c.id);
            let between: Vec<(Arc<Track>, Clip)> = chosen
                .iter()
                .filter(|(t, c)| follows(t, c) && t.clips.at(c.start - 1).is_some_and(|p| is_chosen(t, &p)))
                .cloned()
                .collect();
            if !between.is_empty() {
                between
            } else {
                chosen
                    .iter()
                    .filter_map(|(t, c)| {
                        if follows(t, c) {
                            Some((t.clone(), *c))
                        } else {
                            t.clips.at(c.end()).map(|next| (t.clone(), next))
                        }
                    })
                    .collect()
            }
        } else {
            let shown = sequence.tracks.iter().rev().filter(|t| t.kind == TrackKind::Video && !t.mix.hidden);
            let nearest = |track: &Arc<Track>| -> Option<Clip> {
                let here = track.clips.at(playhead).or_else(|| track.clips.last_before(playhead))?;
                let after = track.clips.at(here.end());
                [Some(here), after]
                    .into_iter()
                    .flatten()
                    .filter(|c| follows(track, c))
                    .min_by_key(|c| (c.start - playhead).abs())
            };
            shown
                .filter_map(|t| nearest(t).map(|c| (t.clone(), c)))
                .min_by_key(|(_, c)| (c.start - playhead).abs())
                .into_iter()
                .collect()
        };
        targets.sort_by_key(|(t, c)| (t.id, c.start));
        targets.dedup_by(|a, b| a.0.id == b.0.id && a.1.id == b.1.id);
        if targets.is_empty() {
            *error = Some(
                if from_selection {
                    "The selected clips touch no other clip: a transition sits between two clips that touch."
                } else {
                    "There is no cut here: a transition sits between two clips that touch."
                }
                .to_owned(),
            );
            return None;
        }
        let has = |track: &Track, c: &Clip| track.props_of(c.id).and_then(|p| p.transition);
        // All of them have it: the key takes it off again.
        let remove = targets.iter().all(|(t, c)| has(t, c).is_some_and(|x| x.kind == kind));
        let mut ops = Vec::new();
        let mut last: Option<(TrackId, f32)> = None;
        for (track, clip) in &targets {
            let mut props = track.props_of(clip.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
            props.transition = if remove {
                None
            } else {
                // Opposite to the transition before this one on the track:
                // the one just given in this pass, or the nearest earlier one.
                let previous = match last {
                    Some((id, angle)) if id == track.id => Some(angle),
                    _ => {
                        let mut earlier = track.clips.last_before(clip.start);
                        let mut found = None;
                        for _ in 0..256 {
                            let Some(c) = earlier else { break };
                            found = has(track, &c).map(|p| p.angle);
                            if found.is_some() {
                                break;
                            }
                            earlier = track.clips.last_before(c.start);
                        }
                        found
                    }
                };
                let angle = previous.map_or(0.0, |p| (p + 180.0).rem_euclid(360.0));
                last = Some((track.id, angle));
                Some(sg_core::Transition { angle, ..sg_core::Transition::of(kind) })
            };
            let props = (props != ClipProps::default()).then(|| Box::new(props));
            ops.push(Op::SetClipProps { seq: sequence.id, track: track.id, clip: clip.id, props });
        }
        match session.apply(Op::Batch(ops), None) {
            Ok(()) => {
                // Without a selection the clip of the cut becomes it, so that the inspector shows the transition.
                if !from_selection {
                    self.selection =
                        targets.iter().map(|(t, c)| Sel { track: t.id, start: c.start, id: c.id }).collect();
                }
                targets.iter().map(|(_, c)| c.start).min()
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
/// The speed ramp a sound clip follows: that of the picture of the same
/// take (same asset, same place), if it has one. The player's mix uses the
/// same rule.
fn picture_ramp(sequence: &Sequence, track: &Track, clip: &Clip) -> Option<sg_core::Param> {
    if track.kind != TrackKind::Audio {
        return None;
    }
    let lane = Lane::of(track, sequence);
    let start = lane.frame(clip.start);
    if lane.units(start) != clip.start {
        return None;
    }
    sequence.tracks.iter().filter(|t| t.kind == TrackKind::Video).find_map(|t| {
        let picture = t.clips.get(start).filter(|p| p.asset == clip.asset && lane.units(p.len) == clip.len)?;
        t.props_of(picture.id).map(|p| p.ramp.clone()).filter(|ramp| !ramp.is_one())
    })
}

fn split_ops(
    seq: sg_core::SequenceId,
    track: &Track,
    lane: Lane,
    clip: &Clip,
    at: i64,
    right_id: ClipId,
    picture_ramp: Option<&sg_core::Param>,
) -> Vec<Op> {
    if at <= clip.start || at >= clip.end() {
        return Vec::new();
    }
    let default = ClipProps::default();
    let props = match track.props_of(clip.id) {
        Some(props) => &**props,
        // Nothing to share out, unless the sound follows a ramp.
        None if picture_ramp.is_none() => {
            let mut ops: Vec<Op> = Op::split(seq, track.id, clip, at, right_id).into_iter().collect();
            // Both halves of a note clip have the notes; each plays its own part of them.
            if let Some(notes) = track.notes.get(&clip.id).filter(|_| !ops.is_empty()) {
                ops.push(Op::SetClipNotes { seq, track: track.id, clip: right_id, notes: Some(Vec::clone(notes)) });
            }
            return ops;
        }
        None => &default,
    };
    // How far into the clip the cut is, in sequence frames (the unit of keyframes).
    let cut = lane.frame(at) - lane.frame(clip.start);
    let consumed = match lane.kind {
        TrackKind::Video => props.source_offset(cut),
        TrackKind::Audio => {
            let (num, den) = if props.speed_valid() { props.speed } else { (1, 1) };
            match picture_ramp {
                // The sound follows the picture's ramp: the area under it, in samples.
                Some(ramp) => (ramp.integral(cut) * num as f64 / den as f64 * lane.per_frame()).round() as i64,
                // The set speed times the samples played.
                None => ((at - clip.start) as i128 * num as i128 / den as i128) as i64,
            }
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
    let mut ops = vec![
        Op::SetClip { seq, track: track.id, start: clip.start, clip: left_clip },
        Op::InsertClip { seq, track: track.id, clip: right_clip },
        Op::SetClipProps { seq, track: track.id, clip: clip.id, props: stored(left) },
        Op::SetClipProps { seq, track: track.id, clip: right_id, props: stored(right) },
    ];
    // Both halves of a note clip have the notes; each plays its own part of them.
    if let Some(notes) = track.notes.get(&clip.id) {
        ops.push(Op::SetClipNotes { seq, track: track.id, clip: right_id, notes: Some(Vec::clone(notes)) });
    }
    ops
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

/// What the grid of a sequence with a tempo is made of: lines drawn and
/// what edges snap to. A view setting, not part of the project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridStep {
    /// Quarters of a beat, beats or bars, whichever has room at this zoom.
    #[default]
    Auto,
    Bar,
    /// A note value: 1/`den` of a whole note (a beat is a quarter note);
    /// as a triplet, two thirds of that.
    Note {
        den: u8,
        triplet: bool,
    },
}

impl GridStep {
    pub const ALL: [GridStep; 10] = [
        GridStep::Auto,
        GridStep::Bar,
        GridStep::Note { den: 2, triplet: false },
        GridStep::Note { den: 4, triplet: false },
        GridStep::Note { den: 8, triplet: false },
        GridStep::Note { den: 8, triplet: true },
        GridStep::Note { den: 16, triplet: false },
        GridStep::Note { den: 16, triplet: true },
        GridStep::Note { den: 32, triplet: false },
        GridStep::Note { den: 32, triplet: true },
    ];

    pub fn label(self) -> String {
        match self {
            GridStep::Auto => "auto".to_owned(),
            GridStep::Bar => "bar".to_owned(),
            GridStep::Note { den, triplet } => format!("1/{den}{}", if triplet { "T" } else { "" }),
        }
    }

    /// The step in beats; `None`: by zoom.
    fn beats(self, beats_per_bar: u8) -> Option<f64> {
        match self {
            GridStep::Auto => None,
            GridStep::Bar => Some(beats_per_bar as f64),
            GridStep::Note { den, triplet } => Some(4.0 / den as f64 * if triplet { 2.0 / 3.0 } else { 1.0 }),
        }
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
            // The grid of the song before is not this one's.
            self.grid = Arc::default();
            self.grid_for = None;
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
    /// Where the first beat that was found in the song lies on the
    /// timeline, in samples: through the first clip of the song that plays it.
    pub fn first_beat(&self, sequence: &Sequence) -> Option<i64> {
        let song = sequence.song?;
        let first = *self.grid.first()?;
        sequence
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            // (Looked for among the first clips of each track only: this is asked every frame the
            // song's settings are on screen, and a song lies near the start of its track.)
            // (A clip trimmed past that beat still says where it would lie: bars are counted on from there.)
            .filter_map(|t| t.clips.iter().take(64).find(|c| c.asset == song.asset))
            .map(|c| c.start + first - c.src_in)
            .min()
    }

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
        // A tempo is a grid by itself: beats, or only bars where beats would crowd.
        if let Some(tempo) = sequence.tempo {
            let beat = tempo.frames_per_beat(sequence.rate);
            let bar = beat * tempo.beats_per_bar as f64;
            let roomy = |unit: f64| unit / self.frames_per_point >= min_points as f64;
            // The chosen step if its lines have room, else beats, bars, then every second, fourth, ... bar.
            let chosen = self.grid_step.beats(tempo.beats_per_bar).map(|beats| beat * beats);
            let mut unit = chosen.into_iter().chain([beat, bar]).find(|u| roomy(*u)).unwrap_or(bar);
            while !roomy(unit) && unit < bar * (1 << 20) as f64 {
                unit *= 2.0;
            }
            let origin = tempo.origin_frames(sequence.rate);
            let mut index = ((f0.max(0.0) - origin) / unit).ceil();
            let mut lines = Vec::new();
            while origin + index * unit < f1 && lines.len() < 4096 {
                let along = index * unit;
                // (The caller draws every fourth line strongly: bars count as that.)
                let on_bar = ((along / bar).round() * bar - along).abs() < 1e-6 * bar;
                lines.push((origin + along, if on_bar { 0 } else { 1 }));
                index += 1.0;
            }
            return lines;
        }
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
        // With a tempo: the nearest quarter of a beat while those are far
        // enough apart to aim at, else the nearest beat, else the nearest bar.
        if let Some(tempo) = sequence.tempo {
            let beat = tempo.frames_per_beat(sequence.rate);
            let roomy = |unit: f64| unit / self.frames_per_point >= SNAP as f64 * 2.0;
            let bar = beat * tempo.beats_per_bar as f64;
            let unit = match self.grid_step.beats(tempo.beats_per_bar) {
                // A chosen grid is kept at every zoom: it was asked for.
                Some(beats) => beat * beats,
                None => [beat / 4.0, beat].into_iter().find(|u| roomy(*u)).unwrap_or(bar),
            };
            let origin = tempo.origin_frames(sequence.rate);
            let line = ((frame - origin) / unit).round() * unit + origin;
            // (The line before the timeline's start is not one to snap to.)
            return Some(if line < 0.0 { line + unit } else { line });
        }
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
        // Pasting onto something covers nothing: there is no other audio
        // track, so one is made for the copy. One undo takes both back.
        let mut error = None;
        timeline.act(Action::PasteClips, &mut session, 1600, &mut error);
        assert_eq!(error, None);
        assert_eq!(layout(&session).1, vec![(0, 600), (600, 600), (1500, 600)]);
        let lanes = |session: &Session| -> Vec<(String, Vec<i64>)> {
            let sequence = &session.project().sequences[0];
            let audio = sequence.tracks.iter().filter(|t| t.kind == TrackKind::Audio);
            audio
                .map(|t| (t.name.clone(), t.clips.iter().map(|c| Lane::of(t, sequence).frame(c.start)).collect()))
                .collect()
        };
        assert_eq!(lanes(&session), vec![("A1".to_owned(), vec![0, 600, 1500]), ("A2".to_owned(), vec![1600])]);
        // Again where only the new track has room: no third one is made.
        timeline.act(Action::PasteClips, &mut session, 900, &mut error);
        assert_eq!(lanes(&session)[1], ("A2".to_owned(), vec![900, 1600]));
        timeline.act(Action::PasteClips, &mut session, 1700, &mut error);
        assert_eq!(lanes(&session).len(), 3);
        for _ in 0..3 {
            session.undo().unwrap();
        }
        assert_eq!(lanes(&session), vec![("A1".to_owned(), vec![0, 600, 1500])]);
        // Cut copies and takes out, leaving the gap; the copy carries what the clip was given.
        let project = session.project().clone();
        let seq = &project.sequences[0];
        let audio = seq.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap();
        let clip = audio.clips.at(Lane::of(audio, seq).units(1500)).unwrap();
        let props = ClipProps { gain_db: sg_core::Param::constant(-6.0), fade_in: 12, ..ClipProps::default() };
        let op = Op::SetClipProps { seq: seq.id, track: audio.id, clip: clip.id, props: Some(Box::new(props.clone())) };
        session.apply(op, None).unwrap();
        let project = session.project().clone();
        timeline.select(&project.sequences[0], audio, &clip, false, true);
        act(&mut timeline, &mut session, Action::Cut, 0);
        assert_eq!(layout(&session).1, vec![(0, 600), (600, 600)]);
        act(&mut timeline, &mut session, Action::PasteClips, 3000);
        assert_eq!(layout(&session).1, vec![(0, 600), (600, 600), (3000, 600)]);
        let project = session.project().clone();
        let audio = project.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap();
        let pasted = audio.clips.at(Lane::of(audio, &project.sequences[0]).units(3000)).unwrap();
        assert_eq!(audio.props_of(pasted.id).map(|p| ClipProps::clone(p)), Some(props));
        assert!(timeline.is_selected(audio.id, &pasted));

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

        // The arrow keys move the selection by frames, picture and sound together.
        let mut error = None;
        timeline.act(Action::Nudge(10), &mut session, 0, &mut error);
        timeline.act(Action::Nudge(-1), &mut session, 0, &mut error);
        assert_eq!(layout(&session), (vec![(0, 600), (609, 600)], vec![(0, 600), (609, 600)]));
        assert_eq!(timeline.selection[0].start, 609);
        timeline.act(Action::Nudge(-10), &mut session, 0, &mut error);
        assert_eq!(
            (layout(&session).0, error.take()),
            (vec![(0, 600), (609, 600)], Some("No room to move the selection there.".to_owned()))
        );
        timeline.act(Action::Nudge(-9), &mut session, 0, &mut error);
        assert_eq!(layout(&session), (vec![(0, 600), (600, 600)], vec![(0, 600), (600, 600)]));
        // Onto the first take: no room.
        timeline.plan_move(&seq, &view, -300.0, 0, primary, false, false);
        assert!(timeline.pending.is_none());
        assert!(timeline.ghosts.iter().any(|g| !g.3));
        // With Ctrl it goes over the first take, which keeps its first half
        // (picture and sound alike); undo brings everything back.
        timeline.plan_move(&seq, &view, -300.0, 0, primary, false, true);
        assert!(timeline.ghosts.iter().all(|g| g.3));
        let (removes, over) = (timeline.pending.take().unwrap(), std::mem::take(&mut timeline.pending_over));
        session.move_over(removes, over).unwrap();
        assert_eq!(layout(&session), (vec![(0, 300), (300, 600)], vec![(0, 300), (300, 600)]));
        session.undo().unwrap();
        assert_eq!(layout(&session), (vec![(0, 600), (600, 600)], vec![(0, 600), (600, 600)]));
        // To the right by 100 frames: both clips move, as one undo step.
        timeline.plan_move(&seq, &view, 100.0, 0, primary, false, false);
        let ops = timeline.pending.take().unwrap();
        session.apply(Op::Batch(ops), None).unwrap();
        assert_eq!(layout(&session), (vec![(0, 600), (700, 600)], vec![(0, 600), (700, 600)]));
        // Snapping pulls a near miss onto the neighbour's edge.
        let seq = session.project().sequences[0].clone();
        let moved = seq.tracks[0].clips.get(700).unwrap();
        timeline.select(&seq, &seq.tracks[0], &moved, false, false);
        timeline.plan_move(&seq, &view, -96.0, 0, timeline.selection[0], true, false);
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
                // The new pair lies together on top: the sound right under its picture.
                vec![
                    (TrackKind::Video, "V2".to_owned(), vec![100]),
                    (TrackKind::Audio, "A2".to_owned(), vec![100]),
                    (TrackKind::Video, "V1".to_owned(), vec![0, 600]),
                    (TrackKind::Audio, "A1".to_owned(), vec![0, 600]),
                ]
            );
            assert!(session.undo().unwrap());
            assert_eq!(*session.project(), with_asset);
        }
        // With two pairs there: a take stays within one pair. Where both are
        // free it takes the lower one; pointed at the sound track of the
        // upper one, that pair; where neither has room, a third pair is made.
        session.drop_asset(&asset, 100, None, None).unwrap();
        let seq = session.project().sequences[0].clone();
        let pair = |row, frame| {
            let planned = place_at(&seq, &asset, frame, row, None);
            (planned.video, planned.audio)
        };
        assert_eq!(pair(None, 2000), (Some(Slot::Row(2)), Some(Slot::Row(3))));
        assert_eq!(pair(Some(1), 2000), (Some(Slot::Row(0)), Some(Slot::Row(1))));
        assert_eq!(pair(None, 150), (Some(Slot::New), Some(Slot::New)));
        // Only the lower pair is taken at 1300: the take goes to the upper one, whole.
        assert_eq!(pair(Some(2), 1150), (Some(Slot::Row(0)), Some(Slot::Row(1))));
        for _ in 0..2 {
            assert!(session.undo().unwrap());
        }
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
        // A cut made before later ones reaches to the next cut and leaves the rest.
        assert!(session.switch_angle(1, 100, group).unwrap());
        assert_eq!(
            programme(&session),
            vec![(1, 0, 100, 0), (2, 100, 100, 40), (2, 200, 100, 140), (1, 300, 300, 300), (2, 600, 60, 540)]
        );
        // One pass of switching is one undo step.
        assert!(session.undo().unwrap());
        assert_eq!(programme(&session), vec![(1, 0, 600, 0)]);
    }

    #[test]
    fn a_transition_goes_on_the_cuts_of_the_selected_clips_not_to_the_playhead() {
        use sg_core::TransitionKind::{Black, Whip};
        let (_dir, mut session) = session();
        let mut timeline = Timeline::default();
        // Three pictures in a row: 0..300, 300..600, 600..1200.
        act(&mut timeline, &mut session, Action::Split, 300);
        let kinds = |session: &Session| -> Vec<Option<sg_core::TransitionKind>> {
            let track = &session.project().sequences[0].tracks[0];
            track.clips.iter().map(|c| track.props_of(c.id).and_then(|p| p.transition).map(|t| t.kind)).collect()
        };
        let pick = |timeline: &mut Timeline, session: &Session, starts: &[i64]| {
            let project = session.project().clone();
            let track = &project.sequences[0].tracks[0];
            timeline.selection = starts
                .iter()
                .map(|s| Sel { track: track.id, start: *s, id: track.clips.get(*s).unwrap().id })
                .collect();
        };
        let mut error = None;
        // The middle one selected, the playhead far away near the last cut: the cut at its start.
        pick(&mut timeline, &session, &[300]);
        assert_eq!(timeline.toggle_transition(&mut session, 610, Whip, &mut error), Some(300));
        assert_eq!(kinds(&session), [None, Some(Whip), None]);
        // Two that touch: the cut between them, and only that one.
        pick(&mut timeline, &session, &[300, 600]);
        assert_eq!(timeline.toggle_transition(&mut session, 0, Black, &mut error), Some(600));
        assert_eq!(kinds(&session), [None, Some(Whip), Some(Black)]);
        // The first has nothing before it: the cut at its end. Another kind replaces the one there.
        pick(&mut timeline, &session, &[0]);
        assert_eq!(timeline.toggle_transition(&mut session, 1100, Black, &mut error), Some(300));
        assert_eq!(kinds(&session), [None, Some(Black), Some(Black)]);
        // All three: both cuts between them have it already, so it comes off both, as one step.
        pick(&mut timeline, &session, &[0, 300, 600]);
        timeline.toggle_transition(&mut session, 0, Black, &mut error);
        assert_eq!(kinds(&session), [None, None, None]);
        assert!(session.undo().unwrap());
        assert_eq!(kinds(&session), [None, Some(Black), Some(Black)]);
        assert_eq!(error, None);
        // Nothing selected: the cut nearest the playhead, as before.
        timeline.selection.clear();
        assert_eq!(timeline.toggle_transition(&mut session, 610, Whip, &mut error), Some(600));
        assert_eq!(kinds(&session), [None, Some(Black), Some(Whip)]);
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
    fn splitting_a_ramped_take_keeps_its_sound_with_the_picture() {
        use sg_core::{Key, Param};
        let (_dir, mut session) = session();
        let project = session.project().clone();
        let seq = project.sequences[0].clone();
        let (track, clip) = (seq.tracks[0].clone(), seq.tracks[0].clips.at(0).unwrap());
        // Half speed throughout, on the picture; the sound has no properties.
        let props = ClipProps {
            ramp: Param { value: 1.0, keys: vec![Key::new(0, 0.5), Key::new(600, 0.5)] },
            ..ClipProps::default()
        };
        let set = Op::SetClipProps { seq: seq.id, track: track.id, clip: clip.id, props: Some(Box::new(props)) };
        session.apply(set, None).unwrap();
        let mut timeline = Timeline::default();
        let seq = session.project().sequences[0].clone();
        timeline.select(&seq, &track, &clip, false, false);
        act(&mut timeline, &mut session, Action::Split, 200);
        let project = session.project().clone();
        let (pictures, sounds) = (&project.sequences[0].tracks[0], &project.sequences[0].tracks[1]);
        let picture = pictures.clips.at(200).unwrap();
        let sound = sounds.clips.at(200 * 800).unwrap();
        // Both tails go on from source frame 100: 200 frames at half speed.
        assert_eq!((picture.start, picture.src_in), (200, 100));
        assert_eq!((sound.start, sound.src_in), (200 * 800, 100 * 800));
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
        let song = session.project().sequences[0].song.unwrap();
        let mut timeline = Timeline {
            grid: Arc::new((0..120).map(|n| n * 24_000).collect()),
            song_beats: Some((song.asset, None)),
            grid_for: Some((song.asset, song.tempo)),
            ..Timeline::default()
        };
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
