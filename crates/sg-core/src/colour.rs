//! Colour in two stages, as agreed in 05-color.md: a *match* that brings
//! every clip of a scenario to the same exposure, white balance, contrast
//! and saturation, then a *look*: a chain of small modules applied to the
//! whole scenario. Everything here is arithmetic on linear-light RGB; the
//! compositor's shader does the same sums (`sg-render`, composite.wgsl) and
//! a test holds the two together.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{AssetId, ScenarioId};

/// Rec.709 luminance weights.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn luma(c: [f32; 3]) -> f32 {
    c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2]
}

/// What the match stage knows about an asset's picture, measured on
/// sampled frames in linear light.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColourStats {
    /// Mean of the natural logarithm of luminance: the exposure.
    pub log_lum: f32,
    /// Standard deviation of the same: the contrast.
    pub spread: f32,
    /// Mean ln(R/G) and ln(B/G) of the nearly grey parts: the white balance.
    pub wb: [f32; 2],
    /// Mean saturation: (max - min) / luminance per pixel, with the white
    /// balance taken out first.
    pub saturation: f32,
}

impl ColourStats {
    /// Measures linear-light RGB pixels. `None` if there is nothing to
    /// measure (black frames).
    pub fn measure(pixels: impl Iterator<Item = [f32; 3]>) -> Option<ColourStats> {
        // Clipped and nearly black pixels say nothing about the scene. A
        // channel at zero (a saturated colour) is kept, lifted just off the
        // floor so that its ratios stay finite: leaving such pixels out would
        // make two cameras be measured on different parts of the picture.
        let usable: Vec<[f32; 3]> =
            pixels.filter(|c| (0.002..=0.95).contains(&luma(*c))).map(|c| c.map(|v| v.max(0.002))).collect();
        if usable.len() < 16 {
            return None;
        }
        let n = usable.len() as f64;
        let saturation = |c: &[f32; 3]| {
            let (max, min) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
            // Against luminance: changing saturation changes this in proportion.
            ((max - min) / luma(*c).max(1e-4)).min(4.0)
        };
        let (mut sum, mut sum_sq) = (0.0f64, 0.0f64);
        for c in &usable {
            let ln = (luma(*c) as f64).ln();
            sum += ln;
            sum_sq += ln * ln;
        }
        let mean = sum / n;
        // White balance: the average cast of the parts that are nearly grey.
        // Which parts those are depends on the cast, so it is found in
        // rounds: take the cast out, look again for what is grey now.
        let mut wb = [0.0f32; 2];
        for _ in 0..4 {
            let neutral = [(-wb[0]).exp(), 1.0, (-wb[1]).exp()];
            let (mut weight, mut cast) = (0.0f64, [0.0f64; 2]);
            for c in &usable {
                let s = saturation(&[c[0] * neutral[0], c[1], c[2] * neutral[2]]);
                // Fades out smoothly: no pixel jumps in or out between rounds.
                let w = (-(s / 0.15) * (s / 0.15)).exp() as f64 + 1e-4;
                weight += w;
                cast = [cast[0] + w * (c[0] / c[1]).ln() as f64, cast[1] + w * (c[2] / c[1]).ln() as f64];
            }
            wb = cast.map(|v| (v / weight) as f32);
        }
        // Saturation is measured with the cast taken out: a cast alone
        // would otherwise read as colour.
        let neutral = [(-wb[0]).exp(), 1.0, (-wb[1]).exp()];
        let saturation =
            usable.iter().map(|c| saturation(&[c[0] * neutral[0], c[1], c[2] * neutral[2]]) as f64).sum::<f64>() / n;
        Some(ColourStats {
            log_lum: mean as f32,
            spread: (sum_sq / n - mean * mean).max(0.0).sqrt() as f32,
            wb,
            saturation: saturation as f32,
        })
    }

