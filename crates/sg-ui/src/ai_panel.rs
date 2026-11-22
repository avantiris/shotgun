//! The window in which a clip is changed by a sentence (docs/spec/10-ai.md):
//! which service or program does it, the sentence, and, before anything
//! is sent, what leaves this machine, where it goes and what it costs.
//! The models are added, changed and removed here too; they are kept in
//! a file of this machine, which nobody has to edit by hand.

use egui::{RichText, pos2};
use sg_core::{Clip, TrackId};
use sg_project::ai::{Kind, Service};

use crate::theme;

/// Sentences to begin from.
const SUGGESTIONS: [&str; 4] =
    ["Dark, epic storm clouds in the sky", "A golden sunset", "Night, with stars in the sky", "It is snowing"];

const KINDS: [(Kind, &str); 4] = [
    (Kind::Fal, "fal.ai"),
    (Kind::Runway, "Runway"),
    (Kind::Replicate, "Replicate"),
    (Kind::Command, "A program on this machine"),
];

/// A model being added or changed.
struct Draft {
    /// Which of the services it replaces; `None`: a new one.
    at: Option<usize>,
    service: Service,
    key: String,
    command: String,
}

impl Draft {
    fn of(at: Option<usize>, service: Service) -> Draft {
        let key = service.key.clone().unwrap_or_default();
        let command = sg_project::ai::command_line(&service.command);
        Draft { at, service, key, command }
    }

    /// The service as the fields now stand.
    fn made(&self) -> Service {
        let mut service = self.service.clone();
        service.name = service.name.trim().to_owned();
        service.model = service.model.trim().to_owned();
        service.key = Some(self.key.trim().to_owned()).filter(|key| !key.is_empty());
        service.command = sg_project::ai::command_words(&self.command);
        service
    }
}

pub struct AiPanel {
    pub track: TrackId,
    pub clip: Clip,
    /// How long the part of the video is that the clip shows.
    pub seconds: f64,
    pub sentence: String,
    chosen: usize,
    draft: Option<Draft>,
    /// From a script: the service by its name, sent as soon as the services are known.
    pub scripted: Option<String>,
}

pub enum AiAction {
    Send(Box<Service>, String),
    /// The models as they are to be kept from now on.
    Save(Vec<Service>),
    Close,
}

impl AiPanel {
    pub fn new(track: TrackId, clip: Clip, seconds: f64) -> AiPanel {
        AiPanel { track, clip, seconds, sentence: String::new(), chosen: 0, draft: None, scripted: None }
    }

