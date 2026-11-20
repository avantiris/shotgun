//! The mixer: one strip per audio track (level meter, fader, pan, mute,
//! solo, low cut, noise reduction) and the master.

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::{Op, Sequence, TrackKind, TrackSettings};

use crate::session::Session;
use crate::stage::ViewerState;
use crate::theme;

const STRIP_W: f32 = 104.0;
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
    /// A track whose voice should be listened to and set up, for a style
    /// or (`None`) the one that fits best; taken by the shell.
    pub voice_request: Option<(sg_core::TrackId, Option<sg_media::voice::VoiceStyle>)>,
    /// The track being listened to right now.
    pub listening: Option<sg_core::TrackId>,
    /// The plugins found on the machine (name, path), once they were looked for.
    pub available: Option<Vec<(String, String)>>,
    /// The plugins should be looked for; taken by the shell.
    pub scan_request: bool,
    /// The plugins popover should open (to install some); taken by the shell.
    pub popover_request: bool,
    /// The window of this track's plugin should be shown; taken by the shell.
    pub editor_request: Option<sg_core::TrackId>,
}

/// Settings to start from (feedback F4.3): low cut, noise reduction,
/// equaliser and compressor for a kind of sound. Fader, pan, mute and solo
/// are not part of a preset. The values are common starting points of
/// sound engineers, to be adjusted by ear.
pub struct Preset {
    pub name: &'static str,
    pub tip: &'static str,
    low_cut_hz: f32,
    denoise: f32,
    eq_low_db: f32,
    eq_mid_db: f32,
    eq_mid_hz: f32,
    eq_high_db: f32,
    comp_threshold_db: f32,
    comp_ratio: f32,
}

const fn preset(
    name: &'static str,
    tip: &'static str,
    low_cut_hz: f32,
    denoise: f32,
    eq: [f32; 4],
    comp: [f32; 2],
) -> Preset {
    Preset {
        name,
        tip,
        low_cut_hz,
        denoise,
        eq_low_db: eq[0],
        eq_mid_db: eq[1],
        eq_mid_hz: eq[2],
        eq_high_db: eq[3],
        comp_threshold_db: comp[0],
        comp_ratio: comp[1],
    }
}

pub const PRESETS: &[Preset] = &[
    preset("Flat", "Everything off: the track as it was recorded.", 0.0, 0.0, [0.0, 0.0, 1000.0, 0.0], [0.0, 3.0]),
    preset(
        "Voice",
        "Speech recorded decently: rumble off below 80 Hz, a little presence at 3 kHz and air, levelled gently (3:1 above -18 dB).",
        80.0,
        0.0,
        [0.0, 2.0, 3000.0, 2.0],
        [-18.0, 3.0],
    ),
    preset(
        "Voice, noisy room",
        "Speech with hiss, hum or room noise behind it: noise reduction at 60 %, low cut at 100 Hz, presence, levelled.",
        100.0,
        0.6,
        [-1.0, 2.5, 3000.0, 1.0],
        [-20.0, 3.0],
    ),
    preset(
        "Voice-over",
        "A voice close to the microphone, radio-like: warmth below, the boxy 350 Hz turned down, bright on top, levelled firmly (4:1 above -22 dB).",
        80.0,
        0.2,
        [1.5, -2.5, 350.0, 3.0],
        [-22.0, 4.0],
    ),
    preset(
        "Camera sound outdoors",
        "The camera's own microphone outside: wind and handling rumble off below 150 Hz, half the noise reduction, levelled.",
        150.0,
        0.5,
        [-2.0, 1.5, 2500.0, 0.0],
        [-16.0, 3.0],
    ),
    preset(
        "Music",
        "A finished song: almost untouched. Only what is below 30 Hz goes, with a light touch of low and high and a soft compressor for peaks.",
        30.0,
        0.0,
        [1.0, 0.0, 1000.0, 1.0],
        [-10.0, 2.0],
    ),
    preset(
        "Music under a voice",
        "Music that plays behind speech: a dip at 2.5 kHz leaves room for the voice. Also pull its fader down to about -14 dB.",
        40.0,
        0.0,
        [0.0, -4.0, 2500.0, -1.0],
        [-18.0, 2.5],
    ),
    preset(
        "Bass and kick",
        "More weight below, the muddy 300 Hz turned down, held firmly (4:1 above -14 dB).",
        30.0,
        0.0,
        [3.0, -3.0, 300.0, 0.0],
        [-14.0, 4.0],
    ),
    preset(
        "Bright and clear",
        "For a dull recording: less of the boxy 400 Hz, more above 8 kHz. No compressor.",
        60.0,
        0.0,
        [0.0, -2.0, 400.0, 4.0],
        [0.0, 3.0],
    ),
    preset(
        "Telephone",
        "An effect: only the middle is left, squeezed hard, like a voice through a phone.",
        400.0,
        0.0,
        [-12.0, 6.0, 1800.0, -14.0],
        [-20.0, 6.0],
    ),
];

