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
    /// H.264 small enough to send: at most this many megabytes (10^6 bytes).
    Share { megabytes: u32 },
    /// Animated GIF, 480 pixels wide, 20 fps.
    Gif,
}

impl Preset {
    pub fn label(&self) -> &'static str {
        match self {
            Preset::Master => "master",
            Preset::Vertical => "vertical",
            Preset::Share { .. } => "share",
            Preset::Gif => "gif",
        }
    }

    fn extension(&self) -> &'static str {
        if *self == Preset::Gif { "gif" } else { "mp4" }
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
    let engine = Engine::start(Backend::Manual).map_err(|e| ExportError::Tool(e.to_string()))?;
    engine.set_mix(sg_play::audio_mix(root, sequence));
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
        for sample in &block[..frames * 2] {
            file.write_all(&sample.to_le_bytes())?;
        }
        done += frames as u64;
    }
    file.flush()?;
    Ok(())
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
    let mut decoders: HashMap<(AssetId, bool), VideoDecoder> = HashMap::new();
    // The source frame last handed on per asset: stills and held frames are sent once.
    let mut shown: HashMap<(AssetId, bool), i64> = HashMap::new();
    let mut spent = 0.0;
    for frame in range.0..range.1 {
        if cancel.is_cancelled() {
            return Err(ExportError::Cancelled);
        }
        let layers = plan(&job.project, sequence, frame);
        let mut fresh = Vec::with_capacity(layers.len());
        for layer in &layers {
            let Source::Asset { asset: id, frame: source_frame } = &layer.source else {
                fresh.push(None);
                continue;
            };
            let asset = job.project.assets.get(id);
            // A stabilised clip reads the stabilised copy of its asset.
            let key = (*id, layer.props.as_ref().is_some_and(|p| p.stabilise));
            let (Some(asset), true) = (asset, shown.get(&key) != Some(source_frame)) else {
                fresh.push(None);
                continue;
            };
            let failed = |e: sg_media::decode::DecodeError| ExportError::Tool(format!("{}: {e}", asset.label));
            if let std::collections::hash_map::Entry::Vacant(slot) = decoders.entry(key) {
                // Originals, or the constant-rate conform where one was needed.
                let conform = cache::conform(&job.root, *id, asset.kind == AssetKind::Image);
                let stable_copy = cache::stable(&job.root, *id);
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
    let mut pictures: HashMap<(AssetId, bool), (wgpu::TextureView, (u32, u32), i32)> = HashMap::new();
    let mut titles: HashMap<(String, u32, [u8; 4]), (wgpu::TextureView, (u32, u32))> = HashMap::new();
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
                for (index, (layer, fresh)) in layers.iter().zip(fresh).enumerate() {
                    match &layer.source {
                        Source::Title => {
                            let Some(title) = layer.props.as_ref().and_then(|p| p.title.as_ref()) else { continue };
                            let key =
                                (title.text.clone(), (title.size * size.1 as f32).round().max(4.0) as u32, title.color);
                            let entry = titles.entry(key.clone()).or_insert_with(|| {
                                let image = sg_render::text::rasterize(&key.0, key.1 as f32, key.2);
                                let texture = Compositor::upload(device, queue, image.width, image.height, &image.rgba);
                                (texture.create_view(&Default::default()), (image.width, image.height))
                            });
                            drawn.push((entry.0.clone(), entry.1, 0, true, index));
                        }
                        Source::Asset { asset: id, .. } => {
                            let stable = layer.props.as_ref().is_some_and(|p| p.stabilise);
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
                                pictures
                                    .insert((*id, stable), (view, (picture.width, picture.height), picture.rotation));
                            }
                            if let Some((view, picture_size, rotation)) = pictures.get(&(*id, stable)) {
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

/// Decodes the whole file once and counts its frames.
fn verify(path: &Path, expected_frames: i64, cancel: &Cancel) -> Result<(), ExportError> {
    let mut decode = Command::new("ffmpeg");
    decode.args(["-v", "error", "-xerror", "-i"]).arg(path).args(["-f", "null", "-"]);
    run_lines(decode, cancel, |_| {}).map_err(|e| match e {
        ToolError::Cancelled => ExportError::Cancelled,
        other => ExportError::Verify(other.to_string()),
    })?;
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

/// Two-pass H.264 under a size limit (S4, D6).
fn share_copy(
    render: &Path,
    out: &Path,
    megabytes: u32,
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
fn gif(render: &Path, out: &Path, cancel: &Cancel) -> Result<(), ExportError> {
    let quote = |p: &Path| -> Result<String, ExportError> {
        let text = p.to_string_lossy();
        // The paths are made by the app from slugs; refuse anything that could break the quoting.
        if text.contains('\'') {
            Err(ExportError::Tool("the project path contains a quote character".into()))
        } else {
            Ok(format!("'{text}'"))
        }
    };
    let script = format!(
        "set -o pipefail; ffmpeg -v error -nostdin -i {} -an -vf 'fps=20,scale=480:-2:flags=lanczos,format=yuv444p' -f yuv4mpegpipe - \
         | gifski --quiet --y4m-color-override bt709 --fps 20 --quality 90 -o {} -",
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
        let has_sound = job.preset != Preset::Gif
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
        for hardware in [true, false] {
            let mut encoder = Encoder::start(size, sequence.rate, audio, hardware, ten_bit, deep, &rendered)?;
            match render(job, &sequence, range, size, cover, deep, device, queue, &mut encoder, cancel, progress) {
                Ok(fps) => match encoder.finish() {
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
        if matches!(job.preset, Preset::Share { .. } | Preset::Gif) {
            progress("checking", 0.0);
            verify(&rendered, frames, cancel)?;
        }
        let out = output_path(&job.root, &job.project, &job.preset);
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
            Preset::Share { megabytes } => {
                share_copy(&rendered, &out, *megabytes, seconds, fps, &work, cancel, progress)?
            }
            Preset::Gif => {
                progress("making GIF", 0.5);
                gif(&rendered, &out, cancel)?;
            }
        }
        if job.preset != Preset::Gif {
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
        Ok(Exported {
            path: out,
            bytes,
            frames,
            seconds: started.elapsed().as_secs_f64(),
            render_fps,
            stage_ms,
            encoder: encoder_name,
        })
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}
