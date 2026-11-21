//! The realtime side: what runs inside the audio callback.
//!
//! A [`Graph`] is built by the disk thread (allocation happens there) and
//! handed over through a single atomic slot; the callback adopts it at a
//! block boundary and parks the old one for the disk thread to free.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use crate::Shared;

/// Largest block the graph can be asked for (PipeWire's quantum limit).
pub const MAX_BLOCK: usize = 8192;

/// Where the playhead is, for processors that care (tempo-synced effects later).
#[derive(Clone, Copy, Debug)]
pub struct Transport {
    /// Timeline sample of the first frame of this block.
    pub position: i64,
    pub playing: bool,
}

/// An audio effect or instrument. Built-in processors implement this, and a
/// hosted CLAP/VST3/LV2 plugin will be wrapped in it (06-audio.md).
pub trait Processor: Send {
    /// Called off the realtime thread before the first `process`.
    fn prepare(&mut self, sample_rate: u32, max_block: usize);
    /// Processes interleaved stereo in place. Realtime: no allocation, no
    /// locks, no syscalls.
    fn process(&mut self, audio: &mut [f32], transport: Transport);
    /// Delay this processor adds, in frames.
    fn latency(&self) -> u32 {
        0
    }
    /// Frames of output after the input ends (reverb tails).
    fn tail(&self) -> u32 {
        0
    }
}

/// Fader settings that change while the graph plays: written by the disk
/// thread, read by the realtime thread, no graph swap in between.
#[derive(Default)]
pub struct Live {
    gain: AtomicU32,
    pan: AtomicU32,
    mute: AtomicBool,
    /// Shares sent to the reverb and to the echo.
    sends: [AtomicU32; 2],
}

impl Live {
    pub fn new(gain: f32, pan: f32, mute: bool) -> Arc<Live> {
        let live = Arc::new(Live::default());
        live.set(gain, pan, mute);
        live
    }

    pub fn set(&self, gain: f32, pan: f32, mute: bool) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
        self.pan.store(pan.to_bits(), Ordering::Relaxed);
        self.mute.store(mute, Ordering::Relaxed);
    }

    pub fn set_sends(&self, reverb: f32, echo: f32) {
        self.sends[0].store(reverb.to_bits(), Ordering::Relaxed);
        self.sends[1].store(echo.to_bits(), Ordering::Relaxed);
    }

    fn sends(&self) -> [f32; 2] {
        [0, 1].map(|n| f32::from_bits(self.sends[n].load(Ordering::Relaxed)))
    }

    /// The fader as a factor, and whether the track is muted (for the disk
    /// thread's ducking: a muted voice ducks nothing).
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    pub fn is_muted(&self) -> bool {
        self.mute.load(Ordering::Relaxed)
    }

    /// Left and right factors (constant-power pan; centre is unity on both).
    fn factors(&self) -> [f32; 2] {
        if self.mute.load(Ordering::Relaxed) {
            return [0.0; 2];
        }
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        let pan = f32::from_bits(self.pan.load(Ordering::Relaxed));
        let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        let norm = std::f32::consts::SQRT_2;
        [gain * angle.cos() * norm, gain * angle.sin() * norm]
    }
}

/// Track or master fader with pan and mute. A change is spread over one
/// block so that it does not click.
pub struct Fader {
    live: Arc<Live>,
    now: [f32; 2],
}

impl Fader {
    pub fn new(live: Arc<Live>) -> Fader {
        let now = live.factors();
        Fader { live, now }
    }
}

impl Processor for Fader {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}

    fn process(&mut self, audio: &mut [f32], _transport: Transport) {
        let target = self.live.factors();
        let frames = audio.as_chunks_mut::<2>().0;
        let step = [
            (target[0] - self.now[0]) / frames.len().max(1) as f32,
            (target[1] - self.now[1]) / frames.len().max(1) as f32,
        ];
        for frame in frames {
            self.now[0] += step[0];
            self.now[1] += step[1];
            frame[0] *= self.now[0];
            frame[1] *= self.now[1];
        }
        self.now = target;
    }
}

