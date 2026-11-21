//! Recording (docs/spec/09-daw.md, M13): a PipeWire capture stream whose
//! sound is written to a WAV file, with the timeline position its first
//! frame belongs to.
//!
//! The capture callback runs on PipeWire's realtime thread: it copies
//! samples into a lock-free ring and reads a few atomics, nothing else. A
//! writer thread empties the ring into the file.
//!
//! Where a take belongs on the timeline: what the performer plays at an
//! instant belongs with what they *hear* at that instant. So the first
//! captured frame is given the heard position at the moment it arrives,
//! less the frames of its own buffer and the capture stream's delay.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

use pipewire as pw;
use pw::properties::properties;
use pw::spa;

use crate::{AudioError, SAMPLE_RATE};

/// A finished recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Take {
    /// 48 kHz stereo float WAV.
    pub path: PathBuf,
    /// The timeline sample its first frame belongs to.
    pub start: i64,
    pub frames: u64,
    /// Frames that were lost because the writer could not keep up (0 normally).
    pub lost: u64,
}

#[derive(Default)]
struct Meeting {
    /// The timeline sample of the first captured frame, once known.
    start: AtomicI64,
    started: AtomicBool,
    lost: AtomicU64,
    stop: AtomicBool,
}

pub struct Recording {
    quit: pw::channel::Sender<()>,
    capture: Option<std::thread::JoinHandle<()>>,
    writer: Option<std::thread::JoinHandle<std::io::Result<u64>>>,
    meeting: Arc<Meeting>,
    path: PathBuf,
}

struct Capture {
    ring: rtrb::Producer<f32>,
    meeting: Arc<Meeting>,
    heard: Box<dyn Fn() -> Option<i64> + Send>,
    /// Told the peak of every buffer that arrives (for a meter): like
    /// `heard`, on the realtime thread, and must only touch atomics.
    level: Box<dyn Fn(f32) + Send>,
}

/// The realtime callback of the capture stream.
fn capture(stream: &pw::stream::Stream, data: &mut Capture) {
    let Some(mut buffer) = stream.dequeue_buffer() else { return };
    let Some(d) = buffer.datas_mut().first_mut() else { return };
    let (offset, size) = (d.chunk().offset() as usize, d.chunk().size() as usize);
    let Some(bytes) = d.data() else { return };
    let Some(bytes) = bytes.get(offset..offset + size) else { return };
    let frames = bytes.len() / 8;
    if frames == 0 {
        return;
    }
    if !data.meeting.started.load(Ordering::Relaxed) {
        // Before playback runs there is nothing to belong to: what arrives
        // then is not part of the take.
        let Some(heard) = (data.heard)() else { return };
        // pw_stream_get_time_n is documented as realtime-safe. For a
        // capture stream the delay is how old the samples are.
        let age = stream.time().map_or(0, |t| t.delay());
        data.meeting.start.store(heard - frames as i64 - age, Ordering::Relaxed);
        data.meeting.started.store(true, Ordering::Release);
    }
    // SAFETY: the buffer is PipeWire's mapped memory, negotiated as
    // interleaved 32-bit float; `bytes` was bounds-checked above.
    let samples: &[f32] = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast(), frames * 2) };
    (data.level)(samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs())));
    match data.ring.write_chunk_uninit(samples.len()) {
        Ok(chunk) => {
            chunk.fill_from_iter(samples.iter().copied());
        }
        // The writer is behind: this buffer is lost, and counted.
        Err(_) => {
            data.meeting.lost.fetch_add(frames as u64, Ordering::Relaxed);
        }
    }
}

fn format_pod() -> Option<Vec<u8>> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(2);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let object = spa::pod::Object {
        type_: spa::sys::SPA_TYPE_OBJECT_Format,
        id: spa::sys::SPA_PARAM_EnumFormat,
        properties: info.into(),
    };
    let cursor = std::io::Cursor::new(Vec::new());
    spa::pod::serialize::PodSerializer::serialize(cursor, &spa::pod::Value::Object(object))
        .ok()
        .map(|(c, _)| c.into_inner())
}

