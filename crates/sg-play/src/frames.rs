//! Decoded frames for the preview: a pool of decode workers and a cache of
//! the textures made from what they return.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, Sender};
use sg_core::{Asset, AssetId, AssetKind};
use sg_media::decode::{VideoDecoder, VideoFrame};
use sg_project::cache;

/// Which file of an asset a frame comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Quality {
    /// The all-intra proxy: instant seeks, 540 lines.
    Proxy,
    /// The original (or its constant-rate conform).
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameKey {
    pub asset: AssetId,
    pub quality: Quality,
    pub frame: i64,
}

struct Request {
    key: FrameKey,
    root: PathBuf,
    asset: Arc<Asset>,
    epoch: u64,
}

pub struct Decoded {
    pub key: FrameKey,
    pub frame: Result<VideoFrame, String>,
}

/// Longest side of a preview frame per quality. Stills can be far larger
/// than any viewer; they are scaled at decode.
fn max_dim(quality: Quality) -> u32 {
    match quality {
        Quality::Proxy => 1920,
        Quality::Full => 4096,
    }
}

/// The file to decode for `quality`, and the thread count to open it with.
fn source(root: &Path, asset: &Asset, quality: Quality) -> (PathBuf, i32) {
    let conform = cache::conform(root, asset.id, asset.kind == AssetKind::Image);
    let proxy = cache::proxy(root, asset.id);
    if quality == Quality::Proxy && proxy.is_file() {
        // All-intra: one thread seeks 4 to 8 times faster than frame threading (S1).
        return (proxy, 1);
    }
    if conform.is_file() {
        // The lossless alpha mezzanine is intra too.
        return (conform, if asset.kind == AssetKind::Image { 1 } else { 0 });
    }
    (root.join(&asset.file), 0)
}

/// Decoders kept open per worker.
const OPEN_DECODERS: usize = 6;

