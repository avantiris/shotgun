//! The viewer.

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::time::SAMPLE_RATE;
use sg_core::{Rate, Sequence, TrackKind};

use crate::theme;

/// What the preview engine currently shows; filled in by the application.
#[derive(Default)]
pub struct ViewerState {
    pub texture: Option<egui::TextureId>,
    /// Playhead in sequence frames.
    pub frame: i64,
    pub playing: bool,
    /// Peak level per audio track since the last frame (linear), and of the mix.
    pub meters: Vec<f32>,
    pub master_meter: f32,
    /// Decibels the master limiter took off since the last frame.
    pub limiting_db: f32,
    /// The camera angles side by side, while the sequence has camera tracks.
    pub angles: Option<AnglesView>,
    /// Lines for the performance overlay.
    pub stats: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Transport {
    Toggle,
    Seek(i64),
    Step(i64),
}

/// `HH:MM:SS:FF` at the rate's nominal frames per second (no drop-frame).
pub fn timecode(frame: i64, rate: Rate) -> String {
    let fps = (rate.num() as i64 + rate.den() as i64 - 1) / rate.den() as i64;
    let frame = frame.max(0);
    let (secs, ff) = (frame / fps.max(1), frame % fps.max(1));
    format!("{:02}:{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60, ff)
}

/// End of the last clip, in frames.
fn sequence_end(sequence: &Sequence) -> i64 {
    sequence
        .tracks
        .iter()
        .map(|t| match t.kind {
            TrackKind::Video => t.clips.end(),
            TrackKind::Audio => sequence.rate.samples_to_frames(t.clips.end(), SAMPLE_RATE) + 1,
        })
        .max()
        .unwrap_or(0)
}

/// Draws the viewer. Returns how far the picture was dragged this frame, in
/// frame widths and heights.
/// The texture of the camera-angle monitor and how it is laid out.
#[derive(Clone, Copy)]
pub struct AnglesView {
    pub texture: egui::TextureId,
    pub count: usize,
    pub cols: u32,
    pub rows: u32,
}

/// What happened in the viewer this frame.
#[derive(Default)]
pub struct ViewerEvents {
    /// The picture was dragged by this much, in fractions of the frame.
    pub dragged: Option<egui::Vec2>,
    /// A camera angle was clicked (0 is the first).
    pub angle: Option<usize>,
}

/// The viewer: the programme picture with the transport line below it and,
/// when the sequence has camera angles, those side by side underneath.
/// `live` is the angle the programme shows at the playhead.
pub fn viewer(
    ui: &mut egui::Ui,
    sequence: &Sequence,
    view: &ViewerState,
    recording: bool,
    live: Option<usize>,
) -> ViewerEvents {
    const BAR_H: f32 = 22.0;
    let mut events = ViewerEvents::default();
    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::drag());
    let aspect = sequence.width as f32 / sequence.height.max(1) as f32;
    // The angles take a strip at the bottom: one row of them about a
    // quarter of the height, never more than half.
    let strip = view.angles.map_or(0.0, |a| {
        let row = (available.height() * 0.26).clamp(70.0, 190.0);
        (row * a.rows as f32).min(available.height() * 0.5) + 8.0
    });
    let picture_area = Rect::from_min_max(available.min, pos2(available.max.x, available.max.y - BAR_H - strip));
    let size = if picture_area.width() / picture_area.height().max(1.0) > aspect {
        vec2(picture_area.height() * aspect, picture_area.height())
    } else {
        vec2(picture_area.width(), picture_area.width() / aspect)
    };
    let rect = Rect::from_center_size(picture_area.center(), size);
    let painter = ui.painter().clone();
    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    if let Some(texture) = view.texture {
        painter.image(texture, rect, whole, Color32::WHITE);
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, theme::LINE), StrokeKind::Outside);
    let y = picture_area.bottom() + BAR_H / 2.0;
    let state = if view.playing { "playing" } else { "paused" };
    let text =
        format!("{}  /  {}", timecode(view.frame, sequence.rate), timecode(sequence_end(sequence), sequence.rate));
    painter.text(pos2(available.center().x, y), Align2::CENTER_CENTER, text, FontId::monospace(12.0), theme::TEXT);
    painter.text(pos2(rect.right(), y), Align2::RIGHT_CENTER, state, FontId::proportional(11.0), theme::DIM);
    if recording {
        painter.text(
            pos2(rect.left(), y),
            Align2::LEFT_CENTER,
            "REC: drag the picture while it plays",
            FontId::proportional(11.0),
            theme::ERROR,
        );
    }
    if let Some(angles) = view.angles {
        let area = Rect::from_min_max(pos2(available.min.x, available.max.y - strip + 8.0), available.max);
        let grid_aspect = aspect * angles.cols as f32 / angles.rows as f32;
        let grid_size = if area.width() / area.height().max(1.0) > grid_aspect {
            vec2(area.height() * grid_aspect, area.height())
        } else {
            vec2(area.width(), area.width() / grid_aspect)
        };
        let grid = Rect::from_center_size(area.center(), grid_size);
        painter.rect_filled(grid, 0.0, Color32::BLACK);
        painter.image(angles.texture, grid, whole, Color32::WHITE);
        let cell = vec2(grid.width() / angles.cols as f32, grid.height() / angles.rows as f32);
        for n in 0..angles.count {
            let (col, row) = (n as u32 % angles.cols, n as u32 / angles.cols);
            let at = Rect::from_min_size(grid.min + vec2(col as f32 * cell.x, row as f32 * cell.y), cell);
            let on_air = live == Some(n);
            let tag = Rect::from_min_size(at.min + vec2(4.0, 4.0), vec2(20.0, 18.0));
            painter.rect_filled(tag, 3.0, if on_air { theme::ACCENT } else { theme::BG.gamma_multiply(0.85) });
            let colour = if on_air { theme::BG } else { theme::TEXT };
            painter.text(tag.center(), Align2::CENTER_CENTER, format!("{}", n + 1), FontId::proportional(12.0), colour);
            let stroke = if on_air { Stroke::new(2.0, theme::ACCENT) } else { Stroke::new(1.0, theme::LINE) };
            painter.rect_stroke(at, 0.0, stroke, StrokeKind::Inside);
            // A click on an angle cuts to it, like its number key.
            let hit = ui.interact(at, ui.scope_id().with(("angle", n)), Sense::click());
            if hit.on_hover_text(format!("Cut to camera {} here (key {})", n + 1, n + 1)).clicked() {
                events.angle = Some(n);
            }
        }
    }
    let delta = response.drag_delta();
    let inside = response.interact_pointer_pos().is_some_and(|p| p.y < picture_area.bottom());
    events.dragged = (response.dragged() && inside && delta != egui::Vec2::ZERO)
        .then(|| vec2(delta.x / size.x.max(1.0), delta.y / size.y.max(1.0)));
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes() {
        assert_eq!(timecode(0, Rate::FPS_60), "00:00:00:00");
        assert_eq!(timecode(60 * 3725 + 7, Rate::FPS_60), "01:02:05:07");
        // 29.97 counts 30 frames per timecode second (non-drop).
        assert_eq!(timecode(31, "30000/1001".parse().unwrap()), "00:00:01:01");
        assert_eq!(timecode(-5, Rate::FPS_25), "00:00:00:00");
    }
}
