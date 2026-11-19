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
        ui.label(RichText::new("CLIP").size(11.0).strong().color(theme::DIM));
        let mut props = target.props.clone();
        let at = target.at;
        let video = target.track.kind == TrackKind::Video;
        let fps = sequence.rate.num() as f32 / sequence.rate.den() as f32;
        let mut changed = false;

        if let Some(title) = &mut props.title {
            ui.add(egui::TextEdit::multiline(&mut title.text).desired_rows(2).desired_width(f32::INFINITY))
                .changed()
                .then(|| changed = true);
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
        }
        if video {
            changed |= value_row(ui, "Position X", &mut props.x, at, -2.0..=2.0, 0.002, "");
            changed |= value_row(ui, "Position Y", &mut props.y, at, -2.0..=2.0, 0.002, "");
            changed |= value_row(ui, "Scale", &mut props.scale, at, 0.01..=10.0, 0.005, "");
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
        ui.horizontal(|ui| {
            if video && props.title.is_none() {
                // A factor on the speed that keyframes can change along the clip.
                // The sound of the take is not ramped with it.
                changed |= value_row(ui, "Speed ramp", &mut props.ramp, at, 0.05..=8.0, 0.005, " x");
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
        if video {
            changed |= transition_rows(ui, &mut props.transition, target);
        }
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
            let mut keys: Vec<Key> =
                recorded.iter().map(|sample| Key { frame: sample.0, value: pick(sample) }).collect();
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

/// The transition on the cut at the start of the clip. Returns whether
/// anything changed.
fn transition_rows(ui: &mut egui::Ui, transition: &mut Option<sg_core::Transition>, target: &Target) -> bool {
    use sg_core::{Transition, TransitionKind};
    let before = *transition;
    // A transition needs a clip to come from, ending where this one starts.
    let follows = target.clip.start > 0 && target.track.clips.at(target.clip.start - 1).is_some();
    ui.add_space(6.0);
    ui.label(RichText::new("TRANSITION INTO THIS CLIP").size(11.0).strong().color(theme::DIM));
    let Some(t) = transition else {
        let button = ui.add_enabled(follows, egui::Button::new("Add whip-pan (B)"));
        if button.on_disabled_hover_text("There is no clip directly before this one.").clicked() {
            *transition = Some(Transition::default());
        }
        return *transition != before;
    };
    let label = |ui: &mut egui::Ui, text: &str| {
        ui.add_sized([84.0, 16.0], egui::Label::new(RichText::new(text).color(theme::DIM)).halign(egui::Align::LEFT));
    };
    ui.horizontal(|ui| {
        label(ui, "Kind");
        for (kind, name) in
            [(TransitionKind::Whip, "Whip"), (TransitionKind::Slide, "Slide"), (TransitionKind::Zoom, "Zoom")]
        {
            if ui.selectable_label(t.kind == kind, name).clicked() && t.kind != kind {
                // The numbers that suit the kind; the direction stays.
                *t = Transition { angle: t.angle, ..Transition::of(kind) };
            }
        }
    });
    ui.horizontal(|ui| {
        label(ui, "Length");
        let mut frames = t.frames as i32;
        if ui.add(egui::DragValue::new(&mut frames).range(2..=16).speed(0.1).suffix(" frames")).changed() {
            t.frames = ((frames.clamp(2, 16) + 1) / 2 * 2) as u8;
        }
    });
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
