//! Analysis jobs on the conformed sound of assets: the beats of a song and
//! the position of a take in it. Blocking; run on a job thread.

use std::path::Path;

use sg_core::AssetId;
use sg_media::analysis::{self, Beats, Offset};
use sg_media::peaks::read_mono;
use sg_media::tool::{Cancel, ToolError};

use crate::layout::cache;

fn sound(root: &Path, id: AssetId, cancel: &Cancel) -> Result<Vec<f32>, ToolError> {
    let path = cache::audio(root, id);
    let (mono, rate) = read_mono(&path, cancel)
        .map_err(|e| ToolError::other(format!("its sound is not ready yet or it has none ({e})")))?;
    if rate as usize != analysis::RATE {
        return Err(ToolError::other(format!("conformed sound at {rate} Hz instead of 48 kHz")));
    }
    Ok(mono)
}

/// The beats of an asset's sound: read from the cache, or found and cached.
pub fn beats(root: &Path, id: AssetId, cancel: &Cancel) -> Result<Beats, ToolError> {
    let file = cache::beats(root, id);
    if let Some(known) = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
        return Ok(known);
    }
    let found = analysis::find_beats(&sound(root, id, cancel)?)
        .ok_or_else(|| ToolError::other("no steady rhythm was found in it"))?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written under another name first: a reader never sees half a file.
    let temporary = file.with_extension("beats.tmp");
    std::fs::write(&temporary, serde_json::to_vec(&found).map_err(|e| ToolError::other(e.to_string()))?)?;
    std::fs::rename(&temporary, &file)?;
    Ok(found)
}

/// Where the sound of `take` sits in the sound of `song`.
pub fn sync(root: &Path, song: AssetId, take: AssetId, cancel: &Cancel) -> Result<Offset, ToolError> {
    let song = sound(root, song, cancel)?;
    let take = sound(root, take, cancel)?;
    analysis::find_offset(&song, &take).ok_or_else(|| ToolError::other("there is too little sound to compare"))
}

/// Measures the picture of an asset for the colour match: twelve frames
/// spread over its length (one for a still), small, in linear light.
pub fn colour(root: &Path, asset: &sg_core::Asset, cancel: &Cancel) -> Result<sg_core::ColourStats, ToolError> {
    use sg_media::decode::VideoDecoder;
    let video = asset.video.as_ref().ok_or_else(|| ToolError::other("it has no picture"))?;
    // The constant-rate conform where there is one, as the export reads it.
    let conform = cache::conform(root, asset.id, asset.kind == sg_core::AssetKind::Image);
    let path = if conform.is_file() { conform } else { root.join(&asset.file) };
    let failed = |e: sg_media::decode::DecodeError| ToolError::other(e.to_string());
    let mut decoder = VideoDecoder::open(&path, 0).map_err(failed)?;
    decoder.set_rate(video.rate.num(), video.rate.den());
    decoder.set_max_dim(160);
    let frames = if video.is_still() { 1 } else { 12 };
    // The curve the compositor uses to get to linear light.
    let linear: Vec<f32> = (0..256)
        .map(|v| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        })
        .collect();
    let mut pixels: Vec<[f32; 3]> = Vec::new();
    for n in 0..frames {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        // Not the very first and last frames: fades and slates live there.
        let index = (video.frames.max(1) as f64 * (n as f64 + 0.5) / frames as f64) as i64;
        let frame = decoder.frame(index).map_err(failed)?;
        pixels.extend(
            frame
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| [linear[p[0] as usize], linear[p[1] as usize], linear[p[2] as usize]]),
        );
    }
    sg_core::ColourStats::measure(pixels.into_iter()).ok_or_else(|| ToolError::other("its picture is black throughout"))
}

/// A look chain as a file in `looks/`.
#[derive(serde::Serialize, serde::Deserialize)]
struct LookFile {
    name: String,
    modules: Vec<sg_core::Module>,
}

/// Writes a look chain to `looks/<slug>.toml` and returns the path
/// relative to the bundle.
pub fn save_look(root: &Path, name: &str, modules: &[sg_core::Module]) -> Result<String, ToolError> {
    let relative = format!("looks/{}.toml", sg_core::text::slug(name));
    let file = LookFile { name: name.to_owned(), modules: modules.to_vec() };
    let text = toml::to_string(&file).map_err(|e| ToolError::other(e.to_string()))?;
    std::fs::create_dir_all(root.join("looks"))?;
    let temporary = root.join(format!("{relative}.tmp"));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, root.join(&relative))?;
    Ok(relative)
}

/// The looks saved in `looks/`, by name. Files that do not read are skipped.
pub fn load_looks(root: &Path) -> Vec<(String, Vec<sg_core::Module>)> {
    let Ok(entries) = std::fs::read_dir(root.join("looks")) else { return Vec::new() };
    let mut looks: Vec<(String, Vec<sg_core::Module>)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "toml"))
        .filter_map(|e| toml::from_str::<LookFile>(&std::fs::read_to_string(e.path()).ok()?).ok())
        .map(|file| (file.name, file.modules.into_iter().map(sg_core::Module::clamped).collect()))
        .collect();
    looks.sort_by(|a, b| a.0.cmp(&b.0));
    looks
}

/// Makes the stabilised copy of an asset and its proxy, unless they are
/// there already. `progress` gets the share done, 0 to 1.
pub fn stabilise(
    root: &Path,
    asset: &sg_core::Asset,
    cancel: &Cancel,
    progress: &mut dyn FnMut(f32),
) -> Result<(), ToolError> {
    let video = asset.video.as_ref().filter(|v| !v.is_still()).ok_or_else(|| ToolError::other("it is not a video"))?;
    let (out, proxy) = (cache::stable(root, asset.id), cache::stable_proxy(root, asset.id));
    if out.is_file() && proxy.is_file() {
        return Ok(());
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conform = cache::conform(root, asset.id, false);
    let source = if conform.is_file() { conform } else { root.join(&asset.file) };
    let seconds = video.frames as f64 * video.rate.den() as f64 / video.rate.num() as f64;
    let motion = out.with_extension("trf");
    sg_media::derive::stabilise(&source, &out, &motion, &video.rate.to_string(), seconds, cancel, &mut |f| {
        progress(f * 0.85)
    })?;
    sg_media::derive::proxy(&out, &proxy, seconds, cancel, &mut |f| progress(0.85 + f * 0.15))
}
