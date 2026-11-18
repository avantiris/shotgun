//! Minimal FFmpeg decoder over raw ffmpeg-sys-next bindings (throwaway spike code).
#![allow(dead_code)]
use ffmpeg_next::ffi::*;
use std::ffi::{c_int, c_void, CStr, CString};
use std::ptr::{null, null_mut};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hw {
    Sw,
    Vaapi,
    Vulkan,
}

impl Hw {
    pub fn parse(s: &str) -> Hw {
        match s {
            "sw" => Hw::Sw,
            "vaapi" => Hw::Vaapi,
            "vulkan" => Hw::Vulkan,
            _ => panic!("mode must be sw|vaapi|vulkan"),
        }
    }
    fn pix_fmt(self) -> AVPixelFormat {
        match self {
            Hw::Sw => AVPixelFormat::AV_PIX_FMT_NONE,
            Hw::Vaapi => AVPixelFormat::AV_PIX_FMT_VAAPI,
            Hw::Vulkan => AVPixelFormat::AV_PIX_FMT_VULKAN,
        }
    }
}

#[link(name = "va")]
extern "C" {
    fn vaSyncSurface(dpy: *mut c_void, surface: u32) -> c_int;
}

pub fn err_str(e: c_int) -> String {
    let mut buf = [0i8; 256];
    unsafe {
        av_strerror(e, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

fn ck(e: c_int, what: &str) -> c_int {
    if e < 0 {
        panic!("{what}: {}", err_str(e));
    }
    e
}

/// Wrapper so hw device refs can cross threads.
pub struct HwDevice(pub *mut AVBufferRef);
unsafe impl Send for HwDevice {}
unsafe impl Sync for HwDevice {}

impl HwDevice {
    /// Device created by FFmpeg itself.
    pub fn create(hw: Hw) -> HwDevice {
        let mut r: *mut AVBufferRef = null_mut();
        unsafe {
            match hw {
                Hw::Vaapi => {
                    let dev = CString::new(
                        std::env::var("S1_DRM_NODE").unwrap_or("/dev/dri/renderD128".into()),
                    )
                    .unwrap();
                    ck(
                        av_hwdevice_ctx_create(
                            &mut r,
                            AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                            dev.as_ptr(),
                            null_mut(),
                            0,
                        ),
                        "vaapi device",
                    );
                }
                Hw::Vulkan => {
                    ck(
                        av_hwdevice_ctx_create(
                            &mut r,
                            AVHWDeviceType::AV_HWDEVICE_TYPE_VULKAN,
                            null(),
                            null_mut(),
                            0,
                        ),
                        "vulkan device",
                    );
                }
                Hw::Sw => {}
            }
        }
        HwDevice(r)
    }
}

unsafe extern "C" fn get_format(
    ctx: *mut AVCodecContext,
    fmts: *const AVPixelFormat,
) -> AVPixelFormat {
    let want: AVPixelFormat = std::mem::transmute((*ctx).opaque as usize as i32);
    let mut p = fmts;
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == want {
            return want;
        }
        p = p.add(1);
    }
    eprintln!("get_format: hw format {:?} not offered, falling back to software", want);
    *fmts
}

pub struct Decoder {
    pub fmt: *mut AVFormatContext,
    pub ctx: *mut AVCodecContext,
    pub stream: c_int,
    pkt: *mut AVPacket,
    pub frame: *mut AVFrame,
    pub sw_frame: *mut AVFrame,
    pub hw: Hw,
    pub time_base: AVRational,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub nb_frames: i64,
    pub duration_ts: i64,
    pub start_ts: i64,
    draining: bool,
    eof: bool,
    pub packets_sent: u64,
    pub skip_nonref: bool,
    pub open_gop_retries: u32,
    skip_nonref_before: Option<i64>,
}
unsafe impl Send for Decoder {}

impl Decoder {
    /// `threads`: 0 = FFmpeg auto. For hw decode 1 is used unless overridden.
    pub fn open(path: &str, hw: Hw, dev: Option<&HwDevice>, threads: i32) -> Decoder {
        unsafe {
            let cpath = CString::new(path).unwrap();
            let mut fmt: *mut AVFormatContext = null_mut();
            ck(
                avformat_open_input(&mut fmt, cpath.as_ptr(), null(), null_mut()),
                "open input",
            );
            ck(avformat_find_stream_info(fmt, null_mut()), "stream info");
            let mut codec: *const AVCodec = null();
            let stream = ck(
                av_find_best_stream(fmt, AVMediaType::AVMEDIA_TYPE_VIDEO, -1, -1, &mut codec, 0),
                "best stream",
            );
            let st = *(*fmt).streams.add(stream as usize);
            let ctx = avcodec_alloc_context3(codec);
            ck(avcodec_parameters_to_context(ctx, (*st).codecpar), "params");
            (*ctx).pkt_timebase = (*st).time_base;
            if hw != Hw::Sw {
                let d = dev.expect("hw device required");
                (*ctx).hw_device_ctx = av_buffer_ref(d.0);
                (*ctx).opaque = hw.pix_fmt() as i32 as usize as *mut c_void;
                (*ctx).get_format = Some(get_format);
                (*ctx).thread_count = if threads > 0 { threads } else { 1 };
                if std::env::var("S1_ALLOW_PROFILE_MISMATCH").is_ok() {
                    (*ctx).hwaccel_flags |= AV_HWACCEL_FLAG_ALLOW_PROFILE_MISMATCH as i32;
                }
            } else {
                (*ctx).thread_count = threads;
                (*ctx).thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
            }
            ck(avcodec_open2(ctx, codec, null_mut()), "open codec");
            let fr = av_guess_frame_rate(fmt, st, null_mut());
            Decoder {
                fmt,
                ctx,
                stream,
                pkt: av_packet_alloc(),
                frame: av_frame_alloc(),
                sw_frame: av_frame_alloc(),
                hw,
                time_base: (*st).time_base,
                fps: fr.num as f64 / fr.den as f64,
                width: (*ctx).width as u32,
                height: (*ctx).height as u32,
                nb_frames: (*st).nb_frames,
                duration_ts: (*st).duration,
                start_ts: if (*st).start_time == AV_NOPTS_VALUE { 0 } else { (*st).start_time },
                draining: false,
                eof: false,
                packets_sent: 0,
                skip_nonref: false,
                open_gop_retries: 0,
                skip_nonref_before: None,
            }
        }
    }

    pub fn codec_name(&self) -> String {
        unsafe { CStr::from_ptr((*(*self.ctx).codec).name).to_string_lossy().into_owned() }
    }

    pub fn ts_to_secs(&self, ts: i64) -> f64 {
        ts as f64 * self.time_base.num as f64 / self.time_base.den as f64
    }

    /// Decodes the next frame into `self.frame`. Returns false at EOF.
    pub fn next_frame(&mut self) -> bool {
        unsafe {
            loop {
                let r = avcodec_receive_frame(self.ctx, self.frame);
                if r == 0 {
                    return true;
                }
                if r == AVERROR_EOF {
                    self.eof = true;
                    return false;
                }
                if r != AVERROR(libc::EAGAIN) {
                    panic!("receive_frame: {}", err_str(r));
                }
                if self.draining {
                    return false;
                }
                // feed
                loop {
                    let r = av_read_frame(self.fmt, self.pkt);
                    if r < 0 {
                        self.draining = true;
                        avcodec_send_packet(self.ctx, null());
                        break;
                    }
                    if (*self.pkt).stream_index == self.stream {
                        if let Some(t) = self.skip_nonref_before {
                            (*self.ctx).skip_frame = if (*self.pkt).pts < t {
                                AVDiscard::AVDISCARD_NONREF
                            } else {
                                AVDiscard::AVDISCARD_DEFAULT
                            };
                        }
                        let r = avcodec_send_packet(self.ctx, self.pkt);
                        av_packet_unref(self.pkt);
                        self.packets_sent += 1;
                        if r < 0 && r != AVERROR(libc::EAGAIN) {
                            panic!("send_packet: {}", err_str(r));
                        }
                        break;
                    }
                    av_packet_unref(self.pkt);
                }
            }
        }
    }

    /// Ask the decoder to output everything it has without reading more packets
    /// (used after feeding exactly the target packet of an intra file).
    pub fn frame_pts(&self) -> i64 {
        unsafe {
            let f = &*self.frame;
            if f.best_effort_timestamp != AV_NOPTS_VALUE {
                f.best_effort_timestamp
            } else {
                f.pts
            }
        }
    }

    /// Seek so that the next frames returned lead up to `ts` (stream time base):
    /// jumps to the keyframe at or before `ts` and flushes the decoder.
    pub fn seek_keyframe(&mut self, ts: i64) {
        unsafe {
            ck(
                av_seek_frame(self.fmt, self.stream, ts, AVSEEK_FLAG_BACKWARD),
                "seek",
            );
            avcodec_flush_buffers(self.ctx);
            self.draining = false;
            self.eof = false;
        }
    }

    /// Frame-accurate seek: returns number of frames decoded to reach `ts`
    /// (the frame with pts >= ts is left in `self.frame`).
    pub fn seek_exact(&mut self, ts: i64) -> u32 {
        let mut seek_ts = ts;
        let mut n = 0;
        loop {
            self.seek_keyframe(seek_ts);
            // optional: while rolling forward to the target, let the decoder drop
            // non-reference frames (their packets have pts < target)
            self.skip_nonref_before = if self.skip_nonref { Some(ts) } else { None };
            let mut first = true;
            let mut retry = false;
            while self.next_frame() {
                n += 1;
                let pts = self.frame_pts();
                if first && pts > ts {
                    // Open-GOP stream: the demuxer picked the keyframe by DTS and the
                    // target is a leading picture (RASL) of that keyframe, which the
                    // decoder drops after a seek. Start one keyframe earlier.
                    self.open_gop_retries += 1;
                    seek_ts = seek_ts.min(pts) - (pts - ts).max(1) * 4;
                    retry = true;
                    break;
                }
                first = false;
                if pts >= ts {
                    return n;
                }
            }
            if !retry {
                panic!("seek_exact: hit EOF before ts {ts}");
            }
        }
    }


    /// Block until the hw frame in `self.frame` is fully decoded on the GPU.
    /// VAAPI: vaSyncSurface. Vulkan: handled by caller (needs a VkDevice), see gpu.rs.
    pub fn sync_vaapi(&self) {
        unsafe {
            let f = &*self.frame;
            if f.format != AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
                return;
            }
            let fc = (*f.hw_frames_ctx).data as *mut AVHWFramesContext;
            let va = (*(*fc).device_ctx).hwctx as *mut AVVAAPIDeviceContext;
            let surf = f.data[3] as usize as u32;
            let r = vaSyncSurface((*va).display as *mut c_void, surf);
            assert!(r == 0, "vaSyncSurface failed: {r}");
        }
    }

    pub fn va_surface_id(&self) -> u32 {
        unsafe { (*self.frame).data[3] as usize as u32 }
    }

    /// Copy the hw frame in `self.frame` to CPU memory (`self.sw_frame`, NV12/P010).
    /// For software frames this just moves the reference.
    pub fn download(&mut self) -> *mut AVFrame {
        unsafe {
            av_frame_unref(self.sw_frame);
            if (*self.frame).hw_frames_ctx.is_null() {
                av_frame_ref(self.sw_frame, self.frame);
            } else {
                ck(
                    av_hwframe_transfer_data(self.sw_frame, self.frame, 0),
                    "hwframe transfer",
                );
                (*self.sw_frame).pts = (*self.frame).pts;
            }
            self.sw_frame
        }
    }

    pub fn is_hw_frame(&self) -> bool {
        unsafe { !(*self.frame).hw_frames_ctx.is_null() }
    }

    pub fn frame_format(&self) -> String {
        unsafe {
            let f = &*self.frame;
            if f.format < 0 {
                return "none".into();
            }
            let fmt: AVPixelFormat = std::mem::transmute(f.format);
            let mut s = CStr::from_ptr(av_get_pix_fmt_name(fmt)).to_string_lossy().into_owned();
            if !f.hw_frames_ctx.is_null() {
                let fc = (*f.hw_frames_ctx).data as *mut AVHWFramesContext;
                s += &format!(
                    "(sw_format={})",
                    CStr::from_ptr(av_get_pix_fmt_name((*fc).sw_format)).to_string_lossy()
                );
            }
            s
        }
    }

    pub fn sw_format(&self) -> AVPixelFormat {
        unsafe {
            let f = &*self.frame;
            if f.hw_frames_ctx.is_null() {
                std::mem::transmute(f.format)
            } else {
                (*((*f.hw_frames_ctx).data as *mut AVHWFramesContext)).sw_format
            }
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.frame);
            av_frame_free(&mut self.sw_frame);
            av_packet_free(&mut self.pkt);
            avcodec_free_context(&mut self.ctx);
            avformat_close_input(&mut self.fmt);
        }
    }
}

