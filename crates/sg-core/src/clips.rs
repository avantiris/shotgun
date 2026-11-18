//! The clips of one track: sorted by start, never overlapping.
//!
//! Stored as a two-level tree of shared chunks so that an edit copies one
//! chunk (at most [`CHUNK_MAX`] clips) and a short vector of chunk references,
//! whatever the track length. Each chunk reference carries a `base` offset
//! that is added to the starts stored in the chunk; a ripple moves every later
//! chunk by changing its base and does not touch the clips.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ids::{AssetId, ClipId};

const CHUNK_MAX: usize = 512;
/// A chunk smaller than this is merged with a neighbour if both fit in one.
const CHUNK_MIN: usize = CHUNK_MAX / 4;

/// One clip on a track. `start` and `len` are in the track's unit (frames on
/// video tracks, samples on audio tracks); `src_in` is the offset into the
/// asset in the same unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub asset: AssetId,
    pub start: i64,
    pub len: i64,
    pub src_in: i64,
}

impl Clip {
    pub fn end(&self) -> i64 {
        self.start + self.len
    }
}

/// What [`ClipStore::visit`] reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visible {
    Clip(Clip),
    /// Several clips too short to tell apart at this zoom.
    Run {
        start: i64,
        end: i64,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum ClipError {
    #[error(
        "clip at {start} with length {len} is invalid (length must be positive, start and source offset not negative)"
    )]
    Invalid { start: i64, len: i64 },
    #[error("clip {start}..{end} overlaps another clip on the track")]
    Overlap { start: i64, end: i64 },
    #[error("no clip starts at {0}")]
    NotFound(i64),
    #[error("shifting by {delta} from {from} would move a clip before the track start or into its neighbour")]
    BadShift { from: i64, delta: i64 },
    #[error("clips are not sorted by start")]
    Unsorted,
}

#[derive(Clone)]
struct ChunkRef {
    base: i64,
    /// Absolute start of the first clip and end of the last, kept here so
    /// lookups do not touch the chunk memory.
    first: i64,
    end: i64,
    clips: Arc<Vec<Clip>>,
}

impl ChunkRef {
    /// `clips` holds starts relative to `base` and is not empty.
    fn new(base: i64, clips: Vec<Clip>) -> ChunkRef {
        let first = base + clips[0].start;
        let end = base + clips[clips.len() - 1].end();
        ChunkRef { base, first, end, clips: Arc::new(clips) }
    }

    fn abs(&self, i: usize) -> Clip {
        let c = self.clips[i];
        Clip { start: c.start + self.base, ..c }
    }
}

#[derive(Clone, Default)]
pub struct ClipStore {
    chunks: Vec<ChunkRef>,
    len: usize,
}

impl ClipStore {
    pub fn new() -> ClipStore {
        ClipStore::default()
    }

