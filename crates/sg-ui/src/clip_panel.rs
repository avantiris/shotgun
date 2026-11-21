//! The inspector section for the selected clip: placement, crop, opacity,
//! speed, volume, fades, title text, and its keyframes.

use egui::RichText;
use sg_core::time::SAMPLE_RATE;
use sg_core::{Clip, ClipProps, Key, Op, Param, Sequence, Track, TrackKind, props};

use crate::session::Session;
use crate::theme;

/// The clip the inspector and the viewer act on.
#[derive(Clone)]
pub struct Target {
    pub track: std::sync::Arc<Track>,
    pub clip: Clip,
    /// Playhead relative to the clip start, in sequence frames.
    pub at: i64,
    pub props: ClipProps,
}

impl Target {
    pub fn new(sequence: &Sequence, track: &std::sync::Arc<Track>, clip: Clip, playhead: i64) -> Target {
        let start = match track.kind {
            TrackKind::Video => clip.start,
            TrackKind::Audio => sequence.rate.samples_to_frames(clip.start, SAMPLE_RATE),
        };
        let props = track.props_of(clip.id).map(|p| ClipProps::clone(p)).unwrap_or_default();
        Target { track: track.clone(), clip, at: playhead - start, props }
    }
}

#[derive(Default)]
pub struct ClipPanel {
    /// Undo group of the gesture in progress: one drag of a slider, or one
    /// drag in the viewer, is one undo step.
    group: Option<u64>,
    /// Motion recorded while playing: (frame relative to the clip, x, y).
    recorded: Vec<(i64, f32, f32)>,
    pub recording: bool,
    /// An asset whose stabilised copy should be made; taken by the shell.
    pub stabilise_request: Option<sg_core::AssetId>,
    /// The fonts in the project's folder, once they have been listed.
    pub fonts: Option<Vec<String>>,
    /// The list of fonts is wanted; taken by the shell.
    pub fonts_request: bool,
    fonts_asked: bool,
    /// Put the cursor into the title's text field with the next frame (a
    /// title was just added, or asked for by double click or menu).
    focus_title: bool,
    /// A keyframe in the list was clicked: move the playhead to this
    /// sequence frame. Taken by the editor.
    pub seek_request: Option<i64>,
    /// The inspector shows the clip's keyframes instead of its settings.
    keys_tab: bool,
    /// ... or the transition into the clip, with all its settings.
    transition_tab: bool,
}

/// Writes changed properties back as an op.
fn commit(
    session: &mut Session,
    sequence: &Sequence,
    target: &Target,
    props: ClipProps,
    group: Option<u64>,
    error: &mut Option<String>,
) {
    // Defaults are not stored.
    let props = (props != ClipProps::default()).then(|| Box::new(props));
    let op = Op::SetClipProps { seq: sequence.id, track: target.track.id, clip: target.clip.id, props };
    if let Err(e) = session.apply(op, group) {
        *error = Some(e.to_string());
    }
}

impl ClipPanel {
    /// Shows the settings page with the cursor in the title's text.
    pub fn show_title_text(&mut self) {
        (self.keys_tab, self.transition_tab, self.focus_title) = (false, false, true);
    }

    /// Shows the keyframes page (or the settings).
    pub fn show_keyframes(&mut self, shown: bool) {
        (self.keys_tab, self.transition_tab) = (shown, false);
    }

    /// Shows the transition page: one was just put on the clip's cut.
    pub fn show_transition(&mut self) {
        (self.keys_tab, self.transition_tab) = (false, true);
    }

