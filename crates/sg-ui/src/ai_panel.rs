//! The window in which a clip is changed by a sentence (docs/spec/10-ai.md):
//! which service or program does it, the sentence, and, before anything
//! is sent, what leaves this machine, where it goes and what it costs.

use egui::{RichText, pos2};
use sg_core::{Clip, TrackId};
use sg_project::ai::Service;

use crate::theme;

/// Sentences to begin from.
const SUGGESTIONS: [&str; 4] =
    ["Dark, epic storm clouds in the sky", "A golden sunset", "Night, with stars in the sky", "It is snowing"];

pub struct AiPanel {
    pub track: TrackId,
    pub clip: Clip,
    /// How long the part of the video is that the clip shows.
    pub seconds: f64,
    pub sentence: String,
    chosen: usize,
    /// From a script: the service by its name, sent as soon as the services are known.
    pub scripted: Option<String>,
}

pub enum AiAction {
    Send(Box<Service>, String),
    Close,
}

impl AiPanel {
    pub fn new(track: TrackId, clip: Clip, seconds: f64) -> AiPanel {
        AiPanel { track, clip, seconds, sentence: String::new(), chosen: 0, scripted: None }
    }

    /// `services`: `None` while the file is read. `file`: where it is.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        services: Option<&Result<Vec<Service>, String>>,
        file: &str,
    ) -> Option<AiAction> {
        if let (Some(name), Some(Ok(services))) = (&self.scripted, services) {
            return Some(match services.iter().find(|service| &service.name == name) {
                Some(service) => AiAction::Send(Box::new(service.clone()), self.sentence.clone()),
                None => AiAction::Close,
            });
        }
        let mut action = None;
        let mut open = true;
        egui::Window::new("Change this clip by a sentence (AI)")
            .id(egui::Id::unique("ai-panel"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(pos2(360.0, 120.0))
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.label(
                    "The part of the video that this clip shows is handed whole to a model that makes it anew with \
                     your change in it. It may change more than you asked for; the result takes the clip's place \
                     and the video itself stays.",
                );
                ui.add_space(6.0);
                let services = match services {
                    None => {
                        ui.label(RichText::new("Reading which models there are...").color(theme::DIM));
                        return;
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(theme::ERROR));
                        return;
                    }
                    Some(Ok(services)) if services.is_empty() => {
                        ui.label(format!(
                            "No model is set up yet. Such models do not run inside Shotgun: write which service \
                             (fal.ai, Runway, Replicate, with your own key from them) or which program on this \
                             machine does it into {file}, like this, and open this window again:"
                        ));
                        ui.add_space(4.0);
                        let mut example = sg_project::ai::EXAMPLE;
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            ui.add(egui::TextEdit::multiline(&mut example).code_editor().desired_width(f32::INFINITY));
                        });
                        return;
                    }
                    Some(Ok(services)) => services,
                };
                self.chosen = self.chosen.min(services.len() - 1);
                for (i, service) in services.iter().enumerate() {
                    ui.radio_value(&mut self.chosen, i, &service.name);
                }
                let service = &services[self.chosen];
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.sentence)
                        .hint_text("What should be different?")
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal_wrapped(|ui| {
                    for suggestion in SUGGESTIONS {
                        if ui.small_button(suggestion).clicked() {
                            self.sentence = suggestion.to_owned();
                        }
                    }
                });
                ui.add_space(6.0);
                let seconds = self.seconds;
                if service.is_local() {
                    ui.label(format!("Done on this machine by: {}", service.goes_to()));
                } else {
                    ui.label(format!(
                        "Leaves this machine: {seconds:.1} s of this clip's picture (no sound, at most 1920 points \
                         wide) and the sentence. They go to {}.",
                        service.goes_to()
                    ));
                    match service.price_per_second {
                        Some(price) => ui.label(format!(
                            "About ${:.2} ({seconds:.1} s at ${price} a second, as written in the file; the service's own price counts).",
                            price * seconds
                        )),
                        None => ui.label("What it costs is not written in the file: see the service's prices."),
                    };
                }
                let too_long = service.max_seconds.filter(|most| seconds > *most + 0.05);
                let too_short = service.min_seconds.filter(|least| seconds < *least - 0.05);
                if let Some(most) = too_long {
                    ui.label(
                        RichText::new(format!("This model takes at most {most} s: cut the clip into shorter ones.")).color(theme::ERROR),
                    );
                }
                if let Some(least) = too_short {
                    ui.label(RichText::new(format!("This model takes at least {least} s.")).color(theme::ERROR));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready = !self.sentence.trim().is_empty() && too_long.is_none() && too_short.is_none();
                    let send = if service.is_local() { "Make it" } else { "Send it" };
                    if ui.add_enabled(ready, egui::Button::new(send)).clicked() {
                        action = Some(AiAction::Send(Box::new(service.clone()), self.sentence.trim().to_owned()));
                    }
                    if ui.button("Not now").clicked() {
                        action = Some(AiAction::Close);
                    }
                });
            });
        if !open {
            action = Some(AiAction::Close);
        }
        action
    }
}
