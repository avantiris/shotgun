//! A CLAP plugin in this process: the same requests as for a VST3 one,
//! answered through CLAP's C interface (`clap-sys`).
//!
//! Like the rest of the program this is an FFI boundary throughout. One
//! thread does everything, so CLAP's "main thread" and "audio thread" are
//! the same thread here, which the standard allows.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON, CLAP_EVENT_TRANSPORT,
    CLAP_TRANSPORT_HAS_BEATS_TIMELINE, CLAP_TRANSPORT_HAS_SECONDS_TIMELINE, CLAP_TRANSPORT_HAS_TEMPO,
    CLAP_TRANSPORT_HAS_TIME_SIGNATURE, CLAP_TRANSPORT_IS_PLAYING, clap_event_header, clap_event_note,
    clap_event_transport, clap_input_events, clap_output_events,
};
use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports};
use clap_sys::ext::gui::{
    CLAP_EXT_GUI, CLAP_WINDOW_API_X11, clap_host_gui, clap_plugin_gui, clap_window, clap_window_handle,
};
use clap_sys::ext::posix_fd_support::{
    CLAP_EXT_POSIX_FD_SUPPORT, CLAP_POSIX_FD_READ, clap_host_posix_fd_support, clap_plugin_posix_fd_support,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state, clap_plugin_state};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_host_timer_support, clap_plugin_timer_support};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::process::{CLAP_PROCESS_ERROR, clap_process};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::CLAP_VERSION;
use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::{ClientMessageEvent, ConfigureWindowAux, ConnectionExt};
use x11rb::rust_connection::RustConnection;

use crate::{Hosted, SAMPLE_RATE, make_window};
use sg_plug::MAX_FRAMES;

/// What the plugin has asked its host for. The plugin may ask from
/// threads of its own, hence the lock.
#[derive(Default)]
struct Asked {
    /// (id, period, next time due)
    timers: Vec<(u32, Duration, Instant)>,
    next_timer: u32,
    descriptors: Vec<i32>,
    resize: Option<(u16, u16)>,
}

#[derive(Default)]
struct Shared {
    asked: Mutex<Asked>,
    /// `on_main_thread` is wanted.
    callback: AtomicBool,
    /// The plugin closed its window itself.
    closed: AtomicBool,
}

/// The host as the plugin sees it; `host_data` leads back to [`Shared`].
unsafe fn shared<'a>(host: *const clap_host) -> &'a Shared {
    // SAFETY: every `clap_host` handed out is the one made in `load`,
    // whose `host_data` points at a `Shared` that outlives the plugin.
    unsafe { &*((*host).host_data as *const Shared) }
}

unsafe extern "C" fn register_timer(host: *const clap_host, period_ms: u32, id: *mut u32) -> bool {
    // SAFETY: see `shared`; `id` is the plugin's to be written to.
    unsafe {
        let mut asked = shared(host).asked.lock().unwrap_or_else(PoisonError::into_inner);
        asked.next_timer += 1;
        let (timer, every) = (asked.next_timer, Duration::from_millis(period_ms.max(1) as u64));
        asked.timers.push((timer, every, Instant::now() + every));
        *id = timer;
    }
    true
}

unsafe extern "C" fn unregister_timer(host: *const clap_host, id: u32) -> bool {
    // SAFETY: see `shared`.
    unsafe { shared(host) }.asked.lock().unwrap_or_else(PoisonError::into_inner).timers.retain(|t| t.0 != id);
    true
}

unsafe extern "C" fn register_fd(host: *const clap_host, fd: i32, _flags: u32) -> bool {
    // SAFETY: see `shared`.
    unsafe { shared(host) }.asked.lock().unwrap_or_else(PoisonError::into_inner).descriptors.push(fd);
    true
}

unsafe extern "C" fn modify_fd(_host: *const clap_host, _fd: i32, _flags: u32) -> bool {
    true
}

unsafe extern "C" fn unregister_fd(host: *const clap_host, fd: i32) -> bool {
    // SAFETY: see `shared`.
    unsafe { shared(host) }.asked.lock().unwrap_or_else(PoisonError::into_inner).descriptors.retain(|d| *d != fd);
    true
}

unsafe extern "C" fn nothing(_host: *const clap_host) {}

unsafe extern "C" fn request_callback(host: *const clap_host) {
    // SAFETY: see `shared`.
    unsafe { shared(host) }.callback.store(true, Ordering::Release);
}