    /// Builds a store from clips already sorted by start.
    pub fn from_sorted(clips: impl IntoIterator<Item = Clip>) -> Result<ClipStore, ClipError> {
        let mut chunks = Vec::new();
        let mut cur: Vec<Clip> = Vec::new();
        let mut len = 0;
        let mut prev_end = 0;
        let mut prev_start = 0;
        // Fill to 3/4 so the first edits after loading do not split every chunk.
        let fill = CHUNK_MAX * 3 / 4;
        for c in clips {
            validate(&c)?;
            if len > 0 && c.start < prev_end {
                return Err(if c.start < prev_start {
                    ClipError::Unsorted
                } else {
                    ClipError::Overlap { start: c.start, end: c.end() }
                });
            }
            prev_end = c.end();
            prev_start = c.start;
            len += 1;
            cur.push(c);
            if cur.len() == fill {
                chunks.push(ChunkRef::new(0, std::mem::take(&mut cur)));
            }
        }
        if !cur.is_empty() {
            chunks.push(ChunkRef::new(0, cur));
        }
        Ok(ClipStore { chunks, len })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// End of the last clip, 0 for an empty track.
    pub fn end(&self) -> i64 {
        self.chunks.last().map_or(0, |c| c.end)
    }

    /// Index of the last chunk whose first clip starts at or before `t`
    /// (0 if `t` is before everything). Only valid on a non-empty store.
    fn chunk_for(&self, t: i64) -> usize {
        self.chunks.partition_point(|c| c.first <= t).saturating_sub(1)
    }

    /// The clip that starts exactly at `start`.
    pub fn get(&self, start: i64) -> Option<Clip> {
        if self.chunks.is_empty() {
            return None;
        }
        let ci = self.chunk_for(start);
        let chunk = &self.chunks[ci];
        let rel = start - chunk.base;
        let i = chunk.clips.binary_search_by_key(&rel, |c| c.start).ok()?;
        Some(chunk.abs(i))
    }

    /// The clip covering time `t` (`start <= t < end`).
    pub fn at(&self, t: i64) -> Option<Clip> {
        if self.chunks.is_empty() {
            return None;
        }
        let chunk = &self.chunks[self.chunk_for(t)];
        let rel = t - chunk.base;
        let i = chunk.clips.partition_point(|c| c.start <= rel).checked_sub(1)?;
        let clip = chunk.abs(i);
        (t < clip.end()).then_some(clip)
    }

    pub fn insert(&self, clip: Clip) -> Result<ClipStore, ClipError> {
        validate(&clip)?;
        let overlap = ClipError::Overlap { start: clip.start, end: clip.end() };
        let mut out = self.clone();
        out.len += 1;
        if self.chunks.is_empty() {
            out.chunks.push(ChunkRef::new(0, vec![clip]));
            return Ok(out);
        }
        let ci = self.chunk_for(clip.start);
        let chunk = &self.chunks[ci];
        let rel = clip.start - chunk.base;
        let i = chunk.clips.partition_point(|c| c.start < rel);
        // `chunk_for` picked the last chunk starting at or before the clip, so
        // the previous clip, if any, is in this chunk.
        if i > 0 && chunk.clips[i - 1].end() > rel {
            return Err(overlap);
        }
        let next_start = match chunk.clips.get(i) {
            Some(next) => Some(next.start + chunk.base),
            None => self.chunks.get(ci + 1).map(|c| c.first),
        };
        if next_start.is_some_and(|s| s < clip.end()) {
            return Err(overlap);
        }
        let mut clips = Vec::with_capacity(chunk.clips.len() + 1);
        clips.extend_from_slice(&chunk.clips[..i]);
        clips.push(Clip { start: rel, ..clip });
        clips.extend_from_slice(&chunk.clips[i..]);
        if clips.len() > CHUNK_MAX {
            let right = clips.split_off(clips.len() / 2);
            out.chunks[ci] = ChunkRef::new(chunk.base, clips);
            out.chunks.insert(ci + 1, ChunkRef::new(chunk.base, right));
        } else {
            out.chunks[ci] = ChunkRef::new(chunk.base, clips);
        }
        Ok(out)
    }

    /// Removes the clip that starts at `start` and returns it.
    pub fn remove(&self, start: i64) -> Result<(ClipStore, Clip), ClipError> {
        if self.chunks.is_empty() {
            return Err(ClipError::NotFound(start));
        }
        let ci = self.chunk_for(start);
        let chunk = &self.chunks[ci];
        let rel = start - chunk.base;
        let i = chunk.clips.binary_search_by_key(&rel, |c| c.start).map_err(|_| ClipError::NotFound(start))?;
        let removed = chunk.abs(i);
        let mut out = self.clone();
        out.len -= 1;
        if chunk.clips.len() == 1 {
            out.chunks.remove(ci);
            return Ok((out, removed));
        }
        let mut clips = Vec::with_capacity(chunk.clips.len() - 1);
        clips.extend_from_slice(&chunk.clips[..i]);
        clips.extend_from_slice(&chunk.clips[i + 1..]);
        // Keep chunks from decaying into many tiny ones after mass deletion.
        let fits = |n: usize| self.chunks.get(n).is_some_and(|o| clips.len() + o.clips.len() <= CHUNK_MAX);
        let neighbour = [ci + 1, ci.wrapping_sub(1)].into_iter().find(|&n| fits(n));
        match neighbour {
            Some(neighbour) if clips.len() < CHUNK_MIN => {
                let other = &self.chunks[neighbour];
                let rebase = other.base - chunk.base;
                let moved = other.clips.iter().map(|c| Clip { start: c.start + rebase, ..*c });
                if neighbour > ci {
                    clips.extend(moved);
                } else {
                    clips.splice(0..0, moved);
                }
                let lo = ci.min(neighbour);
                out.chunks[lo] = ChunkRef::new(chunk.base, clips);
                out.chunks.remove(lo + 1);
            }
            _ => out.chunks[ci] = ChunkRef::new(chunk.base, clips),
        }
        Ok((out, removed))
    }

    /// Moves every clip that starts at or after `from` by `delta` (ripple).
    /// Returns the new store and the old start of the first moved clip, or
    /// `None` if no clip was affected.
    pub fn shift_from(&self, from: i64, delta: i64) -> Result<(ClipStore, Option<i64>), ClipError> {
        if self.chunks.is_empty() || delta == 0 {
            return Ok((self.clone(), None));
        }
        let mut ci = self.chunk_for(from);
        let mut i = self.chunks[ci].clips.partition_point(|c| c.start < from - self.chunks[ci].base);
        if i == self.chunks[ci].clips.len() {
            ci += 1;
            i = 0;
        }
        let Some(chunk) = self.chunks.get(ci) else {
            return Ok((self.clone(), None));
        };
        let first_moved = chunk.abs(i).start;
        let prev_end = if i > 0 {
            chunk.abs(i - 1).end()
        } else if ci > 0 {
            self.chunks[ci - 1].end
        } else {
            0
        };
        if first_moved + delta < prev_end {
            return Err(ClipError::BadShift { from, delta });
        }
        let mut out = self.clone();
        let mut whole_from = ci;
        if i > 0 {
            // The boundary falls inside this chunk: copy it and move its tail.
            let mut clips = chunk.clips.as_ref().clone();
            for c in &mut clips[i..] {
                c.start += delta;
            }
            out.chunks[ci] = ChunkRef::new(chunk.base, clips);
            whole_from = ci + 1;
        }
        for c in &mut out.chunks[whole_from..] {
            c.base += delta;
            c.first += delta;
            c.end += delta;
        }
        Ok((out, Some(first_moved)))
    }

    /// Position (chunk, index) of the first clip that starts at or after `t`.
    fn seek_start(&self, t: i64) -> (usize, usize) {
        // The first chunk starting at or after `t`; the clip may still be in
        // the tail of the chunk before it.
        let ci = self.chunks.partition_point(|c| c.first < t);
        if let Some(prev) = ci.checked_sub(1).map(|p| &self.chunks[p]) {
            let i = prev.clips.partition_point(|c| c.start + prev.base < t);
            if i < prev.clips.len() {
                return (ci - 1, i);
            }
        }
        (ci, 0)
    }

    fn clip_at(&self, pos: (usize, usize)) -> Option<Clip> {
        let chunk = self.chunks.get(pos.0)?;
        (pos.1 < chunk.clips.len()).then(|| chunk.abs(pos.1))
    }

    fn step(&self, pos: (usize, usize)) -> (usize, usize) {
        match self.chunks.get(pos.0) {
            Some(chunk) if pos.1 + 1 < chunk.clips.len() => (pos.0, pos.1 + 1),
            _ => (pos.0 + 1, 0),
        }
    }

    fn step_back(&self, pos: (usize, usize)) -> Option<(usize, usize)> {
        if pos.1 > 0 {
            return Some((pos.0, pos.1 - 1));
        }
        let ci = pos.0.checked_sub(1)?;
        Some((ci, self.chunks[ci].clips.len() - 1))
    }

    /// The first clip that starts at or after `t`.
    pub fn first_at_or_after(&self, t: i64) -> Option<Clip> {
        self.clip_at(self.seek_start(t))
    }

    /// The last clip that starts before `t`.
    pub fn last_before(&self, t: i64) -> Option<Clip> {
        self.clip_at(self.step_back(self.seek_start(t))?)
    }

    /// Visits what is visible of `t0..t1` at a zoom where clips shorter than
    /// `min_len` would be narrower than a couple of pixels: such clips are
    /// reported merged into runs of at most `min_len` each. The number of
    /// calls is therefore bounded by `(t1 - t0) / min_len` plus the number of
    /// clips that are wide enough to see, however many clips the track has;
    /// every skip is a binary search (spike S2).
    pub fn visit(&self, t0: i64, t1: i64, min_len: i64, mut f: impl FnMut(Visible)) {
        let min_len = min_len.max(1);
        // Start at the clip covering t0, if there is one.
        let mut pos = self.seek_start(t0);
        if let Some(before) = self.step_back(pos).filter(|p| self.clip_at(*p).is_some_and(|c| c.end() > t0)) {
            pos = before;
        }
        while let Some(clip) = self.clip_at(pos).filter(|c| c.start < t1) {
            if clip.len >= min_len {
                f(Visible::Clip(clip));
                pos = self.step(pos);
                continue;
            }
            // Everything that starts within one bucket becomes one run.
            let mut next = self.seek_start(clip.start + min_len);
            let mut last = self.step_back(next).and_then(|p| self.clip_at(p)).unwrap_or(clip);
            // A wide clip that merely starts inside the bucket is drawn on
            // its own in the next round.
            if last.start > clip.start
                && last.len >= min_len
                && let Some(back) = self.step_back(next)
            {
                next = back;
                last = self.step_back(next).and_then(|p| self.clip_at(p)).unwrap_or(clip);
            }
            f(Visible::Run { start: clip.start, end: last.end().max(clip.start + 1) });
            pos = next;
        }
    }

    /// All clips in order.
    pub fn iter(&self) -> impl Iterator<Item = Clip> + '_ {
        self.chunks.iter().flat_map(|ch| (0..ch.clips.len()).map(move |i| ch.abs(i)))
    }