/// Keeps the mix below full scale: when a peak would go over the ceiling
/// the level is turned down at once and comes back over a tenth of a
/// second. Always on the master, so that a loud mix distorts neither in
/// the room nor in the exported file; what it had to do is reported.
///
/// As a rule it looks at the samples themselves and lets full scale pass:
/// a song that is already mastered up to it plays untouched. For a master
/// made here ([`Limiter::true_peak`]) it looks at the wave between the
/// samples as well, which is what a converter or a lossy codec will
/// make of them, and holds that under a ceiling below full scale.
pub struct Limiter {
    shared: Arc<Shared>,
    gain: f32,
    ceiling: f32,
    /// With a true-peak ceiling: the last frames, newest last, and the
    /// weights that give the wave at each quarter between two samples.
    between: Option<Box<Between>>,
}

/// Full scale itself: a sound that is already mastered up to it passes
/// untouched; only what would clip is held back.
const CEILING: f32 = 1.0;
/// Samples each estimate between two samples is made from.
const TAPS: usize = 12;
/// How far behind the newest sample the estimate lies.
const CENTRE: usize = 6;
/// How long before a peak the level starts to come down (5 ms): turned
/// down that gently, the limiter does not crackle on a loud master.
const AHEAD: usize = 240;
/// A true-peak limiter is given a ceiling this much lower than asked:
/// twelve neighbours find the wave's peaks to about a tenth of a decibel.
const MARGIN_DB: f32 = 0.2;

struct Between {
    /// A ring of the last 16 frames.
    past: [[f32; 2]; 16],
    at: usize,
    weights: [[f32; TAPS]; 3],
    /// The sound on its way out, [`AHEAD`] frames behind the estimate,
    /// and the level each of those frames asks for at most.
    held: Vec<[f32; 2]>,
    wanted: Vec<f32>,
    /// The lowest of `wanted` at each of the last moments, and their sum:
    /// their mean is the level, which so reaches every low point in time
    /// and in a straight line.
    lows: Vec<f32>,
    sum: f64,
    slot: usize,
}

impl Between {
    fn new() -> Between {
        // The wave between samples is the sum of sinc curves through them;
        // twelve neighbours under a smooth window are enough to find its peaks.
        let mut weights = [[0.0f32; TAPS]; 3];
        for (phase, row) in weights.iter_mut().enumerate() {
            let shift = (phase + 1) as f32 / 4.0;
            for (k, weight) in row.iter_mut().enumerate() {
                let x = k as f32 - CENTRE as f32 + shift;
                let sinc =
                    if x.abs() < 1e-6 { 1.0 } else { (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x) };
                let window = 0.5 + 0.5 * (std::f32::consts::PI * x / (CENTRE as f32 + 0.5)).cos();
                *weight = sinc * window;
            }
        }
        Between {
            past: [[0.0; 2]; 16],
            at: 0,
            weights,
            held: vec![[0.0; 2]; AHEAD],
            wanted: vec![1.0; AHEAD],
            lows: vec![1.0; AHEAD],
            sum: AHEAD as f64,
            slot: 0,
        }
    }

    /// Takes in the frame the estimate was made for and the level it asks
    /// for at most; gives back the frame that leaves now and the level to
    /// give it.
    fn ahead(&mut self, frame: [f32; 2], wanted: f32) -> ([f32; 2], f32) {
        let slot = self.slot;
        let leaving = std::mem::replace(&mut self.held[slot], frame);
        self.wanted[slot] = wanted;
        let low = self.wanted.iter().fold(1.0f32, |m, w| m.min(*w));
        self.sum += low as f64 - self.lows[slot] as f64;
        self.lows[slot] = low;
        self.slot = (slot + 1) % AHEAD;
        // (Never above what the leaving frame itself asked for: rounding in the sum.)
        (leaving, (self.sum / AHEAD as f64) as f32)
    }

    /// Takes a frame in; gives back the frame the estimate is made for
    /// and the highest the wave gets around it.
    fn push(&mut self, frame: [f32; 2]) -> ([f32; 2], f32) {
        self.past[self.at] = frame;
        let back = |k: usize| self.past[(self.at + 16 - k) & 15];
        let mut peak = 0.0f32;
        for channel in 0..2 {
            peak = peak.max(back(CENTRE)[channel].abs());
            for row in &self.weights {
                let mut sum = 0.0f32;
                for (k, weight) in row.iter().enumerate() {
                    sum += back(k)[channel] * weight;
                }
                peak = peak.max(sum.abs());
            }
        }
        let centre = back(CENTRE);
        self.at = (self.at + 1) & 15;
        (centre, peak)
    }
}

