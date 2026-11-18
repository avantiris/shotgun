//! The realtime side: what runs inside the audio callback.
//!
//! A [`Graph`] is built by the disk thread (allocation happens there) and
//! handed over through a single atomic slot; the callback adopts it at a
//! block boundary and parks the old one for the disk thread to free.

use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};

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

/// Track fader and pan (constant power).
pub struct GainPan {
    left: f32,
    right: f32,
}

impl GainPan {
    pub fn new(gain: f32, pan: f32) -> GainPan {
        let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        // Centre keeps unity gain on both sides.
        let norm = std::f32::consts::SQRT_2;
        GainPan { left: gain * angle.cos() * norm, right: gain * angle.sin() * norm }
    }
}

impl Processor for GainPan {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}

    fn process(&mut self, audio: &mut [f32], _transport: Transport) {
        for frame in audio.as_chunks_mut::<2>().0 {
            frame[0] *= self.left;
            frame[1] *= self.right;
        }
    }
}

pub struct Track {
    /// Interleaved stereo, filled by the disk thread from the graph's start
    /// position onwards.
    pub ring: rtrb::Consumer<f32>,
    pub chain: Vec<Box<dyn Processor>>,
    pub mute: bool,
}

pub struct Graph {
    pub tracks: Vec<Track>,
    pub master: Vec<Box<dyn Processor>>,
    /// Timeline sample the rings start at.
    pub start: i64,
    pub playing: bool,
}

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
}

impl Rt {
    pub(crate) fn new(shared: Arc<Shared>) -> (Rt, Arc<Slot>) {
        let slot = Arc::new(Slot { incoming: AtomicPtr::default(), retired: AtomicPtr::default() });
        let rt =
            Rt { shared, slot: slot.clone(), graph: None, position: 0, scratch: vec![0.0; MAX_BLOCK * 2], limbo: None };
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
            self.position = graph.start;
            self.shared.running.store(graph.playing, Ordering::Release);
            if let Some(old) = self.graph.replace(graph) {
                self.retire(old);
            }
            self.shared.adopted.fetch_add(1, Ordering::Release);
        }
        let frames = out.len() / 2;
        let playing = self.graph.as_ref().is_some_and(|g| g.playing);

        // Publish the clock before rendering: readers extrapolate from it.
        self.shared.seq.fetch_add(1, Ordering::AcqRel);
        self.shared.clock_pos.store(self.position, Ordering::Relaxed);
        self.shared.clock_ns.store(now_ns, Ordering::Relaxed);
        self.shared.clock_delay.store(delay, Ordering::Relaxed);
        self.shared.seq.fetch_add(1, Ordering::AcqRel);
        self.shared.quantum.store(frames as u32, Ordering::Relaxed);
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);

        out.fill(0.0);
        if let (true, Some(graph)) = (playing, self.graph.as_mut()) {
            let transport = Transport { position: self.position, playing };
            let mut dry = false;
            let len = out.len();
            for chunk_start in (0..len).step_by(MAX_BLOCK * 2) {
                let block = &mut out[chunk_start..(chunk_start + MAX_BLOCK * 2).min(len)];
                for track in &mut graph.tracks {
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
                    if track.mute {
                        continue;
                    }
                    for processor in &mut track.chain {
                        processor.process(scratch, transport);
                    }
                    for (o, s) in block.iter_mut().zip(scratch.iter()) {
                        *o += *s;
                    }
                }
                for processor in &mut graph.master {
                    processor.process(block, transport);
                }
            }
            if dry {
                self.shared.underruns.fetch_add(1, Ordering::Relaxed);
            }
            self.position += frames as i64;
        }
        let spent = crate::monotonic_ns().saturating_sub(started);
        self.shared.process_max_ns.fetch_max(spent, Ordering::Relaxed);
    }
}

impl Rt {
    /// The shared counters, for a backend that reports device state into them.
    pub(crate) fn shared_for_backend(&self) -> Arc<Shared> {
        self.shared.clone()
    }
}