impl Preset {
    /// The settings of a track with this preset put on.
    pub fn on(&self, mix: TrackSettings) -> TrackSettings {
        TrackSettings {
            low_cut_hz: self.low_cut_hz,
            denoise: self.denoise,
            eq_low_db: self.eq_low_db,
            eq_mid_db: self.eq_mid_db,
            eq_mid_hz: self.eq_mid_hz,
            eq_high_db: self.eq_high_db,
            comp_threshold_db: self.comp_threshold_db,
            comp_ratio: self.comp_ratio,
            ..mix
        }
    }

    /// The preset a track is set to exactly, if any.
    pub fn of(mix: &TrackSettings) -> Option<&'static Preset> {
        PRESETS.iter().find(|preset| preset.on(*mix) == *mix)
    }
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
        ui.add_space(25.0);
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
                        // What the settings below are at, by name: a preset or the user's own.
                        let current = Preset::of(&mix).map_or("Own", |preset| preset.name);
                        ui.scope(|ui| {
                            ui.spacing_mut().button_padding = vec2(5.0, 3.0);
                            ui.label(RichText::new("Preset").size(10.5).color(theme::DIM));
                            let text = RichText::new(current).size(10.5);
                            let button = egui::Button::new(text).truncate().min_size(vec2(STRIP_W - 8.0, 20.0));
                            let (response, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                                for preset in PRESETS {
                                    let chosen = ui.selectable_label(preset.name == current, preset.name);
                                    if chosen.on_hover_text(preset.tip).clicked() {
                                        mix = preset.on(mix);
                                        ui.close();
                                    }
                                }
                            });
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Preset: {current}"))
                            });
                            response.on_hover_text(
                                "Starting points for low cut, noise reduction, equaliser and compressor. \
                                 Fader, pan, mute and solo stay as they are.",
                            );
                        });
                        // The vocal chain's ear (09-daw.md): listen to what is on the track and set the strip.
                        ui.scope(|ui| {
                            ui.spacing_mut().button_padding = vec2(5.0, 3.0);
                            let busy = self.listening == Some(track.id);
                            let text = RichText::new(if busy { "Listening..." } else { "Voice" }).size(10.5);
                            let button = egui::Button::new(text).min_size(vec2(STRIP_W - 8.0, 20.0));
                            let (response, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                                let auto = ui.button("Listen and set up").on_hover_text(
                                    "Measures the voice on this track (level, noise between phrases, pitch, tone) and sets \
                                     fader, low cut, noise reduction, equaliser and compressor for the style that fits.",
                                );
                                if auto.clicked() {
                                    self.voice_request = Some((track.id, None));
                                    ui.close();
                                }
                                ui.separator();
                                for style in sg_media::voice::VoiceStyle::ALL {
                                    if ui.button(format!("As: {}", style.name())).clicked() {
                                        self.voice_request = Some((track.id, Some(style)));
                                        ui.close();
                                    }
                                }
                            });
                            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Voice"));
                            response.on_hover_text("Set this strip up for a recorded voice by listening to it");
                        });
                        // The track's plugin: an effect after the stages of this strip, or an instrument.
                        ui.label(RichText::new("Plugin").size(10.5).color(theme::DIM));
                        let mut plugin: Option<Option<sg_core::PluginRef>> = None;
                        ui.scope(|ui| {
                            ui.spacing_mut().button_padding = vec2(5.0, 3.0);
                            match &track.plugin {
                                Some(on) => {
                                    ui.label(RichText::new(&on.name).size(11.0));
                                    ui.horizontal(|ui| {
                                        let window = ui.button(RichText::new("Window").size(10.5));
                                        if window.on_hover_text("Shows the plugin's own window").clicked() {
                                            self.editor_request = Some(track.id);
                                        }
                                        let off = ui.button(RichText::new("Off").size(10.5));
                                        if off.on_hover_text("Takes the plugin off this track").clicked() {
                                            plugin = Some(None);
                                        }
                                    });
                                }
                                None => {
                                    let text = RichText::new("Add plugin").size(10.5);
                                    let button = egui::Button::new(text).min_size(vec2(STRIP_W - 8.0, 20.0));
                                    let (response, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                                        match &self.available {
                                            None => {
                                                self.scan_request = true;
                                                ui.label(RichText::new("Looking for plugins...").color(theme::DIM));
                                            }
                                            Some(found) => {
                                                for (name, path) in found {
                                                    if ui.button(name).on_hover_text(path).clicked() {
                                                        plugin = Some(Some(sg_core::PluginRef {
                                                            name: name.clone(),
                                                            path: path.clone(),
                                                        }));
                                                        ui.close();
                                                    }
                                                }
                                                if found.is_empty() {
                                                    ui.label(RichText::new("No plugins were found.").color(theme::DIM));
                                                }
                                                ui.separator();
                                            }
                                        }
                                        if ui.button("Install Native Instruments, find plugins...").clicked() {
                                            self.popover_request = true;
                                            ui.close();
                                        }
                                    });
                                    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Add plugin"));
                                    response.on_hover_text(
                                        "An effect the track's sound runs through, or an instrument that plays the notes \
                                         of its note clips (right click on the track in the timeline to add one).",
                                    );
                                }
                            }
                        });
                        if let Some(plugin) = plugin {
                            ops.push(Op::SetTrackPlugin { seq: sequence.id, track: track.id, plugin });
                        }
                        if let Some(db) = fader(ui, self.levels[index], mix.gain_db) {
                            mix.gain_db = db;
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(22.0);
                            if toggle(ui, mix.mute, "M", theme::ERROR, "Mute") {
                                mix.mute = !mix.mute;
                            }
                            if toggle(ui, mix.solo, "S", theme::ACCENT, "Solo: only soloed tracks are heard") {
                                mix.solo = !mix.solo;
                            }
                        });
                        let field = |ui: &mut egui::Ui, label: &str, widget: egui::DragValue<'_>, tip: &str| {
                            ui.label(RichText::new(label).size(10.5).color(theme::DIM));
                            ui.add_sized([88.0, 20.0], widget).on_hover_text(tip);
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
                        // The voice chain's own stages: on voice tracks only, so that other strips stay short.
                        if mix.voice {
                            let mut sharp = mix.deess * 100.0;
                            field(
                                ui,
                                "De-esser",
                                egui::DragValue::new(&mut sharp).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %"),
                                "Turns down the S sounds of a voice, by up to 12 dB at 100 %. It finds them itself, \
                                 whatever the level of the recording; vowels keep their brightness.",
                            );
                            mix.deess = sharp / 100.0;
                            // Pitch correction: how much, the key, and how hard.
                            ui.add_space(4.0);
                            let mut tune = mix.tune * 100.0;
                            field(
                                ui,
                                "Tune",
                                egui::DragValue::new(&mut tune).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %"),
                                "Pitch correction: moves what is sung onto the notes of the key below. 0 is off, 100 all \
                                 the way. \"Voice\" proposes the key by listening.",
                            );
                            mix.tune = tune / 100.0;
                            const NOTES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
                            const SCALES: [&str; 3] = ["all notes", "major", "minor"];
                            ui.label(RichText::new("Key").size(10.5).color(theme::DIM));
                            ui.horizontal(|ui| {
                                ui.spacing_mut().button_padding = vec2(4.0, 2.0);
                                ui.add_space(4.0);
                                egui::ComboBox::from_id_salt(("tune key", index))
                                    .width(34.0)
                                    .selected_text(NOTES[mix.tune_key as usize % 12])
                                    .show_ui(ui, |ui| {
                                        for (n, name) in NOTES.iter().enumerate() {
                                            ui.selectable_value(&mut mix.tune_key, n as u8, *name);
                                        }
                                    });
                                egui::ComboBox::from_id_salt(("tune scale", index))
                                    .width(44.0)
                                    .selected_text(SCALES[mix.tune_scale.min(2) as usize])
                                    .show_ui(ui, |ui| {
                                        for (n, name) in SCALES.iter().enumerate() {
                                            ui.selectable_value(&mut mix.tune_scale, n as u8, *name);
                                        }
                                    });
                            });
                            let mut speed = mix.tune_speed * 100.0;
                            field(
                                ui,
                                "Tune glide",
                                egui::DragValue::new(&mut speed).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %"),
                                "How a note is pulled into place: 0 at once (the hard, robotic sound of rap hooks), \
                                 100 gently (natural singing).",
                            );
                            mix.tune_speed = speed / 100.0;
                            ui.add_space(4.0);
                            let mut level = mix.level * 100.0;
                            field(
                                ui,
                                "Leveller",
                                egui::DragValue::new(&mut level).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %"),
                                "A hand on the fader: loud phrases are pulled down and quiet ones up, towards the track's \
                                 own average. It needs no threshold.",
                            );
                            mix.level = level / 100.0;
                            let mut drive = mix.drive * 100.0;
                            field(
                                ui,
                                "Saturation",
                                egui::DragValue::new(&mut drive).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %"),
                                "Rounds off the peaks: denser and a little rougher, as a voice through good hardware.",
                            );
                            mix.drive = drive / 100.0;
                        }
                        ui.add_space(4.0);
                        ui.checkbox(&mut mix.voice, RichText::new("Voice track").size(10.5)).on_hover_text(
                            "While a voice track sounds, the tracks that duck (the field below, on their strips) go down.",
                        );
                        if !mix.voice {
                            field(
                                ui,
                                "Duck under voice",
                                egui::DragValue::new(&mut mix.duck_db).range(0.0..=24.0).speed(0.1).fixed_decimals(1).suffix(" dB"),
                                "How far this track goes down while a voice track sounds, so that the voice has room. \
                                 0 is off; 2 to 4 dB is usual for the beat.",
                            );
                        }
                        ui.add_space(4.0);
                        for (name, value, tip) in [
                            ("Reverb", &mut mix.reverb, "How much of this track goes into the room all tracks share. \
                              10 to 20 % sets a voice into a space without washing it out."),
                            ("Echo", &mut mix.echo, "How much of this track is repeated in time with the tempo \
                              (every beat), each repeat quieter and duller."),
                        ] {
                            let mut percent = *value * 100.0;
                            field(
                                ui,
                                name,
                                egui::DragValue::new(&mut percent).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %"),
                                tip,
                            );
                            *value = percent / 100.0;
                        }
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
                    // How loud the mix is as streaming services measure it.
                    let loud = if view.playing && view.loudness > -70.0 {
                        format!("{:.1} LUFS", view.loudness)
                    } else {
                        "- LUFS".to_owned()
                    };
                    ui.label(RichText::new(loud).size(11.0).color(theme::DIM)).on_hover_text(
                        "Loudness of the last three seconds. Streaming services play songs at about -14 LUFS; \
                         loud rap masters sit around -9. The sound export can bring the whole song to either.",
                    );
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

