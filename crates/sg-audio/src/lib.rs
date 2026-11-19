//! The audio engine: one realtime mix for the editor now and the DAW later.
//!
//! Three kinds of thread take part (02-architecture.md):
//!
//! - the **realtime thread** (PipeWire's, or the caller's with the manual
//!   backend) runs [`graph::Rt::process`]: it only copies, scales and sums.
//!   No allocation, lock, log or syscall happens there;
//! - the **disk thread** reads conformed 48 kHz float WAV files and keeps a
//!   ring buffer per track filled ahead of the playhead;
//! - **control** calls ([`Engine::play`], [`Engine::seek`], ...) come from any
//!   other thread and never wait for the other two.
//!
//! The engine's sample counter is the master clock of playback:
//! [`Engine::position`] is the timeline sample being *heard* now, with the
//! output delay reported by the device taken off (spike S3, section 4).

mod disk;
mod fx;
pub mod graph;
mod null;
mod pw;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

pub use fx::Tone;
pub use graph::{Processor, Transport};

/// Internal rate. Everything on the timeline's audio tracks is in samples
/// at this rate.
pub const SAMPLE_RATE: u32 = 48_000;
/// Requested PipeWire quantum (06-audio.md).
pub const DEFAULT_QUANTUM: u32 = 256;
/// Tracks that have a level meter.
pub const MAX_METERS: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("cannot start the audio thread: {0}")]
    Thread(#[from] std::io::Error),
    #[error("PipeWire: {0}")]
    PipeWire(String),
}

/// A stretch of one conformed WAV file on a track.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipSpan {
    /// Timeline position in samples.
    pub start: i64,
    pub len: i64,
    /// Offset into the file in samples.
    pub src_in: i64,
    pub wav: PathBuf,
    /// Linear gain of this clip.
    pub gain: f32,
    /// Fade lengths in samples of timeline time.
    pub fade_in: i64,
    pub fade_out: i64,
    /// Playback speed as a fraction. Other than 1/1 the sound is resampled,
    /// so its pitch changes with the speed.
    pub speed: (u32, u32),
    /// Volume keyframes: (samples of timeline time since the clip's start,
    /// decibels), ascending. Empty: `gain` alone. Between two the level moves
    /// evenly in decibels; before the first and after the last it holds.
    pub gain_keys: Vec<(i64, f32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackMix {
    /// Sorted by start, not overlapping.
    pub clips: Vec<ClipSpan>,
    pub gain: f32,
    /// -1 (left) to 1 (right).
    pub pan: f32,
    pub mute: bool,
    /// Low cut in hertz; 0 is off.
    pub low_cut_hz: f32,
    /// Equaliser and compressor.
    pub tone: Tone,
    /// Noise reduction: the share of the treated sound, 0 (off) to 1.
    pub denoise: f32,
}

/// What to play: derived from the project snapshot by the caller.
#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    pub tracks: Vec<TrackMix>,
    /// Linear gain of the master fader.
    pub master_gain: f32,
}

impl Default for Mix {
    fn default() -> Mix {
        Mix { tracks: Vec::new(), master_gain: 1.0 }
    }
}

pub enum Backend {
    /// A PipeWire playback stream. `target` names a sink (for tests: a null
    /// sink); `None` lets the session manager pick the default output.
    PipeWire { target: Option<String>, quantum: u32 },
    /// No device; a thread keeps time and discards the sound.
    Null,
    /// No device: the caller pulls blocks with [`Engine::pull`]. For tests
    /// and for running without any audio output.
    Manual,
}

/// Numbers for the performance overlay.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Frames per callback as last seen (the graph quantum).
    pub quantum: u32,
    /// Output delay reported by the device, in frames.
    pub delay_frames: i64,
    /// Lost cycles reported by the graph clock.
    pub xruns: u64,
    /// Blocks in which a track's ring buffer ran dry.
    pub underruns: u64,
    pub process_max_ns: u64,
    pub callbacks: u64,
    /// Scheduling policy of the realtime thread (2 = SCHED_RR); 0 means it
    /// is not realtime and RTKit is probably missing.
    pub sched_policy: i32,
}

