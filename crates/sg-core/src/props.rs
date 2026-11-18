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
}

/// A value that is either constant or follows keyframes (linear between
/// them, held before the first and after the last).
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
                let t = (frame - a.frame) as f32 / (b.frame - a.frame) as f32;
                a.value + (b.value - a.value) * t
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
            Err(i) => self.keys.insert(i, Key { frame, value }),
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
}

impl Default for Title {
    fn default() -> Title {
        Title { text: "Title".to_owned(), size: 0.12, color: [255, 255, 255, 255] }
    }
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
    /// Playback speed as a fraction: 2/1 is double speed.
    pub speed: (u32, u32),
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
            speed: (1, 1),
            title: None,
        }
    }
}

impl ClipProps {
    /// The animatable picture parameters, for "keyframe everything here".
    pub fn picture_params(&mut self) -> [&mut Param; 5] {
        [&mut self.x, &mut self.y, &mut self.scale, &mut self.rotation, &mut self.opacity]
    }

    pub fn speed_valid(&self) -> bool {
        self.speed.0 > 0 && self.speed.1 > 0
    }

    /// Source units consumed after `elapsed` timeline units at this speed.
    pub fn source_offset(&self, elapsed: i64) -> i64 {
        if !self.speed_valid() {
            return elapsed;
        }
        (elapsed as i128 * self.speed.0 as i128 / self.speed.1 as i128) as i64
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
        let keys: Vec<Key> = (0..=100)
            .map(|f| Key { frame: f, value: if f <= 50 { f as f32 } else { 50.0 - (f - 50) as f32 * 0.5 } })
            .collect();
        let simple = simplify(&keys, 0.01);
        assert_eq!(simple.iter().map(|k| k.frame).collect::<Vec<_>>(), [0, 50, 100]);
        // Noise below the tolerance disappears, the shape stays.
        let noisy: Vec<Key> =
            keys.iter().map(|k| Key { value: k.value + if k.frame % 2 == 0 { 0.05 } else { -0.05 }, ..*k }).collect();
        assert!(simplify(&noisy, 0.2).len() <= 5);
        assert_eq!(simplify(&keys[..2], 0.1).len(), 2);
    }
}