impl Limiter {
    pub(crate) fn new(shared: Arc<Shared>) -> Limiter {
        Limiter { shared, gain: 1.0, ceiling: CEILING, between: None }
    }

    /// A limiter that holds the wave between the samples under
    /// `ceiling_db` (decibels below full scale, as -1.0). It hands the
    /// sound on [`Limiter::latency`] samples late.
    pub(crate) fn true_peak(shared: Arc<Shared>, ceiling_db: f32) -> Limiter {
        let ceiling = 10f32.powf((ceiling_db.min(0.0) - MARGIN_DB) / 20.0);
        Limiter { shared, gain: 1.0, ceiling, between: Some(Box::new(Between::new())) }
    }
}

impl Processor for Limiter {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}

    fn process(&mut self, audio: &mut [f32], _transport: Transport) {
        // Back to unity with a time constant of about 100 ms at 48 kHz.
        const RELEASE: f32 = 0.0002;
        let ceiling = self.ceiling;
        let mut lowest = 1.0f32;
        for frame in audio.as_chunks_mut::<2>().0 {
            let peak = match &mut self.between {
                Some(between) => {
                    let (centre, peak) = between.push(*frame);
                    *frame = centre;
                    peak
                }
                None => frame[0].abs().max(frame[1].abs()),
            };
            let needed = if peak > ceiling { ceiling / peak } else { 1.0 };
            self.gain = if needed < self.gain { needed } else { self.gain + (1.0 - self.gain) * RELEASE };
            let gain = match &mut self.between {
                // Seen ahead of time: the level is on its way down before the peak leaves.
                Some(between) => {
                    let (leaving, gain) = between.ahead(*frame, self.gain);
                    *frame = leaving;
                    gain
                }
                None => self.gain,
            };
            // (Clamped as well: rounding must not put a sample over the line.)
            frame[0] = (frame[0] * gain).clamp(-ceiling, ceiling);
            frame[1] = (frame[1] * gain).clamp(-ceiling, ceiling);
            lowest = lowest.min(gain);
        }
        // The strongest reduction since it was last read (a lock-free minimum).
        self.shared.limited.fetch_min(lowest.to_bits(), Ordering::Relaxed);
    }

    fn latency(&self) -> u32 {
        if self.between.is_some() { (CENTRE + AHEAD) as u32 } else { 0 }
    }
}

impl Limiter {
    /// Samples by which a true-peak limiter hands the sound on late.
    pub const TRUE_PEAK_DELAY: usize = CENTRE + AHEAD;

    #[cfg(test)]
    fn delay(&self) -> usize {
        self.latency() as usize
    }
}

pub struct Track {
    /// Interleaved stereo, filled by the disk thread from the graph's start
    /// position onwards.
    pub ring: rtrb::Consumer<f32>,
    pub chain: Vec<Box<dyn Processor>>,
    /// The track's live settings: its sends are read from them.
    pub live: Arc<Live>,
    /// Played by hand: heard also while nothing plays.
    pub by_hand: bool,
    /// 0: straight to the master. 1 to 4: through that group.
    pub group: u8,
}

/// A group of tracks on its way to the master: where they are summed, and its fader.
pub struct Group {
    pub sum: Vec<f32>,
    pub fader: Fader,
    pub live: Arc<Live>,
}

pub struct Graph {
    pub tracks: Vec<Track>,
    pub master: Vec<Box<dyn Processor>>,
    /// The shared reverb and echo, if any track sends to them.
    pub space: Option<crate::space::Space>,
    /// The metronome.
    pub click: Option<crate::space::Click>,
    /// Timeline sample the rings start at.
    pub start: i64,
    pub playing: bool,
    /// A grain for scrubbing: only this many frames are played, faded at
    /// both ends, and the clock stays where it is.
    pub grain: Option<usize>,
    /// Some track is played by hand.
    pub by_hand: bool,
    /// A stretch played over and over: at its end the position goes back to its start.
    pub loop_range: Option<(i64, i64)>,
    /// The groups some track goes through (`None`: no track names that one).
    pub groups: [Option<Group>; 4],
    /// Which graph this is, counted by the thread that builds them: the
    /// callback says which one it has, so that the builder knows whether
    /// its newest has been taken (one replaced before it was taken never is).
    pub serial: u64,
}