/// Runs a capture stream on this thread until `quit` says otherwise;
/// `process` is its realtime callback, with `data` as its own.
pub(crate) fn run<D: 'static>(
    data: D,
    process: fn(&pw::stream::Stream, &mut D),
    name: &str,
    source: Option<String>,
    quit: pw::channel::Receiver<()>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let fail = |e: String| {
        let _ = ready.send(Err(e));
    };
    pw::init();
    let setup = || -> Result<_, String> {
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
        let core = context.connect_rc(None).map_err(|e| format!("cannot connect to the PipeWire daemon: {e}"))?;
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::NODE_NAME => name,
            *pw::keys::APP_NAME => "Shotgun",
            *pw::keys::NODE_LATENCY => "256/48000",
            *pw::keys::NODE_RATE => "1/48000",
        };
        // "<sink>.monitor" records what a sink plays (tests use a null sink).
        match source.as_deref().map(|s| (s.strip_suffix(".monitor"), s)) {
            Some((Some(sink), _)) => {
                props.insert(*pw::keys::TARGET_OBJECT, sink);
                props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
            }
            Some((None, source)) => props.insert(*pw::keys::TARGET_OBJECT, source),
            None => {}
        }
        let stream = pw::stream::StreamRc::new(core.clone(), name, props).map_err(|e| e.to_string())?;
        Ok((mainloop, context, core, stream))
    };
    let (mainloop, _context, _core, stream) = match setup() {
        Ok(parts) => parts,
        Err(e) => return fail(e),
    };
    let listener = stream.add_local_listener_with_user_data(data).process(process).register();
    let _listener = match listener {
        Ok(listener) => listener,
        Err(e) => return fail(e.to_string()),
    };
    let Some(format) = format_pod() else { return fail("cannot describe the audio format".into()) };
    let Some(pod) = spa::pod::Pod::from_bytes(&format) else { return fail("cannot describe the audio format".into()) };
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    if let Err(e) = stream.connect(spa::utils::Direction::Input, None, flags, &mut [pod]) {
        return fail(e.to_string());
    }
    let quit_loop = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| quit_loop.quit());
    let _ = ready.send(Ok(()));
    mainloop.run();
}

/// A float WAV header for stereo at the engine's rate; the sizes are filled in at the end.
fn header(frames: u64) -> Vec<u8> {
    let data = (frames * 8).min(u32::MAX as u64 - 64) as u32;
    let mut bytes = Vec::with_capacity(44);
    bytes.extend(b"RIFF");
    bytes.extend((36 + data).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(SAMPLE_RATE.to_le_bytes());
    bytes.extend((SAMPLE_RATE * 8).to_le_bytes());
    bytes.extend(8u16.to_le_bytes());
    bytes.extend(32u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data.to_le_bytes());
    bytes
}

/// Which side alone carries the sound, given the peak of each: a
/// microphone on one input of a two-input interface arrives on the left
/// (or the right) only, and would be recorded and heard in one ear
/// (feedback F16.12). The other side must be 50 dB below it, which the
/// noise of an unused input is and a quiet stereo side is not.
pub(crate) fn lone_side(peaks: [f32; 2]) -> Option<usize> {
    let loud = if peaks[0] >= peaks[1] { 0 } else { 1 };
    (peaks[loud] > 1e-3 && peaks[1 - loud] < peaks[loud] * 0.003).then_some(loud)
}

/// Puts the sound of one side of a take's file on both sides.
fn spread(path: &Path, side: usize) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let start = header(0).len() as u64;
    let mut bytes = vec![0u8; 8 * 16_384];
    let mut at = start;
    loop {
        file.seek(SeekFrom::Start(at))?;
        let mut got = 0;
        while got < bytes.len() {
            match file.read(&mut bytes[got..])? {
                0 => break,
                n => got += n,
            }
        }
        let whole = got - got % 8;
        if whole == 0 {
            return Ok(());
        }
        for frame in bytes[..whole].chunks_exact_mut(8) {
            let (left, right) = frame.split_at_mut(4);
            if side == 0 { right.copy_from_slice(left) } else { left.copy_from_slice(right) }
        }
        file.seek(SeekFrom::Start(at))?;
        file.write_all(&bytes[..whole])?;
        at += whole as u64;
    }
}

