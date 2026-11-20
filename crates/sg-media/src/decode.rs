//! In-process video decoding with libavcodec, for playback and scrubbing.
//!
//! A [`VideoDecoder`] is addressed by frame index at the file's nominal
//! frame rate. Asking for the frame after the last one decodes forward;
//! anything else seeks to the keyframe before the target and rolls forward
//! (spike S1, gotcha 10). Frames come out as 8-bit RGBA in system memory.
//!
//! M3 decodes in software and uploads; the zero-copy VAAPI path measured in
//! S1 replaces this for full-resolution playback in M5.

use std::ffi::{CStr, CString, c_int};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use ffmpeg_next::ffi::*;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("{what}: {message}")]
    Ffmpeg { what: &'static str, message: String },
    #[error("the file has no video stream that can be decoded")]
    NoVideo,
    #[error("the file ended before the first frame")]
    Empty,
}

fn check(code: c_int, what: &'static str) -> Result<c_int, DecodeError> {
    if code >= 0 {
        return Ok(code);
    }
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: `buf` is a valid, writable buffer of the given length; av_strerror
    // always NUL-terminates it.
    let message = unsafe {
        av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    Err(DecodeError::Ffmpeg { what, message })
}

/// One decoded picture, 8-bit RGBA with straight alpha, top row first.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Shared, so that handing a frame on does not copy the picture.
    pub rgba: Arc<Vec<u8>>,
    /// Index of this frame at the decoder's frame rate.
    pub index: i64,
    /// Degrees the picture must be turned counter-clockwise for display
    /// (0, 90, 180 or 270), from the file's display matrix.
    pub rotation: i32,
    /// Set instead of `rgba` (which is then empty) after [`VideoDecoder::set_planes`].
    pub planes: Option<Arc<Planes>>,
}

/// An 8-bit 4:2:0 picture as decoded: rows tightly packed, top row first.
pub struct Planes {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub chroma_size: (u32, u32),
    pub matrix: Matrix,
    pub full_range: bool,
}

