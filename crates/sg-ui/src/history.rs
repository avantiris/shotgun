//! Checkpoints and the list of saved states (git commits), as popovers.

use egui::{Align2, RichText, Stroke, vec2};
use sg_project::HistoryEntry;

use crate::theme;

pub enum HistoryRequest {
    Checkpoint(String),
    Restore { commit: String, description: String },
}

#[derive(Default)]
pub struct HistoryPanel {
    open: bool,
    entries: Option<Vec<HistoryEntry>>,
    /// The checkpoint name being typed.
    naming: Option<String>,
    focus: bool,
}

fn ago(time: u64, now: u64) -> String {
    let secs = now.saturating_sub(time);
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

impl HistoryPanel {
    pub fn begin_checkpoint(&mut self) {
        self.naming = Some(String::new());
        self.focus = true;
    }

    /// Opens the list; the caller loads the entries.
    pub fn open(&mut self) {
        self.open = true;
        self.entries = None;
    }

    pub fn set_entries(&mut self, entries: Vec<HistoryEntry>) {
        self.entries = Some(entries);
    }

    /// Closes whatever is open. Returns whether something was.
    pub fn cancel(&mut self) -> bool {
        self.naming.take().is_some() | std::mem::take(&mut self.open)
    }

    fn frame() -> egui::Frame {
        egui::Frame::new()
            .fill(theme::RAISED)
            .stroke(Stroke::new(1.0, theme::LINE))
            .corner_radius(6.0)
            .inner_margin(14.0)
    }

    pub fn ui(&mut self, ctx: &egui::Context, now: u64) -> Vec<HistoryRequest> {
        let mut out = Vec::new();
        if let Some(name) = &mut self.naming {
            let mut done = false;
            egui::Area::new(egui::Id::unique("checkpoint"))
                .fade_in(false)
                .anchor(Align2::CENTER_TOP, vec2(0.0, 80.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    HistoryPanel::frame().show(ui, |ui| {
                        ui.set_width(380.0);
                        ui.label(RichText::new("NAME THIS STATE").size(11.0).strong().color(theme::DIM));
                        let response = ui.add(
                            egui::TextEdit::singleline(name)
                                .hint_text("e.g. rough cut done")
                                .desired_width(f32::INFINITY),
                        );
                        if std::mem::take(&mut self.focus) {
                            response.request_focus();
                        }
                        if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let name = name.trim();
                            out.push(HistoryRequest::Checkpoint(if name.is_empty() {
                                "checkpoint".to_owned()
                            } else {
                                name.to_owned()
                            }));
                            done = true;
                        }
                        ui.label(
                            RichText::new("Enter to save, Esc to cancel. Saving itself is automatic.")
                                .color(theme::DIM),
                        );
                    });
                });
            if done {
                self.naming = None;
            }
        }
        if self.open {
            let mut close = false;
            egui::Area::new(egui::Id::unique("history"))
                .fade_in(false)
                .anchor(Align2::CENTER_TOP, vec2(0.0, 80.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    HistoryPanel::frame().show(ui, |ui| {
                        ui.set_width(560.0);
                        ui.label(RichText::new("HISTORY").size(11.0).strong().color(theme::DIM));
                        match &self.entries {
                            None => {
                                ui.label(RichText::new("Reading...").color(theme::DIM));
                            }
                            Some(entries) if entries.is_empty() => {
                                ui.label(RichText::new("No saved states yet.").color(theme::DIM));
                            }
                            Some(entries) => {
                                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                                    for (i, entry) in entries.iter().enumerate() {
                                        ui.horizontal(|ui| {
                                            ui.add_sized(
                                                [84.0, 18.0],
                                                egui::Label::new(RichText::new(ago(entry.time, now)).color(theme::DIM)),
                                            );
                                            for name in &entry.checkpoints {
                                                ui.label(RichText::new(name).color(theme::ACCENT).strong());
                                            }
                                            ui.add(egui::Label::new(&entry.message).truncate());
                                            // The newest state is the current one.
                                            if i > 0 {
                                                ui.with_layout(
                                                    egui::Layout::right_to_left(egui::Align::Center),
                                                    |ui| {
                                                        if ui.small_button("Restore").clicked() {
                                                            let description = entry
                                                                .checkpoints
                                                                .first()
                                                                .cloned()
                                                                .unwrap_or_else(|| entry.message.clone());
                                                            out.push(HistoryRequest::Restore {
                                                                commit: entry.commit.clone(),
                                                                description,
                                                            });
                                                            close = true;
                                                        }
                                                    },
                                                );
                                            }
                                        });
                                    }
                                });
                            }
                        }
                        ui.label(
                            RichText::new("Restoring adds a new state; nothing is lost. Esc to close.")
                                .color(theme::DIM),
                        );
                    });
                });
            if close {
                self.open = false;
            }
        }
        out
    }
}

impl HistoryPanel {
    /// Commit and description of the n-th loaded entry.
    pub fn entry(&self, n: usize) -> Option<(String, String)> {
        let entry = self.entries.as_ref()?.get(n)?;
        Some((entry.commit.clone(), entry.checkpoints.first().cloned().unwrap_or_else(|| entry.message.clone())))
    }
}