/// Frames over which a grain comes up and goes down (5 ms): no click.
const GRAIN_EDGE: usize = 240;

/// Single-slot hand-over of graphs between the disk thread and the callback.
pub struct Slot {
    /// A graph waiting to be adopted.
    incoming: AtomicPtr<Graph>,
    /// The graph the callback has finished with.
    retired: AtomicPtr<Graph>,
}

impl Slot {
    fn take(slot: &AtomicPtr<Graph>) -> Option<Box<Graph>> {
        let ptr = slot.swap(std::ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: every non-null pointer stored in a slot came from
        // `Box::into_raw` and the swap makes this side its only owner.
        (!ptr.is_null()).then(|| unsafe { Box::from_raw(ptr) })
    }

    fn put(slot: &AtomicPtr<Graph>, graph: Box<Graph>) -> Option<Box<Graph>> {
        let old = slot.swap(Box::into_raw(graph), Ordering::AcqRel);
        // SAFETY: as in `take`.
        (!old.is_null()).then(|| unsafe { Box::from_raw(old) })
    }

    /// Disk thread: offers a graph. A graph that was offered before and not
    /// yet adopted comes back to be freed here.
    pub fn offer(&self, graph: Box<Graph>) -> Option<Box<Graph>> {
        Slot::put(&self.incoming, graph)
    }

    /// Disk thread: collects a graph the callback is done with.
    pub fn collect(&self) -> Option<Box<Graph>> {
        Slot::take(&self.retired)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        Slot::take(&self.incoming);
        Slot::take(&self.retired);
    }
}

/// The realtime state. Owned by whoever calls `process`.
pub struct Rt {
    shared: Arc<Shared>,
    slot: Arc<Slot>,
    graph: Option<Box<Graph>>,
    /// Timeline sample of the next output frame.
    position: i64,
    scratch: Vec<f32>,
    /// A retired graph that could not be parked yet.
    limbo: Option<Box<Graph>>,
    /// Frames of the current grain already played.
    grain_done: usize,
    loudness: crate::loudness::ShortTerm,
    /// The monitored input, while there is one, and one that has ended
    /// and could not be parked yet.
    monitor: Option<Box<rtrb::Consumer<f32>>>,
    monitor_limbo: Option<Box<rtrb::Consumer<f32>>>,
    /// Peak of each side of the monitored input over the last seconds.
    monitor_sides: [f32; 2],
}

impl Rt {
    pub(crate) fn new(shared: Arc<Shared>) -> (Rt, Arc<Slot>) {
        let slot = Arc::new(Slot { incoming: AtomicPtr::default(), retired: AtomicPtr::default() });
        let rt = Rt {
            shared,
            slot: slot.clone(),
            graph: None,
            position: 0,
            scratch: vec![0.0; MAX_BLOCK * 2],
            limbo: None,
            grain_done: 0,
            loudness: crate::loudness::ShortTerm::default(),
            monitor: None,
            monitor_limbo: None,
            monitor_sides: [0.0; 2],
        };
        (rt, slot)
    }

    fn retire(&mut self, graph: Box<Graph>) {
        // The parking place holds one graph. If the disk thread has not
        // collected the previous one yet, keep this one until the next block
        // rather than freeing memory here.
        if self.slot.retired.load(Ordering::Acquire).is_null() {
            self.slot.retired.store(Box::into_raw(graph), Ordering::Release);
        } else {
            self.limbo = Some(graph);
        }
    }

    /// Renders one block of interleaved stereo. `now_ns` is the monotonic
    /// time of this cycle and `delay` the frames until the first sample
    /// written here is heard.
    pub fn process(&mut self, out: &mut [f32], now_ns: u64, delay: i64) {
        let started = crate::monotonic_ns();
        if let Some(graph) = self.limbo.take() {
            self.retire(graph);
        }
        if let Some(graph) = Slot::take(&self.slot.incoming) {
            // (A graph that goes on from where the last one was, a fader
            // or a clip having changed, keeps the meter's memory.)
            if graph.start != self.position {
                self.loudness.clear();
            }
            self.position = graph.start;
            self.grain_done = 0;
            self.shared.running.store(graph.playing && graph.grain.is_none(), Ordering::Release);
            if let Some(old) = self.graph.replace(graph) {
                self.retire(old);
            }
            self.shared.adopted.store(self.graph.as_ref().map_or(0, |g| g.serial), Ordering::Release);
        }
        let frames = out.len() / 2;
        let grain = self.graph.as_ref().and_then(|g| g.grain);
        let playing = self.graph.as_ref().is_some_and(|g| g.playing) && grain.is_none_or(|len| self.grain_done < len);

        // Publish the clock before rendering: readers extrapolate from it.
        self.shared.seq.fetch_add(1, Ordering::AcqRel);
        self.shared.clock_pos.store(self.position, Ordering::Relaxed);
        self.shared.clock_ns.store(now_ns, Ordering::Relaxed);
        self.shared.clock_delay.store(delay, Ordering::Relaxed);
        self.shared.seq.fetch_add(1, Ordering::AcqRel);
        self.shared.quantum.store(frames as u32, Ordering::Relaxed);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);

