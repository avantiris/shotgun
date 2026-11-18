//! The viewer and the timeline strip under it.
//!
//! The timeline here shows the whole sequence at a fixed fit and draws its
//! clips with the egui painter: enough to place, play and scrub in M3. The
//! zoomable, instanced timeline of 02-architecture.md replaces it in M4.

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::time::SAMPLE_RATE;
use sg_core::{Project, Rate, Sequence, TrackKind};

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

pub fn viewer(ui: &mut egui::Ui, sequence: &Sequence, view: &ViewerState) {
    const BAR_H: f32 = 22.0;
    let available = ui.available_rect_before_wrap();
    ui.allocate_rect(available, Sense::hover());
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
}

pub fn timeline(
    ui: &mut egui::Ui,
    project: &Project,
    sequence: &Sequence,
    view: &ViewerState,
    out: &mut Vec<Transport>,
) {
    const HEADER_W: f32 = 120.0;
    const RULER_H: f32 = 20.0;
    const ROW_H: f32 = 34.0;
    let rect = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);
    let lanes_left = rect.left() + HEADER_W;
    let lanes_w = (rect.width() - HEADER_W).max(1.0);

    // The whole sequence plus some room, never less than 20 seconds.
    let rate = sequence.rate;
    let fps = rate.num() as f64 / rate.den() as f64;
    let end = sequence_end(sequence);
    let span = ((end as f64 * 1.15).max(fps * 20.0)).ceil();
    let per_frame = lanes_w as f64 / span;
    let x_of = |frame: i64| lanes_left + (frame as f64 * per_frame) as f32;

    let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), RULER_H));
    painter.rect_filled(ruler, 0.0, theme::PANEL);
    // A tick about every 90 points, on a round number of seconds.
    let secs_per_tick = [1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0, 3600.0]
        .into_iter()
        .find(|s| s * fps * per_frame >= 90.0)
        .unwrap_or(3600.0);
    let mut tick = 0.0;
    while tick * fps <= span {
        let frame = (tick * fps).round() as i64;
        let x = x_of(frame);
        painter.line_segment([pos2(x, ruler.top() + 12.0), pos2(x, ruler.bottom())], Stroke::new(1.0, theme::LINE));
        painter.text(
            pos2(x + 3.0, ruler.top() + 2.0),
            Align2::LEFT_TOP,
            &timecode(frame, rate)[..8],
            FontId::monospace(10.0),
            theme::DIM,
        );
        tick += secs_per_tick;
    }

    for (i, track) in sequence.tracks.iter().enumerate() {
        let top = ruler.bottom() + i as f32 * ROW_H;
        let lane = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), ROW_H - 1.0));
        painter.rect_filled(lane, 0.0, if i % 2 == 0 { theme::PANEL } else { theme::RAISED.gamma_multiply(0.8) });
        let colour = match track.kind {
            TrackKind::Video => theme::VIDEO,
            TrackKind::Audio => theme::AUDIO,
        };
        painter.rect_filled(Rect::from_min_size(lane.min, vec2(3.0, lane.height())), 0.0, colour);
        painter.text(
            pos2(lane.left() + 12.0, lane.center().y),
            Align2::LEFT_CENTER,
            &track.name,
            FontId::proportional(13.0),
            theme::TEXT,
        );
        let to_frames = |t: i64| match track.kind {
            TrackKind::Video => t,
            TrackKind::Audio => rate.samples_to_frames(t, SAMPLE_RATE),
        };
        for clip in track.clips.iter() {
            let (x0, x1) = (x_of(to_frames(clip.start)), x_of(to_frames(clip.end())));
            let body = Rect::from_min_max(pos2(x0, lane.top() + 2.0), pos2(x1.max(x0 + 1.0), lane.bottom() - 2.0));
            painter.rect_filled(body, 3.0, colour.gamma_multiply(0.55));
            painter.rect_stroke(body, 3.0, Stroke::new(1.0, colour), StrokeKind::Inside);
            if body.width() > 40.0 {
                let label = project.assets.get(&clip.asset).map_or("missing asset", |a| a.label.as_str());
                painter.with_clip_rect(body.shrink(3.0)).text(
                    pos2(body.left() + 5.0, body.center().y),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(11.0),
                    theme::TEXT,
                );
            }
        }
    }
    painter
        .line_segment([pos2(lanes_left, rect.top()), pos2(lanes_left, rect.bottom())], Stroke::new(1.0, theme::LINE));
    let x = x_of(view.frame);
    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, theme::ACCENT));

    // Click or drag anywhere on the lanes moves the playhead (scrub).
    if response.is_pointer_button_down_on()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let frame = (((pointer.x - lanes_left) as f64 / per_frame).round() as i64).clamp(0, end.max(0));
        if frame != view.frame {
            out.push(Transport::Seek(frame));
        }
    }
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
