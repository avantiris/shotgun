//! The plugins popover: what is installed, and the set-up of the Native
//! Instruments software step by step, done by the program itself
//! (feedback F14.3; docs/spec/09-daw.md, "Kontakt").

use egui::{Align2, RichText, Stroke, vec2};
use sg_project::plugins::{Format, NiState, Plugin};

use crate::theme;

/// The steps of the Native Instruments set-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NiStep {
    /// Create the Wine prefix and install Native Access into it.
    Setup,
    /// Open Native Access to log in and install products.
    Launch,
    /// Make the installed plugins available through the bridge.
    Bridge,
    /// Not a step of its own: look for what is wrong and repair it.
    Check,
}

/// What the panel asks the application to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginRequest {
    /// Make a track for this plugin (name, path) with a note clip on it,
    /// and open the piano roll and the plugin's window: ready to play.
    Play(String, String),
    /// Look for plugins and at the state of the set-up.
    Scan,
    Run(NiStep),
    /// Stop the step that is running.
    Stop,
}

/// What comes back from the job thread.
pub enum PluginUpdate {
    /// A line the running step printed.
    Line(String),
    Found {
        plugins: Vec<Plugin>,
        ni: NiState,
    },
    /// The file everything the steps print is written to.
    LogFile(std::path::PathBuf),
    /// The running step ended.
    Finished(Result<(), String>),
}

#[derive(Default)]
pub struct PluginsPanel {
    open: bool,
    plugins: Vec<Plugin>,
    ni: Option<NiState>,
    running: Option<NiStep>,
    /// The last lines of the running or last step.
    log: Vec<String>,
    failed: Option<String>,
    /// Where the whole output of the steps is kept.
    log_file: Option<std::path::PathBuf>,
}

impl PluginsPanel {
    /// Opens the popover; the caller asks for a scan.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Closes the popover (a running step continues). Returns whether it was open.
    pub fn cancel(&mut self) -> bool {
        std::mem::take(&mut self.open)
    }

    pub fn started(&mut self, step: NiStep) {
        self.running = Some(step);
        self.log.clear();
        self.failed = None;
    }