/// A track's settings with what listening to its voice proposed put on.
/// Pan, mute and solo stay.
pub fn with_voice(mix: TrackSettings, set: &sg_media::voice::Suggestion) -> TrackSettings {
    TrackSettings {
        gain_db: set.gain_db,
        low_cut_hz: set.low_cut_hz,
        denoise: set.denoise,
        eq_low_db: set.eq_low_db,
        eq_mid_db: set.eq_mid_db,
        eq_mid_hz: set.eq_mid_hz,
        eq_high_db: set.eq_high_db,
        comp_threshold_db: set.comp_threshold_db,
        comp_ratio: set.comp_ratio,
        deess: set.deess,
        tune: set.tune,
        tune_key: set.tune_key,
        tune_scale: set.tune_scale,
        tune_speed: set.tune_speed,
        level: set.level,
        drive: set.drive,
        voice: true,
        duck_db: 0.0,
        reverb: set.reverb,
        echo: set.echo,
        ..mix
    }
}

/// The mixer settings a script step names: `gain=-6`, `pan=0.3`, `mute=1`,
/// `solo=1`, `lowcut=100`, `denoise=0.8`, `preset=1`.
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
        "deess" => mix.deess = value,
        "tune" => mix.tune = value,
        "tunekey" => mix.tune_key = value as u8,
        "tunescale" => mix.tune_scale = value as u8,
        "tunespeed" => mix.tune_speed = value,
        "level" => mix.level = value,
        "drive" => mix.drive = value,
        "voice" => mix.voice = value != 0.0,
        "duck" => mix.duck_db = value,
        "reverb" => mix.reverb = value,
        "echo" => mix.echo = value,
        "hidden" => mix.hidden = value != 0.0,
        // The n-th preset of the list (0 is Flat).
        "preset" => match PRESETS.get(value as usize) {
            Some(preset) => *mix = preset.on(*mix),
            None => return false,
        },
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_stay_inside_what_the_strip_can_show_and_leave_the_fader_alone() {
        let before = TrackSettings { gain_db: -7.0, pan: 0.3, mute: true, solo: true, ..TrackSettings::default() };
        for preset in PRESETS {
            let mix = preset.on(before);
            assert_eq!((mix.gain_db, mix.pan, mix.mute, mix.solo), (-7.0, 0.3, true, true), "{}", preset.name);
            assert!((0.0..=1000.0).contains(&mix.low_cut_hz), "{}", preset.name);
            assert!((0.0..=1.0).contains(&mix.denoise), "{}", preset.name);
            for db in [mix.eq_low_db, mix.eq_mid_db, mix.eq_high_db] {
                assert!((-18.0..=18.0).contains(&db), "{}", preset.name);
            }
            assert!((100.0..=10_000.0).contains(&mix.eq_mid_hz), "{}", preset.name);
            assert!((-60.0..=0.0).contains(&mix.comp_threshold_db), "{}", preset.name);
            assert!((1.0..=20.0).contains(&mix.comp_ratio), "{}", preset.name);
            // Each is told from the others by its values.
            assert_eq!(Preset::of(&mix).map(|p| p.name), Some(preset.name));
        }
        // A fresh track is flat; a track changed by hand is nobody's preset.
        assert_eq!(Preset::of(&TrackSettings::default()).map(|p| p.name), Some("Flat"));
        assert!(Preset::of(&TrackSettings { eq_low_db: 0.7, ..TrackSettings::default() }).is_none());
    }
}