        out.fill(0.0);
        // While nothing plays, the tracks played by hand are still heard.
        let by_hand = self.graph.as_ref().is_some_and(|g| g.by_hand);
        if let (true, Some(graph)) = (playing || by_hand, self.graph.as_mut()) {
            let transport = Transport { position: self.position, playing };
            let mut dry = false;
            let len = out.len();
            for chunk_start in (0..len).step_by(MAX_BLOCK * 2) {
                let block = &mut out[chunk_start..(chunk_start + MAX_BLOCK * 2).min(len)];
                if let Some(space) = &mut graph.space {
                    space.follow();
                    space.to_reverb[..block.len()].fill(0.0);
                    space.to_echo[..block.len()].fill(0.0);
                }
                for group in graph.groups.iter_mut().flatten() {
                    group.sum[..block.len()].fill(0.0);
                }
                for (index, track) in graph.tracks.iter_mut().enumerate() {
                    if !playing && !track.by_hand {
                        continue;
                    }
                    let scratch = &mut self.scratch[..block.len()];
                    let available = track.ring.slots().min(block.len());
                    if let Ok(chunk) = track.ring.read_chunk(available) {
                        let (a, b) = chunk.as_slices();
                        scratch[..a.len()].copy_from_slice(a);
                        scratch[a.len()..a.len() + b.len()].copy_from_slice(b);
                        chunk.commit_all();
                    }
                    // The disk thread fell behind: the rest of the block is silence.
                    scratch[available..].fill(0.0);
                    dry |= available < block.len();
                    for processor in &mut track.chain {
                        processor.process(scratch, transport);
                    }
                    // To its group if it has one, else straight to the master.
                    let group = track.group.checked_sub(1).and_then(|g| graph.groups.get_mut(g as usize)?.as_mut());
                    // (What a grouped track sends to room and echo follows its group's fader.)
                    let through = group.as_ref().map_or(1.0, |g| if g.live.is_muted() { 0.0 } else { g.live.gain() });
                    let into: &mut [f32] = match group {
                        Some(group) => &mut group.sum[..block.len()],
                        None => &mut block[..],
                    };
                    let mut peak = 0.0f32;
                    for (o, s) in into.iter_mut().zip(scratch.iter()) {
                        *o += *s;
                        peak = peak.max(s.abs());
                    }
                    // Non-negative floats order like their bits: a lock-free maximum.
                    if let Some(meter) = self.shared.meters.get(index) {
                        meter.fetch_max(peak.to_bits(), Ordering::Relaxed);
                    }
                    // After the fader: a track that is turned down or muted sends less or nothing.
                    if let Some(space) = &mut graph.space {
                        let [reverb, echo] = track.live.sends();
                        for (bus, share) in
                            [(&mut space.to_reverb, reverb * through), (&mut space.to_echo, echo * through)]
                        {
                            if share > 0.0 {
                                for (b, s) in bus.iter_mut().zip(scratch.iter()) {
                                    *b += *s * share;
                                }
                            }
                        }
                    }
                }
                for group in graph.groups.iter_mut().flatten() {
                    let sum = &mut group.sum[..block.len()];
                    group.fader.process(sum, transport);
                    for (o, s) in block.iter_mut().zip(sum.iter()) {
                        *o += *s;
                    }
                }
                if let Some(space) = &mut graph.space {
                    let (reverb, echo) = (&mut space.to_reverb[..block.len()], &mut space.to_echo[..block.len()]);
                    space.reverb.process(reverb);
                    space.echo.process(echo);
                    for ((o, r), e) in block.iter_mut().zip(reverb.iter()).zip(echo.iter()) {
                        *o += *r + *e;
                    }
                }
                for processor in &mut graph.master {
                    processor.process(block, transport);
                }
                let peak = block.iter().fold(0.0f32, |m, s| m.max(s.abs()));
                self.shared.master_meter.fetch_max(peak.to_bits(), Ordering::Relaxed);
                if !playing {
                    continue;
                }
                if let (Some(loudness), None) = (self.loudness.add(block), grain) {
                    self.shared.loudness.store(loudness.to_bits(), Ordering::Relaxed);
                }
                // The click is not part of the mix: after the master, and not when scrubbing.
                if let (Some(click), None) = (&graph.click, grain) {
                    click.add(block, self.position + (chunk_start / 2) as i64);
                    for sample in block.iter_mut() {
                        *sample = sample.clamp(-1.0, 1.0);
                    }
                }
            }
            if !playing {
                // (Neither the clock nor the grain moves.)
            } else if let Some(len) = grain {
                for (i, frame) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let at = self.grain_done + i;
                    let left = len.saturating_sub(at);
                    let level = (at.min(left) as f32 / GRAIN_EDGE as f32).min(1.0);
                    frame[0] *= level;
                    frame[1] *= level;
                }
                self.grain_done += frames;
            } else if dry {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            if playing {
                let before = self.position;
                self.position += frames as i64;
                // The rings already hold the start of the loop after its end.
                if let Some((from, end)) = graph.loop_range
                    && before < end
                    && self.position >= end
                {
                    self.position = from + (self.position - end);
                }
            }
        }
        self.monitor_input(out);
        let spent = crate::monotonic_ns().saturating_sub(started);
        self.shared.process_max_ns.fetch_max(spent, Ordering::Relaxed);
    }
}

