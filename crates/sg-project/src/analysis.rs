//! Analysis jobs on the conformed sound of assets: the beats of a song and
//! the position of a take in it. Blocking; run on a job thread.

use std::path::{Path, PathBuf};

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

/// Listens to the voice on a track (docs/spec/09-daw.md, vocal chain):
/// `parts` are the stretches of assets its clips play, as (asset, first
/// sample, length) at 48 kHz. Up to four minutes are measured.
pub fn voice(
    root: &Path,
    parts: &[(AssetId, i64, i64)],
    cancel: &Cancel,
) -> Result<sg_media::voice::VoiceStats, ToolError> {
    sg_media::voice::measure(&voice_sound(root, parts, &[], cancel)?)
        .ok_or_else(|| ToolError::other("there is too little sound on this track to listen to"))
}

/// The sound [`voice`] listens to: the stretches one after the other,
/// mono at 48 kHz, four minutes at most.
///
/// `gains`: how loud each stretch is set to play (linear), in the order of
/// `parts`; a stretch without one plays as it is.
pub fn voice_sound(
    root: &Path,
    parts: &[(AssetId, i64, i64)],
    gains: &[f32],
    cancel: &Cancel,
) -> Result<Vec<f32>, ToolError> {
    let mut loaded: std::collections::HashMap<AssetId, Vec<f32>> = std::collections::HashMap::new();
    let mut heard: Vec<f32> = Vec::new();
    for (index, (id, from, len)) in parts.iter().enumerate() {
        let gain = gains.get(index).copied().filter(|gain| gain.is_finite()).unwrap_or(1.0);
        if !loaded.contains_key(id) {
            loaded.insert(*id, sound(root, *id, cancel)?);
        }
        let Some(samples) = loaded.get(id) else { continue };
        let first = (*from).clamp(0, samples.len() as i64) as usize;
        let last = (*from + *len).clamp(first as i64, samples.len() as i64) as usize;
        heard.extend(samples[first..last].iter().map(|sample| sample * gain));
        if heard.len() >= analysis::RATE * 240 {
            break;
        }
    }
    Ok(heard)
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
    // The same input transform the compositor applies to this asset.
    let transfer = sg_core::Transfer::of(video.color_transfer.as_deref());
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
                .map(|p| transfer.decode([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0])),
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

/// Where the model that cuts people out is kept: once per library, not per project.
const MATTE_MODEL: &str = "_models/rvm_mobilenetv3_fp32.onnx";
/// Where it comes from (the authors' release) and what it must be (SHA-256).
const MATTE_MODEL_URL: &str =
    "https://github.com/PeterL1n/RobustVideoMatting/releases/download/v1.0.0/rvm_mobilenetv3_fp32.onnx";
const MATTE_MODEL_SHA256: &str = "88d4531297118f595bf2fd60f6f566aec2e559393802d1f436c380f0cbbd2828";

/// The matting model in the library, fetched (15 MB) the first time it is
/// needed and checked against what it is known to be.
fn matte_model(library: &Path, cancel: &Cancel) -> Result<PathBuf, ToolError> {
    let model = library.join(MATTE_MODEL);
    let checked = |path: &Path| -> Result<bool, ToolError> {
        let mut sum = std::process::Command::new("sha256sum");
        sum.arg(path);
        Ok(sg_media::tool::run_output(sum, cancel)?.starts_with(MATTE_MODEL_SHA256))
    };
    if model.is_file() && checked(&model)? {
        return Ok(model);
    }
    if let Some(dir) = model.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let fetched = model.with_extension("onnx.part");
    let mut curl = std::process::Command::new("curl");
    curl.args(["--silent", "--show-error", "--fail", "--location", "--max-time", "600", "--output"]);
    curl.arg(&fetched).arg(MATTE_MODEL_URL);
    sg_media::tool::run_output(curl, cancel).map_err(|e| {
        let _ = std::fs::remove_file(&fetched);
        ToolError::other(format!(
            "the model (15 MB) could not be fetched from {MATTE_MODEL_URL}: {e}. It is needed once; with the file put at {} by hand it works without a network",
            model.display()
        ))
    })?;
    if !checked(&fetched)? {
        let _ = std::fs::remove_file(&fetched);
        return Err(ToolError::other("the model that was fetched is not the one that was expected; it was not used"));
    }
    std::fs::rename(&fetched, &model)?;
    Ok(model)
}

/// Cuts the person out of a video asset (feedback N2.5): writes the
/// picture with a transparent background as a WebM into the project's
/// cache and returns where it is, to be imported like any other file.
/// `library`: where the model is kept.
pub fn cut_out(
    library: &Path,
    root: &Path,
    asset: &sg_core::Asset,
    cancel: &Cancel,
    progress: &mut dyn FnMut(f32),
) -> Result<PathBuf, ToolError> {
    let video = asset.video.as_ref().filter(|v| !v.is_still()).ok_or_else(|| ToolError::other("it is not a video"))?;
    let model = matte_model(library, cancel)?;
    let folder = root.join(".shotgun/cache/cutout");
    std::fs::create_dir_all(&folder)?;
    let out = folder.join(format!("{} cut out.webm", sg_core::text::slug(&asset.label)));
    // (A copy with its frame rate put right, if there is one: the cut-out then has the same timing.)
    let conform = cache::conform(root, asset.id, false);
    let source = if conform.is_file() { conform } else { root.join(&asset.file) };
    sg_media::derive::cut_out(&model, &source, &out, video.frames, cancel, progress).map_err(|e| match e {
        // (A program started before this was built, from a shell that has not got the tool yet.)
        ToolError::Spawn { .. } => ToolError::other(format!(
            "{e}. The tool comes with the program: start Shotgun again from a new `nix develop` shell, or use the packaged program"
        )),
        other => other,
    })?;
    Ok(out)
}

/// The look-up tables in `assets/lut/`, by file name.
pub fn list_luts(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("assets/lut")) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.to_lowercase().ends_with(".cube"))
        .collect();
    names.sort();
    names
}

