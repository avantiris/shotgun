//! S3 backend (b): the same engine through cpal.
//!
//! usage: cpal_engine list
//!        cpal_engine <alsa|jack|pipewire> <quantum> <secs> [--capture] [--device <cpal device id>]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, HostId, StreamConfig};
use s3_audio::*;
use std::sync::atomic::Ordering::Relaxed;

fn host_id(name: &str) -> HostId {
    match name {
        "alsa" => HostId::Alsa,
        "jack" => HostId::Jack,
        "pipewire" => HostId::PipeWire,
        other => panic!("unknown host {other}"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("list");
    println!("cpal compiled hosts (ALL_HOSTS): {:?}", cpal::ALL_HOSTS);
    println!("cpal available_hosts(): {:?}", cpal::available_hosts());
    if mode == "list" {
        for id in cpal::available_hosts() {
            let host = match cpal::host_from_id(id) {
                Ok(h) => h,
                Err(e) => {
                    println!("host {id:?}: unavailable: {e}");
                    continue;
                }
            };
            println!("host {id:?}:");
            println!(
                "  default output: {:?}",
                host.default_output_device().map(|d| d.id().map(|i| i.to_string()))
            );
            println!(
                "  default input: {:?}",
                host.default_input_device().map(|d| d.id().map(|i| i.to_string()))
            );
            if let Ok(devs) = host.devices() {
                for d in devs {
                    let id = d.id().map(|i| i.to_string()).unwrap_or_default();
                    let out = d.default_output_config().ok();
                    let inp = d.default_input_config().ok();
                    println!("  device {id}: out={out:?} in={inp:?}");
                }
            }
        }
        return Ok(());
    }
    let quantum: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(128);
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let want_capture = args.iter().any(|a| a == "--capture");
    let dev_arg = args.iter().position(|a| a == "--device").and_then(|i| args.get(i + 1));

    if mode == "jack" {
        // PipeWire's libjack takes the wanted quantum from this variable; cpal's JACK host can only
        // accept the server buffer size (BufferSize::Fixed(n) != server size is rejected).
        std::env::set_var("PIPEWIRE_LATENCY", format!("{quantum}/{RATE}"));
    }
    let host = cpal::host_from_id(host_id(mode))?;
    let device = match dev_arg {
        Some(id) => host.device_by_id(&id.parse()?).expect("device id not found"),
        None => host.default_output_device().expect("no default output device"),
    };
    println!("output device: {}", device.id()?);
    println!("default output config: {:?}", device.default_output_config());
    let config = StreamConfig {
        channels: CHANNELS as u16,
        sample_rate: RATE,
        buffer_size: if mode == "jack" { BufferSize::Default } else { BufferSize::Fixed(quantum) },
    };

    let (mut rt, mut main_side) = make();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            let start = rt.begin();
            let mut snap = ClockSnap::default();
            let ts = info.timestamp();
            snap.cpal_latency_ns = ts
                .playback
                .checked_duration_since(ts.callback)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(-1);
            snap.clk_nsec = ts.callback.as_nanos() as u64;
            rt.render_interleaved(data);
            let n = (data.len() / CHANNELS) as u32;
            rt.end(start, n, snap);
        },
        |err| eprintln!("cpal output error: {err} (kind {:?})", err.kind()),
        None,
    )?;
    stream.play()?;
    println!("output stream.buffer_size() = {:?}", stream.buffer_size());

    let mut cap_side = None;
    let mut _cap_stream = None;
    if want_capture {
        let indev = host.default_input_device().expect("no default input device");
        println!("input device: {}", indev.id()?);
        let (mut crt, cms) = make();
        let s = indev.build_input_stream(
            config,
            move |data: &[f32], info: &cpal::InputCallbackInfo| {
                let start = crt.begin();
                let mut snap = ClockSnap::default();
                let ts = info.timestamp();
                snap.cpal_latency_ns = ts
                    .callback
                    .checked_duration_since(ts.capture)
                    .map(|d| d.as_nanos() as i64)
                    .unwrap_or(-1);
                let mut peak = 0f32;
                for v in data {
                    peak = peak.max(v.abs());
                }
                let pm = (peak * 1000.0) as u32;
                if pm > crt.shared.peak_milli.load(Relaxed) {
                    crt.shared.peak_milli.store(pm, Relaxed);
                }
                crt.end(start, (data.len() / CHANNELS) as u32, snap);
            },
            |err| eprintln!("cpal input error: {err} (kind {:?})", err.kind()),
            None,
        )?;
        s.play()?;
        println!("input stream.buffer_size() = {:?}", s.buffer_size());
        cap_side = Some(cms);
        _cap_stream = Some(s);
    }

    let t0 = std::time::Instant::now();
    let mut i = 0u32;
    while t0.elapsed().as_secs() < secs {
        std::thread::sleep(std::time::Duration::from_millis(250));
        i += 1;
        let _ = main_side.params.push(Param::Freq(if i % 2 == 0 { 440.0 } else { 441.0 }));
    }
    println!("output stream.buffer_size() at end = {:?}", stream.buffer_size());
    let sched = sched_of(main_side.shared.tid.load(Relaxed));
    let cap_sched = cap_side.as_ref().map(|c| sched_of(c.shared.tid.load(Relaxed)));
    drop(stream);
    drop(_cap_stream);
    main_side.report(&format!("cpal-{mode}"), quantum, secs, &sched);
    if let Some(mut cs) = cap_side {
        println!("capture peak (0..1000)={}", cs.shared.peak_milli.load(Relaxed));
        cs.report(&format!("cpal-{mode}-capture"), quantum, secs, &cap_sched.unwrap());
    }
    Ok(())
}
