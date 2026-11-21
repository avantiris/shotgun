//! The picture of one stage of a track's chain at work (feedback F16.17):
//! the sound going into it and coming out of it, moving past the playhead,
//! with what the stage is set to drawn into it where that can be shown: the
//! compressor's threshold as a line to pull, the low cut and the
//! equaliser as a curve with handles.
//!
//! The levels come from the engine (`ViewerState::scope`): the peak after
//! every stage for each 5 ms of the timeline around what is heard.

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use sg_core::TrackSettings;

use crate::stage::ViewerState;
use crate::theme;

/// The points along the chain the engine reports the level at
/// (`sg_audio::SCOPE_TAPS`): the source, after each of the eight stages,
/// and what leaves for the fader.
pub const TAPS: usize = 10;

/// The sound by pitch: decibels per band, going into a stage and coming
/// out of it. The bands are evenly spaced in pitch over [`CURVE_HZ`]
/// (`sg_audio::SPECTRUM_BANDS`).
pub const BANDS: usize = 64;
pub type Spectrum = [[f32; BANDS]; 2];
/// What a band that holds nothing reads.
pub const SPECTRUM_FLOOR_DB: f32 = -96.0;
/// The loudness the spectrum is drawn between, bottom to top of the picture.
const SPECTRUM_DB: (f32, f32) = (-84.0, -12.0);

/// The quietest level the pictures show, in decibels.
const FLOOR_DB: f32 = -54.0;
const WAVE_H: f32 = 116.0;
const GAIN_H: f32 = 46.0;
const CURVE_H: f32 = 210.0;
/// The range of the line that shows how much a stage turns the sound up or down.
const GAIN_TOP_DB: f32 = 10.0;
const GAIN_BOTTOM_DB: f32 = -20.0;
/// The range of the equaliser's picture.
const CURVE_DB: f32 = 24.0;
const CURVE_HZ: (f32, f32) = (20.0, 20_000.0);

const IN: Color32 = Color32::from_rgb(96, 96, 106);
const LIMIT: Color32 = Color32::from_rgb(222, 196, 82);

/// What the picture is of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// The whole chain: the source against what leaves for the fader.
    #[default]
    Whole,
    LowCut,
    Denoise,
    Tune,
    Eq,
    DeEss,
    Leveller,
    Comp,
    Drive,
}

impl Stage {
    pub const ALL: [Stage; 9] = [
        Stage::Whole,
        Stage::LowCut,
        Stage::Denoise,
        Stage::Tune,
        Stage::Eq,
        Stage::DeEss,
        Stage::Leveller,
        Stage::Comp,
        Stage::Drive,
    ];