/// State shared between the realtime thread and everyone else, all atomics.
#[derive(Default)]
pub(crate) struct Shared {
    /// Sequence lock around the clock triple below: odd while being written.
    seq: AtomicU64,
    /// Timeline sample at the start of the last callback.
    clock_pos: AtomicI64,
    /// CLOCK_MONOTONIC nanoseconds of that callback's cycle.
    clock_ns: AtomicU64,
    clock_delay: AtomicI64,
    /// The realtime thread is advancing the position.
    running: AtomicBool,
    /// Position while not running (set by control calls).
    parked_pos: AtomicI64,
    quantum: AtomicU32,
    xruns: AtomicU64,
    underruns: AtomicU64,
    process_max_ns: AtomicU64,
    callbacks: AtomicU64,
    sched_policy: AtomicI64,
    /// Bumped by the realtime thread when it adopts a new graph.
    adopted: AtomicU64,
    /// Peak level per track since it was last read (f32 bits), then of the mix.
    meters: [AtomicU32; MAX_METERS],
    master_meter: AtomicU32,
    /// Lowest gain the limiter applied since it was last read (f32 bits; 1.0 when idle).
    limited: AtomicU32,
}

pub(crate) fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid timespec; CLOCK_MONOTONIC always exists. On
    // Linux this goes through the vDSO, not a syscall, so it is fine on the
    // realtime thread.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

pub struct Engine {
    control: crossbeam_channel::Sender<disk::Control>,
    shared: Arc<Shared>,
    disk_thread: Option<std::thread::JoinHandle<()>>,
    backend: Option<pw::PipeWire>,
    null: Option<null::Null>,
    /// With the manual backend, the realtime side lives here.
    manual: Option<std::sync::Mutex<graph::Rt>>,
}

impl Engine {
    pub fn start(backend: Backend) -> Result<Engine, AudioError> {
        let shared = Arc::new(Shared::default());
        shared.limited.store(1.0f32.to_bits(), Ordering::Relaxed);
        let (rt, slot) = graph::Rt::new(shared.clone());
        let (control, control_rx) = crossbeam_channel::unbounded();
        let (mut pipewire, mut null, mut manual) = (None, None, None);
        match backend {
            Backend::Manual => manual = Some(std::sync::Mutex::new(rt)),
            Backend::Null => null = Some(null::Null::start(rt, DEFAULT_QUANTUM)?),
            Backend::PipeWire { target, quantum } => pipewire = Some(pw::PipeWire::start(rt, target, quantum)?),
        }
        let activate = pipewire.as_ref().map(|p| p.activator()).or_else(|| null.as_ref().map(|n| n.activator()));
        let disk_shared = shared.clone();
        let disk_thread = std::thread::Builder::new()
            .name("sg-audio-disk".into())
            .spawn(move || disk::run(control_rx, slot, disk_shared, activate))?;
        Ok(Engine { control, shared, disk_thread: Some(disk_thread), backend: pipewire, null, manual })
    }

    fn send(&self, control: disk::Control) {
        // The disk thread lives as long as the engine.
        let _ = self.control.send(control);
    }

    /// Replaces what is played. Position and play state stay as they are.
    pub fn set_mix(&self, mix: Mix) {
        self.send(disk::Control::Mix(mix));
    }

    /// Starts playing from `pos` (timeline samples).
    pub fn play(&self, pos: i64) {
        self.shared.parked_pos.store(pos, Ordering::Relaxed);
        self.send(disk::Control::Play(pos));
    }

    /// Stops at the position being heard and returns it.
    pub fn pause(&self) -> i64 {
        let pos = self.position();
        self.shared.parked_pos.store(pos, Ordering::Relaxed);
        self.shared.running.store(false, Ordering::Relaxed);
        self.send(disk::Control::Pause(pos));
        pos
    }

    /// Moves the playhead while paused, or restarts playback from `pos`.
    pub fn seek(&self, pos: i64) {
        if self.is_playing() {
            self.play(pos);
        } else {
            self.shared.parked_pos.store(pos, Ordering::Relaxed);
        }
    }

