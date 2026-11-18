//! The disk thread: turns a [`Mix`] into graphs and keeps their ring
//! buffers filled from the conformed WAV files. Everything that allocates
//! or blocks on behalf of the realtime thread happens here.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};

use crate::graph::{GainPan, Graph, MAX_BLOCK, Processor, Slot, Track};
use crate::{ClipSpan, Mix, SAMPLE_RATE, Shared};

pub enum Control {
    Mix(Mix),
    Play(i64),
    Pause(i64),
    /// Reply once everything sent before is in place and buffered.
    Settle(Sender<()>),
    Quit,
}

/// Frames buffered ahead per track: half a second.
const RING_FRAMES: usize = SAMPLE_RATE as usize / 2;
/// Frames written before a graph is offered, so playback starts with data.
const PREFILL_FRAMES: usize = 4096;
/// How often the rings are topped up while playing.
const FILL_INTERVAL: Duration = Duration::from_millis(5);

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
}

struct Disk {
    slot: Arc<Slot>,
    shared: Arc<Shared>,
    activate: Option<Box<dyn Fn(bool) + Send>>,
    mix: Mix,
    feeds: Vec<Feed>,
    files: HashMap<PathBuf, Option<Arc<Wav>>>,
    playing: bool,
    block: Vec<f32>,
    raw: Vec<u8>,
    offered: u64,
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

    /// Writes up to `limit` frames into feed `index`.
    fn fill(&mut self, index: usize, limit: usize) {
        let frames = (self.feeds[index].producer.slots() / 2).min(limit).min(MAX_BLOCK);
        if frames == 0 {
            return;
        }
        let start = self.feeds[index].next;
        let end = start + frames as i64;
        let mut block = std::mem::take(&mut self.block);
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
                wav.mix_into(clip.src_in + (from - clip.start), out, clip.gain, &mut raw);
            }
            i += 1;
        }
        let feed = &mut self.feeds[index];
        if let Ok(mut chunk) = feed.producer.write_chunk(frames * 2) {
            let (a, b) = chunk.as_mut_slices();
            a.copy_from_slice(&block[..a.len()]);
            b.copy_from_slice(&block[a.len()..]);
            chunk.commit_all();
            feed.next = end;
        }
        self.block = block;
        self.raw = raw;
    }

    fn fill_all(&mut self, limit: usize) {
        for index in 0..self.feeds.len() {
            self.fill(index, limit);
        }
    }

    /// Builds a graph for the current mix starting at `pos` and offers it.
    fn rebuild(&mut self, pos: i64, playing: bool) {
        // Files that failed to open are tried again: their audio may have
        // been conformed since.
        self.files.retain(|_, wav| wav.is_some());
        let mut tracks = Vec::with_capacity(self.mix.tracks.len());
        self.feeds.clear();
        for track in &self.mix.tracks {
            let (producer, ring) = rtrb::RingBuffer::new(RING_FRAMES * 2);
            let mut fader: Box<dyn Processor> = Box::new(GainPan::new(track.gain, track.pan));
            fader.prepare(SAMPLE_RATE, MAX_BLOCK);
            tracks.push(Track { ring, chain: vec![fader], mute: track.mute });
            self.feeds.push(Feed { producer, clips: track.clips.clone(), next: pos });
        }
        if playing {
            self.fill_all(PREFILL_FRAMES);
        }
        let graph = Box::new(Graph { tracks, master: Vec::new(), start: pos, playing });
        // A graph offered earlier and never adopted is freed here.
        drop(self.slot.offer(graph));
        self.offered += 1;
        self.playing = playing;
    }

    fn handle(&mut self, control: Control) -> bool {
        match control {
            Control::Mix(mix) => {
                self.mix = mix;
                if self.playing {
                    // Continue from what the callback will render next.
                    let pos = self.shared.clock_pos.load(Ordering::Relaxed)
                        + self.shared.quantum.load(Ordering::Relaxed) as i64;
                    self.rebuild(pos, true);
                }
            }
            Control::Play(pos) => {
                self.rebuild(pos, true);
                if let Some(activate) = &self.activate {
                    activate(true);
                }
            }
            Control::Pause(pos) => {
                self.rebuild(pos, false);
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
            }
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
            Control::Quit => return false,
        }
        true
    }
}

pub fn run(
    control: Receiver<Control>,
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
        files: HashMap::new(),
        playing: false,
        block: Vec::new(),
        raw: Vec::new(),
        offered: 0,
    };
    loop {
        // Idle costs nothing: without playback the thread sleeps until told otherwise.
        let message = if disk.playing {
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
