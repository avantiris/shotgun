//! Export: renders the sequence offline with the same compositor and the
//! same audio mix as the preview and encodes the result (spike S4, D5 to D7).
//!
//! Runs on its own thread and blocks freely. Every hardware export is
//! decoded once afterwards before it is reported as done.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Instant;

use sg_audio::{Backend, Engine, SAMPLE_RATE};
use sg_core::text::slug;
use sg_core::{AssetId, AssetKind, Project, Sequence};
use sg_media::decode::{VideoDecoder, VideoFrame};
use sg_media::tool::{Cancel, ToolError, run_lines, run_output};
use sg_project::cache;
use sg_render::{Compositor, LayerDraw, PlanLayer, Source, YuvImporter, YuvPlanes, plan, sequence_frames};

#[derive(Clone, Debug, PartialEq)]
pub enum Preset {
    /// The sequence as it is: HEVC Main10, constant quality.
    Master,
    /// 9:16: the frame turned upright; pictures fill it and lose their sides.
    Vertical,
    /// H.264 to send: compressed as `compression` says, and in no case
    /// larger than this many megabytes (10^6 bytes).
    Share { megabytes: u32, compression: Compression },
    /// Animated GIF; `compression` decides its width, frame rate and how
    /// much the encoder may give up for a smaller file.
    Gif { compression: Compression },
    /// The sound alone (a song). With `stems`, also every track by itself,
    /// sounding as it does in the mix, in a folder beside the file. With
    /// `loudness`, the whole is brought to that many LUFS.
    Sound { format: SoundFormat, stems: bool, loudness: Option<f32> },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoundFormat {
    /// 24-bit WAV: for mastering elsewhere, and for distributors.
    #[default]
    Wav,
    /// The same without loss at about half the size.
    Flac,
    /// MP3 at 320 kbit/s: to send.
    Mp3,
}

impl SoundFormat {
    pub fn from_level(level: u8) -> SoundFormat {
        [SoundFormat::Wav, SoundFormat::Flac, SoundFormat::Mp3][level.min(2) as usize]
    }

    fn extension(self) -> &'static str {
        match self {
            SoundFormat::Wav => "wav",
            SoundFormat::Flac => "flac",
            SoundFormat::Mp3 => "mp3",
        }
    }

    fn codec(self) -> &'static [&'static str] {
        match self {
            SoundFormat::Wav => &["-c:a", "pcm_s24le"],
            SoundFormat::Flac => &["-c:a", "flac", "-sample_fmt", "s32", "-bits_per_raw_sample", "24"],
            SoundFormat::Mp3 => &["-c:a", "libmp3lame", "-b:a", "320k"],
        }
    }
}

/// How much quality a share copy or GIF trades for a smaller file
/// (feedback F7.1). `Light` is what both were before there was a choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    /// The best the format can do; the largest file.
    None,
    #[default]
    Light,
    Medium,
    Strong,
    /// As small as is still worth looking at.
    Smallest,
}

impl Compression {
    pub const ALL: [Compression; 5] =
        [Compression::None, Compression::Light, Compression::Medium, Compression::Strong, Compression::Smallest];

    /// 0 (none) to 4 (smallest); anything above counts as 4.
    pub fn from_level(level: u8) -> Compression {
        Compression::ALL[(level as usize).min(4)]
    }

    /// GIF: width in pixels, frames per second, gifski's quality, and its
    /// lossy and motion quality (not passed where `None`).
    fn gif(self) -> (u32, u32, u32, Option<u32>, Option<u32>) {
        match self {
            Compression::None => (480, 20, 100, None, None),
            Compression::Light => (480, 20, 90, None, None),
            Compression::Medium => (480, 15, 75, Some(60), Some(70)),
            Compression::Strong => (400, 12, 60, Some(40), Some(50)),
            Compression::Smallest => (320, 10, 45, Some(25), Some(35)),
        }
    }

    /// Share copy: x264's constant quality (lower is better) and the
    /// greatest height and frame rate.
    fn share(self) -> (u32, u32, f64) {
        match self {
            Compression::None => (14, 2160, 60.0),
            Compression::Light => (21, 1080, 60.0),
            Compression::Medium => (26, 1080, 30.0),
            Compression::Strong => (30, 720, 30.0),
            Compression::Smallest => (35, 480, 30.0),
        }
    }
}

impl Preset {
    pub fn label(&self) -> &'static str {
        match self {
            Preset::Master => "master",
            Preset::Vertical => "vertical",
            Preset::Share { .. } => "share",
            Preset::Gif { .. } => "gif",
            Preset::Sound { .. } => "sound",
        }
    }

    fn extension(&self) -> &'static str {
        match self {
            Preset::Gif { .. } => "gif",
            Preset::Sound { format, .. } => format.extension(),
            _ => "mp4",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub root: PathBuf,
    pub project: Arc<Project>,
    pub preset: Preset,
    /// Frames to export (start, end); the whole sequence if `None`.
    pub range: Option<(i64, i64)>,
}

