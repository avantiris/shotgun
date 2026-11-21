//! What one track's sound looks like along its chain: the level after
//! every stage, per short stretch of the timeline, for the picture the
//! mixer draws of a stage at work (feedback F16.17).
//!
//! The stages run on the disk thread half a second before their sound is
//! heard, so the readings are kept by timeline position, as the stage
//! readings are, and looked up around the position that is heard. Only
//! one track is watched at a time: the one whose chain is on screen.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// The points along the chain: the source, then after low cut, noise
/// reduction, tune, equaliser, de-esser, leveller, compressor and
/// saturation, and last what leaves for the fader (plugins and ducking done).
pub const SCOPE_TAPS: usize = 10;
/// Samples per stretch (5 ms: a syllable is a dozen of them).
pub const SCOPE_STEP: i64 = 256;
/// Stretches kept (11 s).
const SLOTS: usize = 2048;
/// Per slot: which stretch it holds, then a peak per tap (f32 bits).
const WIDTH: usize = 1 + SCOPE_TAPS;

pub(crate) struct Scope {
    /// The track that is watched (`usize::MAX`: none).
    track: AtomicUsize,
    slots: Box<[AtomicU32]>,
}

impl Default for Scope {
    fn default() -> Scope {
        Scope { track: AtomicUsize::new(usize::MAX), slots: (0..SLOTS * WIDTH).map(|_| AtomicU32::new(0)).collect() }
    }
}

impl Scope {
    pub(crate) fn watch(&self, track: Option<usize>) {
        self.track.store(track.unwrap_or(usize::MAX), Ordering::Relaxed);
    }

    pub(crate) fn watches(&self, track: usize) -> bool {
        self.track.load(Ordering::Relaxed) == track
    }

    /// The peaks of the stretch `step` at every tap; `None` if it was not
    /// laid down (or has been overwritten since).
    pub(crate) fn read(&self, step: i64) -> Option<[f32; SCOPE_TAPS]> {
        let base = step.rem_euclid(SLOTS as i64) as usize * WIDTH;
        (self.slots[base].load(Ordering::Relaxed) == tag(step))
            .then(|| std::array::from_fn(|tap| f32::from_bits(self.slots[base + 1 + tap].load(Ordering::Relaxed))))
    }
}

/// A stretch's mark in its slot (never 0, which is an empty slot).
fn tag(step: i64) -> u32 {
    (step as u32) | 1 << 31
}

/// The writing side, on the disk thread: where each tap got to, so that a
/// stretch played again (a loop, a jump back) is written anew, not merged
/// with what it was the last time.
pub struct Probe<'a> {
    pub(crate) scope: &'a Scope,
    pub(crate) last: &'a mut [i64; SCOPE_TAPS],
    /// Timeline sample of the first frame of the block as it went into the chain.
    pub(crate) at: i64,
}

impl Probe<'_> {
    /// Notes the level of `block` (interleaved stereo) at `tap`. `back` is
    /// how many frames the sound trails the chain's input by now.
    pub(crate) fn tap(&mut self, tap: usize, back: usize, block: &[f32]) {
        let start = self.at - back as i64;
        let frames = block.as_chunks::<2>().0;
        let mut n = 0;
        while n < frames.len() {
            let step = (start + n as i64).div_euclid(SCOPE_STEP);
            let until = (((step + 1) * SCOPE_STEP - start) as usize).min(frames.len());
            let mut peak = frames[n..until].iter().fold(0.0f32, |m, f| m.max(f[0].abs()).max(f[1].abs()));
            let base = step.rem_euclid(SLOTS as i64) as usize * WIDTH;
            if self.scope.slots[base].load(Ordering::Relaxed) != tag(step) {
                for other in 0..SCOPE_TAPS {
                    self.scope.slots[base + 1 + other].store(0, Ordering::Relaxed);
                }
                self.scope.slots[base].store(tag(step), Ordering::Relaxed);
            } else if self.last[tap] == step {
                peak = peak.max(f32::from_bits(self.scope.slots[base + 1 + tap].load(Ordering::Relaxed)));
            }
            self.scope.slots[base + 1 + tap].store(peak.to_bits(), Ordering::Relaxed);
            self.last[tap] = step;
            n = until;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tap_keeps_the_peak_of_each_stretch_and_a_replay_writes_it_anew() {
        let scope = Scope::default();
        let mut last = [i64::MIN; SCOPE_TAPS];
        // Two blocks that share a stretch: 300 frames at 0.2, then 300 at 0.5.
        let block = |level: f32| vec![level; 600];
        Probe { scope: &scope, last: &mut last, at: 0 }.tap(0, 0, &block(0.2));
        Probe { scope: &scope, last: &mut last, at: 300 }.tap(0, 0, &block(0.5));
        assert_eq!(scope.read(0).map(|taps| taps[0]), Some(0.2));
        assert_eq!(scope.read(1).map(|taps| taps[0]), Some(0.5));
        assert_eq!(scope.read(2).map(|taps| taps[0]), Some(0.5));
        assert_eq!(scope.read(3), None);
        // A later tap of the same sound, trailing by 256 frames, lands one stretch earlier.
        Probe { scope: &scope, last: &mut last, at: 512 }.tap(3, 256, &block(0.1));
        assert_eq!(scope.read(1).map(|taps| (taps[0], taps[3])), Some((0.5, 0.1)));
        // Played again, quieter: the old peak does not stay.
        Probe { scope: &scope, last: &mut last, at: 0 }.tap(0, 0, &block(0.05));
        assert_eq!(scope.read(0).map(|taps| taps[0]), Some(0.05));
        // A stretch a round of the slots later takes the slot over.
        Probe { scope: &scope, last: &mut last, at: SLOTS as i64 * SCOPE_STEP }.tap(0, 0, &block(0.3));
        assert_eq!(scope.read(0), None);
        assert_eq!(scope.read(SLOTS as i64).map(|taps| taps[0]), Some(0.3));
    }
}
