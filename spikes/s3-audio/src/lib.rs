//! Shared measurement helpers for the S3 audio spike (throwaway code).
//!
//! Everything the realtime callback touches is preallocated: atomics, an SPSC
//! ring for per-callback period samples, an SPSC ring for clock snapshots and
//! an SPSC ring for parameter changes.

use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
/// -40 dBFS peak.
pub const GAIN: f32 = 0.01;

#[inline]
pub fn mono_ns() -> u64 {
    // vDSO call, no syscall on x86_64.
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[derive(Clone, Copy, Debug)]
pub enum Param {
    Freq(f32),
    Gain(f32),
}

/// One clock observation taken inside the realtime callback.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClockSnap {
    /// CLOCK_MONOTONIC at callback entry.
    pub cb_mono_ns: u64,
    /// Frames this engine has rendered before this callback (our sample counter).
    pub engine_frames: u64,
    /// Frames rendered in this callback.
    pub n_frames: u32,
    // backend-provided values (0 when the backend does not provide them)
    pub t_now: i64,
    pub t_ticks: u64,
    pub t_delay: i64,
    pub t_queued: u64,
    pub t_buffered: u64,
    pub t_rate_den: u32,
    pub clk_nsec: u64,
    pub clk_position: u64,
    pub clk_duration: u64,
    pub clk_delay: i64,
    pub clk_next_nsec: u64,
    pub clk_rate_diff: f64,
    /// cpal: playback instant minus callback instant, ns.
    pub cpal_latency_ns: i64,
}

#[derive(Default)]
pub struct Shared {
    pub callbacks: AtomicU64,
    pub frames: AtomicU64,
    pub min_frames: AtomicU32,
    pub max_frames: AtomicU32,
    pub max_dur_ns: AtomicU64,
    pub sum_dur_ns: AtomicU64,
    /// callback start-to-start interval > 1.5x the nominal period of the previous block
    pub late: AtomicU64,
    /// driver position jumped by more than the previous duration (missed cycle)
    pub pos_gaps: AtomicU64,
    /// SPA_IO_CLOCK_FLAG_XRUN_RECOVER seen
    pub xrun_recover: AtomicU64,
    /// spa_io_clock.xrun first / last value seen
    pub clk_xrun_first: AtomicU64,
    pub clk_xrun_last: AtomicU64,
    pub quantum_last: AtomicU32,
    pub quantum_min: AtomicU32,
    pub quantum_max: AtomicU32,
    pub no_buffer: AtomicU64,
    pub tid: AtomicI32,
    pub last_delay: AtomicI64,
    pub peak_milli: AtomicU32,
    pub ring_overflow: AtomicU64,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        let s = Shared::default();
        s.min_frames.store(u32::MAX, Relaxed);
        s.quantum_min.store(u32::MAX, Relaxed);
        s.clk_xrun_first.store(u64::MAX, Relaxed);
        Arc::new(s)
    }
}

/// Realtime-side state: owned by the callback closure.
/// Timestamped xrun-ish event recorded by the RT thread.
#[derive(Clone, Copy, Debug)]
pub struct Ev {
    /// ms since the first callback
    pub t_ms: u32,
    /// 0=late callback (val=interval us) 1=driver position gap (val=frames skipped)
    /// 2=XRUN_RECOVER flag 3=driver clock.xrun increased (val=frames added)
    pub kind: u8,
    pub val: u32,
}

pub struct Rt {
    pub shared: Arc<Shared>,
    pub events: rtrb::Producer<Ev>,
    first_start: u64,
    last_clk_xrun: u64,
    cur_ms: u32,
    pub deltas: rtrb::Producer<u32>,
    pub snaps: rtrb::Producer<ClockSnap>,
    pub params: rtrb::Consumer<Param>,
    pub phase: f32,
    pub freq: f32,
    pub gain: f32,
    pub engine_frames: u64,
    last_start: u64,
    last_frames: u32,
    pub last_pos: u64,
    pub last_dur: u64,
    snap_countdown: u32,
}

pub struct MainSide {
    pub shared: Arc<Shared>,
    pub events: rtrb::Consumer<Ev>,
    pub deltas: rtrb::Consumer<u32>,
    pub snaps: rtrb::Consumer<ClockSnap>,
    pub params: rtrb::Producer<Param>,
}

pub fn make() -> (Rt, MainSide) {
    let shared = Shared::new();
    let (dp, dc) = rtrb::RingBuffer::<u32>::new(1 << 20);
    let (sp, sc) = rtrb::RingBuffer::<ClockSnap>::new(4096);
    let (pp, pc) = rtrb::RingBuffer::<Param>::new(256);
    let (ep, ec) = rtrb::RingBuffer::<Ev>::new(65536);
    (
        Rt {
            shared: shared.clone(),
            events: ep,
            first_start: 0,
            last_clk_xrun: u64::MAX,
            cur_ms: 0,
            deltas: dp,
            snaps: sp,
            params: pc,
            phase: 0.0,
            freq: 440.0,
            gain: GAIN,
            engine_frames: 0,
            last_start: 0,
            last_frames: 0,
            last_pos: 0,
            last_dur: 0,
            snap_countdown: 0,
        },
        MainSide { shared, events: ec, deltas: dc, snaps: sc, params: pp },
    )
}

