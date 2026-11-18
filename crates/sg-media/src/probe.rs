//! Reading what a media file contains (spike S4, D2).

use std::path::Path;
use std::process::Command;

use serde::Deserialize;
use sg_core::{AssetKind, AudioInfo, Rate, VideoInfo};

use crate::tool::{Cancel, ToolError, run_output};

#[derive(Clone, Debug, PartialEq)]
pub struct Probed {
    pub kind: AssetKind,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
    /// Title tag of the container, if any: a better label than a file name.
    pub title: Option<String>,
    /// Animated picture with transparency (GIF, animated WebP, APNG): takes
    /// the alpha conform path.
    pub animated_alpha: bool,
}

pub fn probe(path: &Path, cancel: &Cancel) -> Result<Probed, ToolError> {
    let mut cmd = Command::new("ffprobe");
    cmd.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"]).arg(path);
    parse(&run_output(cmd, cancel)?)
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: RawFormat,
}

#[derive(Deserialize)]
struct RawFormat {
    #[serde(default)]
    format_name: String,
    duration: Option<String>,
    #[serde(default)]
    tags: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize, Default)]
struct RawStream {
    #[serde(default)]
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    nb_frames: Option<String>,
    duration: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u16>,
    color_range: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    #[serde(default)]
    disposition: std::collections::BTreeMap<String, i32>,
    #[serde(default)]
    side_data_list: Vec<RawSideData>,
}

#[derive(Deserialize)]
struct RawSideData {
    rotation: Option<f64>,
}

/// Common rates, as (numerator, denominator). A measured rate within 0.1 %
/// of one of these is that rate.
const COMMON_RATES: [(u32, u32); 12] = [
    (24_000, 1001),
    (24, 1),
    (25, 1),
    (30_000, 1001),
    (30, 1),
    (48, 1),
    (50, 1),
    (60_000, 1001),
    (60, 1),
    (100, 1),
    (120_000, 1001),
    (120, 1),
];

/// Snaps a rate to a common one when within 0.1 %, else keeps it exact.
pub fn snap_rate(num: u64, den: u64) -> Option<Rate> {
    if num == 0 || den == 0 {
        return None;
    }
    // The nearest common rate, if it is within 0.1 %: 30 and 29.97 are exactly
    // 0.1 % apart, so "first within tolerance" would confuse them.
    let error = |(n, d): (u32, u32)| {
        (num as u128 * d as u128).abs_diff(n as u128 * den as u128) as f64 / (n as u128 * den as u128) as f64
    };
    let nearest = COMMON_RATES.into_iter().min_by(|a, b| error(*a).total_cmp(&error(*b)));
    if let Some((n, d)) = nearest.filter(|c| error(*c) < 0.001) {
        return Rate::new(n, d).ok();
    }
    // Reduce large fractions (e.g. 19001/317 stays, 34080000/901901 stays) into u32 range.
    let scale = (num.max(den) / u32::MAX as u64) + 1;
    Rate::new((num / scale) as u32, (den / scale).max(1) as u32).ok()
}

fn fraction(text: &str) -> Option<(u64, u64)> {
    let (n, d) = text.split_once('/')?;
    Some((n.parse().ok()?, d.parse().ok()?))
}

fn seconds(text: Option<&String>) -> Option<f64> {
    text?.parse().ok().filter(|s: &f64| s.is_finite() && *s >= 0.0)
}

fn bit_depth(pix_fmt: &str) -> u8 {
    // e.g. yuv420p10le, p010le, gbrp12le, rgb48le
    for (needle, depth) in [("p16", 16), ("48", 16), ("64", 16), ("p14", 14), ("p12", 12), ("p10", 10), ("p010", 10)] {
        if pix_fmt.contains(needle) {
            return depth;
        }
    }
    8
}

fn has_alpha(pix_fmt: &str) -> bool {
    ["yuva", "rgba", "bgra", "argb", "abgr", "gbrap", "ya8", "ya16", "pal8"].iter().any(|p| pix_fmt.starts_with(p))
}

