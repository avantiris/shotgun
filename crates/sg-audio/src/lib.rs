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
pub mod loudness;
pub mod midi;
mod null;
mod pw;
pub mod record;
mod sampler;
mod scope;
pub mod space;
mod synth;
pub mod tune;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

pub use fx::{Tone, leveller_average, process_offline, response_db};
pub use graph::{Processor, Transport};
pub use scope::{SCOPE_STEP, SCOPE_TAPS, SPECTRUM_BANDS, SPECTRUM_FLOOR_DB, SPECTRUM_HZ};

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
    #[error("MIDI: {0}")]
    Midi(String),
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
    /// A changing speed: (samples of timeline time since the clip's start,
    /// samples of source consumed by then), ascending, straight lines
    /// between. Empty: `speed` alone. The sound is resampled along it.
    pub warp: Vec<(i64, f64)>,
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
    /// The plugins on the track, in order: effects after the track's own
    /// stages, and at most one instrument before them, playing `notes`.
    pub plugins: Vec<PluginSpec>,
    /// Sorted by start.
    pub notes: Vec<NoteSpan>,
    /// What the built-in instrument plays the notes with when the track
    /// has no instrument plugin: a recording, or (`None`) its own tone.
    pub sampler: Option<SamplerSpec>,
    /// Keys with a recording of their own (a drum kit): (key, conformed
    /// WAV file). Such a key plays its recording as it is, to its end.
    pub pads: Vec<(u8, PathBuf)>,
    /// Worked out as always (a voice still ducks the others) but not
    /// heard: how one track is exported alone, sounding as it does in the mix.
    pub unheard: bool,
    /// How much of the track (after its fader) goes to the shared reverb
    /// and to the shared echo, as factors; 0 is none.
    pub reverb: f32,
    pub echo: f32,
    /// The group the track goes through on its way to the master: 0 none,
    /// 1 to 4 one of [`Mix::groups`].
    pub group: u8,
    /// Played by hand ([`Engine::note`]): the track is buffered only a few
    /// milliseconds ahead instead of half a second, so that a note is
    /// heard when its key is pressed, and it sounds while nothing plays.
    pub by_hand: bool,
}

/// A recording for the built-in sampler of a track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SamplerSpec {
    /// A conformed WAV file (48 kHz float), as clips play.
    pub wav: PathBuf,
    /// The key at which it plays as it is.
    pub root: u8,
    /// Every note plays it to its end.
    pub one_shot: bool,
}

/// A plugin to run for a track, in a process of its own (crate `sg-plug`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginSpec {
    /// Which plugin of which track it is: the same key keeps the same running plugin.
    pub key: u64,
    /// The plugin's bundle on this machine.
    pub path: PathBuf,
    /// Where its state is kept between sessions.
    pub state_file: PathBuf,
}

/// A note of an instrument track, in timeline samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteSpan {
    pub start: i64,
    pub len: i64,
    pub pitch: u8,
    pub velocity: u8,
}

/// What to play: derived from the project snapshot by the caller.
#[derive(Clone, Debug, PartialEq)]
pub struct Mix {
    pub tracks: Vec<TrackMix>,
    /// Linear gain of the master fader.
    pub master_gain: f32,
    /// Samples between the repeats of the shared echo (a beat of the song).
    pub echo_samples: u32,
    /// The faders of the four groups: gain as a factor, and mute.
    pub groups: [(f32, bool); 4],
    /// The tempo of the song in beats per minute, which plugins are told
    /// (120 without one), and the timeline sample its first bar begins at.
    pub bpm: f64,
    pub bar_origin: i64,
    /// How long the shared room rings, in seconds, and how much of each
    /// repeat of the shared echo is repeated again (0 to 0.9).
    pub room_seconds: f32,
    pub echo_repeat: f32,
    /// No clip sounds from this timeline sample on (what rings goes on):
    /// the end of the range of a song that is exported with its tail.
    pub stop_at: Option<i64>,
    /// The metronome, while it is on. Never in an export.
    pub click: Option<space::Click>,
    /// For a master: the limiter holds the wave between the samples
    /// ("true peak") under this many decibels (as -1.0). `None`: it holds
    /// the samples themselves at full scale, as for everything else.
    pub true_peak_db: Option<f32>,
}