impl Rt {
    /// Call at callback entry. Returns the entry timestamp.
    #[inline]
    pub fn begin(&mut self) -> u64 {
        let now = mono_ns();
        let s = &self.shared;
        if s.tid.load(Relaxed) == 0 {
            // one syscall, first callback only (measurement aid, not product code)
            s.tid.store(unsafe { libc::gettid() }, Relaxed);
        }
        if self.first_start == 0 {
            self.first_start = now;
        }
        self.cur_ms = ((now - self.first_start) / 1_000_000) as u32;
        if self.last_start != 0 {
            let d = now - self.last_start;
            if self.deltas.push(d.min(u32::MAX as u64) as u32).is_err() {
                s.ring_overflow.fetch_add(1, Relaxed);
            }
            let nominal = self.last_frames as u64 * 1_000_000_000 / RATE as u64;
            if nominal > 0 && d > nominal * 3 / 2 {
                s.late.fetch_add(1, Relaxed);
                let _ = self.events.push(Ev { t_ms: self.cur_ms, kind: 0, val: (d / 1000) as u32 });
            }
        }
        self.last_start = now;
        while let Ok(p) = self.params.pop() {
            match p {
                Param::Freq(f) => self.freq = f,
                Param::Gain(g) => self.gain = g,
            }
        }
        now
    }

    /// Render an interleaved stereo f32 block.
    #[inline]
    pub fn render_interleaved(&mut self, out: &mut [f32]) {
        let step = self.freq / RATE as f32;
        for fr in out.chunks_exact_mut(CHANNELS) {
            let v = (self.phase * std::f32::consts::TAU).sin() * self.gain;
            self.phase += step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            fr[0] = v;
            fr[1] = v;
        }
    }

    /// Render the same sample into two planar buffers.
    #[inline]
    pub fn render_planar(&mut self, l: &mut [f32], r: &mut [f32]) {
        let step = self.freq / RATE as f32;
        for (a, b) in l.iter_mut().zip(r.iter_mut()) {
            let v = (self.phase * std::f32::consts::TAU).sin() * self.gain;
            self.phase += step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            *a = v;
            *b = v;
        }
    }

    /// Record driver clock data (PipeWire backends).
    #[inline]
    pub fn clock(&mut self, flags: u32, position: u64, duration: u64, xrun: u64) {
        let s = &self.shared;
        let q = duration as u32;
        s.quantum_last.store(q, Relaxed);
        if q < s.quantum_min.load(Relaxed) {
            s.quantum_min.store(q, Relaxed);
        }
        if q > s.quantum_max.load(Relaxed) {
            s.quantum_max.store(q, Relaxed);
        }
        if flags & 2 != 0 {
            s.xrun_recover.fetch_add(1, Relaxed);
            let _ = self.events.push(Ev { t_ms: self.cur_ms, kind: 2, val: 0 });
        }
        if s.clk_xrun_first.load(Relaxed) == u64::MAX {
            s.clk_xrun_first.store(xrun, Relaxed);
        }
        s.clk_xrun_last.store(xrun, Relaxed);
        if self.last_clk_xrun != u64::MAX && xrun > self.last_clk_xrun {
            let _ = self.events.push(Ev { t_ms: self.cur_ms, kind: 3, val: (xrun - self.last_clk_xrun) as u32 });
        }
        self.last_clk_xrun = xrun;
        if self.last_dur != 0 && position != self.last_pos + self.last_dur {
            s.pos_gaps.fetch_add(1, Relaxed);
            let skipped = position.wrapping_sub(self.last_pos + self.last_dur) as u32;
            let _ = self.events.push(Ev { t_ms: self.cur_ms, kind: 1, val: skipped });
        }
        self.last_pos = position;
        self.last_dur = duration;
    }

