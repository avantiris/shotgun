//! The disk thread: turns a [`Mix`] into graphs and keeps their ring
//! buffers filled from the conformed WAV files. Everything that allocates
//! or blocks on behalf of the realtime thread happens here.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::PoisonError;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::fx::TrackFx;
use crate::graph::{Fader, Graph, Limiter, Live, MAX_BLOCK, Processor, Slot, Track};
use crate::{ClipSpan, Mix, SAMPLE_RATE, Shared};

pub enum Control {
    Mix(Mix),
    Play(i64),
    Pause(i64),
    /// While paused: let a short piece of sound at this position be heard.
    Grain(i64),
    /// Reply once everything sent before is in place and buffered.
    Settle(Sender<()>),
    /// Show the window of the plugin with this key.
    Editor(u64),
    /// Write every plugin's state to its file, then reply.
    SaveStates(Sender<()>),
    /// From a loader thread: the plugin for this track has started, or has not (`None`).
    Loaded(crate::PluginSpec, Option<Box<sg_plug::Client>>),
    Quit,
}

/// Frames buffered ahead per track: half a second.
const RING_FRAMES: usize = SAMPLE_RATE as usize / 2;
/// Frames written before a graph is offered, so playback starts with data.
const PREFILL_FRAMES: usize = 4096;
/// How often the rings are topped up while playing.
const FILL_INTERVAL: Duration = Duration::from_millis(5);
/// Length of the piece of sound heard at the playhead while scrubbing: a
/// tenth of a second, enough to recognise a beat or a syllable.
pub const GRAIN_FRAMES: usize = SAMPLE_RATE as usize / 10;
/// The device is kept running this long after a grain began, so that the
/// next grain of the same scrub does not have to wake it again.
const GRAIN_LINGER: Duration = Duration::from_millis(400);

/// A conformed audio file: 32-bit float WAV, mono or stereo, 48 kHz.
struct Wav {
    file: File,
    data_offset: u64,
    channels: usize,
    frames: i64,
}

impl Wav {
    fn open(path: &Path) -> std::io::Result<Wav> {
        let mut file = File::open(path)?;
        let bad = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_owned());
        let mut head = [0u8; 12];
        file.read_exact(&mut head)?;
        if &head[8..12] != b"WAVE" {
            return Err(bad("not a WAV file"));
        }
        let mut at = 12u64;
        let mut channels = 0usize;
        loop {
            let mut chunk = [0u8; 8];
            file.read_exact_at(&mut chunk, at)?;
            let size = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as u64;
            at += 8;
            match &chunk[0..4] {
                b"fmt " => {
                    let mut fmt = [0u8; 16];
                    file.read_exact_at(&mut fmt, at)?;
                    channels = u16::from_le_bytes([fmt[2], fmt[3]]) as usize;
                    let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                    if bits != 32 || !(1..=2).contains(&channels) {
                        return Err(bad("conformed audio must be 32-bit float, mono or stereo"));
                    }
                }
                b"data" => {
                    if channels == 0 {
                        return Err(bad("WAV data before its format"));
                    }
                    // The size field is unreliable for long files: use the file length.
                    let bytes = file.metadata()?.len().saturating_sub(at);
                    return Ok(Wav { file, data_offset: at, channels, frames: (bytes / (4 * channels as u64)) as i64 });
                }
                _ => {}
            }
            at += size + size % 2;
        }
    }

    /// Adds `frames` frames starting at `offset` to interleaved stereo `out`,
    /// scaled by `gain`. Frames outside the file stay untouched.
    fn mix_into(&self, offset: i64, out: &mut [f32], gain: f32, raw: &mut Vec<u8>) {
        let frames = (out.len() / 2) as i64;
        let first = offset.max(0);
        let last = (offset + frames).min(self.frames);
        if last <= first {
            return;
        }
        let count = (last - first) as usize;
        raw.resize(count * 4 * self.channels, 0);
        if self.file.read_exact_at(raw, self.data_offset + first as u64 * 4 * self.channels as u64).is_err() {
            return;
        }
        let skip = (first - offset) as usize;
        let samples = raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b));
        let target = out[skip * 2..(skip + count) * 2].as_chunks_mut::<2>().0.iter_mut();
        if self.channels == 1 {
            for (frame, s) in target.zip(samples) {
                frame[0] += s * gain;
                frame[1] += s * gain;
            }
        } else {
            let mut samples = samples;
            for frame in target {
                frame[0] += samples.next().unwrap_or(0.0) * gain;
                frame[1] += samples.next().unwrap_or(0.0) * gain;
            }
        }
    }
}

