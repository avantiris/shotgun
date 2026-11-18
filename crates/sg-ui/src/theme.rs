//! Colours and spacing in one place.

use egui::{Color32, Stroke, Visuals};

pub const BG: Color32 = Color32::from_rgb(18, 18, 20);
pub const PANEL: Color32 = Color32::from_rgb(26, 26, 29);
pub const RAISED: Color32 = Color32::from_rgb(36, 36, 41);
pub const LINE: Color32 = Color32::from_rgb(52, 52, 58);
pub const TEXT: Color32 = Color32::from_rgb(228, 228, 232);
pub const DIM: Color32 = Color32::from_rgb(140, 140, 150);
pub const ACCENT: Color32 = Color32::from_rgb(255, 122, 48);
pub const ERROR: Color32 = Color32::from_rgb(232, 84, 84);
pub const VIDEO: Color32 = Color32::from_rgb(76, 141, 214);
pub const AUDIO: Color32 = Color32::from_rgb(87, 184, 148);

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let mut v = Visuals::dark();
        v.panel_fill = PANEL;
        v.window_fill = RAISED;
        v.extreme_bg_color = BG;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        v.widgets.inactive.bg_fill = RAISED;
        v.widgets.inactive.weak_bg_fill = RAISED;
        style.visuals = v;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        // Room around button labels (feedback F3.5): text never touches the edge.
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.menu_margin = egui::Margin::same(8);
        let radius = egui::CornerRadius::same(5);
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = radius;
        }
    });
}
