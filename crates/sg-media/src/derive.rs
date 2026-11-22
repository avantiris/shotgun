//! Files derived from an asset at import. Command lines are the ones measured
//! in spike S4 (docs/spikes/s4-io.md, D3 and D4); change them there first.
//!
//! Every function writes to a temporary name next to the target and renames
//! on success, so a half-written file is never mistaken for a finished one.

use std::path::{Path, PathBuf};
use std::process::Command;

use sg_core::{Rate, VideoInfo};

use crate::tool::{Cancel, ToolError, run_lines};

pub const POSTER_W: u32 = 320;
pub const POSTER_H: u32 = 180;
const VAAPI_DEVICE: &str = "/dev/dri/renderD128";

/// Called with 0..=1 while a transcode runs.
pub type Progress<'a> = &'a mut dyn FnMut(f32);

fn temp_for(out: &Path) -> PathBuf {
    let name = out.file_name().unwrap_or_default().to_string_lossy();
    out.with_file_name(format!(".tmp-{name}"))
}

/// Runs ffmpeg writing to a temporary file, then renames it to `out`.
/// `args` builds everything between `ffmpeg` and the output path.
fn ffmpeg_to(
    out: &Path,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
    args: impl FnOnce(&mut Command),
) -> Result<(), ToolError> {
    let tmp = temp_for(out);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-y", "-progress", "pipe:1", "-nostats"]);
    args(&mut cmd);
    cmd.arg(&tmp);
    let result = run_lines(cmd, cancel, |line| {
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok())
            && total_secs > 0.0
        {
            progress((us / 1e6 / total_secs).clamp(0.0, 1.0) as f32);
        }
    });
    match result {
        Ok(()) => std::fs::rename(&tmp, out).map_err(ToolError::from),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn vaapi_input(cmd: &mut Command, src: &Path) {
    cmd.args(["-hwaccel", "vaapi", "-hwaccel_device", VAAPI_DEVICE, "-hwaccel_output_format", "vaapi", "-i"]).arg(src);
}

/// One frame as raw RGBA, [`POSTER_W`] x [`POSTER_H`], letterboxed.
pub fn poster(src: &Path, out: &Path, at_secs: f64, cancel: &Cancel) -> Result<(), ToolError> {
    let filter = format!(
        "scale={POSTER_W}:{POSTER_H}:force_original_aspect_ratio=decrease,\
         pad={POSTER_W}:{POSTER_H}:(ow-iw)/2:(oh-ih)/2:color=black,format=rgba"
    );
    ffmpeg_to(out, 0.0, cancel, &mut |_| {}, |cmd| {
        cmd.args(["-ss", &format!("{at_secs:.3}"), "-i"]).arg(src).args([
            "-frames:v",
            "1",
            "-vf",
            &filter,
            "-f",
            "rawvideo",
        ]);
    })
}

/// A waveform picture in the poster format, for audio-only assets.
pub fn wave_poster(wav: &Path, out: &Path, cancel: &Cancel) -> Result<(), ToolError> {
    let filter = format!("showwavespic=s={POSTER_W}x{POSTER_H}:colors=0x57B894,format=rgba");
    ffmpeg_to(out, 0.0, cancel, &mut |_| {}, |cmd| {
        cmd.arg("-i").arg(wav).args(["-filter_complex", &filter, "-frames:v", "1", "-f", "rawvideo"]);
    })
}

/// The first audio stream as 48 kHz 32-bit float WAV, mono kept mono,
/// everything else mixed to stereo. The timeline only ever reads this file.
pub fn audio_wav(
    src: &Path,
    out: &Path,
    channels: u16,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    let ac = if channels == 1 { "1" } else { "2" };
    ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        cmd.arg("-i").arg(src).args([
            "-vn",
            "-map",
            "0:a:0",
            "-ac",
            ac,
            "-ar",
            "48000",
            "-c:a",
            "pcm_f32le",
            "-rf64",
            "auto",
        ]);
    })
}

/// Constant-frame-rate mezzanine of a VFR or otherwise awkward video:
/// HEVC on the GPU at CQP 14, short GOP. Falls back to x264 in software if
/// the GPU path fails (codec the GPU cannot decode, no VAAPI).
pub fn conform_video(
    src: &Path,
    out: &Path,
    info: &VideoInfo,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    let fps = format!("fps={}", info.rate);
    let tags = |cmd: &mut Command| {
        for (flag, value) in [
            ("-color_primaries", &info.color_primaries),
            ("-color_trc", &info.color_transfer),
            ("-colorspace", &info.color_space),
            ("-color_range", &info.color_range),
        ] {
            if let Some(v) = value.as_deref().filter(|v| *v != "unknown") {
                cmd.args([flag, v]);
            }
        }
    };
    let hardware = ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        vaapi_input(cmd, src);
        cmd.arg("-an");
        if info.bit_depth > 8 {
            cmd.args(["-vf", &format!("{fps},scale_vaapi=format=p010"), "-c:v", "hevc_vaapi", "-profile:v", "main10"]);
        } else {
            cmd.args(["-vf", &fps, "-c:v", "hevc_vaapi"]);
        }
        // Never add -tag:v hvc1 here: the output is undecodable on this GPU (S4, 5.3).
        cmd.args(["-rc_mode", "CQP", "-qp", "14", "-g", "15", "-bf", "0"]);
        tags(cmd);
    });
    match hardware {
        Err(ToolError::Failed { stderr, .. }) => {
            log::warn!("GPU conform of {} failed, using software: {stderr}", src.display());
            ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
                cmd.arg("-i").arg(src).args(["-an", "-vf", &fps]);
                cmd.args([
                    "-c:v", "libx264", "-preset", "veryfast", "-crf", "14", "-g", "15", "-bf", "0", "-pix_fmt",
                    "yuv420p",
                ]);
                tags(cmd);
            })
        }
        other => other,
    }
}

