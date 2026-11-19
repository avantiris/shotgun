//! What a clip looks and sounds like beyond "this asset from here to
//! there": placement, crop, opacity, speed, volume, fades, and for title
//! clips the text. Animatable values carry keyframes.
//!
//! Most clips have none of this, so properties live in a sparse map per
//! track, keyed by clip id, and the clip arrays stay small.

use serde::{Deserialize, Serialize};

/// A keyframe: at `frame` (relative to the clip's start, in sequence
/// frames) the value is `value`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key {
    pub frame: i64,
    pub value: f32,
    /// How the value travels from this keyframe to the next.
    #[serde(default, skip_serializing_if = "Ease::is_linear")]
    pub ease: Ease,
}

/// The way from one keyframe to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ease {
    /// At a steady pace.
    #[default]
    Linear,
    /// Starts and arrives gently.
    Smooth,
    /// Stays, then jumps at the next keyframe.
    Hold,
}

impl Ease {
    pub const ALL: [Ease; 3] = [Ease::Linear, Ease::Smooth, Ease::Hold];

    fn is_linear(&self) -> bool {
        *self == Ease::Linear
    }

    pub fn label(self) -> &'static str {
        match self {
            Ease::Linear => "Linear",
            Ease::Smooth => "Smooth",
            Ease::Hold => "Hold",
        }
    }

    /// How far the value has come when `t` (0 to 1) of the way has passed.
    pub fn curve(self, t: f64) -> f64 {
        match self {
            Ease::Linear => t,
            Ease::Smooth => t * t * (3.0 - 2.0 * t),
            Ease::Hold => 0.0,
        }
    }

    /// The area under `curve` from 0 to `t`.
    fn area(self, t: f64) -> f64 {
        match self {
            Ease::Linear => t * t / 2.0,
            Ease::Smooth => t * t * t - t * t * t * t / 2.0,
            Ease::Hold => 0.0,
        }
    }
}

impl Key {
    pub fn new(frame: i64, value: f32) -> Key {
        Key { frame, value, ease: Ease::Linear }
    }
}

/// A value that is either constant or follows keyframes (between them as
/// the earlier one's `ease` says, held before the first and after the last).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub value: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<Key>,
}

impl Param {
    pub fn constant(value: f32) -> Param {
        Param { value, keys: Vec::new() }
    }

