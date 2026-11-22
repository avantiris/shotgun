//! The editor screen: the fixed panel layout around an open project.
//! Viewer and timeline fill up in M3 and M4.

use std::sync::atomic::Ordering;

use egui::{Align, FontId, Layout, RichText, Sense, vec2};
use sg_core::{Asset, AssetId, Op, Rate, Sequence};

use crate::actions;
use crate::assets::{AssetRequest, AssetsPanel, summary};
use crate::clip_panel::{ClipPanel, Target};
use crate::icons::{self, Icon};
use crate::keymap::Action;
use crate::session::Session;
use crate::stage::{self, Transport, ViewerState};
use crate::theme;
use crate::timeline::Timeline;

pub enum EditorRequest {
    Close,
    /// Open the plugins popover.
    Plugins,
    RemoveAsset(AssetId),
    AddTrack(sg_core::TrackKind),
    /// A new track for notes, ready to play: with this plugin (name,
    /// path) as its instrument, or with the built-in one.
    InstrumentTrack(Option<(String, String)>),
    SetSize(u32, u32),
    CutsToBeats,
    WhipAll,
    /// Cut to this camera angle (0 is the first) at the playhead.
    Angle(usize),
    Tempo(i8),
    SyncAll,
    ClearSong,
}

#[derive(Default)]
pub struct Editor {
    /// The name being typed while renaming the project.
    rename: Option<String>,
    focus_rename: bool,
    pub assets: AssetsPanel,
    /// The assets panel is put away (feedback F16.9): more room for the
    /// picture or the notes. Importing by drop, paste and key still works.
    pub assets_hidden: bool,
    pub timeline: Timeline,
    pub clip_panel: ClipPanel,
    /// The project's words, a tab beside the assets.
    pub lyrics: crate::lyrics::Lyrics,
    pub mixer: crate::mixer::Mixer,
    pub colour: crate::colour_panel::ColourPanel,
    /// What the colour panel asked for this frame; taken by the shell.
    pub colour_requests: Vec<crate::colour_panel::ColourRequest>,
    was_playing: bool,
    /// The label being typed for the selected asset.
    asset_label: Option<(AssetId, String)>,
    /// Actions whose buttons or menu entries were clicked this frame; the
    /// shell runs them the way it runs their keys.
    pub actions: Vec<Action>,
    /// Undo group while a tempo field is being dragged: one drag, one step.
    tempo_group: Option<u64>,
    /// When "Tap" was clicked of late, in seconds: the tempo by hand.
    taps: Vec<f64>,
    /// The metronome is on.
    pub metronome: bool,
    /// A take is being recorded: the button says so.
    pub recording_sound: bool,
    /// The input sound is recorded from (its PipeWire name); `None`: the system's default.
    pub input: Option<String>,
    /// The inputs there are, (name, label), once asked for.
    pub inputs: Option<Vec<(String, String)>>,
    /// What the input menu asked for this frame; taken by the shell.
    pub input_requests: Vec<InputRequest>,
    /// The input is open: its level is shown.
    pub monitor: bool,
    /// The open input is also heard.
    pub hear_input: bool,
    /// The level shown: the highest of the last moments, falling back slowly.
    input_shown: f32,
    /// The input reached full scale since "Level" was switched on or this was cleared.
    input_clipped: bool,
    /// The settings popover is showing.
    pub settings_open: bool,
    /// The notes of a note clip, shown in place of the viewer while open.
    pub piano: crate::piano_roll::PianoRoll,
}

/// What the input menu asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputRequest {
    /// Find the inputs there are now.
    List,
    /// Record from this one from now on (`None`: the system's default).
    Choose(Option<String>),
}

impl Editor {
    pub fn begin_rename(&mut self, session: &Session) {
        self.rename = Some(session.project().name.clone());
        self.focus_rename = true;
    }