    /// Call at callback exit.
    #[inline]
    pub fn end(&mut self, start: u64, n_frames: u32, mut snap: ClockSnap) {
        let s = &self.shared;
        if self.snap_countdown == 0 {
            snap.cb_mono_ns = start;
            snap.engine_frames = self.engine_frames;
            snap.n_frames = n_frames;
            let _ = self.snaps.push(snap);
            // roughly one snapshot per second
            self.snap_countdown = RATE / n_frames.max(1);
        } else {
            self.snap_countdown -= 1;
        }
        self.engine_frames += n_frames as u64;
        self.last_frames = n_frames;
        s.callbacks.fetch_add(1, Relaxed);
        s.frames.fetch_add(n_frames as u64, Relaxed);
        if n_frames < s.min_frames.load(Relaxed) {
            s.min_frames.store(n_frames, Relaxed);
        }
        if n_frames > s.max_frames.load(Relaxed) {
            s.max_frames.store(n_frames, Relaxed);
        }
        let dur = mono_ns() - start;
        s.sum_dur_ns.fetch_add(dur, Relaxed);
        if dur > s.max_dur_ns.load(Relaxed) {
            s.max_dur_ns.store(dur, Relaxed);
        }
    }
}

/// Scheduling policy/priority of a thread of this process, read from outside the RT thread.
pub fn sched_of(tid: i32) -> String {
    if tid == 0 {
        return "no callback seen".into();
    }
    let pol = unsafe { libc::sched_getscheduler(tid) };
    let mut p = libc::sched_param { sched_priority: 0 };
    unsafe { libc::sched_getparam(tid, &mut p) };
    let name = match pol & !libc::SCHED_RESET_ON_FORK {
        libc::SCHED_OTHER => "SCHED_OTHER",
        libc::SCHED_FIFO => "SCHED_FIFO",
        libc::SCHED_RR => "SCHED_RR",
        libc::SCHED_BATCH => "SCHED_BATCH",
        _ => "other",
    };
    let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid as u32) };
    let comm = std::fs::read_to_string(format!("/proc/self/task/{tid}/comm")).unwrap_or_default();
    format!("tid={tid} comm={} policy={name} rtprio={} nice={nice}", comm.trim(), p.sched_priority)
}