struct Feed {
    producer: rtrb::Producer<f32>,
    clips: Vec<ClipSpan>,
    /// Timeline sample of the next frame to write.
    next: i64,
    fx: TrackFx,
    /// Settings of the clean-up, read at every fill.
    low_cut_hz: f32,
    denoise: f32,
    tone: crate::fx::Tone,
    live: Arc<Live>,
    /// How far this feed is ducked right now, in decibels (0 or below).
    ducked_db: f32,
    /// The key of the track's plugin, if it runs.
    plugin: Option<u64>,
    /// The track has a plugin that is still starting.
    awaiting: bool,
    /// Note starts (sorted by time) and note ends (sorted by time): (sample, pitch, velocity).
    note_ons: Vec<(i64, u8, u8)>,
    note_offs: Vec<(i64, u8)>,
    /// The built-in instrument, for notes on a track without a plugin.
    synth: crate::synth::Synth,
}

struct Disk {
    slot: Arc<Slot>,
    shared: Arc<Shared>,
    activate: Option<Box<dyn Fn(bool) + Send>>,
    mix: Mix,
    feeds: Vec<Feed>,
    master: Arc<Live>,
    files: HashMap<PathBuf, Option<Arc<Wav>>>,
    playing: bool,
    /// A grain is (or was just) sounding; when to let the device rest.
    grain_until: Option<Instant>,
    block: Vec<f32>,
    raw: Vec<u8>,
    /// Scratch for one clip's stretch of a block before gain and fades.
    segment: Vec<f32>,
    offered: u64,
    /// How loud the voice tracks are along the timeline, for the tracks
    /// that duck under them: the peak of each stretch of [`DUCK_STEP`]
    /// frames, and which stretch the slot holds (a slot is reused every
    /// eleven seconds).
    voice_level: Vec<(i64, f32)>,
    /// The plugins that run, by their track's key: where each came from,
    /// where its state is kept, and its process.
    plugins: HashMap<u64, (crate::PluginSpec, sg_plug::Client)>,
    /// Plugins being started on threads of their own (a sample library
    /// takes its time; the sound of the other tracks must not wait), and
    /// those that did not start: neither is started again while the mix
    /// keeps asking for the same one.
    loading: HashMap<u64, PathBuf>,
    failed: HashMap<u64, PathBuf>,
    /// For the loader threads to report back with.
    control: Sender<Control>,
    /// Asked to settle while plugins were still starting: answered when they have.
    settles: Vec<Sender<()>>,
    events: Vec<sg_plug::NoteEvent>,
}

/// Frames per entry of the voice level.
const DUCK_STEP: i64 = 64;
const DUCK_SLOTS: usize = 8192;

/// True if going from `a` to `b` needs no new graph: the same clips on the
/// same tracks, and noise reduction (which changes how far a feed reads
/// ahead) on the same tracks. Faders and amounts may differ.
fn same_structure(a: &Mix, b: &Mix) -> bool {
    a.tracks.len() == b.tracks.len()
        && a.click == b.click
        && a.echo_samples == b.echo_samples
        && a.tracks.iter().zip(&b.tracks).all(|(x, y)| {
            // (Noise reduction and pitch correction each delay the stream: switching one needs a new graph.)
            x.clips == y.clips
                && x.plugin == y.plugin
                && x.notes == y.notes
                && x.unheard == y.unheard
                // (The reverb and echo exist only while some track sends to them.)
                && (x.reverb > 0.0 || x.echo > 0.0) == (y.reverb > 0.0 || y.echo > 0.0)
                && (x.denoise > 0.0) == (y.denoise > 0.0)
                && (x.tone.tune.amount > 0.0) == (y.tone.tune.amount > 0.0)
        })
}

