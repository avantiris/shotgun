//! The process a plugin lives in. Started by [`sg_plug::Client`], which
//! writes requests to its standard input and reads the answers from its
//! standard output; see the library for why and the tags for what.
//!
//! One thread does everything: it waits for a request, for the plugin's
//! own file descriptors and timers (the run loop a Linux VST3 plugin asks
//! its host for) and for the editor window's events, whichever comes first.
//!
//! The whole program is an FFI boundary: a plugin is foreign code loaded
//! into the process and driven through COM interfaces (VST3, here) or a
//! table of C functions (CLAP, in `clap.rs`).

mod clap;

use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sg_plug::{MAX_FRAMES, tag};
use vst3::Steinberg::Linux::*;
use vst3::Steinberg::Vst::BusDirections_::*;
use vst3::Steinberg::Vst::MediaTypes_::*;
use vst3::Steinberg::Vst::ProcessModes_::*;
use vst3::Steinberg::Vst::SymbolicSampleSizes_::*;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};
use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, CreateWindowAux, EventMask, PropMode, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const SAMPLE_RATE: f64 = 48_000.0;

/// A plugin of either format, as the requests see it.
trait Hosted {
    fn name(&self) -> &str;
    fn instrument(&self) -> bool;
    fn has_editor(&self) -> bool;
    /// Interleaved stereo in place; notes as (frame, on, pitch, velocity).
    fn process(&mut self, sound: &mut [f32], notes: &[(u32, bool, u8, u8)]) -> bool;
    fn reset(&mut self);
    /// The tempo of the song in beats per minute, and the timeline
    /// sample the next block starts at.
    fn set_tempo(&mut self, bpm: f64, position: i64);
    fn state(&mut self) -> Vec<u8>;
    fn set_state(&mut self, state: &[u8]) -> bool;
    fn open_editor(&mut self) -> Result<(), String>;
    fn close_editor(&mut self);
    /// What the plugin and its window are waiting for, done once.
    /// Returns how long the next wait may last.
    fn turn(&mut self) -> Duration;
}

/// An X11 window for a plugin to put its own into: the connection, the
/// window and the atom by which the desktop asks to close it.
fn make_window(name: &str, size: (u16, u16)) -> Result<(RustConnection, u32, u32), String> {
    let (x, screen) = RustConnection::connect(None).map_err(|e| format!("no X11 display ({e})"))?;
    let root = &x.setup().roots[screen];
    let window = x.generate_id().map_err(|e| e.to_string())?;
    let events = EventMask::STRUCTURE_NOTIFY | EventMask::EXPOSURE;
    let made = (|| {
        x.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root.root,
            0,
            0,
            size.0,
            size.1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(events).background_pixel(root.black_pixel),
        )?;
        x.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, name.as_bytes())?;
        // Automated runs give their windows a class the desktop keeps out of sight.
        let class = std::env::var("SHOTGUN_APP_ID").unwrap_or_else(|_| "shotgun-plugin".to_owned());
        let both = format!("{class}\0{class}\0");
        x.change_property8(PropMode::REPLACE, window, AtomEnum::WM_CLASS, AtomEnum::STRING, both.as_bytes())?;
        let protocols = x.intern_atom(false, b"WM_PROTOCOLS")?.reply()?.atom;
        let delete = x.intern_atom(false, b"WM_DELETE_WINDOW")?.reply()?.atom;
        x.change_property32(PropMode::REPLACE, window, protocols, AtomEnum::ATOM, &[delete])?;
        x.map_window(window)?;
        x.flush()?;
        Ok::<u32, Box<dyn std::error::Error>>(delete)
    })();
    let delete = made.map_err(|e| e.to_string())?;
    Ok((x, window, delete))
}

/// Brings a plugin's window that is open already to the front (feedback
/// F18.2: asked for again, it may lie under the editor). A window cannot
/// put itself in front on every desktop, but one that newly appears is
/// put there: so it is taken off the screen and put back, and the desktop
/// is asked the usual way as well.
pub(crate) fn raise(x: &RustConnection, window: u32) {
    let asked = (|| {
        let root = x.setup().roots.first().map(|screen| screen.root).unwrap_or(window);
        x.unmap_window(window)?;
        x.map_window(window)?;
        let active = x.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom;
        // (2: the request comes from the user's own doing, not from a program wanting attention.)
        let event = ClientMessageEvent::new(32, window, active, [2u32, 0, 0, 0, 0]);
        let mask = EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY;
        x.send_event(false, root, mask, event)?;
        x.flush()?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })();
    if let Err(why) = asked {
        eprintln!("shotgun-plugin: the window could not be brought to the front: {why}");
    }
}