    /// Clips that intersect `t0..t1`, in order. Cost: O(log n) plus the
    /// number of clips returned.
    pub fn range(&self, t0: i64, t1: i64) -> impl Iterator<Item = Clip> + '_ {
        let first_chunk = self.chunks.partition_point(|c| c.end <= t0);
        self.chunks[first_chunk..]
            .iter()
            .take_while(move |ch| ch.first < t1)
            .flat_map(move |ch| {
                let lo = ch.clips.partition_point(|c| c.end() + ch.base <= t0);
                (lo..ch.clips.len()).map(move |i| ch.abs(i))
            })
            .take_while(move |c| c.start < t1)
    }

    #[cfg(test)]
    fn check_invariants(&self) {
        let mut n = 0;
        let mut prev_end = 0;
        for ch in &self.chunks {
            assert!(!ch.clips.is_empty() && ch.clips.len() <= CHUNK_MAX);
            assert_eq!(ch.first, ch.abs(0).start);
            assert_eq!(ch.end, ch.abs(ch.clips.len() - 1).end());
            for i in 0..ch.clips.len() {
                let c = ch.abs(i);
                assert!(c.len > 0 && c.src_in >= 0);
                assert!(c.start >= prev_end, "overlap or disorder at {}", c.start);
                prev_end = c.end();
                n += 1;
            }
        }
        assert_eq!(n, self.len);
    }
}