fn write(path: &Path, mut ring: rtrb::Consumer<f32>, meeting: &Meeting) -> std::io::Result<u64> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(&header(0))?;
    let mut samples = 0u64;
    let mut bytes = Vec::new();
    let mut peaks = [0.0f32; 2];
    loop {
        let stopping = meeting.stop.load(Ordering::Acquire);
        let ready = ring.slots();
        if ready > 0
            && let Ok(chunk) = ring.read_chunk(ready)
        {
            bytes.clear();
            let (a, b) = chunk.as_slices();
            for (index, sample) in a.iter().chain(b).enumerate() {
                let side = (samples as usize + index) % 2;
                peaks[side] = peaks[side].max(sample.abs());
            }
            bytes.extend(a.iter().chain(b).flat_map(|s| s.to_le_bytes()));
            chunk.commit_all();
            file.write_all(&bytes)?;
            samples += ready as u64;
        }
        if stopping && ring.slots() == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(samples / 2))?;
    file.flush()?;
    drop(file);
    // Sound on one side only: the take gets it on both.
    if let Some(side) = lone_side(peaks) {
        spread(path, side)?;
    }
    Ok(samples / 2)
}

impl Recording {
    /// Starts capturing from `source` (a PipeWire node; `None`: the
    /// default input) into `path`. `heard` gives the timeline sample being
    /// heard at the instant it is called, or `None` while nothing plays
    /// (the take begins when playback does); it runs on the realtime
    /// thread and must only read atomics.
    pub fn start(
        path: PathBuf,
        source: Option<String>,
        heard: Box<dyn Fn() -> Option<i64> + Send>,
        level: Box<dyn Fn(f32) + Send>,
    ) -> Result<Recording, AudioError> {
        // Four seconds of room between the callback and the writer.
        let (producer, consumer) = rtrb::RingBuffer::new(SAMPLE_RATE as usize * 8);
        let meeting = Arc::new(Meeting::default());
        let data = Capture { ring: producer, meeting: meeting.clone(), heard, level };
        let (quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let capture = std::thread::Builder::new()
            .name("sg-audio-record".into())
            .spawn(move || run(data, capture, "shotgun-record", source, quit_rx, ready_tx))?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(AudioError::PipeWire(e)),
            Err(_) => return Err(AudioError::PipeWire("no answer from the recording thread".into())),
        }
        let (file, shared) = (path.clone(), meeting.clone());
        let writer =
            std::thread::Builder::new().name("sg-audio-take".into()).spawn(move || write(&file, consumer, &shared))?;
        Ok(Recording { quit, capture: Some(capture), writer: Some(writer), meeting, path })
    }

    /// Ends the recording and waits for the file to be complete.
    pub fn stop(mut self) -> Result<Take, AudioError> {
        let _ = self.quit.send(());
        if let Some(capture) = self.capture.take() {
            let _ = capture.join();
        }
        self.meeting.stop.store(true, Ordering::Release);
        let written = match self.writer.take().map(std::thread::JoinHandle::join) {
            Some(Ok(result)) => result?,
            _ => return Err(AudioError::PipeWire("the take was not written".into())),
        };
        Ok(Take {
            path: self.path.clone(),
            start: self.meeting.start.load(Ordering::Relaxed),
            frames: written,
            lost: self.meeting.lost.load(Ordering::Relaxed),
        })
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        // Dropped without `stop`: end the threads, keep what was written.
        let _ = self.quit.send(());
        self.meeting.stop.store(true, Ordering::Release);
        if let Some(capture) = self.capture.take() {
            let _ = capture.join();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Something sound can be recorded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// The PipeWire node name: what [`Recording::start`] takes as `source`.
    pub name: String,
    /// What the desktop calls it.
    pub label: String,
}

fn list_inputs() -> Result<Vec<Input>, String> {
    use std::cell::RefCell;
    use std::rc::Rc;
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| e.to_string())?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_rc(None).map_err(|e| format!("cannot connect to the PipeWire daemon: {e}"))?;
    let registry = core.get_registry().map_err(|e| e.to_string())?;
    let found = Rc::new(RefCell::new(Vec::new()));
    // The daemon answers the sync after it has announced every object.
    let pending = core.sync(0).map_err(|e| e.to_string())?;
    let quit = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                quit.quit();
            }
        })
        .register();
    let list = found.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props else { return };
            // Microphones and interfaces; not the monitors of outputs.
            if !matches!(props.get("media.class"), Some("Audio/Source" | "Audio/Duplex" | "Audio/Source/Virtual")) {
                return;
            }
            let Some(name) = props.get("node.name") else { return };
            let label = props.get("node.description").or_else(|| props.get("node.nick")).unwrap_or(name);
            list.borrow_mut().push(Input { name: name.to_owned(), label: label.to_owned() });
        })
        .register();
    mainloop.run();
    let mut inputs = found.take();
    inputs.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(inputs)
}