/// Lossless RGBA mezzanine for animated pictures with transparency.
pub fn conform_alpha(
    src: &Path,
    out: &Path,
    rate: Rate,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        // (ffmpeg's own VP8 and VP9 decoders leave the transparency of a
        // WebM out; libvpx reads it.)
        match src.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
            Some("webm") if webm_codec(src, cancel).as_deref() == Some("vp8") => cmd.args(["-c:v", "libvpx"]),
            Some("webm") => cmd.args(["-c:v", "libvpx-vp9"]),
            _ => cmd,
        };
        cmd.arg("-i").arg(src).args(["-an", "-vf", &format!("fps={rate}"), "-c:v", "utvideo", "-pix_fmt", "gbrap"]);
    })
}

/// The codec of a file's first video stream, as ffprobe names it.
fn webm_codec(src: &Path, cancel: &Cancel) -> Option<String> {
    let mut probe = Command::new("ffprobe");
    probe.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]);
    probe.arg(src);
    crate::tool::run_output(probe, cancel).ok().map(|name| name.trim().to_owned())
}

/// A copy of a video out of focus (a wide Gaussian blur), without sound:
/// what goes behind a cut-out of the same video to make the person stand
/// out, as a lens with a wide aperture would.
pub fn blurred(
    src: &Path,
    out: &Path,
    height: u32,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    // (As wide as a fiftieth of the picture's height, twice over: soft, with no boxes in it.)
    let filter = format!("gblur=sigma={:.1}:steps=2", (height as f32 / 50.0).max(2.0));
    ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        cmd.arg("-i").arg(src).args(["-an", "-vf", &filter]);
        cmd.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-pix_fmt", "yuv420p"]);
    })
}

/// A copy of a file's sound at another pitch, another tempo or both
/// (ffmpeg's rubberband filter: the one changes without the other), as
/// FLAC. `pitch`: the ratio of the frequencies (2 is an octave up);
/// `tempo`: how many times as fast.
pub fn stretch(
    src: &Path,
    out: &Path,
    pitch: f64,
    tempo: f64,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    let filter = format!("rubberband=pitch={pitch:.6}:tempo={tempo:.6}:pitchq=quality");
    ffmpeg_to(out, total_secs / tempo.max(0.01), cancel, progress, |cmd| {
        cmd.arg("-i").arg(src).args(["-vn", "-af", &filter, "-ar", "48000", "-c:a", "flac"]);
    })
}

/// Cuts the person out of a video (`sg-matte`, Robust Video Matting on
/// the CPU): the picture with everything else transparent, as a WebM.
/// `frames`: how many the source has, for the progress.
pub fn cut_out(
    model: &Path,
    src: &Path,
    out: &Path,
    frames: i64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    let tmp = out.with_file_name(format!(".tmp-{}", out.file_name().unwrap_or_default().to_string_lossy()));
    let mut cmd = Command::new("sg-matte");
    // Half of the machine: the editor stays usable while it runs.
    let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(2));
    cmd.arg(model).arg(src).arg(&tmp).arg(threads.to_string());
    let result = run_lines(cmd, cancel, |line| {
        if let Some(done) = line.strip_prefix("frame ").and_then(|n| n.trim().parse::<f32>().ok())
            && frames > 0
        {
            progress((done / frames as f32).clamp(0.0, 1.0));
        }
    });
    match result {
        Ok(()) => std::fs::rename(&tmp, out).map_err(ToolError::from),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// A part of a video with its sky transparent (`sg-sky`: a model marks
/// the sky; docs/spikes/s9-sky.md). `frames` from `from_secs` on.
pub fn sky(
    model: &Path,
    src: &Path,
    out: &Path,
    from_secs: f64,
    frames: i64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    let name = out.file_name().unwrap_or_default().to_string_lossy();
    let tmp = out.with_file_name(format!(".tmp-{name}"));
    let mut cmd = Command::new("sg-sky");
    // Half of the machine: the editor stays usable while it runs.
    let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(2));
    cmd.arg(model).arg(src).arg(&tmp);
    cmd.arg(format!("{from_secs:.6}")).arg(frames.to_string()).arg(threads.to_string());
    let result = run_lines(cmd, cancel, |line| {
        if let Some(done) = line.strip_prefix("frame ").and_then(|n| n.trim().parse::<f32>().ok())
            && frames > 0
        {
            progress((done / frames as f32).clamp(0.0, 1.0));
        }
    });
    match result {
        Ok(()) => std::fs::rename(&tmp, out).map_err(ToolError::from),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// A part of a video as a file of its own, to be handed to a model
/// (sg-project `ai`): `frames` from `from_secs` on, without sound, upright,
/// at most 1920 points on its longer side, as H.264 that anything reads.
pub fn part(
    src: &Path,
    out: &Path,
    from_secs: f64,
    frames: i64,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        if from_secs > 0.0 {
            cmd.args(["-ss", &format!("{from_secs:.6}")]);
        }
        cmd.arg("-i").arg(src).args(["-an", "-frames:v", &frames.to_string()]);
        cmd.args([
            "-vf",
            "scale='min(1920,iw)':'min(1920,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1",
        ]);
        cmd.args(["-c:v", "libx264", "-preset", "fast", "-crf", "17", "-pix_fmt", "yuv420p"]);
        cmd.args(["-movflags", "+faststart", "-f", "mp4"]);
    })
}

