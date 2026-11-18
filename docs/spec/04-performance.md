# 04 Performance

Speed is the first priority. This file says why editors get slow, what
Shotgun does about each cause, and the budgets every milestone is measured
against.

## Why Kdenlive, Resolve and similar get slow, and the countermeasure

| # | Bottleneck | Seen in | Shotgun's answer |
|---|------------|---------|------------------|
| 1 | Long-GOP decode on the interactive path: a backwards seek in H.264/HEVC means decoding from the previous keyframe, sometimes hundreds of frames | all | All-intra low-res proxies for scrubbing; warm decoder pool; read-ahead predictor; keyframe index built at import; decoded-frame cache in VRAM |
| 2 | CPU effects and CPU<->GPU copies per frame (MLT/frei0r; Resolve when a node falls back) | Kdenlive | Entire pipeline on the GPU from decode to display; zero-copy decode where possible (S1) |
| 3 | UI thread does I/O: thumbnails, waveforms, probing, project parsing | Kdenlive, Shotcut | UI thread never blocks (thread model in 02); all derived data is precomputed at import and memory-mapped |
| 4 | Timeline UI cost grows with clip count (one widget per clip, full relayout) | Kdenlive | Timeline is a GPU-instanced renderer over an interval index; only visible clips cost anything |
| 5 | Effects recomputed every frame, no reuse | all | Chains fused into few shader passes; static results cached; background render cache for segments that miss realtime, keyed by content hash |
| 6 | Save/load proportional to project size (large XML, full rewrite) | Kdenlive | Append-only journal; debounced atomic file writes off the UI thread; lazy loading of what is visible |
| 7 | Variable frame rate and awkward codecs cause slow, inaccurate seeks | all | Detected at import and conformed once |
| 8 | Live audio resampling/decoding, waveform generation while zooming | Kdenlive | Audio conformed to 48 kHz float at import; multi-resolution peak files |
| 9 | Mostly single-threaded frame graph | Kdenlive (MLT) | One decoder task per active stream in parallel; render and UI on separate threads |
| 10 | Memory pressure and swapping with many/large sources | all | Explicit byte budgets for caches (RAM and VRAM), eviction by distance from playhead |
| 11 | Undo implemented as state copies | several | Persistent data structure with structural sharing; ops with inverses |
| 12 | Startup loads everything (plugins, fonts, all media) | Resolve | Library view needs only poster frames; projects open lazily |

## Budgets (hard requirements on the target machine)

Machine: RX 7800 XT, 16 cores, 16 GB RAM, NVMe + SATA project drive.

| Metric | Budget |
|--------|--------|
| Cold start to interactive library | <= 300 ms |
| Open project (any size) to interactive timeline | <= 500 ms |
| UI frame time, any project size (10 or 50 000 clips) | <= 6.9 ms p99 (144 Hz) |
| Input to visible response (drag, playhead) | next frame |
| Scrub: seek to displayed frame (proxy) | <= 50 ms p95 |
| Scrub: seek to full-quality frame after pausing | <= 150 ms p95 for sources with GOP <= 1 s; long-GOP sources (S1 measured up to 772 ms) show the proxy frame meanwhile. `OPEN`: user to accept or ask for an intra mezzanine of originals |
| Play start latency (space to first frame + audio) | <= 80 ms |
| Realtime playback, 3 simultaneous 4K60 HEVC layers + colour chain | 0 dropped frames |
| Edit operation (split, trim, move) apply | <= 1 ms |
| Journal append | <= 1 ms |
| Asset usable after local drop (1 GB file) | <= 2 s (copy continues in background if needed) |
| Export 4K H.265 master | >= realtime where the encoder allows |
| Idle CPU with app open, not playing | ~0 % (no redraw when nothing changes) |
| RAM steady state, large project | <= 4 GB |

## How this is enforced

- `sg-bench` (in the workspace) holds reproducible benchmarks: synthetic
  50 000-clip project, seek storms, 3-layer 4K playback, journal throughput.
- Frame-time and seek-latency histograms are built into the app (toggle
  overlay), so the user's bug reports can include numbers.
- A milestone is not done until its relevant budgets are measured and recorded
  in `docs/perf-log.md`. A missed budget is reported as missed, with the number.
- Test media is generated or downloaded into `/disk/shotgun-dev/media`
  (never into the repo, never on the home disk).
