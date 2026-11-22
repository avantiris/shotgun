//! The preview engine: keeps the viewer showing the frame at the playhead
//! and the speakers playing the sound at it.
//!
//! One thread (the "render" thread of 02-architecture.md) owns the
//! compositor and the audio engine. The audio engine's heard position is
//! the clock; each tick the thread works out which frame that is, makes
//! sure its pictures are decoded and composites them. Decoding happens in
//! the pool in [`frames`]. Callers send commands and read atomics.

mod frames;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
pub use sg_audio::midi::MidiIn;
pub use sg_audio::record::{Input, inputs};
use sg_audio::{ClipSpan, Engine, Mix, SAMPLE_RATE, TrackMix};
use sg_core::{AssetId, ClipProps, Project, Sequence, TrackKind};
use sg_project::cache;
use sg_render::{Compositor, LayerDraw, Source, plan, plan_track, sequence_frames};

use crate::frames::{Decoded, FrameKey, Pool, Quality, TextureCache};

pub use sg_audio::{
    AudioError, Backend, DEFAULT_QUANTUM, MAX_METERS, SCOPE_STEP, SCOPE_TAPS, SPECTRUM_BANDS, Stats as AudioStats,
};

/// VRAM spent on decoded preview frames.
const TEXTURE_BUDGET: u64 = 768 << 20;
/// Frames decoded ahead of the playhead during playback.
const READ_AHEAD: i64 = 6;
/// Height of one picture of the camera-angle monitor, in pixels.
const MONITOR_CELL_H: u32 = 270;
/// Angles the monitor shows at most (the keys 1 to 9).
const MONITOR_MAX: usize = 9;
/// Scrub grains follow each other no faster than they last, so that none is cut off.
const GRAIN_SPACING: std::time::Duration = std::time::Duration::from_millis(105);

enum Command {
    Project {
        root: PathBuf,
        project: Arc<Project>,
    },
    Clear,
    Play,
    Pause,
    Toggle,
    Seek(i64),
    Step(i64),
    /// A derived file of this asset appeared: reopen and re-mix.
    AssetChanged(AssetId),
    /// A font file was added or replaced: read fonts again.
    ForgetFonts,
    /// Show the window of the plugin on the track with this key.
    PluginEditor(u64),
    /// Have the plugins' states written to their files, then reply.
    SavePlugins(crossbeam_channel::Sender<()>),
    /// The stages the monitored input is heard through.
    MonitorChain(Option<Box<sg_core::TrackSettings>>),
    /// The metronome on or off.
    Click(bool),
    /// The audio track (its id) whose instrument is played by hand, or none.
    Hand(Option<u64>),
    /// Play the stretch between these timeline samples over and over, or (`None`) play on.
    Loop(Option<(i64, i64)>),
    /// Open a sound input to see its level and, if asked, hear it; `None` closes it.
    Monitor(Option<(Option<String>, bool)>),
    /// Record from a sound input into this file while playing; ends when playback does.
    Record {
        path: PathBuf,
        source: Option<String>,
        /// Start a bar before the playhead even without the metronome.
        lead_in: bool,
    },
    Quit,
}

/// Timings for the performance overlay and the perf log.
#[derive(Clone, Debug, Default)]
pub struct PlayStats {
    /// Seek command to composited frame, proxy quality, in ms (last 512).
    pub seek_ms: Vec<f32>,
    /// Seek command to full-quality frame, in ms.
    pub full_ms: Vec<f32>,
    /// Play command to the first composited frame after audio started, in ms.
    pub play_start_ms: Vec<f32>,
    /// Frames shown during playback, and those shown late or skipped.
    pub played: u64,
    pub dropped: u64,
    pub cache_bytes: u64,
    pub audio: AudioStats,
    pub decode_error: Option<String>,
    /// Messages about plugins not yet shown to the user.
    pub plugin_news: Vec<String>,
    /// Recordings that have ended and wait to be put into the project.
    pub takes: Vec<sg_audio::record::Take>,
}

struct Shared {
    frame: AtomicI64,
    playing: AtomicBool,
    length: AtomicI64,
    /// Bumped when the output texture is replaced (frame size changed).
    output_generation: AtomicU64,
    output: Mutex<wgpu::TextureView>,
    stats: Mutex<PlayStats>,
    monitor: Mutex<wgpu::TextureView>,
    monitor_generation: AtomicU64,
    /// Angles shown, columns and rows, packed 16 bits each; 0 when there are none.
    monitor_grid: AtomicU64,
    /// Which track of the mix is played by hand; -1: none.
    hand: AtomicI64,
}