    /// As the chain's list names it.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Whole => "Whole chain",
            Stage::LowCut => "Low cut",
            Stage::Denoise => "Noise reduction",
            Stage::Tune => "Tune",
            Stage::Eq => "Equaliser",
            Stage::DeEss => "De-esser",
            Stage::Leveller => "Leveller",
            Stage::Comp => "Compressor",
            Stage::Drive => "Saturation",
        }
    }

    pub fn named(name: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|stage| stage.name() == name)
    }

    /// The stage's number in a track's order (`sg_core::STAGES`).
    pub fn number(self) -> Option<u8> {
        Stage::ALL.iter().position(|stage| *stage == self).and_then(|at| at.checked_sub(1)).map(|n| n as u8)
    }

    pub fn numbered(number: u8) -> Option<Stage> {
        Stage::ALL.get(number as usize + 1).copied()
    }

    /// The engine's points before and after the stage, for a track that
    /// has its stages in `order`: point 0 is the source, point n what
    /// comes out of the n-th stage.
    pub fn taps(self, order: &[u8; sg_core::STAGES]) -> (usize, usize) {
        match self.number().and_then(|number| order.iter().position(|stage| *stage == number)) {
            Some(place) => (place, place + 1),
            None => (0, TAPS - 1),
        }
    }

    /// Whether a track with these settings has the stage in its list:
    /// the voice's own stages are on voice tracks only.
    pub fn shown_for(self, mix: &TrackSettings) -> bool {
        mix.voice || !matches!(self, Stage::Tune | Stage::DeEss | Stage::Leveller | Stage::Drive)
    }

    /// Whether the picture has the sound by pitch in it (and the engine
    /// is asked for the spectrum around this stage).
    pub fn by_pitch(self) -> bool {
        matches!(self, Stage::LowCut | Stage::Eq)
    }

    fn about(self) -> &'static str {
        match self {
            Stage::Whole => "The recording as it is against what the whole chain makes of it.",
            Stage::LowCut => {
                "Takes away what lies below the frequency: rumble, the hum of the mains (50 Hz), the thump of a P. \
                 Pull the handle at the bottom; 80 to 120 Hz suits a voice."
            }
            Stage::Denoise => {
                "Turns down hiss and room noise. Look at the pauses: what goes in there should not come out."
            }
            Stage::Tune => "Moves the pitch onto the notes of the key. The level stays as it is.",
            Stage::Eq => {
                "Makes ranges of the sound louder or quieter. Pull the three handles: bass (120 Hz), the bell \
                 (sideways too) and treble (8 kHz). Double-click one to put it back."
            }
            Stage::DeEss => "Turns down the S sounds: short dips of the line below where an S is.",
            Stage::Leveller => {
                "A hand on the fader: loud phrases are pulled down, quiet ones pushed up. The line below shows \
                 the hand."
            }
            Stage::Comp => {
                "Whatever is louder than the yellow line is turned down. Pull the line to where only the loud \
                 parts reach it."
            }
            Stage::Drive => "Rounds off the peaks: the loudest moments come out a little lower and denser.",
        }
    }

    /// Whether the stage turns the level up and down over time (and the
    /// line that shows by how much says something).
    fn moves_level(self) -> bool {
        matches!(self, Stage::Whole | Stage::Denoise | Stage::DeEss | Stage::Leveller | Stage::Comp | Stage::Drive)
    }
}

fn to_db(linear: f32) -> f32 {
    20.0 * linear.max(1e-6).log10()
}

/// Draws the picture of `stage` for the track with the settings `mix`,
/// which the handles in it change.
///
/// `spectrum`: the sound by pitch going into the stage and coming out, in
/// decibels per band (`Spectrum`), as it should be shown now.
pub fn ui(ui: &mut egui::Ui, stage: Stage, view: &ViewerState, mix: &mut TrackSettings, spectrum: &Spectrum) {
    let width = 600.0;
    ui.set_width(width);
    ui.label(RichText::new(stage.about()).size(10.5).color(theme::DIM));
    ui.add_space(4.0);
    if stage.by_pitch() {
        curve(ui, width, stage, view, mix, spectrum);
        ui.add_space(6.0);
    }
    let (before, after) = stage.taps(&mix.order);
    legend(ui, stage);
    wave(ui, width, stage, view, mix, before, after);
    if stage.moves_level() {
        ui.add_space(2.0);
        gain(ui, width, view, before, after);
    }
    ui.add_space(4.0);
    fields(ui, stage, mix);
}

fn legend(ui: &mut egui::Ui, stage: Stage) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (colour, text) in [(IN, "going in"), (theme::ACCENT, "coming out")] {
            let (dot, _) = ui.allocate_exact_size(vec2(9.0, 9.0), Sense::hover());
            ui.painter().rect_filled(dot, 2.0, colour);
            ui.label(RichText::new(text).size(10.0).color(theme::DIM));
            ui.add_space(6.0);
        }
        if stage == Stage::Comp {
            let (dot, _) = ui.allocate_exact_size(vec2(12.0, 9.0), Sense::hover());
            ui.painter().hline(dot.x_range(), dot.center().y, Stroke::new(1.5, LIMIT));
            ui.label(RichText::new("compress above (pull it)").size(10.0).color(theme::DIM));
        }
    });
}