unsafe extern "C" fn request_resize(host: *const clap_host, width: u32, height: u32) -> bool {
    let size = (width.clamp(64, 8192) as u16, height.clamp(64, 8192) as u16);
    // SAFETY: see `shared`.
    unsafe { shared(host) }.asked.lock().unwrap_or_else(PoisonError::into_inner).resize = Some(size);
    true
}

unsafe extern "C" fn request_show(_host: *const clap_host) -> bool {
    false
}

unsafe extern "C" fn gui_closed(host: *const clap_host, _destroyed: bool) {
    // SAFETY: see `shared`.
    unsafe { shared(host) }.closed.store(true, Ordering::Release);
}

static TIMERS: clap_host_timer_support =
    clap_host_timer_support { register_timer: Some(register_timer), unregister_timer: Some(unregister_timer) };
static DESCRIPTORS: clap_host_posix_fd_support = clap_host_posix_fd_support {
    register_fd: Some(register_fd),
    modify_fd: Some(modify_fd),
    unregister_fd: Some(unregister_fd),
};
static WINDOWS: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(nothing),
    request_resize: Some(request_resize),
    request_show: Some(request_show),
    request_hide: Some(request_show),
    closed: Some(gui_closed),
};
static STATE: clap_host_state = clap_host_state { mark_dirty: Some(nothing) };

unsafe extern "C" fn host_extension(_host: *const clap_host, id: *const c_char) -> *const c_void {
    // SAFETY: the plugin passes a C string.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_EXT_TIMER_SUPPORT {
        (&raw const TIMERS).cast()
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT {
        (&raw const DESCRIPTORS).cast()
    } else if id == CLAP_EXT_GUI {
        (&raw const WINDOWS).cast()
    } else if id == CLAP_EXT_STATE {
        (&raw const STATE).cast()
    } else {
        std::ptr::null()
    }
}

/// The notes of one block, as the list the plugin reads them from.
unsafe extern "C" fn events_size(list: *const clap_input_events) -> u32 {
    // SAFETY: `ctx` is the vector `process` put there for the call.
    unsafe { (*((*list).ctx as *const Vec<clap_event_note>)).len() as u32 }
}

unsafe extern "C" fn events_get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
    // SAFETY: as above.
    let events = unsafe { &*((*list).ctx as *const Vec<clap_event_note>) };
    events.get(index as usize).map_or(std::ptr::null(), |event| &raw const event.header)
}

/// What the plugin reports back (parameter changes, ended notes) is not used yet.
unsafe extern "C" fn events_push(_list: *const clap_output_events, _event: *const clap_event_header) -> bool {
    true
}

unsafe extern "C" fn stream_write(stream: *const clap_ostream, buffer: *const c_void, size: u64) -> i64 {
    // SAFETY: `ctx` is the vector `state` put there; the plugin's buffer holds `size` bytes.
    unsafe {
        let bytes = &mut *((*stream).ctx as *mut Vec<u8>);
        bytes.extend_from_slice(std::slice::from_raw_parts(buffer.cast::<u8>(), size as usize));
    }
    size as i64
}

unsafe extern "C" fn stream_read(stream: *const clap_istream, buffer: *mut c_void, size: u64) -> i64 {
    // SAFETY: `ctx` is the pair `set_state` put there; the plugin's buffer has room for `size` bytes.
    unsafe {
        let (bytes, at) = &mut *((*stream).ctx as *mut (&[u8], usize));
        let count = (bytes.len() - *at).min(size as usize);
        std::ptr::copy_nonoverlapping(bytes[*at..].as_ptr(), buffer.cast::<u8>(), count);
        *at += count;
        count as i64
    }
}

struct Window {
    x: RustConnection,
    window: u32,
    delete: u32,
    size: (u16, u16),
}

/// One side's audio ports: for each the samples of its channels and the
/// pointers to them that the plugin is given.
struct Ports {
    channels: Vec<Vec<Vec<f32>>>,
    pointers: Vec<Vec<*mut f32>>,
    buffers: Vec<clap_audio_buffer>,
}