    /// The middle of several measurements, value by value: one odd clip
    /// does not drag the target of a scenario with it.
    pub fn median(all: &[ColourStats]) -> Option<ColourStats> {
        let middle = |f: &dyn Fn(&ColourStats) -> f32| {
            let mut values: Vec<f32> = all.iter().map(f).collect();
            values.sort_by(f32::total_cmp);
            let n = values.len();
            if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 }
        };
        (!all.is_empty()).then(|| ColourStats {
            log_lum: middle(&|s| s.log_lum),
            spread: middle(&|s| s.spread),
            wb: [middle(&|s| s.wb[0]), middle(&|s| s.wb[1])],
            saturation: middle(&|s| s.saturation),
        })
    }
}

/// The match stage for one clip: a small parametric correction, so it
/// cannot produce artefacts and does not flicker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Correction {
    /// Per-channel gain: exposure and white balance.
    pub gain: [f32; 3],
    /// Contrast as an exponent around `pivot`.
    pub contrast: f32,
    pub pivot: f32,
    pub saturation: f32,
}

impl Correction {
    pub const NONE: Correction = Correction { gain: [1.0; 3], contrast: 1.0, pivot: 0.18, saturation: 1.0 };

    /// The correction that takes a picture measured as `own` towards
    /// `target`, `strength` of the way (0 nothing, 1 all of it).
    pub fn solve(own: &ColourStats, target: &ColourStats, strength: f32) -> Correction {
        let s = strength.clamp(0.0, 1.0);
        // White balance first; it changes luminance a little, which the
        // exposure gain then takes into account.
        let wb =
            [(target.wb[0] - own.wb[0]).clamp(-1.0, 1.0).exp(), 1.0, (target.wb[1] - own.wb[1]).clamp(-1.0, 1.0).exp()];
        let shift = luma(wb).ln();
        let exposure = (target.log_lum - own.log_lum - shift).clamp(-3.0, 3.0).exp();
        let contrast = (target.spread / own.spread.max(0.05)).clamp(0.6, 1.6);
        // Saturation as measured scales with nothing else here, so its ratio is the factor.
        let saturation = (target.saturation / own.saturation.max(0.01)).clamp(0.5, 2.0);
        Correction {
            gain: wb.map(|g| (g * exposure).powf(s)),
            contrast: contrast.powf(s),
            pivot: target.log_lum.exp().clamp(0.01, 1.0),
            saturation: saturation.powf(s),
        }
    }

    /// Applies the correction to one linear-light pixel (the shader's sums).
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let c = [c[0] * self.gain[0], c[1] * self.gain[1], c[2] * self.gain[2]];
        // Contrast acts on luminance and scales the colour with it, so it
        // does not change hue or saturation.
        let l = luma(c).max(1e-6);
        let scaled = self.pivot * (l / self.pivot).powf(self.contrast) / l;
        let l = l * scaled;
        c.map(|v| (l + (v * scaled - l) * self.saturation).max(0.0))
    }
}

/// The kinds of look module. The number is what the shader switches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleKind {
    Balance = 1,
    Tone = 2,
    Colour = 3,
    SplitTone = 4,
    Vignette = 5,
    Grain = 6,
    Sharpen = 7,
    Glow = 8,
}

/// A knob of a module: its name, range and resting value.
pub struct Knob {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub rest: f32,
    pub hint: &'static str,
}

const fn knob(name: &'static str, min: f32, max: f32, rest: f32, hint: &'static str) -> Knob {
    Knob { name, min, max, rest, hint }
}