/// What a model gave back for a part, made to stand in for it: the
/// part's size, frame rate and exactly its `frames` (a shorter answer
/// holds its last picture), cut and scrubbed like any clip.
#[allow(clippy::too_many_arguments)]
pub fn fit(
    src: &Path,
    out: &Path,
    width: u32,
    height: u32,
    rate: Rate,
    frames: i64,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    let filter = format!(
        "fps={}/{},scale={width}:{height}:flags=lanczos,setsar=1,tpad=stop_mode=clone:stop=-1",
        rate.num(),
        rate.den()
    );
    // (A key frame every half second.)
    let key_every = (rate.num() as f64 / rate.den() as f64 / 2.0).round().max(1.0);
    ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        cmd.arg("-i").arg(src).args(["-an", "-vf", &filter, "-frames:v", &frames.to_string()]);
        cmd.args(["-c:v", "libx264", "-preset", "fast", "-crf", "15", "-pix_fmt", "yuv420p"]);
        cmd.args(["-g", &format!("{key_every}")]);
        cmd.args(["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709"]);
        cmd.args(["-movflags", "+faststart", "-f", "mp4"]);
    })
}

/// 540p all-intra H.264 proxy for scrubbing.
pub fn proxy(
    src: &Path,
    out: &Path,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    const ENCODE: [&str; 14] = [
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-tune",
        "fastdecode",
        "-crf",
        "20",
        "-g",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-movflags",
        "+faststart",
    ];
    let hardware = ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
        vaapi_input(cmd, src);
        cmd.args(["-an", "-vf", "scale_vaapi=w=-2:h=540,hwdownload,format=nv12"]).args(ENCODE);
    });
    match hardware {
        Err(ToolError::Failed { stderr, .. }) => {
            log::warn!("GPU-decoded proxy of {} failed, using software decode: {stderr}", src.display());
            ffmpeg_to(out, total_secs, cancel, progress, |cmd| {
                cmd.arg("-i").arg(src).args(["-an", "-vf", "scale=-2:540"]).args(ENCODE);
            })
        }
        other => other,
    }
}

/// A copy of `src` with the camera shake taken out (vid.stab, two passes:
/// measure the motion, then move each frame against it and zoom in just
/// enough to hide the borders). Same frame count and rate as the source;
/// a short GOP so that it seeks like a conform. `motion` is a scratch
/// file for the measured motion.
pub fn stabilise(
    src: &Path,
    out: &Path,
    motion: &Path,
    rate: &str,
    total_secs: f64,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ToolError> {
    // The filter takes the file name inside a filter string: keep it plain.
    let name = motion.to_string_lossy();
    if name.contains([':', '\'', ',', '\\', ' ']) {
        return Err(ToolError::other("the project path contains a character the stabiliser cannot take"));
    }
    let mut detect = Command::new("ffmpeg");
    detect.args(["-v", "error", "-nostdin", "-y", "-progress", "pipe:1", "-nostats", "-i"]).arg(src);
    detect.args(["-an", "-vf", &format!("vidstabdetect=shakiness=6:accuracy=12:result={name}"), "-f", "null", "-"]);
    run_lines(detect, cancel, |line| {
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.parse::<f64>().ok())
            && total_secs > 0.0
        {
            // The first pass is the first half of the work.
            progress((us / 1e6 / total_secs).clamp(0.0, 1.0) as f32 * 0.5);
        }
    })
    .inspect_err(|_| {
        // A cancelled or failed first pass leaves half a motion file behind.
        let _ = std::fs::remove_file(motion);
    })?;
    let filter =
        format!("fps={rate},vidstabtransform=input={name}:smoothing=20:optzoom=1:interpol=bicubic,format=yuv420p");
    let result = ffmpeg_to(out, total_secs, cancel, &mut |f| progress(0.5 + f * 0.5), |cmd| {
        cmd.arg("-i").arg(src).args(["-an", "-vf", &filter]);
        cmd.args([
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-crf",
            "14",
            "-g",
            "15",
            "-bf",
            "0",
            "-movflags",
            "+faststart",
        ]);
    });
    let _ = std::fs::remove_file(motion);
    result
}
