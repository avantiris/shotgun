//! The export popover: pick a preset, watch the progress, find the result.

use egui::{Align2, RichText, Stroke, vec2};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportPreset {
    Master,
    Vertical,
    Share,
    Gif,
}

/// What to export, as the UI knows it; the application turns it into a job.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportRequest {
    pub preset: ExportPreset,
    /// Size limit of the share copy in megabytes.
    pub megabytes: u32,
    /// Frames (start, end); the whole sequence if `None`.
    pub range: Option<(i64, i64)>,
    /// Put the finished file on the clipboard.
    pub to_clipboard: bool,
}

/// Progress and results coming back from the export thread.
pub enum ExportUpdate {
    Progress { stage: &'static str, fraction: f32 },
    Done { path: String, bytes: u64, seconds: f64, on_clipboard: bool },
    Failed(String),
}

pub struct ExportPanel {
    open: bool,
    preset: ExportPreset,
    megabytes: u32,
    to_clipboard: bool,
    running: Option<(&'static str, f32)>,
    result: Option<Result<String, String>>,
    last: Option<ExportRequest>,
}

impl Default for ExportPanel {
    fn default() -> ExportPanel {
        ExportPanel {
            open: false,
            preset: ExportPreset::Master,
            megabytes: 10,
            to_clipboard: true,
            running: None,
            result: None,
            last: None,
        }
    }
}

pub enum ExportAction {
    Start(ExportRequest),
    Cancel,
}

impl ExportPanel {
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Closes the popover (a running export continues). Returns whether it was open.
    pub fn cancel(&mut self) -> bool {
        std::mem::take(&mut self.open)
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// The last export again, if there was one.
    pub fn repeat(&self) -> Option<ExportRequest> {
        self.last.clone()
    }

    pub fn started(&mut self, request: ExportRequest) {
        self.running = Some(("starting", 0.0));
        self.result = None;
        self.last = Some(request);
    }

    pub fn update(&mut self, update: ExportUpdate) {
        match update {
            ExportUpdate::Progress { stage, fraction } => self.running = Some((stage, fraction)),
            ExportUpdate::Done { path, bytes, seconds, on_clipboard } => {
                self.running = None;
                let size = if bytes >= 1_000_000 {
                    format!("{:.1} MB", bytes as f64 / 1e6)
                } else {
                    format!("{} kB", bytes / 1000)
                };
                let clipboard = if on_clipboard { ", copied to the clipboard" } else { "" };
                self.result = Some(Ok(format!("{path}  ({size}, took {seconds:.1} s{clipboard})")));
            }
            ExportUpdate::Failed(error) => {
                self.running = None;
                self.result = Some(Err(error));
            }
        }
    }

    /// A one-line status for the top bar while the popover is closed.
    pub fn status(&self) -> Option<String> {
        self.running.map(|(stage, fraction)| format!("Export: {stage} {:.0} %", fraction * 100.0))
    }

    /// `gif_range` is what Ctrl+G would export: the selection or the stretch
    /// between markers.
    pub fn ui(&mut self, ctx: &egui::Context, gif_range: Option<(i64, i64)>) -> Option<ExportAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        egui::Area::new(egui::Id::unique("export"))
            .fade_in(false)
            .anchor(Align2::CENTER_TOP, vec2(0.0, 80.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(Stroke::new(1.0, theme::LINE))
                    .corner_radius(6.0)
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.set_width(520.0);
                        ui.label(RichText::new("EXPORT").size(11.0).strong().color(theme::DIM));
                        for (preset, name, what) in [
                            (ExportPreset::Master, "Master", "The sequence as it is, top quality (H.265 10-bit)"),
                            (
                                ExportPreset::Vertical,
                                "Vertical 9:16",
                                "Upright frame; pictures fill it and lose their sides",
                            ),
                            (ExportPreset::Share, "Share copy", "Small enough to send (H.264), under a size you set"),
                            (ExportPreset::Gif, "GIF", "The selection or the stretch between markers, 480 px wide"),
                        ] {
                            ui.horizontal(|ui| {
                                ui.radio_value(&mut self.preset, preset, name);
                                ui.label(RichText::new(what).color(theme::DIM));
                            });
                        }
                        ui.horizontal(|ui| match self.preset {
                            ExportPreset::Share => {
                                ui.label("At most");
                                ui.add(egui::DragValue::new(&mut self.megabytes).range(1..=2000).suffix(" MB"));
                                ui.checkbox(&mut self.to_clipboard, "copy the file to the clipboard");
                            }
                            ExportPreset::Gif => {
                                ui.checkbox(&mut self.to_clipboard, "copy the GIF to the clipboard");
                            }
                            _ => {}
                        });
                        ui.add_space(4.0);
                        match self.running {
                            Some((stage, fraction)) => {
                                ui.add(egui::ProgressBar::new(fraction).text(stage));
                                if ui.button("Cancel export").clicked() {
                                    action = Some(ExportAction::Cancel);
                                }
                            }
                            None => {
                                if ui.button("Export").clicked() {
                                    let clipboard = matches!(self.preset, ExportPreset::Share | ExportPreset::Gif)
                                        && self.to_clipboard;
                                    let range = if self.preset == ExportPreset::Gif { gif_range } else { None };
                                    action = Some(ExportAction::Start(ExportRequest {
                                        preset: self.preset,
                                        megabytes: self.megabytes,
                                        range,
                                        to_clipboard: clipboard,
                                    }));
                                }
                            }
                        }
                        match &self.result {
                            Some(Ok(text)) => {
                                ui.label(RichText::new(text).color(theme::AUDIO));
                            }
                            Some(Err(error)) => {
                                ui.label(RichText::new(error).color(theme::ERROR));
                            }
                            None => {}
                        }
                        ui.label(
                            RichText::new(
                                "Ctrl+Shift+E repeats the last export. Esc closes; a running export continues.",
                            )
                            .color(theme::DIM),
                        );
                    });
            });
        action
    }
}