    pub fn update(&mut self, update: PluginUpdate) {
        match update {
            PluginUpdate::Line(line) => {
                self.log.push(line);
                let extra = self.log.len().saturating_sub(8);
                self.log.drain(..extra);
            }
            PluginUpdate::Found { plugins, ni } => {
                self.plugins = plugins;
                self.ni = Some(ni);
            }
            PluginUpdate::LogFile(path) => self.log_file = Some(path),
            PluginUpdate::Finished(result) => {
                self.running = None;
                self.failed = result.err();
            }
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<PluginRequest> {
        if !self.open {
            return None;
        }
        let mut request = None;
        egui::Area::new(egui::Id::unique("plugins"))
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
                        ui.set_width(620.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("PLUGINS").size(11.0).strong().color(theme::DIM));
                            if ui.button("Look again").on_hover_text("Search the plugin folders anew").clicked() {
                                request = Some(PluginRequest::Scan);
                            }
                        });
                        if self.plugins.is_empty() {
                            ui.label(
                                RichText::new(
                                    "None found. VST3 plugins are looked for in ~/.vst3 and CLAP plugins in ~/.clap \
                                     (and the system's folders): put one there and look again.",
                                )
                                .color(theme::DIM),
                            );
                        }
                        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                            for plugin in &self.plugins {
                                ui.horizontal(|ui| {
                                    let play = ui.button("Play it");
                                    play.widget_info(|| {
                                        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Play {}", plugin.name))
                                    });
                                    if play
                                        .on_hover_text(
                                            "Makes a track with this instrument on it and a clip for notes, and opens the \
                                             piano roll and the instrument's own window: load a sound there and play. \
                                             (An effect goes on a track's strip instead: Mixer, \"Add plugin\".)",
                                        )
                                        .clicked()
                                    {
                                        request =
                                            Some(PluginRequest::Play(plugin.name.clone(), plugin.path.display().to_string()));
                                    }
                                    ui.label(&plugin.name);
                                    let kind = match (plugin.format, plugin.bridged) {
                                        (Format::Vst3, true) => "VST3, Windows through the bridge",
                                        (Format::Vst3, false) => "VST3",
                                        (Format::Clap, _) => "CLAP",
                                    };
                                    ui.label(RichText::new(kind).color(theme::DIM));
                                });
                            }
                        });
                        ui.add_space(8.0);
                        ui.label(RichText::new("NATIVE INSTRUMENTS (KONTAKT)").size(11.0).strong().color(theme::DIM));
                        match self.ni.clone() {
                            None => {
                                ui.label(RichText::new("Looking...").color(theme::DIM));
                            }
                            Some(ni) if !ni.tools => {
                                ui.label(
                                    RichText::new(
                                        "The programs for this (ni, yabridgectl) were not found. They come with \
                                         Shotgun's own package: start Shotgun from it (nix run, or the dev shell).",
                                    )
                                    .color(theme::ERROR),
                                );
                            }
                            Some(ni) => self.ni_steps(ui, &ni, &mut request),
                        }
                        if !self.log.is_empty() {
                            ui.add_space(6.0);
                            egui::Frame::new().fill(theme::BG).corner_radius(4.0).inner_margin(8.0).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                for line in &self.log {
                                    ui.label(RichText::new(line).monospace().size(11.0).color(theme::DIM));
                                }
                            });
                        }
                        if let Some(error) = &self.failed {
                            ui.label(RichText::new(error).color(theme::ERROR));
                        }
                        if let Some(file) = &self.log_file {
                            ui.label(
                                RichText::new(format!("Everything the steps printed is kept in {}", file.display()))
                                    .size(11.0)
                                    .color(theme::DIM),
                            );
                        }
                        ui.add_space(4.0);
                        ui.label(RichText::new("Esc closes; a running step continues.").color(theme::DIM));
                    });
            });
        request
    }

    /// The three steps, each with what it is for, whether it is done, and its button.
    fn ni_steps(&mut self, ui: &mut egui::Ui, ni: &NiState, request: &mut Option<PluginRequest>) {
        ui.label(
            RichText::new(format!(
                "Kontakt and the other Native Instruments products exist for Windows only. Shotgun runs them \
                 through Wine and makes them appear as plugins. Three steps, done here. Where: {}",
                ni.prefix.display()
            ))
            .color(theme::DIM),
        );
        ui.add_space(4.0);
        let busy = self.running;
        let mut step = |ui: &mut egui::Ui,
                        number: u32,
                        this: NiStep,
                        done: bool,
                        enabled: bool,
                        title: &str,
                        what: &str,
                        button: &str| {
            ui.horizontal(|ui| {
                let mark = if done { "done" } else { "    " };
                ui.label(RichText::new(format!("{number}.")).strong());
                ui.label(RichText::new(mark).color(theme::AUDIO).size(11.0));
                ui.vertical(|ui| {
                    ui.label(RichText::new(title).strong());
                    ui.label(RichText::new(what).color(theme::DIM));
                    match busy {
                        Some(running) if running == this => {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Working...").color(theme::ACCENT));
                                if ui.button("Stop").clicked() {
                                    *request = Some(PluginRequest::Stop);
                                }
                            });
                        }
                        _ => {
                            if ui.add_enabled(enabled && busy.is_none(), egui::Button::new(button)).clicked() {
                                *request = Some(PluginRequest::Run(this));
                            }
                        }
                    }
                });
            });
            ui.add_space(4.0);
        };
        step(
            ui,
            1,
            NiStep::Setup,
            ni.access,
            true,
            "Install Native Access",
            "Makes a Windows environment for it and installs Native Access into it. A few minutes; \
             needs the network. Windows of the installer may come up: let them finish.",
            if ni.access { "Install again" } else { "Install Native Instruments" },
        );
        step(
            ui,
            2,
            NiStep::Launch,
            !ni.installed.is_empty(),
            ni.access,
            "Log in and install Kontakt",
            "Opens Native Access. Log in with your Native Instruments account (the browser opens for \
             it), install Kontakt and what else you own, then close Native Access.",
            "Open Native Access",
        );
        let found = if ni.installed.is_empty() {
            "Makes what you installed appear in the list above.".to_owned()
        } else {
            format!(
                "Makes what you installed appear in the list above. Installed: {}. Available already: {}.",
                ni.installed.join(", "),
                if ni.bridged.is_empty() { "none".to_owned() } else { ni.bridged.join(", ") }
            )
        };
        step(
            ui,
            3,
            NiStep::Bridge,
            !ni.installed.is_empty() && ni.bridged.len() == ni.installed.len(),
            !ni.installed.is_empty(),
            "Make them available",
            &found,
            "Find the installed plugins",
        );
        // Not a step: for when one of them did not do what it should.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Something not working?").color(theme::DIM));
            match busy {
                Some(NiStep::Check) => {
                    ui.label(RichText::new("Checking...").color(theme::ACCENT));
                    if ui.button("Stop").clicked() {
                        *request = Some(PluginRequest::Stop);
                    }
                }
                _ => {
                    let check =
                        ui.add_enabled(busy.is_none() && ni.prefix_ready, egui::Button::new("Check and repair"));
                    if check
                        .on_hover_text(
                            "Looks at the Windows environment, at what Native Access needs and at the link the \
                             browser uses to come back after the login, and repairs what it can. What it finds \
                             is shown below.",
                        )
                        .clicked()
                    {
                        *request = Some(PluginRequest::Run(NiStep::Check));
                    }
                }
            }
        });
    }
}
