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
    pub rgba: Vec<u8>,
    /// Index of this frame at the decoder's frame rate.
    pub index: i64,
}

pub struct VideoDecoder {
    format: *mut AVFormatContext,
    codec: *mut AVCodecContext,
    packet: *mut AVPacket,
    /// The last decoded picture.
    frame: *mut AVFrame,
    scratch: *mut AVFrame,
    /// `frame` holds a picture decoded since the last seek.
    holding: bool,
    scaler: *mut SwsContext,
    /// (width, height, pixel format, colour space, range) the scaler was built for.
    scaler_for: (c_int, c_int, c_int, c_int, c_int),
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
                holding: false,
                scaler: null_mut(),
                scaler_for: (0, 0, 0, 0, 0),
                stream: -1,
                time_base: AVRational { num: 1, den: 1 },
                start_pts: 0,
                rate: (25, 1),
                width: 0,
                height: 0,
                draining: false,
                next_index: None,
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
            (*dec.codec).thread_count = threads;
            if threads != 1 {
                (*dec.codec).thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
            }
            check(avcodec_open2(dec.codec, codec, null_mut()), "open decoder")?;
            dec.packet = av_packet_alloc();
            dec.frame = av_frame_alloc();
            dec.scratch = av_frame_alloc();
            dec.time_base = (*stream).time_base;
            dec.start_pts = if (*stream).start_time == AV_NOPTS_VALUE { 0 } else { (*stream).start_time };
            let guessed = av_guess_frame_rate(dec.format, stream, null_mut());
            if guessed.num > 0 && guessed.den > 0 {
                dec.rate = (guessed.num as i64, guessed.den as i64);
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
        Ok(())
    }

    /// The frame at `index`. Past the end, the last frame of the file.
    pub fn frame(&mut self, index: i64) -> Result<VideoFrame, DecodeError> {
        let index = index.max(0);
        if let Some(last) = &self.last
            && last.index == index
        {
            return Ok(last.clone());
        }
        let sequential = self.next_index.is_some_and(|next| index >= next && index - next <= ROLL_FORWARD);
        let target = self.pts_of(index);
        let mut seek_pts = target;
        let mut attempts = 0;
        if !sequential {
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
        // SAFETY: `self.frame` holds a decoded software frame; the scaler is
        // created for exactly its geometry and format, and the destination
        // buffer is `stride * height` bytes.
        unsafe {
            let f = &*self.frame;
            let (w, h) = (f.width, f.height);
            if w <= 0 || h <= 0 || f.data[0].is_null() {
                return Err(DecodeError::Empty);
            }
            let key = (w, h, f.format, f.colorspace as c_int, f.color_range as c_int);
            if self.scaler.is_null() || self.scaler_for != key {
                sws_freeContext(self.scaler);
                let src: AVPixelFormat = std::mem::transmute::<c_int, AVPixelFormat>(f.format);
                self.scaler = sws_getContext(
                    w,
                    h,
                    src,
                    w,
                    h,
                    AVPixelFormat::AV_PIX_FMT_RGBA,
                    SwsFlags::SWS_BILINEAR as c_int,
                    null_mut(),
                    null_mut(),
                    null(),
                );
                if self.scaler.is_null() {
                    return Err(DecodeError::Ffmpeg { what: "convert", message: "unsupported pixel format".into() });
                }
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
            let stride = w as usize * 4;
            let mut rgba = vec![0u8; stride * h as usize];
            let dst = [rgba.as_mut_ptr(), null_mut(), null_mut(), null_mut()];
            let dst_stride = [stride as c_int, 0, 0, 0];
            sws_scale(
                self.scaler,
                f.data.as_ptr() as *const *const u8,
                f.linesize.as_ptr(),
                0,
                h,
                dst.as_ptr(),
                dst_stride.as_ptr(),
            );
            Ok(VideoFrame { width: w as u32, height: h as u32, rgba, index })
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
            av_packet_free(&mut self.packet);
            avcodec_free_context(&mut self.codec);
            avformat_close_input(&mut self.format);
        }
    }
}