pub fn parse(json: &str) -> Result<Probed, ToolError> {
    let raw: Raw =
        serde_json::from_str(json).map_err(|e| ToolError::other(format!("unreadable ffprobe output: {e}")))?;
    let container_secs = seconds(raw.format.duration.as_ref());
    // Cover art in an audio file is not a video track.
    let picture = raw
        .streams
        .iter()
        .find(|s| s.codec_type == "video" && s.disposition.get("attached_pic").copied().unwrap_or(0) == 0);
    let sound = raw.streams.iter().find(|s| s.codec_type == "audio");

    let format = raw.format.format_name.as_str();
    let image_container = ["image2", "gif", "apng"].contains(&format) || format.ends_with("_pipe");
    let mut animated_alpha = false;

    let video = match picture {
        None => None,
        Some(s) => {
            let (width, height) = match (s.width, s.height) {
                (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
                _ => return Err(ToolError::other("video stream without a size")),
            };
            let pix_fmt = s.pix_fmt.clone().unwrap_or_default();
            let avg = s.avg_frame_rate.as_deref().and_then(fraction).filter(|f| f.0 > 0 && f.1 > 0);
            let real = s.r_frame_rate.as_deref().and_then(fraction).filter(|f| f.0 > 0 && f.1 > 0);
            let rate = avg.or(real).and_then(|(n, d)| snap_rate(n, d)).unwrap_or(Rate::FPS_25);
            let secs = seconds(s.duration.as_ref()).or(container_secs).unwrap_or(0.0);
            let counted = s.nb_frames.as_ref().and_then(|n| n.parse::<i64>().ok()).filter(|n| *n > 0);
            // Display only crosses into the model here, once, as a frame count.
            let estimated = (secs * rate.num() as f64 / rate.den() as f64).round() as i64;
            let mut frames = counted.unwrap_or(estimated).max(1);
            if image_container && format != "gif" && format != "apng" && counted.is_none() {
                frames = 1;
            }
            let alpha = has_alpha(&pix_fmt);
            animated_alpha = image_container && frames > 1 && (alpha || format == "gif");
            let rotation =
                s.side_data_list.iter().find_map(|d| d.rotation).map_or(0, |r| (r.round() as i32).rem_euclid(360));
            // Only the MP4/MOV demuxer derives the average from its sample
            // table; elsewhere the header says nothing about VFR (S4, 2.3).
            let header_vfr = format.contains("mp4") && avg.is_some() && real.is_some() && {
                let ((an, ad), (rn, rd)) = (avg.unwrap_or((1, 1)), real.unwrap_or((1, 1)));
                an as u128 * rd as u128 != rn as u128 * ad as u128
            };
            Some(VideoInfo {
                codec: s.codec_name.clone(),
                width,
                height,
                rate,
                frames,
                bit_depth: bit_depth(&pix_fmt),
                pix_fmt,
                rotation,
                alpha,
                header_vfr,
                color_primaries: s.color_primaries.clone(),
                color_transfer: s.color_transfer.clone(),
                color_space: s.color_space.clone(),
                color_range: s.color_range.clone(),
            })
        }
    };
    let audio = sound.and_then(|s| {
        let sample_rate: u32 = s.sample_rate.as_ref()?.parse().ok().filter(|r| *r > 0)?;
        let secs = seconds(s.duration.as_ref()).or(container_secs).unwrap_or(0.0);
        Some(AudioInfo {
            codec: s.codec_name.clone(),
            sample_rate,
            channels: s.channels.unwrap_or(2).max(1),
            samples: (secs * sample_rate as f64).round() as i64,
        })
    });
    let kind = match (&video, &audio) {
        (Some(v), _) if image_container || (v.frames <= 1 && audio.is_none()) => AssetKind::Image,
        (Some(_), _) => AssetKind::Video,
        (None, Some(_)) => AssetKind::Audio,
        (None, None) => return Err(ToolError::other("no video, audio or image found in this file")),
    };
    let title = raw.format.tags.iter().find(|(k, _)| k.eq_ignore_ascii_case("title")).map(|(_, v)| v.trim().to_owned());
    Ok(Probed { kind, video, audio, title: title.filter(|t| !t.is_empty()), animated_alpha })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONE: &str = r#"{"streams":[
      {"codec_type":"video","codec_name":"hevc","width":3840,"height":2160,"pix_fmt":"yuv420p10le",
       "r_frame_rate":"60000/1001","avg_frame_rate":"323820000/5406109","nb_frames":"1799","duration":"30.013317",
       "color_range":"tv","color_transfer":"arib-std-b67","disposition":{"attached_pic":0},
       "side_data_list":[{"side_data_type":"Display Matrix","rotation":-90},{"side_data_type":"Content light level metadata"}]},
      {"codec_type":"audio","codec_name":"aac","sample_rate":"44100","channels":2,"duration":"30.000000"}],
      "format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"30.013317","tags":{"TITLE":" Holiday "}}}"#;

    #[test]
    fn phone_video() {
        let p = parse(PHONE).unwrap();
        assert_eq!(p.kind, AssetKind::Video);
        let v = p.video.unwrap();
        assert_eq!((v.width, v.height, v.bit_depth, v.rotation, v.frames), (3840, 2160, 10, 270, 1799));
        assert_eq!(v.rate.to_string(), "60000/1001");
        assert!(v.header_vfr, "average differs from the nominal rate");
        assert_eq!(v.display_size(), (2160, 3840));
        assert_eq!(v.color_transfer.as_deref(), Some("arib-std-b67"));
        let a = p.audio.unwrap();
        assert_eq!((a.sample_rate, a.channels, a.samples), (44_100, 2, 1_323_000));
        assert_eq!(p.title.as_deref(), Some("Holiday"));
    }

    #[test]
    fn webm_has_no_frame_count_and_no_header_verdict() {
        let json = r#"{"streams":[{"codec_type":"video","codec_name":"vp9","width":1920,"height":1080,"pix_fmt":"yuv420p",
            "r_frame_rate":"19001/317","avg_frame_rate":"19001/317"}],
            "format":{"format_name":"matroska,webm","duration":"10.000000"}}"#;
        let v = parse(json).unwrap().video.unwrap();
        assert_eq!(v.rate.to_string(), "60000/1001", "59.9401 snaps to 59.94");
        assert_eq!(v.frames, 599);
        assert!(!v.header_vfr);
    }

    #[test]
    fn stills_gifs_and_audio() {
        let jpg = r#"{"streams":[{"codec_type":"video","codec_name":"mjpeg","width":10109,"height":4542,"pix_fmt":"yuvj444p",
            "r_frame_rate":"25/1","avg_frame_rate":"25/1","duration":"0.040000"}],"format":{"format_name":"image2","duration":"0.04"}}"#;
        let p = parse(jpg).unwrap();
        assert_eq!((p.kind, p.video.as_ref().unwrap().frames, p.animated_alpha), (AssetKind::Image, 1, false));

        let gif = r#"{"streams":[{"codec_type":"video","codec_name":"gif","width":480,"height":270,"pix_fmt":"bgra",
            "r_frame_rate":"25/1","avg_frame_rate":"50/3","nb_frames":"25","duration":"1.5"}],"format":{"format_name":"gif"}}"#;
        let p = parse(gif).unwrap();
        assert_eq!((p.kind, p.animated_alpha), (AssetKind::Image, true));
        assert_eq!(p.video.unwrap().frames, 25);

        let mp3 = r#"{"streams":[{"codec_type":"audio","codec_name":"mp3","sample_rate":"48000","channels":2,"duration":"2.5"},
            {"codec_type":"video","codec_name":"mjpeg","width":500,"height":500,"disposition":{"attached_pic":1}}],
            "format":{"format_name":"mp3"}}"#;
        let p = parse(mp3).unwrap();
        assert_eq!((p.kind, p.video, p.audio.unwrap().samples), (AssetKind::Audio, None, 120_000));
    }

    #[test]
    fn rejects_files_without_media() {
        assert!(parse(r#"{"streams":[{"codec_type":"subtitle"}],"format":{"format_name":"srt"}}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn rate_snapping() {
        assert_eq!(snap_rate(2997, 100).unwrap().to_string(), "30000/1001");
        assert_eq!(snap_rate(30, 1).unwrap().to_string(), "30/1");
        assert_eq!(snap_rate(15, 1).unwrap().to_string(), "15/1");
        assert_eq!(snap_rate(34_080_000, 901_901).unwrap().to_string(), "34080000/901901");
        assert_eq!(snap_rate(0, 1), None);
    }

    #[test]
    fn pixel_formats() {
        assert_eq!(
            (bit_depth("yuv420p"), bit_depth("yuv420p10le"), bit_depth("p010le"), bit_depth("gbrp12le")),
            (8, 10, 10, 12)
        );
        assert!(has_alpha("yuva444p12le") && has_alpha("bgra") && !has_alpha("yuv420p"));
    }
}