impl ModuleKind {
    pub const ALL: [ModuleKind; 8] = [
        ModuleKind::Balance,
        ModuleKind::Tone,
        ModuleKind::Colour,
        ModuleKind::SplitTone,
        ModuleKind::Vignette,
        ModuleKind::Grain,
        ModuleKind::Sharpen,
        ModuleKind::Glow,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ModuleKind::Balance => "Balance",
            ModuleKind::Tone => "Tone",
            ModuleKind::Colour => "Colour",
            ModuleKind::SplitTone => "Split tone",
            ModuleKind::Vignette => "Vignette",
            ModuleKind::Grain => "Grain",
            ModuleKind::Sharpen => "Sharpen / soften",
            ModuleKind::Glow => "Glow",
        }
    }

    /// Up to four knobs; the values live in [`Module::knobs`] in this order.
    pub fn knobs(self) -> &'static [Knob] {
        match self {
            ModuleKind::Balance => {
                const {
                    &[
                        knob("Temperature", -1.0, 1.0, 0.0, "Cooler (blue) to warmer (orange)"),
                        knob("Tint", -1.0, 1.0, 0.0, "Green to magenta"),
                        knob("Exposure", -3.0, 3.0, 0.0, "In stops: +1 doubles the light"),
                    ]
                }
            }
            ModuleKind::Tone => {
                const {
                    &[
                        knob("Contrast", -1.0, 1.0, 0.0, "Around the pivot"),
                        knob("Highlights", 0.0, 1.0, 0.0, "Rolls the brightest parts off softly"),
                        knob("Shadows", -1.0, 1.0, 0.0, "Lifts or deepens the darkest parts"),
                        knob("Pivot", 0.05, 0.5, 0.18, "The brightness contrast turns around (0.18 is middle grey)"),
                    ]
                }
            }
            ModuleKind::Colour => {
                const {
                    &[
                        knob("Saturation", 0.0, 2.0, 1.0, "0 is black and white"),
                        knob("Vibrance", -1.0, 1.0, 0.0, "Saturation for the dull colours only: skin stays as it is"),
                    ]
                }
            }
            ModuleKind::SplitTone => {
                const {
                    &[
                        knob(
                            "Shadow hue",
                            0.0,
                            360.0,
                            220.0,
                            "Colour pushed into the dark parts, as an angle on the colour wheel",
                        ),
                        knob("Highlight hue", 0.0, 360.0, 40.0, "Colour pushed into the bright parts"),
                        knob("Amount", 0.0, 1.0, 0.2, ""),
                        knob("Balance", -1.0, 1.0, 0.0, "Where dark ends and bright begins"),
                    ]
                }
            }
            ModuleKind::Vignette => {
                const {
                    &[
                        knob("Amount", -1.0, 1.0, 0.3, "Darkens the corners; negative brightens them"),
                        knob("Size", 0.2, 1.5, 0.8, "How far from the centre it starts"),
                        knob("Softness", 0.05, 1.0, 0.5, ""),
                    ]
                }
            }
            ModuleKind::Grain => {
                const { &[knob("Amount", 0.0, 1.0, 0.15, ""), knob("Size", 1.0, 4.0, 1.5, "In pixels of the frame")] }
            }
            ModuleKind::Sharpen => {
                const {
                    &[
                        knob("Amount", -1.0, 2.0, 0.5, "Above 0 sharpens edges, below 0 softens the picture"),
                        knob(
                            "Radius",
                            0.5,
                            6.0,
                            1.5,
                            "Width of the edges it works on, in pixels of a 1080-line picture",
                        ),
                    ]
                }
            }
            ModuleKind::Glow => {
                const {
                    &[
                        knob("Threshold", 0.0, 1.5, 0.6, "How bright a part must be to glow (1 is white)"),
                        knob("Radius", 0.5, 8.0, 3.0, "How far the glow spreads, in percent of the picture height"),
                        knob("Amount", 0.0, 2.0, 0.5, ""),
                        knob(
                            "Warmth",
                            -1.0,
                            1.0,
                            0.3,
                            "Colour of the glow: above 0 towards red, as film halation; below towards blue",
                        ),
                    ]
                }
            }
        }
    }
}

/// One module of a look chain.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Module {
    pub kind: ModuleKind,
    /// Off: bypassed, settings kept.
    pub on: bool,
    /// How much of the module's result is used, 0 to 1.
    pub mix: f32,
    pub knobs: [f32; 4],
}

impl Module {
    pub fn new(kind: ModuleKind) -> Module {
        let mut knobs = [0.0; 4];
        for (slot, knob) in knobs.iter_mut().zip(kind.knobs()) {
            *slot = knob.rest;
        }
        Module { kind, on: true, mix: 1.0, knobs }
    }

    /// Values from a file or a script, brought into range.
    pub fn clamped(mut self) -> Module {
        for (slot, knob) in self.knobs.iter_mut().zip(self.kind.knobs()) {
            *slot = if slot.is_finite() { slot.clamp(knob.min, knob.max) } else { knob.rest };
        }
        self.mix = if self.mix.is_finite() { self.mix.clamp(0.0, 1.0) } else { 1.0 };
        self
    }
}

