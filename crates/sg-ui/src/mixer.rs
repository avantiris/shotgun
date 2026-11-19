//! The mixer: one strip per audio track (level meter, fader, pan, mute,
//! solo, low cut, noise reduction) and the master.

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::{Op, Sequence, TrackKind, TrackSettings};

use crate::session::Session;
use crate::stage::ViewerState;
use crate::theme;

const STRIP_W: f32 = 82.0;
const FADER_H: f32 = 150.0;
/// The range of the meters and faders in decibels.
const FLOOR_DB: f32 = -60.0;
const TOP_DB: f32 = 12.0;

#[derive(Default)]
pub struct Mixer {
    pub open: bool,
    /// Meter readings as shown: they fall back slowly so that peaks can be read.
    levels: Vec<f32>,
    master: f32,
    limiting: f32,
    /// Undo group of the drag in progress: one drag of a fader is one step.
    group: Option<u64>,
}

fn to_db(linear: f32) -> f32 {
    if linear <= 0.000_001 { -120.0 } else { 20.0 * linear.log10() }
}

/// A vertical level bar: green, yellow from -12 dB, red above full scale.
fn meter(ui: &mut egui::Ui, level: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::BG);
    let y_of = |db: f32| rect.bottom() - (db.clamp(FLOOR_DB, TOP_DB) - FLOOR_DB) / (TOP_DB - FLOOR_DB) * rect.height();
    let top = y_of(to_db(level));
    for (from, to, colour) in
        [(FLOOR_DB, -12.0, theme::AUDIO), (-12.0, 0.0, Color32::from_rgb(222, 196, 82)), (0.0, TOP_DB, theme::ERROR)]
    {
        let (y0, y1) = (y_of(to).max(top), y_of(from));
        if y1 > y0 {
            painter.rect_filled(Rect::from_min_max(pos2(rect.left(), y0), pos2(rect.right(), y1)), 0.0, colour);
        }
    }
    // Full scale: above this line the output clips.
    painter.line_segment([pos2(rect.left(), y_of(0.0)), pos2(rect.right(), y_of(0.0))], Stroke::new(1.0, theme::LINE));
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
}

/// Meter and fader side by side with the value below. Returns the fader
/// value if it changed; a double click puts it back to 0 dB.
fn fader(ui: &mut egui::Ui, level: f32, db: f32) -> Option<f32> {
    let mut value = db;
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        meter(ui, level, FADER_H);
        ui.spacing_mut().slider_width = FADER_H;
        let slider = egui::Slider::new(&mut value, FLOOR_DB..=TOP_DB).vertical().show_value(false).step_by(0.1);
        if ui.add(slider).on_hover_text("Level. Double-click for 0 dB.").double_clicked() {
            value = 0.0;
        }
    });
    let text = if value <= FLOOR_DB { "off".to_owned() } else { format!("{value:+.1} dB") };
    ui.label(RichText::new(text).size(11.0).color(theme::DIM));
    (value != db).then_some(value)
}

fn toggle(ui: &mut egui::Ui, on: bool, label: &str, colour: Color32, tip: &str) -> bool {
    let text = RichText::new(label).strong().color(if on { theme::BG } else { theme::DIM });
    let button = egui::Button::new(text).fill(if on { colour } else { theme::RAISED }).min_size(vec2(30.0, 22.0));
    ui.add(button).on_hover_text(tip).clicked()
}

