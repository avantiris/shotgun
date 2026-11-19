//! The colour panel: scenarios with their match settings and look chain.

use egui::{Color32, RichText, vec2};
use sg_core::{AssetId, Module, ModuleKind, Op, Project, Scenario, ScenarioId};

use crate::session::Session;
use crate::theme;

/// Stripe colours of scenarios, by `Scenario::colour`.
pub const STRIPES: [Color32; 8] = [
    Color32::from_rgb(236, 110, 110),
    Color32::from_rgb(240, 190, 80),
    Color32::from_rgb(130, 200, 110),
    Color32::from_rgb(90, 200, 200),
    Color32::from_rgb(120, 150, 240),
    Color32::from_rgb(190, 130, 235),
    Color32::from_rgb(235, 130, 190),
    Color32::from_rgb(170, 170, 180),
];

pub fn stripe(scenario: &Scenario) -> Color32 {
    STRIPES[scenario.colour as usize % STRIPES.len()]
}

/// What the panel asks the application for (it does no file work itself).
pub enum ColourRequest {
    /// Measure these assets again.
    Analyse(ScenarioId, Vec<AssetId>),
    /// Write the look chain to `looks/<name>.toml`.
    SaveLook(String, Vec<Module>),
    /// Read the presets in `looks/`.
    LoadLooks,
}

#[derive(Default)]
pub struct ColourPanel {
    pub open: bool,
    /// The scenario shown.
    pub current: Option<ScenarioId>,
    /// Undo group of the drag in progress.
    group: Option<u64>,
    preset_name: String,
    /// Presets read from `looks/`, once asked for.
    pub presets: Option<Vec<(String, Vec<Module>)>>,
    /// The look-up tables in `assets/lut/`, read along with the presets.
    pub luts: Vec<String>,
    /// Whether the folder was read since the panel was opened.
    asked: bool,
}