    pub fn is_animated(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn at(&self, frame: i64) -> f32 {
        let after = self.keys.partition_point(|k| k.frame <= frame);
        match (after.checked_sub(1).map(|i| self.keys[i]), self.keys.get(after)) {
            (None, None) => self.value,
            (Some(k), None) | (None, Some(&k)) => k.value,
            (Some(a), Some(b)) => {
                let t = (frame - a.frame) as f64 / (b.frame - a.frame) as f64;
                a.value + (b.value - a.value) * a.ease.curve(t) as f32
            }
        }
    }

    /// Sets the value at `frame`: the constant if there are no keyframes,
    /// otherwise the keyframe there (added if missing).
    pub fn set(&mut self, frame: i64, value: f32) {
        if self.keys.is_empty() {
            self.value = value;
        } else {
            self.set_key(frame, value);
        }
    }

    pub fn set_key(&mut self, frame: i64, value: f32) {
        match self.keys.binary_search_by_key(&frame, |k| k.frame) {
            Ok(i) => self.keys[i].value = value,
            // A keyframe put into a stretch continues that stretch's way of moving.
            Err(i) => {
                let ease = i.checked_sub(1).map_or(Ease::Linear, |before| self.keys[before].ease);
                self.keys.insert(i, Key { frame, value, ease });
            }
        }
    }

    /// Removes the keyframe at `frame`. The last one removed leaves its
    /// value as the constant.
    pub fn remove_key(&mut self, frame: i64) -> bool {
        let Ok(i) = self.keys.binary_search_by_key(&frame, |k| k.frame) else { return false };
        let removed = self.keys.remove(i);
        if self.keys.is_empty() {
            self.value = removed.value;
        }
        true
    }

    /// The way of moving in the stretch `frame` lies in, if it lies between two keyframes.
    pub fn ease_at(&self, frame: i64) -> Option<Ease> {
        let after = self.keys.partition_point(|k| k.frame <= frame);
        (after > 0 && after < self.keys.len()).then(|| self.keys[after - 1].ease)
    }

    /// Sets the way of moving in the stretch `frame` lies in.
    pub fn set_ease(&mut self, frame: i64, ease: Ease) -> bool {
        let after = self.keys.partition_point(|k| k.frame <= frame);
        if after == 0 || after >= self.keys.len() || self.keys[after - 1].ease == ease {
            return false;
        }
        self.keys[after - 1].ease = ease;
        true
    }

    /// A constant 1 (the resting state of a factor).
    pub fn is_one(&self) -> bool {
        self.keys.is_empty() && self.value == 1.0
    }

    /// The area under the curve from frame 0 to `to` (which may be
    /// negative: the value at the start then counts backwards).
    pub fn integral(&self, to: i64) -> f64 {
        let Some(first) = self.keys.first() else { return self.value as f64 * to as f64 };
        if to <= 0 {
            return self.at(0) as f64 * to as f64;
        }
        // Held at the first key's value up to it.
        let mut area = first.value as f64 * (first.frame.clamp(0, to)) as f64;
        for pair in self.keys.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (from, until) = (a.frame.max(0), b.frame.min(to));
            if until > from {
                let width = (b.frame - a.frame) as f64;
                let (t0, t1) = ((from - a.frame) as f64 / width, (until - a.frame) as f64 / width);
                area += width
                    * (a.value as f64 * (t1 - t0) + (b.value - a.value) as f64 * (a.ease.area(t1) - a.ease.area(t0)));
            }
        }
        // Held at the last key's value after it.
        if let Some(last) = self.keys.last().filter(|k| to > k.frame) {
            area += last.value as f64 * (to - last.frame.max(0)) as f64;
        }
        area
    }