impl Disk {
    fn wav(&mut self, path: &Path) -> Option<Arc<Wav>> {
        if let Some(known) = self.files.get(path) {
            return known.clone();
        }
        let opened = match Wav::open(path) {
            Ok(wav) => Some(Arc::new(wav)),
            Err(e) => {
                log::debug!("audio not available yet for {}: {e}", path.display());
                None
            }
        };
        self.files.insert(path.to_owned(), opened.clone());
        opened
    }

    /// Starts the plugins the mix asks for and ends those it no longer
    /// names. Each starts on a thread of its own, gets its state from its
    /// file there and joins the sound when it is ready ([`Control::Loaded`]).
    fn sync_plugins(&mut self) {
        let wanted: Vec<crate::PluginSpec> = self.mix.tracks.iter().filter_map(|t| t.plugin.clone()).collect();
        let still = |key: &u64, path: &PathBuf| wanted.iter().any(|w| w.key == *key && w.path == *path);
        self.plugins.retain(|key, (spec, _)| still(key, &spec.path));
        self.loading.retain(|key, path| still(key, path));
        self.failed.retain(|key, path| still(key, path));
        for spec in wanted {
            let known = |map: &HashMap<u64, PathBuf>| map.contains_key(&spec.key);
            if self.plugins.contains_key(&spec.key) || known(&self.loading) || known(&self.failed) {
                continue;
            }
            self.loading.insert(spec.key, spec.path.clone());
            let (shared, control) = (self.shared.clone(), self.control.clone());
            let say = |shared: &Shared, text: String| {
                shared.plugin_news.lock().unwrap_or_else(PoisonError::into_inner).push(text);
            };
            let load = move || {
                let client = match sg_plug::Client::start(&spec.path) {
                    Ok(mut client) => {
                        if let Ok(state) = std::fs::read(&spec.state_file)
                            && !client.set_state(&state)
                        {
                            say(&shared, format!("{} did not take its saved settings.", client.loaded.name));
                        }
                        Some(Box::new(client))
                    }
                    Err(e) => {
                        say(&shared, format!("{}: {e}", spec.path.display()));
                        None
                    }
                };
                let _ = control.send(Control::Loaded(spec, client));
            };
            if let Err(e) = std::thread::Builder::new().name("sg-audio-plugin-load".into()).spawn(load) {
                log::error!("cannot start a thread to load a plugin: {e}");
            }
        }
    }