impl ColourPanel {
    /// The scenario the panel shows: the chosen one, else the first.
    fn shown(&self, project: &Project) -> Option<ScenarioId> {
        self.current
            .filter(|id| project.scenarios.contains_key(id))
            .or_else(|| project.scenarios.keys().next().copied())
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        error: &mut Option<String>,
        requests: &mut Vec<ColourRequest>,
    ) {
        let project = session.project().clone();
        ui.label(RichText::new("COLOUR").size(11.0).strong().color(theme::DIM));
        if project.scenarios.is_empty() {
            ui.label(
                RichText::new(
                    "No scenarios yet. A scenario is a group of takes that should look like one place and time: \
                     right click on an asset or a clip and choose Scenario.",
                )
                .color(theme::DIM),
            );
            return;
        }
        ui.horizontal_wrapped(|ui| {
            for scenario in project.scenarios.values() {
                let text = RichText::new(&scenario.name).color(stripe(scenario));
                if ui.selectable_label(self.shown(&project) == Some(scenario.id), text).clicked() {
                    self.current = Some(scenario.id);
                }
            }
        });
        let Some(scenario) = self.shown(&project).and_then(|id| project.scenarios.get(&id)) else { return };
        let mut edited = Scenario::clone(scenario);
        let mut remove = false;
        let label = |ui: &mut egui::Ui, text: &str| {
            ui.add_sized(
                [84.0, 16.0],
                egui::Label::new(RichText::new(text).color(theme::DIM)).halign(egui::Align::LEFT),
            );
        };
        ui.horizontal(|ui| {
            label(ui, "Name");
            ui.add(egui::TextEdit::singleline(&mut edited.name).desired_width(150.0));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut edited.match_on, RichText::new("MATCH").size(11.0).strong())
                .on_hover_text("Brings every take of the scenario to the same exposure, white balance, contrast and saturation (Shift+J)");
        });
        ui.horizontal(|ui| {
            label(ui, "Strength");
            let mut percent = edited.strength * 100.0;
            ui.add(egui::Slider::new(&mut percent, 0.0..=100.0).suffix(" %").fixed_decimals(0));
            edited.strength = percent / 100.0;
        });
        let name_of = |id: &AssetId| project.assets.get(id).map_or("missing asset", |a| a.label.as_str()).to_owned();
        ui.horizontal(|ui| {
            label(ui, "Match to");
            let chosen = edited.hero.map_or("the middle of all".to_owned(), |id| name_of(&id));
            egui::ComboBox::from_id_salt("hero").selected_text(chosen).show_ui(ui, |ui| {
                ui.selectable_value(&mut edited.hero, None, "the middle of all");
                for id in scenario.members.keys() {
                    ui.selectable_value(&mut edited.hero, Some(*id), name_of(id));
                }
            });
        });
        let unmeasured: Vec<AssetId> =
            scenario.members.iter().filter(|(_, m)| m.stats.is_none()).map(|(id, _)| *id).collect();
        for (id, member) in &scenario.members {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let grade = scenario.grade(*id).correction;
                let state = match member.stats {
                    // Exposure change in stops, as the plainest summary of what the match does.
                    Some(_) => format!("{:+.2} stops", grade.gain[1].log2()),
                    None => "not measured".to_owned(),
                };
                ui.label(name_of(id));
                ui.label(RichText::new(state).color(theme::DIM));
                if ui.small_button("x").on_hover_text("Take out of the scenario").clicked() {
                    edited.members.remove(id);
                    if edited.hero == Some(*id) {
                        edited.hero = None;
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.button("Measure again").on_hover_text("Looks at the pictures of all takes anew").clicked() {
                requests.push(ColourRequest::Analyse(scenario.id, scenario.members.keys().copied().collect()));
            }
            if !unmeasured.is_empty() {
                ui.label(RichText::new(format!("{} not measured", unmeasured.len())).color(theme::ACCENT));
            }
        });

        ui.add_space(8.0);
        ui.checkbox(&mut edited.look_on, RichText::new("LOOK").size(11.0).strong())
            .on_hover_text("The chain below, applied to every take of the scenario after the match (J)");
        // The folder of tables and saved looks is read when the panel first shows a scenario.
        if !std::mem::replace(&mut self.asked, true) {
            requests.push(ColourRequest::LoadLooks);
        }
        if edited.look.iter().any(|m| m.kind == ModuleKind::Lut) {
            ui.horizontal(|ui| {
                label(ui, "Film look");
                let shown = if edited.lut.is_empty() { "choose a table".to_owned() } else { edited.lut.clone() };
                egui::ComboBox::from_id_salt("lut").selected_text(shown).show_ui(ui, |ui| {
                    ui.selectable_value(&mut edited.lut, String::new(), "none");
                    for name in &self.luts {
                        ui.selectable_value(&mut edited.lut, name.clone(), name);
                    }
                });
            });
            if self.luts.is_empty() {
                ui.label(RichText::new("No tables yet: drop a .cube file onto the window.").color(theme::DIM));
            }
        }
        let mut moved: Option<(usize, isize)> = None;
        let mut dropped = None;
        let count = edited.look.len();
        for (index, module) in edited.look.iter_mut().enumerate() {
            egui::Frame::new().fill(theme::RAISED.gamma_multiply(0.6)).corner_radius(5.0).inner_margin(6.0).show(
                ui,
                |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut module.on, RichText::new(module.kind.name()).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().button_padding = vec2(6.0, 2.0);
                            if ui.button("x").on_hover_text("Remove").clicked() {
                                dropped = Some(index);
                            }
                            if ui.add_enabled(index + 1 < count, egui::Button::new("down")).clicked() {
                                moved = Some((index, 1));
                            }
                            if ui.add_enabled(index > 0, egui::Button::new("up")).clicked() {
                                moved = Some((index, -1));
                            }
                        });
                    });
                    for (value, knob) in module.knobs.iter_mut().zip(module.kind.knobs()) {
                        ui.horizontal(|ui| {
                            label(ui, knob.name);
                            let speed = (knob.max - knob.min) / 300.0;
                            let response = ui.add(
                                egui::DragValue::new(value).range(knob.min..=knob.max).speed(speed).max_decimals(2),
                            );
                            let response =
                                if knob.hint.is_empty() { response } else { response.on_hover_text(knob.hint) };
                            if response.double_clicked() {
                                *value = knob.rest;
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        label(ui, "Mix");
                        let mut percent = module.mix * 100.0;
                        ui.add(
                            egui::DragValue::new(&mut percent)
                                .range(0.0..=100.0)
                                .speed(0.5)
                                .suffix(" %")
                                .max_decimals(0),
                        );
                        module.mix = percent / 100.0;
                    });
                },
            );
        }
        if let Some((index, by)) = moved {
            edited.look.swap(index, (index as isize + by) as usize);
        }
        if let Some(index) = dropped {
            edited.look.remove(index);
        }
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(edited.look.len() < sg_core::colour::MAX_MODULES, |ui| {
                ui.menu_button("Add module", |ui| {
                    for kind in ModuleKind::ALL {
                        if ui.button(kind.name()).clicked() {
                            edited.look.push(Module::new(kind));
                            ui.close();
                        }
                    }
                });
            });
            ui.menu_button("Copy look from", |ui| {
                for other in project.scenarios.values().filter(|s| s.id != scenario.id) {
                    if ui.button(&other.name).clicked() {
                        edited.look = other.look.clone();
                        ui.close();
                    }
                }
                let presets = ui.menu_button("Saved looks", |ui| match &self.presets {
                    Some(presets) if presets.is_empty() => {
                        ui.label("None saved yet.");
                    }
                    Some(presets) => {
                        for (name, modules) in presets {
                            if ui.button(name).clicked() {
                                edited.look = modules.clone();
                                ui.close();
                            }
                        }
                    }
                    None => {
                        ui.label("Reading...");
                    }
                });
                // Read the folder when the menu is first opened.
                if presets.response.clicked() || (presets.inner.is_some() && self.presets.is_none()) {
                    requests.push(ColourRequest::LoadLooks);
                    self.presets.get_or_insert_with(Vec::new);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.preset_name).hint_text("Name for this look").desired_width(150.0),
            );
            let name = self.preset_name.trim().to_owned();
            if ui.add_enabled(!name.is_empty() && !edited.look.is_empty(), egui::Button::new("Save look")).clicked() {
                requests.push(ColourRequest::SaveLook(name, edited.look.clone()));
                self.preset_name.clear();
                self.presets = None;
            }
        });
        ui.add_space(8.0);
        if ui.button("Delete scenario").clicked() {
            remove = true;
        }

        if !ui.input(|i| i.pointer.any_down()) {
            self.group = None;
        }
        let op = if remove {
            Some(Op::SetScenario { id: scenario.id, scenario: None })
        } else if edited != **scenario {
            Some(Op::SetScenario { id: scenario.id, scenario: Some(Box::new(edited)) })
        } else {
            None
        };
        if let Some(op) = op {
            // One drag of a knob is one undo step; typing a name letter by letter too.
            let held = ui.input(|i| i.pointer.any_down()) || ui.ctx().egui_wants_keyboard_input();
            let group = held.then(|| *self.group.get_or_insert_with(|| session.new_group()));
            if let Err(e) = session.apply(op, group) {
                *error = Some(e.to_string());
            }
        }
    }

    /// Switches the match (`true`) or the look of the shown scenario on or off: the A/B keys.
    pub fn toggle(&mut self, session: &mut Session, the_match: bool, error: &mut Option<String>) {
        let project = session.project().clone();
        let Some(scenario) = self.shown(&project).and_then(|id| project.scenarios.get(&id)) else { return };
        let mut edited = Scenario::clone(scenario);
        if the_match {
            edited.match_on = !edited.match_on;
        } else {
            edited.look_on = !edited.look_on;
        }
        let stage = if the_match { "Match" } else { "Look" };
        let state = if (the_match && edited.match_on) || (!the_match && edited.look_on) { "on" } else { "off" };
        session.notice = Some(format!("{stage} of {} {state}.", edited.name));
        if let Err(e) = session.apply(Op::SetScenario { id: scenario.id, scenario: Some(Box::new(edited)) }, None) {
            *error = Some(e.to_string());
        }
    }
}