fn validate(c: &Clip) -> Result<(), ClipError> {
    if c.len <= 0 || c.start < 0 || c.src_in < 0 || c.start.checked_add(c.len).is_none() {
        return Err(ClipError::Invalid { start: c.start, len: c.len });
    }
    Ok(())
}

/// Equality is about the clips, not about how they happen to be chunked.
impl PartialEq for ClipStore {
    fn eq(&self, other: &ClipStore) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl fmt::Debug for ClipStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;

    fn clip(start: i64, len: i64) -> Clip {
        Clip { id: ClipId((start as u64).wrapping_add(1)), asset: AssetId(7), start, len, src_in: 0 }
    }

    fn store(clips: &[(i64, i64)]) -> ClipStore {
        ClipStore::from_sorted(clips.iter().map(|&(s, l)| clip(s, l))).unwrap()
    }

    #[test]
    fn insert_keeps_order_and_rejects_overlap() {
        let s = ClipStore::new().insert(clip(10, 5)).unwrap().insert(clip(0, 10)).unwrap();
        assert_eq!(s.iter().map(|c| c.start).collect::<Vec<_>>(), [0, 10]);
        assert!(matches!(s.insert(clip(14, 2)), Err(ClipError::Overlap { .. })));
        assert!(matches!(s.insert(clip(10, 1)), Err(ClipError::Overlap { .. })));
        assert!(matches!(s.insert(clip(20, 0)), Err(ClipError::Invalid { .. })));
        assert!(matches!(s.insert(clip(-1, 1)), Err(ClipError::Invalid { .. })));
        assert_eq!(s.insert(clip(15, 1)).unwrap().len(), 3);
    }

