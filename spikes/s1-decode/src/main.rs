//! Spike S1: hardware decode -> wgpu texture. Throwaway code.
//!
//!   s1 decode <file> <sw|vaapi|vulkan> [download] [threads=N]
//!   s1 seek   <file> <sw|vaapi|vulkan> [n=100] [threads=N] [download]
//!   s1 multi  <file> <sw|vaapi|vulkan> <streams> [download]
//!   s1 import <file> <dmabuf|dmabuf-nocache|vkdec|vamap|cpu-vaapi|cpu-sw> [frames=N]
//!   s1 verify <file> <frame-index>
//!   s1 play   <file> <dmabuf|vkdec|cpu-vaapi> [secs=10]
mod dec;
mod gpu;
mod play;

use dec::*;
use std::time::Instant;

fn arg_kv(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix(&format!("{key}=")).map(|s| s.to_string()))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: see top of src/main.rs");
        std::process::exit(2);
    }
    unsafe {
        ffmpeg_next::ffi::av_log_set_level(
            arg_kv(&args, "loglevel").map(|s| s.parse().unwrap()).unwrap_or(16),
        )
    };
    // Vulkan Video decode intermittently oopses the amdgpu kernel driver on the target machine
    // (kernel 7.2.15-zen1, see docs/spikes/s1-decode.md): require an explicit opt-in.
    let wants_vulkan = args[2..].iter().any(|a| a == "vulkan" || a == "vkdec" || a == "cpu-vulkan" || a.ends_with("=vkdec") || a.ends_with("=cpu-vulkan"));
    if wants_vulkan && !vulkan_allowed() {
        eprintln!("Vulkan Video decode is opt-in (kernel oops risk on this machine): set S1_VULKAN=1");
        std::process::exit(2);
    }
    match args[1].as_str() {
        "decode" => cmd_decode(&args),
        "seek" => cmd_seek(&args),
        "multi" => cmd_multi(&args),
        "import" => gpu::cmd_import(&args),
        "verify" => gpu::cmd_verify(&args),
        "play" => play::cmd_play(&args),
        _ => panic!("unknown command"),
    }
}

fn file_label(p: &str) -> &str {
    p.rsplit('/').next().unwrap()
}

fn cmd_decode(args: &[String]) {
    let path = &args[2];
    let hw = Hw::parse(&args[3]);
    let download = args.iter().any(|a| a == "download");
    let threads: i32 = arg_kv(args, "threads").map(|s| s.parse().unwrap()).unwrap_or(0);
    let dev = if hw != Hw::Sw { Some(HwDevice::create(hw)) } else { None };
    let mut d = Decoder::open(path, hw, dev.as_ref(), threads);
    let cpu0 = cpu_secs();
    let t0 = Instant::now();
    let mut n = 0u64;
    let mut fmt = String::new();
    let mut sink = 0u64;
    let last = unsafe { ffmpeg_next::ffi::av_frame_alloc() };
    while d.next_frame() {
        if n == 0 {
            fmt = d.frame_format();
        }
        n += 1;
        if download {
            let f = d.download();
            // touch the data so the copy is real
            unsafe { sink += *(*f).data[0] as u64 };
        }
        if hw != Hw::Sw && !download {
            // keep the measurement honest: do not let submissions queue up unbounded,
            // and remember the newest frame so the tail can be synced after EOF
            if n % 8 == 0 {
                d.sync();
            }
            unsafe {
                ffmpeg_next::ffi::av_frame_unref(last);
                ffmpeg_next::ffi::av_frame_ref(last, d.frame);
            }
        }
    }
    // everything still in flight on the GPU must finish inside the timed region
    sync_frame(last);
    let dt = t0.elapsed().as_secs_f64();
    let cpu = cpu_secs() - cpu0;
    println!(
        "decode file={} codec={} mode={:?}{} threads={} frames={} time={:.3}s fps={:.1} ms/frame={:.3} cpu={:.2}s cpu/frame={:.3}ms fmt={} {}",
        file_label(path),
        d.codec_name(),
        hw,
        if download { "+download" } else { "" },
        unsafe { (*d.ctx).thread_count },
        n,
        dt,
        n as f64 / dt,
        dt * 1000.0 / n as f64,
        cpu,
        cpu * 1000.0 / n as f64,
        fmt,
        if sink == u64::MAX { "" } else { "" },
    );
}