#[derive(Clone, Debug)]
pub struct Exported {
    pub path: PathBuf,
    pub bytes: u64,
    pub frames: i64,
    /// Wall time of the whole export.
    pub seconds: f64,
    /// Rendered and encoded frames per second of the render pass.
    pub render_fps: f64,
    /// Milliseconds per frame spent decoding, uploading, compositing with
    /// read-back, and feeding the encoder.
    pub stage_ms: [f64; 4],
    pub encoder: &'static str,
    /// A sound export: its loudness in LUFS (`None`: silence), its highest
    /// peak in decibels below full scale, and how many stems were written.
    pub sound: Option<(Option<f32>, f32, usize)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("cancelled")]
    Cancelled,
    #[error("there is nothing to export: the sequence is empty")]
    Empty,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Tool(String),
    #[error("the exported file does not play: {0}")]
    Verify(String),
    #[error("the GPU reported an error while the frames were drawn (the log has it); the file was not kept")]
    Gpu,
}

impl From<ToolError> for ExportError {
    fn from(e: ToolError) -> ExportError {
        match e {
            ToolError::Cancelled => ExportError::Cancelled,
            other => ExportError::Tool(other.to_string()),
        }
    }
}

const VAAPI_DEVICE: &str = "/dev/dri/renderD128";
const COLOUR: [&str; 8] =
    ["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"];

/// Reports (stage, fraction done).
pub type Progress<'a> = &'a mut dyn FnMut(&'static str, f32);

fn even(n: u32) -> u32 {
    (n.max(2) / 2) * 2
}

/// A free name in `exports/`: `<project>-<preset>-<n>.<ext>`.
fn output_path(root: &Path, project: &Project, preset: &Preset) -> PathBuf {
    let dir = root.join("exports");
    let base = format!("{}-{}", slug(&project.name), preset.label());
    (1..)
        .map(|n| dir.join(format!("{base}-{n}.{}", preset.extension())))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join(base))
}

/// Renders the sequence's sound to a 48 kHz float WAV through the engine's
/// own mixing code.
fn render_audio(
    root: &Path,
    sequence: &Sequence,
    range: (i64, i64),
    out: &Path,
    cancel: &Cancel,
) -> Result<(), ExportError> {
    render_mix(sg_play::audio_mix(root, sequence), sequence, range, out, cancel).map(|_| ())
}

/// Renders a mix of the sequence to a 48 kHz float WAV and measures it.
fn render_mix(
    mix: sg_audio::Mix,
    sequence: &Sequence,
    range: (i64, i64),
    out: &Path,
    cancel: &Cancel,
) -> Result<sg_audio::loudness::Loudness, ExportError> {
    let engine = Engine::start(Backend::Manual).map_err(|e| ExportError::Tool(e.to_string()))?;
    engine.set_mix(mix);
    let mut loudness = sg_audio::loudness::Loudness::default();
    let first = sequence.rate.frames_to_samples(range.0, SAMPLE_RATE);
    let total = (sequence.rate.frames_to_samples(range.1, SAMPLE_RATE) - first).max(0) as u64;
    let mut file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let data = total * 8;
    // WAV sizes are 32-bit; an RF64 header would be needed beyond 4 GB (3.1 hours).
    let riff = (36 + data).min(u32::MAX as u64) as u32;
    file.write_all(b"RIFF")?;
    file.write_all(&riff.to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&3u16.to_le_bytes())?;
    file.write_all(&2u16.to_le_bytes())?;
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&(SAMPLE_RATE * 8).to_le_bytes())?;
    file.write_all(&8u16.to_le_bytes())?;
    file.write_all(&32u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&(data.min(u32::MAX as u64) as u32).to_le_bytes())?;
    engine.play(first);
    let mut block = vec![0.0f32; 4096 * 2];
    let mut done = 0u64;
    while done < total {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        // The disk thread refills the ring buffers; wait for it, then take
        // less than they hold.
        engine.settle();
        let frames = (total - done).min(4096) as usize;
        engine.pull(&mut block[..frames * 2]);
        loudness.add(&block[..frames * 2]);
        for sample in &block[..frames * 2] {
            file.write_all(&sample.to_le_bytes())?;
        }
        done += frames as u64;
    }
    file.flush()?;
    Ok(loudness)
}

