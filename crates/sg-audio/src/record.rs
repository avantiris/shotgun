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

use std::io::{Seek, SeekFrom, Write};
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

fn run(
    data: Capture,
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
            *pw::keys::NODE_NAME => "shotgun-record",
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
        let stream = pw::stream::StreamRc::new(core.clone(), "shotgun-record", props).map_err(|e| e.to_string())?;
        Ok((mainloop, context, core, stream))
    };
    let (mainloop, _context, _core, stream) = match setup() {
        Ok(parts) => parts,
        Err(e) => return fail(e),
    };
    let listener = stream.add_local_listener_with_user_data(data).process(capture).register();
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

fn write(path: &Path, mut ring: rtrb::Consumer<f32>, meeting: &Meeting) -> std::io::Result<u64> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    file.write_all(&header(0))?;
    let mut samples = 0u64;
    let mut bytes = Vec::new();
    loop {
        let stopping = meeting.stop.load(Ordering::Acquire);
        let ready = ring.slots();
        if ready > 0
            && let Ok(chunk) = ring.read_chunk(ready)
        {
            bytes.clear();
            let (a, b) = chunk.as_slices();
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
    ) -> Result<Recording, AudioError> {
        // Four seconds of room between the callback and the writer.
        let (producer, consumer) = rtrb::RingBuffer::new(SAMPLE_RATE as usize * 8);
        let meeting = Arc::new(Meeting::default());
        let data = Capture { ring: producer, meeting: meeting.clone(), heard };
        let (quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let capture = std::thread::Builder::new()
            .name("sg-audio-record".into())
            .spawn(move || run(data, source, quit_rx, ready_tx))?;
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