struct Host;

impl Class for Host {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let text: Vec<u16> = "Shotgun".encode_utf16().chain([0]).collect();
        // SAFETY: the plugin passes a buffer of 128 characters.
        unsafe { (&mut *name)[..text.len()].copy_from_slice(&text) };
        kResultOk
    }

    unsafe fn createInstance(&self, _cid: *mut TUID, _iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        // SAFETY: `obj` is where the plugin wants the object; none is made.
        unsafe { *obj = std::ptr::null_mut() };
        kNotImplemented
    }
}

/// The plugin tells its host that a control was touched; nothing to do
/// with it yet (no automation), but the plugin expects someone to listen.
struct Handler;

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn performEdit(&self, _id: ParamID, _value: ParamValue) -> tresult {
        kResultOk
    }

    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }

    unsafe fn restartComponent(&self, _flags: int32) -> tresult {
        kResultOk
    }
}

struct Events(RefCell<Vec<Event>>);

impl Class for Events {
    type Interfaces = (IEventList,);
}

impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> int32 {
        self.0.borrow().len() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match self.0.borrow().get(index as usize) {
            Some(event) => {
                // SAFETY: the plugin passes a valid place for one event.
                unsafe { *e = *event };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn addEvent(&self, _e: *mut Event) -> tresult {
        kResultOk
    }
}

/// A stream in memory, for the plugin's state.
struct Memory(RefCell<(Vec<u8>, usize)>);

impl Class for Memory {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for Memory {
    unsafe fn read(&self, buffer: *mut c_void, bytes: int32, read: *mut int32) -> tresult {
        let mut inner = self.0.borrow_mut();
        let (data, at) = &mut *inner;
        let count = (bytes.max(0) as usize).min(data.len() - *at);
        // SAFETY: the plugin's buffer holds `bytes` bytes; no more than that are written.
        unsafe { std::ptr::copy_nonoverlapping(data[*at..].as_ptr(), buffer as *mut u8, count) };
        *at += count;
        if !read.is_null() {
            // SAFETY: checked above.
            unsafe { *read = count as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, bytes: int32, written: *mut int32) -> tresult {
        let mut inner = self.0.borrow_mut();
        let (data, at) = &mut *inner;
        // SAFETY: the plugin's buffer holds `bytes` bytes.
        let incoming = unsafe { std::slice::from_raw_parts(buffer as *const u8, bytes.max(0) as usize) };
        if data.len() < *at + incoming.len() {
            data.resize(*at + incoming.len(), 0);
        }
        data[*at..*at + incoming.len()].copy_from_slice(incoming);
        *at += incoming.len();
        if !written.is_null() {
            // SAFETY: checked above.
            unsafe { *written = bytes };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut inner = self.0.borrow_mut();
        let (data, at) = &mut *inner;
        let to = match mode {
            0 => pos,
            1 => *at as i64 + pos,
            _ => data.len() as i64 + pos,
        };
        *at = to.clamp(0, data.len() as i64) as usize;
        if !result.is_null() {
            // SAFETY: checked above.
            unsafe { *result = *at as i64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        // SAFETY: the plugin passes a valid place for the position.
        unsafe { *pos = self.0.borrow().1 as i64 };
        kResultOk
    }
}

fn stream(bytes: Vec<u8>) -> (ComWrapper<Memory>, Option<ComPtr<IBStream>>) {
    let memory = ComWrapper::new(Memory(RefCell::new((bytes, 0))));
    let pointer = memory.to_com_ptr::<IBStream>();
    (memory, pointer)
}

/// What the plugin asked to be called for: file descriptors and timers.
#[derive(Default)]
struct Wanted {
    handlers: Vec<(ComPtr<IEventHandler>, i32)>,
    timers: Vec<(ComPtr<ITimerHandler>, Duration, Instant)>,
    /// A size the editor asked for, to be given to its window.
    resize: Option<(u16, u16)>,
}

/// The editor's frame, and the run loop a plugin finds through it.
struct Frame(Arc<Mutex<Wanted>>);

impl Class for Frame {
    type Interfaces = (IPlugFrame, IRunLoop);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        // SAFETY: the plugin passes its own view and a valid rectangle.
        unsafe {
            let rect = *size;
            let wanted =
                ((rect.right - rect.left).clamp(1, 8192) as u16, (rect.bottom - rect.top).clamp(1, 8192) as u16);
            self.0.lock().unwrap_or_else(PoisonError::into_inner).resize = Some(wanted);
            if let Some(view) = ComRef::from_raw(view) {
                view.onSize(size);
            }
        }
        kResultOk
    }
}

impl IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(&self, handler: *mut IEventHandler, fd: FileDescriptor) -> tresult {
        // SAFETY: the plugin passes a live handler; a reference is taken.
        if let Some(handler) = unsafe { ComRef::from_raw(handler) } {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).handlers.push((handler.to_com_ptr(), fd));
        }
        kResultOk
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).handlers.retain(|(known, _)| known.as_ptr() != handler);
        kResultOk
    }

    unsafe fn registerTimer(&self, handler: *mut ITimerHandler, milliseconds: TimerInterval) -> tresult {
        // SAFETY: as above.
        if let Some(handler) = unsafe { ComRef::from_raw(handler) } {
            let every = Duration::from_millis(milliseconds.max(1));
            let mut wanted = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            wanted.timers.push((handler.to_com_ptr(), every, Instant::now() + every));
        }
        kResultOk
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).timers.retain(|(known, ..)| known.as_ptr() != handler);
        kResultOk
    }
}

fn text(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The shared object inside a bundle.
fn module(bundle: &Path) -> Option<PathBuf> {
    let dir = bundle.join("Contents/x86_64-linux");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "so"))
}

struct Editor {
    view: ComPtr<IPlugView>,
    x: RustConnection,
    window: u32,
    delete: u32,
    size: (u16, u16),
}

struct Plugin {
    // Dropped in this order: the plugin's objects before the library they live in.
    editor: Option<Editor>,
    controller: Option<ComPtr<IEditController>>,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    _factory: ComPtr<IPluginFactory>,
    library: libloading::Library,
    events: ComWrapper<Events>,
    event_list: ComPtr<IEventList>,
    frame: ComWrapper<Frame>,
    wanted: Arc<Mutex<Wanted>>,
    _host: ComWrapper<Host>,
    _handler: ComWrapper<Handler>,
    name: String,
    inputs: i32,
    outputs: i32,
    instrument: bool,
    has_editor: bool,
    /// Frames processed so far, for the plugin's clock.
    clock: i64,
    /// The tempo the plugin is told, in beats per minute.
    bpm: f64,
    held: Vec<u8>,
    buffers: [Vec<f32>; 4],
}

impl Plugin {
    fn load(bundle: &Path) -> Result<Plugin, String> {
        let file = module(bundle).ok_or_else(|| format!("{} holds no Linux plugin", bundle.display()))?;
        // SAFETY: see the top of the file. Every pointer handed to the
        // plugin outlives the call it is handed to.
        unsafe {
            let library = libloading::Library::new(&file).map_err(|e| e.to_string())?;
            if let Ok(entry) = library.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry") {
                entry(std::ptr::null_mut());
            }
            let get = library
                .get::<unsafe extern "system" fn() -> *mut IPluginFactory>(b"GetPluginFactory")
                .map_err(|e| e.to_string())?;
            let factory = ComPtr::from_raw(get()).ok_or("the plugin has no factory")?;
            let mut chosen = None;
            for index in 0..factory.countClasses() {
                let mut info: PClassInfo = std::mem::zeroed();
                factory.getClassInfo(index, &mut info);
                if chosen.is_none() && text(&info.category) == "Audio Module Class" {
                    chosen = Some(info);
                }
            }
            let info = chosen.ok_or("the plugin has no audio class")?;
            // What it calls itself: an instrument says so in its sub-categories.
            let mut says_instrument = None;
            if let Some(newer) = factory.cast::<IPluginFactory2>() {
                for index in 0..newer.countClasses() {
                    let mut more: PClassInfo2 = std::mem::zeroed();
                    if newer.getClassInfo2(index, &mut more) == kResultOk && more.cid == info.cid {
                        says_instrument = Some(text(&more.subCategories).contains("Instrument"));
                    }
                }
            }
            let mut raw: *mut c_void = std::ptr::null_mut();
            factory.createInstance(info.cid.as_ptr(), IComponent::IID.as_ptr() as *const c_char, &mut raw);
            let component = ComPtr::from_raw(raw as *mut IComponent).ok_or("the plugin made no component")?;
            let host = ComWrapper::new(Host);
            let context = host.to_com_ptr::<IHostApplication>().ok_or("host")?;
            if component.initialize(context.as_ptr() as *mut FUnknown) != kResultOk {
                return Err("the plugin did not initialise".to_owned());
            }
            let processor = component.cast::<IAudioProcessor>().ok_or("the plugin processes no audio")?;
            // The edit controller: the component itself, or a class of its own.
            let controller = component.cast::<IEditController>().or_else(|| {
                let mut class: TUID = [0; 16];
                if component.getControllerClassId(&mut class) != kResultOk {
                    return None;
                }
                let mut raw: *mut c_void = std::ptr::null_mut();
                factory.createInstance(class.as_ptr(), IEditController::IID.as_ptr() as *const c_char, &mut raw);
                let controller = ComPtr::from_raw(raw as *mut IEditController)?;
                controller.initialize(context.as_ptr() as *mut FUnknown);
                // The two halves talk to each other through the host.
                if let (Some(a), Some(b)) =
                    (component.cast::<IConnectionPoint>(), controller.cast::<IConnectionPoint>())
                {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                }
                Some(controller)
            });
            let handler = ComWrapper::new(Handler);
            if let (Some(controller), Some(listener)) = (&controller, handler.to_com_ptr::<IComponentHandler>()) {
                controller.setComponentHandler(listener.as_ptr());
            }
            let buses = |media, direction| component.getBusCount(media as i32, direction as i32);
            let channels = |direction| {
                let mut bus: BusInfo = std::mem::zeroed();
                let known = component.getBusInfo(kAudio as i32, direction as i32, 0, &mut bus) == kResultOk;
                if known { bus.channelCount.clamp(0, 2) } else { 0 }
            };
            let (inputs, outputs) = (channels(kInput), channels(kOutput));
            let takes_notes = buses(kEvent, kInput) > 0;
            let mut setup = ProcessSetup {
                processMode: kRealtime as i32,
                symbolicSampleSize: kSample32 as i32,
                maxSamplesPerBlock: MAX_FRAMES as i32,
                sampleRate: SAMPLE_RATE,
            };
            processor.setupProcessing(&mut setup);
            for (media, direction) in [(kAudio, kInput), (kAudio, kOutput), (kEvent, kInput)] {
                // The first bus of each kind is used; the others (the many
                // separate outputs of a sampler, a sidechain input) are
                // switched off, so that the plugin expects no buffers for them.
                for bus in 0..buses(media, direction) {
                    component.activateBus(media as i32, direction as i32, bus, (bus == 0) as u8);
                }
            }
            component.setActive(1);
            processor.setProcessing(1);
            let has_editor = controller.is_some();
            let events = ComWrapper::new(Events(RefCell::new(Vec::new())));
            let event_list = events.to_com_ptr::<IEventList>().ok_or("events")?;
            let wanted = Arc::new(Mutex::new(Wanted::default()));
            let mut plugin = Plugin {
                editor: None,
                controller,
                processor,
                component,
                _factory: factory,
                library,
                events,
                event_list,
                frame: ComWrapper::new(Frame(wanted.clone())),
                wanted,
                _host: host,
                _handler: handler,
                name: text(&info.name),
                inputs,
                outputs,
                // (Without that word: whatever takes notes and no sound.)
                instrument: says_instrument.unwrap_or(takes_notes && inputs == 0),
                has_editor,
                clock: 0,
                bpm: 120.0,
                held: Vec::new(),
                buffers: std::array::from_fn(|_| vec![0.0; MAX_FRAMES]),
            };
            // The controller shows what the component is set to.
            let state = plugin.component_state();
            plugin.show_controller(&state);
            Ok(plugin)
        }
    }

    fn component_state(&mut self) -> Vec<u8> {
        let (memory, pointer) = stream(Vec::new());
        if let Some(pointer) = pointer {
            // SAFETY: the stream lives until the end of this function.
            unsafe { self.component.getState(pointer.as_ptr()) };
        }
        memory.0.borrow().0.clone()
    }

    fn show_controller(&mut self, component_state: &[u8]) {
        let (_memory, pointer) = stream(component_state.to_vec());
        if let (Some(controller), Some(pointer)) = (&self.controller, pointer) {
            // SAFETY: as above.
            unsafe { controller.setComponentState(pointer.as_ptr()) };
        }
    }

    /// The component's state and the controller's, each with its length before it.
    fn state(&mut self) -> Vec<u8> {
        let component = self.component_state();
        let (memory, pointer) = stream(Vec::new());
        if let (Some(controller), Some(pointer)) = (&self.controller, pointer) {
            // SAFETY: as above.
            unsafe { controller.getState(pointer.as_ptr()) };
        }
        let controller = memory.0.borrow().0.clone();
        let mut packed = Vec::with_capacity(8 + component.len() + controller.len());
        for part in [&component, &controller] {
            packed.extend((part.len() as u32).to_le_bytes());
            packed.extend(part.iter());
        }
        packed
    }

    fn set_state(&mut self, packed: &[u8]) -> bool {
        let mut parts = Vec::new();
        let mut rest = packed;
        while rest.len() >= 4 && parts.len() < 2 {
            let len = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            if rest.len() < 4 + len {
                return false;
            }
            parts.push(rest[4..4 + len].to_vec());
            rest = &rest[4 + len..];
        }
        let [component, controller] = parts.as_slice() else { return false };
        let (_memory, pointer) = stream(component.clone());
        let Some(pointer) = pointer else { return false };
        // SAFETY: the streams live until the end of this function.
        let taken = unsafe { self.component.setState(pointer.as_ptr()) } == kResultOk;
        self.show_controller(component);
        let (_memory, pointer) = stream(controller.clone());
        if let (Some(known), Some(pointer)) = (&self.controller, pointer) {
            // SAFETY: as above.
            unsafe { known.setState(pointer.as_ptr()) };
        }
        taken
    }

    /// One block: `sound` is interleaved stereo, replaced by the plugin's output.
    fn process(&mut self, sound: &mut [f32], notes: &[(u32, bool, u8, u8)]) -> bool {
        let frames = sound.len() / 2;
        {
            let mut events = self.events.0.borrow_mut();
            events.clear();
            for &(offset, on, pitch, velocity) in notes {
                // SAFETY: an all-zero event is a valid one to fill in.
                let mut event: Event = unsafe { std::mem::zeroed() };
                event.sampleOffset = offset.min(frames.saturating_sub(1) as u32) as i32;
                if on {
                    event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
                    event.__field0.noteOn = NoteOnEvent {
                        channel: 0,
                        pitch: pitch as i16,
                        tuning: 0.0,
                        velocity: velocity as f32 / 127.0,
                        length: 0,
                        noteId: -1,
                    };
                    self.held.push(pitch);
                } else {
                    event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
                    event.__field0.noteOff =
                        NoteOffEvent { channel: 0, pitch: pitch as i16, velocity: 0.0, noteId: -1, tuning: 0.0 };
                    self.held.retain(|held| *held != pitch);
                }
                events.push(event);
            }
        }
        let [in_left, in_right, out_left, out_right] = &mut self.buffers;
        for (n, frame) in sound.chunks_exact(2).enumerate() {
            (in_left[n], in_right[n]) = (frame[0], frame[1]);
        }
        if self.inputs == 1 {
            for n in 0..frames {
                in_left[n] = (in_left[n] + in_right[n]) * 0.5;
            }
        }
        out_left[..frames].fill(0.0);
        out_right[..frames].fill(0.0);
        let mut in_channels = [in_left.as_mut_ptr(), in_right.as_mut_ptr()];
        let mut out_channels = [out_left.as_mut_ptr(), out_right.as_mut_ptr()];
        // SAFETY: the buffers hold MAX_FRAMES samples each and `frames`
        // is no more than that; the structures live until `process` returns.
        let result = unsafe {
            let mut inputs: AudioBusBuffers = std::mem::zeroed();
            inputs.numChannels = self.inputs;
            inputs.__field0.channelBuffers32 = in_channels.as_mut_ptr();
            let mut outputs: AudioBusBuffers = std::mem::zeroed();
            outputs.numChannels = self.outputs;
            outputs.__field0.channelBuffers32 = out_channels.as_mut_ptr();
            let mut context: ProcessContext = std::mem::zeroed();
            // What tempo-synced parts of a plugin go by: it plays, at this
            // tempo, in four-four time, this many quarter notes in.
            context.state = ProcessContext_::StatesAndFlags_::kPlaying
                | ProcessContext_::StatesAndFlags_::kTempoValid
                | ProcessContext_::StatesAndFlags_::kTimeSigValid
                | ProcessContext_::StatesAndFlags_::kProjectTimeMusicValid;
            context.sampleRate = SAMPLE_RATE;
            context.projectTimeSamples = self.clock;
            context.projectTimeMusic = self.clock as f64 / SAMPLE_RATE * self.bpm / 60.0;
            context.tempo = self.bpm;
            (context.timeSigNumerator, context.timeSigDenominator) = (4, 4);
            let mut data: ProcessData = std::mem::zeroed();
            data.processMode = kRealtime as i32;
            data.symbolicSampleSize = kSample32 as i32;
            data.numSamples = frames as i32;
            data.numInputs = (self.inputs > 0) as i32;
            data.numOutputs = (self.outputs > 0) as i32;
            data.inputs = if self.inputs > 0 { &mut inputs } else { std::ptr::null_mut() };
            data.outputs = if self.outputs > 0 { &mut outputs } else { std::ptr::null_mut() };
            data.inputEvents = self.event_list.as_ptr();
            data.processContext = &mut context;
            self.processor.process(&mut data)
        };
        self.clock += frames as i64;
        for (n, frame) in sound.chunks_exact_mut(2).enumerate() {
            let right = if self.outputs == 1 { out_left[n] } else { out_right[n] };
            (frame[0], frame[1]) = (out_left[n], right);
        }
        result == kResultOk
    }

    /// Ends every note that is sounding.
    fn reset(&mut self) {
        let held: Vec<(u32, bool, u8, u8)> = self.held.iter().map(|pitch| (0, false, *pitch, 0)).collect();
        if !held.is_empty() {
            let mut silence = vec![0.0f32; 128];
            self.process(&mut silence, &held);
        }
        self.held.clear();
    }

    fn open_editor(&mut self) -> Result<(), String> {
        if let Some(editor) = &self.editor {
            raise(&editor.x, editor.window);
            return Ok(());
        }
        let controller = self.controller.clone().ok_or("the plugin has no window of its own")?;
        // SAFETY: see the top of the file.
        unsafe {
            let view = ComPtr::from_raw(controller.createView(ViewType::kEditor))
                .ok_or("the plugin has no window of its own")?;
            if view.isPlatformTypeSupported(kPlatformTypeX11EmbedWindowID) != kResultOk {
                return Err("the plugin's window cannot be shown on this system".to_owned());
            }
            let mut rect: ViewRect = std::mem::zeroed();
            view.getSize(&mut rect);
            let size =
                ((rect.right - rect.left).clamp(64, 8192) as u16, (rect.bottom - rect.top).clamp(64, 8192) as u16);
            let (x, window, delete) = make_window(&self.name, size)?;
            if let Some(frame) = self.frame.to_com_ptr::<IPlugFrame>() {
                view.setFrame(frame.as_ptr());
            }
            if view.attached(window as usize as *mut c_void, kPlatformTypeX11EmbedWindowID) != kResultOk {
                let _ = x.destroy_window(window);
                let _ = x.flush();
                return Err("the plugin did not open its window".to_owned());
            }
            self.editor = Some(Editor { view, x, window, delete, size });
        }
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(editor) = self.editor.take() {
            // SAFETY: the view is the one that was attached.
            unsafe {
                editor.view.setFrame(std::ptr::null_mut());
                editor.view.removed();
            }
            let _ = editor.x.destroy_window(editor.window);
            let _ = editor.x.flush();
        }
    }

    /// What the plugin and its window are waiting for, done once.
    /// Returns how long the next wait may last.
    fn turn(&mut self) -> Duration {
        let (ready, due): (Vec<(ComPtr<IEventHandler>, i32)>, Vec<ComPtr<ITimerHandler>>) = {
            let mut wanted = self.wanted.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let due = wanted
                .timers
                .iter_mut()
                .filter(|(_, _, next)| *next <= now)
                .map(|(handler, every, next)| {
                    *next = now + *every;
                    handler.clone()
                })
                .collect();
            (wanted.handlers.clone(), due)
        };
        // The file descriptors the plugin watches: which of them can be read now.
        let mut polled: Vec<libc::pollfd> =
            ready.iter().map(|(_, fd)| libc::pollfd { fd: *fd, events: libc::POLLIN, revents: 0 }).collect();
        if !polled.is_empty() {
            // SAFETY: `polled` is a valid array of that length; no waiting (timeout 0).
            unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, 0) };
        }
        for ((handler, fd), state) in ready.iter().zip(&polled) {
            if state.revents & libc::POLLIN != 0 {
                // SAFETY: the handler was registered by the plugin and is kept alive by our reference.
                unsafe { handler.onFDIsSet(*fd) };
            }
        }
        for handler in due {
            // SAFETY: as above.
            unsafe { handler.onTimer() };
        }
        let mut close = false;
        if let Some(editor) = &mut self.editor {
            while let Ok(Some(event)) = editor.x.poll_for_event() {
                if let XEvent::ClientMessage(ClientMessageEvent { data, .. }) = event {
                    close |= data.as_data32()[0] == editor.delete;
                }
            }
            let asked = self.wanted.lock().unwrap_or_else(PoisonError::into_inner).resize.take();
            if let Some(size) = asked.filter(|size| *size != editor.size) {
                let aux = ConfigureWindowAux::new().width(size.0 as u32).height(size.1 as u32);
                let _ = editor.x.configure_window(editor.window, &aux);
                editor.size = size;
            }
            let _ = editor.x.flush();
        }
        if close {
            self.close_editor();
        }
        let wanted = self.wanted.lock().unwrap_or_else(PoisonError::into_inner);
        let next = wanted.timers.iter().map(|(_, _, next)| next.saturating_duration_since(Instant::now())).min();
        // With a window open or descriptors to watch: look again soon.
        let idle = if self.editor.is_some() || !wanted.handlers.is_empty() { 15 } else { 200 };
        next.unwrap_or(Duration::from_millis(idle)).min(Duration::from_millis(idle))
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        self.close_editor();
        // SAFETY: the reverse of `load`, on objects that are still alive.
        unsafe {
            self.processor.setProcessing(0);
            self.component.setActive(0);
            if let Some(controller) = &self.controller
                && controller.cast::<IComponent>().is_none()
            {
                controller.terminate();
            }
            self.component.terminate();
        }
        let _ = &self.library;
    }
}