/// How far from the middle of a picture `half` high a level is drawn.
fn reach(level: f32, half: f32) -> f32 {
    ((to_db(level) - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0) * half
}

/// The sound before and after the stage, moving from right to left past
/// the playhead: mirrored like a waveform, but in decibels, so that quiet
/// sound (noise in a pause) can be seen beside loud.
fn wave(
    ui: &mut egui::Ui,
    width: f32,
    stage: Stage,
    view: &ViewerState,
    mix: &mut TrackSettings,
    before: usize,
    after: usize,
) {
    let sense = if stage == Stage::Comp { Sense::drag() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(width, WAVE_H), sense);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::RADIUS, theme::BG);
    let (mid, half) = (rect.center().y, rect.height() / 2.0 - 2.0);
    for db in [-12.0f32, -24.0, -36.0] {
        let off = (db - FLOOR_DB) / -FLOOR_DB * half;
        for y in [mid - off, mid + off] {
            painter.hline(rect.x_range(), y, Stroke::new(1.0, theme::RAISED));
        }
        let text = format!("{db:.0}");
        let at = pos2(rect.right() - 3.0, mid - off);
        painter.text(at, egui::Align2::RIGHT_CENTER, text, egui::FontId::proportional(9.0), theme::LINE);
    }
    let steps = view.scope.len();
    if steps == 0 || view.scope.iter().all(Option::is_none) {
        let text = "Play the track: its sound moves through here";
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(11.5), theme::DIM);
    } else {
        let dx = rect.width() / steps as f32;
        let stroke = |colour: Color32| Stroke::new(dx.max(1.0) + 0.3, colour);
        let out = theme::ACCENT.gamma_multiply(0.85);
        for (n, taps) in view.scope.iter().enumerate() {
            let Some(taps) = taps else { continue };
            let x = rect.left() + (n as f32 + 0.5) * dx;
            let (a, b) = (reach(taps[before], half), reach(taps[after], half));
            // What goes in behind, what comes out in front: grey that
            // sticks out was taken away, colour that sticks out was added.
            if a > 0.0 {
                painter.vline(x, (mid - a)..=(mid + a), stroke(IN));
            }
            if b > 0.0 {
                painter.vline(x, (mid - b.min(a))..=(mid + b.min(a)), stroke(out));
                if b > a {
                    for (from, to) in [(mid - b, mid - a), (mid + a, mid + b)] {
                        painter.vline(x, from..=to, stroke(theme::ACCENT));
                    }
                }
            }
        }
        // What is heard now; to its right what is about to be.
        let now = rect.left() + (steps - view.scope_ahead.min(steps)) as f32 * dx;
        painter.rect_filled(Rect::from_min_max(pos2(now, rect.top()), rect.max), 0.0, Color32::from_black_alpha(90));
        painter.vline(now, rect.y_range(), Stroke::new(1.0, theme::TEXT));
    }
    if stage == Stage::Comp {
        if response.dragged()
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let db = FLOOR_DB * (1.0 - ((pointer.y - mid).abs() / half).min(1.0));
            mix.comp_threshold_db = (db * 2.0).round() / 2.0;
        }
        let on = mix.comp_threshold_db < 0.0;
        let off = (mix.comp_threshold_db.max(FLOOR_DB) - FLOOR_DB) / -FLOOR_DB * half;
        let colour = if on { LIMIT } else { LIMIT.gamma_multiply(0.4) };
        for y in [mid - off, mid + off] {
            painter.hline(rect.x_range(), y, Stroke::new(1.5, colour));
        }
        let text = if on { format!("{:.1} dB", mix.comp_threshold_db) } else { "off (pull down)".to_owned() };
        let at = pos2(rect.left() + 5.0, mid - off + 2.0);
        painter.text(at, egui::Align2::LEFT_TOP, text, egui::FontId::proportional(10.0), colour);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Compressor threshold line"));
        response.on_hover_cursor(egui::CursorIcon::ResizeVertical);
    }
    painter.rect_stroke(rect, theme::RADIUS, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
}