    /// Moves every keyframe by `delta` frames (after a head trim or split).
    pub fn shift_keys(&mut self, delta: i64) {
        for key in &mut self.keys {
            key.frame += delta;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    /// Letter height as a fraction of the frame height.
    pub size: f32,
    /// Straight RGBA.
    pub color: [u8; 4],
    /// File name of the font in `assets/font/`; empty for the built-in one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub font: String,
}

impl Default for Title {
    fn default() -> Title {
        Title { text: "Title".to_owned(), size: 0.12, color: [255, 255, 255, 255], font: String::new() }
    }
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipProps {
    /// Centre of the picture relative to the frame centre, in frame widths
    /// (x, right is positive) and frame heights (y, down is positive).
    pub x: Param,
    pub y: Param,
    /// 1 fits the picture into the frame.
    pub scale: Param,
    /// Degrees, clockwise.
    pub rotation: Param,
    pub opacity: Param,
    /// Fractions of the picture cut off at left, top, right, bottom.
    pub crop: [f32; 4],
    /// Volume change in decibels.
    pub gain_db: Param,
    /// Fade lengths in sequence frames.
    pub fade_in: i64,
    pub fade_out: i64,
    /// Speed ramp: a factor on the speed that can change along the clip
    /// (keyframes), for the picture. 1 is the clip's speed as set.
    #[serde(skip_serializing_if = "Param::is_one")]
    pub ramp: Param,
    /// Playback speed as a fraction: 2/1 is double speed.
    pub speed: (u32, u32),
    /// Small colour trim of this clip alone, after the match of its
    /// scenario: exposure in stops, warmth from -1 (cooler) to 1 (warmer).
    #[serde(skip_serializing_if = "is_zero")]
    pub exposure: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub warmth: f32,
    /// Show the stabilised copy of the asset (made on first use).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stabilise: bool,
    /// How the picture arrives from the clip before it on the track.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    /// Set on title clips: the text to draw instead of an asset.
    pub title: Option<Title>,
}

impl Default for ClipProps {
    fn default() -> ClipProps {
        ClipProps {
            x: Param::constant(0.0),
            y: Param::constant(0.0),
            scale: Param::constant(1.0),
            rotation: Param::constant(0.0),
            opacity: Param::constant(1.0),
            crop: [0.0; 4],
            gain_db: Param::constant(0.0),
            fade_in: 0,
            fade_out: 0,
            ramp: Param::constant(1.0),
            speed: (1, 1),
            title: None,
            transition: None,
            stabilise: false,
            exposure: 0.0,
            warmth: 0.0,
        }
    }
}

impl ClipProps {
    /// The animatable picture parameters, for "keyframe everything here".
    /// The values the keyframe keys act on (the speed ramp among them).
    pub fn picture_params(&mut self) -> [&mut Param; 6] {
        [&mut self.x, &mut self.y, &mut self.scale, &mut self.rotation, &mut self.opacity, &mut self.ramp]
    }

    pub fn speed_valid(&self) -> bool {
        self.speed.0 > 0 && self.speed.1 > 0
    }

    /// Source units consumed after `elapsed` timeline units at this speed.
    pub fn source_offset(&self, elapsed: i64) -> i64 {
        let (num, den) = if self.speed_valid() { self.speed } else { (1, 1) };
        if !self.ramp.is_animated() && self.ramp.value == 1.0 {
            // No ramp: exact integer arithmetic.
            return (elapsed as i128 * num as i128 / den as i128) as i64;
        }
        // With a ramp the source position is the area under the speed curve.
        (self.ramp.integral(elapsed) * num as f64 / den as f64).floor() as i64
    }

    /// Linear gain of fades at `elapsed` of a clip `len` long (same unit as
    /// `fade_in`/`fade_out` after conversion by the caller).
    pub fn fade_gain(elapsed: i64, len: i64, fade_in: i64, fade_out: i64) -> f32 {
        let mut gain = 1.0f32;
        if fade_in > 0 && elapsed < fade_in {
            gain *= (elapsed.max(0) as f32) / fade_in as f32;
        }
        if fade_out > 0 && len - elapsed < fade_out {
            gain *= ((len - elapsed).max(0) as f32) / fade_out as f32;
        }
        gain
    }
}

/// Reduces a recorded path to the keyframes that matter: points that lie
/// within `tolerance` of the line between their kept neighbours are dropped
/// (Ramer-Douglas-Peucker on value over time).
pub fn simplify(keys: &[Key], tolerance: f32) -> Vec<Key> {
    if keys.len() <= 2 {
        return keys.to_vec();
    }
    let mut keep = vec![false; keys.len()];
    keep[0] = true;
    keep[keys.len() - 1] = true;
    let mut stack = vec![(0usize, keys.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (ka, kb) = (keys[a], keys[b]);
        let mut worst = (0.0f32, a);
        for (i, k) in keys.iter().enumerate().take(b).skip(a + 1) {
            let t = (k.frame - ka.frame) as f32 / (kb.frame - ka.frame).max(1) as f32;
            let off = (k.value - (ka.value + (kb.value - ka.value) * t)).abs();
            if off > worst.0 {
                worst = (off, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    keys.iter().zip(keep).filter(|(_, k)| *k).map(|(k, _)| *k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_and_holds() {
        let mut p = Param::constant(3.0);
        assert_eq!(p.at(-5), 3.0);
        p.set(10, 4.0);
        assert_eq!((p.value, p.is_animated()), (4.0, false));
        p.set_key(10, 0.0);
        p.set_key(20, 10.0);
        assert_eq!((p.at(0), p.at(10), p.at(15), p.at(20), p.at(99)), (0.0, 0.0, 5.0, 10.0, 10.0));
        // With keyframes, `set` edits the key under the playhead.
        p.set(15, 9.0);
        assert_eq!(p.keys.len(), 3);
        assert_eq!(p.at(15), 9.0);
        assert!(p.remove_key(15) && !p.remove_key(15));
        p.remove_key(10);
        p.remove_key(20);
        assert_eq!((p.is_animated(), p.value), (false, 10.0));
    }

    #[test]
    fn defaults_serialise_to_almost_nothing_and_survive_unknown_gaps() {
        let json = serde_json::to_string(&ClipProps::default()).unwrap();
        let back: ClipProps = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ClipProps::default());
        // Fields missing in an older file take their defaults.
        let sparse: ClipProps = serde_json::from_str(r#"{"opacity":{"value":0.5}}"#).unwrap();
        assert_eq!(sparse.opacity.value, 0.5);
        assert_eq!(sparse.scale.value, 1.0);
    }

    #[test]
    fn speed_and_fades() {
        let fast = ClipProps { speed: (2, 1), ..ClipProps::default() };
        assert_eq!(fast.source_offset(30), 60);
        let slow = ClipProps { speed: (1, 4), ..ClipProps::default() };
        assert_eq!(slow.source_offset(30), 7);
        assert_eq!(ClipProps::fade_gain(0, 100, 10, 10), 0.0);
        assert_eq!(ClipProps::fade_gain(5, 100, 10, 10), 0.5);
        assert_eq!(ClipProps::fade_gain(50, 100, 10, 10), 1.0);
        assert_eq!(ClipProps::fade_gain(95, 100, 10, 10), 0.5);
        assert_eq!(ClipProps::fade_gain(50, 100, 0, 0), 1.0);
    }

    #[test]
    fn simplify_keeps_corners_and_drops_the_straight_parts() {
        // A straight ramp with a kink at frame 50.
        let keys: Vec<Key> =
            (0..=100).map(|f| Key::new(f, if f <= 50 { f as f32 } else { 50.0 - (f - 50) as f32 * 0.5 })).collect();
        let simple = simplify(&keys, 0.01);
        assert_eq!(simple.iter().map(|k| k.frame).collect::<Vec<_>>(), [0, 50, 100]);
        // Noise below the tolerance disappears, the shape stays.
        let noisy: Vec<Key> =
            keys.iter().map(|k| Key { value: k.value + if k.frame % 2 == 0 { 0.05 } else { -0.05 }, ..*k }).collect();
        assert!(simplify(&noisy, 0.2).len() <= 5);
        assert_eq!(simplify(&keys[..2], 0.1).len(), 2);
    }
}

/// What a transition looks like. All three are one engine with other
/// numbers (05-color.md, "Whip-pan transition").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransitionKind {
    /// The outgoing picture races out of the frame, blurred by its speed;
    /// at the height of the blur the incoming one takes over and comes to
    /// rest. Reads as a cut; feels like the camera whipped across.
    Whip,
    /// The incoming picture pushes the outgoing one out.
    Slide,
    /// As the whip, but into and out of the picture instead of across it.
    Zoom,
}

/// A transition on the cut at the start of a clip, centred on the cut.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Length in sequence frames: 2 to 16, even.
    pub frames: u8,
    /// Direction the picture moves in, degrees: 0 to the left (the camera
    /// whips right), 90 up, 180 to the right, 270 down.
    pub angle: f32,
    /// Motion blur: the share of each frame's time the shutter is open.
    /// 0 is none, 1 a full frame, up to 2 for more streak than a camera gives.
    pub blur: f32,
    /// How far the picture travels on each side of the cut, in frame
    /// widths (whip), or how much it grows, as a factor minus one (zoom).
    pub amount: f32,
    /// Shape of the speed-up: 1 is even, larger starts slower and ends faster.
    pub ease: f32,
}

impl Default for Transition {
    fn default() -> Transition {
        Transition { kind: TransitionKind::Whip, frames: 4, angle: 0.0, blur: 1.0, amount: 1.5, ease: 2.5 }
    }
}

/// The picture's displacement at one instant of a transition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shift {
    /// In frame widths (right is positive) and heights (down is positive).
    pub x: f32,
    pub y: f32,
    /// Size factor about the frame centre.
    pub zoom: f32,
}

impl Shift {
    pub const REST: Shift = Shift { x: 0.0, y: 0.0, zoom: 1.0 };
}

impl Transition {
    /// The values a file or a script may hold, brought into range.
    pub fn clamped(self) -> Transition {
        let finite = |v: f32, low: f32, high: f32, or: f32| if v.is_finite() { v.clamp(low, high) } else { or };
        Transition {
            kind: self.kind,
            frames: (self.frames.clamp(2, 16) / 2) * 2,
            angle: finite(self.angle, -360.0, 360.0, 0.0),
            blur: finite(self.blur, 0.0, 2.0, 1.0),
            amount: finite(self.amount, 0.0, 4.0, 1.5),
            ease: finite(self.ease, 1.0, 6.0, 2.5),
        }
    }

    /// The transition a kind starts with.
    pub fn of(kind: TransitionKind) -> Transition {
        match kind {
            TransitionKind::Whip => Transition::default(),
            TransitionKind::Slide => {
                Transition { kind, frames: 8, blur: 0.0, amount: 1.0, ease: 2.0, ..Transition::default() }
            }
            TransitionKind::Zoom => Transition { kind, amount: 0.6, ..Transition::default() },
        }
    }

    /// Displacement of the outgoing (`incoming == false`) or incoming
    /// picture at `progress` through the transition, 0 at its first
    /// instant and 1 at its last; the cut is at 0.5.
    pub fn shift(&self, progress: f32, incoming: bool) -> Shift {
        let p = progress.clamp(0.0, 1.0);
        let (sin, cos) = self.angle.to_radians().sin_cos();
        // Angle 0 moves the picture left, 90 up (y is down).
        let along = |distance: f32| Shift { x: -cos * distance, y: -sin * distance, zoom: 1.0 };
        match self.kind {
            TransitionKind::Whip | TransitionKind::Zoom => {
                // Outgoing: from rest, ever faster, up to the cut. Incoming:
                // the same path in reverse, arriving from the other side.
                let travelled = if incoming {
                    -(2.0 - 2.0 * p).clamp(0.0, 1.0).powf(self.ease)
                } else {
                    (2.0 * p).clamp(0.0, 1.0).powf(self.ease)
                };
                if self.kind == TransitionKind::Whip {
                    along(travelled * self.amount)
                } else {
                    // Into the picture, then out of the next one's enlargement.
                    Shift { zoom: 1.0 + travelled.abs() * self.amount, ..Shift::REST }
                }
            }
            TransitionKind::Slide => {
                // One frame width or height of travel, eased at both ends.
                let eased =
                    if p < 0.5 { 0.5 * (2.0 * p).powf(self.ease) } else { 1.0 - 0.5 * (2.0 - 2.0 * p).powf(self.ease) };
                along(if incoming { eased - 1.0 } else { eased })
            }
        }
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;

    #[test]
    fn whip_leaves_and_arrives_in_the_same_direction() {
        let whip = Transition::default();
        // Out to the left, ever faster.
        let out: Vec<f32> = [0.0, 0.1, 0.3, 0.5].map(|p| whip.shift(p, false).x).to_vec();
        assert_eq!(out[0], 0.0);
        assert!(out[1] > out[2] && out[2] > out[3]);
        assert!((out[3] + 1.5).abs() < 1e-5);
        assert!(out[1] - out[2] < out[2] - out[3]);
        // In from the right, slowing to rest.
        let arriving: Vec<f32> = [0.5, 0.6, 0.8, 1.0].map(|p| whip.shift(p, true).x).to_vec();
        assert!((arriving[0] - 1.5).abs() < 1e-5 && arriving[3] == 0.0);
        assert!(arriving[0] > arriving[1] && arriving[1] > arriving[2]);
        // Upwards.
        let up = Transition { angle: 90.0, ..whip }.shift(0.5, false);
        assert!(up.x.abs() < 1e-5 && (up.y + 1.5).abs() < 1e-5);
    }

    #[test]
    fn slide_keeps_the_two_pictures_side_by_side() {
        let slide = Transition::of(TransitionKind::Slide);
        for p in [0.0, 0.3, 0.5, 0.9, 1.0] {
            let (a, b) = (slide.shift(p, false), slide.shift(p, true));
            // Both move left; the incoming one is one frame width behind.
            assert!((b.x - a.x - 1.0).abs() < 1e-5, "{p}: {a:?} {b:?}");
        }
        assert_eq!(slide.shift(0.0, false).x, 0.0);
        assert!((slide.shift(1.0, true).x).abs() < 1e-6);
    }

    #[test]
    fn zoom_grows_into_the_cut_and_settles_after_it() {
        let zoom = Transition::of(TransitionKind::Zoom);
        assert_eq!(zoom.shift(0.0, false).zoom, 1.0);
        assert!((zoom.shift(0.5, false).zoom - 1.6).abs() < 1e-5);
        assert!((zoom.shift(0.5, true).zoom - 1.6).abs() < 1e-5);
        assert_eq!(zoom.shift(1.0, true).zoom, 1.0);
    }

    #[test]
    fn stored_values_are_brought_into_range() {
        let wild = Transition { frames: 7, blur: 9.0, amount: f32::NAN, ease: 0.0, ..Transition::default() }.clamped();
        assert_eq!((wild.frames, wild.blur, wild.amount, wild.ease), (6, 2.0, 1.5, 1.0));
        // Old files without the field read as "no transition".
        let props: ClipProps = serde_json::from_str("{}").unwrap();
        assert_eq!(props.transition, None);
        let json = serde_json::to_string(&ClipProps::default()).unwrap();
        assert!(!json.contains("transition"));
    }
}

#[cfg(test)]
mod ramp_tests {
    use super::*;

    #[test]
    fn the_area_under_a_ramp_is_the_source_position() {
        // Constant: plain multiplication.
        assert_eq!(Param::constant(0.5).integral(100), 50.0);
        // Held at 1 until frame 20, down to 0.25 at frame 60, held after.
        let ramp = Param { value: 1.0, keys: vec![Key::new(20, 1.0), Key::new(60, 0.25)] };
        assert_eq!(ramp.integral(10), 10.0);
        assert_eq!(ramp.integral(20), 20.0);
        // Halfway down the slope the speed is 0.625: 20 frames at a mean of 0.8125.
        assert!((ramp.integral(40) - (20.0 + 20.0 * 0.8125)).abs() < 1e-6);
        assert!((ramp.integral(60) - (20.0 + 40.0 * 0.625)).abs() < 1e-6);
        assert!((ramp.integral(100) - (45.0 + 40.0 * 0.25)).abs() < 1e-6);
        assert_eq!(ramp.integral(-8), -8.0);
        // It never runs backwards while the speed is positive.
        let mut last = -1.0;
        for frame in 0..120 {
            assert!(ramp.integral(frame) > last);
            last = ramp.integral(frame);
        }
    }

    #[test]
    fn a_ramp_scales_the_set_speed_and_no_ramp_stays_exact() {
        let mut props = ClipProps { speed: (2, 1), ..ClipProps::default() };
        assert_eq!(props.source_offset(7), 14);
        props.ramp = Param { value: 1.0, keys: vec![Key::new(0, 1.0), Key::new(100, 0.0)] };
        // After 100 frames at a mean of half the doubled speed: 100 source frames.
        assert_eq!(props.source_offset(100), 100);
        assert_eq!(props.source_offset(50), 75);
        // Without a ramp the field is not written to files at all.
        assert!(!serde_json::to_string(&ClipProps::default()).unwrap().contains("ramp"));
    }
    #[test]
    fn easing_shapes_the_way_between_keyframes() {
        let mut p = Param { value: 0.0, keys: vec![Key::new(0, 0.0), Key::new(100, 10.0), Key::new(200, 0.0)] };
        assert_eq!(p.at(25), 2.5);
        assert!(p.set_ease(50, Ease::Smooth));
        assert!(!p.set_ease(50, Ease::Smooth));
        assert_eq!(p.ease_at(50), Some(Ease::Smooth));
        assert_eq!(p.ease_at(150), Some(Ease::Linear));
        assert_eq!(p.ease_at(200), None);
        // Gentle at both ends, the same in the middle, exact on the keyframes.
        assert!(p.at(10) < 0.5 && p.at(90) > 9.5);
        assert_eq!((p.at(0), p.at(50), p.at(100)), (0.0, 5.0, 10.0));
        // The second stretch is untouched.
        assert_eq!(p.at(150), 5.0);
        // A keyframe put into the smooth stretch continues it.
        p.set_key(40, p.at(40));
        assert_eq!(p.keys[1].ease, Ease::Smooth);
        assert!(p.set_ease(150, Ease::Hold));
        assert_eq!((p.at(100), p.at(199), p.at(200)), (10.0, 10.0, 0.0));
        assert!(!Param::constant(1.0).set_ease(5, Ease::Hold));
        // Text form: the plain way is not written, so old projects read as before.
        assert_eq!(serde_json::to_string(&Key::new(3, 1.0)).unwrap(), r#"{"frame":3,"value":1.0}"#);
        let held = Key { ease: Ease::Hold, ..Key::new(3, 1.0) };
        assert_eq!(serde_json::from_str::<Key>(&serde_json::to_string(&held).unwrap()).unwrap(), held);
    }

    #[test]
    fn the_integral_follows_the_easing() {
        for ease in Ease::ALL {
            let key = Key { ease, ..Key::new(10, 1.0) };
            let p = Param { value: 1.0, keys: vec![key, Key::new(70, 0.25), Key::new(90, 2.0)] };
            // Against a sum of the values frame by frame (midpoints would be exact for straight lines only).
            for to in [5, 10, 33, 70, 81, 120] {
                let fine: f64 = (0..to * 100)
                    .map(|i| {
                        let x = i as f64 / 100.0 + 0.005;
                        let (a, b) = (x.floor() as i64, x.floor() as i64 + 1);
                        let t = x - a as f64;
                        (p.at(a) as f64 * (1.0 - t) + p.at(b) as f64 * t) / 100.0
                    })
                    .sum();
                // Hold jumps within a frame, which the frame-wise sum smears: allow for that.
                let slack = if ease == Ease::Hold { 0.5 } else { 0.02 };
                assert!((p.integral(to) - fine).abs() < slack, "{ease:?} to {to}: {} against {fine}", p.integral(to));
            }
        }
    }
}

impl ClipProps {
    /// The clip's own colour trim as gains on red, green and blue; `None`
    /// if it has none.
    pub fn trim_gain(&self) -> Option<[f32; 3]> {
        if self.exposure == 0.0 && self.warmth == 0.0 {
            return None;
        }
        let (stops, warmth) = (self.exposure.clamp(-4.0, 4.0), self.warmth.clamp(-1.0, 1.0));
        let level = 2f32.powf(stops);
        // The same curve as the temperature knob of the Balance module.
        Some([level * (0.35 * warmth).exp(), level, level * (-0.35 * warmth).exp()])
    }
}
