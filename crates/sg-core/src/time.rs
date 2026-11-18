//! Time maths. Positions and lengths in the model are integers: frames at the
//! sequence rate on video tracks, samples on audio tracks. Floating-point
//! seconds never enter the model.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RateError {
    #[error("frame rate must have a non-zero numerator and denominator")]
    Zero,
    #[error("invalid frame rate {0:?} (expected e.g. 60/1 or 30000/1001)")]
    Parse(String),
}

/// A frame rate as an exact fraction, always stored reduced.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Rate {
    num: u32,
    den: u32,
}

impl Rate {
    pub const FPS_24: Rate = Rate { num: 24, den: 1 };
    pub const FPS_25: Rate = Rate { num: 25, den: 1 };
    pub const FPS_30: Rate = Rate { num: 30, den: 1 };
    pub const FPS_60: Rate = Rate { num: 60, den: 1 };

    pub fn new(num: u32, den: u32) -> Result<Rate, RateError> {
        if num == 0 || den == 0 {
            return Err(RateError::Zero);
        }
        let g = gcd(num, den);
        Ok(Rate { num: num / g, den: den / g })
    }

    pub fn num(self) -> u32 {
        self.num
    }

    pub fn den(self) -> u32 {
        self.den
    }

    /// Sample position at which frame `frames` starts (rounded down).
    pub fn frames_to_samples(self, frames: i64, sample_rate: u32) -> i64 {
        rescale_floor(frames, self.den as i64 * sample_rate as i64, self.num as i64)
    }

    /// The frame that is on screen at sample position `samples`.
    pub fn samples_to_frames(self, samples: i64, sample_rate: u32) -> i64 {
        rescale_floor(samples, self.num as i64, self.den as i64 * sample_rate as i64)
    }
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

impl FromStr for Rate {
    type Err = RateError;
    fn from_str(s: &str) -> Result<Rate, RateError> {
        let err = || RateError::Parse(s.to_owned());
        let (n, d) = s.split_once('/').ok_or_else(err)?;
        Rate::new(n.parse().map_err(|_| err())?, d.parse().map_err(|_| err())?)
    }
}

/// Written as `"60000/1001"`, in the journal and in asset index files.
impl Serialize for Rate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Rate, D::Error> {
        let s = std::borrow::Cow::<str>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `floor(v * num / den)` without overflow. `den` must be positive.
pub fn rescale_floor(v: i64, num: i64, den: i64) -> i64 {
    debug_assert!(den > 0);
    (v as i128 * num as i128).div_euclid(den as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduces_and_parses() {
        assert_eq!(Rate::new(120, 2).unwrap(), Rate::FPS_60);
        assert_eq!("30000/1001".parse::<Rate>().unwrap().to_string(), "30000/1001");
        assert_eq!(Rate::new(0, 1), Err(RateError::Zero));
        assert!("60".parse::<Rate>().is_err());
        assert!("a/b".parse::<Rate>().is_err());
    }

    #[test]
    fn frame_sample_conversion_is_exact() {
        assert_eq!(Rate::FPS_60.frames_to_samples(60, 48_000), 48_000);
        assert_eq!(Rate::FPS_60.frames_to_samples(1, 48_000), 800);
        let ntsc = Rate::new(30_000, 1001).unwrap();
        // 30000 frames of 29.97 fps are exactly 1001 seconds.
        assert_eq!(ntsc.frames_to_samples(30_000, 48_000), 1001 * 48_000);
        assert_eq!(ntsc.samples_to_frames(1001 * 48_000, 48_000), 30_000);
    }

    #[test]
    fn sample_to_frame_floors_also_below_zero() {
        assert_eq!(Rate::FPS_60.samples_to_frames(799, 48_000), 0);
        assert_eq!(Rate::FPS_60.samples_to_frames(800, 48_000), 1);
        assert_eq!(Rate::FPS_60.samples_to_frames(-1, 48_000), -1);
    }

    #[test]
    fn no_overflow_for_long_timelines() {
        // 100 hours at 60 fps in 192 kHz samples.
        let frames = 100 * 3600 * 60;
        assert_eq!(Rate::FPS_60.frames_to_samples(frames, 192_000), 100 * 3600 * 192_000);
        assert_eq!(rescale_floor(i64::MAX / 2, 2, 2), i64::MAX / 2);
    }

    #[test]
    fn frame_of_its_own_first_sample() {
        for rate in [Rate::FPS_24, Rate::FPS_25, Rate::new(30_000, 1001).unwrap(), Rate::FPS_60] {
            for f in [0i64, 1, 2, 999, 123_457] {
                let s = rate.frames_to_samples(f, 48_000);
                // Flooring the sample position may land one sample early.
                let back = rate.samples_to_frames(s + 1, 48_000);
                assert_eq!(back, f, "{rate} frame {f}");
            }
        }
    }
}
