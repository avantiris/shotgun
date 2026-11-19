//! Identifiers. All are written as fixed-width lowercase hex, in files and in
//! the journal, so they survive JSON (no 64-bit number precision loss) and
//! sort stably in diffs.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid id {0:?}")]
pub struct IdError(pub String);

macro_rules! hex_id {
    ($(#[$doc:meta])* $name:ident, $int:ty, $width:literal) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub $int);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!("{:0", $width, "x}"), self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({:0", $width, "x})"), self.0)
            }
        }

        impl FromStr for $name {
            type Err = IdError;
            fn from_str(s: &str) -> Result<Self, IdError> {
                if s.len() != $width || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(IdError(s.to_owned()));
                }
                <$int>::from_str_radix(s, 16).map($name).map_err(|_| IdError(s.to_owned()))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = std::borrow::Cow::<str>::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

hex_id!(ClipId, u64, 16);
hex_id!(TrackId, u64, 16);
hex_id!(SequenceId, u64, 16);
hex_id!(ProjectId, u128, 32);
hex_id!(ScenarioId, u64, 16);
hex_id!(
    /// First 16 bytes of the BLAKE3 hash of the asset's content.
    AssetId,
    u128,
    32
);

impl AssetId {
    /// The 8 hex digits used in asset file names.
    pub fn short(&self) -> String {
        format!("{:08x}", (self.0 >> 96) as u32)
    }
}

/// Id source. The core has no access to randomness, so the caller seeds it.
/// Ids are carried inside ops, which keeps replay deterministic.
#[derive(Clone, Debug)]
pub struct IdGen(u64);

impl IdGen {
    pub fn new(seed: u64) -> Self {
        IdGen(seed)
    }

    /// splitmix64; skips 0 so that 0 can never be a live id.
    pub fn next_u64(&mut self) -> u64 {
        loop {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            if z != 0 {
                return z;
            }
        }
    }

    pub fn clip(&mut self) -> ClipId {
        ClipId(self.next_u64())
    }

    pub fn scenario(&mut self) -> ScenarioId {
        ScenarioId(self.next_u64())
    }

    pub fn track(&mut self) -> TrackId {
        TrackId(self.next_u64())
    }

    pub fn sequence(&mut self) -> SequenceId {
        SequenceId(self.next_u64())
    }

    pub fn project(&mut self) -> ProjectId {
        ProjectId((self.next_u64() as u128) << 64 | self.next_u64() as u128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let id = ClipId(0xAB);
        assert_eq!(id.to_string(), "00000000000000ab");
        assert_eq!("00000000000000ab".parse::<ClipId>(), Ok(id));
        let a = AssetId(0x0123_4567_89ab_cdef_0000_0000_0000_0001);
        assert_eq!(a.to_string().parse::<AssetId>(), Ok(a));
        assert_eq!(a.short(), "01234567");
    }

    #[test]
    fn rejects_malformed() {
        assert!("ab".parse::<ClipId>().is_err());
        assert!("+0000000000000ab".parse::<ClipId>().is_err());
        assert!("zz000000000000ab".parse::<ClipId>().is_err());
    }

    #[test]
    fn json_is_a_string() {
        let json = serde_json::to_string(&TrackId(u64::MAX)).unwrap();
        assert_eq!(json, "\"ffffffffffffffff\"");
        assert_eq!(serde_json::from_str::<TrackId>(&json).unwrap(), TrackId(u64::MAX));
    }

    #[test]
    fn generator_is_deterministic_and_distinct() {
        let mut a = IdGen::new(7);
        let mut b = IdGen::new(7);
        let xs: Vec<u64> = (0..1000).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..1000).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        let set: std::collections::HashSet<_> = xs.iter().collect();
        assert_eq!(set.len(), 1000);
    }
}
