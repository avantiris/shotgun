//! Variable-frame-rate detection by reading every packet timestamp: the
//! only method that was right for every test file in spike S4 (2.3).

use std::path::Path;
use std::process::Command;

use crate::tool::{Cancel, ToolError, run_lines};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scan {
    pub vfr: bool,
    pub packets: u64,
    /// Frame intervals that differ from the usual one.
    pub irregular: u64,
}

/// Reads the whole file (about 1.3 GB/s from the page cache).
pub fn scan(path: &Path, cancel: &Cancel) -> Result<Scan, ToolError> {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts", "-of", "csv=p=0"]).arg(path);
    let mut pts = Vec::new();
    run_lines(cmd, cancel, |line| {
        if let Ok(value) = line.trim().trim_end_matches(',').parse::<i64>() {
            pts.push(value);
        }
    })?;
    Ok(analyse(&mut pts))
}

/// An interval is irregular if it differs from the median interval by more
/// than one time-base tick or 0.5 %, whichever is larger. One irregular
/// interval makes the file VFR.
pub fn analyse(pts: &mut [i64]) -> Scan {
    pts.sort_unstable();
    let mut deltas: Vec<i64> = pts.windows(2).map(|w| w[1] - w[0]).collect();
    let packets = pts.len() as u64;
    if deltas.is_empty() {
        return Scan { vfr: false, packets, irregular: 0 };
    }
    let mid = deltas.len() / 2;
    let median = *deltas.select_nth_unstable(mid).1;
    let tolerance = (median / 200).max(1);
    let irregular = deltas.iter().filter(|d| (**d - median).abs() > tolerance).count() as u64;
    Scan { vfr: irregular > 0, packets, irregular }
}

impl Scan {
    /// One line for the cache: `cfr 1799 0`.
    pub fn to_line(&self) -> String {
        format!("{} {} {}\n", if self.vfr { "vfr" } else { "cfr" }, self.packets, self.irregular)
    }

    pub fn from_line(line: &str) -> Option<Scan> {
        let mut parts = line.split_ascii_whitespace();
        let vfr = match parts.next()? {
            "vfr" => true,
            "cfr" => false,
            _ => return None,
        };
        Some(Scan { vfr, packets: parts.next()?.parse().ok()?, irregular: parts.next()?.parse().ok()? })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_rate_with_millisecond_rounding_is_cfr() {
        // 59.94 fps in a 1 ms time base: intervals of 16 and 17.
        let mut pts: Vec<i64> = (0..600).map(|i| i * 1001 / 60).collect();
        assert_eq!(analyse(&mut pts), Scan { vfr: false, packets: 600, irregular: 0 });
    }

    #[test]
    fn one_dropped_frame_is_vfr() {
        let mut pts: Vec<i64> = (0..600).filter(|i| *i != 300).map(|i| i * 1001).collect();
        let s = analyse(&mut pts);
        assert_eq!((s.vfr, s.irregular), (true, 1));
    }

    #[test]
    fn decode_order_does_not_matter() {
        // B-frames: packets arrive out of presentation order.
        let mut pts = vec![0, 3003, 1001, 2002, 6006, 4004, 5005];
        assert!(!analyse(&mut pts).vfr);
    }

    #[test]
    fn edge_cases_and_cache_line() {
        assert!(!analyse(&mut []).vfr);
        assert!(!analyse(&mut [5]).vfr);
        let s = Scan { vfr: true, packets: 10, irregular: 2 };
        assert_eq!(Scan::from_line(&s.to_line()), Some(s));
        assert_eq!(Scan::from_line("garbage"), None);
    }
}
