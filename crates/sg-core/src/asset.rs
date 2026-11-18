//! Assets: media files inside the bundle, described by one small index file
//! each (`assets/index/<id>.toml`). The index is part of the project state
//! and tracked by git; the media file itself is not (spike S5).

use serde::{Deserialize, Serialize};

use crate::ids::AssetId;
use crate::time::Rate;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    Video,
    Audio,
    /// Stills and GIFs.
    Image,
}

impl AssetKind {
    /// Folder under `assets/`.
    pub fn dir(self) -> &'static str {
        match self {
            AssetKind::Video => "video",
            AssetKind::Audio => "audio",
            AssetKind::Image => "image",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    /// Full BLAKE3 hash of the file, 64 hex digits. The id is its prefix.
    pub blake3: String,
    /// Size in bytes; with the hash, enough to detect a missing or swapped file.
    pub size: u64,
    /// Path relative to the bundle root. Never changes after import.
    pub file: String,
    pub kind: AssetKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioInfo>,
}

/// Where the asset came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// Original path or URL.
    pub origin: String,
    /// Unix seconds of the import.
    pub imported: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploader: Option<String>,
}

/// Picture stream as reported by the container. Also present for stills
/// (`frames == 1`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoInfo {
    pub codec: String,
    /// Stored size, before rotation.
    pub width: u32,
    pub height: u32,
    pub rate: Rate,
    pub frames: i64,
    pub pix_fmt: String,
    pub bit_depth: u8,
    /// Degrees the picture must be turned for display (0, 90, 180, 270).
    #[serde(default)]
    pub rotation: i32,
    #[serde(default)]
    pub alpha: bool,
    /// The container header suggests a variable frame rate. The packet scan
    /// run after import has the final word (see `sg-media`).
    #[serde(default)]
    pub header_vfr: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_primaries: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_transfer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_range: Option<String>,
}

impl VideoInfo {
    /// Size as displayed, after rotation.
    pub fn display_size(&self) -> (u32, u32) {
        if self.rotation.rem_euclid(180) == 90 { (self.height, self.width) } else { (self.width, self.height) }
    }

    pub fn is_still(&self) -> bool {
        self.frames <= 1
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioInfo {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    /// Length in samples at `sample_rate`.
    pub samples: i64,
}

impl Asset {
    /// Path of the index file, relative to the bundle root.
    pub fn index_path(id: AssetId) -> String {
        format!("assets/index/{id}.toml")
    }

    /// Length in frames at `rate` (a sequence rate): what a clip made from
    /// the whole asset occupies on a video track. Stills get `still_frames`.
    pub fn frames_at(&self, rate: Rate, still_frames: i64) -> i64 {
        if let Some(v) = &self.video {
            if v.is_still() {
                return still_frames;
            }
            // frames / own rate * target rate, rounded to nearest.
            let num = v.frames as i128 * v.rate.den() as i128 * rate.num() as i128;
            let den = v.rate.num() as i128 * rate.den() as i128;
            return ((num + den / 2) / den) as i64;
        }
        match &self.audio {
            Some(a) => rate.samples_to_frames(a.samples, a.sample_rate),
            None => still_frames,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample(id: u128) -> Asset {
        Asset {
            id: AssetId(id),
            blake3: format!("{id:064x}"),
            size: 1234,
            file: format!("assets/video/clip-{}.mp4", AssetId(id).short()),
            kind: AssetKind::Video,
            label: "Clip".into(),
            tags: vec![],
            source: Source { origin: "/home/x/clip.mp4".into(), imported: 1_700_000_000, uploader: None },
            video: Some(VideoInfo {
                codec: "hevc".into(),
                width: 3840,
                height: 2160,
                rate: Rate::new(60_000, 1001).unwrap(),
                frames: 600,
                pix_fmt: "yuv420p10le".into(),
                bit_depth: 10,
                rotation: 90,
                alpha: false,
                header_vfr: false,
                color_primaries: Some("bt709".into()),
                color_transfer: None,
                color_space: None,
                color_range: Some("tv".into()),
            }),
            audio: Some(AudioInfo { codec: "aac".into(), sample_rate: 44_100, channels: 2, samples: 441_441 }),
        }
    }

    #[test]
    fn index_file_round_trips_and_reads_well() {
        let asset = sample(0xAB);
        let text = toml::to_string(&asset).unwrap();
        assert!(text.contains("rate = \"60000/1001\""), "{text}");
        assert!(text.contains("[source]") && text.contains("[video]") && text.contains("[audio]"));
        assert!(!text.contains("tags"), "empty lists are left out");
        assert_eq!(toml::from_str::<Asset>(&text).unwrap(), asset);
    }

    #[test]
    fn length_on_a_timeline() {
        let asset = sample(1);
        // 600 frames of 59.94 are 10.01 s: 600.6 -> 601 frames at 60 fps, 300 at 29.97.
        assert_eq!(asset.frames_at(Rate::FPS_60, 300), 601);
        assert_eq!(asset.frames_at(Rate::new(30_000, 1001).unwrap(), 300), 300);
        let mut audio_only = sample(2);
        audio_only.video = None;
        assert_eq!(audio_only.frames_at(Rate::FPS_60, 300), 600);
        let mut still = sample(3);
        still.video.as_mut().unwrap().frames = 1;
        assert_eq!(still.frames_at(Rate::FPS_60, 300), 300);
        assert_eq!(asset.video.as_ref().unwrap().display_size(), (2160, 3840));
    }
}
