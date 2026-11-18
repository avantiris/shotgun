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
pub fn viewer(ui: &mut egui::Ui, sequence: &Sequence, view: &ViewerState, recording: bool) -> Option<egui::Vec2> {
    const BAR_H: f32 = 22.0;
    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::drag());
    let picture_area = Rect::from_min_max(available.min, pos2(available.max.x, available.max.y - BAR_H));
    let aspect = sequence.width as f32 / sequence.height.max(1) as f32;
    let size = if picture_area.width() / picture_area.height().max(1.0) > aspect {
        vec2(picture_area.height() * aspect, picture_area.height())
    } else {
        vec2(picture_area.width(), picture_area.width() / aspect)
    };
    let rect = Rect::from_center_size(picture_area.center(), size);
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::BLACK);
    if let Some(texture) = view.texture {
        painter.image(texture, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, theme::LINE), StrokeKind::Outside);
    let y = available.max.y - BAR_H / 2.0;
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
    let delta = response.drag_delta();
    (response.dragged() && delta != egui::Vec2::ZERO)
        .then(|| vec2(delta.x / size.x.max(1.0), delta.y / size.y.max(1.0)))
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