/// Modules a chain can hold (the shader's uniform has room for this many).
pub const MAX_MODULES: usize = 8;

/// An asset's place in a scenario.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Member {
    /// Its measurements, once analysed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<ColourStats>,
}

/// A group of clips that should look like one place and time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scenario {
    pub id: ScenarioId,
    pub name: String,
    /// Which of the stripe colours marks its clips.
    #[serde(default)]
    pub colour: u8,
    /// Strength of the match, 0 to 1.
    pub strength: f32,
    /// The asset every other one is matched to; without one, the middle of
    /// all members.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hero: Option<AssetId>,
    /// A/B switches: with one off, that stage is skipped (in export too).
    pub match_on: bool,
    pub look_on: bool,
    /// The look chain, in the order it is applied.
    #[serde(default)]
    pub look: Vec<Module>,
    /// The assets that belong to it, with their measurements once analysed.
    #[serde(default)]
    pub members: BTreeMap<AssetId, Member>,
}

impl Scenario {
    pub fn new(id: ScenarioId, name: impl Into<String>, colour: u8) -> Scenario {
        Scenario {
            id,
            name: name.into(),
            colour,
            strength: 1.0,
            hero: None,
            match_on: true,
            look_on: true,
            look: Vec::new(),
            members: BTreeMap::new(),
        }
    }

    /// What the members are matched to: the hero if it is measured, else
    /// the middle of all measured members.
    pub fn target(&self) -> Option<ColourStats> {
        if let Some(stats) = self.hero.and_then(|hero| self.members.get(&hero)?.stats) {
            return Some(stats);
        }
        let measured: Vec<ColourStats> = self.members.values().filter_map(|m| m.stats).collect();
        ColourStats::median(&measured)
    }

    /// Everything the renderer needs for a picture of `asset`.
    pub fn grade(&self, asset: AssetId) -> Grade {
        let correction = match (self.match_on, self.members.get(&asset).and_then(|m| m.stats), self.target()) {
            (true, Some(own), Some(target)) => Correction::solve(&own, &target, self.strength),
            _ => Correction::NONE,
        };
        let look = if self.look_on {
            self.look.iter().filter(|m| m.on && m.mix > 0.0).take(MAX_MODULES).map(|m| m.clamped()).collect()
        } else {
            Vec::new()
        };
        Grade { correction, look }
    }

    /// Values from a file brought into range.
    pub fn clamped(mut self) -> Scenario {
        self.strength = if self.strength.is_finite() { self.strength.clamp(0.0, 1.0) } else { 1.0 };
        self.look = self.look.into_iter().take(MAX_MODULES).map(Module::clamped).collect();
        self
    }
}

/// The colour treatment of one layer: match, then look.
#[derive(Clone, Debug, PartialEq)]
pub struct Grade {
    pub correction: Correction,
    pub look: Vec<Module>,
}

