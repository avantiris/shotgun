//! S3 backend (a): native PipeWire client through the `pipewire` crate (pw_stream).
//!
//! usage: pw_engine <quantum> <secs> [--capture] [--force] [--no-rt]
//!   --capture  also open a capture stream at the same node.latency
//!   --force    additionally set node.force-quantum (pins the graph quantum)

use pipewire as pw;
use pw::{properties::properties, spa};
use s3_audio::*;
use spa::pod::Pod;
use std::sync::atomic::Ordering::Relaxed;

struct Ud {
    rt: Rt,
    pos: *mut spa::sys::spa_io_position,
    capture: bool,
}

fn format_pod() -> Vec<u8> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner()
}

fn process(stream: &pw::stream::Stream, ud: &mut Ud) {
    let start = ud.rt.begin();
    let mut snap = ClockSnap::default();
    // Driver clock: raw pointer handed to us in io_changed (no safe wrapper in pipewire 0.10).
    if !ud.pos.is_null() {
        let c = unsafe { &(*ud.pos).clock };
        ud.rt.clock(c.flags, c.position, c.duration, c.xrun);
        snap.clk_nsec = c.nsec;
        snap.clk_position = c.position;
        snap.clk_duration = c.duration;
        snap.clk_delay = c.delay;
        snap.clk_next_nsec = c.next_nsec;
        snap.clk_rate_diff = c.rate_diff;
    }
    // pw_stream_get_time_n(): documented RT-safe.
    if let Ok(t) = stream.time() {
        snap.t_now = t.now();
        snap.t_ticks = t.ticks();
        snap.t_delay = t.delay();
        snap.t_queued = t.queued();
        snap.t_buffered = t.buffered();
        snap.t_rate_den = t.rate().denom;
        ud.rt.shared.last_delay.store(t.delay(), Relaxed);
    }
    let mut n_frames = 0u32;
    match stream.dequeue_buffer() {
        None => {
            ud.rt.shared.no_buffer.fetch_add(1, Relaxed);
        }
        Some(mut buffer) => {
            let requested = buffer.requested() as usize;
            let stride = CHANNELS * 4;
            let datas = buffer.datas_mut();
            if let Some(d) = datas.first_mut() {
                if ud.capture {
                    let size = d.chunk().size() as usize;
                    let off = d.chunk().offset() as usize;
                    if let Some(bytes) = d.data() {
                        let end = (off + size).min(bytes.len());
                        let bytes = &bytes[off.min(end)..end];
                        let s: &[f32] = unsafe {
                            std::slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len() / 4)
                        };
                        let mut peak = 0f32;
                        for v in s {
                            peak = peak.max(v.abs());
                        }
                        let pm = (peak * 1000.0) as u32;
                        if pm > ud.rt.shared.peak_milli.load(Relaxed) {
                            ud.rt.shared.peak_milli.store(pm, Relaxed);
                        }
                        n_frames = (bytes.len() / stride) as u32;
                    }
                } else {
                    if let Some(bytes) = d.data() {
                        let mut n = bytes.len() / stride;
                        if requested > 0 {
                            n = n.min(requested);
                        }
                        // mmap'ed buffer, page aligned
                        let s: &mut [f32] = unsafe {
                            std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast(), n * CHANNELS)
                        };
                        ud.rt.render_interleaved(s);
                        n_frames = n as u32;
                    }
                    let chunk = d.chunk_mut();
                    *chunk.offset_mut() = 0;
                    *chunk.stride_mut() = stride as i32;
                    *chunk.size_mut() = n_frames * stride as u32;
                }
            }
        }
    }
    ud.rt.end(start, n_frames, snap);
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let quantum: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(128);
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30);
    let want_capture = args.iter().any(|a| a == "--capture");
    let force = args.iter().any(|a| a == "--force");
    let no_rt = args.iter().any(|a| a == "--no-rt");

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let latency = format!("{quantum}/{RATE}");
    let mk_props = |name: &str, cat: &str| {
        let mut p = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => cat,
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::NODE_NAME => name,
            *pw::keys::APP_NAME => "shotgun-s3",
            *pw::keys::NODE_LATENCY => latency.as_str(),
            *pw::keys::NODE_RATE => "1/48000",
        };
        if force {
            p.insert(*pw::keys::NODE_FORCE_QUANTUM, quantum.to_string());
        }
        p
    };
    let flags = {
        let mut f = pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
        if !no_rt {
            f |= pw::stream::StreamFlags::RT_PROCESS;
        }
        f
    };

    let (rt, mut main_side) = make();
    let play = pw::stream::StreamBox::new(&core, "s3-pw-play", mk_props("s3-pw-play", "Playback"))?;
    let _play_listener = play
        .add_local_listener_with_user_data(Ud { rt, pos: std::ptr::null_mut(), capture: false })
        .io_changed(|_, ud, id, area, _size| {
            if id == spa::sys::SPA_IO_Position {
                ud.pos = area.cast();
            }
        })
        .state_changed(|_, _, old, new| eprintln!("play state {old:?} -> {new:?}"))
        .process(process)
        .register()?;
    let fmt = format_pod();
    play.connect(spa::utils::Direction::Output, None, flags, &mut [Pod::from_bytes(&fmt).unwrap()])?;

    let mut cap_side = None;
    let mut _cap_keep = None;
    if want_capture {
        let (rt, ms) = make();
        let cap =
            pw::stream::StreamBox::new(&core, "s3-pw-capture", mk_props("s3-pw-capture", "Capture"))?;
        let l = cap
            .add_local_listener_with_user_data(Ud { rt, pos: std::ptr::null_mut(), capture: true })
            .io_changed(|_, ud, id, area, _size| {
                if id == spa::sys::SPA_IO_Position {
                    ud.pos = area.cast();
                }
            })
            .state_changed(|_, _, old, new| eprintln!("capture state {old:?} -> {new:?}"))
            .process(process)
            .register()?;
        cap.connect(spa::utils::Direction::Input, None, flags, &mut [Pod::from_bytes(&fmt).unwrap()])?;
        cap_side = Some(ms);
        _cap_keep = Some((cap, l));
    }

    // Control thread: parameter changes through the lock-free queue, then quit.
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
    println!("node ids: play={} (0xffffffff until connected)", play.node_id());
    mainloop.run();
    ctl.join().unwrap();

    println!("play node id={}", play.node_id());
    let label = if no_rt { "pipewire-stream(no RT_PROCESS)" } else { "pipewire-stream" };
    let sched = sched_of(main_side.shared.tid.load(Relaxed));
    main_side.report(label, quantum, secs, &sched);
    if let Some(mut cs) = cap_side {
        let sched = sched_of(cs.shared.tid.load(Relaxed));
        println!("capture peak (0..1000)={}", cs.shared.peak_milli.load(Relaxed));
        cs.report("pipewire-stream-capture", quantum, secs, &sched);
    }
    Ok(())
}