/// (user+sys) CPU seconds consumed by this process so far.
pub fn cpu_secs() -> f64 {
    unsafe {
        let mut ru: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut ru);
        let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
        tv(ru.ru_utime) + tv(ru.ru_stime)
    }
}

pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

impl Drop for HwDevice {
    fn drop(&mut self) {
        unsafe { av_buffer_unref(&mut self.0) }
    }
}

/// Block until the hw frame is completely decoded on the GPU.
/// VAAPI: vaSyncSurface. Vulkan: wait on the frame's timeline semaphore.
pub fn sync_frame(f: *mut AVFrame) {
    use ash::vk::{self, Handle};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static DEVS: OnceLock<Mutex<HashMap<usize, ash::Device>>> = OnceLock::new();
    unsafe {
        let f = &*f;
        if f.hw_frames_ctx.is_null() {
            return;
        }
        let fc = (*f.hw_frames_ctx).data as *mut AVHWFramesContext;
        if f.format == AVPixelFormat::AV_PIX_FMT_VAAPI as i32 {
            let va = (*(*fc).device_ctx).hwctx as *mut AVVAAPIDeviceContext;
            let r = vaSyncSurface((*va).display as *mut c_void, f.data[3] as usize as u32);
            assert!(r == 0, "vaSyncSurface failed: {r}");
        } else if f.format == AVPixelFormat::AV_PIX_FMT_VULKAN as i32 {
            let vkc = &*((*(*fc).device_ctx).hwctx as *const AVVulkanDeviceContext);
            let mut devs = DEVS.get_or_init(Default::default).lock().unwrap();
            let dev = devs.entry(vkc.act_dev as usize).or_insert_with(|| {
                let entry = ash::Entry::load().unwrap();
                let inst = ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(vkc.inst as u64));
                let dev = ash::Device::load(inst.fp_v1_0(), vk::Device::from_raw(vkc.act_dev as u64));
                std::mem::forget(entry);
                dev
            });
            let vfc = &*((*fc).hwctx as *const AVVulkanFramesContext);
            let vkf = f.data[0] as *mut AVVkFrame;
            (vfc.lock_frame.unwrap())(fc, vkf);
            let sems = [vk::Semaphore::from_raw((*vkf).sem[0])];
            let vals = [(*vkf).sem_value[0]];
            (vfc.unlock_frame.unwrap())(fc, vkf);
            dev.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&sems).values(&vals), u64::MAX)
                .expect("vkWaitSemaphores");
        }
    }
}

impl Decoder {
    pub fn sync(&self) {
        sync_frame(self.frame)
    }
}