/// By how much the stage turns the sound up or down along the same
/// stretch of time: what comes out against what goes in.
fn gain(ui: &mut egui::Ui, width: f32, view: &ViewerState, before: usize, after: usize) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, GAIN_H), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::RADIUS, theme::BG);
    let y_of = |db: f32| {
        let db = db.clamp(GAIN_BOTTOM_DB, GAIN_TOP_DB);
        rect.top() + (GAIN_TOP_DB - db) / (GAIN_TOP_DB - GAIN_BOTTOM_DB) * rect.height()
    };
    painter.hline(rect.x_range(), y_of(0.0), Stroke::new(1.0, theme::LINE));
    for db in [-6.0f32, -12.0] {
        painter.hline(rect.x_range(), y_of(db), Stroke::new(1.0, theme::RAISED));
        let at = pos2(rect.right() - 3.0, y_of(db));
        painter.text(at, egui::Align2::RIGHT_CENTER, format!("{db:.0}"), egui::FontId::proportional(9.0), theme::LINE);
    }
    let steps = view.scope.len();
    let dx = rect.width() / steps.max(1) as f32;
    // Only where something goes in: the change of silence is no number.
    let change = |taps: &[f32; TAPS]| (to_db(taps[before]) > -50.0).then(|| to_db(taps[after]) - to_db(taps[before]));
    let mut line: Vec<egui::Pos2> = Vec::new();
    let flush = |line: &mut Vec<egui::Pos2>| {
        if line.len() > 1 {
            painter.add(egui::Shape::line(std::mem::take(line), Stroke::new(1.5, theme::ACCENT)));
        }
        line.clear();
    };
    for (n, taps) in view.scope.iter().enumerate() {
        match taps.as_ref().and_then(change) {
            Some(db) => line.push(pos2(rect.left() + (n as f32 + 0.5) * dx, y_of(db))),
            None => flush(&mut line),
        }
    }
    flush(&mut line);
    // The number at the playhead: the strongest of the last moments, so that it can be read.
    let now = steps.saturating_sub(view.scope_ahead.min(steps));
    let recent = view.scope[now.saturating_sub(24)..now].iter().filter_map(|taps| taps.as_ref().and_then(change));
    let strongest = recent.fold(0.0f32, |most, db| if db.abs() > most.abs() { db } else { most });
    let text = if strongest.abs() >= 0.3 { format!("{strongest:+.1} dB now") } else { "0 dB: untouched".to_owned() };
    let at = pos2(rect.left() + 5.0, rect.top() + 3.0);
    painter.text(at, egui::Align2::LEFT_TOP, text, egui::FontId::proportional(10.0), theme::DIM);
    if steps > 0 {
        painter.vline(rect.left() + now as f32 * dx, rect.y_range(), Stroke::new(1.0, theme::TEXT));
    }
    painter.rect_stroke(rect, theme::RADIUS, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
}

