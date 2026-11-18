//! PipeWire backend: one playback `pw_stream`, as decided in spike S3.
//! The PipeWire objects are not `Send`, so they live on their own thread
//! with the main loop; the realtime callback runs on PipeWire's data thread.

use std::sync::atomic::Ordering;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;

use crate::graph::Rt;
use crate::{AudioError, SAMPLE_RATE};

enum Message {
    Active(bool),
    Quit,
}

pub struct PipeWire {
    sender: pw::channel::Sender<Message>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct UserData {
    rt: Rt,
    /// Driver clock, handed over in `io_changed`; null until then.
    position: *mut spa::sys::spa_io_position,
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

/// The realtime callback.
fn process(stream: &pw::stream::Stream, data: &mut UserData) {
    let shared = data.rt.shared_for_backend();
    if !data.position.is_null() {
        // SAFETY: PipeWire keeps the io area valid while the stream exists
        // and updates it before each cycle; this thread only reads it.
        let clock = unsafe { &(*data.position).clock };
        shared.xruns.store(clock.xrun, Ordering::Relaxed);
    }
    // pw_stream_get_time_n is documented as realtime-safe.
    let (now, delay) = match stream.time() {
        Ok(t) => (t.now().max(0) as u64, t.delay() + t.buffered() as i64),
        Err(_) => (crate::monotonic_ns(), 0),
    };
    let Some(mut buffer) = stream.dequeue_buffer() else { return };
    let requested = buffer.requested() as usize;
    let Some(d) = buffer.datas_mut().first_mut() else { return };
    let mut frames = 0;
    if let Some(bytes) = d.data() {
        frames = bytes.len() / 8;
        if requested > 0 {
            frames = frames.min(requested);
        }
        // SAFETY: the buffer is PipeWire's mapped, page-aligned memory of at
        // least `frames * 8` bytes, negotiated as interleaved f32.
        let samples: &mut [f32] = unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast(), frames * 2) };
        data.rt.process(samples, now, delay);
    }
    let chunk = d.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = 8;
    *chunk.size_mut() = (frames * 8) as u32;
}

fn run(
    rt: Rt,
    target: Option<String>,
    quantum: u32,
    receiver: pw::channel::Receiver<Message>,
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
        let latency = format!("{quantum}/{SAMPLE_RATE}");
        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::NODE_NAME => "shotgun",
            *pw::keys::APP_NAME => "Shotgun",
            *pw::keys::NODE_LATENCY => latency.as_str(),
            *pw::keys::NODE_RATE => "1/48000",
        };
        if let Some(target) = &target {
            props.insert(*pw::keys::TARGET_OBJECT, target.as_str());
        }
        let stream = pw::stream::StreamRc::new(core.clone(), "shotgun-playback", props).map_err(|e| e.to_string())?;
        Ok((mainloop, context, core, stream))
    };
    let (mainloop, _context, _core, stream) = match setup() {
        Ok(parts) => parts,
        Err(e) => return fail(e),
    };
    let listener = stream
        .add_local_listener_with_user_data(UserData { rt, position: std::ptr::null_mut() })
        .io_changed(|_, data, id, area, _size| {
            if id == spa::sys::SPA_IO_Position {
                data.position = area.cast();
            }
        })
        .process(process)
        .register();
    let _listener = match listener {
        Ok(listener) => listener,
        Err(e) => return fail(e.to_string()),
    };
    let Some(format) = format_pod() else { return fail("cannot describe the audio format".into()) };
    let Some(pod) = spa::pod::Pod::from_bytes(&format) else { return fail("cannot describe the audio format".into()) };
    // RT_PROCESS: without it the node runs one quantum late on a normal thread (S3).
    let flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    if let Err(e) = stream.connect(spa::utils::Direction::Output, None, flags, &mut [pod]) {
        return fail(e.to_string());
    }
    // Start parked: while the stream is active the whole device graph runs
    // at our quantum, so it is only active during playback.
    let _ = stream.set_active(false);
    let quit_loop = mainloop.clone();
    let _receiver = receiver.attach(mainloop.loop_(), move |message| match message {
        Message::Active(active) => {
            let _ = stream.set_active(active);
        }
        Message::Quit => quit_loop.quit(),
    });
    let _ = ready.send(Ok(()));
    mainloop.run();
}

impl PipeWire {
    pub fn start(rt: Rt, target: Option<String>, quantum: u32) -> Result<PipeWire, AudioError> {
        let (sender, receiver) = pw::channel::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("sg-audio-pipewire".into())
            .spawn(move || run(rt, target, quantum, receiver, ready_tx))?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(PipeWire { sender, thread: Some(thread) }),
            Ok(Err(e)) => Err(AudioError::PipeWire(e)),
            Err(_) => Err(AudioError::PipeWire("no answer from the PipeWire thread".into())),
        }
    }

    /// A function that starts and stops the device stream, callable from any thread.
    pub fn activator(&self) -> Box<dyn Fn(bool) + Send> {
        let sender = self.sender.clone();
        Box::new(move |active| {
            let _ = sender.send(Message::Active(active));
        })
    }
}

impl Drop for PipeWire {
    fn drop(&mut self) {
        let _ = self.sender.send(Message::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