impl Hosted for Plugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn instrument(&self) -> bool {
        self.instrument
    }

    fn has_editor(&self) -> bool {
        self.has_editor
    }

    fn process(&mut self, sound: &mut [f32], notes: &[(u32, bool, u8, u8)]) -> bool {
        Plugin::process(self, sound, notes)
    }

    fn reset(&mut self) {
        Plugin::reset(self);
    }

    fn set_tempo(&mut self, bpm: f64, position: i64) {
        (self.bpm, self.clock) = (bpm, position);
    }

    fn state(&mut self) -> Vec<u8> {
        Plugin::state(self)
    }

    fn set_state(&mut self, state: &[u8]) -> bool {
        Plugin::set_state(self, state)
    }

    fn open_editor(&mut self) -> Result<(), String> {
        Plugin::open_editor(self)
    }

    fn close_editor(&mut self) {
        Plugin::close_editor(self);
    }

    fn turn(&mut self) -> Duration {
        Plugin::turn(self)
    }
}

/// Loads the plugin at `path`: a `.clap` file, or a VST3 bundle.
fn load(path: &Path) -> Result<Box<dyn Hosted>, String> {
    if path.extension().is_some_and(|e| e == "clap") {
        Ok(Box::new(clap::ClapPlugin::load(path)?))
    } else {
        Ok(Box::new(Plugin::load(path)?))
    }
}