    /// The undo group for an edit made now. While a pointer button is held
    /// (a drag), every edit shares one group.
    fn group_for(&mut self, ui_dragging: bool, session: &mut Session) -> Option<u64> {
        if !ui_dragging {
            self.group = None;
            return None;
        }
        Some(*self.group.get_or_insert_with(|| session.new_group()))
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        sequence: &Sequence,
        target: &Target,
        error: &mut Option<String>,
    ) {
        let mut props = target.props.clone();
        let heading = if props.title.is_some() { "TITLE" } else { "CLIP" };
        ui.label(RichText::new(heading).size(11.0).strong().color(theme::DIM));
        // Two pages: the clip's settings, and its keyframes in detail.
        let keyframes = {
            let ClipProps { x, y, scale, rotation, opacity, ramp, gain_db, .. } = &props;
            let frames: std::collections::BTreeSet<i64> = [x, y, scale, rotation, opacity, ramp, gain_db]
                .iter()
                .flat_map(|p| p.keys.iter().map(|k| k.frame))
                .collect();
            frames.len()
        };
        ui.horizontal(|ui| {
            let video = target.track.kind == TrackKind::Video;
            // (A sound clip has no transition page.)
            self.transition_tab &= video;
            if ui.selectable_label(!self.keys_tab && !self.transition_tab, "Settings").clicked() {
                (self.keys_tab, self.transition_tab) = (false, false);
            }
            if video {
                let name = if props.transition.is_some() { "Transition (1)" } else { "Transition" };
                let tab = ui.selectable_label(self.transition_tab, name);
                tab.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Transition"));
                if tab.on_hover_text("How the picture arrives from the clip before this one").clicked() {
                    (self.keys_tab, self.transition_tab) = (false, true);
                }
            }
            let tab = ui.selectable_label(self.keys_tab, format!("Keyframes ({keyframes})"));
            tab.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Keyframes"));
            if tab.on_hover_text("Every keyframe of this clip: when, which values, and how they travel on").clicked() {
                (self.keys_tab, self.transition_tab) = (true, false);
            }
        });
        ui.add_space(4.0);
        if self.transition_tab {
            let changed = transition_rows(ui, &mut props.transition, target);
            return self.finish(ui, changed, session, sequence, target, props, error);
        }
        if self.keys_tab {
            let start = match target.track.kind {
                TrackKind::Video => target.clip.start,
                TrackKind::Audio => sequence.rate.samples_to_frames(target.clip.start, SAMPLE_RATE),
            };
            let changed = keyframe_list(ui, &mut props, target.at, start, sequence.rate, &mut self.seek_request);
            return self.finish(ui, changed, session, sequence, target, props, error);
        }
        let at = target.at;
        let video = target.track.kind == TrackKind::Video;
        let fps = sequence.rate.num() as f32 / sequence.rate.den() as f32;
        let mut changed = false;

        if let Some(title) = &mut props.title {
            ui.label(RichText::new("Text (type here; Enter starts a new line)").size(10.5).color(theme::DIM));
            let mut shown = egui::TextEdit::multiline(&mut title.text)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("What the title says")
                .show(ui);
            let field = shown.response;
            field.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, "Title text"));
            if std::mem::take(&mut self.focus_title) && !self.keys_tab {
                // With everything selected: typing replaces the text that is there.
                use egui::text::{CCursor, CCursorRange};
                field.request_focus();
                let all = CCursorRange::two(CCursor::new(0), CCursor::new(title.text.chars().count()));
                shown.state.cursor.set_char_range(Some(all));
                shown.state.store(ui.ctx(), field.id);
            }
            changed |= field.changed();
            ui.horizontal(|ui| {
                ui.add_sized(
                    [84.0, 16.0],
                    egui::Label::new(RichText::new("Text size").color(theme::DIM)).halign(egui::Align::LEFT),
                );
                changed |= ui
                    .add(egui::DragValue::new(&mut title.size).speed(0.002).range(0.01..=0.8).max_decimals(3))
                    .changed();
                let mut colour = egui::Color32::from_rgba_unmultiplied(
                    title.color[0],
                    title.color[1],
                    title.color[2],
                    title.color[3],
                );
                if ui.color_edit_button_srgba(&mut colour).changed() {
                    title.color = colour.to_srgba_unmultiplied();
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized(
                    [84.0, 16.0],
                    egui::Label::new(RichText::new("Shadow").color(theme::DIM)).halign(egui::Align::LEFT),
                );
                changed |= ui
                    .add(egui::DragValue::new(&mut title.shadow).speed(0.01).range(0.0..=1.0).max_decimals(2))
                    .on_hover_text(
                        "A soft dark shadow under the letters, for titles on bright or busy pictures. 0 is none",
                    )
                    .changed();
            });
            // The folder is listed the first time a title is shown.
            if !self.fonts_asked {
                (self.fonts_asked, self.fonts_request) = (true, true);
            }
            ui.horizontal(|ui| {
                ui.add_sized(
                    [84.0, 16.0],
                    egui::Label::new(RichText::new("Font").color(theme::DIM)).halign(egui::Align::LEFT),
                );
                let shown = if title.font.is_empty() { "Built-in" } else { title.font.as_str() };
                egui::ComboBox::from_id_salt("title font")
                    .selected_text(shown.to_owned())
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(title.font.is_empty(), "Built-in").clicked() && !title.font.is_empty() {
                            title.font.clear();
                            changed = true;
                        }
                        for name in self.fonts.iter().flatten() {
                            if ui.selectable_label(title.font == *name, name).clicked() && title.font != *name {
                                title.font = name.clone();
                                changed = true;
                            }
                        }
                    })
                    .response
                    .on_hover_text("Drop a .ttf or .otf file onto the window to add a font to the project");
            });
        }
        if video {
            changed |= value_row(ui, "Position X", &mut props.x, at, -2.0..=2.0, 0.002, "");
            changed |= value_row(ui, "Position Y", &mut props.y, at, -2.0..=2.0, 0.002, "");
            changed |= value_row(ui, "Scale", &mut props.scale, at, 0.01..=10.0, 0.005, "");
            // A picture of another shape than the frame: fitted with bars
            // (scale 1) or enlarged until it covers the frame.
            let cover = session.project().assets.get(&target.clip.asset).and_then(|a| a.video.as_ref()).map(|v| {
                let (w, h) = if v.rotation % 180 == 90 { (v.height, v.width) } else { (v.width, v.height) };
                let (across, down) =
                    (sequence.width as f32 / w.max(1) as f32, sequence.height as f32 / h.max(1) as f32);
                across.max(down) / across.min(down)
            });
            if let Some(cover) = cover.filter(|c| *c > 1.01 && props.title.is_none()) {
                ui.horizontal(|ui| {
                    ui.add_space(92.0);
                    for (label, scale, tip) in [
                        ("Fit", 1.0, "The whole picture, with bars where its shape differs from the frame"),
                        ("Fill", cover, "Enlarged until the frame is covered; the picture loses its edges"),
                    ] {
                        let now = (props.scale.at(at) - scale).abs() < 0.001;
                        if ui.selectable_label(now, label).on_hover_text(tip).clicked() && !now {
                            props.scale.set(at, scale);
                            props.x.set(at, 0.0);
                            props.y.set(at, 0.0);
                            changed = true;
                        }
                    }
                });
            }
            changed |= value_row(ui, "Rotation", &mut props.rotation, at, -360.0..=360.0, 0.5, " deg");
            changed |= value_row(ui, "Opacity", &mut props.opacity, at, 0.0..=1.0, 0.005, "");
            ui.horizontal(|ui| {
                ui.add_sized(
                    [84.0, 16.0],
                    egui::Label::new(RichText::new("Crop L T R B").color(theme::DIM)).halign(egui::Align::LEFT),
                );
                for side in &mut props.crop {
                    changed |=
                        ui.add(egui::DragValue::new(side).speed(0.002).range(0.0..=0.49).max_decimals(3)).changed();
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .button("Keyframe (F)")
                    .on_hover_text("Fix position, scale, rotation and opacity at the playhead")
                    .clicked()
                {
                    for param in props.picture_params() {
                        let now = param.at(at);
                        param.set_key(at, now);
                    }
                    changed = true;
                }
                if ui.button("Remove (Shift+F)").clicked() {
                    for param in props.picture_params() {
                        changed |= param.remove_key(at);
                    }
                }
            });
        } else {
            changed |= value_row(ui, "Volume", &mut props.gain_db, at, -60.0..=24.0, 0.1, " dB");
        }
        if props.title.is_none() {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [84.0, 16.0],
                    egui::Label::new(RichText::new("Speed").color(theme::DIM)).halign(egui::Align::LEFT),
                );
                let mut percent = props.speed.0 as f32 * 100.0 / props.speed.1.max(1) as f32;
                if ui
                    .add(egui::DragValue::new(&mut percent).speed(1.0).range(5.0..=1600.0).suffix(" %").max_decimals(0))
                    .changed()
                {
                    props.speed = (percent.round().max(1.0) as u32, 100);
                    changed = true;
                }
            });
        }
        if video && props.title.is_none() {
            // A factor on the speed that keyframes can change along the clip.
            // The sound of the take is not ramped with it.
            changed |= value_row(ui, "Speed ramp", &mut props.ramp, at, 0.05..=8.0, 0.005, " x");
            for (name, value, range, tip) in [
                (
                    "Exposure",
                    &mut props.exposure,
                    -4.0..=4.0,
                    "Brightness of this clip alone, in stops, on top of its scenario's match",
                ),
                ("Warmth", &mut props.warmth, -1.0..=1.0, "Cooler or warmer, this clip alone"),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [84.0, 16.0],
                        egui::Label::new(RichText::new(name).color(theme::DIM)).halign(egui::Align::LEFT),
                    );
                    let response =
                        ui.add(egui::DragValue::new(value).range(range).speed(0.01).max_decimals(2)).on_hover_text(tip);
                    changed |= response.changed();
                    if response.double_clicked() {
                        *value = 0.0;
                        changed = true;
                    }
                });
            }
            let was = props.stabilise;
            ui.checkbox(&mut props.stabilise, "Stabilise").on_hover_text(
                "Shows a copy of the take with the camera shake taken out. The copy is made once per asset, in the background; \
                 until it is ready the picture is shown as it is.",
            );
            if props.stabilise != was {
                changed = true;
                if props.stabilise {
                    self.stabilise_request = Some(target.clip.asset);
                }
            }
        }
        ui.horizontal(|ui| {
            ui.add_sized(
                [84.0, 16.0],
                egui::Label::new(RichText::new("Fade in / out").color(theme::DIM)).halign(egui::Align::LEFT),
            );
            for fade in [&mut props.fade_in, &mut props.fade_out] {
                let mut secs = *fade as f32 / fps;
                if ui
                    .add(egui::DragValue::new(&mut secs).speed(0.02).range(0.0..=30.0).suffix(" s").max_decimals(2))
                    .changed()
                {
                    *fade = (secs * fps).round() as i64;
                    changed = true;
                }
            }
        });
        self.finish(ui, changed, session, sequence, target, props, error);
    }

    /// Writes what the panel changed this frame; one drag is one undo step.
    #[allow(clippy::too_many_arguments)]
    fn finish(
        &mut self,
        ui: &egui::Ui,
        changed: bool,
        session: &mut Session,
        sequence: &Sequence,
        target: &Target,
        props: ClipProps,
        error: &mut Option<String>,
    ) {
        if changed {
            let dragging = ui.input(|i| i.pointer.any_down());
            let group = self.group_for(dragging, session);
            commit(session, sequence, target, props, group, error);
        } else if !ui.input(|i| i.pointer.any_down()) {
            self.group = None;
        }
    }

    /// A drag in the viewer moves the clip: `delta` is in frame widths and
    /// heights. While recording during playback the motion is collected
    /// and becomes keyframes when [`finish_recording`](Self::finish_recording) runs.
    pub fn viewer_drag(
        &mut self,
        session: &mut Session,
        sequence: &Sequence,
        target: &Target,
        delta: egui::Vec2,
        playing: bool,
        error: &mut Option<String>,
    ) {
        if target.track.kind != TrackKind::Video {
            return;
        }
        let mut props = target.props.clone();
        let (x, y) = (props.x.at(target.at) + delta.x, props.y.at(target.at) + delta.y);
        if self.recording && playing {
            // Shown at once through a plain value; keyframes come at the end.
            self.recorded.push((target.at, x, y));
            props.x = Param::constant(x);
            props.y = Param::constant(y);
        } else {
            props.x.set(target.at, x);
            props.y.set(target.at, y);
        }
        let group = self.group_for(true, session);
        commit(session, sequence, target, props, group, error);
    }

    /// A handle in the viewer was dragged: the picture grows by `factor`
    /// and turns by `degrees`.
    pub fn viewer_handles(
        &mut self,
        session: &mut Session,
        sequence: &Sequence,
        target: &Target,
        factor: f32,
        degrees: f32,
        error: &mut Option<String>,
    ) {
        let mut props = target.props.clone();
        props.scale.set(target.at, (props.scale.at(target.at) * factor).clamp(0.01, 10.0));
        props.rotation.set(target.at, props.rotation.at(target.at) + degrees);
        let group = self.group_for(true, session);
        commit(session, sequence, target, props, group, error);
    }

    /// Turns the recorded motion into keyframes, thinned so that the path
    /// stays editable. One undo step together with the drag that made it.
    pub fn finish_recording(
        &mut self,
        session: &mut Session,
        sequence: &Sequence,
        target: Option<&Target>,
        error: &mut Option<String>,
    ) {
        self.recording = false;
        let recorded = std::mem::take(&mut self.recorded);
        let Some(target) = target.filter(|_| recorded.len() >= 2) else { return };
        let mut props = target.props.clone();
        // Half a percent of the frame is the smallest wobble worth a keyframe.
        let keys = |pick: fn(&(i64, f32, f32)) -> f32| {
            let mut keys: Vec<Key> = recorded.iter().map(|sample| Key::new(sample.0, pick(sample))).collect();
            keys.dedup_by_key(|k| k.frame);
            props::simplify(&keys, 0.005)
        };
        props.x = Param { value: recorded[0].1, keys: keys(|s| s.1) };
        props.y = Param { value: recorded[0].2, keys: keys(|s| s.2) };
        let group = self.group.take();
        commit(session, sequence, target, props, group, error);
    }
}