impl Ports {
    fn new(counts: &[u32]) -> Ports {
        let mut channels: Vec<Vec<Vec<f32>>> =
            counts.iter().map(|n| (0..*n).map(|_| vec![0.0f32; MAX_FRAMES]).collect()).collect();
        let mut pointers: Vec<Vec<*mut f32>> =
            channels.iter_mut().map(|port| port.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
        let buffers = pointers
            .iter_mut()
            .zip(counts)
            .map(|(port, count)| clap_audio_buffer {
                data32: port.as_mut_ptr(),
                data64: std::ptr::null_mut(),
                channel_count: *count,
                latency: 0,
                constant_mask: 0,
            })
            .collect();
        Ports { channels, pointers, buffers }
    }
}

pub struct ClapPlugin {
    plugin: *const clap_plugin,
    entry: *const clap_plugin_entry,
    // Kept for as long as the plugin: it holds pointers to both.
    _host: Box<clap_host>,
    shared: Box<Shared>,
    _library: libloading::Library,
    name: String,
    instrument: bool,
    gui: *const clap_plugin_gui,
    window: Option<Window>,
    inputs: Ports,
    outputs: Ports,
    clock: i64,
    /// The tempo the plugin is told, in beats per minute.
    bpm: f64,
    held: Vec<u8>,
    events: Vec<clap_event_note>,
}

fn note(offset: u32, on: bool, pitch: u8, velocity: u8) -> clap_event_note {
    clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time: offset,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF },
            flags: 0,
        },
        note_id: -1,
        port_index: 0,
        channel: 0,
        key: pitch as i16,
        velocity: velocity as f64 / 127.0,
    }
}

impl ClapPlugin {
    /// One of the plugin's extensions, or null.
    unsafe fn extension<T>(&self, id: &CStr) -> *const T {
        // SAFETY: the plugin is alive for as long as `self`.
        unsafe { (*self.plugin).get_extension.map_or(std::ptr::null(), |get| get(self.plugin, id.as_ptr()).cast()) }
    }

    pub fn load(file: &Path) -> Result<ClapPlugin, String> {
        let path = CString::new(file.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
        // SAFETY: see the top of the file. The library outlives everything taken from it.
        unsafe {
            let library = libloading::Library::new(file).map_err(|e| e.to_string())?;
            let entry: *const clap_plugin_entry =
                *library.get::<*const clap_plugin_entry>(b"clap_entry").map_err(|e| e.to_string())?;
            let (Some(init), Some(get_factory)) = ((*entry).init, (*entry).get_factory) else {
                return Err("not a CLAP plugin".to_owned());
            };
            if !init(path.as_ptr()) {
                return Err("the plugin did not start".to_owned());
            }
            let factory: *const clap_plugin_factory = get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()).cast();
            if factory.is_null() {
                return Err("the plugin file offers no plugins".to_owned());
            }
            let (Some(count), Some(describe), Some(create)) =
                ((*factory).get_plugin_count, (*factory).get_plugin_descriptor, (*factory).create_plugin)
            else {
                return Err("the plugin file offers no plugins".to_owned());
            };
            // A file may hold several plugins (Surge XT: the synthesiser
            // and its effects): the first is the one the file is named after.
            if count(factory) == 0 {
                return Err("the plugin file offers no plugins".to_owned());
            }
            let descriptor = describe(factory, 0);
            if descriptor.is_null() {
                return Err("the plugin does not describe itself".to_owned());
            }
            let name = CStr::from_ptr((*descriptor).name).to_string_lossy().into_owned();
            let mut instrument = false;
            let mut feature = (*descriptor).features;
            while !feature.is_null() && !(*feature).is_null() {
                instrument |= CStr::from_ptr(*feature) == clap_sys::plugin_features::CLAP_PLUGIN_FEATURE_INSTRUMENT;
                feature = feature.add(1);
            }
            let shared = Box::new(Shared::default());
            let host = Box::new(clap_host {
                clap_version: CLAP_VERSION,
                host_data: (&raw const *shared) as *mut c_void,
                name: c"Shotgun".as_ptr(),
                vendor: c"Shotgun".as_ptr(),
                url: c"".as_ptr(),
                version: c"0.1".as_ptr(),
                get_extension: Some(host_extension),
                request_restart: Some(nothing),
                request_process: Some(nothing),
                request_callback: Some(request_callback),
            });
            let plugin = create(factory, &raw const *host, (*descriptor).id);
            if plugin.is_null() {
                return Err("the plugin could not be made".to_owned());
            }
            if !(*plugin).init.is_some_and(|init| init(plugin)) {
                if let Some(destroy) = (*plugin).destroy {
                    destroy(plugin);
                }
                return Err("the plugin did not start".to_owned());
            }
            let mut loaded = ClapPlugin {
                plugin,
                entry,
                _host: host,
                shared,
                _library: library,
                name,
                instrument,
                gui: std::ptr::null(),
                window: None,
                inputs: Ports::new(&[]),
                outputs: Ports::new(&[]),
                clock: 0,
                bpm: 120.0,
                held: Vec::new(),
                events: Vec::new(),
            };
            // How many ports with how many channels each; without the
            // extension: stereo out, and stereo in for an effect.
            let ports: *const clap_plugin_audio_ports = loaded.extension(CLAP_EXT_AUDIO_PORTS);
            let counts = |input: bool| -> Vec<u32> {
                let (Some(count), Some(get)) =
                    (ports.as_ref().and_then(|p| p.count), ports.as_ref().and_then(|p| p.get))
                else {
                    return if input && instrument { Vec::new() } else { vec![2] };
                };
                (0..count(plugin, input))
                    .map(|index| {
                        let mut info: clap_audio_port_info = std::mem::zeroed();
                        get(plugin, index, input, &mut info);
                        info.channel_count.min(8)
                    })
                    .collect()
            };
            loaded.inputs = Ports::new(&counts(true));
            loaded.outputs = Ports::new(&counts(false));
            let gui: *const clap_plugin_gui = loaded.extension(CLAP_EXT_GUI);
            let shown = gui
                .as_ref()
                .and_then(|g| g.is_api_supported)
                .is_some_and(|supported| supported(plugin, CLAP_WINDOW_API_X11.as_ptr(), false));
            loaded.gui = if shown { gui } else { std::ptr::null() };
            let active = (*plugin).activate.is_some_and(|activate| activate(plugin, SAMPLE_RATE, 1, MAX_FRAMES as u32));
            if !active || !(*plugin).start_processing.is_some_and(|start| start(plugin)) {
                return Err("the plugin did not become active".to_owned());
            }
            Ok(loaded)
        }
    }
}

