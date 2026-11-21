//! The project's words: lyrics, ideas, a list of what is left to do. A
//! tab beside the assets, so that it can stay open while recording
//! (feedback F16.13).
//!
//! The text is part of the project (`Project::lyrics`), so it is saved,
//! undone and kept in checkpoints like everything else. It is not written
//! into the journal with every key: what is typed is put down when the
//! typing pauses or the field is left.

use egui::{FontId, RichText};
use sg_core::Op;

use crate::session::Session;
use crate::theme;

/// Seconds without a key before what was typed becomes part of the project.
const PAUSE: f64 = 0.8;

pub struct Lyrics {
    /// The tab is the one shown in the side panel.
    pub open: bool,
    /// The text as it is in the field.
    text: String,
    /// When the field last changed, while that is not in the project yet.
    changed_at: Option<f64>,
    /// Undo group of the typing since the field was entered: one undo
    /// step for one sitting, not one for every pause.
    group: Option<u64>,
    /// Size of the letters: larger to read from where one stands to record.
    size: f32,
}

impl Default for Lyrics {
    fn default() -> Lyrics {
        Lyrics { open: false, text: String::new(), changed_at: None, group: None, size: 15.0 }
    }
}

impl Lyrics {
    /// Whether something typed is not in the project yet.
    pub fn pending(&self) -> bool {
        self.changed_at.is_some()
    }

    /// Makes what was typed part of the project.
    pub fn flush(&mut self, session: &mut Session, error: &mut Option<String>) {
        if self.changed_at.take().is_none() || *session.project().lyrics == *self.text {
            return;
        }
        let group = *self.group.get_or_insert_with(|| session.new_group());
        if let Err(e) = session.apply(Op::SetLyrics { text: self.text.clone() }, Some(group)) {
            *error = Some(e.to_string());
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, session: &mut Session, error: &mut Option<String>) {
        // What the project has, unless something newer is being typed (an undo shows at once).
        if self.changed_at.is_none() && *session.project().lyrics != *self.text {
            self.text = session.project().lyrics.to_string();
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("LYRICS").size(11.0).strong().color(theme::DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("A+").on_hover_text("Larger letters").clicked() {
                    self.size = (self.size + 2.0).min(44.0);
                }
                if ui.small_button("A-").on_hover_text("Smaller letters").clicked() {
                    self.size = (self.size - 2.0).max(11.0);
                }
                let state = if self.changed_at.is_some() { "writing..." } else { "kept with the project" };
                ui.label(RichText::new(state).size(10.5).color(theme::DIM));
            });
        });
        let now = ui.input(|i| i.time);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let field = egui::TextEdit::multiline(&mut self.text)
                .font(FontId::proportional(self.size))
                .desired_width(f32::INFINITY)
                .desired_rows(12)
                .hint_text("Words for this project: lyrics, ideas, what is left to do.");
            let response = ui.add_sized(ui.available_size(), field);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Lyrics field"));
            if response.changed() {
                self.changed_at = Some(now);
            }
            let paused = self.changed_at.is_some_and(|at| now - at >= PAUSE);
            if paused || (self.changed_at.is_some() && !response.has_focus()) {
                self.flush(session, error);
            } else if self.changed_at.is_some() {
                // (Nothing else may happen until then: come back when the pause is over.)
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
            if response.lost_focus() {
                self.group = None;
            }
        });
    }
}