/// What low cut and equaliser together do to each frequency, with a
/// handle for each of them.
fn curve(
    ui: &mut egui::Ui,
    width: f32,
    stage: Stage,
    view: &ViewerState,
    mix: &mut TrackSettings,
    spectrum: &Spectrum,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, CURVE_H), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, theme::RADIUS, theme::BG);
    let span = (CURVE_HZ.1 / CURVE_HZ.0).ln();
    let x_of = |hz: f32| rect.left() + (hz.clamp(CURVE_HZ.0, CURVE_HZ.1) / CURVE_HZ.0).ln() / span * rect.width();
    let hz_of = |x: f32| CURVE_HZ.0 * (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * span).exp();
    let y_of = |db: f32| rect.center().y - db.clamp(-CURVE_DB, CURVE_DB) / CURVE_DB * (rect.height() / 2.0 - 4.0);
    let db_of = |y: f32| (rect.center().y - y) / (rect.height() / 2.0 - 4.0) * CURVE_DB;
    for (hz, name) in [
        (50.0, "50"),
        (100.0, "100"),
        (200.0, "200"),
        (500.0, "500"),
        (1000.0, "1k"),
        (2000.0, "2k"),
        (5000.0, "5k"),
        (10_000.0, "10k"),
    ] {
        painter.vline(x_of(hz), rect.y_range(), Stroke::new(1.0, theme::RAISED));
        let at = pos2(x_of(hz) + 2.0, rect.bottom() - 2.0);
        painter.text(at, egui::Align2::LEFT_BOTTOM, name, egui::FontId::proportional(9.0), theme::LINE);
    }
    for db in [-12.0f32, 12.0] {
        painter.hline(rect.x_range(), y_of(db), Stroke::new(1.0, theme::RAISED));
        let at = pos2(rect.right() - 3.0, y_of(db));
        painter.text(at, egui::Align2::RIGHT_CENTER, format!("{db:+.0}"), egui::FontId::proportional(9.0), theme::LINE);
    }
    // Behind the curve, the sound itself by pitch (feedback F16.24): grey
    // what goes into the stage, orange what comes out, so that a boost or
    // a cut is seen on the sound it acts on.
    let level_y = |db: f32| {
        let share = ((db - SPECTRUM_DB.0) / (SPECTRUM_DB.1 - SPECTRUM_DB.0)).clamp(0.0, 1.0);
        rect.bottom() - share * rect.height()
    };
    for (bands, colour) in [(&spectrum[0], IN.gamma_multiply(0.55)), (&spectrum[1], theme::ACCENT.gamma_multiply(0.45))]
    {
        if bands.iter().all(|db| *db <= SPECTRUM_DB.0) {
            continue;
        }
        let mut mesh = egui::Mesh::default();
        for (band, db) in bands.iter().enumerate() {
            let x = rect.left() + (band as f32 + 0.5) / BANDS as f32 * rect.width();
            mesh.colored_vertex(pos2(x, level_y(*db)), colour);
            mesh.colored_vertex(pos2(x, rect.bottom()), colour);
            if band > 0 {
                let at = band as u32 * 2;
                mesh.add_triangle(at - 2, at - 1, at);
                mesh.add_triangle(at - 1, at, at + 1);
            }
        }
        painter.add(egui::Shape::mesh(mesh));
    }
    painter.hline(rect.x_range(), y_of(0.0), Stroke::new(1.0, theme::LINE));
    // The handles first: the curve is then of what they were pulled to.
    let handle = |ui: &mut egui::Ui, label: &str, at: egui::Pos2, lit: bool| {
        let area = Rect::from_center_size(at, vec2(18.0, 18.0));
        let response = ui.interact(area, ui.scope_id().with(("scope-handle", label)), Sense::click_and_drag());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, label));
        let hot = response.hovered() || response.dragged();
        let colour = if lit { theme::ACCENT } else { theme::DIM };
        painter.circle(at, if hot { 6.5 } else { 5.0 }, theme::BG, Stroke::new(2.0, colour));
        let pulled = response.dragged().then(|| response.interact_pointer_pos()).flatten();
        (pulled, response.double_clicked())
    };
    let tenth = |db: f32| (db.clamp(-18.0, 18.0) * 10.0).round() / 10.0;
    // The low cut sits on the bottom edge, at its frequency (at the left end while off).
    let cut_at = pos2(x_of(mix.low_cut_hz.max(CURVE_HZ.0)), rect.bottom() - 14.0);
    let (pulled, reset) = handle(ui, "Low cut handle", cut_at, stage == Stage::LowCut);
    if let Some(pointer) = pulled {
        let hz = hz_of(pointer.x).round();
        // Pulled to the left end: off.
        mix.low_cut_hz = if hz <= CURVE_HZ.0 + 2.0 { 0.0 } else { hz.min(1000.0) };
    }
    if reset {
        mix.low_cut_hz = 0.0;
    }
    let lit = stage == Stage::Eq;
    let (pulled, reset) = handle(ui, "Bass handle", pos2(x_of(120.0), y_of(mix.eq_low_db)), lit);
    if let Some(pointer) = pulled {
        mix.eq_low_db = tenth(db_of(pointer.y));
    }
    if reset {
        mix.eq_low_db = 0.0;
    }
    let (pulled, reset) = handle(ui, "Bell handle", pos2(x_of(mix.eq_mid_hz), y_of(mix.eq_mid_db)), lit);
    if let Some(pointer) = pulled {
        mix.eq_mid_db = tenth(db_of(pointer.y));
        mix.eq_mid_hz = ((hz_of(pointer.x) / 10.0).round() * 10.0).clamp(100.0, 10_000.0);
    }
    if reset {
        mix.eq_mid_db = 0.0;
    }
    let (pulled, reset) = handle(ui, "Treble handle", pos2(x_of(8000.0), y_of(mix.eq_high_db)), lit);
    if let Some(pointer) = pulled {
        mix.eq_high_db = tenth(db_of(pointer.y));
    }
    if reset {
        mix.eq_high_db = 0.0;
    }
    if let Some(response_db) = view.eq_response {
        let points = (0..=120).map(|n| {
            let x = rect.left() + rect.width() * n as f32 / 120.0;
            pos2(x, y_of(response_db(mix, hz_of(x))))
        });
        painter.add(egui::Shape::line(points.collect(), Stroke::new(2.0, theme::TEXT)));
    }
    let text =
        if mix.low_cut_hz > 0.0 { format!("cut below {:.0} Hz", mix.low_cut_hz) } else { "low cut off".to_owned() };
    painter.text(
        cut_at + vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(10.0),
        theme::DIM,
    );
    painter.rect_stroke(rect, theme::RADIUS, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
}