    /// Writes the state of every running plugin to its file.
    fn save_plugin_states(&mut self) {
        for (spec, client) in self.plugins.values_mut() {
            let Some(state) = client.state() else { continue };
            let written = spec
                .state_file
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(spec.state_file.with_extension("tmp"), &state))
                .and_then(|()| std::fs::rename(spec.state_file.with_extension("tmp"), &spec.state_file));
            if let Err(e) = written {
                let mut news = self.shared.plugin_news.lock().unwrap_or_else(PoisonError::into_inner);
                news.push(format!("The settings of {} could not be saved: {e}", client.loaded.name));
            }
        }
    }

    /// Runs feed `index`'s plugin over `block`, which starts at timeline
    /// sample `start`, with the notes that begin or end inside it. Without
    /// a plugin the built-in instrument plays them.
    fn run_plugin(&mut self, index: usize, start: i64, block: &mut [f32]) {
        let feed = &mut self.feeds[index];
        let end = start + (block.len() / 2) as i64;
        let plugin = feed.plugin.and_then(|key| self.plugins.get_mut(&key));
        if plugin.is_none() && feed.note_ons.is_empty() {
            return;
        }
        self.events.clear();
        let from = feed.note_offs.partition_point(|(at, _)| *at < start);
        for &(at, pitch) in feed.note_offs[from..].iter().take_while(|(at, _)| *at < end) {
            self.events.push(sg_plug::NoteEvent { offset: (at - start) as u32, on: false, pitch, velocity: 0 });
        }
        let from = feed.note_ons.partition_point(|(at, ..)| *at < start);
        for &(at, pitch, velocity) in feed.note_ons[from..].iter().take_while(|(at, ..)| *at < end) {
            self.events.push(sg_plug::NoteEvent { offset: (at - start) as u32, on: true, pitch, velocity });
        }
        // (A note that ends where the next of its pitch begins: the end first.)
        self.events.sort_by_key(|e| (e.offset, e.on));
        let Some((_, client)) = plugin else {
            if feed.awaiting {
                // Its instrument is still starting: not the built-in one in its place.
                return;
            }
            let events: Vec<(u32, bool, u8, u8)> =
                self.events.iter().map(|e| (e.offset, e.on, e.pitch, e.velocity)).collect();
            feed.synth.process(block, &events);
            return;
        };
        let was_dead = client.is_dead();
        if !client.process(block, &self.events) && !was_dead {
            let mut news = self.shared.plugin_news.lock().unwrap_or_else(PoisonError::into_inner);
            news.push(format!(
                "{} stopped working. Its track plays without it; take it off the track and put it on again to restart it.",
                client.loaded.name
            ));
        }
    }

    /// Whether the feed's notes are played before its own stages: by an
    /// instrument plugin, or by the built-in instrument when it has no plugin.
    fn is_instrument(&self, index: usize) -> bool {
        match self.feeds[index].plugin.and_then(|key| self.plugins.get(&key)) {
            Some((_, client)) => client.loaded.instrument,
            None => !self.feeds[index].note_ons.is_empty(),
        }
    }

    /// Lays the clips of feed `index` into `block` (interleaved stereo)
    /// from timeline sample `start` and runs the track's clean-up over it.
    /// An instrument plays before the clean-up, an effect plugin after it;
    /// `warm` (running the clean-up in before playback) leaves plugins out.
    fn render(&mut self, index: usize, start: i64, block: &mut Vec<f32>, frames: usize, warm: bool) {
        let end = start + frames as i64;
        let mut raw = std::mem::take(&mut self.raw);
        block.clear();
        block.resize(frames * 2, 0.0);
        // Clips are sorted: skip to the first one that reaches this block.
        let first = self.feeds[index].clips.partition_point(|c| c.start + c.len <= start);
        let mut i = first;
        while let Some(clip) = self.feeds[index].clips.get(i).cloned() {
            if clip.start >= end {
                break;
            }
            if let Some(wav) = self.wav(&clip.wav) {
                let from = clip.start.max(start);
                let to = (clip.start + clip.len).min(end);
                let out = &mut block[((from - start) * 2) as usize..((to - start) * 2) as usize];
                render_clip(&wav, &clip, from, out, &mut raw, &mut self.segment);
            }
            i += 1;
        }
        let instrument = self.is_instrument(index);
        if instrument && !warm {
            self.run_plugin(index, start, block);
        }
        let feed = &mut self.feeds[index];
        feed.fx.process(block, feed.low_cut_hz, feed.denoise, &feed.tone);
        if !instrument && !warm {
            self.run_plugin(index, start, block);
        }
        self.raw = raw;
    }

    /// Writes up to `limit` frames into feed `index`.
    fn fill(&mut self, index: usize, limit: usize) {
        let mut frames = (self.feeds[index].producer.slots() / 2).min(limit).min(MAX_BLOCK);
        let ducks = self.feeds[index].tone.duck_db > 0.0 && !self.feeds[index].tone.voice;
        if ducks {
            // Only as far as the voices have been laid down: their level there is needed.
            let voices = self.feeds.iter().filter(|f| f.tone.voice);
            if let Some(heard) = voices.map(|f| f.next).min() {
                frames = frames.min((heard - self.feeds[index].next).max(0) as usize);
            }
        }
        if frames == 0 {
            return;
        }
        // The clean-up hands sound back late; reading that much ahead puts
        // it where it belongs.
        let start = self.feeds[index].next + self.feeds[index].fx.delay() as i64;
        let mut block = std::mem::take(&mut self.block);
        self.render(index, start, &mut block, frames, false);
        let at = self.feeds[index].next;
        if self.feeds[index].tone.voice && !self.feeds[index].live.is_muted() {
            // What the voice sounds like here, fader included, for the tracks that duck.
            let gain = self.feeds[index].live.gain();
            for (n, frame) in block.as_chunks::<2>().0.iter().enumerate() {
                let step = (at + n as i64).div_euclid(DUCK_STEP);
                let slot = &mut self.voice_level[step.rem_euclid(DUCK_SLOTS as i64) as usize];
                if slot.0 != step {
                    *slot = (step, 0.0);
                }
                slot.1 = slot.1.max(frame[0].abs().max(frame[1].abs()) * gain);
            }
        } else if ducks {
            // Down within about 20 ms of the voice coming in, back up over
            // about 300 ms: fully from -30 dB of voice, not at all below -50.
            let rate = SAMPLE_RATE as f32;
            let (down, up) = (1.0 - (-1.0 / (0.02 * rate)).exp(), 1.0 - (-1.0 / (0.3 * rate)).exp());
            let depth = self.feeds[index].tone.duck_db.min(24.0);
            let mut ducked = self.feeds[index].ducked_db;
            for (n, frame) in block.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                let step = (at + n as i64).div_euclid(DUCK_STEP);
                let slot = self.voice_level[step.rem_euclid(DUCK_SLOTS as i64) as usize];
                let voice_db = if slot.0 == step { 20.0 * slot.1.max(1e-6).log10() } else { -120.0 };
                let wanted = -depth * ((voice_db + 50.0) / 20.0).clamp(0.0, 1.0);
                ducked += if wanted < ducked { down } else { up } * (wanted - ducked);
                let gain = 10f32.powf(ducked / 20.0);
                frame[0] *= gain;
                frame[1] *= gain;
            }
            self.feeds[index].ducked_db = ducked;
        }
        let feed = &mut self.feeds[index];
        if let Ok(mut chunk) = feed.producer.write_chunk(frames * 2) {
            let (a, b) = chunk.as_mut_slices();
            a.copy_from_slice(&block[..a.len()]);
            b.copy_from_slice(&block[a.len()..]);
            chunk.commit_all();
            feed.next += frames as i64;
        }
        self.block = block;
    }

    fn fill_all(&mut self, limit: usize) {
        // The voices first: the tracks that duck under them follow.
        for voices in [true, false] {
            for index in 0..self.feeds.len() {
                if self.feeds[index].tone.voice == voices {
                    self.fill(index, limit);
                }
            }
        }
    }

    /// Builds a graph for the current mix starting at `pos` and offers it.
    fn rebuild(&mut self, pos: i64, playing: bool, grain: Option<usize>) {
        // Files that failed to open are tried again: their audio may have
        // been conformed since.
        self.files.retain(|_, wav| wav.is_some());
        let mut tracks = Vec::with_capacity(self.mix.tracks.len());
        self.feeds.clear();
        // What the plugins were sounding belongs to the place playback left.
        for (_, client) in self.plugins.values_mut() {
            client.reset();
        }
        // (Positions start anew: what was heard of the voices before does not apply.)
        self.voice_level.iter_mut().for_each(|slot| *slot = (i64::MIN, 0.0));
        for track in &self.mix.tracks {
            let (producer, ring) = rtrb::RingBuffer::new(RING_FRAMES * 2);
            let live = Live::new(track.gain, track.pan, track.mute);
            live.set_sends(track.reverb, track.echo);
            // An unheard track keeps its own fader for what it does to
            // the others; what goes to the output is silence.
            let heard = if track.unheard { Live::new(0.0, 0.0, true) } else { live.clone() };
            let mut fader: Box<dyn Processor> = Box::new(Fader::new(heard.clone()));
            fader.prepare(SAMPLE_RATE, MAX_BLOCK);
            tracks.push(Track { ring, chain: vec![fader], live: heard });
            self.feeds.push(Feed {
                producer,
                clips: track.clips.clone(),
                next: pos,
                fx: TrackFx::new(track.denoise > 0.0, track.tone.tune.amount > 0.0),
                low_cut_hz: track.low_cut_hz,
                denoise: track.denoise,
                tone: track.tone,
                live,
                ducked_db: 0.0,
                plugin: track.plugin.as_ref().map(|p| p.key).filter(|key| self.plugins.contains_key(key)),
                awaiting: track.plugin.as_ref().is_some_and(|p| self.loading.contains_key(&p.key)),
                note_ons: track.notes.iter().map(|n| (n.start, n.pitch, n.velocity)).collect(),
                note_offs: {
                    let mut ends: Vec<(i64, u8)> = track.notes.iter().map(|n| (n.start + n.len, n.pitch)).collect();
                    ends.sort_unstable();
                    ends
                },
                synth: crate::synth::Synth::default(),
            });
        }
        if playing {
            // Run the clean-up over the stretch it will hold back, so that
            // what comes out first is the sound at `pos`.
            let mut block = std::mem::take(&mut self.block);
            for index in 0..self.feeds.len() {
                let delay = self.feeds[index].fx.delay();
                if delay > 0 {
                    self.render(index, pos, &mut block, delay, true);
                }
            }
            self.block = block;
            self.fill_all(grain.map_or(PREFILL_FRAMES, |frames| frames.max(PREFILL_FRAMES)));
        }
        self.master = Live::new(self.mix.master_gain, 0.0, false);
        let mut master: Box<dyn Processor> = Box::new(Fader::new(self.master.clone()));
        master.prepare(SAMPLE_RATE, MAX_BLOCK);
        let mut limiter: Box<dyn Processor> = Box::new(Limiter::new(self.shared.clone()));
        limiter.prepare(SAMPLE_RATE, MAX_BLOCK);
        let sends = self.mix.tracks.iter().any(|t| t.reverb > 0.0 || t.echo > 0.0);
        let space = sends.then(|| crate::space::Space::new(self.mix.echo_samples as usize));
        let click = self.mix.click;
        let graph = Box::new(Graph { tracks, master: vec![master, limiter], space, click, start: pos, playing, grain });
        // A graph offered earlier and never adopted is freed here.
        drop(self.slot.offer(graph));
        self.offered += 1;
        // A grain is filled once, before it is offered; it is not playback.
        self.playing = playing && grain.is_none();
    }

    /// Lets the device rest once the last grain has had its time.
    fn end_grain(&mut self) {
        self.grain_until = None;
        // (The grain's graph stays where it is: it has gone quiet by itself.)
        if let (false, Some(activate)) = (self.playing, &self.activate) {
            activate(false);
        }
    }

    fn handle(&mut self, control: Control) -> bool {
        match control {
            Control::Editor(key) => {
                let opened =
                    self.plugins.get_mut(&key).map(|(_, client)| (client.open_editor(), client.loaded.name.clone()));
                let said = match opened {
                    Some((true, _)) => None,
                    Some((false, name)) => Some(format!("{name} has no window of its own.")),
                    None if self.loading.contains_key(&key) => Some("The plugin is still starting.".to_owned()),
                    None => Some("The plugin is not running.".to_owned()),
                };
                if let Some(said) = said {
                    self.shared.plugin_news.lock().unwrap_or_else(PoisonError::into_inner).push(said);
                }
            }
            Control::SaveStates(reply) => {
                self.save_plugin_states();
                let _ = reply.send(());
            }
            Control::Mix(mix) => {
                let live_only = self.playing && same_structure(&self.mix, &mix);
                self.mix = mix;
                self.sync_plugins();
                if live_only {
                    // A fader moved: no new graph, so nothing is refilled
                    // and nothing can click.
                    for (feed, track) in self.feeds.iter_mut().zip(&self.mix.tracks) {
                        feed.live.set(track.gain, track.pan, track.mute);
                        feed.live.set_sends(track.reverb, track.echo);
                        (feed.low_cut_hz, feed.denoise, feed.tone) = (track.low_cut_hz, track.denoise, track.tone);
                    }
                    self.master.set(self.mix.master_gain, 0.0, false);
                } else if self.playing {
                    // Continue from what the callback will render next.
                    let pos = self.shared.clock_pos.load(Ordering::Relaxed)
                        + self.shared.quantum.load(Ordering::Relaxed) as i64;
                    self.rebuild(pos, true, None);
                }
            }
            Control::Play(pos) => {
                self.grain_until = None;
                self.rebuild(pos, true, None);
                if let Some(activate) = &self.activate {
                    activate(true);
                }
            }
            Control::Grain(pos) => {
                if !self.playing {
                    // The whole grain is in the rings before the graph is offered.
                    self.rebuild(pos, true, Some(GRAIN_FRAMES));
                    self.grain_until = Some(Instant::now() + GRAIN_LINGER);
                    if let Some(activate) = &self.activate {
                        activate(true);
                    }
                }
            }
            Control::Pause(pos) => {
                self.grain_until = None;
                self.rebuild(pos, false, None);
                if let Some(activate) = &self.activate {
                    // Give the callback one more cycle to go quiet, then stop
                    // driving the device graph at our quantum.
                    std::thread::sleep(Duration::from_millis(12));
                    if let Some(policy) = realtime_policy() {
                        self.shared.sched_policy.store(policy, Ordering::Relaxed);
                    }
                    activate(false);
                }
                self.shared.running.store(false, Ordering::Relaxed);
                // A good moment: nothing is heard that this could disturb.
                self.save_plugin_states();
            }
            Control::Loaded(spec, client) => {
                // (Taken off its track meanwhile: it is ended again by being dropped.)
                if self.loading.get(&spec.key) == Some(&spec.path) {
                    self.loading.remove(&spec.key);
                    match client {
                        Some(client) => {
                            self.plugins.insert(spec.key, (spec, *client));
                        }
                        None => {
                            self.failed.insert(spec.key, spec.path);
                        }
                    }
                    if self.playing {
                        // It joins from what the callback will render next.
                        let pos = self.shared.clock_pos.load(Ordering::Relaxed)
                            + self.shared.quantum.load(Ordering::Relaxed) as i64;
                        self.rebuild(pos, true, None);
                    }
                }
                if self.loading.is_empty() {
                    for reply in std::mem::take(&mut self.settles) {
                        self.fill_all(usize::MAX);
                        let _ = reply.send(());
                    }
                }
            }
            Control::Settle(reply) if !self.loading.is_empty() => self.settles.push(reply),
            Control::Settle(reply) => {
                self.fill_all(usize::MAX);
                // With a live backend, wait until the callback has the newest graph.
                let deadline = Instant::now() + Duration::from_millis(500);
                while self.activate.is_some()
                    && self.playing
                    && self.shared.adopted.load(Ordering::Acquire) < self.offered
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = reply.send(());
            }
            Control::Quit => {
                self.save_plugin_states();
                return false;
            }
        }
        true
    }
}

