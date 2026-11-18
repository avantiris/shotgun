//! The import pipeline (03-project-bundle.md, "Assets"):
//!
//! 1. fetch or copy into the bundle (temporary name, then atomic rename)
//! 2. hash + probe -> the asset is **usable** and reported to the UI
//! 3. poster, conformed audio, peaks, frame-rate scan
//! 4. conform, if the scan or the format asks for one
//! 5. proxy
//!
//! Steps 1 to 3 run on a few light workers so that several drops progress
//! side by side; 4 and 5 run on one heavy worker so that transcodes do not
//! fight each other for the GPU encoder and the CPU.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use crossbeam_channel::Sender;
use sg_core::text::slug;
use sg_core::{Asset, AssetId, AssetKind, Source};
use sg_media::derive;
use sg_media::scan::Scan;
use sg_media::tool::{Cancel, ToolError};

use crate::fetch::{self, Fetched};
use crate::layout::cache;
use crate::unix_now;

pub enum ImportSource {
    /// A file or folder on this machine (drop, file picker, pasted file).
    Path(PathBuf),
    Url(String),
    /// Pasted image data.
    Bytes {
        data: Vec<u8>,
        mime: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Derived {
    Poster,
    Audio,
    Peaks,
    Scan,
    Conform,
    Proxy,
}

pub enum ImportEvent {
    /// `fraction` is `None` while the size of the work is unknown.
    Progress {
        job: u64,
        label: String,
        stage: &'static str,
        fraction: Option<f32>,
    },
    /// The media file is in the bundle and described; the UI adds it to the project.
    Usable {
        job: u64,
        root: PathBuf,
        asset: Box<Asset>,
    },
    /// The same content is already in the project.
    Duplicate {
        job: u64,
        label: String,
        id: AssetId,
    },
    Derived {
        root: PathBuf,
        id: AssetId,
        what: Derived,
    },
    /// The media file of an indexed asset is not in the bundle.
    Missing {
        root: PathBuf,
        id: AssetId,
    },
    Failed {
        job: u64,
        label: String,
        error: String,
    },
    Finished {
        job: u64,
    },
}

type Task = Box<dyn FnOnce() + Send>;
type Known = Arc<BTreeMap<AssetId, Arc<Asset>>>;

struct Inner {
    light: Sender<Task>,
    heavy: Sender<Task>,
    events: Box<dyn Fn(ImportEvent) + Send + Sync>,
    next_job: AtomicU64,
    cancels: Mutex<HashMap<u64, Cancel>>,
}

pub struct Importer {
    inner: Arc<Inner>,
    threads: Vec<JoinHandle<()>>,
}

const LIGHT_WORKERS: usize = 3;

impl Importer {
    /// `events` is called from worker threads.
    pub fn new(events: impl Fn(ImportEvent) + Send + Sync + 'static) -> std::io::Result<Importer> {
        let (light, light_rx) = crossbeam_channel::unbounded::<Task>();
        let (heavy, heavy_rx) = crossbeam_channel::unbounded::<Task>();
        let mut threads = Vec::new();
        for (name, rx, count) in [("sg-import", light_rx, LIGHT_WORKERS), ("sg-transcode", heavy_rx, 1)] {
            for i in 0..count {
                let rx = rx.clone();
                threads.push(std::thread::Builder::new().name(format!("{name}-{i}")).spawn(move || {
                    for task in rx {
                        task();
                    }
                })?);
            }
        }
        let inner =
            Inner { light, heavy, events: Box::new(events), next_job: AtomicU64::new(1), cancels: Mutex::default() };
        Ok(Importer { inner: Arc::new(inner), threads })
    }

    /// Starts an import into the bundle at `root`. `known` is the project's
    /// current asset set, to recognise content that is already there.
    pub fn import(&self, root: PathBuf, known: Known, source: ImportSource) -> u64 {
        let (job, cancel) = self.inner.new_job();
        let inner = self.inner.clone();
        self.inner.light(move || inner.run_import(job, &root, &known, source, &cancel));
        job
    }

    /// Rebuilds missing cache files of a project's assets and reports assets
    /// whose media is gone. Called after opening a project.
    pub fn restore_cache(&self, root: PathBuf, assets: Known) -> u64 {
        let (job, cancel) = self.inner.new_job();
        let inner = self.inner.clone();
        self.inner.light(move || {
            for asset in assets.values() {
                if cancel.is_cancelled() {
                    break;
                }
                let origin = Path::new(&asset.source.origin);
                let present = root.join(&asset.file).is_file()
                    // The app was closed before the copy of a dropped file had
                    // finished: fetch it again from where it came from.
                    || (origin.is_absolute()
                        && std::fs::metadata(origin).is_ok_and(|m| m.len() == asset.size)
                        && inner.store(job, &root, asset, origin, &cancel).is_ok());
                if !present {
                    inner.emit(ImportEvent::Missing { root: root.clone(), id: asset.id });
                } else if !inner.derive_light(job, &root, asset, &cancel, false) {
                    break;
                } else {
                    inner.queue_heavy(job, &root, asset, &cancel, false);
                }
            }
            inner.finish_after_heavy(job);
        });
        job
    }

    pub fn cancel(&self, job: u64) {
        if let Some(cancel) = self.inner.cancels.lock().unwrap_or_else(PoisonError::into_inner).get(&job) {
            cancel.cancel();
        }
    }

    /// Cancels everything and waits for the workers. Call at shutdown.
    pub fn shutdown(self) {
        for cancel in self.inner.cancels.lock().unwrap_or_else(PoisonError::into_inner).values() {
            cancel.cancel();
        }
        let Importer { inner, threads } = self;
        // Workers stop when the last sender is gone; queued tasks hold
        // clones of `inner`, and finish quickly once cancelled.
        drop(inner);
        for thread in threads {
            let _ = thread.join();
        }
    }
}

/// A file ready to be taken into the bundle.
struct Staged {
    path: PathBuf,
    /// Already inside the bundle's staging folder: move it instead of copying.
    owned: bool,
    label: String,
    origin: String,
    uploader: Option<String>,
}

fn staging_dir(root: &Path, job: u64) -> PathBuf {
    root.join(format!("assets/.tmp-{job}-{}", std::process::id()))
}

fn seconds(asset: &Asset) -> f64 {
    // For progress display only.
    match (&asset.video, &asset.audio) {
        (Some(v), _) if !v.is_still() => v.frames as f64 * v.rate.den() as f64 / v.rate.num() as f64,
        (_, Some(a)) => a.samples as f64 / a.sample_rate as f64,
        _ => 0.0,
    }
}

impl Inner {
    fn emit(&self, event: ImportEvent) {
        (self.events)(event);
    }

    fn light(&self, task: impl FnOnce() + Send + 'static) {
        // Fails only during shutdown, when nobody waits for the result.
        let _ = self.light.send(Box::new(task));
    }

    fn new_job(&self) -> (u64, Cancel) {
        let job = self.next_job.fetch_add(1, Ordering::Relaxed);
        let cancel = Cancel::new();
        self.cancels.lock().unwrap_or_else(PoisonError::into_inner).insert(job, cancel.clone());
        (job, cancel)
    }

    fn progress(&self, job: u64, label: &str, stage: &'static str, fraction: Option<f32>) {
        self.emit(ImportEvent::Progress { job, label: label.to_owned(), stage, fraction });
    }

    fn fail(&self, job: u64, label: &str, error: &ToolError) {
        if !matches!(error, ToolError::Cancelled) {
            log::warn!("import of {label} failed: {error}");
            self.emit(ImportEvent::Failed { job, label: label.to_owned(), error: error.to_string() });
        }
    }

    fn run_import(self: &Arc<Self>, job: u64, root: &Path, known: &Known, source: ImportSource, cancel: &Cancel) {
        let staging = staging_dir(root, job);
        let label = match &source {
            ImportSource::Path(p) => {
                p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
            }
            ImportSource::Url(url) => url.clone(),
            ImportSource::Bytes { .. } => "pasted image".to_owned(),
        };
        match self.stage(job, &staging, source, &label, cancel) {
            Ok(staged) => {
                for item in staged {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let item_label = item.label.clone();
                    match self.ingest(job, root, known, item, cancel) {
                        Ok(Some(asset)) => {
                            if self.derive_light(job, root, &asset, cancel, true) {
                                self.queue_heavy(job, root, &asset, cancel, true);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => self.fail(job, &item_label, &e),
                    }
                }
            }
            Err(e) => self.fail(job, &label, &e),
        }
        let _ = std::fs::remove_dir_all(&staging);
        self.finish_after_heavy(job);
    }

    /// Reports the job finished once the heavy tasks queued before this call
    /// are done (the heavy worker runs them in order).
    fn finish_after_heavy(self: &Arc<Self>, job: u64) {
        let inner = self.clone();
        let _ = self.heavy.send(Box::new(move || {
            inner.cancels.lock().unwrap_or_else(PoisonError::into_inner).remove(&job);
            inner.emit(ImportEvent::Finished { job });
        }));
    }

    /// Step 1a: turn the source into local files.
    fn stage(
        &self,
        job: u64,
        staging: &Path,
        source: ImportSource,
        label: &str,
        cancel: &Cancel,
    ) -> Result<Vec<Staged>, ToolError> {
        match source {
            ImportSource::Path(path) => {
                let mut files = Vec::new();
                collect_files(&path, &mut files, 0)?;
                if files.is_empty() {
                    return Err(ToolError::other("nothing to import here"));
                }
                Ok(files
                    .into_iter()
                    .map(|path| Staged {
                        label: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                        origin: path.display().to_string(),
                        path,
                        owned: false,
                        uploader: None,
                    })
                    .collect())
            }
            ImportSource::Url(url) => {
                std::fs::create_dir_all(staging)?;
                self.progress(job, label, "downloading", None);
                let fetched =
                    fetch::fetch(&url, staging, cancel, &mut |f| self.progress(job, label, "downloading", f))?;
                Ok(fetched
                    .into_iter()
                    .map(|Fetched { path, label, uploader }| Staged {
                        path,
                        owned: true,
                        label,
                        origin: url.clone(),
                        uploader,
                    })
                    .collect())
            }
            ImportSource::Bytes { data, mime } => {
                std::fs::create_dir_all(staging)?;
                let ext = match mime.as_str() {
                    "image/png" => "png",
                    "image/jpeg" => "jpg",
                    "image/gif" => "gif",
                    "image/webp" => "webp",
                    _ => return Err(ToolError::other(format!("cannot import pasted data of type {mime}"))),
                };
                let path = staging.join(format!("pasted.{ext}"));
                std::fs::write(&path, data)?;
                Ok(vec![Staged {
                    path,
                    owned: true,
                    label: format!("Pasted {}", unix_now()),
                    origin: "clipboard".into(),
                    uploader: None,
                }])
            }
        }
    }

    /// Steps 1b and 2. The asset is reported usable as soon as it is hashed
    /// and probed, which only reads the source; a file from outside the
    /// bundle is copied in afterwards, so a large drop is usable before its
    /// copy has finished (04-performance.md). `Ok(None)` if the content is
    /// already in the project.
    fn ingest(
        &self,
        job: u64,
        root: &Path,
        known: &Known,
        staged: Staged,
        cancel: &Cancel,
    ) -> Result<Option<Arc<Asset>>, ToolError> {
        let Staged { path, owned, label, origin, uploader } = staged;
        self.progress(job, &label, "reading", Some(0.0));
        let (probed, hashed) = std::thread::scope(|scope| {
            let probe = scope.spawn(|| sg_media::probe(&path, cancel));
            let hashed = hash_file(&path, None, cancel, &mut |f| self.progress(job, &label, "reading", Some(f)));
            (probe.join().unwrap_or_else(|_| Err(ToolError::other("probe crashed"))), hashed)
        });
        let (hash, size) = hashed?;
        let probed = probed.map_err(|e| match e {
            ToolError::Failed { .. } | ToolError::Other(_) => {
                ToolError::other("this file is not video, audio or an image that can be read")
            }
            other => other,
        })?;
        let hex = hash.to_hex();
        let id = AssetId(u128::from_str_radix(&hex[..32], 16).unwrap_or(0));
        if known.contains_key(&id) {
            self.emit(ImportEvent::Duplicate { job, label, id });
            return Ok(None);
        }
        let label = probed.title.filter(|_| owned && label.len() < 4).unwrap_or(label);
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or_else(|| "bin".to_owned());
        let file = format!("assets/{}/{}-{}.{extension}", probed.kind.dir(), slug(&label), id.short());
        let asset = Arc::new(Asset {
            id,
            blake3: hex.to_string(),
            size,
            file,
            kind: probed.kind,
            label,
            tags: Vec::new(),
            source: Source { origin, imported: unix_now(), uploader },
            video: probed.video,
            audio: probed.audio,
        });
        if owned {
            // Downloads and pasted data are already inside the bundle.
            std::fs::rename(&path, root.join(&asset.file))?;
        }
        self.emit(ImportEvent::Usable { job, root: root.to_owned(), asset: Box::new(Asset::clone(&asset)) });
        if !owned {
            self.store(job, root, &asset, &path, cancel)?;
        }
        Ok(Some(asset))
    }

    /// Copies an outside file to its place in the bundle and checks that it
    /// is still the content that was hashed.
    fn store(&self, job: u64, root: &Path, asset: &Asset, from: &Path, cancel: &Cancel) -> Result<(), ToolError> {
        let part = root.join(format!("assets/.part-{job}-{}", asset.id.short()));
        let copied =
            hash_file(from, Some(&part), cancel, &mut |f| self.progress(job, &asset.label, "copying", Some(f)));
        let result = match copied {
            Ok((hash, _)) if hash.to_hex().as_str() == asset.blake3 => {
                std::fs::rename(&part, root.join(&asset.file)).map_err(ToolError::from)
            }
            Ok(_) => Err(ToolError::other("the file changed while it was being imported")),
            Err(e) => Err(e),
        };
        if result.is_err() {
            let _ = std::fs::remove_file(&part);
            self.emit(ImportEvent::Missing { root: root.to_owned(), id: asset.id });
        }
        result
    }

    /// Step 3. Skips what already exists. Returns false if cancelled.
    fn derive_light(&self, job: u64, root: &Path, asset: &Asset, cancel: &Cancel, fresh: bool) -> bool {
        let source = root.join(&asset.file);
        let id = asset.id;
        let total = seconds(asset);
        if fresh {
            // The copy went through the page cache; make it durable now that
            // the user no longer waits for it.
            if let Err(e) = std::fs::File::open(&source).and_then(|f| f.sync_all()) {
                log::warn!("cannot sync {}: {e}", source.display());
            }
        }
        let step = |what: Derived,
                    out: PathBuf,
                    stage: &'static str,
                    run: &mut dyn FnMut(&Path) -> Result<(), ToolError>|
         -> bool {
            if out.exists() {
                return true;
            }
            self.progress(job, &asset.label, stage, None);
            match run(&out) {
                Ok(()) => {
                    self.emit(ImportEvent::Derived { root: root.to_owned(), id, what });
                    true
                }
                Err(ToolError::Cancelled) => false,
                Err(e) => {
                    // A missing poster or waveform must not block the rest.
                    self.fail(job, &asset.label, &ToolError::other(format!("{stage} failed: {e}")));
                    true
                }
            }
        };
        let wav = cache::audio(root, id);
        if asset.audio.is_some() {
            let channels = asset.audio.as_ref().map_or(2, |a| a.channels);
            let ok = step(Derived::Audio, wav.clone(), "preparing audio", &mut |out| {
                derive::audio_wav(&source, out, channels, total, cancel, &mut |f| {
                    self.progress(job, &asset.label, "preparing audio", Some(f))
                })
            }) && step(Derived::Peaks, cache::peaks(root, id), "drawing waveform", &mut |out| {
                sg_media::peaks::from_wav(&wav, cancel)?.write(out).map_err(ToolError::from)
            });
            if !ok {
                return false;
            }
        }
        let poster_ok =
            step(Derived::Poster, cache::poster(root, id), "making thumbnail", &mut |out| match &asset.video {
                Some(v) if v.is_still() => derive::poster(&source, out, 0.0, cancel),
                Some(_) => derive::poster(&source, out, (total / 2.0).min(1.0), cancel),
                None => derive::wave_poster(&wav, out, cancel),
            });
        if !poster_ok {
            return false;
        }
        let moving = asset.kind == AssetKind::Video && asset.video.as_ref().is_some_and(|v| !v.is_still());
        if moving {
            return step(Derived::Scan, cache::scan(root, id), "checking frame rate", &mut |out| {
                let scan = sg_media::scan::scan(&source, cancel)?;
                std::fs::write(out, scan.to_line()).map_err(ToolError::from)
            });
        }
        true
    }

    /// Steps 4 and 5, on the heavy worker.
    fn queue_heavy(self: &Arc<Self>, job: u64, root: &Path, asset: &Asset, cancel: &Cancel, _fresh: bool) {
        let Some(video) = asset.video.clone().filter(|v| !v.is_still()) else { return };
        let animated_alpha = asset.kind == AssetKind::Image;
        let (inner, root, cancel, asset) = (self.clone(), root.to_owned(), cancel.clone(), asset.clone());
        let _ = self.heavy.send(Box::new(move || {
            let id = asset.id;
            let source = root.join(&asset.file);
            let total = seconds(&asset);
            let vfr = std::fs::read_to_string(cache::scan(&root, id))
                .ok()
                .and_then(|l| Scan::from_line(&l))
                .is_some_and(|s| s.vfr);
            let conform = cache::conform(&root, id, animated_alpha);
            let run = |what: Derived,
                       out: &Path,
                       stage: &'static str,
                       f: &mut dyn FnMut(derive::Progress<'_>) -> Result<(), ToolError>| {
                if out.exists() || cancel.is_cancelled() {
                    return;
                }
                inner.progress(job, &asset.label, stage, Some(0.0));
                match f(&mut |fraction| inner.progress(job, &asset.label, stage, Some(fraction))) {
                    Ok(()) => inner.emit(ImportEvent::Derived { root: root.clone(), id, what }),
                    Err(e) => inner.fail(job, &asset.label, &ToolError::other(format!("{stage} failed: {e}"))),
                }
            };
            if animated_alpha {
                // Animated pictures are small; the mezzanine doubles as the scrub source.
                let rate = video.rate;
                run(Derived::Conform, &conform, "converting", &mut |p| {
                    derive::conform_alpha(&source, &conform, rate, total, &cancel, p)
                });
                return;
            }
            if vfr {
                run(Derived::Conform, &conform, "fixing frame rate", &mut |p| {
                    derive::conform_video(&source, &conform, &video, total, &cancel, p)
                });
            }
            // A VFR source is proxied from its conform so both share one timing.
            let proxy_source = if conform.exists() { conform.clone() } else { source.clone() };
            let proxy = cache::proxy(&root, id);
            run(Derived::Proxy, &proxy, "making proxy", &mut |p| {
                derive::proxy(&proxy_source, &proxy, total, &cancel, p)
            });
        }));
    }
}

/// Files under `path`, depth first, skipping hidden entries.
fn collect_files(path: &Path, out: &mut Vec<PathBuf>, depth: u32) -> Result<(), ToolError> {
    let meta = std::fs::metadata(path).map_err(|e| ToolError::other(format!("{}: {e}", path.display())))?;
    if meta.is_file() {
        out.push(path.to_owned());
    } else if meta.is_dir() && depth < 8 {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)?.flatten().map(|e| e.path()).collect();
        entries.sort();
        for entry in entries {
            let hidden = entry.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if !hidden {
                collect_files(&entry, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

/// Hashes `source` and, if `copy_to` is given, writes it there in the same
/// pass. Not synced: the data sits in the page cache when this returns.
fn hash_file(
    source: &Path,
    copy_to: Option<&Path>,
    cancel: &Cancel,
    progress: &mut dyn FnMut(f32),
) -> Result<(blake3::Hash, u64), ToolError> {
    let mut input = std::fs::File::open(source)?;
    let total = input.metadata()?.len();
    let mut output = copy_to.map(std::fs::File::create).transpose()?;
    let mut hasher = blake3::Hasher::new();
    // Large blocks so that the hash can use every core on each of them.
    let mut buf = vec![0u8; 16 << 20];
    let mut done = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let mut filled = 0;
        while filled < buf.len() {
            match input.read(&mut buf[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        if filled == 0 {
            break;
        }
        let block = &buf[..filled];
        match &mut output {
            Some(file) => {
                let ((), written) = rayon_join(
                    || {
                        hasher.update_rayon(block);
                    },
                    || file.write_all(block),
                );
                written?;
            }
            None => {
                hasher.update_rayon(block);
            }
        }
        done += filled as u64;
        progress(if total == 0 { 1.0 } else { (done as f64 / total as f64) as f32 });
    }
    Ok((hasher.finalize(), done))
}

/// Runs two closures at the same time on scoped threads.
fn rayon_join<A: Send, B: Send>(a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B + Send) -> (A, B) {
    std::thread::scope(|scope| {
        let handle = scope.spawn(b);
        let ra = a();
        match handle.join() {
            Ok(rb) => (ra, rb),
            Err(panic) => std::panic::resume_unwind(panic),
        }
    })
}

/// Reads an asset's poster (raw RGBA) for the UI. Job thread only.
pub fn read_poster(root: &Path, id: AssetId) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(cache::poster(root, id))?;
    if bytes.len() == (derive::POSTER_W * derive::POSTER_H * 4) as usize {
        Ok(bytes)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "poster has the wrong size"))
    }
}
