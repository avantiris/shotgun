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

use std::sync::{Arc, Mutex, PoisonError};

use egui::{Align2, Color32, FontId, PointerButton, Rect, Sense, Stroke, StrokeKind, pos2};
use sg_core::time::SAMPLE_RATE;
use sg_core::{Asset, Clip, ClipId, Marker, Op, Project, Rate, Sequence, Track, TrackId, TrackKind, Visible};

use crate::keymap::Action;
use crate::session::Session;
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
            quads: 0,
        }
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
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
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
        self.paint(ui, rect, lanes, &project, &sequence, view, &x_of);
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

        for (row, track) in sequence.tracks.iter().enumerate() {
            let top = rect.top() + RULER_H + row as f32 * row_h;
            if top > rect.bottom() {
                break;
            }
            let shade = if row % 2 == 0 { theme::PANEL } else { theme::RAISED.gamma_multiply(0.8) };
            instances.push(Instance::new(quad(rect.left(), top, rect.right(), top + row_h - 1.0), shade, 0.0, 0.0));
            let colour = kind_colour(track.kind);
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
                    if selected {
                        instances.push(Instance::new(quad(x0, y1 - 2.0, x1, y1), theme::ACCENT, 0.0, 0.0));
                    }
                    if x1 - x0 >= LABEL_MIN && row_h >= 16.0 && labels.len() < 400 {
                        let name = project.assets.get(&clip.asset).map_or("missing asset", |a| a.label.as_str());
                        labels.push((
                            x0.max(lanes.left()) + 5.0,
                            (y0 + y1) / 2.0,
                            x1 - x0.max(lanes.left()) - 8.0,
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
                theme::TEXT,
            );
        }
        self.paint_ruler(&painter, rect, lanes, sequence, x_of);
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
                    ops.extend(Op::split(seq, track.id, &clip, at, session.new_clip_id()));
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
            session.place_asset(&asset, 0).unwrap();
        }
        (dir, session)
    }

    /// (start, length) of every clip, video in frames and audio in frames too.
    fn layout(session: &Session) -> (Vec<(i64, i64)>, Vec<(i64, i64)>) {
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
        let mut timeline = Timeline::default();
        timeline.frames_per_point = 1.0;
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
}