/// Which standard turns the picture's Y'CbCr into RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl Matrix {
    /// Luma weights of red and blue.
    pub fn kr_kb(self) -> (f32, f32) {
        match self {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
            Matrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

pub struct VideoDecoder {
    format: *mut AVFormatContext,
    codec: *mut AVCodecContext,
    packet: *mut AVPacket,
    /// The last decoded picture.
    frame: *mut AVFrame,
    scratch: *mut AVFrame,
    /// System-memory copy of a hardware frame.
    download: *mut AVFrame,
    /// `frame` holds a picture decoded since the last seek.
    holding: bool,
    scaler: *mut SwsContext,
    /// The RGBA picture the scaler writes into.
    converted: *mut AVFrame,
    /// (width, height, pixel format, colour space, range) the scaler was built for.
    scaler_for: (c_int, c_int, c_int, c_int, c_int),
    /// Longest side of the frames handed out; larger pictures are scaled down.
    max_dim: u32,
    planes: bool,
    rotation: i32,
    skip_nonref: bool,
    /// While set, packets before this timestamp only feed reference frames.
    skip_before: Option<i64>,
    stream: c_int,
    time_base: AVRational,
    start_pts: i64,
    /// Frames per second as a fraction.
    rate: (i64, i64),
    width: u32,
    height: u32,
    draining: bool,
    /// Index of the frame the next sequential decode produces, if known.
    next_index: Option<i64>,
    seeks: u64,
    /// The last frame handed out, repeated when asked past the end.
    last: Option<VideoFrame>,
}

// SAFETY: the FFmpeg contexts are owned by this value alone and are only
// touched through `&mut self`; libav objects may move between threads as
// long as they are not used concurrently.
unsafe impl Send for VideoDecoder {}

/// How far ahead of the current position a request is served by decoding
/// forward instead of seeking.
const ROLL_FORWARD: i64 = 24;

impl VideoDecoder {
    /// `threads`: 1 for all-intra proxies (frame threading makes their seeks
    /// 4 to 8 times slower, S1), 0 lets FFmpeg choose for long-GOP files.
    pub fn open(path: &Path, threads: i32) -> Result<VideoDecoder, DecodeError> {
        VideoDecoder::open_with(path, threads, false)
    }

    /// With `hardware`, decodes on the GPU through VAAPI where the codec
    /// allows it and reads back only the frames that are asked for: a seek
    /// into a long-GOP 4K file rolls forward at GPU speed. Sustained
    /// playback through this read-back is slow (S1); use it for single frames.
    pub fn open_with(path: &Path, threads: i32, hardware: bool) -> Result<VideoDecoder, DecodeError> {
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| DecodeError::Ffmpeg { what: "open", message: "path contains a NUL byte".into() })?;
        // SAFETY: plain libav open sequence. Every pointer passed is either
        // valid or null where the API allows null; on any error the partially
        // built decoder is dropped, which frees what was allocated.
        unsafe {
            let mut dec = VideoDecoder {
                format: null_mut(),
                codec: null_mut(),
                packet: null_mut(),
                frame: null_mut(),
                scratch: null_mut(),
                download: null_mut(),
                holding: false,
                scaler: null_mut(),
                converted: null_mut(),
                scaler_for: (0, 0, 0, 0, 0),
                max_dim: u32::MAX,
                planes: false,
                rotation: 0,
                skip_nonref: false,
                skip_before: None,
                stream: -1,
                time_base: AVRational { num: 1, den: 1 },
                start_pts: 0,
                rate: (25, 1),
                width: 0,
                height: 0,
                draining: false,
                next_index: None,
                seeks: 0,
                last: None,
            };
            check(avformat_open_input(&mut dec.format, c_path.as_ptr(), null(), null_mut()), "open")?;
            check(avformat_find_stream_info(dec.format, null_mut()), "read stream info")?;
            let mut codec: *const AVCodec = null();
            dec.stream = av_find_best_stream(dec.format, AVMediaType::AVMEDIA_TYPE_VIDEO, -1, -1, &mut codec, 0);
            if dec.stream < 0 || codec.is_null() {
                return Err(DecodeError::NoVideo);
            }
            let stream = *(*dec.format).streams.add(dec.stream as usize);
            dec.codec = avcodec_alloc_context3(codec);
            check(avcodec_parameters_to_context(dec.codec, (*stream).codecpar), "set up decoder")?;
            (*dec.codec).pkt_timebase = (*stream).time_base;
            let device = if hardware { vaapi_device() } else { null_mut() };
            if !device.is_null() {
                // One thread per hardware stream (S1, gotcha 2). If the codec
                // has no hardware decoder, get_format falls back to software.
                (*dec.codec).hw_device_ctx = av_buffer_ref(device);
                (*dec.codec).get_format = Some(pick_hardware_format);
                (*dec.codec).thread_count = 1;
            } else {
                (*dec.codec).thread_count = threads;
                if threads != 1 {
                    (*dec.codec).thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
                }
            }
            check(avcodec_open2(dec.codec, codec, null_mut()), "open decoder")?;
            dec.packet = av_packet_alloc();
            dec.frame = av_frame_alloc();
            dec.scratch = av_frame_alloc();
            dec.download = av_frame_alloc();
            dec.converted = av_frame_alloc();
            dec.time_base = (*stream).time_base;
            dec.start_pts = if (*stream).start_time == AV_NOPTS_VALUE { 0 } else { (*stream).start_time };
            let guessed = av_guess_frame_rate(dec.format, stream, null_mut());
            if guessed.num > 0 && guessed.den > 0 {
                dec.rate = (guessed.num as i64, guessed.den as i64);
            }
            let params = (*stream).codecpar;
            let matrix = av_packet_side_data_get(
                (*params).coded_side_data,
                (*params).nb_coded_side_data,
                AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
            );
            if !matrix.is_null() && (*matrix).size >= 36 && !(*matrix).data.is_null() {
                // The matrix is nine 16.16 fixed-point numbers; its first row
                // gives the angle. Same result as av_display_rotation_get.
                let m = (*matrix).data as *const i32;
                let angle = -(m.add(1).read_unaligned() as f64).atan2(m.read_unaligned() as f64).to_degrees();
                dec.rotation = ((angle / 90.0).round() as i32 * 90).rem_euclid(360);
            }
            dec.width = (*dec.codec).width.max(0) as u32;
            dec.height = (*dec.codec).height.max(0) as u32;
            if dec.width == 0 || dec.height == 0 || dec.time_base.num <= 0 || dec.time_base.den <= 0 {
                return Err(DecodeError::NoVideo);
            }
            Ok(dec)
        }
    }

    /// Overrides the frame rate used to turn indices into timestamps, with
    /// the (snapped) rate recorded in the asset index.
    pub fn set_rate(&mut self, num: u32, den: u32) {
        if num > 0 && den > 0 {
            self.rate = (num as i64, den as i64);
        }
    }

    /// Skips non-reference pictures on the way from a keyframe to a seek
    /// target. Worth it for long-GOP files; pointless for all-intra ones.
    pub fn set_skip_nonref(&mut self, skip: bool) {
        self.skip_nonref = skip;
    }

    /// Limits the longest side of decoded frames. Set before the first frame.
    pub fn set_max_dim(&mut self, max_dim: u32) {
        self.max_dim = max_dim;
    }

    /// Hands out 8-bit 4:2:0 pictures as their planes ([`VideoFrame::planes`])
    /// instead of converting them to RGBA, for a caller that converts on the
    /// GPU. Other formats still arrive as RGBA.
    pub fn set_planes(&mut self, planes: bool) {
        self.planes = planes;
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Ticks of the stream time base per frame, as a fraction.
    fn ticks_per_frame(&self) -> (i128, i128) {
        (self.time_base.den as i128 * self.rate.1 as i128, self.time_base.num as i128 * self.rate.0 as i128)
    }

    fn pts_of(&self, index: i64) -> i64 {
        let (num, den) = self.ticks_per_frame();
        self.start_pts + ((index as i128 * num + den / 2).div_euclid(den)) as i64
    }

    fn index_of(&self, pts: i64) -> i64 {
        let (num, den) = self.ticks_per_frame();
        (((pts - self.start_pts) as i128 * den + num / 2).div_euclid(num)) as i64
    }

    /// Decodes the next frame into `self.frame`. `Ok(false)` at the end.
    fn decode_next(&mut self) -> Result<bool, DecodeError> {
        // SAFETY: the contexts were opened in `open` and stay valid until drop;
        // the packet is unreferenced after every use.
        unsafe {
            loop {
                // Decode into the scratch frame and swap, so that `self.frame`
                // keeps the last good picture when the decoder reports the end.
                let got = avcodec_receive_frame(self.codec, self.scratch);
                if got == 0 {
                    std::mem::swap(&mut self.frame, &mut self.scratch);
                    self.holding = true;
                    return Ok(true);
                }
                if got == AVERROR_EOF || self.draining {
                    return Ok(false);
                }
                if got != AVERROR(libc::EAGAIN) {
                    check(got, "decode")?;
                }
                loop {
                    if av_read_frame(self.format, self.packet) < 0 {
                        self.draining = true;
                        avcodec_send_packet(self.codec, null());
                        break;
                    }
                    let ours = (*self.packet).stream_index == self.stream;
                    if ours {
                        let pts = (*self.packet).pts;
                        let skip = self.skip_before.is_some_and(|target| pts != AV_NOPTS_VALUE && pts < target);
                        (*self.codec).skip_frame =
                            if skip { AVDiscard::AVDISCARD_NONREF } else { AVDiscard::AVDISCARD_DEFAULT };
                    }
                    let sent = if ours { avcodec_send_packet(self.codec, self.packet) } else { 0 };
                    av_packet_unref(self.packet);
                    if ours {
                        // A damaged packet is skipped; the decoder recovers at the next keyframe.
                        if sent < 0 && sent != AVERROR(libc::EAGAIN) {
                            log::debug!("packet rejected by the decoder ({sent})");
                        }
                        break;
                    }
                }
            }
        }
    }

    fn current_pts(&self) -> i64 {
        // SAFETY: `self.frame` holds a decoded frame when this is called.
        unsafe {
            let f = &*self.frame;
            if f.best_effort_timestamp != AV_NOPTS_VALUE { f.best_effort_timestamp } else { f.pts }
        }
    }

    fn seek_to(&mut self, pts: i64) -> Result<(), DecodeError> {
        // SAFETY: valid contexts; flushing after a seek is the documented order.
        unsafe {
            check(av_seek_frame(self.format, self.stream, pts, AVSEEK_FLAG_BACKWARD), "seek")?;
            avcodec_flush_buffers(self.codec);
        }
        self.draining = false;
        self.holding = false;
        self.next_index = None;
        self.seeks += 1;
        Ok(())
    }

    /// Whether `index` comes next for this decoder, or a few frames on:
    /// reading it needs no seek.
    pub fn continues_at(&self, index: i64) -> bool {
        self.next_index.is_some_and(|next| index >= next && index - next <= ROLL_FORWARD)
            || self.last.as_ref().is_some_and(|last| last.index == index)
    }

    /// How often the file was repositioned so far. Playing or exporting
    /// straight through a file must not add to it (tests, benchmarks).
    pub fn seeks(&self) -> u64 {
        self.seeks
    }

    /// The frame at `index`. Past the end, the last frame of the file.
    pub fn frame(&mut self, index: i64) -> Result<VideoFrame, DecodeError> {
        let index = index.max(0);
        if let Some(last) = &self.last
            && last.index == index
        {
            return Ok(last.clone());
        }
        let sequential = self.continues_at(index);
        let target = self.pts_of(index);
        let mut seek_pts = target;
        let mut attempts = 0;
        if sequential {
            self.skip_before = None;
        } else {
            // While rolling forward from the keyframe, pictures nothing else
            // depends on need not be decoded (S1, gotcha 10).
            self.skip_before = self.skip_nonref.then_some(target);
            self.seek_to(seek_pts)?;
        }
        loop {
            let mut first_after_seek = !sequential || attempts > 0;
            let mut reached = false;
            while self.decode_next()? {
                let pts = self.current_pts();
                let at = self.index_of(pts);
                // Open GOP: the first frame after the seek is already past the
                // target because its leading pictures were dropped. Go one
                // keyframe further back (S1, failure 5).
                if first_after_seek && at > index && attempts < 4 && seek_pts > self.start_pts {
                    attempts += 1;
                    seek_pts = (seek_pts.min(pts) - (pts - target).max(1) * 4).max(self.start_pts);
                    self.seek_to(seek_pts)?;
                    first_after_seek = true;
                    reached = false;
                    continue;
                }
                first_after_seek = false;
                self.next_index = Some(at + 1);
                if at >= index {
                    reached = true;
                    break;
                }
            }
            if reached {
                let at = self.index_of(self.current_pts());
                let frame = self.convert(at.max(index).min(at))?;
                self.last = Some(frame.clone());
                return Ok(frame);
            }
            // End of file before the target: hold the last picture of the file,
            // which is still in `self.frame`.
            if self.holding {
                let frame = self.convert(index)?;
                self.last = Some(frame.clone());
                return Ok(frame);
            }
            if attempts >= 4 || seek_pts <= self.start_pts {
                return Err(DecodeError::Empty);
            }
            // Nothing decodable from this seek point: try from the very start.
            attempts = 4;
            seek_pts = self.start_pts;
            self.seek_to(seek_pts)?;
        }
    }

    /// Converts the picture in `self.frame` to RGBA.
    fn convert(&mut self, index: i64) -> Result<VideoFrame, DecodeError> {
        // SAFETY: `self.frame` holds a decoded frame. A hardware frame is first
        // copied into `self.download`, which then owns system-memory planes.
        // The scaler is created for exactly the source geometry and format,
        // and the destination buffer is `stride * height` bytes.
        unsafe {
            let mut source = self.frame;
            if (*source).format == AVPixelFormat::AV_PIX_FMT_VAAPI as c_int {
                av_frame_unref(self.download);
                check(av_hwframe_transfer_data(self.download, source, 0), "read back the decoded frame")?;
                av_frame_copy_props(self.download, source);
                source = self.download;
            }
            let f = &*source;
            let (w, h) = (f.width, f.height);
            if w <= 0 || h <= 0 || f.data[0].is_null() {
                return Err(DecodeError::Empty);
            }
            // Output size: the picture itself, or scaled down to fit the cap.
            let cap = self.max_dim.max(16) as i64;
            let longest = w.max(h) as i64;
            let (ow, oh) = if longest > cap {
                (((w as i64 * cap / longest).max(1)) as c_int, ((h as i64 * cap / longest).max(1)) as c_int)
            } else {
                (w, h)
            };
            let planar = f.format == AVPixelFormat::AV_PIX_FMT_YUV420P as c_int
                || f.format == AVPixelFormat::AV_PIX_FMT_YUVJ420P as c_int;
            if self.planes && planar && (ow, oh) == (w, h) && !f.data[1].is_null() && !f.data[2].is_null() {
                let (cw, ch) = ((w + 1) / 2, (h + 1) / 2);
                // Rows are copied without the padding libavcodec keeps after each.
                let copy = |plane: usize, pw: c_int, ph: c_int| {
                    let mut out = Vec::with_capacity(pw as usize * ph as usize);
                    for row in 0..ph as isize {
                        let line = f.data[plane].offset(row * f.linesize[plane] as isize);
                        out.extend_from_slice(std::slice::from_raw_parts(line, pw as usize));
                    }
                    out
                };
                // Untagged video: BT.709 for HD and up, BT.601 below, as players do.
                let matrix = match f.colorspace {
                    AVColorSpace::AVCOL_SPC_BT470BG | AVColorSpace::AVCOL_SPC_SMPTE170M => Matrix::Bt601,
                    AVColorSpace::AVCOL_SPC_BT2020_NCL | AVColorSpace::AVCOL_SPC_BT2020_CL => Matrix::Bt2020,
                    AVColorSpace::AVCOL_SPC_BT709 => Matrix::Bt709,
                    _ if h >= 720 => Matrix::Bt709,
                    _ => Matrix::Bt601,
                };
                let planes = Planes {
                    y: copy(0, w, h),
                    u: copy(1, cw, ch),
                    v: copy(2, cw, ch),
                    chroma_size: (cw as u32, ch as u32),
                    matrix,
                    full_range: f.color_range == AVColorRange::AVCOL_RANGE_JPEG
                        || f.format == AVPixelFormat::AV_PIX_FMT_YUVJ420P as c_int,
                };
                return Ok(VideoFrame {
                    width: w as u32,
                    height: h as u32,
                    rgba: Arc::new(Vec::new()),
                    planes: Some(Arc::new(planes)),
                    index,
                    rotation: self.rotation,
                });
            }
            let key = (w, h, f.format, f.colorspace as c_int, f.color_range as c_int);
            if self.scaler.is_null() || self.scaler_for != key {
                sws_freeContext(self.scaler);
                let src: AVPixelFormat = std::mem::transmute::<c_int, AVPixelFormat>(f.format);
                self.scaler = sws_alloc_context();
                if self.scaler.is_null() {
                    return Err(DecodeError::Ffmpeg { what: "convert", message: "out of memory".into() });
                }
                {
                    let s = &mut *self.scaler;
                    (s.src_w, s.src_h, s.src_format) = (w, h, src as c_int);
                    (s.dst_w, s.dst_h, s.dst_format) = (ow, oh, AVPixelFormat::AV_PIX_FMT_RGBA as c_int);
                    s.flags = SwsFlags::SWS_BILINEAR as _;
                    // Converting a 4K picture on one core takes longer than
                    // decoding it; small pictures are not worth the hand-over.
                    s.threads = if w as i64 * h as i64 >= 1920 * 1080 { 8 } else { 1 };
                }
                if sws_init_context(self.scaler, null_mut(), null_mut()) < 0 {
                    sws_freeContext(self.scaler);
                    self.scaler = null_mut();
                    return Err(DecodeError::Ffmpeg { what: "convert", message: "unsupported pixel format".into() });
                }
                av_frame_unref(self.converted);
                (*self.converted).width = ow;
                (*self.converted).height = oh;
                (*self.converted).format = AVPixelFormat::AV_PIX_FMT_RGBA as c_int;
                // Untagged video: BT.709 for HD and up, BT.601 below, as players do.
                let matrix = match f.colorspace {
                    AVColorSpace::AVCOL_SPC_BT470BG | AVColorSpace::AVCOL_SPC_SMPTE170M => SWS_CS_ITU601,
                    AVColorSpace::AVCOL_SPC_BT2020_NCL | AVColorSpace::AVCOL_SPC_BT2020_CL => SWS_CS_BT2020,
                    AVColorSpace::AVCOL_SPC_BT709 => SWS_CS_ITU709,
                    _ if h >= 720 => SWS_CS_ITU709,
                    _ => SWS_CS_ITU601,
                };
                let full = (f.color_range == AVColorRange::AVCOL_RANGE_JPEG) as c_int;
                let coefficients = sws_getCoefficients(matrix);
                sws_setColorspaceDetails(self.scaler, coefficients, full, coefficients, 1, 0, 1 << 16, 1 << 16);
                self.scaler_for = key;
            }
            let stride = ow as usize * 4;
            // The frame-based call is the one that uses the scaler's threads.
            // `converted` keeps its buffer from one picture to the next.
            check(sws_scale_frame(self.scaler, self.converted, source), "convert")?;
            let out = &*self.converted;
            if out.data[0].is_null() || (out.linesize[0] as usize) < stride {
                return Err(DecodeError::Empty);
            }
            let mut rgba = Vec::with_capacity(stride * oh as usize);
            for row in 0..oh as usize {
                let line = std::slice::from_raw_parts(out.data[0].add(row * out.linesize[0] as usize), stride);
                rgba.extend_from_slice(line);
            }
            Ok(VideoFrame {
                width: ow as u32,
                height: oh as u32,
                rgba: Arc::new(rgba),
                planes: None,
                index,
                rotation: self.rotation,
            })
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        // SAFETY: each pointer is either null or owned by this decoder; the
        // free functions accept null and reset the pointers.
        unsafe {
            sws_freeContext(self.scaler);
            av_frame_free(&mut self.frame);
            av_frame_free(&mut self.scratch);
            av_frame_free(&mut self.download);
            av_frame_free(&mut self.converted);
            av_packet_free(&mut self.packet);
            avcodec_free_context(&mut self.codec);
            avformat_close_input(&mut self.format);
        }
    }
}

/// The process-wide VAAPI device, or null if there is none.
fn vaapi_device() -> *mut AVBufferRef {
    struct Device(*mut AVBufferRef);
    // SAFETY: an AVBufferRef to a hardware device context is reference
    // counted and FFmpeg allows `av_buffer_ref` on it from any thread.
    unsafe impl Send for Device {}
    // SAFETY: as above; the pointer itself is never mutated after creation.
    unsafe impl Sync for Device {}
    static DEVICE: std::sync::OnceLock<Device> = std::sync::OnceLock::new();
    DEVICE
        .get_or_init(|| {
            let mut device = null_mut();
            // SAFETY: plain creation call; on failure `device` stays null.
            let code = unsafe {
                av_hwdevice_ctx_create(
                    &mut device,
                    AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                    c"/dev/dri/renderD128".as_ptr(),
                    null_mut(),
                    0,
                )
            };
            if code < 0 {
                log::warn!("no VAAPI device: full-quality frames are decoded in software");
                device = null_mut();
            }
            Device(device)
        })
        .0
}

/// libavcodec asks which of the offered formats to decode into.
unsafe extern "C" fn pick_hardware_format(_codec: *mut AVCodecContext, formats: *const AVPixelFormat) -> AVPixelFormat {
    // SAFETY: `formats` is a list terminated by AV_PIX_FMT_NONE, as the
    // callback contract says.
    unsafe {
        let mut p = formats;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p == AVPixelFormat::AV_PIX_FMT_VAAPI {
                return *p;
            }
            p = p.add(1);
        }
        // Not offered: the first entry is the software format.
        *formats
    }
}