/// One row: name, a drag field showing the value at the playhead, and a
/// mark if the value has keyframes. Returns whether it was changed.
fn value_row(
    ui: &mut egui::Ui,
    name: &str,
    param: &mut Param,
    at: i64,
    range: std::ops::RangeInclusive<f32>,
    speed: f32,
    suffix: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_sized([84.0, 16.0], egui::Label::new(RichText::new(name).color(theme::DIM)).halign(egui::Align::LEFT));
        let mut v = param.at(at);
        if ui.add(egui::DragValue::new(&mut v).speed(speed).range(range).suffix(suffix).max_decimals(3)).changed() {
            param.set(at, v);
            changed = true;
        }
        if param.is_animated() {
            let on_key = param.keys.iter().any(|k| k.frame == at);
            ui.label(RichText::new(if on_key { "key" } else { "animated" }).size(10.0).color(theme::ACCENT));
        }
    });
    changed
}

/// Every keyframe of the clip in detail (feedback F6.3): when it is, what
/// it fixes and at which values, and how the values travel on from it.
/// A click on its time moves the playhead there; the values can be
/// changed in place. `start` is the clip's first sequence frame. Returns
/// whether anything changed.
fn keyframe_list(
    ui: &mut egui::Ui,
    props: &mut ClipProps,
    at: i64,
    start: i64,
    rate: sg_core::Rate,
    seek: &mut Option<i64>,
) -> bool {
    /// More than this are a recorded movement: listed up to here.
    const SHOWN: usize = 80;
    let ClipProps { x, y, scale, rotation, opacity, ramp, gain_db, .. } = props;
    let mut params: [(&str, &mut Param, std::ops::RangeInclusive<f32>, f32); 7] = [
        ("X", x, -2.0..=2.0, 0.002),
        ("Y", y, -2.0..=2.0, 0.002),
        ("Scale", scale, 0.01..=10.0, 0.005),
        ("Rotation", rotation, -360.0..=360.0, 0.5),
        ("Opacity", opacity, 0.0..=1.0, 0.005),
        ("Speed ramp", ramp, 0.05..=8.0, 0.005),
        ("Volume dB", gain_db, -60.0..=24.0, 0.1),
    ];
    let frames: std::collections::BTreeSet<i64> =
        params.iter().flat_map(|(_, param, _, _)| param.keys.iter().map(|k| k.frame)).collect();
    if frames.is_empty() {
        ui.label(
            RichText::new(
                "None yet. A keyframe fixes the clip's values at the playhead (F, or the Keyframe button). \
                 Move the playhead, change a value, set another: the clip moves from one to the other.",
            )
            .color(theme::DIM),
        );
        return false;
    }
    let mut changed = false;
    let last = frames.last().copied();
    for frame in frames.iter().copied().take(SHOWN) {
        ui.push_id(("keyframe", frame), |ui| {
            let here = frame == at;
            let fill = if here { theme::RAISED } else { egui::Color32::TRANSPARENT };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(theme::RADIUS)
                .inner_margin(egui::Margin::symmetric(4, 3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let time = crate::stage::timecode(start + frame, rate);
                        let text =
                            RichText::new(time).monospace().color(if here { theme::ACCENT } else { theme::TEXT });
                        if ui
                            .add(egui::Button::new(text).small())
                            .on_hover_text("Move the playhead to this keyframe")
                            .clicked()
                        {
                            *seek = Some(start + frame);
                        }
                        let now = params
                            .iter()
                            .find_map(|(_, p, _, _)| p.keys.iter().find(|k| k.frame == frame))
                            .map(|k| k.ease);
                        // (After the last keyframe nothing travels any more.)
                        for ease in sg_core::Ease::ALL.into_iter().filter(|_| Some(frame) != last) {
                            let label = RichText::new(ease.label()).size(10.5);
                            let chosen = ui.selectable_label(now == Some(ease), label);
                            if chosen.on_hover_text("How the values travel from this keyframe to the next").clicked() {
                                for (_, param, _, _) in &mut params {
                                    for key in param.keys.iter_mut().filter(|k| k.frame == frame) {
                                        changed |= key.ease != ease;
                                        key.ease = ease;
                                    }
                                }
                            }
                        }
                        if ui.add(egui::Button::new("x").small()).on_hover_text("Remove this keyframe").clicked() {
                            for (_, param, _, _) in &mut params {
                                changed |= param.remove_key(frame);
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for (name, param, range, speed) in &mut params {
                            let Some(key) = param.keys.iter_mut().find(|k| k.frame == frame) else { continue };
                            ui.label(RichText::new(*name).size(10.5).color(theme::DIM));
                            let value =
                                egui::DragValue::new(&mut key.value).speed(*speed).range(range.clone()).max_decimals(3);
                            changed |= ui.add(value).changed();
                            ui.add_space(4.0);
                        }
                    });
                });
        });
    }
    if frames.len() > SHOWN {
        ui.label(RichText::new(format!("and {} more", frames.len() - SHOWN)).color(theme::DIM));
    }
    changed
}