/// The stage's settings as numbers, under its picture.
fn fields(ui: &mut egui::Ui, stage: Stage, mix: &mut TrackSettings) {
    let share = |ui: &mut egui::Ui, label: &str, value: &mut f32| {
        let mut percent = *value * 100.0;
        ui.label(RichText::new(label).size(10.5).color(theme::DIM));
        let field = egui::DragValue::new(&mut percent).range(0.0..=100.0).speed(1.0).fixed_decimals(0).suffix(" %");
        if ui.add(field).changed() {
            *value = percent / 100.0;
        }
    };
    ui.horizontal(|ui| match stage {
        Stage::Whole => {
            ui.label(RichText::new("Choose a stage in the list to see and set it alone.").size(10.5).color(theme::DIM));
        }
        Stage::LowCut => {
            ui.label(RichText::new("Cut below").size(10.5).color(theme::DIM));
            ui.add(
                egui::DragValue::new(&mut mix.low_cut_hz)
                    .range(0.0..=1000.0)
                    .speed(1.0)
                    .fixed_decimals(0)
                    .suffix(" Hz"),
            );
        }
        Stage::Denoise => share(ui, "Noise reduction", &mut mix.denoise),
        Stage::Tune => share(ui, "Tune", &mut mix.tune),
        Stage::Eq => {
            for (label, db) in
                [("Bass", &mut mix.eq_low_db), ("Bell", &mut mix.eq_mid_db), ("Treble", &mut mix.eq_high_db)]
            {
                ui.label(RichText::new(label).size(10.5).color(theme::DIM));
                ui.add(egui::DragValue::new(db).range(-18.0..=18.0).speed(0.1).fixed_decimals(1).suffix(" dB"));
            }
            ui.label(RichText::new("Bell at").size(10.5).color(theme::DIM));
            let hz = egui::DragValue::new(&mut mix.eq_mid_hz).range(100.0..=10_000.0).speed(10.0).fixed_decimals(0);
            ui.add(hz.suffix(" Hz"));
        }
        Stage::DeEss => share(ui, "De-esser", &mut mix.deess),
        Stage::Leveller => share(ui, "Leveller", &mut mix.level),
        Stage::Comp => {
            ui.label(RichText::new("Compress above").size(10.5).color(theme::DIM));
            let threshold =
                egui::DragValue::new(&mut mix.comp_threshold_db).range(-60.0..=0.0).speed(0.2).fixed_decimals(1);
            ui.add(threshold.suffix(" dB"));
            ui.label(RichText::new("Ratio").size(10.5).color(theme::DIM));
            ui.add(
                egui::DragValue::new(&mut mix.comp_ratio)
                    .range(1.0..=20.0)
                    .speed(0.05)
                    .fixed_decimals(1)
                    .suffix(" : 1"),
            );
            ui.label(RichText::new("Make-up").size(10.5).color(theme::DIM));
            let makeup = egui::DragValue::new(&mut mix.comp_makeup_db).range(0.0..=24.0).speed(0.1).fixed_decimals(1);
            ui.add(makeup.suffix(" dB"));
        }
        Stage::Drive => share(ui, "Saturation", &mut mix.drive),
    });
}