fn worker(requests: Receiver<Request>, results: Sender<Decoded>, epoch: Arc<AtomicU64>) {
    // Most recently used last.
    let mut decoders: Vec<((AssetId, Quality), PathBuf, VideoDecoder)> = Vec::new();
    for request in requests {
        // A seek happened since this was asked for: nobody wants it any more.
        if request.epoch < epoch.load(Ordering::Relaxed) {
            continue;
        }
        let id = (request.key.asset, request.key.quality);
        let (path, threads) = source(&request.root, &request.asset, request.key.quality);
        // Full-quality frames of long-GOP video: roll forward on the GPU and
        // read back the one frame wanted. 4K HEVC with a 1 s GOP seeks in
        // about 115 ms this way against 390 ms in software.
        let hardware = request.key.quality == Quality::Full && threads == 0 && request.asset.kind == AssetKind::Video;
        // A proxy or conform may have appeared since the decoder was opened.
        decoders.retain(|(k, p, _)| *k != id || *p == path);
        let slot = match decoders.iter().position(|(k, _, _)| *k == id) {
            Some(i) => Ok(i),
            None => VideoDecoder::open_with(&path, threads, hardware).map(|mut decoder| {
                decoder.set_skip_nonref(hardware);
                if let Some(v) = &request.asset.video {
                    decoder.set_rate(v.rate.num(), v.rate.den());
                }
                decoder.set_max_dim(max_dim(request.key.quality));
                if decoders.len() >= OPEN_DECODERS {
                    decoders.remove(0);
                }
                decoders.push((id, path.clone(), decoder));
                decoders.len() - 1
            }),
        };
        let frame = match slot {
            Ok(i) => {
                let entry = decoders.remove(i);
                decoders.push(entry);
                let last = decoders.len() - 1;
                decoders[last].2.frame(request.key.frame).map_err(|e| e.to_string())
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        };
        if results.send(Decoded { key: request.key, frame }).is_err() {
            return;
        }
    }
}

pub struct Pool {
    workers: Vec<Sender<Request>>,
    epoch: Arc<AtomicU64>,
    /// Requests sent and not answered yet.
    pending: HashMap<FrameKey, u64>,
}

const WORKERS: usize = 4;

impl Pool {
    pub fn new(results: Sender<Decoded>) -> std::io::Result<Pool> {
        let epoch = Arc::new(AtomicU64::new(0));
        let mut workers = Vec::new();
        for i in 0..WORKERS {
            let (tx, rx) = crossbeam_channel::unbounded();
            let (results, epoch) = (results.clone(), epoch.clone());
            std::thread::Builder::new().name(format!("sg-decode-{i}")).spawn(move || worker(rx, results, epoch))?;
            workers.push(tx);
        }
        Ok(Pool { workers, epoch, pending: HashMap::new() })
    }

    /// Everything asked for so far is obsolete (the playhead jumped).
    pub fn new_epoch(&mut self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.pending.clear();
    }

    pub fn request(&mut self, key: FrameKey, root: &Path, asset: &Arc<Asset>) {
        let epoch = self.epoch.load(Ordering::Relaxed);
        if self.pending.get(&key) == Some(&epoch) {
            return;
        }
        self.pending.insert(key, epoch);
        // One asset and quality always goes to the same worker, so its
        // decoder stays warm and sequential requests decode sequentially.
        let lane = (key.asset.0 as usize ^ (key.asset.0 >> 64) as usize).wrapping_add(key.quality as usize)
            % self.workers.len();
        let _ = self.workers[lane].send(Request { key, root: root.to_owned(), asset: asset.clone(), epoch });
    }

    pub fn answered(&mut self, key: &FrameKey) {
        self.pending.remove(key);
    }
}

pub struct Cached {
    pub view: wgpu::TextureView,
    pub size: (u32, u32),
    pub rotation: i32,
    bytes: u64,
    used: u64,
    _texture: wgpu::Texture,
}

/// Textures of decoded frames, bounded by a byte budget (04-performance.md:
/// explicit budgets, eviction by least recent use).
pub struct TextureCache {
    entries: HashMap<FrameKey, Cached>,
    bytes: u64,
    budget: u64,
    clock: u64,
}

impl TextureCache {
    pub fn new(budget: u64) -> TextureCache {
        TextureCache { entries: HashMap::new(), bytes: 0, budget, clock: 0 }
    }

    pub fn contains(&self, key: &FrameKey) -> bool {
        self.entries.contains_key(key)
    }

    pub fn get(&mut self, key: &FrameKey) -> Option<&Cached> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|entry| {
            entry.used = clock;
            &*entry
        })
    }

    pub fn insert(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: FrameKey, frame: &VideoFrame) {
        let texture = sg_render::Compositor::upload(device, queue, frame.width, frame.height, &frame.rgba);
        let view = texture.create_view(&Default::default());
        let bytes = frame.rgba.len() as u64;
        self.clock += 1;
        if let Some(old) = self.entries.insert(
            key,
            Cached {
                view,
                size: (frame.width, frame.height),
                rotation: frame.rotation,
                bytes,
                used: self.clock,
                _texture: texture,
            },
        ) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.bytes > self.budget && self.entries.len() > 1 {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| *k) else { break };
            if let Some(evicted) = self.entries.remove(&oldest) {
                self.bytes -= evicted.bytes;
            }
        }
    }

    /// The cached frame of `asset` nearest below `frame`, for showing
    /// something while the exact frame is still being decoded.
    pub fn nearest_before(&self, asset: AssetId, frame: i64) -> Option<FrameKey> {
        self.entries
            .keys()
            .filter(|k| k.asset == asset && k.frame <= frame && frame - k.frame <= 8)
            .max_by_key(|k| (k.frame, k.quality == Quality::Proxy))
            .copied()
    }

    pub fn forget_asset(&mut self, asset: AssetId) {
        let bytes = &mut self.bytes;
        self.entries.retain(|k, e| {
            let keep = k.asset != asset;
            if !keep {
                *bytes -= e.bytes;
            }
            keep
        });
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}