impl Rt {
    /// Measures the monitored input and, if it is to be heard, adds it to
    /// `out`. Nothing here allocates or frees: rings come and go through
    /// pointer slots, as graphs do.
    fn monitor_input(&mut self, out: &mut [f32]) {
        let park = |shared: &Shared, ring: Box<rtrb::Consumer<f32>>| -> Option<Box<rtrb::Consumer<f32>>> {
            if shared.monitor_out.load(Ordering::Acquire).is_null() {
                shared.monitor_out.store(Box::into_raw(ring), Ordering::Release);
                None
            } else {
                Some(ring)
            }
        };
        if let Some(ring) = self.monitor_limbo.take() {
            self.monitor_limbo = park(&self.shared, ring);
        }
        let incoming = self.shared.monitor_in.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !incoming.is_null() && self.monitor_limbo.is_none() {
            // SAFETY: a non-null pointer in the slot came from `Box::into_raw`
            // and the swap makes this side its only owner.
            let new = unsafe { Box::from_raw(incoming) };
            if let Some(old) = self.monitor.replace(new) {
                self.monitor_limbo = park(&self.shared, old);
            }
        } else if !incoming.is_null() {
            // No room to put the old one down this block: take the new one at the next.
            self.shared.monitor_in.store(incoming, Ordering::Release);
        }
        let Some(ring) = &mut self.monitor else { return };
        let waiting = ring.slots();
        if waiting == 0 {
            if ring.is_abandoned()
                && let Some(ended) = self.monitor.take()
            {
                self.monitor_limbo = park(&self.shared, ended);
            }
            return;
        }
        // What has piled up beyond two blocks is skipped: the input is
        // heard as late as it has to be and no later.
        let keep = (out.len() * 2).max(512);
        if waiting > keep
            && let Ok(old) = ring.read_chunk((waiting - out.len()) & !1)
        {
            old.commit_all();
        }
        let take = ring.slots().min(out.len()) & !1;
        let Ok(chunk) = ring.read_chunk(take) else { return };
        let heard = self.shared.monitor_heard.load(Ordering::Relaxed);
        let (a, b) = chunk.as_slices();
        let mut peak = 0.0f32;
        // An input that sounds on one side only is heard on both, as its take
        // will be: judged from the last seconds, so that it does not flip.
        let lone = crate::record::lone_side(self.monitor_sides);
        let mut sides = self.monitor_sides.map(|p| p * 0.999);
        let mut arriving = a.iter().chain(b);
        for frame in out.chunks_exact_mut(2) {
            let (Some(&left), Some(&right)) = (arriving.next(), arriving.next()) else { break };
            sides = [sides[0].max(left.abs()), sides[1].max(right.abs())];
            peak = peak.max(left.abs()).max(right.abs());
            let (left, right) = match lone {
                Some(0) => (left, left),
                Some(_) => (right, right),
                None => (left, right),
            };
            if heard {
                frame[0] = (frame[0] + left).clamp(-1.0, 1.0);
                frame[1] = (frame[1] + right).clamp(-1.0, 1.0);
            }
        }
        self.monitor_sides = sides;
        chunk.commit_all();
        self.shared.input_meter.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }

    /// The shared counters, for a backend that reports device state into them.
    pub(crate) fn shared_for_backend(&self) -> Arc<Shared> {
        self.shared.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// The highest the wave through `samples` (one channel) gets, found
    /// with many more neighbours and finer steps than the limiter uses.
    fn true_peak(samples: &[f32]) -> f32 {
        let mut peak = 0.0f32;
        for n in 64..samples.len() - 64 {
            for step in 0..16 {
                let t = step as f32 / 16.0;
                let mut sum = 0.0f32;
                for k in -48i32..=48 {
                    let x = t - k as f32;
                    let sinc = if x.abs() < 1e-6 {
                        1.0
                    } else {
                        (std::f32::consts::PI * x).sin() / (std::f32::consts::PI * x)
                    };
                    let window = 0.5 + 0.5 * (std::f32::consts::PI * x / 49.0).cos();
                    sum += samples[(n as i32 + k) as usize] * sinc * window;
                }
                peak = peak.max(sum.abs());
            }
        }
        peak
    }

    #[test]
    fn the_true_peak_limiter_holds_the_wave_between_the_samples() {
        // A quarter of the sample rate, sampled halfway up its crests: no
        // sample is above 0.99, the wave between them reaches 1.4.
        let tone = |n: usize| 1.4 * (std::f32::consts::FRAC_PI_2 * n as f32 + std::f32::consts::FRAC_PI_4).sin();
        let mut audio: Vec<f32> = (0..9600).flat_map(|n| [tone(n), tone(n)]).collect();
        let left = |audio: &[f32]| audio.iter().step_by(2).copied().collect::<Vec<f32>>();
        assert!(audio.iter().all(|s| s.abs() < 1.0));
        assert!(true_peak(&left(&audio)) > 1.35);
        let transport = Transport { position: 0, playing: true };
        // The plain limiter sees nothing to do.
        let mut plain = audio.clone();
        Limiter::new(Arc::new(Shared::default())).process(&mut plain, transport);
        assert_eq!(plain, audio);
        // The true-peak one holds the wave under -1 dB (0.891), a few milliseconds late.
        let mut limiter = Limiter::true_peak(Arc::new(Shared::default()), -1.0);
        let delay = limiter.delay();
        assert_eq!(delay, Limiter::TRUE_PEAK_DELAY);
        limiter.process(&mut audio, transport);
        let held = true_peak(&left(&audio)[1000..]);
        assert!(held <= 0.8913 && held > 0.8, "{held}");
        // A quiet sound passes as it is, only later.
        let quiet: Vec<f32> = (0..2000).flat_map(|n| [0.3 * (n as f32 * 0.05).sin(); 2]).collect();
        let mut passed = quiet.clone();
        Limiter::true_peak(Arc::new(Shared::default()), -1.0).process(&mut passed, transport);
        assert_eq!(passed[delay * 2..], quiet[..quiet.len() - delay * 2]);
        // A sudden loud stretch after quiet: the level is already down when it leaves,
        // and it came down gradually (no step in the quiet sound just before it).
        let mut sudden: Vec<f32> =
            (0..4800).flat_map(|n| [if n < 2400 { 0.2 } else { 1.6 } * (n as f32 * 0.3).sin(); 2]).collect();
        let original = sudden.clone();
        Limiter::true_peak(Arc::new(Shared::default()), -1.0).process(&mut sudden, transport);
        assert!(sudden.iter().all(|s| s.abs() <= 0.8913));
        let before = (2400 + delay - 100) * 2;
        let ratio = sudden[before] / original[before - delay * 2];
        assert!(ratio > 0.6 && ratio < 0.999, "{ratio}");
    }
}