pub struct Player {
    meters: sg_audio::Meters,
    keys: sg_audio::Keys,
    commands: Sender<Command>,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Player {
    /// `notify` is called from the player thread whenever the output
    /// texture or the playhead changed and the UI should redraw.
    pub fn start(
        device: wgpu::Device,
        queue: wgpu::Queue,
        backend: Backend,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Result<Player, AudioError> {
        let audio = Engine::start(backend)?;
        let (meters, keys) = (audio.meters(), audio.keys());
        let compositor = Compositor::new(device.clone(), queue.clone(), 1920, 1080);
        let monitor = Compositor::new(device.clone(), queue.clone(), MONITOR_CELL_H * 16 / 9, MONITOR_CELL_H);
        let shared = Arc::new(Shared {
            frame: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            length: AtomicI64::new(0),
            output_generation: AtomicU64::new(1),
            output: Mutex::new(compositor.output_view().clone()),
            stats: Mutex::default(),
            monitor: Mutex::new(monitor.output_view().clone()),
            monitor_generation: AtomicU64::new(1),
            monitor_grid: AtomicU64::new(0),
            hand: AtomicI64::new(-1),
        });
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let pool = Pool::new(result_tx)?;
        let mut state = State {
            recording: None,
            listening: None,
            click: false,
            hand: None,
            looping: None,
            device,
            queue,
            compositor,
            audio,
            pool,
            cache: TextureCache::new(TEXTURE_BUDGET),
            shared: shared.clone(),
            notify,
            project: None,
            frame: 0,
            playing: false,
            rendered: None,
            monitor,
            monitor_grid: (0, 1, 1),
            monitor_rendered: None,
            titles: HashMap::new(),
            fonts: sg_render::text::Fonts::default(),
            seek_started: None,
            last_grain: None,
            full_started: None,
            play_started: None,
            last_played: None,
        };
        let thread =
            std::thread::Builder::new().name("sg-player".into()).spawn(move || state.run(command_rx, result_rx))?;
        Ok(Player { meters, keys, commands, shared, thread: Some(thread) })
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Hands over the current snapshot. Cheap; call after every edit.
    pub fn set_project(&self, root: PathBuf, project: Arc<Project>) {
        self.send(Command::Project { root, project });
    }

    pub fn clear_project(&self) {
        self.send(Command::Clear);
    }

    pub fn play(&self) {
        self.send(Command::Play);
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn toggle(&self) {
        self.send(Command::Toggle);
    }

    /// Moves the playhead to a sequence frame.
    pub fn seek(&self, frame: i64) {
        // Readers see the new position at once, before the thread gets to it.
        self.shared.frame.store(frame.max(0), Ordering::Relaxed);
        self.send(Command::Seek(frame));
    }

    pub fn step(&self, frames: i64) {
        self.send(Command::Step(frames));
    }

    pub fn asset_changed(&self, asset: AssetId) {
        self.send(Command::AssetChanged(asset));
    }

    /// A font file of the project was added or replaced.
    pub fn forget_fonts(&self) {
        self.send(Command::ForgetFonts);
    }

    /// The playhead, in sequence frames.
    pub fn frame(&self) -> i64 {
        self.shared.frame.load(Ordering::Relaxed)
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Length of the sequence in frames.
    pub fn length(&self) -> i64 {
        self.shared.length.load(Ordering::Relaxed)
    }

    /// The texture the viewer shows, and a number that changes when the
    /// texture is replaced by another one.
    pub fn output(&self) -> (wgpu::TextureView, u64) {
        let view = self.shared.output.lock().unwrap_or_else(PoisonError::into_inner).clone();
        (view, self.shared.output_generation.load(Ordering::Relaxed))
    }

    /// Peak levels since the last call: per audio track into `tracks`, the
    /// mix as the return value. Only atomics are read: fine on the UI thread.
    pub fn take_meters(&self, tracks: &mut [f32]) -> f32 {
        self.meters.take(tracks)
    }

    /// The camera-angle monitor, if the sequence has hidden camera tracks:
    /// its texture, a number that changes when the texture is replaced, and
    /// how many angles it shows in how many columns and rows.
    pub fn angles(&self) -> Option<(wgpu::TextureView, u64, usize, u32, u32)> {
        let grid = self.shared.monitor_grid.load(Ordering::Relaxed);
        let (count, cols, rows) =
            ((grid & 0xFFFF) as usize, (grid >> 16 & 0xFFFF) as u32, (grid >> 32 & 0xFFFF) as u32);
        if count == 0 {
            return None;
        }
        let view = self.shared.monitor.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Some((view, self.shared.monitor_generation.load(Ordering::Relaxed), count, cols, rows))
    }

    /// Decibels the master limiter took off since the last call.
    /// Loudness of the last three seconds of the mix, in LUFS.
    pub fn loudness(&self) -> f32 {
        self.meters.loudness()
    }

    pub fn take_limiting(&self) -> f32 {
        self.meters.take_limiting()
    }

    pub fn stats(&self) -> PlayStats {
        self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Shows the window of the plugin on the track with this key.
    pub fn open_plugin_editor(&self, key: u64) {
        self.send(Command::PluginEditor(key));
    }

    /// Has the plugins' states written to the project and waits for it,
    /// two seconds at most: before an export, which runs plugins of its own.
    pub fn save_plugins(&self) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.send(Command::SavePlugins(tx));
        let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
    }

    /// Something to wait on until everything asked of the player before
    /// this has been done, the plugins' part of it included (a project
    /// that was cleared is no longer written to).
    pub fn settled(&self) -> crossbeam_channel::Receiver<()> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.send(Command::SavePlugins(tx));
        rx
    }

    /// Turns the metronome on or off: clicks on the beats of a sequence
    /// that has a tempo while it plays, and a bar of them before a recording.
    pub fn set_click(&self, on: bool) {
        self.send(Command::Click(on));
    }

    /// Plays the stretch between two timeline samples over and over once
    /// the playhead reaches its end; `None`: plays on as usual. Recordings
    /// are not looped under.
    pub fn set_loop(&self, range: Option<(i64, i64)>) {
        self.send(Command::Loop(range));
    }

    /// Names the audio track whose instrument is played by hand with
    /// [`Player::note`] (`None`: no track). That track is then heard within
    /// a few milliseconds of a key, also while nothing plays.
    pub fn set_hand(&self, track: Option<u64>) {
        self.send(Command::Hand(track));
    }

    /// A key pressed or let go on the instrument played by hand. Returns
    /// the timeline sample that was being heard, if something plays: where
    /// the note belongs when it is written down.
    pub fn note(&self, on: bool, pitch: u8, velocity: u8) -> Option<i64> {
        let track = usize::try_from(self.shared.hand.load(Ordering::Relaxed)).ok()?;
        self.keys.note(track, on, pitch, velocity);
        self.keys.heard()
    }

    /// Opens a sound input (`source`; `None` inside: the default one) to
    /// see its level with [`Player::take_input_level`] and, with the flag,
    /// to hear it. `None` closes it again.
    pub fn monitor(&self, input: Option<(Option<String>, bool)>) {
        self.send(Command::Monitor(input));
    }

    /// The settings of the track whose stages the monitored input is heard
    /// through (the one that takes the recording); `None`: as it arrives.
    pub fn monitor_chain(&self, mix: Option<sg_core::TrackSettings>) {
        self.send(Command::MonitorChain(mix.map(Box::new)));
    }

    /// What the de-esser, the compressor and the ducking of each audio
    /// track do to what is heard right now, in decibels (0 or below).
    pub fn stages(&self, tracks: &mut [[f32; 3]]) {
        self.meters.stages(tracks);
    }

    /// Which audio track's chain is on screen (`None`: none): the level
    /// after each of its stages is then kept for [`Player::scope`].
    /// With `spectrum_at`, the spectrum going into and out of the stage
    /// after that point of the chain is kept too, for [`Player::spectrum`].
    pub fn watch(&self, track: Option<usize>, spectrum_at: Option<usize>) {
        self.meters.watch(track, spectrum_at);
    }

    /// What is heard now of the watched track by pitch, in decibels per
    /// band: going into the stage asked for with [`Player::watch`], and coming out.
    pub fn spectrum(&self) -> Option<[[f32; SPECTRUM_BANDS]; 2]> {
        self.meters.spectrum()
    }

    /// The watched track's level after every stage, for the `behind`
    /// stretches of [`SCOPE_STEP`] samples up to what is heard now and
    /// the `ahead` ones after it, oldest first.
    pub fn scope(&self, behind: usize, ahead: usize, out: &mut Vec<Option<[f32; SCOPE_TAPS]>>) {
        self.meters.scope(behind, ahead, out);
    }

    /// Peak level of the monitored input since the last call (linear).
    pub fn take_input_level(&self) -> f32 {
        self.meters.take_input()
    }

    /// Starts playing and recording from `source` (`None`: the default
    /// input) into the file at `path`. The recording ends when playback
    /// is stopped; the take is then found with [`Player::take_recordings`].
    ///
    /// `lead_in`: playback starts a bar before the playhead (two seconds
    /// without a tempo) also when the metronome is off: for a take that
    /// is punched into what is already there, the singer hears the way in.
    pub fn record(&self, path: PathBuf, source: Option<String>, lead_in: bool) {
        self.send(Command::Record { path, source, lead_in });
    }

    /// The recordings that have ended since the last call.
    pub fn take_recordings(&self) -> Vec<sg_audio::record::Take> {
        std::mem::take(&mut self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).takes)
    }

    /// Messages about plugins since the last call (one that did not load or stopped working).
    pub fn take_plugin_news(&self) -> Vec<String> {
        std::mem::take(&mut self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).plugin_news)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Open {
    root: PathBuf,
    project: Arc<Project>,
}

impl Open {
    fn sequence(&self) -> Option<&Arc<Sequence>> {
        self.project.sequences.first()
    }
}

struct State {
    /// The recording in progress.
    recording: Option<sg_audio::record::Recording>,
    /// The input being monitored, and which one it is.
    listening: Option<(Option<String>, sg_audio::record::Monitor)>,
    /// The metronome is on.
    click: bool,
    /// The track played by hand (its id).
    hand: Option<u64>,
    /// The stretch played over and over, in timeline samples.
    looping: Option<(i64, i64)>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    compositor: Compositor,
    audio: Engine,
    pool: Pool,
    cache: TextureCache,
    shared: Arc<Shared>,
    notify: Arc<dyn Fn() + Send + Sync>,
    project: Option<Open>,
    /// Playhead in sequence frames.
    frame: i64,
    playing: bool,
    /// What the output texture currently shows.
    rendered: Option<(i64, Vec<LayerKey>)>,
    /// The camera angles side by side, for choosing between them.
    monitor: Compositor,
    /// Angles shown, columns and rows of the monitor.
    monitor_grid: (usize, u32, u32),
    monitor_rendered: Option<(i64, Vec<Option<FrameKey>>)>,
    /// Rasterised titles.
    titles: HashMap<TitleKey, (wgpu::TextureView, (u32, u32))>,
    /// Font files of the project, as far as titles have asked for them.
    fonts: sg_render::text::Fonts,
    seek_started: Option<Instant>,
    /// When the last scrub grain was asked for.
    last_grain: Option<Instant>,
    full_started: Option<Instant>,
    play_started: Option<Instant>,
    last_played: Option<i64>,
}

fn push_sample(samples: &mut Vec<f32>, value: f32) {
    if samples.len() >= 512 {
        samples.remove(0);
    }
    samples.push(value);
}

impl State {
    fn stats(&self, f: impl FnOnce(&mut PlayStats)) {
        f(&mut self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner));
    }

    fn length(&self) -> i64 {
        self.project.as_ref().and_then(|o| o.sequence()).map_or(0, |s| sequence_frames(s, SAMPLE_RATE))
    }

    /// The audio mix of the open sequence.
    fn mix(&self) -> Mix {
        match self.project.as_ref().and_then(|o| o.sequence().map(|s| (o, s))) {
            Some((open, sequence)) => {
                let mut mix = Mix { click: self.click(sequence), ..audio_mix(&open.root, sequence) };
                // The tracks of the mix are the audio tracks, in order.
                let audio = sequence.tracks.iter().filter(|t| t.kind == TrackKind::Audio);
                let hand = self.hand.and_then(|id| audio.clone().position(|t| t.id.0 == id));
                if let Some(track) = hand.and_then(|index| mix.tracks.get_mut(index)) {
                    track.by_hand = true;
                }
                self.shared.hand.store(hand.map_or(-1, |index| index as i64), Ordering::Relaxed);
                mix
            }
            None => {
                self.shared.hand.store(-1, Ordering::Relaxed);
                Mix::default()
            }
        }
    }

    /// The metronome's beats, if it is on and the sequence has a tempo.
    fn click(&self, sequence: &Sequence) -> Option<sg_audio::space::Click> {
        let tempo = sequence.tempo.filter(|_| self.click)?;
        Some(sg_audio::space::Click {
            beat: (SAMPLE_RATE as u64 * 60_000, tempo.milli_bpm.max(1) as u64),
            beats_per_bar: tempo.beats_per_bar.max(1) as u32,
            origin: tempo.origin,
        })
    }

    fn samples_of(&self, frame: i64) -> i64 {
        self.project.as_ref().and_then(|o| o.sequence()).map_or(0, |s| s.rate.frames_to_samples(frame, SAMPLE_RATE))
    }

    fn frame_of(&self, samples: i64) -> i64 {
        self.project.as_ref().and_then(|o| o.sequence()).map_or(0, |s| s.rate.samples_to_frames(samples, SAMPLE_RATE))
    }

    fn set_frame(&mut self, frame: i64) {
        // While a take is made the playhead runs on past the last clip:
        // the take is what will lie there (in an empty project it stood
        // still at the start, and the take could not be seen growing).
        let end = if self.recording.is_some() { i64::MAX } else { self.length().max(0) };
        self.frame = frame.clamp(0, end);
        self.shared.frame.store(self.frame, Ordering::Relaxed);
    }

    fn start_playing(&mut self) {
        self.start_playing_after(0);
    }

    /// Starts playing `lead` samples before the playhead (a count-in).
    fn start_playing_after(&mut self, lead: i64) {
        if self.project.is_none() || self.playing {
            return;
        }
        // At the end: play again from the start.
        if self.frame >= self.length() && self.recording.is_none() {
            self.set_frame(0);
        }
        self.playing = true;
        self.last_played = None;
        self.play_started = Some(Instant::now());
        self.audio.play(self.samples_of(self.frame) - lead);
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    fn stop_playing(&mut self) {
        if !self.playing {
            return;
        }
        self.playing = false;
        let samples = self.audio.pause();
        let frame = self.frame_of(samples);
        self.set_frame(frame);
        self.shared.playing.store(false, Ordering::Relaxed);
        // Nothing is asked of the decoders beyond this frame any more.
        self.pool.new_epoch();
        // A recording ends with the playback it was made to.
        if let Some(recording) = self.recording.take() {
            self.audio.set_loop(self.looping);
            match recording.stop() {
                Ok(take) => self.stats(|s| s.takes.push(take)),
                Err(e) => self.stats(|s| s.plugin_news.push(format!("The recording was lost: {e}"))),
            }
            (self.notify)();
        }
    }

    fn jump(&mut self, frame: i64) {
        self.pool.new_epoch();
        self.set_frame(frame);
        self.seek_started = Some(Instant::now());
        self.full_started = self.seek_started;
        if self.playing {
            self.last_played = None;
            self.audio.play(self.samples_of(self.frame));
        } else if self.last_grain.is_none_or(|at| at.elapsed() >= GRAIN_SPACING) {
            // Scrubbing is heard: a short piece of the sound at the playhead.
            self.last_grain = Some(Instant::now());
            self.audio.grain(self.samples_of(self.frame));
        } else {
            self.audio.seek(self.samples_of(self.frame));
        }
    }

    fn handle(&mut self, command: Command) -> bool {
        match command {
            Command::Project { root, project } => {
                let same_place = self.project.as_ref().is_some_and(|o| o.root == root);
                if !same_place {
                    self.stop_playing();
                    self.cache = TextureCache::new(TEXTURE_BUDGET);
                    self.fonts.clear();
                    self.titles.clear();
                    self.compositor.clear_luts();
                    self.frame = 0;
                }
                let size = project.sequences.first().map_or((1920, 1080), |s| (s.width, s.height));
                self.project = Some(Open { root, project });
                if self.compositor.size() != size {
                    self.compositor.resize(size.0, size.1);
                    *self.shared.output.lock().unwrap_or_else(PoisonError::into_inner) =
                        self.compositor.output_view().clone();
                    self.shared.output_generation.fetch_add(1, Ordering::Relaxed);
                }
                self.audio.set_mix(self.mix());
                self.shared.length.store(self.length(), Ordering::Relaxed);
                self.set_frame(self.frame);
                self.rendered = None;
            }
            Command::Clear => {
                self.stop_playing();
                self.project = None;
                self.audio.set_mix(Mix::default());
                self.cache = TextureCache::new(TEXTURE_BUDGET);
                self.rendered = None;
                self.shared.length.store(0, Ordering::Relaxed);
                self.set_frame(0);
            }
            Command::Play => self.start_playing(),
            Command::Pause => self.stop_playing(),
            Command::Toggle => {
                if self.playing {
                    self.stop_playing();
                } else {
                    self.start_playing();
                }
            }
            Command::Seek(frame) => self.jump(frame),
            Command::Step(delta) => {
                self.stop_playing();
                self.jump(self.frame + delta);
            }
            Command::AssetChanged(asset) => {
                self.cache.forget_asset(asset);
                self.audio.set_mix(self.mix());
                self.rendered = None;
            }
            Command::ForgetFonts => {
                // (Fonts and look-up tables alike: files of the project that are read once.)
                self.fonts.clear();
                self.titles.clear();
                self.compositor.clear_luts();
                self.rendered = None;
            }
            Command::Record { path, source, lead_in } => {
                if self.recording.is_none() && self.project.is_some() {
                    // The input first, so that nothing of the performance is missed; the take begins with the playback.
                    match self.audio.record(path, source) {
                        Ok(recording) => {
                            self.recording = Some(recording);
                            // A take is one pass along the timeline: the loop waits until it has ended.
                            self.audio.set_loop(None);
                            // With the metronome on, a bar of clicks comes first.
                            let sequence = self.project.as_ref().and_then(|o| o.sequence());
                            let bar = sequence.and_then(|s| {
                                let click = self.click(s)?;
                                Some((click.beat.0 * click.beats_per_bar as u64 / click.beat.1) as i64)
                            });
                            // Without the metronome, a lead-in if asked for: a bar of the tempo, or two seconds.
                            let way_in = lead_in.then(|| {
                                sequence.and_then(|s| s.tempo).map_or(2 * SAMPLE_RATE as i64, |t| {
                                    SAMPLE_RATE as i64 * 60_000 * t.beats_per_bar.max(1) as i64
                                        / t.milli_bpm.max(1) as i64
                                })
                            });
                            self.start_playing_after(bar.or(way_in).unwrap_or(0));
                        }
                        Err(e) => {
                            self.stats(|s| s.plugin_news.push(format!("Cannot record: {e}")));
                            (self.notify)();
                        }
                    }
                }
            }
            Command::Click(on) => {
                self.click = on;
                self.audio.set_mix(self.mix());
            }
            Command::Hand(track) => {
                self.hand = track;
                self.audio.set_mix(self.mix());
            }
            Command::Loop(range) => {
                self.looping = range;
                // (A recording is one pass: it is not looped under.)
                self.audio.set_loop(range.filter(|_| self.recording.is_none()));
            }
            Command::MonitorChain(mix) => {
                self.audio.monitor_chain(mix.map(|mix| (mix.low_cut_hz, tone_of(&mix))));
            }
            Command::Monitor(None) => self.listening = None,
            Command::Monitor(Some((source, hear))) => {
                // Only hearing changed: the stream stays as it is.
                if self.listening.as_ref().is_some_and(|(open, _)| *open == source) {
                    self.audio.hear_monitor(hear);
                } else {
                    // (The one before is closed first: its ring is handed back.)
                    self.listening = None;
                    match self.audio.monitor(source.clone(), hear) {
                        Ok(monitor) => self.listening = Some((source, monitor)),
                        Err(e) => {
                            self.stats(|s| s.plugin_news.push(format!("Cannot listen to the input: {e}")));
                            (self.notify)();
                        }
                    }
                }
            }
            Command::PluginEditor(key) => self.audio.open_plugin_editor(key),
            Command::SavePlugins(reply) => {
                self.audio.save_plugin_states();
                let _ = reply.send(());
            }
            Command::Quit => return false,
        }
        true
    }

    /// Makes the output show the playhead frame as well as currently
    /// possible and asks for what is missing. Returns whether something is
    /// still being decoded for it.
    fn update(&mut self) -> bool {
        // What the audio engine has to say about plugins goes on to the application.
        let news = self.audio.plugin_news();
        if !news.is_empty() {
            self.stats(|s| s.plugin_news.extend(news));
            (self.notify)();
        }
        let Some(open) = &self.project else { return false };
        let Some(sequence) = open.sequence() else { return false };
        let (root, project) = (open.root.clone(), open.project.clone());
        let sequence = sequence.clone();
        if self.playing && self.audio.is_playing() {
            // The realtime thread has taken over: sound is on its way out and
            // the frame at the playhead is on screen. That is "playing".
            if let Some(started) = self.play_started.take() {
                let ms = started.elapsed().as_secs_f32() * 1000.0;
                self.stats(|s| push_sample(&mut s.play_start_ms, ms));
            }
            let frame = self.frame_of(self.audio.position());
            // (While recording the clock runs on past the end: the take may be the first thing there.)
            if frame >= self.length() && self.recording.is_none() {
                self.stop_playing();
                self.set_frame(self.length());
            } else {
                self.set_frame(frame.max(0));
            }
        }
        let layers = plan(&project, &sequence, self.frame);
        // Paused: ask for the full-quality picture at once. A newer seek makes
        // the request obsolete before a worker starts on it, so dragging the
        // playhead costs at most one decode in flight.
        let paused = !self.playing;
        let frame_height = self.compositor.size().1 as f32;
        let mut waiting = false;
        let mut keys = Vec::with_capacity(layers.len());
        // What to draw: the texture to use and the layer it belongs to.
        let mut chosen: Vec<(LayerKey, usize)> = Vec::with_capacity(layers.len());
        let mut exact = true;
        for (index, layer) in layers.iter().enumerate() {
            let (asset_id, source_frame) = match &layer.source {
                Source::Asset { asset, frame } => (*asset, *frame),
                Source::Title => {
                    let Some(title) = layer.props.as_ref().and_then(|p| p.title.as_ref()) else { continue };
                    let key = TitleKey {
                        text: title.text.clone(),
                        px: (title.size * frame_height).round().max(4.0) as u32,
                        color: title.color,
                        font: title.font.clone(),
                        shadow: (title.shadow.clamp(0.0, 1.0) * 100.0).round() as u8,
                    };
                    if !self.titles.contains_key(&key) {
                        let font = self.fonts.get(&root, &key.font);
                        let image = sg_render::text::rasterize(
                            &key.text,
                            key.px as f32,
                            key.color,
                            font.as_ref().map(|bytes| bytes.as_slice()),
                        );
                        let image = sg_render::text::with_shadow(image, key.px as f32, key.shadow as f32 / 100.0);
                        let texture =
                            Compositor::upload(&self.device, &self.queue, image.width, image.height, &image.rgba);
                        // Titles are few and small; a simple cap keeps the map bounded.
                        if self.titles.len() >= 64 {
                            self.titles.clear();
                        }
                        self.titles.insert(
                            key.clone(),
                            (texture.create_view(&Default::default()), (image.width, image.height)),
                        );
                    }
                    keys.push(LayerKey::Title(key.clone()));
                    chosen.push((LayerKey::Title(key), index));
                    continue;
                }
            };
            let Some(asset) = project.assets.get(&asset_id) else { continue };
            let stable = layer.props.as_ref().is_some_and(|p| p.stabilise);
            let proxy = FrameKey { asset: asset_id, quality: Quality::Proxy, frame: source_frame, stable };
            let full = FrameKey { quality: Quality::Full, ..proxy };
            if paused && !self.cache.contains(&full) {
                self.pool.request(full, &root, asset);
                waiting = true;
            }
            let pick = if self.cache.contains(&full) {
                Some(full)
            } else if self.cache.contains(&proxy) {
                Some(proxy)
            } else {
                self.pool.request(proxy, &root, asset);
                waiting = true;
                exact = false;
                // Meanwhile: the picture just before, so playback does not flash black.
                self.cache.nearest_before(asset_id, source_frame)
            };
            if let Some(key) = pick {
                keys.push(LayerKey::Frame(key));
                chosen.push((LayerKey::Frame(key), index));
            }
        }
        if self.playing {
            for ahead in 1..=READ_AHEAD {
                for layer in plan(&project, &sequence, self.frame + ahead) {
                    let Source::Asset { asset: asset_id, frame } = layer.source else { continue };
                    let stable = layer.props.as_ref().is_some_and(|p| p.stabilise);
                    let key = FrameKey { asset: asset_id, quality: Quality::Proxy, frame, stable };
                    if let (false, Some(asset)) = (self.cache.contains(&key), project.assets.get(&asset_id)) {
                        self.pool.request(key, &root, asset);
                    }
                }
            }
        }

        if self.rendered.as_ref() != Some(&(self.frame, keys.clone())) {
            // Clone the views first: `get` records the use for eviction order.
            // The rotation comes with the decoded file, so a proxy that was
            // written upright is not turned a second time.
            let views: Vec<Drawable> = chosen
                .iter()
                .filter_map(|(key, index)| match key {
                    LayerKey::Frame(key) => {
                        self.cache.get(key).map(|c| (c.view.clone(), c.size, c.rotation, *index, false))
                    }
                    LayerKey::Title(key) => {
                        self.titles.get(key).map(|(view, size)| (view.clone(), *size, 0, *index, true))
                    }
                })
                .collect();
            let draws: Vec<LayerDraw<'_>> = views
                .iter()
                .map(|(view, size, rotation, index, title)| layers[*index].draw(view, *size, *rotation, *title))
                .collect();
            sg_render::load_luts(&mut self.compositor, &root, &layers);
            self.compositor.render(&draws);
            self.rendered = Some((self.frame, keys.clone()));
            let now = Instant::now();
            if self.playing {
                let late = !exact || self.last_played.is_some_and(|last| self.frame > last + 1);
                self.last_played = Some(self.frame);
                self.stats(|s| {
                    s.played += 1;
                    s.dropped += late as u64;
                });
            } else if exact {
                let all_full = keys.iter().all(|k| !matches!(k, LayerKey::Frame(f) if f.quality != Quality::Full));
                let (seek, full) = (self.seek_started.take(), if all_full { self.full_started.take() } else { None });
                self.stats(|s| {
                    if let Some(t) = seek {
                        push_sample(&mut s.seek_ms, (now - t).as_secs_f32() * 1000.0);
                    }
                    if let Some(t) = full {
                        push_sample(&mut s.full_ms, (now - t).as_secs_f32() * 1000.0);
                    }
                });
            }
            (self.notify)();
        }
        let monitor_waiting = self.update_monitor(&root, &project, &sequence);
        waiting || monitor_waiting
    }

    /// Renders the camera angles (the hidden video tracks) at the playhead
    /// side by side, from proxies. Besides showing what there is to cut
    /// to, this keeps every angle decoded, so a cut to it is on screen at
    /// once. Returns whether a picture is still being decoded for it.
    fn update_monitor(&mut self, root: &std::path::Path, project: &Arc<Project>, sequence: &Arc<Sequence>) -> bool {
        let angles: Vec<&Arc<sg_core::Track>> =
            sequence.tracks.iter().filter(|t| t.kind == TrackKind::Video && t.mix.hidden).take(MONITOR_MAX).collect();
        let count = angles.len();
        if count == 0 {
            if self.monitor_grid.0 != 0 {
                self.monitor_grid = (0, 1, 1);
                self.shared.monitor_grid.store(0, Ordering::Relaxed);
                self.monitor_rendered = None;
                (self.notify)();
            }
            return false;
        }
        // Up to three in a row, more in a square.
        let cols = if count <= 3 { count as u32 } else { (count as f32).sqrt().ceil() as u32 };
        let rows = (count as u32).div_ceil(cols);
        let cell = ((MONITOR_CELL_H * sequence.width / sequence.height.max(1)).clamp(64, 1024) / 2 * 2, MONITOR_CELL_H);
        if self.monitor_grid != (count, cols, rows) || self.monitor.size() != (cols * cell.0, rows * cell.1) {
            self.monitor.resize(cols * cell.0, rows * cell.1);
            *self.shared.monitor.lock().unwrap_or_else(PoisonError::into_inner) = self.monitor.output_view().clone();
            self.shared.monitor_generation.fetch_add(1, Ordering::Relaxed);
            self.monitor_grid = (count, cols, rows);
            self.shared.monitor_grid.store(count as u64 | (cols as u64) << 16 | (rows as u64) << 32, Ordering::Relaxed);
            self.monitor_rendered = None;
        }
        let mut waiting = false;
        let mut picks: Vec<Option<FrameKey>> = Vec::with_capacity(count);
        for track in &angles {
            let source = plan_track(project, sequence, track, self.frame).map(|layer| layer.source);
            let Some(Source::Asset { asset: asset_id, frame }) = source else {
                picks.push(None);
                continue;
            };
            let key = FrameKey { asset: asset_id, quality: Quality::Proxy, frame, stable: false };
            if self.cache.contains(&key) {
                picks.push(Some(key));
            } else {
                if let Some(asset) = project.assets.get(&asset_id) {
                    self.pool.request(key, root, asset);
                    waiting = true;
                }
                picks.push(self.cache.nearest_before(asset_id, frame));
            }
        }
        if self.playing {
            for ahead in 1..=READ_AHEAD {
                for track in &angles {
                    let source = plan_track(project, sequence, track, self.frame + ahead).map(|layer| layer.source);
                    let Some(Source::Asset { asset: asset_id, frame }) = source else { continue };
                    let key = FrameKey { asset: asset_id, quality: Quality::Proxy, frame, stable: false };
                    if let (false, Some(asset)) = (self.cache.contains(&key), project.assets.get(&asset_id)) {
                        self.pool.request(key, root, asset);
                    }
                }
            }
        }
        if self.monitor_rendered.as_ref() != Some(&(self.frame, picks.clone())) {
            let (width, height) = ((cols * cell.0) as f32, (rows * cell.1) as f32);
            let views: Vec<(wgpu::TextureView, (u32, u32), i32, usize)> = picks
                .iter()
                .enumerate()
                .filter_map(|(n, key)| self.cache.get(key.as_ref()?).map(|c| (c.view.clone(), c.size, c.rotation, n)))
                .collect();
            let draws: Vec<LayerDraw<'_>> = views
                .iter()
                .map(|(view, size, rotation, n)| {
                    // The picture as displayed, fitted into its cell instead of the whole monitor.
                    let (w, h) = if rotation.rem_euclid(180) == 90 { (size.1, size.0) } else { *size };
                    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
                    let whole = (width / w).min(height / h);
                    let in_cell = (cell.0 as f32 / w).min(cell.1 as f32 / h);
                    let (col, row) = (*n as u32 % cols, *n as u32 / cols);
                    LayerDraw {
                        x: (col as f32 + 0.5) / cols as f32 - 0.5,
                        y: (row as f32 + 0.5) / rows as f32 - 0.5,
                        scale: in_cell / whole,
                        ..LayerDraw::fitted(view, *size, *rotation)
                    }
                })
                .collect();
            self.monitor.render(&draws);
            self.monitor_rendered = Some((self.frame, picks));
            (self.notify)();
        }
        waiting
    }

    fn run(&mut self, commands: Receiver<Command>, results: Receiver<Decoded>) {
        loop {
            let busy = self.update();
            let audio = self.audio.stats();
            let bytes = self.cache.bytes();
            self.stats(|s| {
                s.audio = audio;
                s.cache_bytes = bytes;
            });
            // Playing: wake often enough to catch every frame boundary.
            // Waiting for a decode or for the playhead to settle: short naps.
            // Otherwise sleep until a command arrives; idle costs nothing.
            let timeout = if self.playing {
                Some(Duration::from_millis(2))
            } else if busy {
                Some(Duration::from_millis(20))
            } else {
                None
            };
            let mut first = true;
            loop {
                let wait = if first { timeout } else { Some(Duration::ZERO) };
                first = false;
                crossbeam_channel::select! {
                    recv(commands) -> command => match command {
                        Ok(command) => {
                            if !self.handle(command) {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    recv(results) -> decoded => {
                        if let Ok(decoded) = decoded {
                            self.pool.answered(&decoded.key);
                            match decoded.frame {
                                Ok(frame) => {
                                    let t = Instant::now();
                                    self.cache.insert(&self.device, &self.queue, decoded.key, &frame);
                                    log::debug!(
                                        "{:?} frame {} arrived {:.1} ms after the seek, upload {:.1} ms",
                                        decoded.key.quality,
                                        decoded.key.frame,
                                        self.seek_started.or(self.full_started).map_or(-1.0, |s| (t - s).as_secs_f32() * 1000.0),
                                        t.elapsed().as_secs_f32() * 1000.0
                                    );
                                }
                                Err(e) => {
                                    log::warn!("decode failed: {e}");
                                    self.stats(|s| s.decode_error = Some(e));
                                }
                            }
                        }
                    },
                    default(wait.unwrap_or(Duration::from_secs(3600))) => break,
                }
            }
        }
    }
}

/// A rasterised title: its text, letter height in pixels and colour.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct TitleKey {
    text: String,
    px: u32,
    color: [u8; 4],
    font: String,
    /// Shadow strength in hundredths.
    shadow: u8,
}

/// What a layer of the rendered frame was drawn from.
#[derive(Clone, Debug, PartialEq)]
enum LayerKey {
    Frame(FrameKey),
    Title(TitleKey),
}

/// A texture ready to draw: view, size, file rotation, the layer it
/// belongs to, and whether it is a title (drawn at its pixel size).
type Drawable = (wgpu::TextureView, (u32, u32), i32, usize, bool);

/// What a sequence sounds like: its audio tracks as the engine wants them.
/// Preview and export both build their mix here, so they cannot differ.
pub fn audio_mix(root: &std::path::Path, sequence: &Sequence) -> Mix {
    let db = |decibels: f32| 10f32.powf(decibels / 20.0);
    let any_solo = sequence.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.mix.solo);
    let tracks = sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|track| TrackMix {
            clips: track
                .clips
                .iter()
                // (A clip without an asset is a note clip: it has notes, not a file.)
                .filter(|c| c.asset != sg_core::AssetId(0))
                .map(|c| {
                    let default = ClipProps::default();
                    let props = track.props_of(c.id).map_or(&default, |p| &**p);
                    let samples = |frames: i64| sequence.rate.frames_to_samples(frames, SAMPLE_RATE);
                    ClipSpan {
                        start: c.start,
                        len: c.len,
                        src_in: c.src_in,
                        wav: cache::audio(root, c.asset),
                        // Decibels to linear. With keyframes the level moves along the clip.
                        gain: if props.gain_db.is_animated() { 1.0 } else { 10f32.powf(props.gain_db.value / 20.0) },
                        gain_keys: level_points(&props.gain_db, samples),
                        fade_in: samples(props.fade_in),
                        fade_out: samples(props.fade_out),
                        speed: if props.speed_valid() { props.speed } else { (1, 1) },
                        warp: speed_warp(sequence, &c, props),
                    }
                })
                .collect(),
            gain: db(track.mix.gain_db),
            pan: track.mix.pan,
            // While any track is soloed, the others are silent.
            mute: track.mix.mute || (any_solo && !track.mix.solo),
            low_cut_hz: track.mix.low_cut_hz,
            denoise: track.mix.denoise,
            tone: tone_of(&track.mix),
            plugins: track
                .plugins
                .iter()
                .map(|plugin| sg_audio::PluginSpec {
                    key: plugin.key(track.id),
                    path: std::path::PathBuf::from(&plugin.path),
                    // In the bundle: a plugin's settings travel with the project.
                    state_file: root.join("plugins").join(plugin.state_file(track.id)),
                })
                .collect(),
            notes: track_notes(sequence, track),
            pads: track.pads.iter().map(|(key, asset)| (*key, cache::audio(root, *asset))).collect(),
            sampler: track.sampler.map(|sampler| sg_audio::SamplerSpec {
                wav: cache::audio(root, sampler.asset),
                root: sampler.root,
                one_shot: sampler.one_shot,
            }),
            unheard: false,
            group: track.mix.group.min(sg_core::GROUPS as u8),
            by_hand: false,
            // (Squared: the lower half of the control is the useful, quiet part.)
            reverb: track.mix.reverb * track.mix.reverb,
            echo: track.mix.echo * track.mix.echo,
        })
        .collect();
    // The echo repeats in time with the beat, at the note value chosen;
    // without a tempo a beat is taken as 3/8 of a second.
    let beat =
        sequence.tempo.map_or(SAMPLE_RATE as u64 * 3 / 8, |t| SAMPLE_RATE as u64 * 60_000 / t.milli_bpm.max(1) as u64);
    let (num, den) = sequence.space.echo_note.beats();
    Mix {
        tracks,
        master_gain: db(sequence.master_db),
        echo_samples: (beat * num as u64 / den as u64) as u32,
        groups: sequence.groups.map(|g| (db(g.gain_db), g.mute)),
        bpm: sequence.tempo.map_or(120.0, sg_core::Tempo::bpm),
        bar_origin: sequence.tempo.map_or(0, |t| t.origin),
        room_seconds: sequence.space.room_ds as f32 / 10.0,
        echo_repeat: sequence.space.echo_repeat as f32 / 100.0,
        stop_at: None,
        click: None,
        true_peak_db: None,
        // (A timeline is cut wherever the hand falls.)
        ease_edges: true,
    }
}

/// The notes of a track's note clips in timeline samples, in order. A
/// tick is a 960th of a beat at the sequence's tempo (120 a minute
/// without one); a note that reaches past its clip is cut there.
fn track_notes(sequence: &Sequence, track: &sg_core::Track) -> Vec<sg_audio::NoteSpan> {
    let bpm = sequence.tempo.map_or(120.0, sg_core::Tempo::bpm);
    let per_tick = SAMPLE_RATE as f64 * 60.0 / (bpm * sg_core::TICKS_PER_BEAT as f64);
    let mut notes = Vec::new();
    for clip in track.clips.iter().filter(|c| c.asset == sg_core::AssetId(0)) {
        let Some(list) = track.notes.get(&clip.id) else { continue };
        for note in list.iter() {
            // A note clip that was cut or trimmed at its start plays its
            // notes from that point on (`src_in`, in samples as for sound):
            // what lies before it is left out, a note across it begins there.
            let from = (note.start as f64 * per_tick).round() as i64 - clip.src_in;
            let until = from + (note.len as f64 * per_tick).round() as i64;
            let start = from.max(0);
            let len = until.min(clip.len) - start;
            if start < clip.len && len > 0 {
                notes.push(sg_audio::NoteSpan {
                    start: clip.start + start,
                    len,
                    pitch: note.pitch,
                    velocity: note.velocity,
                });
            }
        }
    }
    notes.sort_by_key(|n| (n.start, n.pitch));
    notes
}

/// Volume keyframes as the engine wants them: points with straight lines
/// between. A stretch that is not straight is given points along its way.
fn level_points(level: &sg_core::Param, samples: impl Fn(i64) -> i64) -> Vec<(i64, f32)> {
    let mut points = Vec::with_capacity(level.keys.len());
    for (i, key) in level.keys.iter().enumerate() {
        points.push((samples(key.frame), key.value));
        let Some(next) = level.keys.get(i + 1) else { continue };
        match key.ease {
            sg_core::Ease::Linear => {}
            // Held until one sample before the next keyframe.
            sg_core::Ease::Hold => points.push((samples(next.frame) - 1, key.value)),
            sg_core::Ease::Smooth => {
                const STEPS: i64 = 16;
                let (from, to) = (samples(key.frame), samples(next.frame));
                for step in 1..STEPS {
                    let t = step as f64 / STEPS as f64;
                    let value = key.value + (next.value - key.value) * key.ease.curve(t) as f32;
                    points.push((from + (to - from) * step / STEPS, value));
                }
            }
        }
    }
    // Keyframes a frame apart can make points fall on the same sample.
    points.dedup_by_key(|p| p.0);
    points
}

/// The source position of a sound clip along a speed ramp, for the engine.
/// The ramp is a property of the picture; the sound of the same take (same
/// asset, same place) follows it. Empty when no ramp applies.
fn speed_warp(sequence: &Sequence, clip: &sg_core::Clip, own: &ClipProps) -> Vec<(i64, f64)> {
    let samples = |frames: i64| sequence.rate.frames_to_samples(frames, SAMPLE_RATE);
    let start = sequence.rate.samples_to_frames(clip.start, SAMPLE_RATE);
    if samples(start) != clip.start {
        // Not on a frame: no picture clip can be its partner.
        return Vec::new();
    }
    let ramp = sequence
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .filter_map(|t| {
            let picture = t.clips.get(start).filter(|p| p.asset == clip.asset && samples(p.len) == clip.len)?;
            t.props_of(picture.id).map(|p| &p.ramp)
        })
        .find(|ramp| !ramp.is_one());
    let Some(ramp) = ramp else { return Vec::new() };
    let (num, den) = if own.speed_valid() { own.speed } else { (1, 1) };
    let frames = sequence.rate.samples_to_frames(clip.len, SAMPLE_RATE);
    // The area under a moving speed is curved: points along every stretch.
    let mut at: Vec<i64> = vec![0, frames];
    let mut edges: Vec<i64> = std::iter::once(0)
        .chain(ramp.keys.iter().map(|k| k.frame.clamp(0, frames)))
        .chain(std::iter::once(frames))
        .collect();
    edges.dedup();
    for pair in edges.windows(2) {
        const STEPS: i64 = 16;
        at.extend((0..=STEPS).map(|step| pair[0] + (pair[1] - pair[0]) * step / STEPS));
    }
    at.sort_unstable();
    at.dedup();
    let per_frame = samples(1_000_000) as f64 / 1_000_000.0;
    at.into_iter().map(|f| (samples(f), ramp.integral(f) * num as f64 / den as f64 * per_frame)).collect()
}

/// A track's settings as the engine's stages take them.
fn tone_of(mix: &sg_core::TrackSettings) -> sg_audio::Tone {
    sg_audio::Tone {
        low_db: mix.eq_low_db,
        mid_db: mix.eq_mid_db,
        mid_hz: mix.eq_mid_hz,
        high_db: mix.eq_high_db,
        threshold_db: mix.comp_threshold_db,
        ratio: mix.comp_ratio,
        makeup_db: mix.comp_makeup_db,
        deess: mix.deess,
        tune: sg_audio::tune::Tune {
            amount: mix.tune,
            key: mix.tune_key,
            scale: mix.tune_scale,
            speed: mix.tune_speed,
        },
        level: mix.level,
        level_ref_db: mix.level_ref_db,
        drive: mix.drive,
        voice: mix.voice,
        duck_db: mix.duck_db,
        order: mix.order,
    }
}

/// A whole recording (interleaved stereo) through the stages of a track
/// with these settings, in place: what the track sounds like before its
/// plugins and its fader.
pub fn process_offline(stereo: &mut [f32], mix: &sg_core::TrackSettings) {
    sg_audio::process_offline(stereo, mix.low_cut_hz, mix.denoise, &tone_of(mix));
}

/// Listens to a voice (mono, 48 kHz) and sets a strip up for it: what
/// [`sg_media::voice::suggest`] proposes from the measurements, then put
/// right by ear, as it were: the take is run through the stages with the
/// proposed settings and measured again, and fader, compressor and noise
/// reduction are moved until what comes out is as loud and as even as
/// the style wants and its pauses are quiet. `style`: `None` for the one
/// that fits. `None` back if there is too little sound.
///
/// (Feedback N2.2: the proposal alone was worked out for a chain that has
/// changed since, and nothing checked what it led to.)
pub fn fit_voice(
    samples: &[f32],
    style: Option<sg_media::voice::VoiceStyle>,
) -> Option<(sg_media::voice::VoiceStats, sg_media::voice::Suggestion)> {
    use sg_media::voice;
    let stats = voice::measure(samples)?;
    let style = style.unwrap_or_else(|| voice::VoiceStyle::guess(&stats));
    let mut set = voice::suggest(&stats, style);
    let (spread, level) = style.aims();
    // A minute from the middle is enough to judge by, and keeps this quick.
    let most = SAMPLE_RATE as usize * 60;
    let from = samples.len().saturating_sub(most) / 2;
    let part = &samples[from..(from + most).min(samples.len())];
    // (The last pass moves nothing but make-up and fader: what it sets is what was measured.)
    const PASSES: usize = 6;
    // The noise reduction as it was before the last step up, with how clear
    // the pauses were then; and whether stepping it up has helped so far.
    let (mut stepped, mut helps): (Option<(f32, f32)>, bool) = (None, true);
    for pass in 0..PASSES {
        let last = pass + 1 == PASSES;
        let mix = set.onto(sg_core::TrackSettings::default());
        // (The pitch correction changes no level; without it this is four times as fast.)
        let tone = sg_audio::Tone { tune: sg_audio::tune::Tune { amount: 0.0, ..tone_of(&mix).tune }, ..tone_of(&mix) };
        let mut stereo: Vec<f32> = part.iter().flat_map(|s| [*s, *s]).collect();
        // The level the leveller is to level to: the average it finds over
        // the whole of this, with what comes before it set as it is now.
        let reference = sg_audio::leveller_average(&stereo, mix.low_cut_hz, mix.denoise, &tone);
        set.level_ref_db = reference.map_or(0.0, |db| (db * 10.0).round() / 10.0);
        let tone = sg_audio::Tone { level_ref_db: set.level_ref_db, ..tone };
        sg_audio::process_offline(&mut stereo, mix.low_cut_hz, mix.denoise, &tone);
        let heard: Vec<f32> = stereo.as_chunks::<2>().0.iter().map(|frame| frame[0]).collect();
        let Some(after) = voice::measure(&heard) else { break };
        let mut settled = true;
        // Noise that can still be heard in the pauses: more of the noise reduction.
        // (Only under a take that had a floor of noise to begin with: what
        // lies between the phrases of a clean one is breath.)
        let clear = after.noise_db.map(|noise| after.loudness_db - noise);
        let noisy = stats.noise_db.is_some() && clear.is_some_and(|clear| clear < 45.0);
        // More of it only as long as more helps: what the reduction does
        // not take for noise (the tail of a room, another voice, music
        // that bled in) stays whatever it is set to, and the voice pays.
        // Found on finished vocals from records, which all ended at 100 %.
        if let (Some((before, was)), Some(now)) = (stepped.take(), clear)
            && now < was + 2.0
        {
            set.denoise = before;
            helps = false;
        }
        if noisy && helps && set.denoise < 1.0 && !last {
            stepped = Some((set.denoise, clear.unwrap_or(0.0)));
            set.denoise = (set.denoise + 0.25).min(1.0);
            settled = false;
        }
        // The loud phrases against the average: the compressor's threshold.
        let off = (after.loud_db - after.loudness_db) - spread;
        let slope = 1.0 - 1.0 / set.comp_ratio.max(1.0);
        if last {
            // (Nothing more is moved.)
        } else if off > 1.0 && set.comp_threshold_db > -44.0 {
            set.comp_threshold_db = (set.comp_threshold_db - off / slope).max(-45.0);
            settled = false;
        } else if off < -2.5 && set.comp_threshold_db < -6.0 {
            set.comp_threshold_db = (set.comp_threshold_db - off / slope * 0.7).min(-6.0);
            settled = false;
        }
        // What the stages took off the level comes back after the
        // compressor; the fader stays near where a fader belongs and
        // does what is left.
        let missing = level - after.loudness_db;
        let makeup = (set.comp_makeup_db + missing).clamp(0.0, 24.0);
        if (makeup - set.comp_makeup_db).abs() > 0.5 {
            settled = false;
        }
        set.gain_db = (missing - (makeup - set.comp_makeup_db)).clamp(-12.0, 12.0);
        set.comp_makeup_db = makeup;
        if settled {
            break;
        }
    }
    let round = |v: f32| (v * 10.0).round() / 10.0;
    set.gain_db = round(set.gain_db);
    set.comp_threshold_db = round(set.comp_threshold_db);
    set.comp_makeup_db = round(set.comp_makeup_db);
    Some((stats, set))
}

/// What the low cut and the equaliser of these settings do to a steady
/// tone of `hz`, in decibels: for the curve the mixer draws.
pub fn response_db(mix: &sg_core::TrackSettings, hz: f32) -> f32 {
    sg_audio::response_db(mix.low_cut_hz, &tone_of(mix), hz)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use sg_core::model::{SequenceHeader, TrackHeader};
    use sg_core::time::Rate;
    use sg_core::{AssetId, Clip, ClipId, Key, Op, Param, Project, ProjectId, SequenceId, TrackId, apply};

    #[test]
    fn a_noisy_voice_is_set_up_so_that_it_comes_out_at_its_level_with_quiet_pauses() {
        // Phrases of a gliding voice, every third one much louder, over mains hum and hiss.
        let rate = super::SAMPLE_RATE as f32;
        let mut seed = 0x2545_f491u32;
        let (mut phase, mut level) = (0.0f32, 0.0f32);
        let take: Vec<f32> = (0..super::SAMPLE_RATE as usize * 12)
            .map(|i| {
                let t = i as f32 / rate;
                let wanted =
                    if t.fract() < 0.7 { if (t as usize).is_multiple_of(3) { 0.5 } else { 0.12 } } else { 0.0 };
                level += 0.002 * (wanted - level);
                let hz = 160.0 * (1.0 + 0.2 * (t * 5.0).sin());
                phase += 2.0 * std::f32::consts::PI * hz / rate;
                let voice: f32 = (1..=20).map(|h| (phase * h as f32).sin() / h as f32).sum();
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let hiss = 0.008 * (seed as f32 / u32::MAX as f32 * 2.0 - 1.0);
                level * 0.4 * voice + 0.03 * (2.0 * std::f32::consts::PI * 50.0 * t).sin() + hiss
            })
            .collect();
        let (stats, set) = super::fit_voice(&take, Some(sg_media::voice::VoiceStyle::RapLead)).unwrap();
        assert!(stats.noise_db.is_some_and(|noise| stats.loudness_db - noise < 25.0), "{stats:?}");
        assert!(set.denoise >= 0.75 && set.low_cut_hz >= 100.0 && set.comp_makeup_db > 0.0, "{set:?}");
        // The take with those settings on, as the track plays it.
        let mix = set.onto(sg_core::TrackSettings::default());
        let mut stereo: Vec<f32> = take.iter().flat_map(|s| [*s, *s]).collect();
        super::process_offline(&mut stereo, &mix);
        let heard: Vec<f32> = stereo.as_chunks::<2>().0.iter().map(|frame| frame[0]).collect();
        let after = sg_media::voice::measure(&heard).unwrap();
        let (spread, aim) = set.style.aims();
        // At the style's level with the fader, the loud phrases not far above, and the pauses far below.
        assert!((after.loudness_db + set.gain_db - aim).abs() < 1.5, "{after:?} {set:?}");
        assert!(after.loud_db - after.loudness_db < spread + 2.5, "{after:?}");
        assert!(after.noise_db.is_none_or(|noise| after.loudness_db - noise > 40.0), "{after:?}");
        // The same length, and in time: the first loud phrase starts where it did.
        assert_eq!(heard.len(), take.len());
    }

    use super::*;

    #[test]
    fn a_note_clip_cut_at_its_start_plays_its_notes_from_there() {
        let (seq, a1) = (SequenceId(1), TrackId(2));
        // At 120 a minute a tick is 25 samples. The clip begins one beat (24 000 samples) into its notes.
        let clip = Clip { id: ClipId(1), asset: AssetId(0), start: 100_000, len: 48_000, src_in: 24_000 };
        let note = |start: i64, len: i64, pitch: u8| sg_core::Note { start, len, pitch, velocity: 100 };
        let ops = [
            Op::AddSequence {
                index: 0,
                sequence: SequenceHeader {
                    id: seq,
                    name: "Main".into(),
                    rate: Rate::FPS_60,
                    width: 1920,
                    height: 1080,
                },
            },
            Op::AddTrack { seq, index: 0, track: TrackHeader { id: a1, kind: TrackKind::Audio, name: "A1".into() } },
            Op::InsertClip { seq, track: a1, clip },
            Op::SetClipNotes {
                seq,
                track: a1,
                clip: clip.id,
                // Before the cut; across it; inside; across the clip's end; after it.
                notes: Some(vec![
                    note(0, 480, 60),
                    note(720, 480, 62),
                    note(1440, 240, 64),
                    note(2640, 960, 65),
                    note(3000, 100, 67),
                ]),
            },
        ];
        let mut project = std::sync::Arc::new(Project::new(ProjectId(1), "p"));
        for op in &ops {
            project = apply(&project, op).unwrap().0;
        }
        let mix = audio_mix(std::path::Path::new("/nowhere"), &project.sequences[0]);
        let played: Vec<(i64, i64, u8)> = mix.tracks[0].notes.iter().map(|n| (n.start, n.len, n.pitch)).collect();
        // The first is left out, the second begins with the clip, the fourth ends with it, the last is left out.
        assert_eq!(played, [(100_000, 6_000, 62), (112_000, 6_000, 64), (142_000, 6_000, 65)]);
    }

    #[test]
    fn the_sound_of_a_take_follows_the_pictures_speed_ramp() {
        let (seq, v1, a1) = (SequenceId(1), TrackId(1), TrackId(2));
        let picture = Clip { id: ClipId(1), asset: AssetId(7), start: 120, len: 600, src_in: 0 };
        // The same stretch in samples: 60 frames a second, 800 samples a frame.
        let sound = Clip { id: ClipId(2), start: 120 * 800, len: 600 * 800, ..picture };
        // Normal speed for a second, then down to half speed over two seconds.
        let ramp = Param { value: 1.0, keys: vec![Key::new(60, 1.0), Key::new(180, 0.5)] };
        let props = ClipProps { ramp: ramp.clone(), ..ClipProps::default() };
        let ops = [
            Op::AddSequence {
                index: 0,
                sequence: SequenceHeader {
                    id: seq,
                    name: "Main".into(),
                    rate: Rate::FPS_60,
                    width: 1920,
                    height: 1080,
                },
            },
            Op::AddTrack { seq, index: 0, track: TrackHeader { id: v1, kind: TrackKind::Video, name: "V1".into() } },
            Op::AddTrack { seq, index: 1, track: TrackHeader { id: a1, kind: TrackKind::Audio, name: "A1".into() } },
            Op::InsertClip { seq, track: v1, clip: picture },
            Op::InsertClip { seq, track: a1, clip: sound },
            Op::SetClipProps { seq, track: v1, clip: picture.id, props: Some(Box::new(props)) },
        ];
        let mut project = std::sync::Arc::new(Project::new(ProjectId(1), "p"));
        for op in &ops {
            project = apply(&project, op).unwrap().0;
        }
        let mix = audio_mix(std::path::Path::new("/nowhere"), &project.sequences[0]);
        let warp = &mix.tracks[0].clips[0].warp;
        assert_eq!(warp.first(), Some(&(0, 0.0)));
        // At the first keyframe a second of source has gone by; at the end
        // the area under the curve: 60 + 120 * 0.75 + 420 * 0.5 frames.
        assert!(warp.contains(&(60 * 800, 60.0 * 800.0)));
        let end = warp.last().unwrap();
        assert_eq!(end.0, 600 * 800);
        assert!((end.1 - (60.0 + 90.0 + 210.0) * 800.0).abs() < 1.0, "{end:?}");
        assert!(warp.windows(2).all(|w| w[0].0 < w[1].0 && w[0].1 < w[1].1));
        // Without a ramp nothing is handed over.
        let plain = apply(&project, &Op::SetClipProps { seq, track: v1, clip: picture.id, props: None }).unwrap().0;
        assert!(audio_mix(std::path::Path::new("/nowhere"), &plain.sequences[0]).tracks[0].clips[0].warp.is_empty());
    }
}
