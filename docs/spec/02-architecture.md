# 02 Architecture

Rust workspace, one binary. Decisions marked `SPIKE Sx` are provisional until
the spike in `docs/spikes/` confirms them; the spike result wins over this text.

## Process and thread model

One process. Threads have fixed roles and never block each other:

| Thread(s)            | Owns                                   | May block on        |
|----------------------|----------------------------------------|---------------------|
| UI / main            | window, input, UI layout, draw submit  | nothing             |
| Render               | GPU compositing of the viewer frame    | GPU only            |
| Decode pool (N)      | demux + hardware/software decode       | disk, decoder       |
| Audio realtime       | audio graph `process()`                | **nothing, ever**   |
| Audio disk           | reading/decoding audio into ring bufs  | disk                |
| Job pool             | import, proxy, thumbnails, peaks, git  | anything            |
| Export               | offline render + encode                | anything            |

Rules:

- The UI thread does no file I/O, no decoding, no git, no waiting on locks held
  by workers. It reads immutable snapshots and sends commands.
- The audio realtime thread does not allocate, lock, log or make syscalls.
  Communication is via lock-free SPSC queues and atomically swapped graphs.
- Project state is an immutable persistent structure (`Arc` snapshots).
  Edits produce a new snapshot; readers keep theirs. No reader ever waits for
  an edit.

## Crates

```
crates/
  sg-core      Project model: sequences, tracks, clips, keyframes, scenarios.
               Pure data + edit operations. No I/O, no GPU. Heavily unit tested.
  sg-project   Bundle on disk: layout, journal (undo/redo), git snapshots,
               asset store, import pipeline orchestration.
  sg-media     FFmpeg: probe, demux, decode (VAAPI/Vulkan hw, software fallback),
               frame cache, proxies, thumbnails, audio peaks.
  sg-render    GPU compositor on wgpu. Given (snapshot, time, quality) produces
               a frame. Used identically by preview and export.
  sg-audio     Realtime engine: graph, mixer, insert chains, PipeWire backend,
               transport/clock, metering.
  sg-export    Offline render loop + encoders + GIF + clipboard.
  sg-ui        UI: library, timeline, viewer, inspector, key map.
  sg-app       Binary: wiring, startup, config.
```

Also in the workspace: `sg-bench` (benchmarks for the budgets in 04).

As built in M1:

- A track's clips are a sorted, non-overlapping store of shared chunks (up to
  512 clips each) with a per-chunk time offset (`sg-core/src/clips.rs`). An
  edit copies one chunk and a short vector of chunk references; a ripple
  changes offsets and copies nothing. Lookup by time is two binary searches.
  Clips are addressed by track and start position.
- The open project's `Document` (snapshot + undo history) lives on the UI
  thread, which applies edit ops itself: they are pure, in memory and take
  microseconds. Each record then goes through a channel to that project's
  writer thread (journal, saves). Create, open and close run on one serial
  project I/O thread, so a close has finished writing before a later open
  reads the same folder.
- `shotgun --script "<steps>"` drives the app through the same code paths as
  keys and clicks and can write off-screen screenshots (`shot:<file>`).
  Automated checks use this instead of injecting input.

Dependency direction: `sg-core` at the bottom, `sg-app` at the top. `sg-render`
and `sg-audio` do not depend on `sg-ui`. `sg-core` depends on nothing internal.

## Rendering

- wgpu on Vulkan. All compositing in **linear light, RGBA16F**.
- Frame pipeline for time `t`:
  1. Resolve active clips from the snapshot (interval lookup, O(log n)).
  2. Request frames from `sg-media` (cache hit, or decode).
  3. Per clip: input transform to working space, match correction, per-clip trim.
  4. Per scenario: look chain.
  5. Composite layers: transform, crop, opacity, blend, transitions, titles.
  6. Output transform to display (preview) or delivery space (export).
- Effects are WGSL passes. A chain of simple modules is fused into as few
  passes as possible; a module is a pure function of pixel + parameters where
  it can be, so colour chains compile into one shader.
- Decode to GPU, decided by spike S1 (`docs/spikes/s1-decode.md`, read its
  gotcha list before writing `sg-media`): VAAPI decode -> DRM PRIME DMA-BUF ->
  wgpu-hal `texture_from_dmabuf_fd` per plane -> `create_texture_from_hal`,
  cached per VA surface, `vaSyncSurface` on the decode thread, NV12/P010 to
  RGB in a shader. Zero-copy, bit-exact against software decode, no raw `ash`.
  Measured 241 fps end to end for 4K60 10-bit HEVC (4.15 ms per frame).
- **FFmpeg Vulkan video decode is forbidden.** It works and is slightly
  faster per stream, but it crashed the amdgpu kernel driver
  (`vcn_v4_0_ring_patch_cs_in_place`) in about one of eight runs on this
  machine. Do not enable it, not even for a test, without the user's consent.
- Never download VAAPI frames to the CPU (16-34 fps at 4K). Codecs the
  hardware cannot decode use software decode + `write_texture`.
- FFmpeg bindings: `ffmpeg-next` 9.1 / `ffmpeg-sys-next` 9.1 (the only crate
  that binds the VAAPI/DRM hwcontext structs for FFmpeg 9).