/// The sound alone: the mix as a file, brought to a loudness if asked,
/// and every track by itself if asked.
#[allow(clippy::too_many_arguments)]
fn export_sound(
    job: &Job,
    sequence: &Sequence,
    range: (i64, i64),
    (format, stems, target): (SoundFormat, bool, Option<f32>),
    work: &Path,
    cancel: &Cancel,
    progress: Progress<'_>,
    started: Instant,
) -> Result<Exported, ExportError> {
    let mut mix = sg_play::audio_mix(&job.root, sequence);
    if mix.tracks.iter().all(|t| t.clips.is_empty() && t.notes.is_empty()) {
        return Err(ExportError::Empty);
    }
    let rendered = work.join("mix.wav");
    progress("mixing sound", 0.0);
    let mut heard = render_mix(mix.clone(), sequence, range, &rendered, cancel)?;
    if let (Some(target), Some(now)) = (target, heard.integrated()) {
        // Turned up or down by what is missing, and mixed again: the
        // limiter on the master then holds what would go over full scale.
        // (At most 30 dB up: a near-silent piece is not made of noise.)
        mix.master_gain *= 10f32.powf((target - now).clamp(-60.0, 30.0) / 20.0);
        progress("setting the loudness", 0.3);
        heard = render_mix(mix.clone(), sequence, range, &rendered, cancel)?;
    }
    let encode = |from: &Path, to: &Path| {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(from).args(format.codec()).arg(to);
        tool(cmd, cancel)
    };
    let out = output_path(&job.root, &job.project, &job.preset);
    let unfinished = Unfinished(Some(out.clone()));
    progress("writing the file", 0.5);
    encode(&rendered, &out)?;
    let mut written = 0;
    if stems {
        // Beside the file, under its name: `<name>-stems/01 <track>.wav`.
        let folder = out.with_extension("").with_file_name(format!(
            "{}-stems",
            out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        ));
        std::fs::create_dir_all(&folder)?;
        let names: Vec<&str> =
            sequence.tracks.iter().filter(|t| t.kind == sg_core::TrackKind::Audio).map(|t| t.name.as_str()).collect();
        // (A muted track is not in the song: it gets no file.)
        let sounding: Vec<usize> = (0..mix.tracks.len())
            .filter(|i| {
                let track = &mix.tracks[*i];
                !track.mute && (!track.clips.is_empty() || !track.notes.is_empty())
            })
            .collect();
        for (n, index) in sounding.iter().enumerate() {
            progress("writing the tracks", 0.5 + 0.5 * n as f32 / sounding.len() as f32);
            let mut alone = mix.clone();
            for (other, track) in alone.tracks.iter_mut().enumerate() {
                track.unheard = other != *index;
            }
            let stem = work.join("stem.wav");
            render_mix(alone, sequence, range, &stem, cancel)?;
            let name = slug(names.get(*index).copied().unwrap_or("track"));
            encode(&stem, &folder.join(format!("{:02}-{name}.{}", n + 1, format.extension())))?;
            written += 1;
        }
    }
    progress("checking", 0.95);
    let mut decode = Command::new("ffmpeg");
    decode.args(["-v", "error", "-xerror", "-i"]).arg(&out).args(["-f", "null", "-"]);
    run_lines(decode, cancel, |_| {}).map_err(|e| ExportError::Verify(e.to_string()))?;
    let bytes = std::fs::metadata(&out)?.len();
    unfinished.keep();
    Ok(Exported {
        path: out,
        bytes,
        frames: range.1 - range.0,
        seconds: started.elapsed().as_secs_f64(),
        render_fps: 0.0,
        stage_ms: [0.0; 4],
        encoder: "sound only",
        sound: Some((heard.integrated(), heard.peak_db(), written)),
    })
}

struct Encoder {
    child: Child,
    stderr: Option<std::thread::JoinHandle<String>>,
}