    #[test]
    fn lookup() {
        let s = store(&[(0, 10), (10, 5), (20, 5)]);
        assert_eq!(s.get(10).unwrap().len, 5);
        assert_eq!(s.get(11), None);
        assert_eq!(s.at(14).unwrap().start, 10);
        assert_eq!(s.at(15), None);
        assert_eq!(s.at(24).unwrap().start, 20);
        assert_eq!(s.at(25), None);
        assert_eq!(s.end(), 25);
        assert_eq!(ClipStore::new().at(0), None);
    }

    #[test]
    fn range_returns_intersecting_clips() {
        let s = store(&[(0, 10), (10, 5), (20, 5)]);
        let starts = |a, b| s.range(a, b).map(|c| c.start).collect::<Vec<_>>();
        assert_eq!(starts(0, 100), [0, 10, 20]);
        assert_eq!(starts(9, 10), [0]);
        assert_eq!(starts(10, 11), [10]);
        assert_eq!(starts(15, 20), Vec::<i64>::new());
        assert_eq!(starts(14, 21), [10, 20]);
    }

    #[test]
    fn remove_returns_the_clip() {
        let s = store(&[(0, 10), (10, 5)]);
        let (s2, c) = s.remove(10).unwrap();
        assert_eq!((c.start, c.len), (10, 5));
        assert_eq!(s2.len(), 1);
        assert_eq!(s.len(), 2, "the old snapshot is untouched");
        assert_eq!(s2.remove(10).unwrap_err(), ClipError::NotFound(10));
    }

    #[test]
    fn shift_ripples_later_clips_only() {
        let s = store(&[(0, 10), (20, 5), (30, 5)]);
        let (s2, first) = s.shift_from(20, -10).unwrap();
        assert_eq!(first, Some(20));
        assert_eq!(s2.iter().map(|c| c.start).collect::<Vec<_>>(), [0, 10, 20]);
        assert!(matches!(s.shift_from(20, -11), Err(ClipError::BadShift { .. })));
        assert_eq!(s.shift_from(31, 5).unwrap().1, None);
        let (s3, _) = s.shift_from(0, 7).unwrap();
        assert_eq!(s3.iter().map(|c| c.start).collect::<Vec<_>>(), [7, 27, 37]);
        assert!(matches!(s.shift_from(0, -1), Err(ClipError::BadShift { .. })));
    }

    #[test]
    fn from_sorted_rejects_bad_input() {
        assert_eq!(ClipStore::from_sorted([clip(10, 5), clip(0, 5)]).unwrap_err(), ClipError::Unsorted);
        assert!(matches!(ClipStore::from_sorted([clip(0, 5), clip(4, 5)]), Err(ClipError::Overlap { .. })));
    }

    #[test]
    fn many_clips_split_and_merge_chunks() {
        let n = 5000;
        let mut s = ClipStore::new();
        // Insert in a scrambled order to exercise splits everywhere.
        for k in 0..n {
            let i = (k * 2654435761u64 as i64).rem_euclid(n);
            if s.get(i * 10).is_none() {
                s = s.insert(clip(i * 10, 10)).unwrap();
            }
        }
        for i in 0..n {
            if s.get(i * 10).is_none() {
                s = s.insert(clip(i * 10, 10)).unwrap();
            }
        }
        s.check_invariants();
        assert_eq!(s.len(), n as usize);
        assert!(s.chunks.len() >= n as usize / CHUNK_MAX);
        for i in (0..n).filter(|i| i % 50 != 0) {
            s = s.remove(i * 10).unwrap().0;
        }
        s.check_invariants();
        assert_eq!(s.len(), 100);
        assert!(s.chunks.len() <= 2, "chunks merged, got {}", s.chunks.len());
    }