impl MainSide {
    /// Print the result block. `label` identifies backend, `req` the requested quantum.
    pub fn report(&mut self, label: &str, req: u32, secs: u64, sched: &str) {
        let s = &self.shared;
        let mut d: Vec<u32> = Vec::with_capacity(1 << 20);
        while let Ok(v) = self.deltas.pop() {
            d.push(v);
        }
        // drop the first 200 ms worth of callbacks (stream start-up, quantum switch)
        let frames_min = s.min_frames.load(Relaxed);
        let frames_max = s.max_frames.load(Relaxed);
        let cbs = s.callbacks.load(Relaxed);
        let frames = s.frames.load(Relaxed);
        let avg_frames = if cbs > 0 { frames as f64 / cbs as f64 } else { 0.0 };
        let skip = ((RATE as f64 * 0.5) / avg_frames.max(1.0)) as usize;
        let d = if d.len() > skip * 2 { d[skip..].to_vec() } else { d };
        // nominal period: median interval
        let mut sorted = d.clone();
        sorted.sort_unstable();
        let pct = |v: &Vec<u32>, p: f64| -> f64 {
            if v.is_empty() {
                return f64::NAN;
            }
            let i = ((v.len() - 1) as f64 * p).round() as usize;
            v[i] as f64 / 1000.0
        };
        let median_us = pct(&sorted, 0.5);
        let nominal_us = avg_frames * 1e6 / RATE as f64;
        let mut dev: Vec<u32> =
            d.iter().map(|&v| (v as f64 - nominal_us * 1000.0).abs() as u32).collect();
        dev.sort_unstable();
        println!("RESULT backend={label} requested={req} secs={secs}");
        println!(
            "  quantum granted (driver clock.duration): last={} min={} max={}",
            s.quantum_last.load(Relaxed),
            s.quantum_min.load(Relaxed),
            s.quantum_max.load(Relaxed)
        );
        println!(
            "  callbacks={cbs} frames={frames} frames/callback min={frames_min} max={frames_max} avg={avg_frames:.2}"
        );
        println!(
            "  period us: nominal={nominal_us:.1} median={median_us:.1} min={:.1} max={:.1}  (n={} after skipping first {skip})",
            pct(&sorted, 0.0),
            pct(&sorted, 1.0),
            sorted.len()
        );
        println!(
            "  period deviation |dt-nominal| us: p50={:.1} p99={:.1} p99.9={:.1} max={:.1}",
            pct(&dev, 0.5),
            pct(&dev, 0.99),
            pct(&dev, 0.999),
            pct(&dev, 1.0)
        );
        println!(
            "  callback duration us: avg={:.2} max={:.1}",
            if cbs > 0 { s.sum_dur_ns.load(Relaxed) as f64 / cbs as f64 / 1000.0 } else { 0.0 },
            s.max_dur_ns.load(Relaxed) as f64 / 1000.0
        );
        let xf = s.clk_xrun_first.load(Relaxed);
        let xl = s.clk_xrun_last.load(Relaxed);
        println!(
            "  own xrun detection: late(>1.5x period)={} position_gaps={} xrun_recover_flag={} clock.xrun first={} last={} no_buffer={} ring_overflow={}",
            s.late.load(Relaxed),
            s.pos_gaps.load(Relaxed),
            s.xrun_recover.load(Relaxed),
            if xf == u64::MAX { 0 } else { xf },
            xl,
            s.no_buffer.load(Relaxed),
            s.ring_overflow.load(Relaxed)
        );
println!("  callback thread: {sched}");
        let mut evs = Vec::new();
        while let Ok(e) = self.events.pop() {
            evs.push(e);
        }
        let names = ["late", "position_gap", "xrun_recover_flag", "clock.xrun_increase"];
        let end_ms = secs as u32 * 1000;
        for (k, name) in names.iter().enumerate() {
            let all: Vec<&Ev> = evs.iter().filter(|e| e.kind as usize == k).collect();
            if all.is_empty() {
                continue;
            }
            let startup = all.iter().filter(|e| e.t_ms < 2000).count();
            let steady = all.iter().filter(|e| e.t_ms >= 2000 && e.t_ms + 1000 < end_ms).count();
            let tail = all.len() - startup - steady;
            let sum: u64 = all.iter().map(|e| e.val as u64).sum();
            let times: Vec<String> = all.iter().take(12).map(|e| format!("{}ms:{}", e.t_ms, e.val)).collect();
            println!("  events {name}: total={} startup(<2s)={startup} steady={steady} last-second={tail} sum(val)={sum} first: {}", all.len(), times.join(" "));
        }
        println!("  steady-state event count (2s..end-1s): {}", evs.iter().filter(|e| e.t_ms >= 2000 && e.t_ms + 1000 < end_ms).count());
        let mut snaps = Vec::new();
        while let Ok(v) = self.snaps.pop() {
            snaps.push(v);
        }
        println!("  clock snapshots ({} total, showing up to 4 from the middle):", snaps.len());
        let mid = snaps.len() / 2;
        for sn in snaps.iter().skip(mid.saturating_sub(2)).take(4) {
            println!("    {sn:?}");
        }
        // Clock analysis over all snapshots (skip the first two: start-up).
        let pw: Vec<&ClockSnap> = snaps.iter().skip(2).filter(|s| s.t_rate_den != 0).collect();
        if pw.len() >= 3 {
            let (a, b) = (pw[0], pw[pw.len() - 1]);
            let off_ticks: Vec<i64> =
                pw.iter().map(|s| s.t_ticks as i64 - s.engine_frames as i64).collect();
            let off_pos: Vec<i64> =
                pw.iter().map(|s| s.clk_position as i64 - s.engine_frames as i64).collect();
            let wake: Vec<i64> = pw.iter().map(|s| s.cb_mono_ns as i64 - s.t_now).collect();
            let delays: Vec<i64> = pw.iter().map(|s| s.t_delay).collect();
            let cdel: Vec<i64> = pw.iter().map(|s| s.clk_delay).collect();
            let mm = |v: &Vec<i64>| (*v.iter().min().unwrap(), *v.iter().max().unwrap());
            let eff = (b.engine_frames - a.engine_frames) as f64 * 1e9 / (b.t_now - a.t_now) as f64;
            // residual of "position = N0 + (now - now0) * eff_rate" at every snapshot, in frames
            let mut max_res = 0f64;
            for s in &pw {
                let pred = a.engine_frames as f64 + (s.t_now - a.t_now) as f64 * eff / 1e9;
                max_res = max_res.max((pred - s.engine_frames as f64).abs());
            }
            println!(
                "  clock check over {} snapshots: pw_time.ticks-engine_frames min/max={:?}  clock.position-engine_frames min/max={:?}",
                pw.len(), mm(&off_ticks), mm(&off_pos)
            );
            println!(
                "    effective rate vs CLOCK_MONOTONIC={eff:.3} Hz, max residual of linear extrapolation={max_res:.2} frames, callback entry - pw_time.now min/max={:?} ns",
                mm(&wake)
            );
            println!(
                "    pw_time.delay min/max={:?} frames, clock.delay min/max={:?} frames",
                mm(&delays), mm(&cdel)
            );
        }
        if let Some(sn) = snaps.get(mid) {
            if sn.t_rate_den != 0 {
                let delay_ms = sn.t_delay as f64 * 1000.0 / sn.t_rate_den as f64;
                println!(
                    "  pw_time: delay={} frames ({delay_ms:.2} ms) buffered={} queued={}  clock.delay={}",
                    sn.t_delay, sn.t_buffered, sn.t_queued, sn.clk_delay
                );
            }
            if sn.cpal_latency_ns != 0 {
                println!(
                    "  cpal timestamp: playback-callback = {:.2} ms",
                    sn.cpal_latency_ns as f64 / 1e6
                );
            }
        }
    }
}
