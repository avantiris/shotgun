//! Colours, edges and spacing in one place.
//!
//! The look (feedback N2.3, 2026-11-22, `PROPOSED` in its details): flat
//! surfaces with hard edges. A control is a dark face with a one-pixel
//! border and nearly square corners; pointing at it lights the border,
//! pressing it fills it with the accent, and what is switched on carries
//! the accent as its border. Nothing is rounded like a pill and nothing
//! grows when pointed at.

use egui::{Color32, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(14, 14, 16);
pub const PANEL: Color32 = Color32::from_rgb(22, 22, 25);
pub const RAISED: Color32 = Color32::from_rgb(33, 33, 38);
pub const LINE: Color32 = Color32::from_rgb(56, 56, 64);
pub const TEXT: Color32 = Color32::from_rgb(228, 228, 232);
pub const DIM: Color32 = Color32::from_rgb(138, 138, 150);
pub const ACCENT: Color32 = Color32::from_rgb(255, 122, 48);
pub const ERROR: Color32 = Color32::from_rgb(232, 84, 84);
pub const VIDEO: Color32 = Color32::from_rgb(76, 141, 214);
pub const AUDIO: Color32 = Color32::from_rgb(87, 184, 148);

/// The corners of controls and of what is painted by hand: just off square.
pub const RADIUS: f32 = 1.5;

/// The face of a control at rest, pointed at, and of one that is on.
const FACE: Color32 = Color32::from_rgb(38, 38, 44);
const FACE_HOT: Color32 = Color32::from_rgb(52, 52, 60);
const FACE_ON: Color32 = Color32::from_rgb(82, 46, 24);
const PRESSED: Color32 = Color32::from_rgb(206, 92, 30);
const EDGE_HOT: Color32 = Color32::from_rgb(104, 104, 118);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut v = Visuals::dark();
        v.panel_fill = PANEL;
        v.window_fill = Color32::from_rgb(24, 24, 28);
        v.window_stroke = Stroke::new(1.0, LINE);
        v.extreme_bg_color = BG;
        v.faint_bg_color = Color32::from_rgb(26, 26, 30);
        v.override_text_color = Some(TEXT);
        // What is on (a chosen tab, a pressed toggle): a warm dark face
        // with the accent around it, not a block of colour.
        v.selection.bg_fill = FACE_ON;
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.hyperlink_color = ACCENT;
        let radius = egui::CornerRadius::same(RADIUS.round() as u8);
        v.window_corner_radius = egui::CornerRadius::same(3);
        v.menu_corner_radius = egui::CornerRadius::same(3);
        // A short, tight shadow: windows lie on the surface, they do not float above it.
        let shadow = egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(120) };
        v.window_shadow = shadow;
        v.popup_shadow = shadow;

        v.widgets.noninteractive.bg_fill = PANEL;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, DIM);
        v.widgets.noninteractive.corner_radius = radius;

        v.widgets.inactive.bg_fill = FACE;
        v.widgets.inactive.weak_bg_fill = FACE;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(204, 204, 212));
        v.widgets.inactive.expansion = 0.0;

        // Pointed at: the edge lights up and the label goes white. No growing.
        v.widgets.hovered.bg_fill = FACE_HOT;
        v.widgets.hovered.weak_bg_fill = FACE_HOT;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, EDGE_HOT);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.hovered.expansion = 0.0;

        // Pressed: filled with the accent for as long as it is held.
        // (Its text colour is also that of all strong text: white.)
        v.widgets.active.bg_fill = PRESSED;
        v.widgets.active.weak_bg_fill = PRESSED;
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.active.expansion = 0.0;

        // Open (a menu's button while its menu shows): the accent as its edge.
        v.widgets.open.bg_fill = FACE_HOT;
        v.widgets.open.weak_bg_fill = FACE_HOT;
        v.widgets.open.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.open.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.open.expansion = 0.0;

        for widget in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            widget.corner_radius = radius;
        }
        v.slider_trailing_fill = true;
        v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.5 };
        v.striped = false;
        v.indent_has_left_vline = false;
        style.visuals = v;

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        // Room around button labels (feedback F3.5): text never touches the edge.
        style.spacing.button_padding = egui::vec2(11.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.spacing.window_margin = egui::Margin::same(10);
        // Quick: a control answers the hand at once.
        style.animation_time = 0.07;
        // Thin bars that take no room from what they scroll and widen under the pointer.
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        // The text cursor and what is selected in a field: the accent, not a system blue.
        style.visuals.text_cursor.stroke = Stroke::new(1.5, ACCENT);
        // A tooltip comes when the pointer rests, not while it passes.
        style.interaction.tooltip_delay = 0.45;
    });
}