/// The transition on the cut at the start of the clip. Returns whether
/// anything changed.
fn transition_rows(ui: &mut egui::Ui, transition: &mut Option<sg_core::Transition>, target: &Target) -> bool {
    use sg_core::{Transition, TransitionKind};
    let before = *transition;
    // A transition needs a clip to come from, ending where this one starts.
    let follows = target.clip.start > 0 && target.track.clips.at(target.clip.start - 1).is_some();
    ui.label(RichText::new("TRANSITION INTO THIS CLIP").size(11.0).strong().color(theme::DIM));
    let Some(t) = transition else {
        if !follows {
            ui.label(RichText::new("There is no clip directly before this one to come from.").color(theme::DIM));
        }
        ui.horizontal_wrapped(|ui| {
            for (kind, name) in [
                (TransitionKind::Whip, "Add whip-pan (B)"),
                (TransitionKind::Black, "Add black (Shift+B)"),
                (TransitionKind::Slide, "Add slide"),
                (TransitionKind::Zoom, "Add zoom"),
            ] {
                if ui.add_enabled(follows, egui::Button::new(name)).clicked() {
                    *transition = Some(Transition::of(kind));
                }
            }
        });
        return *transition != before;
    };
    let label = |ui: &mut egui::Ui, text: &str| {
        ui.add_sized([84.0, 16.0], egui::Label::new(RichText::new(text).color(theme::DIM)).halign(egui::Align::LEFT));
    };
    ui.horizontal(|ui| {
        label(ui, "Kind");
        for (kind, name) in [
            (TransitionKind::Whip, "Whip"),
            (TransitionKind::Slide, "Slide"),
            (TransitionKind::Zoom, "Zoom"),
            (TransitionKind::Black, "Black"),
        ] {
            if ui.selectable_label(t.kind == kind, name).clicked() && t.kind != kind {
                // The numbers that suit the kind; the direction stays.
                *t = Transition { angle: t.angle, ..Transition::of(kind) };
            }
        }
    });
    ui.horizontal(|ui| {
        label(ui, "Length");
        let mut frames = t.frames as i32;
        let longest = Transition::LONGEST as i32;
        let field = egui::DragValue::new(&mut frames).range(2..=longest).speed(0.1).suffix(" frames");
        if ui.add(field).on_hover_text("Half of it lies before the cut, half after. Even numbers, up to 60.").changed()
        {
            t.frames = ((frames.clamp(2, longest) + 1) / 2 * 2) as u8;
        }
    });
    if t.kind == TransitionKind::Black {
        // Nothing moves: black for the length above, hard or with a fade at its ends.
        ui.horizontal(|ui| {
            label(ui, "Fade");
            let mut percent = t.amount.clamp(0.0, 1.0) * 100.0;
            let field = egui::DragValue::new(&mut percent).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %");
            if ui
                .add(field)
                .on_hover_text(
                    "0: hard. The picture cuts to black and black cuts to the next picture. \
                     Above 0 that share of the time fades in and out of the black; 100 is a dip with no full black but at the cut.",
                )
                .changed()
            {
                t.amount = percent / 100.0;
            }
            for (name, share) in [("Hard", 0.0f32), ("Half", 0.5), ("Dip", 1.0)] {
                if ui.selectable_label((t.amount - share).abs() < 0.005, name).clicked() {
                    t.amount = share;
                }
            }
        });
        if t.amount > 0.0 {
            ui.horizontal(|ui| {
                label(ui, "Darkens");
                ui.add(egui::DragValue::new(&mut t.ease).range(1.0..=6.0).speed(0.02).max_decimals(2))
                    .on_hover_text("1 fades evenly; larger goes dark sooner and comes back later");
            });
        }
        if ui.button("Remove transition").clicked() {
            *transition = None;
        }
        return *transition != before;
    }
    if t.kind != TransitionKind::Zoom {
        ui.horizontal(|ui| {
            label(ui, "Moves");
            for (angle, name) in [(0.0f32, "left"), (180.0, "right"), (90.0, "up"), (270.0, "down")] {
                if ui.selectable_label((t.angle.rem_euclid(360.0) - angle).abs() < 0.5, name).clicked() {
                    t.angle = angle;
                }
            }
            ui.add(egui::DragValue::new(&mut t.angle).range(0.0..=360.0).speed(1.0).suffix(" deg").max_decimals(0));
        });
    }
    ui.horizontal(|ui| {
        label(ui, "Blur");
        ui.add(egui::DragValue::new(&mut t.blur).range(0.0..=2.0).speed(0.01).max_decimals(2))
            .on_hover_text("Share of each frame the shutter is open: 0 none, 1 as a camera, 2 stronger");
    });
    if t.kind != TransitionKind::Slide {
        ui.horizontal(|ui| {
            label(ui, if t.kind == TransitionKind::Zoom { "Zoom" } else { "Travel" });
            ui.add(egui::DragValue::new(&mut t.amount).range(0.0..=4.0).speed(0.01).max_decimals(2)).on_hover_text(
                "Whip: frame widths travelled on each side of the cut. Zoom: how much the picture grows.",
            );
        });
    }
    ui.horizontal(|ui| {
        label(ui, "Speed-up");
        ui.add(egui::DragValue::new(&mut t.ease).range(1.0..=6.0).speed(0.02).max_decimals(2))
            .on_hover_text("1 moves evenly; larger starts gently and is fastest at the cut");
    });
    if ui.button("Remove transition").clicked() {
        *transition = None;
    }
    *transition != before
}