    /// Opens the fields for a new model (from a script, to look at them).
    pub fn add(&mut self) {
        self.draft = Some(Draft::of(None, Service::blank(Kind::Fal)));
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
            .default_pos(pos2(360.0, 90.0))
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.set_width(480.0);
                let services = match services {
                    None => {
                        ui.label(RichText::new("Reading which models there are...").color(theme::DIM));
                        return;
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(theme::ERROR));
                        ui.label("The file was written by hand and cannot be read; put it right or remove it.");
                        return;
                    }
                    Some(Ok(services)) => services,
                };
                if self.draft.is_some() {
                    action = self.draft_ui(ui, services);
                } else {
                    action = self.send_ui(ui, services, file);
                }
            });
        if !open {
            action = Some(AiAction::Close);
        }
        action
    }

    fn send_ui(&mut self, ui: &mut egui::Ui, services: &[Service], file: &str) -> Option<AiAction> {
        let mut action = None;
        ui.label(
            "The part of the video that this clip shows is handed whole to a model that makes it anew with your \
             change in it. It may change more than you asked for; the result takes the clip's place and the video \
             itself stays.",
        );
        ui.add_space(6.0);
        if services.is_empty() {
            ui.label(
                "No model is set up yet. Such models do not run inside Shotgun: they are at a service (fal.ai, \
                 Runway, Replicate), where you need an account and a key of your own, or they are a program on \
                 this machine.",
            );
        }
        self.chosen = self.chosen.min(services.len().saturating_sub(1));
        for (i, service) in services.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.chosen, i, &service.name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").on_hover_text(format!("Remove {}", service.name)).clicked() {
                        let mut rest = services.to_vec();
                        rest.remove(i);
                        action = Some(AiAction::Save(rest));
                    }
                    if ui.small_button("Edit").on_hover_text(format!("Edit {}", service.name)).clicked() {
                        self.draft = Some(Draft::of(Some(i), service.clone()));
                    }
                });
            });
        }
        if ui.button("Add a model...").clicked() {
            self.draft = Some(Draft::of(None, Service::blank(Kind::Fal)));
        }
        let kept = format!("Kept in {file}, with your keys; only you can read it.");
        ui.label(RichText::new(kept).color(theme::DIM).small());
        let Some(service) = services.get(self.chosen) else { return action };
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
                "Leaves this machine: {seconds:.1} s of this clip's picture (no sound, at most 1920 points wide) and \
                 the sentence. They go to {}.",
                service.goes_to()
            ));
            match service.price_per_second {
                Some(price) => ui.label(format!(
                    "About ${:.2} ({seconds:.1} s at ${price} a second, as you wrote it down; the service's own \
                     price counts).",
                    price * seconds
                )),
                None => ui.label("What it costs is not written down: see the service's prices."),
            };
        }
        let too_long = service.max_seconds.filter(|most| seconds > *most + 0.05);
        let too_short = service.min_seconds.filter(|least| seconds < *least - 0.05);
        if let Some(most) = too_long {
            let said = format!("This model takes at most {most} s: cut the clip into shorter ones.");
            ui.label(RichText::new(said).color(theme::ERROR));
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
        action
    }

    /// The fields of one model.
    fn draft_ui(&mut self, ui: &mut egui::Ui, services: &[Service]) -> Option<AiAction> {
        let mut action = None;
        let draft = self.draft.as_mut()?;
        ui.label(if draft.at.is_some() { "Edit this model" } else { "Add a model" });
        if draft.at.is_none() {
            let said = "Begin from one that is known (as the services described them in November 2026):";
            ui.label(RichText::new(said).color(theme::DIM));
            ui.horizontal_wrapped(|ui| {
                for preset in sg_project::ai::presets() {
                    if ui.small_button(&preset.name).clicked() {
                        // (A key that was typed already stays.)
                        *draft = Draft { key: std::mem::take(&mut draft.key), ..Draft::of(None, preset) };
                    }
                }
            });
        }
        ui.add_space(4.0);
        egui::Grid::new("ai-draft").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            let width = 330.0;
            ui.label("Where");
            let shown = KINDS.iter().find(|(kind, _)| *kind == draft.service.kind).map_or("", |(_, name)| name);
            egui::ComboBox::from_id_salt("ai-kind").selected_text(shown).width(width).show_ui(ui, |ui| {
                for (kind, name) in KINDS {
                    ui.selectable_value(&mut draft.service.kind, kind, name);
                }
            });
            ui.end_row();
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut draft.service.name).hint_text("What you call it").desired_width(width),
            )
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Name of the model"));
            ui.end_row();
            if draft.service.kind == Kind::Command {
                ui.label("Program");
                let hint = "my-model --in {in} --out {out} --prompt {prompt}";
                ui.add(egui::TextEdit::singleline(&mut draft.command).hint_text(hint).desired_width(width))
                    .on_hover_text(
                        "The program and its words. {in} is the video it is given, {out} the video it must write, \
                         {prompt} the sentence. A line \"progress 0.4\" on its output is shown as progress.",
                    );
                ui.end_row();
            } else {
                ui.label("Model");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.service.model)
                        .hint_text("The model's name at the service")
                        .desired_width(width),
                );
                ui.end_row();
                ui.label("Key");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.key)
                        .password(true)
                        .hint_text("Your key from the service")
                        .desired_width(width),
                )
                .on_hover_text(
                    "From your account at the service. It is kept in a file only you can read, and sent to that \
                     service only.",
                )
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Key of the service"));
                ui.end_row();
                ui.label("Price");
                ui.horizontal(|ui| {
                    let mut price = draft.service.price_per_second.unwrap_or(0.0);
                    let field = egui::DragValue::new(&mut price).speed(0.005).range(0.0..=100.0);
                    ui.add(field.prefix("$").suffix(" a second"));
                    draft.service.price_per_second = Some(price).filter(|price| *price > 0.0);
                    ui.label(RichText::new("for the estimate only").color(theme::DIM));
                });
                ui.end_row();
            }
            ui.label("Length");
            ui.horizontal(|ui| {
                let mut least = draft.service.min_seconds.unwrap_or(0.0);
                let mut most = draft.service.max_seconds.unwrap_or(0.0);
                ui.add(egui::DragValue::new(&mut least).speed(0.1).range(0.0..=600.0).suffix(" s"));
                ui.label("to");
                ui.add(egui::DragValue::new(&mut most).speed(0.1).range(0.0..=600.0).suffix(" s"));
                draft.service.min_seconds = Some(least).filter(|least| *least > 0.0);
                draft.service.max_seconds = Some(most).filter(|most| *most > 0.0);
                ui.label(RichText::new("what the model takes; 0: any").color(theme::DIM));
            });
            ui.end_row();
        });
        ui.add_space(6.0);
        let made = draft.made();
        let taken = services.iter().enumerate().any(|(i, other)| Some(i) != draft.at && other.name == made.name);
        let missing = if taken { Err("another model has that name".to_owned()) } else { made.check() };
        if let Err(missing) = &missing {
            ui.label(RichText::new(format!("Still missing: {missing}.")).color(theme::DIM));
        }
        let mut back = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(missing.is_ok(), egui::Button::new("Save")).clicked() {
                let mut all = services.to_vec();
                match draft.at {
                    Some(i) if i < all.len() => all[i] = made.clone(),
                    _ => all.push(made.clone()),
                }
                action = Some(AiAction::Save(all));
            }
            back = ui.button("Back").clicked();
        });
        if let Some(AiAction::Save(all)) = &action {
            // The one just saved is the one chosen.
            self.chosen = all.iter().position(|service| service.name == made.name).unwrap_or(0);
            self.draft = None;
        } else if back {
            self.draft = None;
        }
        action
    }
}
