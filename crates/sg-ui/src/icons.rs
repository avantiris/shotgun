//! Small drawn icons for the action bars (feedback F15.5): a button that
//! is a picture, with the name of what it does and its key in the tooltip.
//! Drawn with lines rather than taken from a font, so that every one of
//! them is there on every machine.

use egui::{Color32, Pos2, Rect, Shape, Stroke, vec2};

use crate::keymap::{self, Action};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Record,
    Metronome,
    Loop,
    Title,
    Marker,
    Group,
    Checkpoint,
    History,
    Export,
    Keys,
    Plugins,
    Settings,
}

/// Draws `icon` into a square of 16 points around `centre`.
fn paint(painter: &egui::Painter, icon: Icon, centre: Pos2, colour: Color32) {
    let line = Stroke::new(1.5, colour);
    let at = |x: f32, y: f32| centre + vec2(x, y);
    let lines = |points: &[(f32, f32)]| {
        painter.add(Shape::line(points.iter().map(|(x, y)| at(*x, *y)).collect(), line));
    };
    match icon {
        Icon::Record => {
            painter.circle_filled(centre, 5.0, colour);
        }
        Icon::Metronome => {
            // The case, narrower at the top, and the pendulum leaning out of it.
            lines(&[(-5.0, 7.0), (-2.5, -7.0), (2.5, -7.0), (5.0, 7.0), (-5.0, 7.0)]);
            lines(&[(0.0, 4.0), (4.5, -5.0)]);
        }
        Icon::Loop => {
            // A track that comes back to its start, with the arrow that says which way.
            painter.rect_stroke(Rect::from_center_size(centre, vec2(14.0, 9.0)), 4.5, line, egui::StrokeKind::Middle);
            painter.add(Shape::convex_polygon(vec![at(1.0, -7.5), at(1.0, -1.5), at(5.0, -4.5)], colour, Stroke::NONE));
        }
        Icon::Title => {
            lines(&[(-5.0, -6.0), (5.0, -6.0)]);
            lines(&[(0.0, -6.0), (0.0, 6.5)]);
        }
        Icon::Marker => {
            // As on the ruler: a small flag pointing down at its frame.
            painter.add(Shape::convex_polygon(
                vec![at(-4.5, -6.5), at(4.5, -6.5), at(4.5, 0.0), at(0.0, 5.5), at(-4.5, 0.0)],
                colour,
                Stroke::NONE,
            ));
        }
        Icon::Group => {
            let square = |x: f32, y: f32| Rect::from_center_size(at(x, y), vec2(8.5, 8.5));
            painter.rect_stroke(square(-2.5, -2.5), 1.5, line, egui::StrokeKind::Middle);
            painter.rect_stroke(square(2.5, 2.5), 1.5, line, egui::StrokeKind::Middle);
        }
        Icon::Checkpoint => {
            // A bookmark.
            lines(&[(-4.0, -7.0), (4.0, -7.0), (4.0, 7.0), (0.0, 3.0), (-4.0, 7.0), (-4.0, -7.0)]);
        }
        Icon::History => {
            painter.circle_stroke(centre, 6.5, line);
            lines(&[(0.0, -4.0), (0.0, 0.0), (3.0, 2.0)]);
        }
        Icon::Export => {
            // Out of the tray, upwards.
            lines(&[(-6.0, 1.5), (-6.0, 6.5), (6.0, 6.5), (6.0, 1.5)]);
            lines(&[(0.0, 3.0), (0.0, -6.5)]);
            lines(&[(-3.5, -3.0), (0.0, -6.5), (3.5, -3.0)]);
        }
        Icon::Keys => {
            // A keyboard: its outline and a few keys.
            painter.rect_stroke(Rect::from_center_size(centre, vec2(15.0, 10.0)), 2.0, line, egui::StrokeKind::Middle);
            for x in [-4.0, 0.0, 4.0] {
                painter.circle_filled(at(x, -1.5), 0.9, colour);
            }
            lines(&[(-3.5, 2.2), (3.5, 2.2)]);
        }
        Icon::Plugins => {
            // A plug: two pins, the body, the lead.
            lines(&[(-2.5, -7.0), (-2.5, -3.0)]);
            lines(&[(2.5, -7.0), (2.5, -3.0)]);
            painter.rect_stroke(Rect::from_min_max(at(-5.0, -3.0), at(5.0, 2.5)), 2.0, line, egui::StrokeKind::Middle);
            lines(&[(0.0, 2.5), (0.0, 7.0)]);
        }
        Icon::Settings => {
            // A cog: a ring and eight teeth.
            painter.circle_stroke(centre, 3.8, line);
            for step in 0..8 {
                let angle = step as f32 * std::f32::consts::FRAC_PI_4;
                let (sin, cos) = angle.sin_cos();
                painter.line_segment([at(cos * 5.2, sin * 5.2), at(cos * 7.3, sin * 7.3)], Stroke::new(2.0, colour));
            }
        }
    }
}

/// What the tooltip of a button says: its name, what it does, its key.
fn tip(name: &str, what: Option<&str>, key: &str) -> String {
    let mut text = name.to_owned();
    if let Some(what) = what.filter(|what| !what.eq_ignore_ascii_case(name)) {
        text.push_str(&format!(": {what}"));
    }
    if !key.is_empty() {
        text.push_str(&format!("  ({key})"));
    }
    text
}

/// A button that is an icon. `name` is what it is called (in the tooltip,
/// and for whoever looks for it by name); `on` draws it as switched on.
pub fn button(
    ui: &mut egui::Ui,
    icon: Icon,
    name: &str,
    what: Option<&str>,
    key: &str,
    enabled: bool,
    on: bool,
) -> bool {
    let size = vec2(28.0, 24.0);
    let fill = if on { theme::ACCENT } else { theme::RAISED };
    let response = ui.add_enabled(enabled, egui::Button::new("").min_size(size).fill(fill));
    let colour = match (enabled, on) {
        (false, _) => theme::LINE,
        (true, true) => theme::BG,
        (true, false) if icon == Icon::Record => theme::ERROR,
        (true, false) => theme::TEXT,
    };
    paint(ui.painter(), icon, response.rect.center(), colour);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, name));
    response.on_hover_text(tip(name, what, key)).on_disabled_hover_text(tip(name, what, key)).clicked()
}

/// The same for an action of the key map: its description and key come from there.
pub fn action(ui: &mut egui::Ui, icon: Icon, action: Action, name: &str, enabled: bool, on: bool) -> bool {
    let what = keymap::describe(action).map(|(_, what)| what);
    button(ui, icon, name, what, &keymap::hint(action), enabled, on)
}