/// The inputs PipeWire offers now. Asks the daemon and waits for its
/// answer (a few milliseconds; three seconds at most): not for the UI thread.
pub fn inputs() -> Result<Vec<Input>, AudioError> {
    let (tx, rx) = std::sync::mpsc::channel();
    // A thread of its own so that a daemon that never answers cannot hold the caller.
    std::thread::Builder::new().name("sg-audio-inputs".into()).spawn(move || {
        let _ = tx.send(list_inputs());
    })?;
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(result) => result.map_err(AudioError::PipeWire),
        Err(_) => Err(AudioError::PipeWire("no answer from the PipeWire daemon".into())),
    }
}

/// The input as it arrives, for the level meter and for hearing oneself
/// (docs/spec/09-daw.md, M13): a capture stream whose samples go straight
/// to the playback callback through a small ring. Ends when dropped.
pub struct Monitor {
    quit: pw::channel::Sender<()>,
    capture: Option<std::thread::JoinHandle<()>>,
    pub(crate) control: Option<crossbeam_channel::Sender<crate::disk::Control>>,
}

/// The realtime callback of the monitor stream: copies what arrived into the ring.
fn pass_on(stream: &pw::stream::Stream, ring: &mut rtrb::Producer<f32>) {
    let Some(mut buffer) = stream.dequeue_buffer() else { return };
    let Some(d) = buffer.datas_mut().first_mut() else { return };
    let (offset, size) = (d.chunk().offset() as usize, d.chunk().size() as usize);
    let Some(bytes) = d.data() else { return };
    let Some(bytes) = bytes.get(offset..offset + size) else { return };
    let frames = bytes.len() / 8;
    // SAFETY: the buffer is PipeWire's mapped memory, negotiated as
    // interleaved 32-bit float; `bytes` was bounds-checked above.
    let samples: &[f32] = unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast(), frames * 2) };
    // (A full ring: the playback side is not running. What does not fit is dropped.)
    if let Ok(chunk) = ring.write_chunk_uninit(samples.len().min(ring.slots() & !1)) {
        chunk.fill_from_iter(samples.iter().copied());
    }
}

impl Monitor {
    /// Starts capturing from `source` (`None`: the default input) into `ring`.
    pub(crate) fn start(ring: rtrb::Producer<f32>, source: Option<String>) -> Result<Monitor, AudioError> {
        let (quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let capture = std::thread::Builder::new()
            .name("sg-audio-monitor".into())
            .spawn(move || run(ring, pass_on, "shotgun-monitor", source, quit_rx, ready_tx))?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Monitor { quit, capture: Some(capture), control: None }),
            Ok(Err(e)) => Err(AudioError::PipeWire(e)),
            Err(_) => Err(AudioError::PipeWire("no answer from the monitor thread".into())),
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(capture) = self.capture.take() {
            let _ = capture.join();
        }
        // The device may rest again if nothing else needs it.
        if let Some(control) = &self.control {
            let _ = control.send(crate::disk::Control::Monitor(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_take_with_sound_on_one_side_only_gets_it_on_both() {
        assert_eq!(lone_side([0.4, 0.0]), Some(0));
        assert_eq!(lone_side([0.0001, 0.3]), Some(1));
        // A quiet side of a stereo recording, and silence, stay as they are.
        assert_eq!(lone_side([0.4, 0.02]), None);
        assert_eq!(lone_side([0.0, 0.0]), None);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        let frames: Vec<[f32; 2]> = (0..40_000).map(|n| [0.0, (n as f32 * 0.01).sin() * 0.5]).collect();
        let mut bytes = header(frames.len() as u64);
        bytes.extend(frames.iter().flatten().flat_map(|s| s.to_le_bytes()));
        std::fs::write(&path, &bytes).unwrap();
        spread(&path, 1).unwrap();
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after.len(), bytes.len());
        assert_eq!(after[..44], bytes[..44]);
        for (frame, was) in after[44..].chunks_exact(8).zip(&frames) {
            let side = |at: usize| f32::from_le_bytes([frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]);
            assert_eq!((side(0), side(4)), (was[1], was[1]));
        }
    }
}
