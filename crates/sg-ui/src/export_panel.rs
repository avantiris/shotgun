//! The export popover: pick a preset, watch the progress, find the result.

use egui::{Align2, RichText, Stroke, vec2};

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportPreset {
    Master,
    Vertical,
    Share,
    Gif,
    /// The sound alone.
    Sound,
}

/// What to export, as the UI knows it; the application turns it into a job.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportRequest {
    pub preset: ExportPreset,
    /// Size limit of the share copy in megabytes.
    pub megabytes: u32,
    /// Share copy and GIF: 0 (no compression beyond the format's own) to
    /// 4 (smallest file).
    pub compression: u8,
    /// Frames (start, end); the whole sequence if `None`.
    pub range: Option<(i64, i64)>,
    /// Put the finished file on the clipboard.
    pub to_clipboard: bool,
    /// Sound export: 0 WAV, 1 FLAC, 2 MP3; every track by itself as
    /// well; and the loudness to bring the song to, in LUFS.
    pub sound_format: u8,
    pub stems: bool,
    pub loudness: Option<f32>,
}

/// Progress and results coming back from the export thread.
pub enum ExportUpdate {
    Progress {
        stage: &'static str,
        fraction: f32,
    },
    /// `note`: what else is worth knowing about the file (a song's loudness).
    Done {
        path: String,
        bytes: u64,
        seconds: f64,
        on_clipboard: bool,
        note: String,
    },
    Failed(String),
}

pub struct ExportPanel {
    open: bool,
    preset: ExportPreset,
    megabytes: u32,
    compression: u8,
    to_clipboard: bool,
    sound_format: u8,
    stems: bool,
    loudness: Option<f32>,
    /// Export the selection or marked stretch instead of everything.
    only_range: bool,
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
            compression: 1,
            to_clipboard: true,
            sound_format: 0,
            stems: false,
            loudness: None,
            only_range: false,
            running: None,
            result: None,
            last: None,
        }
    }
}

pub enum ExportAction {
    Start(ExportRequest),
    Cancel,
    /// Open the file manager where the exports are.
    ShowFolder,
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

    /// The compression chosen in the panel, for the GIF key.
    pub fn compression(&self) -> u8 {
        self.compression
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
            ExportUpdate::Done { path, bytes, seconds, on_clipboard, note } => {
                self.running = None;
                let size = if bytes >= 1_000_000 {
                    format!("{:.1} MB", bytes as f64 / 1e6)
                } else {
                    format!("{} kB", bytes / 1000)
                };
                let clipboard = if on_clipboard { ", copied to the clipboard" } else { "" };
                let note = if note.is_empty() { note } else { format!("\n{note}") };
                self.result = Some(Ok(format!("{path}  ({size}, took {seconds:.1} s{clipboard}){note}")));
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
                            (ExportPreset::Share, "Share copy", "To send (H.264): compressed as chosen, never above a size you set"),
                            (ExportPreset::Gif, "GIF", "The selection or the stretch between markers"),
                            (ExportPreset::Sound, "Sound only", "The song without picture: WAV, FLAC or MP3"),
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
                            ExportPreset::Sound => {
                                for (level, name, tip) in [
                                    (0, "WAV", "24-bit, no loss: for distributors and for mastering elsewhere."),
                                    (1, "FLAC", "No loss, about half the size."),
                                    (2, "MP3", "320 kbit/s: to send."),
                                ] {
                                    if ui.selectable_label(self.sound_format == level, name).on_hover_text(tip).clicked() {
                                        self.sound_format = level;
                                    }
                                }
                                ui.checkbox(&mut self.stems, "every track as a file of its own too").on_hover_text(
                                    "In a folder beside the song: each track as it sounds in the mix, \
                                     with its effects, its share of reverb and echo, and ducked as in the mix.",
                                );
                            }
                            _ => {}
                        });
                        if matches!(self.preset, ExportPreset::Share | ExportPreset::Gif) {
                            let gif = self.preset == ExportPreset::Gif;
                            ui.horizontal(|ui| {
                                ui.label("Compression");
                                for (level, name, share, as_gif) in [
                                    (0, "None", "Near the master's quality, up to 4K. The largest file.", "The best a GIF can be: 480 px wide, 20 pictures a second, nothing given up. The largest file."),
                                    (1, "Light", "Good quality at up to 1080p. No loss you would notice.", "480 px, 20 pictures a second, hardly any loss."),
                                    (2, "Medium", "1080p at up to 30 pictures a second; fine detail softens.", "480 px, 15 pictures a second, some noise in flat areas. Often half the size of Light."),
                                    (3, "Strong", "720p; visibly softer, much smaller.", "400 px, 12 pictures a second, visible noise and less smooth motion."),
                                    (4, "Smallest", "480p; blocky in motion. For when size is all that counts.", "320 px, 10 pictures a second, rough. For when size is all that counts."),
                                ] {
                                    let tip = if gif { as_gif } else { share };
                                    if ui.selectable_label(self.compression == level, name).on_hover_text(tip).clicked() {
                                        self.compression = level;
                                    }
                                }
                            });
                        }
                        if self.preset == ExportPreset::Sound {
                            ui.horizontal(|ui| {
                                ui.label("Loudness");
                                for (target, name, tip) in [
                                    (None, "As mixed", "Nothing is changed: the file is as loud as the mixer made it."),
                                    (Some(-14.0), "-14 LUFS", "What streaming services play at: nothing is turned down by them."),
                                    (Some(-9.0), "-9 LUFS", "Loud, as rap masters are: the limiter holds the peaks, which costs some punch."),
                                ] {
                                    if ui.selectable_label(self.loudness == target, name).on_hover_text(tip).clicked() {
                                        self.loudness = target;
                                    }
                                }
                            });
                        }
                        if self.preset != ExportPreset::Gif {
                            // The same stretch the GIF key takes.
                            let what = match gif_range {
                                Some(_) => {
                                    "Only the selected clips, or the stretch between the markers around the playhead"
                                }
                                None => {
                                    "Only a stretch: select clips, or set markers (G) before and after the playhead"
                                }
                            };
                            if gif_range.is_none() {
                                // No stretch any more: the whole sequence it is, and the box says so.
                                self.only_range = false;
                            }
                            ui.add_enabled(gif_range.is_some(), egui::Checkbox::new(&mut self.only_range, what));
                        }
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
                                    let range = if self.preset == ExportPreset::Gif || self.only_range {
                                        gif_range
                                    } else {
                                        None
                                    };
                                    action = Some(ExportAction::Start(ExportRequest {
                                        preset: self.preset,
                                        megabytes: self.megabytes,
                                        compression: self.compression,
                                        range,
                                        to_clipboard: clipboard,
                                        sound_format: self.sound_format,
                                        stems: self.stems,
                                        loudness: self.loudness,
                                    }));
                                }
                            }
                        }
                        let folder = ui.button("Open folder");
                        folder.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Open the exports folder"));
                        if folder.on_hover_text("Opens the project's exports folder in the file manager").clicked() {
                            action = Some(ExportAction::ShowFolder);
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