- As built in M3 (differs from the plan above on purpose, see 08): frames
  reach the compositor as 8-bit RGBA textures uploaded from system memory.
  Proxies and stills are decoded in software; full-quality frames of video
  are decoded through VAAPI and only the wanted frame is read back. The
  zero-copy DMA-BUF import above replaces this for full-resolution playback
  in M5. The compositor already works in linear light in `Rgba16Float`; the
  input transform is the sRGB curve for every source until 05 is built.
- The preview engine (`sg-play`) is the render thread: it reads the audio
  engine's heard position, plans the layers at that frame, asks a pool of
  four decode workers for missing pictures (one asset and quality always on
  the same worker, so its decoder stays warm), keeps decoded textures in a
  768 MB least-recently-used cache, composites, and wakes the UI. During
  playback and while a frame is missing it shows proxies; a paused frame is
  replaced by the full-quality one as soon as that is decoded.
- Preview quality is adaptive (proxy source and/or reduced viewer resolution
  under load); export always uses originals at full resolution.
- Export (M6) runs on its own thread as three stages side by side: plan and
  decode (software, sequential, full size), composite on the GPU, feed the
  encoder process. 8-bit 4:2:0 pictures cross to the GPU as planes and are
  converted there; the finished frame is read back as NV12 and piped to
  `ffmpeg`, which uploads it to the GPU encoder. Sound is rendered offline
  by the audio engine (manual backend) with the mix the preview plays.
  `PROPOSED`: for a variable-frame-rate source export reads its
  constant-rate conform, not the original, because frame numbers only mean
  the same thing in both for constant-rate files.

## Media

- Decoders are pooled per asset and kept warm near the playhead; a predictor
  reads ahead in play direction and around the playhead while scrubbing.
- Frame cache with a byte budget (VRAM for decoded textures, RAM for audio).
- Proxies: all-intra H.264 files generated at import for instant scrubbing
  (S1: seek p95 2-12 ms up to 1080p, so 1080p proxies are affordable; decode
  intra files with one thread or VAAPI, frame threading is 4-8x slower per
  seek). Originals are untouched and used for export.
- Full-quality seeks into long-GOP originals are slow by nature (S1: p95
  772 ms at GOP 250, 214 ms at GOP 60; 442 / 101 ms when non-reference frames
  are skipped). The paused frame therefore shows the proxy instantly and is
  replaced by the original when it arrives; open-GOP HEVC needs a seek retry.
- Variable-frame-rate sources, GIFs, image sequences and other formats that
  cannot be seeked accurately are **conformed** at import to a constant-rate,
  visually lossless mezzanine; the original stays in the bundle as source.
- Audio of every asset is conformed at import to 48 kHz float (FLAC or WAV in
  the cache) with a multi-resolution peak file, so the timeline never decodes
  compressed audio or computes waveforms live.

## Audio

See `06-audio.md`. Audio is the master clock; video frames are presented
against the audio engine's sample position.

## UI

- Decided by spike S2 (`docs/spikes/s2-ui.md`): winit 0.30 + wgpu 30 + egui
  0.37 for panels and widgets; the timeline is our own instanced wgpu pipeline
  inside an egui paint callback. Measured at 144 Hz: 2.2-3.2 ms CPU p99 with
  50 000 clips, against 12-13 ms when egui's painter draws the clips.
- Timeline renderer: per-track sorted start/end arrays, binary search for the
  first visible clip, clips narrower than 2 px merged into one quad per 2 px
  bucket (caps the draw at ~17 000 quads at any project size), one buffer
  upload, one instanced draw. Labels use egui text, not glyphon.
- Redraw is event-driven (`ControlFlow::Wait`). Playback ticks come from other
  threads through `EventLoopProxy`, never through
  `egui::Context::request_repaint()` (it doubles the redraws).
- File drag-and-drop and import paste: winit 0.30 has no Wayland DnD, so a
  second `wayland-client` event queue on winit's `wl_display` handles the data
  device. All wayland-client objects must be dropped before the winit event
  loop (otherwise the app crashes on exit).
- Keys are bound by **physical position** (winit `KeyCode`, which uses
  US-position names), so the Swiss layout and any other layout behave
  identically. The Swiss `Y` key cap is `KeyCode::KeyZ`.
- Wayland native. Client-side decorations are minimal; Hyprland tiles the
  window.

## External tools

Used as subprocesses by the job pool, never on a hot path:

- `yt-dlp` for link downloads.
- `git` (and `git-lfs` if spike S5 says so) for snapshots.
- `gifski` for high-quality GIF encoding.
- `wl-copy` for clipboard export (or the Wayland data-device protocol directly).

FFmpeg is linked as a library for decode/encode on the playback and export
paths. Import (M2) runs the `ffprobe` and `ffmpeg` programs with the command
lines measured in S4: probe, frame-rate scan, poster, audio conform, video
conform and proxy are one-off jobs on the job pool where a process start does
not matter, and the spike numbers apply unchanged. Elsewhere the binary is only a
fallback and a verification tool.

## Why Rust + wgpu

Agreed with the user. Safe multithreading for the thread model above, a
realtime-capable audio engine without a GC, direct FFmpeg/PipeWire bindings,
and wgpu gives Vulkan performance with far less code than raw Vulkan. If S1
shows wgpu blocks zero-copy decode, the fallback is `ash` (raw Vulkan) for the
decode-import path only, sharing the device with wgpu.