pub fn run(
    control: Receiver<Control>,
    sender: Sender<Control>,
    slot: Arc<Slot>,
    shared: Arc<Shared>,
    activate: Option<Box<dyn Fn(bool) + Send>>,
) {
    let mut disk = Disk {
        slot,
        shared,
        activate,
        mix: Mix::default(),
        feeds: Vec::new(),
        master: Live::new(1.0, 0.0, false),
        files: HashMap::new(),
        playing: false,
        grain_until: None,
        block: Vec::new(),
        raw: Vec::new(),
        segment: Vec::new(),
        offered: 0,
        voice_level: vec![(i64::MIN, 0.0); DUCK_SLOTS],
        plugins: HashMap::new(),
        loading: HashMap::new(),
        failed: HashMap::new(),
        control: sender,
        settles: Vec::new(),
        events: Vec::new(),
    };
    loop {
        // Idle costs nothing: without playback the thread sleeps until told otherwise.
        let message = if let Some(until) = disk.grain_until.filter(|_| !disk.playing) {
            match control.recv_deadline(until) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => {
                    disk.end_grain();
                    None
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else if disk.playing {
            match control.recv_timeout(FILL_INTERVAL) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match control.recv() {
                Ok(message) => Some(message),
                Err(_) => return,
            }
        };
        if let Some(message) = message
            && !disk.handle(message)
        {
            return;
        }
        if disk.playing {
            disk.fill_all(usize::MAX);
        }
        // Free graphs the callback has finished with.
        drop(disk.slot.collect());
    }
}

/// Scheduling policy of PipeWire's data thread, read from /proc (the realtime
/// thread itself must not make the syscall). 0 = normal, 1 = FIFO, 2 = RR.
fn realtime_policy() -> Option<i64> {
    for task in std::fs::read_dir("/proc/self/task").ok()?.flatten() {
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        if name.trim().starts_with("data-loop") {
            let stat = std::fs::read_to_string(task.path().join("stat")).ok()?;
            // Field 41 of stat, counted after the parenthesised name.
            return stat.rsplit_once(')')?.1.split_ascii_whitespace().nth(38)?.parse().ok();
        }
    }
    None
}

/// The level in decibels that volume keyframes give at `at` samples into the clip.
fn key_level(keys: &[(i64, f32)], at: i64) -> f32 {
    let after = keys.partition_point(|k| k.0 <= at);
    match (after.checked_sub(1).map(|i| keys[i]), keys.get(after)) {
        (None, None) => 0.0,
        (Some(k), None) | (None, Some(&k)) => k.1,
        (Some(a), Some(b)) => a.1 + (b.1 - a.1) * ((at - a.0) as f32 / (b.0 - a.0) as f32),
    }
}

/// Source samples consumed `at` samples into a clip with a changing speed.
/// Outside the points the nearest stretch is continued.
fn warp_at(warp: &[(i64, f64)], at: i64) -> f64 {
    let after = warp.partition_point(|p| p.0 <= at).clamp(1, warp.len() - 1);
    let (a, b) = (warp[after - 1], warp[after]);
    a.1 + (b.1 - a.1) * ((at - a.0) as f64 / (b.0 - a.0).max(1) as f64)
}

/// Adds the part of `clip` that starts at timeline sample `from` to `out`
/// (interleaved stereo), with the clip's speed, gain and fades applied.
fn render_clip(wav: &Wav, clip: &ClipSpan, from: i64, out: &mut [f32], raw: &mut Vec<u8>, segment: &mut Vec<f32>) {
    let frames = out.len() / 2;
    let elapsed = from - clip.start;
    segment.clear();
    segment.resize(out.len(), 0.0);
    let (num, den) = (clip.speed.0.max(1) as i64, clip.speed.1.max(1) as i64);
    if clip.warp.len() >= 2 {
        // A speed that changes along the clip: each output sample is picked
        // from where the curve says the source has got to.
        let first = warp_at(&clip.warp, elapsed).floor().max(0.0);
        let last = warp_at(&clip.warp, elapsed + frames as i64);
        let needed = (last - first).max(0.0).ceil() as usize + 3;
        let mut source = vec![0.0f32; needed * 2];
        wav.mix_into(clip.src_in + first as i64, &mut source, 1.0, raw);
        for (i, frame) in segment.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let position = (warp_at(&clip.warp, elapsed + i as i64) - first).max(0.0);
            let index = (position.floor() as usize).min(needed - 2);
            let t = (position - index as f64).clamp(0.0, 1.0) as f32;
            for (c, sample) in frame.iter_mut().enumerate() {
                *sample = source[index * 2 + c] * (1.0 - t) + source[index * 2 + 2 + c] * t;
            }
        }
    } else if num == den {
        wav.mix_into(clip.src_in + elapsed, segment, 1.0, raw);
    } else {
        // Read the stretch of source this block covers, then pick each
        // output sample between its two neighbours.
        let first = clip.src_in + elapsed * num / den;
        let needed = (frames as i64 * num / den + 3) as usize;
        let mut source = vec![0.0f32; needed * 2];
        wav.mix_into(first, &mut source, 1.0, raw);
        for (i, frame) in segment.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let position = (elapsed + i as i64) as f64 * num as f64 / den as f64 - (first - clip.src_in) as f64;
            let index = (position.floor().max(0.0) as usize).min(needed - 2);
            let t = (position - index as f64).clamp(0.0, 1.0) as f32;
            for (c, sample) in frame.iter_mut().enumerate() {
                *sample = source[index * 2 + c] * (1.0 - t) + source[index * 2 + 2 + c] * t;
            }
        }
    }
    let faded = clip.fade_in > 0 || clip.fade_out > 0;
    for (i, (o, s)) in out.as_chunks_mut::<2>().0.iter_mut().zip(segment.as_chunks::<2>().0).enumerate() {
        let mut gain = clip.gain;
        if !clip.gain_keys.is_empty() {
            gain *= 10f32.powf(key_level(&clip.gain_keys, elapsed + i as i64) / 20.0);
        }
        if faded {
            let at = elapsed + i as i64;
            if at < clip.fade_in {
                gain *= at.max(0) as f32 / clip.fade_in as f32;
            }
            if clip.len - at < clip.fade_out {
                gain *= (clip.len - at).max(0) as f32 / clip.fade_out as f32;
            }
        }
        o[0] += s[0] * gain;
        o[1] += s[1] * gain;
    }
}