impl Mixer {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: &mut Session,
        sequence: &Sequence,
        view: &ViewerState,
        error: &mut Option<String>,
    ) {
        let tracks: Vec<_> = sequence.tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect();
        // New readings push the bars up at once; they fall back over a few frames.
        self.levels.resize(tracks.len(), 0.0);
        for (index, shown) in self.levels.iter_mut().enumerate() {
            let fresh = view.meters.get(index).copied().unwrap_or(0.0);
            *shown = if view.playing { fresh.max(*shown * 0.86) } else { 0.0 };
        }
        self.master = if view.playing { view.master_meter.max(self.master * 0.86) } else { 0.0 };
        if !ui.input(|i| i.pointer.any_down()) {
            self.group = None;
        }
        let mut ops: Vec<Op> = Vec::new();
        ui.label(RichText::new("MIXER").size(11.0).strong().color(theme::DIM));
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (index, track) in tracks.iter().enumerate() {
                    let mut mix = track.mix;
                    ui.allocate_ui_with_layout(vec2(STRIP_W, 0.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.set_width(STRIP_W);
                        ui.label(RichText::new(&track.name).strong().color(theme::AUDIO));
                        if let Some(db) = fader(ui, self.levels[index], mix.gain_db) {
                            mix.gain_db = db;
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(11.0);
                            if toggle(ui, mix.mute, "M", theme::ERROR, "Mute") {
                                mix.mute = !mix.mute;
                            }
                            if toggle(ui, mix.solo, "S", theme::ACCENT, "Solo: only soloed tracks are heard") {
                                mix.solo = !mix.solo;
                            }
                        });
                        let field = |ui: &mut egui::Ui, label: &str, widget: egui::DragValue<'_>, tip: &str| {
                            ui.label(RichText::new(label).size(10.5).color(theme::DIM));
                            ui.add_sized([70.0, 20.0], widget).on_hover_text(tip);
                        };
                        field(
                            ui,
                            "Pan",
                            egui::DragValue::new(&mut mix.pan).range(-1.0..=1.0).speed(0.01).fixed_decimals(2),
                            "-1 is left, 1 is right",
                        );
                        field(
                            ui,
                            "Low cut",
                            egui::DragValue::new(&mut mix.low_cut_hz)
                                .range(0.0..=1000.0)
                                .speed(1.0)
                                .fixed_decimals(0)
                                .suffix(" Hz"),
                            "Removes rumble below this frequency. 0 is off; 80 to 120 Hz suits a voice.",
                        );
                        let mut percent = mix.denoise * 100.0;
                        field(
                            ui,
                            "Noise reduction",
                            egui::DragValue::new(&mut percent)
                                .range(0.0..=100.0)
                                .speed(1.0)
                                .fixed_decimals(0)
                                .suffix(" %"),
                            "Turns down hiss, hum and room noise behind a voice. Made for speech: music loses detail. \
                             Heard about half a second after a change.",
                        );
                        mix.denoise = percent / 100.0;
                        ui.add_space(4.0);
                        for (label, value, tip) in [
                            ("EQ low", &mut mix.eq_low_db, "Shelf below 120 Hz"),
                            ("EQ mid", &mut mix.eq_mid_db, "Bell one octave wide at the frequency below"),
                            ("EQ high", &mut mix.eq_high_db, "Shelf above 8 kHz"),
                        ] {
                            field(ui, label, egui::DragValue::new(value).range(-18.0..=18.0).speed(0.1).fixed_decimals(1).suffix(" dB"), tip);
                        }
                        field(
                            ui,
                            "Mid at",
                            egui::DragValue::new(&mut mix.eq_mid_hz).range(100.0..=10_000.0).speed(10.0).fixed_decimals(0).suffix(" Hz"),
                            "Centre of the mid band",
                        );
                        ui.add_space(4.0);
                        field(
                            ui,
                            "Compress above",
                            egui::DragValue::new(&mut mix.comp_threshold_db).range(-60.0..=0.0).speed(0.2).fixed_decimals(1).suffix(" dB"),
                            "Parts louder than this are turned down. 0 is off; -18 dB evens out a voice.",
                        );
                        field(
                            ui,
                            "Ratio",
                            egui::DragValue::new(&mut mix.comp_ratio).range(1.0..=20.0).speed(0.05).fixed_decimals(1).suffix(" : 1"),
                            "4 lets one decibel through for every four above the threshold",
                        );
                    });
                    ui.separator();
                    if mix != track.mix {
                        ops.push(Op::SetTrackMix { seq: sequence.id, track: track.id, mix });
                    }
                }
                ui.allocate_ui_with_layout(vec2(STRIP_W, 0.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.set_width(STRIP_W);
                    ui.label(RichText::new("Master").strong());
                    if let Some(db) = fader(ui, self.master, sequence.master_db) {
                        ops.push(Op::SetMasterGain { id: sequence.id, db });
                    }
                    // What the limiter took off, held a little so that it can be read.
                    self.limiting = if view.playing { view.limiting_db.max(self.limiting * 0.9) } else { 0.0 };
                    if self.limiting > 0.1 {
                        ui.label(RichText::new(format!("limit {:.1} dB", self.limiting)).size(11.0).color(theme::ACCENT))
                            .on_hover_text("The mix was louder than full scale and was turned down by this much. Lower the faders for a cleaner sound.");
                    }
                    if to_db(self.master) > 0.0 {
                        ui.label(RichText::new("clipping").size(11.0).color(theme::ERROR));
                    }
                });
            });
        });
        if tracks.is_empty() {
            ui.label(RichText::new("No audio tracks yet.").color(theme::DIM));
        }
        for op in ops {
            let group =
                ui.input(|i| i.pointer.any_down()).then(|| *self.group.get_or_insert_with(|| session.new_group()));
            if let Err(e) = session.apply(op, group) {
                *error = Some(e.to_string());
            }
        }
    }
}

/// The mixer settings a script step names: `gain=-6`, `pan=0.3`, `mute=1`,
/// `solo=1`, `lowcut=100`, `denoise=0.8`.
pub fn set_field(mix: &mut TrackSettings, name: &str, value: f32) -> bool {
    match name {
        "gain" => mix.gain_db = value,
        "pan" => mix.pan = value,
        "mute" => mix.mute = value != 0.0,
        "solo" => mix.solo = value != 0.0,
        "lowcut" => mix.low_cut_hz = value,
        "denoise" => mix.denoise = value,
        "eqlow" => mix.eq_low_db = value,
        "eqmid" => mix.eq_mid_db = value,
        "eqmidhz" => mix.eq_mid_hz = value,
        "eqhigh" => mix.eq_high_db = value,
        "threshold" => mix.comp_threshold_db = value,
        "ratio" => mix.comp_ratio = value,
        "hidden" => mix.hidden = value != 0.0,
        _ => return false,
    }
    true
}