    #[test]
    fn edits_share_untouched_chunks() {
        let s = store(&(0..5000).map(|i| (i * 10, 10)).collect::<Vec<_>>());
        let s2 = s.insert(clip(50_000, 10)).unwrap();
        let shared = s.chunks.iter().zip(&s2.chunks).filter(|(a, b)| Arc::ptr_eq(&a.clips, &b.clips)).count();
        assert_eq!(shared, s.chunks.len() - 1);
        // A ripple from the very start copies nothing.
        let (s3, _) = s.shift_from(0, 5).unwrap();
        assert!(s.chunks.iter().zip(&s3.chunks).all(|(a, b)| Arc::ptr_eq(&a.clips, &b.clips)));
        s3.check_invariants();
    }

    #[derive(Debug, Clone)]
    enum Edit {
        Insert(i64, i64),
        Remove(usize),
        Shift(i64, i64),
    }

    fn edit() -> impl Strategy<Value = Edit> {
        prop_oneof![
            4 => (0i64..4000, 1i64..40).prop_map(|(s, l)| Edit::Insert(s, l)),
            2 => any::<usize>().prop_map(Edit::Remove),
            1 => (0i64..4000, -60i64..60).prop_map(|(f, d)| Edit::Shift(f, d)),
        ]
    }

    /// Reference model: a sorted map from start to length.
    fn model_insert(m: &mut BTreeMap<i64, i64>, s: i64, l: i64) -> bool {
        let prev_ok = m.range(..=s).next_back().is_none_or(|(ps, pl)| ps + pl <= s);
        let next_ok = m.range(s..).next().is_none_or(|(ns, _)| *ns >= s + l);
        if prev_ok && next_ok {
            m.insert(s, l);
        }
        prev_ok && next_ok
    }

    fn model_shift(m: &mut BTreeMap<i64, i64>, from: i64, delta: i64) -> bool {
        let Some((&first, _)) = m.range(from..).next() else { return true };
        let prev_end = m.range(..first).next_back().map_or(0, |(s, l)| s + l);
        if first + delta < prev_end {
            return false;
        }
        let tail = m.split_off(&first);
        m.extend(tail.into_iter().map(|(s, l)| (s + delta, l)));
        true
    }

    proptest! {
        #[test]
        fn behaves_like_a_sorted_map(edits in prop::collection::vec(edit(), 1..400)) {
            let mut s = ClipStore::new();
            let mut m = BTreeMap::new();
            for e in edits {
                match e {
                    Edit::Insert(start, len) => {
                        let ok = model_insert(&mut m, start, len);
                        match s.insert(clip(start, len)) {
                            Ok(n) => { prop_assert!(ok); s = n; }
                            Err(_) => prop_assert!(!ok),
                        }
                    }
                    Edit::Remove(k) => {
                        if m.is_empty() { continue; }
                        let start = *m.keys().nth(k % m.len()).unwrap();
                        m.remove(&start);
                        let (n, c) = s.remove(start).unwrap();
                        prop_assert_eq!(c.start, start);
                        s = n;
                    }
                    Edit::Shift(from, delta) => {
                        let ok = model_shift(&mut m, from, delta);
                        match s.shift_from(from, delta) {
                            Ok((n, _)) => { prop_assert!(ok); s = n; }
                            Err(_) => prop_assert!(!ok),
                        }
                    }
                }
                s.check_invariants();
            }
            let got: Vec<(i64, i64)> = s.iter().map(|c| (c.start, c.len)).collect();
            let want: Vec<(i64, i64)> = m.into_iter().collect();
            prop_assert_eq!(&got, &want);
            for &(start, len) in &want {
                prop_assert_eq!(s.get(start).map(|c| c.len), Some(len));
                prop_assert_eq!(s.at(start + len - 1).map(|c| c.start), Some(start));
            }
            let ranged: Vec<i64> = s.range(1000, 2000).map(|c| c.start).collect();
            let expect: Vec<i64> = want.iter().filter(|(s, l)| s + l > 1000 && *s < 2000).map(|c| c.0).collect();
            prop_assert_eq!(ranged, expect);
        }
    }
}

#[cfg(test)]
mod visit_tests {
    use proptest::prelude::*;

