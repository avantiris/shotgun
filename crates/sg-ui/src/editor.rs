//! The editor screen: the fixed panel layout around an open project.
//! Viewer and timeline fill up in M3 and M4.

use std::sync::atomic::Ordering;

use egui::{Align, FontId, Layout, RichText, Sense, vec2};
use sg_core::{Asset, AssetId, Op, Rate, Sequence};

use crate::assets::{AssetRequest, AssetsPanel, summary};
use crate::clip_panel::{ClipPanel, Target};
use crate::session::Session;
use crate::stage::{self, Transport, ViewerState};
use crate::theme;
use crate::timeline::Timeline;

pub enum EditorRequest {
    Close,
    Undo,
    Redo,
    RemoveAsset(AssetId),
    AddTrack(sg_core::TrackKind),
    SetSize(u32, u32),
    /// Cut to this camera angle (0 is the first) at the playhead.
    Angle(usize),
    Tempo(i8),
    SyncAll,
    ClearSong,
    Export,
}

#[derive(Default)]
pub struct Editor {
    /// The name being typed while renaming the project.
    rename: Option<String>,
    focus_rename: bool,
    pub assets: AssetsPanel,
    pub timeline: Timeline,
    pub clip_panel: ClipPanel,
    pub mixer: crate::mixer::Mixer,
    pub colour: crate::colour_panel::ColourPanel,
    /// What the colour panel asked for this frame; taken by the shell.
    pub colour_requests: Vec<crate::colour_panel::ColourRequest>,
    was_playing: bool,
    /// The label being typed for the selected asset.
    asset_label: Option<(AssetId, String)>,
}

impl Editor {
    pub fn begin_rename(&mut self, session: &Session) {
        self.rename = Some(session.project().name.clone());
        self.focus_rename = true;
    }

    /// Cancels what is in progress: popover first, then a rename. Returns
    /// whether there was something.
    pub fn cancel(&mut self) -> bool {
        self.assets.close_add() || self.rename.take().is_some() || self.asset_label.take().is_some()
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
        let project = session.project().clone();
        let Some(sequence) = project.sequences.first() else {
            egui::CentralPanel::default().show(ui, |ui| ui.label("This project has no sequence."));
            return request;
        };
        let flush = egui::Frame::new().fill(theme::BG);
        egui::Panel::bottom("timeline").resizable(true).default_size(230.0).min_size(120.0).frame(flush).show(
            ui,
            |ui| {
                self.timeline.ui(ui, session, view, transport, error);
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
        egui::Panel::left("assets").default_size(320.0).min_size(170.0).show(ui, |ui| {
            self.assets.ui(ui, &project, asset_requests);
        });
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
                    self.mixer.ui(ui, session, sequence, view, error);
                } else {
                    self.inspector(ui, session, sequence, target.as_ref(), error, &mut request);
                }
            });
        });
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
        let events = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(12.0))
            .show(ui, |ui| stage::viewer(ui, sequence, view, recording, live))
            .inner;
        if let Some(angle) = events.angle {
            request = Some(EditorRequest::Angle(angle));
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
                .selectable_label(self.mixer.open, "Mixer")
                .on_hover_text("Levels, pan, mute, solo and clean-up per audio track (M)")
                .clicked()
            {
                self.mixer.open = !self.mixer.open;
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
            if ui.button("Export").on_hover_text("Ctrl+E").clicked() {
                *request = Some(EditorRequest::Export);
            }
        });
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
        if ui.button("Rename (F2)").clicked() {
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
                if ui
                    .button("Sync all takes")
                    .on_hover_text("Finds every video with sound in the song and lays it on a camera track")
                    .clicked()
                {
                    *request = Some(EditorRequest::SyncAll);
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
                let Some(target) = target.filter(|t| t.track.kind == sg_core::TrackKind::Video) else {
                    return transport;
                };
                let mut props = target.props.clone();
                for param in props.picture_params() {
                    if action == Action::KeyframeSet {
                        let now = param.at(target.at);
                        param.set_key(target.at, now);
                    } else {
                        param.remove_key(target.at);
                    }
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
                    Ok(()) => self.timeline.select_only(track, &clip),
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
