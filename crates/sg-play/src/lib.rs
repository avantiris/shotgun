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
use sg_audio::{ClipSpan, Engine, Mix, SAMPLE_RATE, TrackMix};
use sg_core::{AssetId, ClipProps, Project, Sequence, TrackKind};
use sg_project::cache;
use sg_render::{Compositor, LayerDraw, Source, plan, sequence_frames};

use crate::frames::{Decoded, FrameKey, Pool, Quality, TextureCache};

pub use sg_audio::{AudioError, Backend, DEFAULT_QUANTUM, MAX_METERS, Stats as AudioStats};

/// VRAM spent on decoded preview frames.
const TEXTURE_BUDGET: u64 = 768 << 20;
/// Frames decoded ahead of the playhead during playback.
const READ_AHEAD: i64 = 6;

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
}

struct Shared {
    frame: AtomicI64,
    playing: AtomicBool,
    length: AtomicI64,
    /// Bumped when the output texture is replaced (frame size changed).
    output_generation: AtomicU64,
    output: Mutex<wgpu::TextureView>,
    stats: Mutex<PlayStats>,
}

pub struct Player {
    meters: sg_audio::Meters,
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
        let meters = audio.meters();
        let compositor = Compositor::new(device.clone(), queue.clone(), 1920, 1080);
        let shared = Arc::new(Shared {
            frame: AtomicI64::new(0),
            playing: AtomicBool::new(false),
            length: AtomicI64::new(0),
            output_generation: AtomicU64::new(1),
            output: Mutex::new(compositor.output_view().clone()),
            stats: Mutex::default(),
        });
        let (commands, command_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let pool = Pool::new(result_tx)?;
        let mut state = State {
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
            titles: HashMap::new(),
            seek_started: None,
            full_started: None,
            play_started: None,
            last_played: None,
        };
        let thread =
            std::thread::Builder::new().name("sg-player".into()).spawn(move || state.run(command_rx, result_rx))?;
        Ok(Player { meters, commands, shared, thread: Some(thread) })
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

    pub fn stats(&self) -> PlayStats {
        self.shared.stats.lock().unwrap_or_else(PoisonError::into_inner).clone()
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
    /// Rasterised titles.
    titles: HashMap<TitleKey, (wgpu::TextureView, (u32, u32))>,
    seek_started: Option<Instant>,
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
            Some((open, sequence)) => audio_mix(&open.root, sequence),
            None => Mix::default(),
        }
    }

    fn samples_of(&self, frame: i64) -> i64 {
        self.project.as_ref().and_then(|o| o.sequence()).map_or(0, |s| s.rate.frames_to_samples(frame, SAMPLE_RATE))
    }

    fn frame_of(&self, samples: i64) -> i64 {
        self.project.as_ref().and_then(|o| o.sequence()).map_or(0, |s| s.rate.samples_to_frames(samples, SAMPLE_RATE))
    }

    fn set_frame(&mut self, frame: i64) {
        self.frame = frame.clamp(0, self.length().max(0));
        self.shared.frame.store(self.frame, Ordering::Relaxed);
    }

    fn start_playing(&mut self) {
        if self.project.is_none() || self.playing {
            return;
        }
        // At the end: play again from the start.
        if self.frame >= self.length() {
            self.set_frame(0);
        }
        self.playing = true;
        self.last_played = None;
        self.play_started = Some(Instant::now());
        self.audio.play(self.samples_of(self.frame));
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
    }

    fn jump(&mut self, frame: i64) {
        self.pool.new_epoch();
        self.set_frame(frame);
        self.seek_started = Some(Instant::now());
        self.full_started = self.seek_started;
        if self.playing {
            self.last_played = None;
            self.audio.play(self.samples_of(self.frame));
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
            Command::Quit => return false,
        }
        true
    }

    /// Makes the output show the playhead frame as well as currently
    /// possible and asks for what is missing. Returns whether something is
    /// still being decoded for it.
    fn update(&mut self) -> bool {
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
            if frame >= self.length() {
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
                    };
                    if !self.titles.contains_key(&key) {
                        let image = sg_render::text::rasterize(&key.text, key.px as f32, key.color);
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
            let proxy = FrameKey { asset: asset_id, quality: Quality::Proxy, frame: source_frame };
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
                    let key = FrameKey { asset: asset_id, quality: Quality::Proxy, frame };
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
            let default = ClipProps::default();
            let draws: Vec<LayerDraw<'_>> = views
                .iter()
                .map(|(view, size, rotation, index, title)| {
                    let layer = &layers[*index];
                    let props = layer.props.as_deref().unwrap_or(&default);
                    let at = layer.at;
                    LayerDraw {
                        view,
                        size: *size,
                        rotation: *rotation,
                        opacity: props.opacity.at(at),
                        x: props.x.at(at),
                        y: props.y.at(at),
                        scale: props.scale.at(at),
                        turn_degrees: props.rotation.at(at),
                        crop: props.crop,
                        natural_size: *title,
                    }
                })
                .collect();
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
                .map(|c| {
                    let default = ClipProps::default();
                    let props = track.props_of(c.id).map_or(&default, |p| &**p);
                    let samples = |frames: i64| sequence.rate.frames_to_samples(frames, SAMPLE_RATE);
                    ClipSpan {
                        start: c.start,
                        len: c.len,
                        src_in: c.src_in,
                        wav: cache::audio(root, c.asset),
                        // Decibels to linear; the value at the clip start (volume keyframes: later).
                        gain: 10f32.powf(props.gain_db.at(0) / 20.0),
                        fade_in: samples(props.fade_in),
                        fade_out: samples(props.fade_out),
                        speed: if props.speed_valid() { props.speed } else { (1, 1) },
                    }
                })
                .collect(),
            gain: db(track.mix.gain_db),
            pan: track.mix.pan,
            // While any track is soloed, the others are silent.
            mute: track.mix.mute || (any_solo && !track.mix.solo),
            low_cut_hz: track.mix.low_cut_hz,
            denoise: track.mix.denoise,
        })
        .collect();
    Mix { tracks, master_gain: db(sequence.master_db) }
}