/// xorshift, fixed seed: reproducible seek targets
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn cmd_seek(args: &[String]) {
    let path = &args[2];
    let hw = Hw::parse(&args[3]);
    let n: usize = arg_kv(args, "n").map(|s| s.parse().unwrap()).unwrap_or(100);
    let threads: i32 = arg_kv(args, "threads").map(|s| s.parse().unwrap()).unwrap_or(0);
    let download = args.iter().any(|a| a == "download");
    let skip_nonref = args.iter().any(|a| a == "skipnonref");
    let dev = if hw != Hw::Sw { Some(HwDevice::create(hw)) } else { None };
    let mut d = Decoder::open(path, hw, dev.as_ref(), threads);
    d.skip_nonref = skip_nonref;
    // warm up: decode a few frames so the decoder and hw session exist
    for _ in 0..5 {
        d.next_frame();
    }
    let frame_dur = (1.0 / d.fps / (d.time_base.num as f64 / d.time_base.den as f64)).round() as i64;
    let total = if d.nb_frames > 0 { d.nb_frames } else { d.duration_ts / frame_dur };
    let mut rng = Rng(0x5107_6e5e_ed00_0001);
    let mut lat = Vec::new();
    let mut dist = Vec::new();
    for _ in 0..n {
        // stay 2 frames away from the end so frame threading can always output
        let target_frame = (rng.next() % (total as u64 - 2)) as i64;
        let ts = d.start_ts + target_frame * frame_dur;
        let t0 = Instant::now();
        let decoded = d.seek_exact(ts);
        if download {
            d.download();
        } else {
            d.sync();
        }
        lat.push(t0.elapsed().as_secs_f64() * 1000.0);
        dist.push(decoded as f64);
        assert!(d.frame_pts() == ts, "landed on pts {} wanted {}", d.frame_pts(), ts);
    }
    let mut s = lat.clone();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut ds = dist.clone();
    ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let total_frames: f64 = dist.iter().sum();
    let total_ms: f64 = lat.iter().sum();
    println!(
        "seek file={} codec={} mode={:?}{} threads={} n={} p50={:.1}ms p95={:.1}ms max={:.1}ms mean={:.1}ms | frames decoded per seek: p50={} p95={} max={} | {:.2} ms per decoded frame | open-GOP re-seeks={}",
        file_label(path),
        d.codec_name(),
        hw,
        if download { "+download" } else if d.skip_nonref { "+skipnonref" } else { "" },
        unsafe { (*d.ctx).thread_count },
        n,
        percentile(&s, 0.5),
        percentile(&s, 0.95),
        s[s.len() - 1],
        total_ms / n as f64,
        percentile(&ds, 0.5),
        percentile(&ds, 0.95),
        ds[ds.len() - 1],
        total_ms / total_frames,
        d.open_gop_retries,
    );
}

fn cmd_multi(args: &[String]) {
    let path = args[2].clone();
    let hw = Hw::parse(&args[3]);
    let streams: usize = args[4].parse().unwrap();
    let download = args.iter().any(|a| a == "download");
    let paced = args.iter().any(|a| a == "paced");
    let dev = std::sync::Arc::new(if hw != Hw::Sw { Some(HwDevice::create(hw)) } else { None });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(streams));
    let cpu0 = cpu_secs();
    let t0 = Instant::now();
    let handles: Vec<_> = (0..streams)
        .map(|i| {
            let path = path.clone();
            let dev = dev.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut d = Decoder::open(&path, hw, (*dev).as_ref(), 0);
                barrier.wait();
                let t0 = Instant::now();
                let mut n = 0u64;
                let mut late = 0u64;
                let mut worst_late = 0f64;
                let mut gaps = Vec::new();
                let mut last = Instant::now();
                while d.next_frame() {
                    if download {
                        d.download();
                    } else {
                        d.sync();
                    }
                    n += 1;
                    if paced {
                        // realtime pacing: frame n is due at n/fps; count frames
                        // that were not ready by their deadline.
                        let due = n as f64 / d.fps;
                        let now = t0.elapsed().as_secs_f64();
                        if now > due + 1.0 / d.fps {
                            late += 1;
                            worst_late = worst_late.max((now - due) * 1000.0);
                        } else if now < due {
                            std::thread::sleep(std::time::Duration::from_secs_f64(due - now));
                        }
                    }
                    gaps.push(last.elapsed().as_secs_f64() * 1000.0);
                    last = Instant::now();
                }
                let dt = t0.elapsed().as_secs_f64();
                gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
                println!(
                    "  stream {i}: frames={n} time={dt:.3}s fps={:.1} frame-gap p50={:.2}ms p99={:.2}ms max={:.2}ms late={late} worst_late={worst_late:.1}ms",
                    n as f64 / dt,
                    percentile(&gaps, 0.5),
                    percentile(&gaps, 0.99),
                    gaps[gaps.len() - 1],
                );
                (n, dt)
            })
        })
        .collect();
    let res: Vec<(u64, f64)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let dt = t0.elapsed().as_secs_f64();
    let total: u64 = res.iter().map(|r| r.0).sum();
    let min_fps = res.iter().map(|r| r.0 as f64 / r.1).fold(f64::MAX, f64::min);
    println!(
        "multi file={} mode={:?}{}{} streams={} total_frames={} wall={:.3}s aggregate_fps={:.1} min_stream_fps={:.1} cpu={:.2}s",
        file_label(&path),
        hw,
        if download { "+download" } else { "" },
        if paced { " paced" } else { "" },
        streams,
        total,
        dt,
        total as f64 / dt,
        min_fps,
        cpu_secs() - cpu0
    );
}

pub fn vulkan_allowed() -> bool {
    std::env::var("S1_VULKAN").is_ok_and(|v| v == "1")
}