impl Grade {
    pub fn is_neutral(&self) -> bool {
        // (The pivot alone changes nothing.)
        let c = &self.correction;
        c.gain == [1.0; 3] && c.contrast == 1.0 && c.saturation == 1.0 && self.look.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with a range of brightness and some colour, deterministic.
    fn scene() -> Vec<[f32; 3]> {
        (0..4000)
            .map(|i| {
                let t = i as f32 / 4000.0;
                let l = 0.02 + 0.6 * t * t;
                let hue = (i % 37) as f32 / 37.0 * std::f32::consts::TAU;
                let colourful = if i % 3 == 0 { 0.5 } else { 0.08 };
                [l * (1.0 + colourful * hue.cos()), l, l * (1.0 + colourful * (hue + 2.0).cos())]
            })
            .collect()
    }

    #[test]
    fn a_darker_cooler_flatter_take_is_brought_back() {
        let hero = scene();
        // Another camera: a stop darker, cooler, less contrast, less colour.
        let spoil = Correction { gain: [0.42, 0.5, 0.61], contrast: 0.8, pivot: 0.1, saturation: 0.7 };
        let other: Vec<[f32; 3]> = hero.iter().map(|c| spoil.apply(*c)).collect();
        let (a, b) =
            (ColourStats::measure(hero.iter().copied()).unwrap(), ColourStats::measure(other.iter().copied()).unwrap());
        assert!(
            b.log_lum < a.log_lum - 0.5
                && b.wb[1] > a.wb[1] + 0.05
                && b.spread < a.spread
                && b.saturation < a.saturation,
            "{a:?} {b:?}"
        );
        let fix = Correction::solve(&b, &a, 1.0);
        let fixed: Vec<[f32; 3]> = other.iter().map(|c| fix.apply(*c)).collect();
        let after = ColourStats::measure(fixed.iter().copied()).unwrap();
        assert!((after.log_lum - a.log_lum).abs() < 0.03, "{after:?} vs {a:?}");
        assert!((after.wb[0] - a.wb[0]).abs() < 0.02 && (after.wb[1] - a.wb[1]).abs() < 0.02, "{after:?} vs {a:?}");
        assert!((after.spread - a.spread).abs() < 0.05 * a.spread, "{after:?} vs {a:?}");
        assert!((after.saturation - a.saturation).abs() < 0.1 * a.saturation, "{after:?} vs {a:?}");
        // Pixel by pixel the picture is close to the hero again.
        let worst = hero
            .iter()
            .zip(&fixed)
            .map(|(x, y)| (0..3).map(|k| (x[k] - y[k]).abs() / (x[k] + 0.02)).fold(0.0, f32::max))
            .fold(0.0, f32::max);
        assert!(worst < 0.2, "largest relative difference {worst}");
        // No strength, no change; half strength, half the way (in stops).
        assert_eq!(Correction::solve(&b, &a, 0.0).gain, [1.0; 3]);
        let half = Correction::solve(&b, &a, 0.5);
        assert!((half.gain[1].ln() * 2.0 - fix.gain[1].ln()).abs() < 1e-4);
    }

    #[test]
    fn a_scenario_matches_to_its_hero_or_to_the_middle() {
        let stats = |log_lum: f32| ColourStats { log_lum, spread: 1.0, wb: [0.0, 0.0], saturation: 0.2 };
        let mut scenario = Scenario::new(ScenarioId(1), "Stage", 0);
        for (id, lum) in [(1u128, -2.0f32), (2, -1.0), (3, -3.5)] {
            scenario.members.insert(AssetId(id), Member { stats: Some(stats(lum)) });
        }
        scenario.members.insert(AssetId(4), Member::default());
        assert_eq!(scenario.target().unwrap().log_lum, -2.0);
        // The brightest is brought down a stop-and-a-bit, the unmeasured one is left alone.
        assert!((scenario.grade(AssetId(2)).correction.gain[1] - (-1.0f32).exp()).abs() < 1e-4);
        assert!(scenario.grade(AssetId(4)).is_neutral() && scenario.grade(AssetId(1)).is_neutral());
        scenario.hero = Some(AssetId(3));
        assert_eq!(scenario.target().unwrap().log_lum, -3.5);
        scenario.match_on = false;
        assert!(scenario.grade(AssetId(2)).is_neutral());
        // The look: only what is switched on, and not when the chain is bypassed.
        scenario.look = vec![Module::new(ModuleKind::Tone), Module { on: false, ..Module::new(ModuleKind::Grain) }];
        assert_eq!(scenario.grade(AssetId(2)).look.len(), 1);
        scenario.look_on = false;
        assert!(scenario.grade(AssetId(2)).is_neutral());
    }

    #[test]
    fn modules_start_at_rest_and_stay_in_range() {
        for kind in ModuleKind::ALL {
            let module = Module::new(kind);
            assert!(kind.knobs().len() <= 4 && module.on && module.mix == 1.0);
            let wild = Module { knobs: [99.0, -99.0, f32::NAN, 0.0], mix: 7.0, ..module }.clamped();
            for (value, knob) in wild.knobs.iter().zip(kind.knobs()) {
                assert!((knob.min..=knob.max).contains(value), "{} {}", kind.name(), knob.name);
            }
            assert_eq!(wild.mix, 1.0);
        }
        assert_eq!(ColourStats::measure([[0.0; 3]; 100].into_iter()), None);
    }
}
