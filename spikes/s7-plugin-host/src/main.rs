//! Spike S7: can a VST3 plugin be hosted from Rust with the `vst3`
//! bindings alone? Loads a bundle, plays one note through it block by
//! block (no realtime), prints the level that comes out, and reads its state.
//!
//! s7-plugin-host <bundle.vst3> [class index]

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};

use vst3::Steinberg::Vst::BusDirections_::*;
use vst3::Steinberg::Vst::MediaTypes_::*;
use vst3::Steinberg::Vst::ProcessModes_::*;
use vst3::Steinberg::Vst::SymbolicSampleSizes_::*;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComWrapper, Interface};

struct Host;

impl Class for Host {
    type Interfaces = (IHostApplication,);
}

impl IHostApplicationTrait for Host {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        let text: Vec<u16> = "Shotgun".encode_utf16().chain([0]).collect();
        unsafe { (&mut *name)[..text.len()].copy_from_slice(&text) };
        kResultOk
    }

    unsafe fn createInstance(&self, _cid: *mut TUID, _iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        unsafe { *obj = std::ptr::null_mut() };
        kNotImplemented
    }
}

/// The events of one block.
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
                unsafe { *e = *event };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        self.0.borrow_mut().push(unsafe { *e });
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
        unsafe { std::ptr::copy_nonoverlapping(data[*at..].as_ptr(), buffer as *mut u8, count) };
        *at += count;
        if !read.is_null() {
            unsafe { *read = count as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, bytes: int32, written: *mut int32) -> tresult {
        let mut inner = self.0.borrow_mut();
        let (data, at) = &mut *inner;
        let incoming = unsafe { std::slice::from_raw_parts(buffer as *const u8, bytes.max(0) as usize) };
        if data.len() < *at + incoming.len() {
            data.resize(*at + incoming.len(), 0);
        }
        data[*at..*at + incoming.len()].copy_from_slice(incoming);
        *at += incoming.len();
        if !written.is_null() {
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
            unsafe { *result = *at as i64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        unsafe { *pos = self.0.borrow().1 as i64 };
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bundle = PathBuf::from(std::env::args().nth(1).ok_or("usage: s7-plugin-host <bundle.vst3> [class]")?);
    let wanted: Option<usize> = std::env::args().nth(2).and_then(|n| n.parse().ok());
    let file = module(&bundle).ok_or("no module in the bundle")?;
    let started = std::time::Instant::now();
    // SAFETY: this whole program is one FFI boundary: a plugin is foreign
    // code loaded into the process and driven through COM interfaces.
    unsafe {
        let library = libloading::Library::new(&file)?;
        if let Ok(entry) = library.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry") {
            entry(std::ptr::null_mut());
        }
        let get = library.get::<unsafe extern "system" fn() -> *mut IPluginFactory>(b"GetPluginFactory")?;
        let factory = ComPtr::from_raw(get()).ok_or("no factory")?;
        let mut chosen = None;
        for index in 0..factory.countClasses() {
            let mut info: PClassInfo = std::mem::zeroed();
            factory.getClassInfo(index, &mut info);
            println!("class {index}: {} [{}]", text(&info.name), text(&info.category));
            let audio = text(&info.category) == "Audio Module Class";
            if audio && (wanted == Some(index as usize) || (wanted.is_none() && chosen.is_none())) {
                chosen = Some(info);
            }
        }
        let info = chosen.ok_or("no audio class")?;
        let mut raw: *mut c_void = std::ptr::null_mut();
        let iid = IComponent::IID;
        factory.createInstance(info.cid.as_ptr(), iid.as_ptr() as *const c_char, &mut raw);
        let component = ComPtr::from_raw(raw as *mut IComponent).ok_or("no component")?;
        let host = ComWrapper::new(Host);
        let context = host.to_com_ptr::<IHostApplication>().ok_or("host")?;
        let initialised = component.initialize(context.as_ptr() as *mut FUnknown);
        println!("loaded {} in {:.0} ms, initialize -> {initialised}", text(&info.name), started.elapsed().as_secs_f64() * 1e3);
        let processor = component.cast::<IAudioProcessor>().ok_or("not an audio processor")?;
        let buses = |media, direction| component.getBusCount(media, direction);
        println!(
            "buses: audio in {}, audio out {}, events in {}",
            buses(kAudio as i32, kInput as i32),
            buses(kAudio as i32, kOutput as i32),
            buses(kEvent as i32, kInput as i32)
        );
        const BLOCK: usize = 512;
        let mut setup = ProcessSetup {
            processMode: kOffline as i32,
            symbolicSampleSize: kSample32 as i32,
            maxSamplesPerBlock: BLOCK as i32,
            sampleRate: 48_000.0,
        };
        println!("setupProcessing -> {}", processor.setupProcessing(&mut setup));
        for (media, direction) in [(kAudio, kInput), (kAudio, kOutput), (kEvent, kInput)] {
            if buses(media as i32, direction as i32) > 0 {
                component.activateBus(media as i32, direction as i32, 0, 1);
            }
        }
        println!("setActive -> {}, setProcessing -> {}", component.setActive(1), processor.setProcessing(1));

        let events = ComWrapper::new(Events(RefCell::new(Vec::new())));
        let event_list = events.to_com_ptr::<IEventList>().ok_or("events")?;
        let (mut left, mut right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let (mut in_left, mut in_right) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        let has_input = buses(kAudio as i32, kInput as i32) > 0;
        let mut energy = Vec::new();
        let render = std::time::Instant::now();
        // Two seconds: a note of one second, then its tail.
        for block in 0..(96_000 / BLOCK) {
            events.0.borrow_mut().clear();
            let mut event: Event = std::mem::zeroed();
            if block == 0 {
                event.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
                event.__field0.noteOn = NoteOnEvent { channel: 0, pitch: 60, tuning: 0.0, velocity: 0.8, length: 0, noteId: 1 };
                events.0.borrow_mut().push(event);
            } else if block == 48_000 / BLOCK {
                event.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
                event.__field0.noteOff = NoteOffEvent { channel: 0, pitch: 60, velocity: 0.0, noteId: 1, tuning: 0.0 };
                events.0.borrow_mut().push(event);
            }
            // (An effect gets a tone to work on.)
            for (n, (l, r)) in in_left.iter_mut().zip(in_right.iter_mut()).enumerate() {
                let v = 0.2 * ((block * BLOCK + n) as f32 * 0.03).sin();
                (*l, *r) = (v, v);
            }
            let mut out_channels = [left.as_mut_ptr(), right.as_mut_ptr()];
            let mut in_channels = [in_left.as_mut_ptr(), in_right.as_mut_ptr()];
            let mut outputs: AudioBusBuffers = std::mem::zeroed();
            outputs.numChannels = 2;
            outputs.__field0.channelBuffers32 = out_channels.as_mut_ptr();
            let mut inputs: AudioBusBuffers = std::mem::zeroed();
            inputs.numChannels = 2;
            inputs.__field0.channelBuffers32 = in_channels.as_mut_ptr();
            let mut context: ProcessContext = std::mem::zeroed();
            context.sampleRate = 48_000.0;
            context.projectTimeSamples = (block * BLOCK) as i64;
            context.tempo = 120.0;
            let mut data: ProcessData = std::mem::zeroed();
            data.processMode = kOffline as i32;
            data.symbolicSampleSize = kSample32 as i32;
            data.numSamples = BLOCK as i32;
            data.numInputs = has_input as i32;
            data.numOutputs = 1;
            data.inputs = if has_input { &mut inputs } else { std::ptr::null_mut() };
            data.outputs = &mut outputs;
            data.inputEvents = event_list.as_ptr();
            data.processContext = &mut context;
            let result = processor.process(&mut data);
            if result != kResultOk && block == 0 {
                println!("process -> {result}");
            }
            energy.push(left.iter().chain(&right).map(|s| s * s).sum::<f32>() / (2 * BLOCK) as f32);
        }
        let db = |range: std::ops::Range<usize>| {
            let mean = energy[range.clone()].iter().sum::<f32>() / range.len() as f32;
            10.0 * mean.max(1e-12).log10()
        };
        let blocks = energy.len();
        println!(
            "rendered 2 s in {:.0} ms: note {:.1} dB, tail {:.1} dB, end {:.1} dB",
            render.elapsed().as_secs_f64() * 1e3,
            db(4..blocks / 2),
            db(blocks / 2..blocks * 3 / 4),
            db(blocks - 8..blocks)
        );
        let memory = ComWrapper::new(Memory(RefCell::new((Vec::new(), 0))));
        let stream = memory.to_com_ptr::<IBStream>().ok_or("stream")?;
        let got = component.getState(stream.as_ptr());
        println!("getState -> {got}, {} bytes", memory.0.borrow().0.len());
        memory.0.borrow_mut().1 = 0;
        println!("setState -> {}", component.setState(stream.as_ptr()));
        // The edit controller: the component itself, or a class of its own.
        let controller = component.cast::<IEditController>();
        let mut separate: TUID = [0; 16];
        let has_class = component.getControllerClassId(&mut separate) == kResultOk;
        println!("controller: single component {}, own class {has_class}", controller.is_some());
        processor.setProcessing(0);
        component.setActive(0);
        component.terminate();
        drop((processor, component, factory));
        if let Ok(exit) = library.get::<unsafe extern "C" fn() -> bool>(b"ModuleExit") {
            exit();
        }
        println!("unloaded: {}", CStr::from_bytes_with_nul(b"ok\0")?.to_string_lossy());
    }
    Ok(())
}