    pub fn is_playing(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    /// The timeline sample being heard right now.
    pub fn position(&self) -> i64 {
        if !self.shared.running.load(Ordering::Relaxed) {
            return self.shared.parked_pos.load(Ordering::Relaxed);
        }
        loop {
            let before = self.shared.seq.load(Ordering::Acquire);
            let pos = self.shared.clock_pos.load(Ordering::Relaxed);
            let ns = self.shared.clock_ns.load(Ordering::Relaxed);
            let delay = self.shared.clock_delay.load(Ordering::Relaxed);
            if before.is_multiple_of(2) && self.shared.seq.load(Ordering::Acquire) == before {
                let elapsed = monotonic_ns().saturating_sub(ns) as i128;
                // Do not run ahead more than a short while if callbacks stall.
                let advanced = (elapsed * SAMPLE_RATE as i128 / 1_000_000_000).min(SAMPLE_RATE as i128 / 4) as i64;
                return pos + advanced - delay;
            }
            std::hint::spin_loop();
        }
    }

    pub fn stats(&self) -> Stats {
        let s = &self.shared;
        Stats {
            quantum: s.quantum.load(Ordering::Relaxed),
            delay_frames: s.clock_delay.load(Ordering::Relaxed),
            xruns: s.xruns.load(Ordering::Relaxed),
            underruns: s.underruns.load(Ordering::Relaxed),
            process_max_ns: s.process_max_ns.load(Ordering::Relaxed),
            callbacks: s.callbacks.load(Ordering::Relaxed),
            sched_policy: s.sched_policy.load(Ordering::Relaxed) as i32,
        }
    }

    /// Manual backend: renders the next block of interleaved stereo.
    pub fn pull(&self, out: &mut [f32]) {
        if let Some(rt) = &self.manual {
            let mut rt = rt.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rt.process(out, monotonic_ns(), 0);
        }
    }

    /// Blocks until the realtime side has taken over everything sent so far
    /// and its buffers are filled. For tests and offline use; with the manual
    /// backend the caller must keep pulling from another thread or call this
    /// before pulling.
    pub fn settle(&self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.send(disk::Control::Settle(tx));
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.send(disk::Control::Quit);
        if let Some(thread) = self.disk_thread.take() {
            let _ = thread.join();
        }
        // The PipeWire thread stops in its own drop.
        self.backend.take();
        self.null.take();
    }
}

impl Engine {
    /// Peak levels (linear, 1.0 is full scale) since the last call: one per
    /// track into `tracks`, the mix as the return value.
    pub fn take_meters(&self, tracks: &mut [f32]) -> f32 {
        self.meters().take(tracks)
    }
}

impl TrackMix {
    /// A track at unity gain, centred, with no clean-up.
    pub fn plain(clips: Vec<ClipSpan>) -> TrackMix {
        TrackMix { clips, gain: 1.0, pan: 0.0, mute: false, low_cut_hz: 0.0, denoise: 0.0, tone: Tone::default() }
    }
}

/// Reads the level meters from any thread without touching the engine.
#[derive(Clone)]
pub struct Meters {
    shared: Arc<Shared>,
}

impl Meters {
    /// Peak levels (linear, 1.0 is full scale) since the last call: one per
    /// track into `tracks`, the mix as the return value.
    pub fn take(&self, tracks: &mut [f32]) -> f32 {
        for (out, meter) in tracks.iter_mut().zip(&self.shared.meters) {
            *out = f32::from_bits(meter.swap(0, Ordering::Relaxed));
        }
        f32::from_bits(self.shared.master_meter.swap(0, Ordering::Relaxed))
    }

    /// How far the limiter on the master had to turn the mix down since
    /// the last call, in decibels (0: not at all).
    pub fn take_limiting(&self) -> f32 {
        let gain = f32::from_bits(self.shared.limited.swap(1.0f32.to_bits(), Ordering::Relaxed));
        if gain > 0.0 && gain < 1.0 { -20.0 * gain.log10() } else { 0.0 }
    }
}

impl Engine {
    pub fn meters(&self) -> Meters {
        Meters { shared: self.shared.clone() }
    }
}
