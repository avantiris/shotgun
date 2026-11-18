//! The editor screen. In M1 this is the fixed panel layout around an open
//! project: the panels fill up in later milestones.

use std::sync::atomic::Ordering;

use egui::{Align, Align2, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::{Op, Rate, Sequence, TrackKind};

use crate::session::Session;
use crate::theme;

pub enum EditorRequest {
    Close,
    Undo,
    Redo,
}

#[derive(Default)]
pub struct Editor {
    /// The name being typed while renaming the project.
    rename: Option<String>,
    focus_rename: bool,
}

impl Editor {
    pub fn begin_rename(&mut self, session: &Session) {
        self.rename = Some(session.project().name.clone());
        self.focus_rename = true;
    }

    /// Cancels a rename in progress. Returns whether there was one.
    pub fn cancel(&mut self) -> bool {
        self.rename.take().is_some()
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        error: &mut Option<String>,
    ) -> Option<EditorRequest> {
        let mut request = None;
        let bar = egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(12, 6));
        egui::Panel::top("top-bar").exact_size(40.0).frame(bar).show(ui, |ui| {
            ui.horizontal_centered(|ui| self.top_bar(ui, session, error, &mut request));
        });
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else {
            egui::CentralPanel::default().show(ui, |ui| ui.label("This project has no sequence."));
            return request;
        };
        let flush = egui::Frame::new().fill(theme::BG);
        egui::Panel::bottom("timeline")
            .resizable(true)
            .default_size(230.0)
            .min_size(120.0)
            .frame(flush)
            .show(ui, |ui| timeline(ui, sequence));
        egui::Panel::left("assets").default_size(260.0).min_size(160.0).show(ui, |ui| {
            // A panel shrinks to its content unless the content claims the width.
            ui.take_available_width();
            section(ui, "ASSETS");
            ui.label(RichText::new("No assets yet.").color(theme::DIM));
        });
        egui::Panel::right("inspector").default_size(300.0).min_size(200.0).show(ui, |ui| {
            self.inspector(ui, session, sequence, &mut request);
        });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(12.0))
            .show(ui, |ui| viewer(ui, sequence));
        request
    }

    fn top_bar(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        error: &mut Option<String>,
        request: &mut Option<EditorRequest>,
    ) {
        if ui.button("Library").on_hover_text("Close project (Ctrl+W)").clicked() {
            *request = Some(EditorRequest::Close);
        }
        ui.add_space(8.0);
        match &mut self.rename {
            Some(name) => {
                let response =
                    ui.add(egui::TextEdit::singleline(name).font(FontId::proportional(16.0)).desired_width(280.0));
                if std::mem::take(&mut self.focus_rename) {
                    response.request_focus();
                }
                if response.lost_focus() {
                    let name = name.trim().to_owned();
                    self.rename = None;
                    if !name.is_empty()
                        && name != session.project().name
                        && let Err(e) = session.apply(Op::SetProjectName { name }, None)
                    {
                        *error = Some(e.to_string());
                    }
                }
            }
            None => {
                let label =
                    egui::Label::new(RichText::new(&session.project().name).size(16.0).strong()).sense(Sense::click());
                if ui.add(label).on_hover_text("Double-click or F2 to rename").double_clicked() {
                    self.begin_rename(session);
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let status = session.writer_status();
            let (appended, saved) = (status.appended.load(Ordering::Relaxed), status.saved.load(Ordering::Relaxed));
            match status.error() {
                Some(e) => ui.label(RichText::new(format!("Autosave failing: {e}")).color(theme::ERROR)),
                None if saved == appended => ui.label(RichText::new("Saved").color(theme::DIM)),
                None => ui.label(RichText::new("Saving...").color(theme::ACCENT)),
            };
        });
    }

    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        session: &Session,
        sequence: &Sequence,
        request: &mut Option<EditorRequest>,
    ) {
        let project = session.project();
        ui.take_available_width();
        section(ui, "PROJECT");
        row(ui, "Name", &project.name);
        row(ui, "Folder", &session.root.display().to_string());
        if ui.small_button("Rename (F2)").clicked() {
            self.begin_rename(session);
        }
        ui.add_space(10.0);
        section(ui, "SEQUENCE");
        row(ui, "Name", &sequence.name);
        row(ui, "Format", &format!("{}x{}, {}", sequence.width, sequence.height, fps(sequence.rate)));
        row(ui, "Tracks", &sequence.tracks.len().to_string());
        row(ui, "Clips", &sequence.clip_count().to_string());
        ui.add_space(10.0);
        section(ui, "HISTORY");
        let history = session.document().history();
        row(ui, "Undo steps", &history.undo_len().to_string());
        row(ui, "Redo steps", &history.redo_len().to_string());
        ui.horizontal(|ui| {
            if ui.add_enabled(history.can_undo(), egui::Button::new("Undo (Y)")).clicked() {
                *request = Some(EditorRequest::Undo);
            }
            if ui.add_enabled(history.can_redo(), egui::Button::new("Redo (Shift+Y)")).clicked() {
                *request = Some(EditorRequest::Redo);
            }
        });
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).size(11.0).strong().color(theme::DIM));
    ui.add_space(2.0);
}

fn row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(84.0, 16.0), Layout::left_to_right(Align::Center), |ui| {
            ui.label(RichText::new(key).color(theme::DIM));
            ui.take_available_width();
        });
        ui.add(egui::Label::new(value).truncate());
    });
}

fn fps(rate: Rate) -> String {
    if rate.den() == 1 {
        format!("{} fps", rate.num())
    } else {
        // Display only; the model keeps the exact fraction.
        format!("{:.2} fps", rate.num() as f64 / rate.den() as f64)
    }
}

fn viewer(ui: &mut egui::Ui, sequence: &Sequence) {
    let available = ui.available_rect_before_wrap();
    let aspect = sequence.width as f32 / sequence.height.max(1) as f32;
    let size = if available.width() / available.height() > aspect {
        vec2(available.height() * aspect, available.height())
    } else {
        vec2(available.width(), available.width() / aspect)
    };
    let rect = Rect::from_center_size(available.center(), size);
    ui.allocate_rect(available, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
    let label = format!("{}x{}, {}", sequence.width, sequence.height, fps(sequence.rate));
    painter.text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(13.0), theme::DIM);
}

/// Track rows only: clips are drawn by the instanced timeline renderer in M4.
fn timeline(ui: &mut egui::Ui, sequence: &Sequence) {
    const HEADER_W: f32 = 120.0;
    const RULER_H: f32 = 20.0;
    const ROW_H: f32 = 34.0;
    let rect = ui.available_rect_before_wrap();
    ui.allocate_rect(rect, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::BG);
    let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), RULER_H));
    painter.rect_filled(ruler, 0.0, theme::PANEL);
    painter.text(
        pos2(rect.left() + HEADER_W + 4.0, ruler.center().y),
        Align2::LEFT_CENTER,
        "00:00:00:00",
        FontId::monospace(10.0),
        theme::DIM,
    );
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
        if !track.clips.is_empty() {
            let text = format!("{} clips", track.clips.len());
            painter.text(
                pos2(lane.left() + HEADER_W + 4.0, lane.center().y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(12.0),
                theme::DIM,
            );
        }
    }
    let x = rect.left() + HEADER_W;
    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, theme::LINE));
    // Playhead at the start.
    painter.line_segment([pos2(x + 1.0, rect.top()), pos2(x + 1.0, rect.bottom())], Stroke::new(1.5, theme::ACCENT));
}