impl Hosted for ClapPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn instrument(&self) -> bool {
        self.instrument
    }

    fn has_editor(&self) -> bool {
        !self.gui.is_null()
    }

    fn process(&mut self, sound: &mut [f32], notes: &[(u32, bool, u8, u8)]) -> bool {
        let frames = (sound.len() / 2).min(MAX_FRAMES);
        self.events.clear();
        for (offset, on, pitch, velocity) in notes {
            self.events.push(note((*offset).min(frames.saturating_sub(1) as u32), *on, *pitch, *velocity));
            self.held.retain(|held| held != pitch);
            if *on {
                self.held.push(*pitch);
            }
        }
        // (The list must be in order of time; note ends come before starts at the same instant.)
        self.events.sort_by_key(|e| (e.header.time, e.header.type_ == CLAP_EVENT_NOTE_ON));
        // The track's sound goes into the first input port: left and
        // right, or their middle if the port has one channel.
        for port in &mut self.inputs.channels {
            for channel in port.iter_mut() {
                channel[..frames].fill(0.0);
            }
        }
        if let Some(port) = self.inputs.channels.first_mut() {
            let mono = port.len() == 1;
            for (side, channel) in port.iter_mut().take(2).enumerate() {
                for (to, frame) in channel.iter_mut().zip(sound.as_chunks::<2>().0) {
                    *to = if mono { (frame[0] + frame[1]) * 0.5 } else { frame[side] };
                }
            }
        }
        let input =
            clap_input_events { ctx: (&raw mut self.events).cast(), size: Some(events_size), get: Some(events_get) };
        let output = clap_output_events { ctx: std::ptr::null_mut(), try_push: Some(events_push) };
        // What tempo-synced parts of a plugin go by: it plays, at this
        // tempo, in four-four time, this far in.
        let seconds = self.clock as f64 / SAMPLE_RATE;
        let beats = seconds * self.bpm / 60.0;
        let transport = clap_event_transport {
            header: clap_event_header {
                size: std::mem::size_of::<clap_event_transport>() as u32,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_: CLAP_EVENT_TRANSPORT,
                flags: 0,
            },
            flags: CLAP_TRANSPORT_HAS_TEMPO
                | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                | CLAP_TRANSPORT_IS_PLAYING,
            song_pos_beats: (beats * CLAP_BEATTIME_FACTOR as f64) as i64,
            song_pos_seconds: (seconds * CLAP_SECTIME_FACTOR as f64) as i64,
            tempo: self.bpm,
            tempo_inc: 0.0,
            loop_start_beats: 0,
            loop_end_beats: 0,
            loop_start_seconds: 0,
            loop_end_seconds: 0,
            bar_start: ((beats / 4.0).floor() * 4.0 * CLAP_BEATTIME_FACTOR as f64) as i64,
            bar_number: (beats / 4.0).floor() as i32,
            tsig_num: 4,
            tsig_denom: 4,
        };
        let call = clap_process {
            steady_time: self.clock,
            frames_count: frames as u32,
            transport: &raw const transport,
            audio_inputs: self.inputs.buffers.as_ptr(),
            audio_outputs: self.outputs.buffers.as_mut_ptr(),
            audio_inputs_count: self.inputs.buffers.len() as u32,
            audio_outputs_count: self.outputs.buffers.len() as u32,
            in_events: &raw const input,
            out_events: &raw const output,
        };
        // SAFETY: every pointer in `call` leads to memory owned by `self`
        // or by this frame, with room for `frames` samples per channel.
        let status =
            unsafe { (*self.plugin).process.map_or(CLAP_PROCESS_ERROR, |process| process(self.plugin, &call)) };
        self.clock += frames as i64;
        if status == CLAP_PROCESS_ERROR {
            return false;
        }
        let Some(port) = self.outputs.channels.first() else { return false };
        let (left, right) = match port.as_slice() {
            [] => return false,
            [only] => (only, only),
            [left, right, ..] => (left, right),
        };
        for (n, frame) in sound.as_chunks_mut::<2>().0.iter_mut().take(frames).enumerate() {
            *frame = [left[n], right[n]];
        }
        true
    }

    fn set_tempo(&mut self, bpm: f64, position: i64) {
        (self.bpm, self.clock) = (bpm, position);
    }

    fn reset(&mut self) {
        // What is still held ends with the next block; what rings is cut.
        let held: Vec<(u32, bool, u8, u8)> = self.held.drain(..).map(|pitch| (0, false, pitch, 0)).collect();
        if !held.is_empty() {
            let mut quiet = [0.0f32; 128];
            self.process(&mut quiet, &held);
        }
        // SAFETY: the plugin is active and this is its only thread.
        unsafe {
            if let Some(reset) = (*self.plugin).reset {
                reset(self.plugin);
            }
        }
    }

    fn state(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        // SAFETY: the stream leads to `bytes`, alive during the call.
        unsafe {
            let state: *const clap_plugin_state = self.extension(CLAP_EXT_STATE);
            let Some(save) = state.as_ref().and_then(|s| s.save) else { return Vec::new() };
            let stream = clap_ostream { ctx: (&raw mut bytes).cast(), write: Some(stream_write) };
            if !save(self.plugin, &stream) {
                return Vec::new();
            }
        }
        bytes
    }

    fn set_state(&mut self, bytes: &[u8]) -> bool {
        let mut from: (&[u8], usize) = (bytes, 0);
        // SAFETY: the stream leads to `from`, alive during the call.
        unsafe {
            let state: *const clap_plugin_state = self.extension(CLAP_EXT_STATE);
            let Some(load) = state.as_ref().and_then(|s| s.load) else { return false };
            let stream = clap_istream { ctx: (&raw mut from).cast(), read: Some(stream_read) };
            load(self.plugin, &stream)
        }
    }

    fn open_editor(&mut self) -> Result<(), String> {
        if let Some(window) = &self.window {
            crate::raise(&window.x, window.window);
            return Ok(());
        }
        // SAFETY: `gui` is the plugin's own table, checked in `load`.
        unsafe {
            let gui = self.gui.as_ref().ok_or("the plugin has no window of its own")?;
            let (Some(create), Some(set_parent)) = (gui.create, gui.set_parent) else {
                return Err("the plugin has no window of its own".to_owned());
            };
            if !create(self.plugin, CLAP_WINDOW_API_X11.as_ptr(), false) {
                return Err("the plugin did not make its window".to_owned());
            }
            let (mut width, mut height) = (800u32, 600u32);
            if let Some(get_size) = gui.get_size {
                get_size(self.plugin, &mut width, &mut height);
            }
            let size = (width.clamp(64, 8192) as u16, height.clamp(64, 8192) as u16);
            let (x, window, delete) = match make_window(&self.name, size) {
                Ok(made) => made,
                Err(why) => {
                    if let Some(destroy) = gui.destroy {
                        destroy(self.plugin);
                    }
                    return Err(why);
                }
            };
            let parent =
                clap_window { api: CLAP_WINDOW_API_X11.as_ptr(), specific: clap_window_handle { x11: window as _ } };
            if !set_parent(self.plugin, &parent) {
                if let Some(destroy) = gui.destroy {
                    destroy(self.plugin);
                }
                let _ = x.destroy_window(window);
                let _ = x.flush();
                return Err("the plugin did not open its window".to_owned());
            }
            if let Some(show) = gui.show {
                show(self.plugin);
            }
            self.shared.closed.store(false, Ordering::Release);
            self.window = Some(Window { x, window, delete, size });
        }
        Ok(())
    }

    fn close_editor(&mut self) {
        if let Some(window) = self.window.take() {
            // SAFETY: the window was made by `open_editor` with this table.
            unsafe {
                if let Some(destroy) = self.gui.as_ref().and_then(|g| g.destroy) {
                    destroy(self.plugin);
                }
            }
            let _ = window.x.destroy_window(window.window);
            let _ = window.x.flush();
        }
    }

    fn turn(&mut self) -> Duration {
        let (descriptors, due): (Vec<i32>, Vec<u32>) = {
            let mut asked = self.shared.asked.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let due = asked
                .timers
                .iter_mut()
                .filter(|(_, _, next)| *next <= now)
                .map(|(id, every, next)| {
                    *next = now + *every;
                    *id
                })
                .collect();
            (asked.descriptors.clone(), due)
        };
        let mut polled: Vec<libc::pollfd> =
            descriptors.iter().map(|fd| libc::pollfd { fd: *fd, events: libc::POLLIN, revents: 0 }).collect();
        // SAFETY: `polled` is a valid array of that length (no waiting);
        // the extension tables are the plugin's own.
        unsafe {
            if !polled.is_empty() {
                libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, 0);
            }
            let watched: *const clap_plugin_posix_fd_support = self.extension(CLAP_EXT_POSIX_FD_SUPPORT);
            if let Some(on_fd) = watched.as_ref().and_then(|w| w.on_fd) {
                for state in polled.iter().filter(|state| state.revents & libc::POLLIN != 0) {
                    on_fd(self.plugin, state.fd, CLAP_POSIX_FD_READ);
                }
            }
            let timers: *const clap_plugin_timer_support = self.extension(CLAP_EXT_TIMER_SUPPORT);
            if let Some(on_timer) = timers.as_ref().and_then(|t| t.on_timer) {
                for id in due {
                    on_timer(self.plugin, id);
                }
            }
            if self.shared.callback.swap(false, Ordering::AcqRel)
                && let Some(on_main_thread) = (*self.plugin).on_main_thread
            {
                on_main_thread(self.plugin);
            }
        }
        let mut close = self.shared.closed.swap(false, Ordering::AcqRel);
        if let Some(window) = &mut self.window {
            while let Ok(Some(event)) = window.x.poll_for_event() {
                if let XEvent::ClientMessage(ClientMessageEvent { data, .. }) = event {
                    close |= data.as_data32()[0] == window.delete;
                }
            }
            let asked = self.shared.asked.lock().unwrap_or_else(PoisonError::into_inner).resize.take();
            if let Some(size) = asked.filter(|size| *size != window.size) {
                let aux = ConfigureWindowAux::new().width(size.0 as u32).height(size.1 as u32);
                let _ = window.x.configure_window(window.window, &aux);
                window.size = size;
            }
            let _ = window.x.flush();
        }
        if close {
            self.close_editor();
        }
        let asked = self.shared.asked.lock().unwrap_or_else(PoisonError::into_inner);
        let next = asked.timers.iter().map(|(_, _, next)| next.saturating_duration_since(Instant::now())).min();
        let idle = if self.window.is_some() || !asked.descriptors.is_empty() { 15 } else { 200 };
        next.unwrap_or(Duration::from_millis(idle)).min(Duration::from_millis(idle))
    }
}

impl Drop for ClapPlugin {
    fn drop(&mut self) {
        self.close_editor();
        // SAFETY: the reverse of `load`, on a plugin that is still alive;
        // the library is dropped after this, as the last field.
        unsafe {
            let plugin = &*self.plugin;
            if let Some(stop) = plugin.stop_processing {
                stop(self.plugin);
            }
            if let Some(deactivate) = plugin.deactivate {
                deactivate(self.plugin);
            }
            if let Some(destroy) = plugin.destroy {
                destroy(self.plugin);
            }
            if let Some(deinit) = (*self.entry).deinit {
                deinit();
            }
        }
        // (Kept until here: the plugin wrote into them.)
        let _ = (&self.inputs.pointers, &self.outputs.pointers);
    }
}