    use super::*;

    fn clip(start: i64, len: i64) -> Clip {
        Clip { id: ClipId(start as u64 + 1), asset: AssetId(1), start, len, src_in: 0 }
    }

    #[test]
    fn edges() {
        let s = ClipStore::from_sorted([clip(0, 10), clip(10, 5), clip(40, 5)]).unwrap();
        assert_eq!(s.first_at_or_after(0).map(|c| c.start), Some(0));
        assert_eq!(s.first_at_or_after(11).map(|c| c.start), Some(40));
        assert_eq!(s.first_at_or_after(41), None);
        assert_eq!(s.last_before(0), None);
        assert_eq!(s.last_before(10).map(|c| c.start), Some(0));
        assert_eq!(s.last_before(1000).map(|c| c.start), Some(40));
        assert_eq!(ClipStore::new().first_at_or_after(0), None);
    }

    #[test]
    fn wide_clips_are_reported_one_by_one() {
        let s = ClipStore::from_sorted([clip(0, 100), clip(100, 100), clip(300, 100)]).unwrap();
        let mut seen = Vec::new();
        s.visit(50, 350, 10, |v| seen.push(v));
        assert_eq!(seen, [Visible::Clip(clip(0, 100)), Visible::Clip(clip(100, 100)), Visible::Clip(clip(300, 100))]);
        seen.clear();
        s.visit(200, 300, 10, |v| seen.push(v));
        assert!(seen.is_empty());
    }

    #[test]
    fn short_clips_merge_and_a_wide_one_in_between_survives() {
        // Ten 1-long clips, then a wide clip, then ten more short ones.
        let mut clips: Vec<Clip> = (0..10).map(|i| clip(i * 2, 1)).collect();
        clips.push(clip(20, 500));
        clips.extend((0..10).map(|i| clip(520 + i * 2, 1)));
        let s = ClipStore::from_sorted(clips).unwrap();
        let mut seen = Vec::new();
        s.visit(0, 1000, 100, |v| seen.push(v));
        assert_eq!(
            seen,
            [Visible::Run { start: 0, end: 19 }, Visible::Clip(clip(20, 500)), Visible::Run { start: 520, end: 539 }]
        );
    }

    proptest! {
        /// Whatever the zoom: every visible clip is covered by exactly the
        /// items reported, in order, without overlap, and the number of
        /// items stays bounded by the window, not by the clip count.
        #[test]
        fn visit_covers_everything_once(
            lens in prop::collection::vec((1i64..40, 0i64..6), 1..3000),
            window in (0i64..20_000, 1i64..30_000),
            min_len in 1i64..400,
        ) {
            let mut t = 0;
            let clips: Vec<Clip> = lens.iter().map(|(len, gap)| { let c = clip(t + gap, *len); t = c.end(); c }).collect();
            let s = ClipStore::from_sorted(clips.clone()).unwrap();
            let (t0, t1) = (window.0, window.0 + window.1);
            let mut items = Vec::new();
            s.visit(t0, t1, min_len, |v| items.push(v));

            let span = |v: &Visible| match v { Visible::Clip(c) => (c.start, c.end()), Visible::Run { start, end } => (*start, *end) };
            for pair in items.windows(2) {
                prop_assert!(span(&pair[0]).1 <= span(&pair[1]).0, "items overlap or are out of order: {:?}", pair);
            }
            for c in clips.iter().filter(|c| c.end() > t0 && c.start < t1) {
                let covered = items.iter().any(|v| match v {
                    Visible::Clip(x) => x == c,
                    Visible::Run { start, end } => *start <= c.start && c.end() <= *end,
                });
                prop_assert!(covered, "clip {}..{} is not drawn", c.start, c.end());
            }
            for v in &items {
                if let Visible::Clip(c) = v {
                    prop_assert!(c.len >= min_len && c.end() > t0 && c.start < t1);
                }
            }
            // Each item advances by at least min_len, or is a wide clip.
            let wide = clips.iter().filter(|c| c.len >= min_len && c.end() > t0 && c.start < t1).count();
            prop_assert!(items.len() <= (window.1 / min_len) as usize + 2 * wide + 3, "{} items", items.len());
        }
    }
}