    /// Cancels what is in progress: popover first, then a rename. Returns
    /// whether there was something.
    pub fn cancel(&mut self) -> bool {
        // (The label field of a selected asset is always there; it is not
        // something in progress, and Esc must get past it.)
        self.assets.close_add()
            || self.rename.take().is_some()
            || std::mem::take(&mut self.settings_open)
            || self.piano.close()
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        error: &mut Option<String>,
        asset_requests: &mut Vec<AssetRequest>,
        view: &ViewerState,
        transport: &mut Vec<Transport>,
    ) -> Option<EditorRequest> {
        let mut request = None;
        let bar = egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::symmetric(12, 6));
        egui::Panel::top("top-bar").exact_size(40.0).frame(bar).show(ui, |ui| {
            ui.horizontal_centered(|ui| self.top_bar(ui, session, error, &mut request));
        });
        self.settings(ui.ctx(), view);
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else {
            egui::CentralPanel::default().show(ui, |ui| ui.label("This project has no sequence."));
            return request;
        };
        let flush = egui::Frame::new().fill(theme::BG);
        egui::Panel::bottom("timeline").resizable(true).default_size(270.0).min_size(150.0).frame(flush).show(
            ui,
            |ui| {
                if let Some(wanted) = self.tools(ui, session, sequence, view, error) {
                    request = Some(wanted);
                }
                self.timeline.ui(ui, session, view, transport, error);
                // A title asked for its text: the inspector shows it with the cursor in the field.
                if std::mem::take(&mut self.timeline.edit_title) {
                    self.show_title_text();
                }
                if let Some((track, clip)) = self.timeline.edit_notes.take() {
                    self.piano.show(track, clip);
                }
                // Posters at the heads of clips: the asset panel holds the textures.
                let painter = ui.painter().with_clip_rect(self.timeline.lanes);
                let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                for (asset, at) in self.timeline.thumbs.drain(..) {
                    if let Some(texture) = self.assets.poster_id(asset) {
                        painter.image(texture, at, whole, egui::Color32::from_white_alpha(235));
                    }
                }
            },
        );
        if !self.assets_hidden {
            egui::Panel::left("assets").default_size(320.0).min_size(170.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let files = ui.selectable_label(!self.lyrics.open, "Files");
                    if files.on_hover_text("The project's recordings, pictures and videos").clicked() {
                        self.lyrics.open = false;
                    }
                    let words = ui.selectable_label(self.lyrics.open, "Lyrics");
                    if words
                        .on_hover_text("Words kept with the project: lyrics to read while recording, ideas, notes")
                        .clicked()
                    {
                        self.lyrics.open = true;
                    }
                });
                if self.lyrics.open {
                    self.lyrics.ui(ui, session, error);
                } else {
                    self.assets.ui(ui, &project, asset_requests);
                }
            });
        }
        // Words typed and then put out of sight are not lost.
        if self.lyrics.pending() && (self.assets_hidden || !self.lyrics.open) {
            self.lyrics.flush(session, error);
        }
        // The clip the inspector and the viewer act on, as of this frame.
        let target = self.target(session, view.frame);
        if self.clip_panel.recording && self.was_playing && !view.playing {
            self.clip_panel.finish_recording(session, sequence, target.as_ref(), error);
        }
        self.was_playing = view.playing;
        let target = self.target(session, view.frame);
        egui::Panel::right("inspector").default_size(300.0).min_size(200.0).show(ui, |ui| {
            // The content can be taller than a small window.
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                if self.colour.open {
                    self.colour.ui(ui, session, error, &mut self.colour_requests);
                } else if self.mixer.open {
                    let on_audio = |id: &sg_core::TrackId| {
                        sequence.track(*id).is_some_and(|t| t.kind == sg_core::TrackKind::Audio)
                    };
                    let current = self.piano.hand().or(target.as_ref().map(|t| t.track.id)).filter(on_audio);
                    self.mixer.follow(current);
                    self.mixer.ui(ui, session, sequence, view, error);
                } else {
                    self.inspector(ui, session, sequence, target.as_ref(), error, &mut request);
                }
            });
        });
        if let Some(frame) = self.piano.seek_request.take() {
            transport.push(Transport::Seek(frame));
        }
        if let Some(frame) = self.clip_panel.seek_request.take() {
            transport.push(Transport::Seek(frame));
        }
        let recording = self.clip_panel.recording;
        // The angle the programme shows at the playhead, to mark it in the monitor.
        let live = crate::session::program_row(sequence)
            .and_then(|row| sequence.tracks[row].clips.at(view.frame))
            .and_then(|on_air| {
                crate::session::angle_rows(sequence).iter().position(|row| {
                    sequence.tracks[*row]
                        .clips
                        .at(view.frame)
                        .is_some_and(|c| c.asset == on_air.asset && c.src_in - c.start == on_air.src_in - on_air.start)
                })
            });
        // Where the selected picture or title lies, for the handles in the
        // viewer: drag inside to move it, a corner to size it, the knob to turn it.
        let outline = target
            .as_ref()
            .filter(|t| t.track.kind == sg_core::TrackKind::Video && !t.track.mix.hidden)
            .and_then(|t| {
                let scale = t.props.scale.at(t.at);
                let (w, h) = match &t.props.title {
                    // A title is as large as its letters: measured with the
                    // built-in font (the one titles use unless another is
                    // chosen, for which this is close enough for a handle).
                    Some(title) => {
                        let px = (title.size * sequence.height as f32).clamp(4.0, 2000.0);
                        let size = ui.fonts_mut(|fonts| {
                            let font = FontId::proportional(px);
                            title.text.lines().fold(vec2(0.0, 0.0), |sum, line| {
                                let line = fonts.layout_no_wrap(line.to_owned(), font.clone(), egui::Color32::WHITE);
                                vec2(sum.x.max(line.size().x), sum.y + line.size().y)
                            })
                        });
                        (size.x.max(px) * scale, size.y.max(px) * scale)
                    }
                    None => {
                        let video = project.assets.get(&t.clip.asset)?.video.as_ref()?;
                        let (w, h) = if video.rotation % 180 == 90 {
                            (video.height, video.width)
                        } else {
                            (video.width, video.height)
                        };
                        // Fitted into the frame at scale 1.
                        let fit = (sequence.width as f32 / w.max(1) as f32)
                            .min(sequence.height as f32 / h.max(1) as f32)
                            * scale;
                        (w as f32 * fit, h as f32 * fit)
                    }
                };
                Some(stage::Outline {
                    centre: egui::vec2(t.props.x.at(t.at), t.props.y.at(t.at)),
                    half: egui::vec2(w / sequence.width as f32 / 2.0, h / sequence.height as f32 / 2.0),
                    turn: t.props.rotation.at(t.at),
                })
            });
        // An open note clip takes the viewer's place.
        if self.piano.is_open() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::BG).inner_margin(6.0))
                .show(ui, |ui| self.piano.ui(ui, session, view.frame, view.playing, error));
            return request;
        }
        let events = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(12.0))
            .show(ui, |ui| stage::viewer(ui, sequence, view, recording, live, outline))
            .inner;
        if let Some(angle) = events.angle {
            request = Some(EditorRequest::Angle(angle));
        }
        if let (true, Some(target)) = (events.scaled.is_some() || events.turned.is_some(), &target) {
            let (factor, degrees) = (events.scaled.unwrap_or(1.0), events.turned.unwrap_or(0.0));
            self.clip_panel.viewer_handles(session, sequence, target, factor, degrees, error);
        }
        if let (Some(delta), Some(target)) = (events.dragged, &target) {
            self.clip_panel.viewer_drag(session, sequence, target, delta, view.playing, error);
        }
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
        ui.add_space(8.0);
        if ui
            .selectable_label(!self.assets_hidden, "Assets")
            .on_hover_text("Shows or hides the panel with the project's recordings, at the left")
            .clicked()
        {
            self.assets_hidden = !self.assets_hidden;
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let status = session.writer_status();
            let (appended, saved) = (status.appended.load(Ordering::Relaxed), status.saved.load(Ordering::Relaxed));
            match status.error() {
                Some(e) => ui.label(RichText::new(format!("Autosave failing: {e}")).color(theme::ERROR)),
                None if saved == appended => ui.label(RichText::new("Saved").color(theme::DIM)),
                None => ui.label(RichText::new("Saving...").color(theme::ACCENT)),
            };
            ui.add_space(8.0);
            if ui
                .selectable_label(self.mixer.open && !self.mixer.chain, "Mixer")
                .on_hover_text("Levels, pan, mute, solo and clean-up per audio track (M)")
                .clicked()
            {
                self.mixer.open = !self.mixer.open || self.mixer.chain;
                self.mixer.chain = false;
                self.colour.open = false;
            }
            // (Laid out from the right: this one stands to the left of "Mixer".)
            if ui
                .selectable_label(self.mixer.open && self.mixer.chain, "Chain")
                .on_hover_text(
                    "The selected track alone: what its sound goes through, in order (instrument, vocal chain, \
                     plugins, sends), and its strip to set each",
                )
                .clicked()
            {
                self.mixer.open = !(self.mixer.open && self.mixer.chain);
                self.mixer.chain = true;
                self.colour.open = false;
            }
            if ui
                .selectable_label(self.colour.open, "Colour")
                .on_hover_text("Scenarios: match takes to each other, then give them a look (K)")
                .clicked()
            {
                self.colour.open = !self.colour.open;
                self.mixer.open = false;
            }
            // What acts on the selected clips (feedback F15.2, F15.3): beside
            // the panels, not among the things done all the time.
            self.clip_menu(ui);
            ui.add_space(8.0);
            // The rest as icons; the name and the key are in the tooltip (F15.5).
            ui.spacing_mut().item_spacing.x = 4.0;
            if icons::button(
                ui,
                Icon::Settings,
                "Settings",
                Some("which input sound is recorded from, and its level"),
                "",
                true,
                self.settings_open,
            ) {
                self.settings_open = !self.settings_open;
            }
            if icons::button(
                ui,
                Icon::Plugins,
                "Plugins",
                Some("the plugins found on this machine, and the set-up of Native Instruments (Kontakt)"),
                "",
                true,
                false,
            ) {
                *request = Some(EditorRequest::Plugins);
            }
            for (action, icon, name) in [
                (Action::Help, Icon::Keys, "Keys"),
                (Action::Export, Icon::Export, "Export"),
                (Action::History, Icon::History, "History"),
                (Action::Checkpoint, Icon::Checkpoint, "Checkpoint"),
            ] {
                if icons::action(ui, icon, action, name, true, false) {
                    self.actions.push(action);
                }
            }
        });
    }

    /// "+ Track" above the timeline: an empty track of either kind, or one
    /// for notes with an instrument on it and its piano roll open.
    fn add_track_menu(&mut self, ui: &mut egui::Ui, request: &mut Option<EditorRequest>) {
        let (response, _) = egui::containers::menu::MenuButton::new("+ Track").ui(ui, |ui| {
            ui.set_min_width(280.0);
            let mut choose = |ui: &mut egui::Ui, label: &str, tip: &str, wanted: EditorRequest| {
                if ui.button(label).on_hover_text(tip).clicked() {
                    *request = Some(wanted);
                    ui.close();
                }
            };
            choose(
                ui,
                "Audio track",
                "An empty track for sound: recordings, songs, samples. It is added at the bottom.",
                EditorRequest::AddTrack(sg_core::TrackKind::Audio),
            );
            choose(
                ui,
                "Instrument track",
                "A track for notes with the built-in instrument, a clip to write them into at the playhead, and \
                 the piano roll open. Another instrument can be chosen on its mixer strip.",
                EditorRequest::InstrumentTrack(None),
            );
            choose(
                ui,
                "Video track",
                "An empty track for pictures, titles and video. It is added on top.",
                EditorRequest::AddTrack(sg_core::TrackKind::Video),
            );
            ui.separator();
            ui.label(RichText::new("Instrument track with a plugin").size(11.0).color(theme::DIM));
            match &self.mixer.available {
                None => {
                    self.mixer.scan_request = true;
                    ui.label(RichText::new("Looking for plugins...").color(theme::DIM));
                }
                Some(found) if found.is_empty() => {
                    ui.label(RichText::new("None found: see Plugins (the plug in the top bar).").color(theme::DIM));
                }
                Some(found) => {
                    for (name, path) in found.clone() {
                        let wanted = EditorRequest::InstrumentTrack(Some((name.clone(), path.clone())));
                        choose(ui, &name, &path, wanted);
                    }
                }
            }
        });
        response.on_hover_text("A new track: for sound, for notes with an instrument, or for pictures");
    }

    /// "Clip" in the top bar: what is done to the selected clips now and
    /// then. Each entry acts on all of them at once.
    fn clip_menu(&mut self, ui: &mut egui::Ui) {
        let selected = self.timeline.has_selection();
        let mut chosen = None;
        egui::containers::menu::MenuButton::new("Clip")
            .ui(ui, |ui| {
                ui.set_min_width(260.0);
                let hint = if selected { "For the selected clips" } else { "Select clips to act on all of them" };
                ui.label(RichText::new(hint).size(11.0).color(theme::DIM));
                ui.separator();
                for (enabled, action, label) in [
                    (selected, Action::KeyframeSet, "Keyframe"),
                    (selected, Action::KeyframeRemove, "Remove keyframe"),
                    (selected, Action::LiveRecord, "Record movement"),
                ] {
                    if actions::enabled_entry(ui, enabled, action, label) {
                        chosen = Some(action);
                    }
                }
                ui.separator();
                // (Without a selection these two act on the cut nearest the playhead.)
                for (action, label) in [(Action::Transition, "Whip-pan"), (Action::Blackout, "Black cut")] {
                    if actions::entry(ui, action, label) {
                        chosen = Some(action);
                    }
                }
            })
            .0
            .on_hover_text(
                "Keyframes, recorded movement and the transitions on the cuts of the selected clips: every one of \
                 them at once.",
            );
        self.actions.extend(chosen);
    }

    /// The settings of this machine (feedback F15.1): where sound is
    /// recorded from, how loud it arrives, and whether it is heard.
    fn settings(&mut self, ctx: &egui::Context, view: &ViewerState) {
        if !self.settings_open {
            return;
        }
        egui::Area::new(egui::Id::unique("settings"))
            .fade_in(false)
            .anchor(egui::Align2::RIGHT_TOP, vec2(-12.0, 46.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(egui::Stroke::new(1.0, theme::LINE))
                    .corner_radius(theme::RADIUS)
                    .inner_margin(14.0)
                    .show(ui, |ui| {
                        ui.set_width(360.0);
                        ui.label(RichText::new("SETTINGS").size(11.0).strong().color(theme::DIM));
                        ui.add_space(4.0);
                        ui.label(RichText::new("Recording").strong());
                        ui.label(
                            RichText::new(
                                "Where \"Record sound\" records from. Kept for this machine, not in the project.",
                            )
                            .color(theme::DIM),
                        );
                        ui.horizontal_wrapped(|ui| {
                            self.input_menu(ui);
                            self.monitor_buttons(ui, view);
                        });
                        ui.add_space(4.0);
                        ui.label(RichText::new("Esc closes.").color(theme::DIM));
                    });
            });
    }

    /// The buttons above the timeline (feedback F4.1, narrowed in rounds 5
    /// and 15): recording, metronome, loop, title, marker, group, and the
    /// tempo. Keyframes and transitions are under "Clip" in the top bar,
    /// the input under the settings. Cutting, copying, undo and zoom stay
    /// with keys and mouse.
    fn tools(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        sequence: &Sequence,
        view: &ViewerState,
        error: &mut Option<String>,
    ) -> Option<EditorRequest> {
        let selected = self.timeline.has_selection();
        let mut wanted = None;
        let request = &mut wanted;
        egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().button_padding = vec2(8.0, 3.0);
                ui.spacing_mut().item_spacing = vec2(5.0, 4.0);
                // First of all, as in every such program: a new track (feedback F15.7).
                self.add_track_menu(ui, request);
                ui.separator();
                // What is done all the time while making something (feedback
                // F15.4), as icons with their name and key on hover (F15.5).
                let (recording, clicking, looping) =
                    (self.recording_sound, self.metronome, self.timeline.loop_range.is_some());
                for (enabled, on, action, icon, name) in [
                    (
                        true,
                        recording,
                        Action::RecordSound,
                        Icon::Record,
                        if recording { "Stop recording" } else { "Record sound" },
                    ),
                    (
                        true,
                        clicking,
                        Action::Metronome,
                        Icon::Metronome,
                        if clicking { "Metronome on" } else { "Metronome" },
                    ),
                    (true, looping, Action::Loop, Icon::Loop, if looping { "Loop on" } else { "Loop" }),
                    (true, false, Action::AddTitle, Icon::Title, "Title"),
                    (true, false, Action::Marker, Icon::Marker, "Marker"),
                    (selected, false, Action::Group, Icon::Group, "Group"),
                ] {
                    if icons::action(ui, icon, action, name, enabled, on) {
                        self.actions.push(action);
                    }
                }
                // Tempo and grid where the bars are (09-daw.md, M11).
                ui.separator();
                self.tempo_fields(ui, session, sequence, error, true);
                if let Some(tempo) = sequence.tempo {
                    self.grid_menu(ui);
                    let here = sequence.rate.frames_to_samples(view.frame.max(0), 48_000);
                    let button = ui.add_enabled(tempo.origin != here, egui::Button::new("Bar 1 here"));
                    if button
                        .on_hover_text(
                            "The first bar begins at the playhead: for music that does not start with the timeline. \
                             Bars, the grid and the metronome move with it; nothing on the timeline does.",
                        )
                        .clicked()
                    {
                        let moved = sg_core::Tempo { origin: here, ..tempo };
                        if let Err(e) = session.apply(Op::SetTempo { id: sequence.id, tempo: Some(moved) }, None) {
                            *error = Some(e.to_string());
                        }
                    }
                }
            });
        });
        wanted
    }

    /// Beside the input: "Level" opens it and shows how loud it arrives
    /// (is the microphone there at all, is it too hot), "Hear" adds it to
    /// what is heard. Hearing is its own button and off at first: through
    /// loudspeakers it would feed back.
    fn monitor_buttons(&mut self, ui: &mut egui::Ui, view: &ViewerState) {
        if ui
            .selectable_label(self.monitor, "Level")
            .on_hover_text("Opens the input and shows its level, without recording")
            .clicked()
        {
            self.monitor = !self.monitor;
            self.input_shown = 0.0;
            self.input_clipped = false;
        }
        if !self.monitor {
            return;
        }
        // Up at once, down by about 20 dB a second: a peak stays readable.
        self.input_shown = view.input_level.max(self.input_shown * 0.96);
        let db = 20.0 * self.input_shown.max(1e-6).log10();
        let (rect, response) = ui.allocate_exact_size(vec2(90.0, 14.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, theme::BG);
        // -60 dB at the left, full scale at the right.
        let filled = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
        let colour = if db > -1.0 {
            egui::Color32::from_rgb(220, 70, 60)
        } else if db > -9.0 {
            egui::Color32::from_rgb(220, 180, 60)
        } else {
            theme::ACCENT
        };
        painter.rect_filled(rect.with_max_x(rect.left() + rect.width() * filled), 2.0, colour);
        let said = if db <= -119.0 { "no signal".to_owned() } else { format!("{db:.0} dB") };
        response.on_hover_text(format!("Input level: {said}. Loud passages should stay in the yellow, below the red."));
        ui.label(RichText::new(said).monospace().size(10.0).color(theme::DIM));
        // Full scale was reached: said until it is clicked away, because a
        // moment of red on the bar is missed while rapping (four of his
        // first five takes clipped, feedback F16.14).
        if view.input_level >= 0.985 {
            self.input_clipped = true;
        }
        if self.input_clipped
            && ui
                .add(egui::Button::new(RichText::new("clipped").size(10.5).color(theme::ERROR)).frame(false))
                .on_hover_text(
                    "The input reached full scale: what is louder than that is cut off and sounds harsh, and no \
                     setting repairs it. Turn the input (the microphone's level in the system's sound settings) \
                     down until loud passages stay in the yellow. Click to clear this.",
                )
                .clicked()
        {
            self.input_clipped = false;
        }
        if ui
            .selectable_label(self.hear_input, "Hear")
            .on_hover_text(
                "Adds the input to what is heard (a few milliseconds late). For headphones: through \
                 loudspeakers the microphone would pick itself up. With a track marked \"R\", it is heard \
                 through that track's low cut, equaliser, de-esser, leveller, compressor and saturation.",
            )
            .clicked()
        {
            self.hear_input = !self.hear_input;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }

    /// Beside "Record sound": which input the take comes from.
    fn input_menu(&mut self, ui: &mut egui::Ui) {
        let chosen = match (&self.input, &self.inputs) {
            (None, _) => "default".to_owned(),
            (Some(name), Some(inputs)) => {
                inputs.iter().find(|(n, _)| n == name).map_or_else(|| name.clone(), |(_, label)| label.clone())
            }
            (Some(name), None) => name.clone(),
        };
        // The button keeps a steady width: long device names are cut.
        let short: String = if chosen.chars().count() > 22 {
            chosen.chars().take(21).chain(std::iter::once('…')).collect()
        } else {
            chosen
        };
        let response = egui::containers::menu::MenuButton::new(format!("Input: {short}"))
            .ui(ui, |ui| {
                let mut choice = None;
                if ui.selectable_label(self.input.is_none(), "System default").clicked() {
                    choice = Some(None);
                }
                match &self.inputs {
                    None => {
                        ui.weak("Looking for inputs…");
                    }
                    Some(inputs) if inputs.is_empty() => {
                        ui.weak("No inputs found");
                    }
                    Some(inputs) => {
                        for (name, label) in inputs {
                            if ui.selectable_label(self.input.as_ref() == Some(name), label).clicked() {
                                choice = Some(Some(name.clone()));
                            }
                        }
                    }
                }
                if let Some(choice) = choice {
                    self.input = choice.clone();
                    self.input_requests.push(InputRequest::Choose(choice));
                    ui.close();
                }
            })
            .0;
        if response.clicked() {
            // Asked for anew at every opening: devices come and go.
            self.input_requests.push(InputRequest::List);
        }
        response.on_hover_text("Where \"Record sound\" records from");
    }

    fn inspector(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        sequence: &Sequence,
        target: Option<&Target>,
        error: &mut Option<String>,
        request: &mut Option<EditorRequest>,
    ) {
        let project = session.project().clone();
        ui.take_available_width();
        if let Some(target) = target {
            self.clip_panel.ui(ui, session, sequence, target, error);
            ui.add_space(10.0);
        }
        if let Some(asset) = self.assets.selected.and_then(|id| project.assets.get(&id)) {
            self.asset_section(ui, session, asset, error, request);
            ui.add_space(10.0);
        }
        section(ui, "PROJECT");
        row(ui, "Name", &project.name);
        row(ui, "Folder", &session.root.display().to_string());
        if actions::button(ui, Action::Rename, "Rename") {
            self.begin_rename(session);
        }
        ui.add_space(10.0);
        if let Some(song) = sequence.song {
            section(ui, "SONG");
            row(ui, "Asset", project.assets.get(&song.asset).map_or("missing", |a| a.label.as_str()));
            let tempo = match self.timeline.bpm(sequence) {
                Some(bpm) => format!("{bpm:.1} beats per minute"),
                None => "finding the beats...".to_owned(),
            };
            row(ui, "Tempo", &tempo);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Half").on_hover_text("Count the song in slower beats").clicked() {
                    *request = Some(EditorRequest::Tempo(song.tempo - 1));
                }
                if ui.button("Double").on_hover_text("Count the song in faster beats").clicked() {
                    *request = Some(EditorRequest::Tempo(song.tempo + 1));
                }
                // The song's tempo as the tempo of the sequence: bars, metronome and echo go by it.
                if let Some(bpm) = self.timeline.bpm(sequence) {
                    // (Songs made to a click are at a whole tempo; what was found is that within a hair.)
                    let whole = bpm.round();
                    let bpm = if (bpm - whole).abs() < 0.3 { whole } else { (bpm * 10.0).round() / 10.0 };
                    // The first bar begins where the song's first beat lies on the timeline.
                    let wanted = sg_core::Tempo {
                        milli_bpm: (bpm as f64 * 1000.0).round() as u32,
                        beats_per_bar: sequence.tempo.map_or(4, |t| t.beats_per_bar),
                        origin: self.timeline.first_beat(sequence).unwrap_or(0),
                    };
                    let tip = format!(
                        "Gives the sequence the song's tempo ({bpm} beats per minute): the ruler counts bars, the \
                         metronome and the echo go by it. The first bar begins on the song's first beat; if the \
                         song counts differently, put the playhead on its one and press \"Bar 1 here\" above the \
                         timeline."
                    );
                    let button = ui.add_enabled(sequence.tempo != Some(wanted), egui::Button::new("Bars at this tempo"));
                    if button.on_hover_text(tip).clicked()
                        && let Err(e) = session.apply(Op::SetTempo { id: sequence.id, tempo: Some(wanted) }, None)
                    {
                        *error = Some(e.to_string());
                    }
                }
                if ui
                    .button("Sync all takes")
                    .on_hover_text("Finds every video with sound in the song and lays it on a camera track")
                    .clicked()
                {
                    *request = Some(EditorRequest::SyncAll);
                }
                if ui.button("Cuts to the beat").on_hover_text("Moves every cut of the programme onto the nearest beat, if one is within a third of a beat. Nothing slips out of sync.").clicked() {
                    *request = Some(EditorRequest::CutsToBeats);
                }
                if ui.button("Whip every cut").on_hover_text("A whip-pan on every cut of the programme that has no transition, left and right in turn").clicked() {
                    *request = Some(EditorRequest::WhipAll);
                }
                if ui.button("No song").clicked() {
                    *request = Some(EditorRequest::ClearSong);
                }
            });
            ui.label(
                RichText::new("Keys 1 to 9 cut to a camera angle at the playhead, also while playing.")
                    .color(theme::DIM),
            );
            ui.add_space(10.0);
        }
        section(ui, "SEQUENCE");
        row(ui, "Name", &sequence.name);
        row(ui, "Format", &format!("{}x{}, {}", sequence.width, sequence.height, fps(sequence.rate)));
        ui.horizontal(|ui| {
            ui.add_sized([84.0, 16.0], egui::Label::new(RichText::new("Frame").color(theme::DIM)).halign(Align::LEFT));
            // Changing the frame keeps every clip; pictures are fitted anew.
            for (label, width, height) in
                [("1080p", 1920, 1080), ("4K", 3840, 2160), ("9:16", 1080, 1920), ("1:1", 1080, 1080)]
            {
                let current = (sequence.width, sequence.height) == (width, height);
                if ui.selectable_label(current, label).clicked() && !current {
                    *request = Some(EditorRequest::SetSize(width, height));
                }
            }
        });
        self.tempo_row(ui, session, sequence, error);
        row(ui, "Tracks", &sequence.tracks.len().to_string());
        row(ui, "Clips", &sequence.clip_count().to_string());
        ui.horizontal(|ui| {
            if ui.button("Add video track").clicked() {
                *request = Some(EditorRequest::AddTrack(sg_core::TrackKind::Video));
            }
            if ui.button("Add audio track").clicked() {
                *request = Some(EditorRequest::AddTrack(sg_core::TrackKind::Audio));
            }
        });
        ui.add_space(10.0);
        section(ui, "HISTORY");
        let history = session.document().history();
        row(ui, "Undo steps", &history.undo_len().to_string());
        row(ui, "Redo steps", &history.redo_len().to_string());
        ui.horizontal(|ui| {
            if actions::enabled_button(ui, history.can_undo(), Action::Undo, "Undo") {
                self.actions.push(Action::Undo);
            }
            if actions::enabled_button(ui, history.can_redo(), Action::Redo, "Redo") {
                self.actions.push(Action::Redo);
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

impl Editor {
    fn asset_section(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        asset: &Asset,
        error: &mut Option<String>,
        request: &mut Option<EditorRequest>,
    ) {
        section(ui, "ASSET");
        // Editing a copy; committed as one op when the field loses focus.
        if self.asset_label.as_ref().is_none_or(|(id, _)| *id != asset.id) {
            self.asset_label = Some((asset.id, asset.label.clone()));
        }
        if let Some((_, label)) = &mut self.asset_label {
            let response = ui.add(egui::TextEdit::singleline(label).desired_width(f32::INFINITY));
            if response.lost_focus() {
                let label = label.trim().to_owned();
                if !label.is_empty()
                    && label != asset.label
                    && let Err(e) = session.apply(Op::SetAssetLabel { id: asset.id, label }, None)
                {
                    *error = Some(e.to_string());
                }
                self.asset_label = None;
            }
        }
        row(ui, "Kind", asset.kind.dir());
        row(ui, "Length", &summary(asset));
        if let Some(v) = &asset.video {
            let depth = if v.bit_depth > 8 { format!(" {}-bit", v.bit_depth) } else { String::new() };
            let rate = if v.is_still() { String::new() } else { format!(", {}", fps(v.rate)) };
            row(ui, "Picture", &format!("{}{depth}{rate}", v.codec));
        }
        if let Some(a) = &asset.audio {
            let layout = match a.channels {
                1 => "mono".to_owned(),
                2 => "stereo".to_owned(),
                n => format!("{n} ch"),
            };
            row(ui, "Sound", &format!("{}, {} Hz, {layout}", a.codec, a.sample_rate));
        }
        row(ui, "From", &asset.source.origin);
        row(ui, "File", &asset.file);
        if self.assets.missing.contains(&asset.id) {
            ui.label(RichText::new("The media file is missing from the project folder.").color(theme::ERROR));
        }
        if ui.button("Remove from project (Del)").clicked() {
            *request = Some(EditorRequest::RemoveAsset(asset.id));
        }
    }
}

impl Editor {
    /// The selected timeline clip with its properties, at the playhead.
    pub fn target(&self, session: &Session, playhead: i64) -> Option<Target> {
        let project = session.project();
        let sequence = project.sequences.first()?;
        let (track, clip) = self.timeline.primary(sequence)?;
        Some(Target::new(sequence, &track, clip, playhead))
    }

    /// The tempo of the sequence (09-daw.md): none, in which case the
    /// timeline counts time, or beats per minute and beats per bar.
    fn tempo_row(&mut self, ui: &mut egui::Ui, session: &mut Session, sequence: &Sequence, error: &mut Option<String>) {
        ui.horizontal(|ui| {
            ui.add_sized([84.0, 16.0], egui::Label::new(RichText::new("Tempo").color(theme::DIM)).halign(Align::LEFT));
            self.tempo_fields(ui, session, sequence, error, false);
        });
    }

    /// The fields of the tempo, in the inspector and above the timeline.
    /// Above the timeline the button that starts one is called "Tempo", and
    /// taking the tempo away again is left to the inspector.
    fn tempo_fields(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        sequence: &Sequence,
        error: &mut Option<String>,
        toolbar: bool,
    ) {
        let dragging = ui.input(|i| i.pointer.any_down());
        if !dragging {
            self.tempo_group = None;
        }
        let mut wanted = sequence.tempo;
        match &mut wanted {
            None => {
                let button = ui.button(if toolbar { "Set tempo" } else { "Bars and beats" });
                if button
                    .on_hover_text(
                        "Gives the sequence a tempo for making music: the ruler counts bars instead of time and \
                         clips snap to beats. Nothing that is on the timeline moves.",
                    )
                    .clicked()
                {
                    wanted = Some(sg_core::Tempo::DEFAULT);
                }
            }
            Some(tempo) => {
                let mut bpm = tempo.bpm();
                let field =
                    egui::DragValue::new(&mut bpm).range(20.0..=400.0).speed(0.1).max_decimals(1).suffix(" bpm");
                if ui.add(field).on_hover_text("Beats per minute").changed() {
                    tempo.milli_bpm = (bpm * 1000.0).round() as u32;
                }
                let mut beats = tempo.beats_per_bar;
                let field = egui::DragValue::new(&mut beats).range(1..=16).speed(0.05).suffix(" / bar");
                if ui.add(field).on_hover_text("Beats in a bar: 4 for most music, 3 for a waltz").changed() {
                    tempo.beats_per_bar = beats;
                }
                if !toolbar && ui.button("Off").on_hover_text("Back to a ruler in time").clicked() {
                    wanted = None;
                }
            }
        }
        // The tempo by hand: clicked in time with the music.
        let tap = ui.button("Tap");
        if tap
            .on_hover_text(
                "Click in time with the music, four times or more: the tempo becomes the pace of your clicks \
                 (a whole number of beats a minute). Two seconds without a click start anew.",
            )
            .clicked()
        {
            let now = ui.input(|i| i.time);
            if let Some(bpm) = tapped(&mut self.taps, now) {
                let tempo = wanted.get_or_insert(sg_core::Tempo::DEFAULT);
                tempo.milli_bpm = bpm * 1000;
            }
        }
        if wanted != sequence.tempo {
            let group = dragging.then(|| *self.tempo_group.get_or_insert_with(|| session.new_group()));
            if let Err(e) = session.apply(Op::SetTempo { id: sequence.id, tempo: wanted }, group) {
                *error = Some(e.to_string());
            }
        }
    }

    /// What clips and the playhead snap to while there is a tempo.
    fn grid_menu(&mut self, ui: &mut egui::Ui) {
        use crate::timeline::GridStep;
        let now = self.timeline.grid_step;
        egui::containers::menu::MenuButton::new(format!("Grid: {}", now.label()))
            .ui(ui, |ui| {
                for step in GridStep::ALL {
                    if ui.selectable_label(now == step, step.label()).clicked() {
                        self.timeline.grid_step = step;
                        ui.close();
                    }
                }
            })
            .0
            .on_hover_text(
                "What edges snap to and which lines are drawn. \"auto\" follows the zoom; a T is a triplet \
                 (three in the room of two).",
            );
    }

    /// Brings the inspector to the selected title with the cursor in its text.
    fn show_title_text(&mut self) {
        (self.mixer.open, self.colour.open) = (false, false);
        self.clip_panel.show_title_text();
    }

    /// Adds a track: video on top, audio at the bottom.
    pub fn add_track(&mut self, session: &mut Session, kind: sg_core::TrackKind, error: &mut Option<String>) {
        if let Err(e) = session.add_track(kind) {
            *error = Some(e.to_string());
        }
    }

    /// Keyframe, title and record keys.
    pub fn clip_action(
        &mut self,
        action: crate::keymap::Action,
        session: &mut Session,
        playhead: i64,
        playing: bool,
        error: &mut Option<String>,
    ) -> Vec<Transport> {
        use crate::keymap::Action;
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first().cloned() else { return Vec::new() };
        let target = self.target(session, playhead);
        let mut transport = Vec::new();
        match action {
            Action::KeyframeSet | Action::KeyframeRemove => {
                let Some(target) = target else { return transport };
                let video = target.track.kind == sg_core::TrackKind::Video;
                let mut props = target.props.clone();
                let at = target.at;
                let key = |param: &mut sg_core::Param| {
                    if action == Action::KeyframeSet {
                        let now = param.at(at);
                        param.set_key(at, now);
                    } else {
                        param.remove_key(at);
                    }
                };
                // A picture clip keys its placement, a sound clip its volume.
                if video {
                    props.picture_params().into_iter().for_each(key);
                } else {
                    key(&mut props.gain_db);
                }
                let props = (props != sg_core::ClipProps::default()).then(|| Box::new(props));
                let op = Op::SetClipProps { seq: sequence.id, track: target.track.id, clip: target.clip.id, props };
                if let Err(e) = session.apply(op, None) {
                    *error = Some(e.to_string());
                }
            }
            Action::AddTitle => {
                // Three seconds, on the topmost video track that is free
                // there; a new track on top if none is.
                let len = (3 * sequence.rate.num() as i64 / sequence.rate.den() as i64).max(1);
                let free = sequence
                    .tracks
                    .iter()
                    .find(|t| {
                        t.kind == sg_core::TrackKind::Video && t.clips.range(playhead, playhead + len).next().is_none()
                    })
                    .map(|t| t.id);
                let mut ops = Vec::new();
                let track = match free {
                    Some(id) => id,
                    None => {
                        let id = session.new_track_id();
                        let count = sequence.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Video).count();
                        let header = sg_core::TrackHeader {
                            id,
                            kind: sg_core::TrackKind::Video,
                            name: format!("V{}", count + 1),
                        };
                        ops.push(Op::AddTrack { seq: sequence.id, index: 0, track: header });
                        id
                    }
                };
                // A title has no asset: the id is all zeros.
                let clip = sg_core::Clip {
                    id: session.new_clip_id(),
                    asset: sg_core::AssetId(0),
                    start: playhead,
                    len,
                    src_in: 0,
                };
                let props =
                    sg_core::ClipProps { title: Some(sg_core::Title::default()), ..sg_core::ClipProps::default() };
                ops.push(Op::InsertClip { seq: sequence.id, track, clip });
                ops.push(Op::SetClipProps { seq: sequence.id, track, clip: clip.id, props: Some(Box::new(props)) });
                match session.apply(Op::Batch(ops), None) {
                    Ok(()) => {
                        self.timeline.select_only(track, &clip);
                        self.show_title_text();
                    }
                    Err(e) => *error = Some(e.to_string()),
                }
            }
            Action::LiveRecord => {
                if self.clip_panel.recording {
                    self.clip_panel.finish_recording(session, &sequence, target.as_ref(), error);
                    if playing {
                        transport.push(Transport::Toggle);
                    }
                } else if target.is_some() {
                    self.clip_panel.recording = true;
                    if !playing {
                        transport.push(Transport::Toggle);
                    }
                } else {
                    *error =
                        Some("Select a clip first, then press R and drag it in the viewer while it plays.".to_owned());
                }
            }
            _ => {}
        }
        transport
    }
}

impl Editor {
    /// Sets one property of the selected clip by name (scripted runs).
    pub fn set_prop(
        &mut self,
        session: &mut Session,
        playhead: i64,
        name: &str,
        value: &str,
        error: &mut Option<String>,
    ) {
        // Not a property: which page of the clip's inspector is shown (`page=keys`).
        if name == "page" {
            return self.clip_panel.show_keyframes(value == "keys");
        }
        let project = session.project().clone();
        let (Some(sequence), Some(target)) = (project.sequences.first(), self.target(session, playhead)) else {
            *error = Some("No clip is selected.".to_owned());
            return;
        };
        let mut props = target.props.clone();
        let number = value.parse::<f32>().unwrap_or(0.0);
        match name {
            "x" => props.x.set(target.at, number),
            "y" => props.y.set(target.at, number),
            "scale" => props.scale.set(target.at, number),
            "rotation" => props.rotation.set(target.at, number),
            "opacity" => props.opacity.set(target.at, number),
            "ramp" => props.ramp.set(target.at, number),
            "exposure" => props.exposure = number,
            "warmth" => props.warmth = number,
            "stabilise" => {
                props.stabilise = number != 0.0;
                if props.stabilise {
                    self.clip_panel.stabilise_request = Some(target.clip.asset);
                }
            }
            "gain" => props.gain_db.set(target.at, number),
            "speed" => props.speed = (number.max(1.0) as u32, 100),
            "fadein" => props.fade_in = number as i64,
            "fadeout" => props.fade_out = number as i64,
            "crop" => props.crop = [number; 4],
            "text" => {
                if let Some(title) = &mut props.title {
                    title.text = value.replace("\\n", "\n");
                }
            }
            "shadow" => {
                if let Some(title) = &mut props.title {
                    title.shadow = number;
                }
            }
            "font" => {
                if let Some(title) = &mut props.title {
                    title.font = value.to_owned();
                }
            }
            _ => {
                *error = Some(format!("Unknown clip property {name}"));
                return;
            }
        }
        let props = (props != sg_core::ClipProps::default()).then(|| Box::new(props));
        let op = Op::SetClipProps { seq: sequence.id, track: target.track.id, clip: target.clip.id, props };
        if let Err(e) = session.apply(op, None) {
            *error = Some(e.to_string());
        }
    }
}

impl Editor {
    pub fn set_size(&mut self, session: &mut Session, width: u32, height: u32, error: &mut Option<String>) {
        let Some(id) = session.project().sequences.first().map(|s| s.id) else { return };
        if let Err(e) = session.apply(Op::SetSequenceSize { id, width, height }, None) {
            *error = Some(e.to_string());
        }
    }
}

/// A click on "Tap" at `now` seconds: the beats a minute the clicks so
/// far are at, from the fourth one on. A pause of two seconds starts a
/// new count; the last eight clicks count, so that the hand can settle.
fn tapped(taps: &mut Vec<f64>, now: f64) -> Option<u32> {
    if taps.last().is_some_and(|last| now - last > 2.0 || now < *last) {
        taps.clear();
    }
    taps.push(now);
    if taps.len() > 8 {
        taps.remove(0);
    }
    let (first, last) = (*taps.first()?, *taps.last()?);
    (taps.len() >= 4 && last > first)
        .then(|| (60.0 * (taps.len() - 1) as f64 / (last - first)).round().clamp(20.0, 400.0) as u32)
}

#[cfg(test)]
mod tap_tests {
    use super::tapped;

    #[test]
    fn four_clicks_in_time_give_the_tempo_and_a_pause_starts_anew() {
        let mut taps = Vec::new();
        // Clicks 0.652 s apart: 92 beats a minute, from the fourth click on.
        let got: Vec<Option<u32>> = (0..5).map(|n| tapped(&mut taps, 10.0 + n as f64 * 0.652)).collect();
        assert_eq!(got, [None, None, None, Some(92), Some(92)]);
        // After a pause the count starts again; then twice as fast.
        assert_eq!(tapped(&mut taps, 20.0), None);
        let fast: Vec<Option<u32>> = (1..4).map(|n| tapped(&mut taps, 20.0 + n as f64 * 0.326)).collect();
        assert_eq!(fast, [None, None, Some(184)]);
    }
}