/// Copies a `.cube` file into `assets/lut/` after checking that it reads.
/// Returns the file name it has there.
pub fn import_lut(root: &Path, source: &Path) -> Result<String, ToolError> {
    let text = std::fs::read_to_string(source)?;
    sg_core::Lut::parse(&text).map_err(|e| ToolError::other(format!("it is not a usable look-up table ({e})")))?;
    let stem = source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = format!("{}.cube", sg_core::text::slug(&stem));
    let dir = root.join("assets/lut");
    std::fs::create_dir_all(&dir)?;
    let temporary = dir.join(format!("{name}.tmp"));
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, dir.join(&name))?;
    Ok(name)
}

/// The fonts in `assets/font/`, by file name.
pub fn list_fonts(root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join("assets/font")) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| is_font(Path::new(name)))
        .collect();
    names.sort();
    names
}

/// True for the file names fonts are taken from: TrueType and OpenType.
pub fn is_font(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
}

/// Copies a font file into `assets/font/` after a look at its first bytes.
/// Returns the file name it has there.
pub fn import_font(root: &Path, source: &Path) -> Result<String, ToolError> {
    let bytes = std::fs::read(source)?;
    // TrueType, OpenType with PostScript outlines, and Apple's TrueType.
    let known = [&[0u8, 1, 0, 0][..], b"OTTO", b"true"].iter().any(|magic| bytes.starts_with(magic));
    if !known {
        return Err(ToolError::other("it is not a TrueType or OpenType font"));
    }
    let stem = source.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let extension = source.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "ttf".into());
    let name = format!("{}.{extension}", sg_core::text::slug(&stem));
    let dir = root.join("assets/font");
    std::fs::create_dir_all(&dir)?;
    let temporary = dir.join(format!("{name}.tmp"));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, dir.join(&name))?;
    Ok(name)
}