impl Encoder {
    /// Starts ffmpeg reading raw RGBA frames on stdin.
    fn start(
        size: (u32, u32),
        rate: sg_core::Rate,
        audio: Option<&Path>,
        hardware: bool,
        ten_bit: bool,
        // The frames themselves are 10-bit (P010), not 8-bit to be widened.
        deep: bool,
        out: &Path,
    ) -> Result<Encoder, ExportError> {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-y"]);
        if hardware {
            cmd.args(["-vaapi_device", VAAPI_DEVICE]);
        }
        // Frames arrive as NV12 or, for 10-bit masters, P010: already
        // limited-range BT.709, converted on the GPU from the floating-point
        // composite.
        let input = if deep { "p010le" } else { "nv12" };
        cmd.args([
            "-f",
            "rawvideo",
            "-pix_fmt",
            input,
            "-s",
            &format!("{}x{}", size.0, size.1),
            "-r",
            &rate.to_string(),
        ]);
        cmd.args(COLOUR).args(["-i", "pipe:0"]);
        if let Some(audio) = audio {
            cmd.arg("-i").arg(audio);
        }
        if hardware {
            let filter = match (ten_bit, deep) {
                (true, true) => "format=p010,hwupload",
                (true, false) => "format=nv12,hwupload,scale_vaapi=format=p010",
                (false, _) => "format=nv12,hwupload",
            };
            cmd.args(["-vf", filter, "-c:v", "hevc_vaapi"]);
            if ten_bit {
                cmd.args(["-profile:v", "main10"]);
            }
            // The default hev1 tag: adding hvc1 here gives an undecodable file on this GPU (S4, 5.3).
            cmd.args(["-rc_mode", "CQP", "-qp", "20"]);
        } else {
            cmd.args(["-vf", "format=yuv420p", "-c:v", "libx264", "-preset", "medium", "-crf", "16"]);
        }
        cmd.args(COLOUR);
        if audio.is_some() {
            // No -shortest: the sound is rendered to exactly the length of the
            // picture, and with a pipe as input -shortest can end the file early.
            cmd.args(["-c:a", "aac", "-b:a", "256k"]);
        }
        cmd.args(["-movflags", "+faststart"]).arg(out);
        cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| ExportError::Tool(format!("cannot start ffmpeg: {e}")))?;
        if let Some(stdin) = &child.stdin {
            // A 4K frame is 12 MB; through the default 64 kB pipe it takes 190
            // round trips between the two processes. Failure only costs speed.
            // SAFETY: fcntl on a descriptor this process owns and keeps open.
            unsafe {
                libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(stdin), libc::F_SETPIPE_SZ, 1 << 20);
            }
        }
        let stderr = child.stderr.take().map(|mut pipe| {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = std::io::Read::read_to_string(&mut pipe, &mut text);
                text
            })
        });
        Ok(Encoder { child, stderr })
    }

    fn write(&mut self, frame: &[u8]) -> std::io::Result<()> {
        match &mut self.child.stdin {
            Some(stdin) => stdin.write_all(frame),
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }

    /// Closes the input and waits; the encoder's complaint if it failed.
    fn finish(mut self) -> Result<(), String> {
        drop(self.child.stdin.take());
        let status = self.child.wait().map_err(|e| e.to_string())?;
        let stderr = self.stderr.take().and_then(|t| t.join().ok()).unwrap_or_default();
        if status.success() { Ok(()) } else { Err(stderr.trim().lines().last().unwrap_or("ffmpeg failed").to_owned()) }
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One frame on its way from the decode stage to the compositing stage.
struct Decoded {
    layers: Vec<PlanLayer>,
    /// Per layer: the picture, if it is another one than this asset showed last.
    fresh: Vec<Option<VideoFrame>>,
}

/// Which decoder and which picture a layer uses: its asset, whether the
/// stabilised copy, and the how-manieth layer of the frame showing that.
type Slot = (AssetId, bool, usize);

/// The slot of every layer of a frame (`None` for a title). The decode
/// stage and the compositing stage both count this way, so they agree.
fn slots(layers: &[PlanLayer]) -> Vec<Option<Slot>> {
    let mut seen: Vec<(AssetId, bool)> = Vec::with_capacity(layers.len());
    layers
        .iter()
        .map(|layer| {
            let Source::Asset { asset, .. } = &layer.source else { return None };
            let key = (*asset, layer.props.as_ref().is_some_and(|p| p.stabilise));
            let nth = seen.iter().filter(|k| **k == key).count();
            seen.push(key);
            Some((key.0, key.1, nth))
        })
        .collect()
}

/// Frames waiting between two stages. Small: a 4K picture is 33 MB.
const QUEUE: usize = 3;

/// First stage: plans every frame and decodes the pictures it needs, at
/// full quality. Returns the seconds spent decoding.
fn decode_stage(
    job: &Job,
    sequence: &Sequence,
    range: (i64, i64),
    cancel: &Cancel,
    out: SyncSender<Decoded>,
) -> Result<f64, ExportError> {
    // One decoder per asset and per layer showing it at the same time: two
    // layers of one asset at different places in it (a clip over a copy of
    // itself) would otherwise send one decoder back and forth, a seek for
    // every frame (measured: 34 ms a frame instead of 2).
    let mut decoders: HashMap<Slot, VideoDecoder> = HashMap::new();
    // The source frame last handed on per slot: stills and held frames are sent once.
    let mut shown: HashMap<Slot, i64> = HashMap::new();
    let mut spent = 0.0;
    for frame in range.0..range.1 {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let layers = plan(&job.project, sequence, frame);
        let mut fresh = Vec::with_capacity(layers.len());
        for (layer, key) in layers.iter().zip(slots(&layers)) {
            let (Source::Asset { asset: id, frame: source_frame }, Some(key)) = (&layer.source, key) else {
                fresh.push(None);
                continue;
            };
            let asset = job.project.assets.get(id);
            let (Some(asset), true) = (asset, shown.get(&key) != Some(source_frame)) else {
                fresh.push(None);
                continue;
            };
            let failed = |e: sg_media::decode::DecodeError| ExportError::Tool(format!("{}: {e}", asset.label));
            if let std::collections::hash_map::Entry::Vacant(slot) = decoders.entry(key) {
                // Originals, or the constant-rate conform where one was needed.
                let conform = cache::conform(&job.root, *id, asset.kind == AssetKind::Image);
                let stable_copy = cache::stable(&job.root, *id);
                // A stabilised clip reads the stabilised copy of its asset.
                let path = if key.1 && stable_copy.is_file() {
                    stable_copy
                } else if conform.is_file() {
                    conform
                } else {
                    job.root.join(&asset.file)
                };
                let mut decoder = VideoDecoder::open(&path, 0).map_err(failed)?;
                if let Some(v) = &asset.video {
                    decoder.set_rate(v.rate.num(), v.rate.den());
                }
                // No preview-sized cap: only what no GPU texture can hold.
                decoder.set_max_dim(8192);
                // Planes go to the GPU as they are and become RGB there.
                decoder.set_planes(true);
                slot.insert(decoder);
            }
            let Some(decoder) = decoders.get_mut(&key) else {
                fresh.push(None);
                continue;
            };
            let mark = Instant::now();
            fresh.push(Some(decoder.frame(*source_frame).map_err(failed)?));
            spent += mark.elapsed().as_secs_f64();
            shown.insert(key, *source_frame);
        }
        if out.send(Decoded { layers, fresh }).is_err() {
            // The compositing stage stopped; it knows why.
            break;
        }
    }
    Ok(spent)
}

/// The offline render: three stages running side by side (decode, composite
/// on the GPU, feed the encoder), every frame at full quality. Returns the
/// frames per second achieved and the milliseconds per frame of each stage.
#[allow(clippy::too_many_arguments)]
fn render(
    job: &Job,
    sequence: &Sequence,
    range: (i64, i64),
    size: (u32, u32),
    cover: bool,
    deep: bool,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut Encoder,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(f64, [f64; 4]), ExportError> {
    let mut compositor = Compositor::new(device.clone(), queue.clone(), size.0, size.1);
    compositor.set_cover(cover);
    let importer = YuvImporter::new(device.clone(), queue.clone());
    // The picture each asset shows at the moment.
    let mut pictures: HashMap<Slot, (wgpu::TextureView, (u32, u32), i32)> = HashMap::new();
    let mut titles: HashMap<(String, u32, [u8; 4], String, u8), (wgpu::TextureView, (u32, u32))> = HashMap::new();
    let mut fonts = sg_render::text::Fonts::default();
    let started = Instant::now();
    let total = (range.1 - range.0).max(1);
    let (mut uploading, mut compositing) = (0.0f64, 0.0f64);
    let (decoded, writing, composited) = std::thread::scope(|scope| {
        let (frames_tx, frames_rx) = sync_channel::<Decoded>(QUEUE);
        let (pixels_tx, pixels_rx) = sync_channel::<Vec<u8>>(QUEUE);
        let decoding = scope.spawn(move || decode_stage(job, sequence, range, cancel, frames_tx));
        let writing = scope.spawn(move || -> (std::io::Result<()>, f64) {
            let mut spent = 0.0;
            for pixels in pixels_rx {
                let mark = Instant::now();
                if let Err(e) = encoder.write(&pixels) {
                    return (Err(e), spent);
                }
                spent += mark.elapsed().as_secs_f64();
            }
            (Ok(()), spent)
        });
        let mut done = 0i64;
        let mut composite = || -> Result<(), ExportError> {
            for Decoded { layers, fresh } in frames_rx.iter() {
                let mut drawn: Vec<(wgpu::TextureView, (u32, u32), i32, bool, usize)> =
                    Vec::with_capacity(layers.len());
                let slot_of = slots(&layers);
                for (index, (layer, fresh)) in layers.iter().zip(fresh).enumerate() {
                    match &layer.source {
                        Source::Title => {
                            let Some(title) = layer.props.as_ref().and_then(|p| p.title.as_ref()) else { continue };
                            let key = (
                                title.text.clone(),
                                (title.size * size.1 as f32).round().max(4.0) as u32,
                                title.color,
                                title.font.clone(),
                                (title.shadow.clamp(0.0, 1.0) * 100.0).round() as u8,
                            );
                            let entry = titles.entry(key.clone()).or_insert_with(|| {
                                let font = fonts.get(&job.root, &key.3);
                                let image = sg_render::text::rasterize(
                                    &key.0,
                                    key.1 as f32,
                                    key.2,
                                    font.as_ref().map(|bytes| bytes.as_slice()),
                                );
                                let image = sg_render::text::with_shadow(image, key.1 as f32, key.4 as f32 / 100.0);
                                let texture = Compositor::upload(device, queue, image.width, image.height, &image.rgba);
                                (texture.create_view(&Default::default()), (image.width, image.height))
                            });
                            drawn.push((entry.0.clone(), entry.1, 0, true, index));
                        }
                        Source::Asset { .. } => {
                            let Some(slot) = slot_of[index] else { continue };
                            if let Some(picture) = fresh {
                                let mark = Instant::now();
                                let converted = picture.planes.as_ref().and_then(|p| {
                                    let (kr, kb) = p.matrix.kr_kb();
                                    importer.import(&YuvPlanes {
                                        size: (picture.width, picture.height),
                                        chroma_size: p.chroma_size,
                                        y: &p.y,
                                        u: &p.u,
                                        v: &p.v,
                                        kr,
                                        kb,
                                        full_range: p.full_range,
                                    })
                                });
                                let Some(texture) = converted.or_else(|| {
                                    let complete =
                                        picture.rgba.len() == picture.width as usize * picture.height as usize * 4;
                                    complete.then(|| {
                                        Compositor::upload(device, queue, picture.width, picture.height, &picture.rgba)
                                    })
                                }) else {
                                    continue;
                                };
                                uploading += mark.elapsed().as_secs_f64();
                                let view = texture.create_view(&Default::default());
                                pictures.insert(slot, (view, (picture.width, picture.height), picture.rotation));
                            }
                            if let Some((view, picture_size, rotation)) = pictures.get(&slot) {
                                drawn.push((view.clone(), *picture_size, *rotation, false, index));
                            }
                        }
                    }
                }
                let draws: Vec<LayerDraw<'_>> = drawn
                    .iter()
                    .map(|(view, picture_size, rotation, title, index)| {
                        layers[*index].draw(view, *picture_size, *rotation, *title)
                    })
                    .collect();
                let mark = Instant::now();
                sg_render::load_luts(&mut compositor, &job.root, &layers);
                compositor.render(&draws);
                let read = if deep { compositor.read_output_p010() } else { compositor.read_output_nv12() };
                let pixels = read.map_err(|e| ExportError::Tool(format!("GPU readback failed: {e:?}")))?;
                compositing += mark.elapsed().as_secs_f64();
                if pixels_tx.send(pixels).is_err() {
                    // The encoder stopped reading; the writing stage has the error.
                    break;
                }
                done += 1;
                progress("rendering", done as f32 / total as f32);
            }
            Ok(())
        };
        let composited = composite();
        // Closing both queues lets the other stages end whatever happened here.
        drop(frames_rx);
        drop(pixels_tx);
        (decoding.join(), writing.join(), composited)
    });
    let gone = || ExportError::Tool("an export thread stopped unexpectedly".into());
    let (written, piping) = writing.map_err(|_| gone())?;
    // A broken pipe comes first: the caller asks the encoder what went wrong.
    written?;
    let decoding = decoded.map_err(|_| gone())??;
    composited?;
    let per_frame = [decoding, uploading, compositing, piping].map(|s| s * 1e3 / total as f64);
    Ok((total as f64 / started.elapsed().as_secs_f64().max(1e-6), per_frame))
}

/// Removes the output file when dropped, unless the export got to its end.
struct Unfinished(Option<PathBuf>);

impl Unfinished {
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Unfinished {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Decodes the whole file once and counts its frames. The graphics card
/// decodes it (five times faster than the processor for a 4K master, and
/// what a player on this machine would use); if that fails for any reason
/// the processor has the last word.
fn verify(path: &Path, expected_frames: i64, cancel: &Cancel) -> Result<(), ExportError> {
    let decode = |hardware: bool| {
        let mut decode = Command::new("ffmpeg");
        decode.args(["-v", "error", "-xerror"]);
        if hardware {
            decode.args(["-hwaccel", "vaapi", "-hwaccel_output_format", "vaapi"]);
        }
        decode.arg("-i").arg(path).args(["-f", "null", "-"]);
        run_lines(decode, cancel, |_| {})
    };
    match decode(true) {
        Ok(()) => {}
        Err(ToolError::Cancelled) => return Err(ExportError::Cancelled),
        Err(_) => decode(false).map_err(|e| match e {
            ToolError::Cancelled => ExportError::Cancelled,
            other => ExportError::Verify(other.to_string()),
        })?,
    }
    let mut probe = Command::new("ffprobe");
    probe
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(path);
    let frames: i64 = run_output(probe, cancel)?.trim().trim_end_matches(',').parse().unwrap_or(-1);
    // Encoders may drop or add a frame at the very end; more than that is a broken file.
    if (frames - expected_frames).abs() > 2 {
        return Err(ExportError::Verify(format!("{frames} frames instead of {expected_frames}")));
    }
    Ok(())
}

fn tool(cmd: Command, cancel: &Cancel) -> Result<(), ExportError> {
    run_lines(cmd, cancel, |_| {}).map_err(ExportError::from)
}

/// H.264 at the quality the compression asks for; only if that comes out
/// above the size limit, two passes at the bitrate that fits (S4, D6).
/// A short clip is therefore as small as its quality needs, not as large
/// as the limit allows.
#[allow(clippy::too_many_arguments)]
fn share_copy(
    render: &Path,
    out: &Path,
    megabytes: u32,
    compression: Compression,
    seconds: f64,
    fps: f64,
    work: &Path,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<(), ExportError> {
    let target = megabytes.max(1) as f64 * 1_000_000.0;
    let audio = work.join("share-audio.m4a");
    let has_audio = {
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(render);
        cmd.args(["-vn", "-c:a", "aac", "-b:a", if megabytes >= 25 { "128k" } else { "96k" }]).arg(&audio);
        tool(cmd, cancel).is_ok() && audio.is_file()
    };
    let audio_bytes = if has_audio { std::fs::metadata(&audio)?.len() as f64 } else { 0.0 };
    {
        let (crf, height, most) = compression.share();
        progress("compressing", 0.1);
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(render);
        if has_audio {
            cmd.arg("-i").arg(&audio).args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "copy"]);
        } else {
            cmd.arg("-an");
        }
        let filter = format!("fps={},scale=-2:'min({height},ih)':flags=lanczos", fps.min(most));
        cmd.args(["-vf", &filter, "-c:v", "libx264", "-preset", "medium", "-crf", &crf.to_string()]);
        cmd.args(["-pix_fmt", "yuv420p"]).args(COLOUR).args(["-movflags", "+faststart"]).arg(out);
        tool(cmd, cancel)?;
        if std::fs::metadata(out)?.len() as f64 <= target {
            return Ok(());
        }
    }
    let budget = target * 0.99 - audio_bytes - target * 0.003;
    let mut bitrate = (budget * 8.0 / seconds.max(0.1)).max(50_000.0);
    // Less picture when there are fewer bits to spend (proposed ladder of S4).
    let (height, rate) = if bitrate >= 6e6 {
        (1080, fps.min(60.0))
    } else if bitrate >= 2e6 {
        (720, fps.min(30.0))
    } else {
        (480, fps.min(30.0))
    };
    let filter = format!("fps={rate},scale=-2:'min({height},ih)':flags=lanczos");
    let log = work.join("share-pass");
    for attempt in 0..3 {
        for pass in [1, 2] {
            progress("fitting to size", (attempt * 2 + pass) as f32 / 6.0);
            let mut cmd = Command::new("ffmpeg");
            cmd.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(render);
            if pass == 2 && has_audio {
                cmd.arg("-i").arg(&audio).args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "copy"]);
            } else {
                cmd.arg("-an");
            }
            cmd.args(["-vf", &filter, "-c:v", "libx264", "-preset", "medium", "-b:v", &format!("{}", bitrate as u64)]);
            cmd.args(["-pass", &pass.to_string(), "-passlogfile"]).arg(&log).args(["-pix_fmt", "yuv420p"]).args(COLOUR);
            if pass == 1 {
                cmd.args(["-f", "null", "-"]);
            } else {
                cmd.args(["-movflags", "+faststart"]).arg(out);
            }
            tool(cmd, cancel)?;
        }
        let size = std::fs::metadata(out)?.len() as f64;
        if size <= target && size >= target * 0.9 {
            return Ok(());
        }
        // Too big, or far too small: scale the bitrate by how the video payload came out.
        let payload = (size - audio_bytes).max(1.0);
        bitrate = (bitrate * budget / payload).max(50_000.0);
        if size <= target && attempt == 2 {
            return Ok(());
        }
    }
    if std::fs::metadata(out)?.len() as f64 > target {
        return Err(ExportError::Tool(format!(
            "could not get under {megabytes} MB; try a larger size or a shorter range"
        )));
    }
    Ok(())
}

/// GIF through gifski (S4, D7).
fn gif(render: &Path, out: &Path, compression: Compression, cancel: &Cancel) -> Result<(), ExportError> {
    let quote = |p: &Path| -> Result<String, ExportError> {
        let text = p.to_string_lossy();
        // The paths are made by the app from slugs; refuse anything that could break the quoting.
        if text.contains('\'') {
            Err(ExportError::Tool("the project path contains a quote character".into()))
        } else {
            Ok(format!("'{text}'"))
        }
    };
    let (width, fps, quality, lossy, motion) = compression.gif();
    let mut options = format!("--fps {fps} --quality {quality}");
    for (name, value) in [("--lossy-quality", lossy), ("--motion-quality", motion)] {
        if let Some(value) = value {
            options.push_str(&format!(" {name} {value}"));
        }
    }
    let script = format!(
        "set -o pipefail; ffmpeg -v error -nostdin -i {} -an -vf 'fps={fps},scale={width}:-2:flags=lanczos,format=yuv444p' -f yuv4mpegpipe - \
         | gifski --quiet --y4m-color-override bt709 {options} -o {} -",
        quote(render)?,
        quote(out)?
    );
    let mut cmd = Command::new("bash");
    cmd.args(["-c", &script]);
    tool(cmd, cancel)
}

/// Runs one export to the end.
pub fn export(
    job: &Job,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    cancel: &Cancel,
    progress: Progress<'_>,
) -> Result<Exported, ExportError> {
    let started = Instant::now();
    let sequence = job.project.sequences.first().ok_or(ExportError::Empty)?.clone();
    let length = sequence_frames(&sequence, SAMPLE_RATE);
    let range = job.range.map_or((0, length), |(a, b)| (a.clamp(0, length), b.clamp(0, length)));
    if range.1 <= range.0 {
        return Err(ExportError::Empty);
    }
    let frames = range.1 - range.0;
    let fps = sequence.rate.num() as f64 / sequence.rate.den() as f64;
    let seconds = frames as f64 / fps;
    let (size, cover) = match job.preset {
        // Upright: the sequence's frame with its sides swapped.
        Preset::Vertical if sequence.width > sequence.height => ((even(sequence.height), even(sequence.width)), true),
        _ => ((even(sequence.width), even(sequence.height)), false),
    };
    let exports = job.root.join("exports");
    let work = exports.join(format!(".tmp-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let result = (|| {
        if let Preset::Sound { format, stems, loudness } = job.preset {
            return export_sound(job, &sequence, range, (format, stems, loudness), &work, cancel, progress, started);
        }
        let has_sound = !matches!(job.preset, Preset::Gif { .. })
            && sequence.tracks.iter().any(|t| t.kind == sg_core::TrackKind::Audio && !t.clips.is_empty());
        let audio = work.join("audio.wav");
        if has_sound {
            progress("mixing sound", 0.0);
            render_audio(&job.root, &sequence, range, &audio, cancel)?;
        }
        let audio = has_sound.then_some(audio.as_path());
        // Masters are 10-bit; intermediates for the share copy and the GIF need not be.
        let ten_bit = matches!(job.preset, Preset::Master | Preset::Vertical);
        // Real 10-bit frames from the compositor cost twice the bytes on the
        // way to the encoder (4K: 34 instead of 57 frames per second). They
        // are worth it where the picture has values between the 8-bit steps
        // of its sources: where a scenario grades it. Otherwise the 8-bit
        // frame is widened on the GPU and nothing is lost.
        let graded = |s: &sg_core::Scenario| (s.look_on && !s.look.is_empty()) || (s.match_on && s.members.len() > 1);
        let deep = ten_bit && job.project.scenarios.values().any(|s| graded(s));
        let rendered = work.join("render.mp4");
        let mut encoder_name = "hevc_vaapi";
        let mut render_fps = 0.0;
        let mut stage_ms = [0.0; 4];
        let gpu_errors = sg_render::gpu_errors();
        for hardware in [true, false] {
            let mut encoder = Encoder::start(size, sequence.rate, audio, hardware, ten_bit, deep, &rendered)?;
            match render(job, &sequence, range, size, cover, deep, device, queue, &mut encoder, cancel, progress) {
                Ok(fps) => match encoder.finish() {
                    // A frame the GPU refused to draw is in the file as something else.
                    Ok(()) if sg_render::gpu_errors() != gpu_errors => return Err(ExportError::Gpu),
                    Ok(()) => {
                        (render_fps, stage_ms) = fps;
                        encoder_name = if hardware { "hevc_vaapi" } else { "libx264" };
                        break;
                    }
                    Err(e) if hardware => log::warn!("hardware encode failed ({e}); encoding in software"),
                    Err(e) => return Err(ExportError::Tool(e)),
                },
                // The encoder went away mid-stream: its error is the real one.
                Err(ExportError::Io(_)) => match encoder.finish() {
                    Err(e) if hardware => log::warn!("hardware encode failed ({e}); encoding in software"),
                    Err(e) => return Err(ExportError::Tool(e)),
                    Ok(()) => return Err(ExportError::Tool("the encoder stopped reading".into())),
                },
                Err(e) => {
                    encoder.kill();
                    return Err(e);
                }
            }
        }
        // A master is the same stream remuxed and is checked once, as the
        // final file; the others are made from this render, so it is checked.
        if matches!(job.preset, Preset::Share { .. } | Preset::Gif { .. }) {
            progress("checking", 0.0);
            verify(&rendered, frames, cancel)?;
        }
        let out = output_path(&job.root, &job.project, &job.preset);
        // A file that was started and not finished, or that failed its
        // check, does not stay beside the good ones.
        let unfinished = Unfinished(Some(out.clone()));
        match &job.preset {
            Preset::Master | Preset::Vertical => {
                if encoder_name == "hevc_vaapi" {
                    // Players on Apple devices want the hvc1 tag; a remux adds it safely.
                    let mut cmd = Command::new("ffmpeg");
                    cmd.args(["-v", "error", "-nostdin", "-y", "-i"]).arg(&rendered);
                    cmd.args(["-c", "copy", "-tag:v", "hvc1", "-movflags", "+faststart"]).arg(&out);
                    tool(cmd, cancel)?;
                } else {
                    std::fs::rename(&rendered, &out)?;
                }
            }
            Preset::Share { megabytes, compression } => {
                share_copy(&rendered, &out, *megabytes, *compression, seconds, fps, &work, cancel, progress)?
            }
            Preset::Gif { compression } => {
                progress("making GIF", 0.5);
                gif(&rendered, &out, *compression, cancel)?;
            }
            // (Left above: a sound export renders no picture.)
            Preset::Sound { .. } => {}
        }
        if !matches!(job.preset, Preset::Gif { .. }) {
            progress("checking", 0.5);
            // The share copy may have a lower frame rate; count what it should have.
            let expected = if matches!(job.preset, Preset::Share { .. }) { -1 } else { frames };
            if expected >= 0 {
                verify(&out, expected, cancel)?;
            } else {
                let mut decode = Command::new("ffmpeg");
                decode.args(["-v", "error", "-xerror", "-i"]).arg(&out).args(["-f", "null", "-"]);
                run_lines(decode, cancel, |_| {}).map_err(|e| ExportError::Verify(e.to_string()))?;
            }
        }
        let bytes = std::fs::metadata(&out)?.len();
        unfinished.keep();
        Ok(Exported {
            path: out,
            bytes,
            frames,
            seconds: started.elapsed().as_secs_f64(),
            render_fps,
            stage_ms,
            encoder: encoder_name,
            sound: None,
        })
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}