impl Default for Mix {
    fn default() -> Mix {
        Mix {
            tracks: Vec::new(),
            master_gain: 1.0,
            echo_samples: SAMPLE_RATE * 3 / 8,
            groups: [(1.0, false); 4],
            bpm: 120.0,
            bar_origin: 0,
            room_seconds: space::ROOM_SECONDS,
            echo_repeat: space::ECHO_REPEAT,
            stop_at: None,
            click: None,
            true_peak_db: None,
        }
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

/// The timeline sample being heard right now: only atomics are read, so
/// the realtime threads may ask too.
fn heard(shared: &Shared) -> i64 {
    if !shared.running.load(Ordering::Relaxed) {
        return shared.parked_pos.load(Ordering::Relaxed);
    }
    loop {
        let before = shared.seq.load(Ordering::Acquire);
        let pos = shared.clock_pos.load(Ordering::Relaxed);
        let ns = shared.clock_ns.load(Ordering::Relaxed);
        let delay = shared.clock_delay.load(Ordering::Relaxed);
        if before.is_multiple_of(2) && shared.seq.load(Ordering::Acquire) == before {
            let elapsed = monotonic_ns().saturating_sub(ns) as i128;
            // Do not run ahead more than a short while if callbacks stall.
            let advanced = (elapsed * SAMPLE_RATE as i128 / 1_000_000_000).min(SAMPLE_RATE as i128 / 4) as i64;
            return pos + advanced - delay;
        }
        std::hint::spin_loop();
    }
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
    /// Messages about plugins, from the disk thread to whoever shows them.
    /// (Never touched by the realtime thread.)
    pub(crate) plugin_news: std::sync::Mutex<Vec<String>>,
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
    /// Loudness of the last three seconds of the mix in LUFS (float bits).
    loudness: AtomicU32,
    /// Lowest gain the limiter applied since it was last read (f32 bits; 1.0 when idle).
    limited: AtomicU32,
    /// The ring a monitor of the input feeds, on its way to the realtime
    /// thread, and the one that thread has finished with (as graphs are
    /// handed over: a pointer swapped in, never a lock).
    pub(crate) monitor_in: std::sync::atomic::AtomicPtr<rtrb::Consumer<f32>>,
    pub(crate) monitor_out: std::sync::atomic::AtomicPtr<rtrb::Consumer<f32>>,
    /// The monitored input is added to what is heard.
    pub(crate) monitor_heard: AtomicBool,
    /// The stages the monitored input is heard through (those of the track
    /// that takes the recording), if any.
    pub(crate) monitor_chain: MonitorChain,
    /// Peak level of the input since it was last read (f32 bits).
    pub(crate) input_meter: AtomicU32,
    /// What the stages of each track did, along the timeline: they work
    /// half a second before their sound is heard, so each stretch of
    /// [`STAGE_STEP`] samples has a slot, read when its turn to be heard
    /// comes. A slot holds which stretch it is (upper half) and the
    /// reductions of de-esser, compressor and ducking in eighths of a
    /// decibel, a byte each.
    pub(crate) stages: Stages,
    /// The level along the chain of the one track whose chain is on screen.
    pub(crate) scope: scope::Scope,
}

/// Settings for the stages the monitored input goes through, handed to
/// the realtime thread without a lock: numbers in atomics behind a
/// counter that is odd while they are being written. The reader takes
/// them only when the counter is even and was the same before and after.
#[derive(Default)]
pub(crate) struct MonitorChain {
    seq: AtomicU32,
    /// 0: no chain. Then the low cut and the tone's numbers as f32 bits,
    /// "voice" as 0 or 1, and the order four stages to a word.
    values: [AtomicU32; 16],
}

impl MonitorChain {
    /// (One writer: the thread that owns the engine.)
    fn set(&self, chain: Option<(f32, Tone)>) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        let mut values = [0u32; 16];
        if let Some((low_cut_hz, tone)) = chain {
            let numbers = [
                low_cut_hz,
                tone.low_db,
                tone.mid_db,
                tone.mid_hz,
                tone.high_db,
                tone.threshold_db,
                tone.ratio,
                tone.makeup_db,
                tone.deess,
                tone.level,
                tone.level_ref_db,
                tone.drive,
            ];
            values[0] = 1;
            for (slot, number) in values[1..].iter_mut().zip(numbers) {
                *slot = number.to_bits();
            }
            values[13] = tone.voice as u32;
            values[14] = u32::from_le_bytes([tone.order[0], tone.order[1], tone.order[2], tone.order[3]]);
            values[15] = u32::from_le_bytes([tone.order[4], tone.order[5], tone.order[6], tone.order[7]]);
        }
        for (slot, value) in self.values.iter().zip(values) {
            slot.store(value, Ordering::Relaxed);
        }
        self.seq.fetch_add(1, Ordering::AcqRel);
    }

    /// What is set now, with the counter it was read at; `None` while it
    /// is being written (the reader keeps what it had).
    pub(crate) fn get(&self) -> Option<(u32, Option<(f32, Tone)>)> {
        let before = self.seq.load(Ordering::Acquire);
        if !before.is_multiple_of(2) {
            return None;
        }
        let values: [u32; 16] = std::array::from_fn(|n| self.values[n].load(Ordering::Relaxed));
        if self.seq.load(Ordering::Acquire) != before {
            return None;
        }
        let number = |n: usize| f32::from_bits(values[n]);
        let (first, second) = (values[14].to_le_bytes(), values[15].to_le_bytes());
        let chain = (values[0] == 1).then(|| {
            let tone = Tone {
                low_db: number(2),
                mid_db: number(3),
                mid_hz: number(4),
                high_db: number(5),
                threshold_db: number(6),
                ratio: number(7),
                makeup_db: number(8),
                deess: number(9),
                level: number(10),
                level_ref_db: number(11),
                drive: number(12),
                voice: values[13] == 1,
                order: [first[0], first[1], first[2], first[3], second[0], second[1], second[2], second[3]],
                ..Tone::default()
            };
            (number(1), tone)
        });
        Some((before, chain))
    }

    pub(crate) fn seq(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }
}

/// The slots of the stage readings: [`STAGE_SLOTS`] for each of [`MAX_METERS`] tracks.
pub(crate) struct Stages(Box<[AtomicU64]>);

impl Default for Stages {
    fn default() -> Stages {
        Stages((0..MAX_METERS * STAGE_SLOTS).map(|_| AtomicU64::new(0)).collect())
    }
}

impl Stages {
    /// The slot of `track` for the stretch `step`; `None` beyond the tracks that have meters.
    fn slot(&self, track: usize, step: i64) -> Option<&AtomicU64> {
        (track < MAX_METERS).then(|| &self.0[track * STAGE_SLOTS + step.rem_euclid(STAGE_SLOTS as i64) as usize])
    }
}

/// Samples per slot of the stage readings (43 ms), and how many slots a
/// track has (2.7 s: more than the half second the stages run ahead).
pub(crate) const STAGE_STEP: i64 = 2048;
pub(crate) const STAGE_SLOTS: usize = 64;

impl Shared {
    /// Notes what the stages of `track` did to the block of `frames`
    /// frames at timeline sample `start`: decibels of reduction, 0 or below.
    pub(crate) fn note_stages(&self, track: usize, start: i64, frames: usize, db: [f32; 3]) {
        let byte = |db: f32| ((-db).clamp(0.0, 31.0) * 8.0) as u64;
        let values = byte(db[0]) | byte(db[1]) << 8 | byte(db[2]) << 16;
        let (first, last) = (start.div_euclid(STAGE_STEP), (start + frames as i64 - 1).div_euclid(STAGE_STEP));
        for step in first..=last {
            let Some(slot) = self.stages.slot(track, step) else { return };
            let now = slot.load(Ordering::Relaxed);
            // A stretch two blocks touched keeps the stronger reading of each stage.
            let merged = if now >> 32 == step as u32 as u64 {
                (0..3).fold(0, |all, n| all | ((now >> (8 * n)) & 255).max((values >> (8 * n)) & 255) << (8 * n))
            } else {
                values
            };
            slot.store((step as u32 as u64) << 32 | merged, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod monitor_chain_tests {
    use super::*;

    #[test]
    fn the_chain_for_the_monitored_input_arrives_as_it_was_set() {
        let chain = MonitorChain::default();
        assert_eq!(chain.get().map(|(_, set)| set), Some(None));
        let tone = Tone {
            low_db: -2.0,
            mid_db: 3.5,
            mid_hz: 450.0,
            high_db: 1.0,
            threshold_db: -18.5,
            ratio: 4.0,
            makeup_db: 13.6,
            deess: 0.35,
            level: 0.6,
            level_ref_db: -30.2,
            drive: 0.25,
            voice: true,
            order: [0, 1, 2, 3, 4, 6, 5, 7],
            ..Tone::default()
        };
        let before = chain.seq();
        chain.set(Some((105.0, tone)));
        let (seq, set) = chain.get().unwrap();
        assert_ne!(seq, before);
        assert_eq!(set, Some((105.0, tone)));
        chain.set(None);
        assert_eq!(chain.get().map(|(_, set)| set), Some(None));
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        for slot in [&self.monitor_in, &self.monitor_out] {
            let ptr = slot.swap(std::ptr::null_mut(), Ordering::AcqRel);
            if !ptr.is_null() {
                // SAFETY: a non-null pointer in these slots came from `Box::into_raw`, and nobody else is left.
                drop(unsafe { Box::from_raw(ptr) });
            }
        }
    }
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
        shared.loudness.store((-120.0f32).to_bits(), Ordering::Relaxed);
        let (rt, slot) = graph::Rt::new(shared.clone());
        let (control, control_rx) = crossbeam_channel::unbounded();
        let (mut pipewire, mut null, mut manual) = (None, None, None);
        match backend {
            Backend::Manual => manual = Some(std::sync::Mutex::new(rt)),
            Backend::Null => null = Some(null::Null::start(rt, DEFAULT_QUANTUM)?),
            Backend::PipeWire { target, quantum } => pipewire = Some(pw::PipeWire::start(rt, target, quantum)?),
        }
        let activate = pipewire.as_ref().map(|p| p.activator()).or_else(|| null.as_ref().map(|n| n.activator()));
        let (disk_shared, loaded) = (shared.clone(), control.clone());
        let disk_thread = std::thread::Builder::new()
            .name("sg-audio-disk".into())
            .spawn(move || disk::run(control_rx, loaded, slot, disk_shared, activate))?;
        Ok(Engine { control, shared, disk_thread: Some(disk_thread), backend: pipewire, null, manual })
    }

    fn send(&self, control: disk::Control) {
        // The disk thread lives as long as the engine.
        let _ = self.control.send(control);
    }

    /// Shows the window of the plugin with this key, if it has one.
    pub fn open_plugin_editor(&self, key: u64) {
        self.send(disk::Control::Editor(key));
    }

    /// Has every plugin's state written to its file and waits for that
    /// (at most two seconds): before an export, which runs plugins of its own.
    pub fn save_plugin_states(&self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.send(disk::Control::SaveStates(tx));
        let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
    }

    /// What happened to plugins since the last call (one that did not
    /// load, one whose process died): messages for the user.
    pub fn plugin_news(&self) -> Vec<String> {
        std::mem::take(&mut *self.shared.plugin_news.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
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

    /// A key pressed or let go on the instrument of track `track` (its
    /// place in the mix), which must be [`TrackMix::by_hand`]: heard a few
    /// milliseconds later, playing or not.
    pub fn note(&self, track: usize, on: bool, pitch: u8, velocity: u8) {
        self.send(disk::Control::Note { track, on, pitch, velocity });
    }

    /// Plays the stretch between these timeline samples over and over
    /// once the playhead reaches its end (`None`: plays on as usual).
    pub fn set_loop(&self, range: Option<(i64, i64)>) {
        self.send(disk::Control::Loop(range));
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

    /// While paused: moves the playhead to `pos` and lets a short piece of
    /// the sound there be heard, for finding a place by ear while scrubbing.
    pub fn grain(&self, pos: i64) {
        // (Stored either way: just after a pause the flag below can lag a moment.)
        self.shared.parked_pos.store(pos, Ordering::Relaxed);
        if self.is_playing() {
            return;
        }
        self.send(disk::Control::Grain(pos));
    }

    pub fn is_playing(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    /// The timeline sample being heard right now.
    pub fn position(&self) -> i64 {
        heard(&self.shared)
    }

    /// Starts recording from `source` (a PipeWire node, `None` for the
    /// default input) into the file at `path`. Needs a sound device: the
    /// backends without one cannot record.
    pub fn record(&self, path: PathBuf, source: Option<String>) -> Result<record::Recording, AudioError> {
        if self.backend.is_none() {
            return Err(AudioError::PipeWire("there is no sound device to record from in this mode".into()));
        }
        let shared = self.shared.clone();
        let playing = move || shared.running.load(Ordering::Relaxed).then(|| heard(&shared));
        // The take's level goes to the input meter: it is seen while it is made.
        let meter = self.shared.clone();
        let level = move |peak: f32| {
            meter.input_meter.fetch_max(peak.to_bits(), Ordering::Relaxed);
        };
        record::Recording::start(path, source, Box::new(playing), Box::new(level))
    }

    /// Opens `source` (`None`: the default input) to see its level
    /// ([`Meters::take_input`]) and, with `hear`, to hear it with the mix,
    /// playing or not. Until the returned handle is dropped. Needs a sound device.
    pub fn monitor(&self, source: Option<String>, hear: bool) -> Result<record::Monitor, AudioError> {
        if self.backend.is_none() {
            return Err(AudioError::PipeWire("there is no sound device to listen to in this mode".into()));
        }
        // A sixth of a second of room: the playback side takes what there is every block.
        let (producer, consumer) = rtrb::RingBuffer::new(SAMPLE_RATE as usize / 3);
        let hand_over = |slot: &std::sync::atomic::AtomicPtr<rtrb::Consumer<f32>>, new: *mut rtrb::Consumer<f32>| {
            let old = slot.swap(new, Ordering::AcqRel);
            if !old.is_null() {
                // SAFETY: a non-null pointer in these slots came from
                // `Box::into_raw`, and the swap makes this side its only owner.
                drop(unsafe { Box::from_raw(old) });
            }
        };
        // (Frees one the realtime thread has finished with, and one it never took.)
        hand_over(&self.shared.monitor_out, std::ptr::null_mut());
        hand_over(&self.shared.monitor_in, Box::into_raw(Box::new(consumer)));
        self.shared.monitor_heard.store(hear, Ordering::Relaxed);
        let mut monitor = record::Monitor::start(producer, source)?;
        monitor.control = Some(self.control.clone());
        self.send(disk::Control::Monitor(true));
        Ok(monitor)
    }

    /// The stages the monitored input is heard through, with their low
    /// cut: those of the track that takes the recording, so that what is
    /// heard while recording is close to what the take will sound like.
    /// Noise reduction and pitch correction are left out (they would make
    /// the voice late in the ear), as are plugins and sends. `None`: as it arrives.
    pub fn monitor_chain(&self, chain: Option<(f32, Tone)>) {
        self.shared.monitor_chain.set(chain);
    }

    /// Whether the monitored input is heard or only measured.
    pub fn hear_monitor(&self, hear: bool) {
        self.shared.monitor_heard.store(hear, Ordering::Relaxed);
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
        // (Plugins that are still starting are waited for: an export must not begin without them.)
        let _ = rx.recv_timeout(std::time::Duration::from_secs(300));
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
        TrackMix {
            clips,
            gain: 1.0,
            pan: 0.0,
            mute: false,
            low_cut_hz: 0.0,
            denoise: 0.0,
            tone: Tone::default(),
            plugins: Vec::new(),
            sampler: None,
            pads: Vec::new(),
            notes: Vec::new(),
            unheard: false,
            reverb: 0.0,
            echo: 0.0,
            group: 0,
            by_hand: false,
        }
    }
}

/// Reads the level meters from any thread without touching the engine.
#[derive(Clone)]
pub struct Meters {
    shared: Arc<Shared>,
}

/// Plays the instruments of tracks by hand, from any thread, without
/// going through whoever owns the [`Engine`].
#[derive(Clone)]
pub struct Keys {
    control: crossbeam_channel::Sender<disk::Control>,
    shared: Arc<Shared>,
}

impl Keys {
    /// As [`Engine::note`].
    pub fn note(&self, track: usize, on: bool, pitch: u8, velocity: u8) {
        let _ = self.control.send(disk::Control::Note { track, on, pitch, velocity });
    }

    /// The timeline sample being heard right now, while something plays:
    /// where a key pressed now belongs.
    pub fn heard(&self) -> Option<i64> {
        self.shared.running.load(Ordering::Relaxed).then(|| heard(&self.shared))
    }
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

    /// What the de-esser, the compressor and the ducking of each track
    /// are doing to what is heard right now: decibels of reduction (0 or
    /// below) per track into `tracks`. All zero while nothing plays.
    pub fn stages(&self, tracks: &mut [[f32; 3]]) {
        let playing = self.shared.running.load(Ordering::Relaxed);
        let step = heard(&self.shared).div_euclid(STAGE_STEP);
        for (track, out) in tracks.iter_mut().enumerate() {
            let slot = self.shared.stages.slot(track, step).map_or(0, |slot| slot.load(Ordering::Relaxed));
            let fresh = playing && slot >> 32 == step as u32 as u64;
            *out = [0, 1, 2].map(|n| if fresh { -(((slot >> (8 * n)) & 255) as f32) / 8.0 } else { 0.0 });
        }
    }

    /// Which track's chain is on screen (`None`: none): its level after
    /// every stage is noted from now on.
    ///
    /// `spectrum_at`: the point along the chain (0: the source, then after
    /// each stage in the track's order) whose spectrum is wanted too, with
    /// that of the point after it.
    pub fn watch(&self, track: Option<usize>, spectrum_at: Option<usize>) {
        self.shared.scope.watch(track, spectrum_at);
    }

    /// What is heard now of the watched track, by pitch: decibels per
    /// band ([`SPECTRUM_BANDS`] of them over [`SPECTRUM_HZ`]) going into
    /// the stage that was asked for and coming out of it.
    pub fn spectrum(&self) -> Option<[[f32; SPECTRUM_BANDS]; 2]> {
        self.shared.scope.spectrum(heard(&self.shared))
    }

    /// The watched track's level after every stage (linear peaks, one
    /// array per stretch of [`SCOPE_STEP`] samples) for the `behind`
    /// stretches up to what is heard now and the `ahead` ones that come
    /// next, oldest first. A stretch nothing is known of is `None`.
    pub fn scope(&self, behind: usize, ahead: usize, out: &mut Vec<Option<[f32; SCOPE_TAPS]>>) {
        let now = heard(&self.shared).div_euclid(SCOPE_STEP);
        out.clear();
        out.extend((now - behind as i64 + 1..=now + ahead as i64).map(|step| self.shared.scope.read(step)));
    }

    /// Peak level of the monitored input since the last call (linear).
    pub fn take_input(&self) -> f32 {
        f32::from_bits(self.shared.input_meter.swap(0, Ordering::Relaxed))
    }

    /// Loudness of the last three seconds of the mix, in LUFS (-120: silence).
    pub fn loudness(&self) -> f32 {
        f32::from_bits(self.shared.loudness.load(Ordering::Relaxed))
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

    pub fn keys(&self) -> Keys {
        Keys { control: self.control.clone(), shared: self.shared.clone() }
    }
}
