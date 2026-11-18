//! S3 backend (a'): native PipeWire *filter* node (pw_filter) with planar DSP ports.
//! pipewire-rs 0.10 has no wrapper for pw_filter, so this goes through `pipewire::sys`
//! (raw bindgen FFI). Main loop / init still use the safe crate API.
//!
//! usage: pw_filter_engine <quantum> <secs> [--duplex]
//!   --duplex  add two input ports (linked to the default source by the runner via pw-link)

use pipewire as pw;
use pw::spa;
use pw::sys;
use s3_audio::*;
use std::ffi::{c_void, CString};
use std::sync::atomic::Ordering::Relaxed;

struct Ud {
    rt: Rt,
    filter: *mut sys::pw_filter,
    out: [*mut c_void; 2],
    inp: [*mut c_void; 2],
}

unsafe extern "C" fn on_process(data: *mut c_void, position: *mut spa::sys::spa_io_position) {
    let ud = &mut *(data as *mut Ud);
    let start = ud.rt.begin();
    let mut snap = ClockSnap::default();
    let c = &(*position).clock;
    let n = c.duration as u32;
    ud.rt.clock(c.flags, c.position, c.duration, c.xrun);
    snap.clk_nsec = c.nsec;
    snap.clk_position = c.position;
    snap.clk_duration = c.duration;
    snap.clk_delay = c.delay;
    snap.clk_next_nsec = c.next_nsec;
    snap.clk_rate_diff = c.rate_diff;
    let mut t: sys::pw_time = std::mem::zeroed();
    if sys::pw_filter_get_time(ud.filter, &mut t) >= 0 {
        snap.t_now = t.now;
        snap.t_ticks = t.ticks;
        snap.t_delay = t.delay;
        snap.t_queued = t.queued;
        snap.t_buffered = t.buffered;
        snap.t_rate_den = t.rate.denom;
    }
    // inputs: just measure the peak to prove data flows in the same cycle
    for p in ud.inp {
        if p.is_null() {
            continue;
        }
        let b = sys::pw_filter_get_dsp_buffer(p, n) as *const f32;
        if !b.is_null() {
            let s = std::slice::from_raw_parts(b, n as usize);
            let mut peak = 0f32;
            for v in s {
                peak = peak.max(v.abs());
            }
            let pm = (peak * 1000.0) as u32 + 1; // +1: mark "buffer was present"
            if pm > ud.rt.shared.peak_milli.load(Relaxed) {
                ud.rt.shared.peak_milli.store(pm, Relaxed);
            }
        }
    }
    let l = sys::pw_filter_get_dsp_buffer(ud.out[0], n) as *mut f32;
    let r = sys::pw_filter_get_dsp_buffer(ud.out[1], n) as *mut f32;
    if !l.is_null() && !r.is_null() {
        ud.rt.render_planar(
            std::slice::from_raw_parts_mut(l, n as usize),
            std::slice::from_raw_parts_mut(r, n as usize),
        );
    } else {
        ud.rt.shared.no_buffer.fetch_add(1, Relaxed);
    }
    ud.rt.end(start, n, snap);
}

unsafe extern "C" fn on_state(
    _data: *mut c_void,
    old: sys::pw_filter_state,
    new: sys::pw_filter_state,
    _err: *const std::os::raw::c_char,
) {
    eprintln!("filter state {old} -> {new}");
}

unsafe fn add_port(filter: *mut sys::pw_filter, dir: u32, name: &str, ch: &str) -> *mut c_void {
    let props = pw::properties::properties! {
        *pw::keys::FORMAT_DSP => "32 bit float mono audio",
        *pw::keys::PORT_NAME => name,
        *pw::keys::AUDIO_CHANNEL => ch,
    };
    sys::pw_filter_add_port(
        filter,
        dir,
        sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
        0,
        props.into_raw(),
        std::ptr::null_mut(),
        0,
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let quantum: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(128);
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let duplex = args.iter().any(|a| a == "--duplex");

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let (rt, mut main_side) = make();
    let latency = format!("{quantum}/{RATE}");
    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => if duplex { "Duplex" } else { "Playback" },
        *pw::keys::MEDIA_ROLE => "Production",
        *pw::keys::MEDIA_CLASS => "Stream/Output/Audio",
        *pw::keys::NODE_NAME => "s3-pw-filter",
        *pw::keys::APP_NAME => "shotgun-s3",
        *pw::keys::NODE_LATENCY => latency.as_str(),
        *pw::keys::NODE_RATE => "1/48000",
        *pw::keys::NODE_AUTOCONNECT => "true",
    };

    let mut events: Box<sys::pw_filter_events> = Box::new(unsafe { std::mem::zeroed() });
    events.version = sys::PW_VERSION_FILTER_EVENTS;
    events.process = Some(on_process);
    events.state_changed = Some(on_state);
    let ud = Box::into_raw(Box::new(Ud {
        rt,
        filter: std::ptr::null_mut(),
        out: [std::ptr::null_mut(); 2],
        inp: [std::ptr::null_mut(); 2],
    }));
    let name = CString::new("s3-pw-filter").unwrap();
    let filter = unsafe {
        let f = sys::pw_filter_new_simple(
            mainloop.loop_().as_raw_ptr(),
            name.as_ptr(),
            props.into_raw(),
            &*events,
            ud.cast(),
        );
        assert!(!f.is_null(), "pw_filter_new_simple failed");
        (*ud).filter = f;
        let out_dir = spa::sys::SPA_DIRECTION_OUTPUT;
        let in_dir = spa::sys::SPA_DIRECTION_INPUT;
        (*ud).out = [add_port(f, out_dir, "out_FL", "FL"), add_port(f, out_dir, "out_FR", "FR")];
        if duplex {
            (*ud).inp = [add_port(f, in_dir, "in_FL", "FL"), add_port(f, in_dir, "in_FR", "FR")];
        }
        let r = sys::pw_filter_connect(
            f,
            sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
            std::ptr::null_mut(),
            0,
        );
        assert!(r >= 0, "pw_filter_connect failed: {r}");
        f
    };

    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let ml = mainloop.clone();
    let _rx = quit_rx.attach(mainloop.loop_(), move |_| ml.quit());
    let mut params = std::mem::replace(&mut main_side.params, rtrb::RingBuffer::new(1).0);
    let ctl = std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let mut i = 0u32;
        while t0.elapsed().as_secs() < secs {
            std::thread::sleep(std::time::Duration::from_millis(250));
            i += 1;
            let _ = params.push(Param::Freq(if i % 2 == 0 { 440.0 } else { 441.0 }));
        }
        let _ = quit_tx.send(());
    });
    mainloop.run();
    ctl.join().unwrap();

    let sched = sched_of(main_side.shared.tid.load(Relaxed));
    println!("filter node id={}", unsafe { sys::pw_filter_get_node_id(filter) });
    if duplex {
        println!(
            "duplex input: peak_milli+1={} (0 = no input buffer ever seen)",
            main_side.shared.peak_milli.load(Relaxed)
        );
    }
    main_side.report(if duplex { "pipewire-filter-duplex" } else { "pipewire-filter" }, quantum, secs, &sched);
    unsafe {
        sys::pw_filter_destroy(filter);
        drop(Box::from_raw(ud));
    }
    Ok(())
}