fn read_u32(from: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    from.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_bytes(to: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    to.write_all(&(bytes.len() as u32).to_le_bytes())?;
    to.write_all(bytes)
}

fn main() -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let descriptor = stdin.as_raw_fd();
    let mut from = BufReader::new(stdin.lock());
    // The plugin may print to standard output; the answers must not mix
    // with that. They go to a copy of the descriptor, and descriptor 1 is
    // pointed at standard error.
    // SAFETY: plain descriptor calls on descriptors this process owns.
    let answers = unsafe {
        let copy = libc::dup(1);
        libc::dup2(2, 1);
        <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(copy)
    };
    let mut to = BufWriter::new(answers);
    let mut plugin: Option<Box<dyn Hosted>> = None;
    let mut sound = vec![0.0f32; MAX_FRAMES * 2];
    let mut bytes = vec![0u8; MAX_FRAMES * 8];
    loop {
        // Wait for a request, but not longer than the plugin's timers and window allow.
        let wait = plugin.as_mut().map_or(Duration::from_millis(500), |p| p.turn());
        if from.buffer().is_empty() {
            let mut request = [libc::pollfd { fd: descriptor, events: libc::POLLIN, revents: 0 }];
            // SAFETY: one valid entry.
            let ready = unsafe { libc::poll(request.as_mut_ptr(), 1, wait.as_millis() as i32) };
            if ready == 0 {
                continue;
            }
            // The other end is gone: nobody to work for.
            if from.fill_buf()?.is_empty() {
                return Ok(());
            }
        }
        let mut request = [0u8; 1];
        from.read_exact(&mut request)?;
        match request[0] {
            tag::LOAD => {
                let len = read_u32(&mut from)? as usize;
                let mut path = vec![0u8; len];
                from.read_exact(&mut path)?;
                let path = PathBuf::from(String::from_utf8_lossy(&path).into_owned());
                match load(&path) {
                    Ok(loaded) => {
                        to.write_all(&[1, loaded.instrument() as u8, loaded.has_editor() as u8])?;
                        write_bytes(&mut to, loaded.name().as_bytes())?;
                        plugin = Some(loaded);
                    }
                    Err(why) => {
                        to.write_all(&[0, 0, 0])?;
                        write_bytes(&mut to, why.as_bytes())?;
                    }
                }
            }
            tag::PROCESS => {
                let frames = (read_u32(&mut from)? as usize).min(MAX_FRAMES);
                let count = read_u32(&mut from)? as usize;
                let mut notes = Vec::with_capacity(count);
                for _ in 0..count {
                    let offset = read_u32(&mut from)?;
                    let mut rest = [0u8; 4];
                    from.read_exact(&mut rest)?;
                    notes.push((offset, rest[0] != 0, rest[1], rest[2]));
                }
                from.read_exact(&mut bytes[..frames * 8])?;
                for (sample, raw) in sound.iter_mut().zip(bytes[..frames * 8].chunks_exact(4)) {
                    *sample = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                }
                let done = plugin.as_mut().is_some_and(|p| p.process(&mut sound[..frames * 2], &notes));
                to.write_all(&[done as u8])?;
                if done {
                    for (raw, sample) in bytes.chunks_exact_mut(4).zip(&sound[..frames * 2]) {
                        raw.copy_from_slice(&sample.to_le_bytes());
                    }
                    to.write_all(&bytes[..frames * 8])?;
                }
            }
            tag::RESET => {
                if let Some(plugin) = &mut plugin {
                    plugin.reset();
                }
                to.write_all(&[1])?;
            }
            tag::TEMPO => {
                let mut raw = [0u8; 8];
                from.read_exact(&mut raw)?;
                let bpm = f64::from_le_bytes(raw);
                from.read_exact(&mut raw)?;
                let position = i64::from_le_bytes(raw);
                if let (Some(plugin), true) = (&mut plugin, bpm.is_finite() && bpm > 0.0) {
                    plugin.set_tempo(bpm, position);
                }
                to.write_all(&[1])?;
            }
            tag::GET_STATE => {
                let state = plugin.as_mut().map(|p| p.state()).unwrap_or_default();
                write_bytes(&mut to, &state)?;
            }
            tag::SET_STATE => {
                let len = read_u32(&mut from)? as usize;
                let mut state = vec![0u8; len];
                from.read_exact(&mut state)?;
                let taken = plugin.as_mut().is_some_and(|p| p.set_state(&state));
                to.write_all(&[taken as u8])?;
            }
            tag::EDITOR_OPEN => {
                let opened = match plugin.as_mut().map(|p| p.open_editor()) {
                    Some(Ok(())) => true,
                    Some(Err(why)) => {
                        eprintln!("shotgun-plugin: {why}");
                        false
                    }
                    None => false,
                };
                to.write_all(&[opened as u8])?;
            }
            tag::EDITOR_CLOSE => {
                if let Some(plugin) = &mut plugin {
                    plugin.close_editor();
                }
                to.write_all(&[1])?;
            }
            _ => return Ok(()),
        }
        to.flush()?;
    }
}
