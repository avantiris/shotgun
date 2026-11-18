# S1 Hardware decode to a wgpu texture

Date: 2026-11-18. Code: `spikes/s1-decode/` (throwaway). Media: `/disk/shotgun-dev/media/s1/`.
Build output: `/disk/shotgun-dev/target-s1`.

**Question.** What is the fastest working path to get hardware-decoded video
frames into a wgpu texture on the target machine, and what does it cost?

**Short answer.** Both zero-copy paths work, bit-exact against software decode:

- (a) VAAPI decode -> DRM PRIME DMA-BUF -> wgpu texture, using only wgpu-hal's
  own `texture_from_dmabuf_fd` (no raw `ash` needed).
- (b) FFmpeg Vulkan Video decode on wgpu's own `VkDevice`, wrapping the
  decoder's `VkImage` as a wgpu NV12/P010 texture.

Each plays 4K60 HEVC end to end (decode + import + YUV->RGB into an RGBA16F
4K target) at 240-280 frames/s single-stream, about 3.6-4.2 ms per frame.
The download/upload fallback (c) is 4x to 15x slower.

**But (b) is not usable on this machine today: Vulkan Video decode crashed
the amdgpu kernel driver (kernel oops) 12 times during this session**, in
roughly 100 process runs that used it. No VAAPI-only run did. Path (a) is the
recommendation. See "What failed" item 6 and **Decision** at the end.

**The kernel on this machine has been oopsing since 12:46 and is tainted;
reboot it before relying on further GPU work or measurements.**

## Measurement conditions (read this first)

- Machine: NixOS, Hyprland, RX 7800 XT (RADV NAVI32, Mesa 26.2.6, Vulkan
  1.4.354; VAAPI via radeonsi), 16 cores, 16 GB RAM, kernel 7.2.15-zen1.
- FFmpeg 9.0.2 (libavcodec 63.1.102, libavutil 61.1.102), Rust 1.99.1.
- **The machine was shared with other spikes for the whole session.** They
  ran a 16-thread CPU burner (`target-s3/release/burn 16`), multi-threaded
  `ffmpeg` encodes/VMAF runs and VAAPI encodes on the same GPU. The 1-minute
  load average was between 16 and 48 for almost every run below (it is noted
  per table). A short quieter window (load 4.2-6.0) existed around 12:53-12:55;
  numbers from it are marked "quiet". Consequences:
  - GPU-bound numbers (hw decode, zero-copy import) moved by roughly 20-30 %
    between quiet and loaded runs.
  - CPU-bound numbers (software decode, CPU upload) moved by up to 3x
    (4K HEVC software decode was measured at 41, 66, 75 and 139 frames/s in
    four runs of the same command).
  - Anything with a real-time deadline (paced multi-stream decode, window
    pacing) is dominated by scheduler stalls caused by the other jobs. Those
    results are reported as measured and marked **not verified on an idle
    machine**.
- From 12:46 on, the kernel had oopsed in the amdgpu VCN code (caused by this
  spike's Vulkan decode runs, see "What failed" item 6). The GPU kept working
  and results before and after look alike, but every number here was taken on
  a kernel in that state. The kernel log also shows a continuous stream of
  corrected PCIe link errors on bridge `00:03.1` (24 000 "AER: Correctable"
  lines since 12:25); whether they affect timing was not investigated.
- `scripts/quiet.sh` waits for the 1-minute load average to fall below a
  threshold and then runs the deadline-sensitive measurements (VAAPI and
  software only). It waited from about 14:11 to 14:23 for load < 5 and never
  started (load stayed between 22 and 32), so **no idle-machine re-run
  exists**. Run it when the machine is free:
  `nix develop --command spikes/s1-decode/scripts/quiet.sh`.
- All runs are single runs unless a range is given. Nothing is estimated.

## What was built

`spikes/s1-decode/` is a standalone Cargo project (binary `s1`):

| File | Content |
|------|---------|
| `src/dec.rs` | FFmpeg demux/decode over raw `ffmpeg-sys-next` FFI: software, VAAPI, Vulkan; frame-exact seek; `vaSyncSurface` / timeline-semaphore sync; download |
| `src/gpu.rs` | wgpu device creation through wgpu-hal (optionally with Vulkan Video extensions and a decode queue), FFmpeg Vulkan device on top of wgpu's `VkDevice`, the import paths, NV12/P010 -> RGB shader, headless benchmark, pixel verification |
| `src/play.rs` | winit + wgpu window player with pacing log |
| `src/main.rs` | CLI: `decode`, `seek`, `multi`, `import`, `verify`, `play` |
| `scripts/gen-media.sh` | test media generation |
| `scripts/bench.sh`, `scripts/key.sh`, `scripts/quiet.sh` | benchmark matrices |
| `bindings-test/*` | one tiny project per FFmpeg binding crate |
| `run.sh` | build + run wrapper (sets the target dir inside the dev shell) |

Every Vulkan decode mode (`vulkan`, `vkdec`, `cpu-vulkan`) is refused by the
binary and skipped by the scripts unless `S1_VULKAN=1` is set, because of the
kernel oops.

Commands (all from the repo root):

```sh
nix develop --command spikes/s1-decode/scripts/gen-media.sh        # media
spikes/s1-decode/run.sh decode /disk/shotgun-dev/media/s1/uhd60_hevc8.mp4 vaapi   # builds, then runs
nix develop --command spikes/s1-decode/scripts/bench.sh decode     # also: import | seek | multi | all
spikes/s1-decode/run.sh verify /disk/shotgun-dev/media/s1/uhd60_hevc8.mp4 37
spikes/s1-decode/run.sh play   /disk/shotgun-dev/media/s1/uhd60_hevc8.mp4 dmabuf secs=10
```

`s1` subcommands:

```
s1 decode <file> <sw|vaapi|vulkan> [download] [threads=N]
s1 seek   <file> <sw|vaapi|vulkan> [n=100] [threads=N] [download] [skipnonref]
s1 multi  <file> <sw|vaapi|vulkan> <streams> [download] [paced]
s1 import <file> <dmabuf|dmabuf-nocache|vkdec|cpu-vaapi|cpu-vulkan|cpu-sw> [frames=N]
s1 verify <file> <frame-index> [only=<path>]
s1 play   <file> <dmabuf|vkdec|cpu-vaapi|cpu-vulkan|cpu-sw> [secs=10]
```

## 1. FFmpeg Rust bindings against FFmpeg 9 / libavcodec 63

Each crate was built in its own project under `bindings-test/` with
`cargo build --release` inside the dev shell (pkg-config finds FFmpeg,
`LIBCLANG_PATH` is set for bindgen), then run against a test file.

| Crate | Version | Builds | Runs | Notes |
|-------|---------|--------|------|-------|
| `ffmpeg-next` + `ffmpeg-sys-next` | 9.1.0 / 9.1.1 | yes, no features, no patches (33 s clean) | yes: decoded 30 frames of the 4K 10-bit file, reports libavcodec 63 | sys crate also binds `hwcontext_drm.h` (`AVDRMFrameDescriptor`), `hwcontext_vaapi.h` and `hwcontext_vulkan.h` (`AVVulkanDeviceContext`, `AVVulkanFramesContext`, `AVVkFrame`) |
| `ffmpeg-the-third` + `ffmpeg-sys-the-third` | 6.0.0+ffmpeg-9.0 | yes, no features | yes: same test | binds `hwcontext_drm.h` but **not** the VAAPI or Vulkan hwcontext structs |
| `rusty_ffmpeg` | 0.17.0+ffmpeg.8.1 | yes with feature `link_system_ffmpeg` (without it: build script panics "No linking method set") | yes (`avcodec_version()` = 63) | raw bindings only; no DRM/VAAPI/Vulkan hwcontext headers (commented out in its `build.rs`) |
| `rsmpeg` | 0.18.0+ffmpeg.8.0 | **no**: 5 compile errors against FFmpeg 9 (`AVCodec.supported_samplerates` / `sample_fmts` are no longer fields, `av_parser_init` argument type) | - | pulls `rusty_ffmpeg 0.16.7+ffmpeg.8` |
| own bindgen sys crate | - | not needed, not built | - | fallback only if none of the above had worked |

The spike itself uses `ffmpeg-next = "9.1.0"` and calls `ffmpeg_next::ffi`
(= `ffmpeg-sys-next 9.1.1`) directly for everything on the decode path; the
safe wrapper has no API for `hw_device_ctx`, `get_format`, `av_hwframe_map` or
the hwcontext structs. Other crate versions used: `wgpu 30.0.2`
(`wgpu-hal 30.0.2`), `ash 0.38.0+1.3.281`, `winit 0.30.13`, `bindgen 0.73.2`
(build dependency of the sys crate).

Not verified: FFmpeg built from source through the crates' `build` features;
static linking; audio/encode APIs of any of the crates.

## 2. Test media

Generated by `scripts/gen-media.sh` (libx265/libx264 `-preset fast`,
`testsrc2` plus temporal noise `noise=alls=12:allf=t` so the encoder has real
work, 20 s each, BT.709 limited range tags, no audio).

| File | Codec / profile | Size, rate | Pixel format | Bitrate | GOP |
|------|-----------------|-----------|--------------|---------|-----|
| `uhd60_hevc8.mp4` | HEVC Main | 3840x2160, 60 fps | yuv420p | 45.5 Mb/s | 250, open GOP (x265 default) |
| `uhd60_hevc10.mp4` | HEVC Main 10 | 3840x2160, 60 fps | yuv420p10le | 45.5 Mb/s | 250 |
| `uhd60_hevc8_gop60.mp4` | HEVC Main | 3840x2160, 60 fps | yuv420p | 45.5 Mb/s | 60 (1 s) |
| `uhd30_h264.mp4` | H.264 High | 3840x2160, 30 fps | yuv420p | 55.1 Mb/s | 250 |
| `fhd60_h264.mp4` | H.264 High | 1920x1080, 60 fps | yuv420p | 12.0 Mb/s | 250 |
| `proxy540_h264_intra.mp4` | H.264 High, all intra (`-g 1 -crf 20`) | 960x540, 60 fps | yuv420p | 7.6 Mb/s | 1 |
| `proxy540_hevc_intra.mp4` | HEVC, x265 `keyint=1 -crf 22`; signalled as **RExt** even with `-profile:v main` | 960x540, 60 fps | yuv420p | 7.0 Mb/s | 1 |
| `proxy540_mjpeg.mov` | MJPEG `-q:v 4` | 960x540, 60 fps | yuvj420p (full range) | 13.4 Mb/s | 1 |
| `proxy1080_h264_intra.mp4` | H.264 High, all intra (`-g 1 -crf 20`) | 1920x1080, 60 fps | yuv420p | 24.9 Mb/s | 1 |

The proxy files are scaled from `uhd60_hevc8.mp4`. The noise makes them a
worst case for intra bitrate.

## 3. Decode benchmarks (no display)

`s1 decode <file> <mode> [download]`, whole file, wall-clock frames/s.
"on GPU" = frame stays in VRAM (every 8th frame and the last one are waited for
with `vaSyncSurface` / `vkWaitSemaphores`, so queued GPU work is inside the
timed region). "+download" = `av_hwframe_transfer_data` to system memory for
every frame. Software = FFmpeg frame threading, 16 threads. Hardware =
1 decoder thread. Run: `bench.sh decode`, 13:15-13:20, **load average 22-24**.

| File | Software | VAAPI on GPU | VAAPI + download | Vulkan on GPU | Vulkan + download |
|------|---------:|-------------:|-----------------:|--------------:|------------------:|
| `uhd60_hevc8` | 41.5 (41-139 across runs) | 269.5 | 33.6 | 229.0 | 144.5 |
| `uhd60_hevc10` | 48.0 | 285.2 | 21.3 | 271.2 | 127.6 |
| `uhd30_h264` | 89.6 | 329.2 | 40.9 | 306.3 | 241.1 |
| `fhd60_h264` | 378.9 | 1190.8 | 200.5 | 1082.7 | 564.9 |
| `proxy540_h264_intra` | 998.9 (380.8 with 1 thread) | 1814.8 | 335.5 | 1839.7 | 994.7 |
| `proxy540_hevc_intra` | 1309.1 (268.3 with 1 thread) | falls back to software (profile RExt); 1778.8 with `AV_HWACCEL_FLAG_ALLOW_PROFILE_MISMATCH` | - | falls back to software, also with the flag | - |
| `proxy540_mjpeg` | 478.8 and 727.6 (two runs, decoder uses 1 thread) | not tested | - | not offered by FFmpeg | - |
| `proxy1080_h264_intra` (load 37-48) | 82.6 with 1 thread | 489.6 | not run | not run | not run |

CPU time per frame for the hardware paths was 0.1-1.7 ms (one core);
software 4K decode used 44-137 ms of CPU per frame spread over 16 threads.

Findings:

- **VAAPI download is unusable at 4K: 21-41 frames/s**, 24-47 ms per frame,
  slower than software decode. FFmpeg's VAAPI transfer reads the mapped
  surface directly (root cause not profiled; the CPU time per frame, 10-26 ms,
  says it is a slow CPU read, not a GPU wait). Vulkan download of the same
  content is 4-6x faster (128-241 frames/s).
- On-GPU decode: VAAPI and Vulkan are within about 15 % of each other for a
  single stream; VAAPI was ahead in every 4K and 1080p pair.

## 4. Import into wgpu

### What works

| Path | Result |
|------|--------|
| (a) VAAPI -> DRM PRIME -> wgpu texture | **works**, via wgpu-hal only |
| (b) FFmpeg Vulkan decode sharing wgpu's `VkDevice` | **works when the kernel driver does not oops** ("What failed" item 6); needs wgpu-hal device creation callback plus about 40 lines of raw `ash` (one layout barrier) |
| (c) download + `queue.write_texture` | works; slow |

Pixel check: `s1 verify <file> 37` renders frame 37 through each path into an
RGBA8 target of the video's size, reads it back and compares it byte for byte
with the same frame decoded in software and uploaded.

| File | (a) dmabuf | (b) vkdec | (c) VAAPI download |
|------|-----------|-----------|--------------------|
| `uhd60_hevc8` | identical | identical | identical |
| `uhd60_hevc10` (P010) | identical | identical | identical |
| `uhd30_h264` | identical | identical | identical |
| `fhd60_h264` | identical | max diff 6, 0.016 % of bytes differ (bottom edge, see gotcha 8) | identical |
| `proxy540_h264_intra` | identical | identical | identical |

### (a) VAAPI -> DMA-BUF, how it is done

1. Decode with `hw_device_ctx` = VAAPI device (`/dev/dri/renderD128`),
   `get_format` returning `AV_PIX_FMT_VAAPI`.
2. `vaSyncSurface(display, surface)` (Vulkan does no implicit sync with the
   VAAPI decode).
3. `av_hwframe_map(drm, frame, AV_HWFRAME_MAP_READ)` with
   `drm->format = AV_PIX_FMT_DRM_PRIME` gives an `AVDRMFrameDescriptor`.
   On this machine: **1 object, 2 layers, modifier `0x200000020801b04`** (an
   AMD tiled modifier, not linear):
   - 4K NV12: `R8` plane offset 0 pitch 3840, `GR88` plane offset 8847360 pitch 4096
   - 4K P010: `R16` offset 0 pitch 7680, `GR32` offset 16711680 pitch 7680
   - 1080p NV12: `R8` pitch 2048, `GR88` offset 2621440 pitch 2048
4. For each plane: `dup()` the fd and call wgpu-hal
   `vulkan::Device::texture_from_dmabuf_fd(fd, desc, modifier, pitch, offset)`
   with format `R8Unorm` / `Rg8Unorm` (`R16Unorm` / `Rg16Unorm` for P010) and
   the plane's size, then `wgpu::Device::create_texture_from_hal::<Vulkan>(tex,
   desc, TextureUses::UNINITIALIZED)`. Requires
   `Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF` (and
   `TEXTURE_FORMAT_16BIT_NORM` for 10-bit).
5. The two plane textures go into a normal bind group; a WGSL shader does
   YUV -> RGB.
6. The imported textures are **cached per VA surface id** (the decoder cycles
   through a fixed pool: 15 surfaces for the 4K HEVC files, 13 for the H.264
   intra proxies), so after warm-up a frame costs one hash lookup.

No wgpu limitation blocked this and no raw `ash` was needed.
`texture_from_dmabuf_fd` only supports single-plane images, which is exactly
what the two-layer export needs. Not tried: importing as one multi-planar
`G8_B8R8_2PLANE_420` image; letting FFmpeg map VAAPI -> Vulkan
(`av_hwframe_map` to `AV_PIX_FMT_VULKAN`).

### (b) FFmpeg Vulkan decode on wgpu's device, how it is done

1. Create the device through wgpu-hal:
   `adapter.as_hal::<Vulkan>()` -> `open_with_callback(features, limits, hints,
   callback)`. In the callback add the extensions `VK_KHR_video_queue`,
   `VK_KHR_video_decode_queue`, `VK_KHR_video_decode_h264/h265/av1/vp9`,
   `VK_KHR_video_maintenance1/2` (those the driver has), a
   `DeviceQueueCreateInfo` for the decode queue family (family 2 on RADV,
   `queueCount = 1`) and `PhysicalDeviceSynchronization2Features` in the
   `pNext` chain. Then `adapter.create_device_from_hal(open_device, desc)`.
2. `av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_VULKAN)`, fill
   `AVVulkanDeviceContext` with wgpu's instance / physical device / device,
   `vkGetInstanceProcAddr`, the enabled instance and device extension lists,
   a feature chain (`timelineSemaphore`, `synchronization2`) and the queue
   families (`qf[0]` graphics, `qf[1]` decode with its `video_caps`), then
   `av_hwdevice_ctx_init`. FFmpeg accepted this and decoded H.264, HEVC 8-bit
   and HEVC 10-bit. (AV1 and VP9 not tested.)
3. Per frame, with the `AVVkFrame` locked (`lock_frame`): the image is one
   multi-planar `VkImage` (`G8_B8R8_2PLANE_420_UNORM` or the 10-bit
   equivalent, image flags `0x508`, concurrent sharing, layout
   `VIDEO_DECODE_DPB_KHR`, RADV reports "dedicated_dpb").
   - Submit a one-barrier command buffer on the graphics queue (raw `ash`)
     that waits on the frame's timeline semaphore `(sem, sem_value)`, moves the
     image to `SHADER_READ_ONLY_OPTIMAL` and signals `sem_value + 1`.
   - `queue.as_hal::<Vulkan>()`: `add_wait_semaphore(sem, Some(v + 1), ..)` and
     `add_signal_semaphore(sem, Some(v + 2))` for the next wgpu submit; write
     `sem_value = v + 2` and the new layout back into the `AVVkFrame`.
   - `hal_device.texture_from_raw(image, desc{format: NV12 | P010},
     Some(drop_callback), TextureMemory::External)` and
     `create_texture_from_hal(.., TextureUses::RESOURCE)`; the drop callback
     holds an `av_frame_clone` so the image outlives the wgpu texture.
   - Plane views: `TextureAspect::Plane0` as `R8Unorm`/`R16Unorm`,
     `Plane1` as `Rg8Unorm`/`Rg16Unorm`. Requires
     `Features::TEXTURE_FORMAT_NV12` / `TEXTURE_FORMAT_P010`.

No CPU wait is needed: the decode -> sample dependency is expressed with the
timeline semaphore. Not verified: Vulkan validation layers were not run; the
barrier out of a video-decode layout is issued on the graphics queue, which
works on RADV but I did not check it against the spec's valid-usage rules.

### End-to-end cost at 4K60

`s1 import <file> <path>`: decode -> import -> render YUV->RGB into a
3840x2160 `Rgba16Float` target -> submit; the device is polled to idle every
4 frames so the figure is real throughput. Whole file (1200 frames) unless
noted.

| Path | 4K60 HEVC 8-bit | 4K60 HEVC 10-bit | Conditions |
|------|----------------:|-----------------:|------------|
| (a) dmabuf, cached import | 267.2 fps / 3.74 ms (200 frames, load not recorded) | 241.2 fps / 4.15 ms (quiet) | see cells |
| (a) dmabuf, cached import | 190.8 fps / 5.24 ms | 196.9 fps / 5.08 ms | load 22 |
| (a) dmabuf, re-import every frame | 249.1 fps / 4.01 ms (200 frames) | - | load not recorded |
| (a) dmabuf, re-import every frame | 173.8 fps / 5.75 ms | 170.7 fps / 5.86 ms | load 22 |
| (b) vkdec | 278.6 and 274.9 fps / 3.59-3.64 ms | 280.2 fps / 3.57 ms | quiet (load 4.2-6.0) |
| (b) vkdec | 229.8 fps / 4.35 ms | 229.4 fps / 4.36 ms | load 22 |
| (c) Vulkan decode + download + `write_texture` | 164.4-168.6 fps / 5.9-6.1 ms (300 and 600 frames); 100.4-125.2 fps (1200 frames, 4 runs) | - | load not recorded (about 6) |
| (c) Vulkan decode + download + `write_texture` | 63.6 fps / 15.7 ms | 32.6 fps / 30.6 ms | load 22 |
| (c) software decode + `write_texture` | 73.3 fps / 13.6 ms | 30.5 fps / 32.7 ms | load 22 |
| (c) VAAPI decode + download + `write_texture` | 18.5 fps / 54.0 ms (200 frames, load not recorded); 16.5 fps / 60.7 ms (300 frames) | 10.8 fps / 92.2 ms (300 frames) | load 22 unless noted |

Per-frame breakdown of three of the runs above (ms):

| Path | decode call | import | render + submit + periodic wait | total |
|------|------------:|-------:|--------------------------------:|------:|
| (b) vkdec 8-bit | 3.01 | 0.08 | 0.50 | 3.59 |
| (a) dmabuf 10-bit, cached | 0.81 | 2.29 (almost all `vaSyncSurface`, i.e. waiting for the GPU decode) | 1.05 | 4.15 |
| (a) dmabuf 8-bit, re-import (200 frames) | 0.57 | 3.03 | 0.41 | 4.01 |

Other sizes (load 18-22): `fhd60_h264` dmabuf 846.5 fps, vkdec 882.4 fps,
software + upload 216.5 fps; `uhd30_h264` dmabuf 194.6 fps, vkdec 99.9 fps
(single outlier run, not repeated), software + upload 108.0 fps;
`proxy540_h264_intra` dmabuf 497.8 fps, vkdec 1123.4 fps, software + upload
417.1 fps; `proxy1080_h264_intra` (load 37-48) dmabuf 139.4 fps, software +
upload 170.7 fps.

`queue.write_texture` upload cost alone (the "import" column of the software
path, loaded machine): 4K 8-bit yuv420p 7.0-11.2 ms, 4K 10-bit 26.2 ms,
1080p 3.4-3.6 ms, 540p 1.1 ms per frame.

## 5. Window playback

`s1 play <4K60 HEVC file> <path> secs=10`: winit 0.30 window on Wayland,
`PresentMode::Fifo`, `Bgra8Unorm` surface. A decode thread feeds a bounded
queue (4 frames); the main thread picks the newest frame whose timestamp is
due, imports and draws it, and redraws on every refresh. The first second
(window map, two Hyprland resize events, first imports) is excluded; the
following 10 s are measured. Hyprland tiled the window to 952x1031 on a
1920x1080 **144 Hz** output, so a 60 fps video frame is shown for 2 or 3
refreshes (13.9 or 20.8 ms) by construction. Load average was about 23 and
another spike had its own window open.

| | (a) dmabuf | (b) vkdec |
|---|-----------:|----------:|
| video frames due / shown / dropped | 600 / 600 / 0 | 600 / 599 / 1 |
| decoder underruns | 0 | 0 |
| present interval (every swap) mean / p50 / p95 / p99 / max, ms | 7.00 / 6.94 / 10.73 / 14.23 / 23.36 | 6.96 / 6.97 / 9.02 / 9.86 / 28.94 |
| interval between new video frames mean / p50 / p95 / p99 / max, ms | 16.67 / 15.59 / 22.79 / 27.12 / 30.19 | 16.69 / 15.09 / 22.00 / 22.85 / 37.08 |
| new-frame intervals above 25 ms (more than 3 refreshes) | 10 | 1 |
| frame lateness when picked, p50 / p99 / max, ms | 3.50 / 13.00 / 16.21 | 3.65 / 8.43 / 8.94 |
| import cost on the render thread, mean / p99 / max, ms | 0.03 / 0.36 / 3.07 | 0.20 / 1.81 / 2.19 |

The picture was checked once by capturing only the spike window's rectangle
with `grim`: correct test pattern, right way up, plausible colours.

A second run of path (a) at 14:10 (load average 32, window tiled to 1276x691
on the **60 Hz** 2560x1440 output, where every missed refresh is a dropped
video frame) was much worse:

| | (a) dmabuf, 60 Hz output, load 32 |
|---|-----------:|
| video frames due / shown / dropped | 600 / 462 / 138 |
| decoder underruns | 6 |
| present interval mean / p50 / p95 / p99 / max, ms | 17.23 / 16.78 / 31.11 / 41.71 / 85.22 |
| interval between new video frames mean / p50 / p95 / p99 / max, ms | 21.63 / 19.61 / 37.10 / 49.64 / 85.34 |
| frame lateness when picked, p50 / p95 / max, ms | 8.86 / 62.33 / 95.46 |
| import cost on the render thread, mean / p99 / max, ms | 0.03 / 0.04 / 3.30 |

The mean present interval above the 16.67 ms refresh period means whole
refreshes were missed, with the render thread's own work at 0.03 ms per
frame. With 32 runnable threads on 16 cores I cannot tell from this run
whether the misses come from the spike's threads, the compositor or the
decoder being starved; the 6 underruns show the decoder was late at least
6 times. **Smooth 4K60 playback in a window is therefore demonstrated once
(144 Hz output, load 23) and contradicted once (60 Hz output, load 32); it
is not verified.** Re-run `s1 play` on an idle machine, on both outputs.

Not verified: pacing on an idle machine; a fullscreen 4K surface;
audio-clock-driven presentation (this player uses the wall clock).

## 6. Seek latency

`s1 seek <file> <mode> [skipnonref]`: 100 random target frames (fixed seed),
warm decoder. Time from the seek request to the target frame being fully
decoded on the GPU (`av_seek_frame` backward, `avcodec_flush_buffers`, decode
forward to the exact frame, `vaSyncSurface` / semaphore wait). Texture import
is not included; it adds 0.02-3 ms (section 4).

Long-GOP, hardware decode:

| File | Mode | p50 | p95 | max | frames decoded per seek p50 / p95 / max | Load |
|------|------|----:|----:|----:|---|---|
| `uhd60_hevc8` (GOP 250) | VAAPI | 414.6 ms | 771.9 ms | 837.8 ms | 119 / 225 / 246 | 18 |
| `uhd60_hevc8` (GOP 250) | VAAPI + `skipnonref` | 188.5 ms | 442.1 ms | 531.2 ms | 48 / 91 / 99 | 16 |
| `uhd60_hevc8` (GOP 250) | Vulkan | 603.0 ms | 1126.9 ms | 1307.1 ms | 119 / 225 / 246 | 18 |
| `uhd60_hevc8` (GOP 250) | software, 16 threads | 2346.9 ms | 5241.3 ms | 6468.1 ms | 119 / 225 / 246 | 18 |
| `uhd60_hevc10` (GOP 250) | VAAPI | 433.7 ms | 772.0 ms | 892.1 ms | 119 / 225 / 246 | 18 |
| `uhd30_h264` (GOP 250) | VAAPI | 461.6 ms | 943.1 ms | 1007.7 ms | 111 / 230 / 240 | 18 |
| `fhd60_h264` (GOP 250) | VAAPI | 103.7 ms | 190.3 ms | 211.3 ms | 119 / 225 / 246 | 18 |
| `uhd60_hevc8_gop60` (GOP 60) | VAAPI | 124.2 ms | 214.1 ms | 260.6 ms | 35 / 60 / 61 | 23 |
| `uhd60_hevc8_gop60` (GOP 60) | VAAPI + `skipnonref` | **56.7 ms** | **101.3 ms** | 112.1 ms | 15 / 25 / 26 | 23 |
| `uhd60_hevc8_gop60` (GOP 60) | Vulkan | 157.7 ms | 264.6 ms | 319.6 ms | 35 / 60 / 61 | 23 |

Roll-forward cost per decoded 4K frame: 3.4-3.8 ms with VAAPI, 4.8 ms with
Vulkan (5.3-5.8 ms in a run at load 27-38). `skipnonref` sets
`AVCodecContext.skip_frame = AVDISCARD_NONREF` for packets whose pts is before
the target, so non-reference B-frames are not decoded while rolling forward;
on these x265 streams it removes about 60 % of the frames.

All-intra proxies:

| File | Mode | p50 | p95 | max | Load |
|------|------|----:|----:|----:|---|
| `proxy540_h264_intra` | software, 1 thread | 2.6 ms | 6.9 ms | 13.9 ms | 18 |
| `proxy540_h264_intra` | software, 16 threads (frame threading) | 14.1 ms | 27.0 ms | 35.5 ms | 18 |
| `proxy540_h264_intra` | VAAPI | 1.2 ms | 2.2 ms | 6.6 ms | 18 |
| `proxy540_h264_intra` | Vulkan | 1.0 ms | 2.9 ms | 4.9 ms | 18 |
| `proxy540_h264_intra` | VAAPI + download | 4.1 ms | 10.1 ms | 15.6 ms | 18 |
| `proxy540_h264_intra` | Vulkan + download | 2.1 ms | 4.6 ms | 10.6 ms | 18 |
| `proxy540_hevc_intra` | software, 1 thread | 5.4 ms | 10.9 ms | 21.3 ms | 18 |
| `proxy540_hevc_intra` | software, 16 threads | 21.3 ms | 36.0 ms | 49.0 ms | 18 |
| `proxy540_mjpeg` | software, 1 thread | 0.7 ms and 1.1 ms (two runs) | 1.1 ms and 2.6 ms | 2.9 ms and 3.2 ms | 18 |
| `proxy1080_h264_intra` | software, 1 thread | 9.4 ms | 10.6 ms | 11.9 ms | 37-48 |
| `proxy1080_h264_intra` | software, 16 threads | 56.4 ms | 81.4 ms | 135.8 ms | 37-48 |
| `proxy1080_h264_intra` | VAAPI | 5.8 ms | 12.1 ms | 20.1 ms | 37-48 |
| `proxy1080_h264_intra` | Vulkan | 3.7 ms | 8.4 ms | 12.9 ms | 37-48 |

Against the budgets in `docs/spec/04-performance.md`:

- **Proxy scrub, 50 ms p95: met with a wide margin** by every all-intra
  candidate when decoded with one thread or in hardware (worst p95 12.1 ms,
  at 1080p, on a heavily loaded machine). Frame-threaded software decode of
  an intra file is the one configuration that misses or nearly misses it
  (27-81 ms p95): frame threading delays output by the thread count.
- **Full-quality seek, 150 ms p95: missed** for 4K long-GOP material with a
  250-frame GOP by 3-5x (442-772 ms p95 with VAAPI), and missed at a 60-frame
  GOP with plain decoding (214 ms). **Met at a 60-frame GOP with
  `skipnonref`** (101 ms p95). 1080p60 with a 250-frame GOP: 190 ms p95,
  missed.

## 7. Three simultaneous 4K60 HEVC decodes

`s1 multi <file> <mode> <streams> [paced]`: one decoder per thread sharing
one hw device; every frame is waited for on the GPU. Free-running numbers
(`bench.sh multi`, load 17-22):

| Streams | VAAPI aggregate fps (slowest stream) | Vulkan aggregate fps (slowest stream) |
|--------:|-------------------------------------:|--------------------------------------:|
| 1 (from section 3) | 269.5 | 229.0 |
| 3 | 424.9 (142.0) | 294.9 (98.4) |
| 4 | 561.6 (140.8) | 298.3 (74.7) |
| 6 | 445.5 (74.4; two streams ran at 139, four at 74.5) | 298.2 (49.8) |

- VAAPI: three 4K60 HEVC streams decode at **142 fps each, 2.4x the required
  60 fps**. Throughput keeps scaling to 4 streams (562 fps total). The
  6-stream split (2 streams at 139 fps, 4 at 74.5) is what two hardware
  decode instances with streams pinned 2 + 4 would look like; I did not
  confirm the instance count from the driver.
- Vulkan: total throughput is flat at about 298 fps for 3, 4 and 6 streams,
  so three streams get 98 fps each (1.6x of required). RADV exposes one
  decode queue (`queueCount = 1`) per device.

Paced (each stream sleeps to 60 fps and counts frames that were ready more
than one frame period late), 20 s:

| Run | Result |
|-----|--------|
| VAAPI 8-bit, load 18-25 | 60.0 fps per stream; 4, 5, 7 late frames of 1200; worst 51.9 ms |
| Vulkan 8-bit, load 18-25 | 60.0 fps; 3, 3, 3 late; worst 30.4 ms |
| VAAPI 10-bit, load 18-25 | 60.0 fps; 6, 7, 8 late; worst 69.3 ms |
| Vulkan 10-bit, load 18-25 | 60.0 fps; 9, 9, 9 late; worst 81.9 ms |
| VAAPI 8-bit, load 26-32 | 60.0 fps; 54, 55, 60 late; worst 48.0 ms |
| VAAPI 8-bit, load 27-38 | 59.9-60.0 fps; 281, 317, 325 late; worst 124.9 ms |
| Vulkan 8-bit, load 27-38 | 60.0 fps; 33, 39, 40 late; worst 178.1 ms |

Answer: **the average rate of 3 x 60 fps is sustained with 2.4x (VAAPI) or
1.6x (Vulkan) throughput headroom. "Zero late frames" was not demonstrated**:
every paced run had late frames, their number grew with the external load
(4 -> 60 -> 325 as load went 18 -> 30 -> 38) and they hit all three streams
at the same moments, which points at the other jobs on the machine rather
than at the decoder. This must be re-run on an idle machine
(`s1 multi <file> vaapi 3 paced`) before the "0 dropped frames" budget is
considered met. Three streams decoded *and* composited in one window were not
tested.

## What failed or surprised, and why

1. `rsmpeg 0.18.0` does not compile against FFmpeg 9 (section 1).
2. VAAPI `av_hwframe_transfer_data` at 4K: 21-41 fps. Do not use it on any
   hot path.
3. x265 all-intra output is signalled as the RExt profile (also with
   `-profile:v main`); VAAPI and Vulkan hwaccels then refuse it and FFmpeg
   silently falls back to software decode. `AV_HWACCEL_FLAG_ALLOW_PROFILE_MISMATCH`
   makes VAAPI accept it (1779 fps); the Vulkan hwaccel still refuses.
4. Vulkan decode images have the coded size (multiple of 16): 1920x1088 for
   1080p, 960x544 for 540p. The first version sampled the whole image and
   was wrong for every non-4K file (58 % of bytes differed). Fixed by sizing
   the texture from `AVHWFramesContext.width/height` and scaling UVs. VAAPI
   surfaces did not need this (the imported image is created with the picture
   size).
5. Frame-exact seek missed the target on the GOP-60 file (6 of the 100 seeks)
   until a retry was added: with open-GOP HEVC, `av_seek_frame(BACKWARD)` can
   land on a keyframe whose leading pictures (which the decoder drops after a
   seek) include the target. The fix is to detect "first output pts > target"
   and seek again further back.
6. **Kernel oops in amdgpu triggered by Vulkan Video decode.** What first
   looked like an intermittent hang of the spike binary is the kernel killing
   the thread that is inside the command-submission ioctl:

   ```
   BUG: unable to handle page fault for address: ffffcd470dca0000
   #PF: supervisor read access in kernel mode, not-present page
   Oops: Oops: 0000 [#5] SMP NOPTI
   CPU: 11 UID: 1000 PID: 89886 Comm: s1  7.2.15-zen1 #1-NixOS PREEMPT(full)
   RIP: 0010:vcn_v4_0_ring_patch_cs_in_place+0x91/0x470 [amdgpu]
   Call Trace: ttm_bo_kmap, amdgpu_cs_ioctl, drm_ioctl_kernel, drm_ioctl, amdgpu_drm_ioctl, __x64_sys_ioctl
   note: s1[89886] exited with irqs disabled
   ```

   - 12 oopses, all with this RIP, all in `s1`, at 12:46:19, 12:49:32,
     12:51:54, 12:53:58, 12:55:58, 12:59:48, 13:01:37, 13:04:13, 13:05:16,
     13:06:16, 14:02:17, 14:05:43 (`journalctl -k | grep -E "Oops|Comm:"`).
   - Each of the 12 coincides with a hung run of a command that was using
     FFmpeg's Vulkan hwaccel: on wgpu's shared device (path (b):
     `import .. vkdec` once, `verify` at the vkdec step at least 3 times) and
     on an FFmpeg-created device (`cpu-vulkan`, alone and inside `verify`).
     Roughly 100 process runs used Vulkan decode in total (not counted
     exactly), so about one run in eight oopsed. Runs on H.264, HEVC 8-bit
     and HEVC 10-bit files were all hit.
   - No VAAPI-only command (`decode/seek/multi .. vaapi`, `import .. dmabuf`,
     `play .. dmabuf`; well over 100 runs) hung. I did not check in the
     kernel source whether VAAPI decode jobs go through the faulting
     function too, so "VAAPI is immune" is an observation from these runs,
     not a proof.
   - Symptom in user space: the process never exits, `ps` shows the main
     thread as a zombie with two Mesa `disk$0` threads left, the expected
     output line is missing, there is no core dump; `kill -9` removes it.
   - Not analysed further (no kernel debugging, no bisect, not reported
     upstream, not tried on another kernel or Mesa version). The faulting
     address is page-aligned, which looks like a read just past a mapped
     buffer; that is a guess from the log, not a finding.
   - The kernel is tainted (`D`) since the first oops. Other spikes' GPU
     numbers after 12:46 were taken on that kernel.
7. The dev shell's `shellHook` exports `CARGO_TARGET_DIR=/disk/shotgun-dev/target`,
   overriding a value set before `nix develop`. My first builds went to the
   shared directory (another spike's artefacts are there too; I removed only
   my `s1` binary). `run.sh` now sets the variable inside the shell.
8. `uhd30_h264` through path (b) measured 99.9 fps in the loaded batch run
   while the same decoder alone did 306 fps; single outlier, not repeated,
   cause unknown (GPU video engine was shared with another spike's VAAPI
   encodes at the time).

## Decision

### (i) FFmpeg binding crate

**`ffmpeg-next = "9.1.0"` with `ffmpeg-sys-next = "9.1.1"`**, default
features, system FFmpeg found through pkg-config. It builds unpatched against
FFmpeg 9.0.2 and is the only candidate whose sys crate already binds the DRM,
VAAPI and Vulkan hwcontext structs the decode path needs. Use the safe
wrapper for probing/demux convenience where it fits and `ffmpeg_next::ffi`
for the decoder, hw device/frames contexts and frame mapping (the safe API
does not cover them). `ffmpeg-the-third 6.0.0+ffmpeg-9.0` is a working second
choice but would need hand-written VAAPI/Vulkan struct definitions. An own
bindgen crate is not needed.

### (ii) Decode -> texture path

**Primary: (a) VAAPI decode -> DRM PRIME -> `wgpu-hal`
`texture_from_dmabuf_fd` per plane -> `create_texture_from_hal`, imports
cached per VA surface, `vaSyncSurface` on the decode thread before the frame
is handed to the renderer.** Reasons, all measured here:

- Zero-copy, bit-exact, 8-bit and 10-bit; 241 fps (10-bit, quiet) and 267 fps
  (8-bit, 200 frames) end to end at 4K60, 0.03 ms mean import cost on the
  render thread once cached.
- Multi-stream capacity is about twice that of the Vulkan path
  (425-562 fps aggregate against 295-298), which is what the 3-layer budget
  plus read-ahead and scrubbing decoders will draw on.
- Faster roll-forward when seeking (3.4-3.8 against 4.8 ms per 4K frame).
- No raw `ash` and no custom device creation: a plain
  `adapter.request_device` with `VULKAN_EXTERNAL_MEMORY_DMA_BUF` (plus
  `TEXTURE_FORMAT_16BIT_NORM` for 10-bit) is enough. The final spike code
  does exactly that for path (a) and `s1 verify` is still bit-exact.
- It never crashed the kernel driver; the Vulkan decode path did, 12 times.

**Path (b) is rejected for now.** It is the simplest per-frame import
(0.08-0.26 ms, proper timeline-semaphore sync, no fds) and slightly faster
for a single stream (275-280 fps), but on this kernel and Mesa it
intermittently oopses amdgpu ("What failed" item 6), it is capped at about
298 fps total, and it needs the device-creation callback, a raw barrier and
coded-size cropping. Re-test it after a kernel update with
`S1_VULKAN=1 spikes/s1-decode/run.sh verify <file> 37` in a loop and
`journalctl -k | grep vcn_v4_0_ring_patch_cs_in_place`; the code in
`src/gpu.rs` is a working starting point. The architecture fallback in
`02-architecture.md` ("`ash` for the import step only") is not needed for
(a).

**Fallback (c)** for anything the hardware cannot decode: software decode +
`queue.write_texture`. It reached 73 fps (4K 8-bit) and 30 fps (4K 10-bit)
on the loaded machine, so 4K 10-bit software sources are below real time
with this upload method; a faster upload was not investigated. Never
download from VAAPI (21-41 fps at 4K), and do not use Vulkan decode +
download either while the kernel problem exists. If a hardware-decoded frame
is needed in system memory (thumbnails, export without a GPU encoder), read
it back from a wgpu texture after the GPU conversion; that readback was not
benchmarked here.

Design consequence not measured here: decoder surfaces come from a small
fixed pool (15 for 4K HEVC), so the VRAM frame cache cannot hold decoder
surfaces; cached frames must be copied or converted into textures the cache
owns (the YUV->RGB pass into RGBA16F already produces one).

### (iii) Proxy codec / resolution

- All-intra proxies meet the 50 ms p95 scrub budget with 4x to 20x margin at
  both 960x540 and 1920x1080, in hardware and in single-threaded software.
  **The seek numbers do not force a low proxy resolution: 1080p all-intra
  H.264 is inside the budget** (p95 8.4-12.1 ms on a loaded machine) at
  24.9 Mb/s for this worst-case noisy content, against 7.6 Mb/s at 540p.
- **H.264 High all-intra (`libx264 -g 1`)** is the candidate that works on
  every decode path here (VAAPI, Vulkan, software) and through both zero-copy
  imports. MJPEG has the fastest software seek (p95 1.1-2.6 ms) but twice the
  bitrate at 540p, full-range output, and no hardware path was tested.
  x265 all-intra is ruled out for hardware decode by the RExt profile issue.
- Full-quality seek on long-GOP 4K originals cannot meet 150 ms p95 by
  decoding from the previous keyframe when the GOP is long (250 frames:
  442-772 ms). It does meet it at a 1 s GOP with non-reference frame
  skipping (101 ms p95). So the 150 ms budget holds only for sources with a
  GOP of roughly 1 s or less; for longer GOPs the product needs the spec's
  other measures (decoder kept warm near the playhead, VRAM frame cache) or
  must show the proxy frame first and replace it when the original arrives.
  The final proxy codec choice belongs to S4; this spike only supplies the
  decode-side numbers.

### (iv) Gotchas for the implementer

1. **NixOS environment.** `LD_LIBRARY_PATH` must contain `vulkan-loader`,
   `libva`, `libdrm`, `wayland`, `libxkbcommon` (the dev shell sets it; wgpu,
   winit and FFmpeg's hwcontexts `dlopen` them). `LIBCLANG_PATH` is required
   to build `ffmpeg-sys-next` (bindgen at build time). Calling
   `vaSyncSurface` needs `#[link(name = "va")]` and `libva` in `buildInputs`.
   No `LIBVA_DRIVER_NAME` was needed. The VAAPI device is
   `/dev/dri/renderD128`. Set `CARGO_TARGET_DIR` inside the shell (failure 7).
   No change to `flake.nix` was necessary.
2. **Hardware decoder setup.** `hw_device_ctx` plus a `get_format` callback
   that returns the hw pixel format; if it is not offered FFmpeg silently
   decodes in software, so check `frame->format` (failure 3). Use
   `thread_count = 1` for hardware decoders. `av_hwframe_map` to DRM PRIME
   exports new fds on every call; `dup` what you keep and free the mapped
   frame.
3. **Thread counts.** Software long-GOP decode: `thread_count = 0` with
   frame threading. Software decode of all-intra proxies for scrubbing:
   **1 thread** (p95 6.9 ms against 27 ms at 540p, 10.6 against 81 ms at
   1080p). One decoder thread per hardware stream.
4. **Pixel formats.** VAAPI and Vulkan give NV12 for 8-bit and P010 for
   10-bit. P010 keeps the 10 bits in the high bits: sampled as `R16Unorm`
   the value must be scaled by 65535/65472. Software decode gives planar
   `yuv420p` / `yuv420p10le` (three planes; 10-bit values in the low bits,
   scale 65535/1023), MJPEG gives `yuvj420p`.
5. **DMA-BUF import.** Pass the modifier from the descriptor
   (`0x200000020801b04` here, not linear) and each plane's own offset and
   pitch; the chroma pitch is not derivable from the width (4096 for a
   1920-wide `GR88` plane). The import relies on RADV accepting single planes
   of a tiled NV12 buffer as `R8`/`RG88` images and on wgpu's first-use
   `UNDEFINED -> SHADER_READ_ONLY` transition keeping the contents; both held
   (bit-exact) on Mesa 26.2.6 and are not guaranteed elsewhere. Key the
   import cache by frames context and surface id; it was only exercised
   within one decoder session (not across flushes with a new frames context).
6. **Sync.** VAAPI: `vaSyncSurface` before the frame is sampled by Vulkan
   (always done here; the behaviour without it was not tested). Vulkan:
   hand the `AVVkFrame` timeline semaphore to wgpu with
   `Queue::as_hal().add_wait_semaphore`, and write the new `sem_value`,
   `layout` and `access` back under `lock_frame`.
7. **Frame lifetime.** Keep an `av_frame_clone` alive until the GPU has
   finished reading the surface (the spike holds the last 12 frames for
   path (a) and uses the texture drop callback for path (b)). Free FFmpeg's
   hw device context before the wgpu device when they share a `VkDevice`.
8. **Vulkan coded size.** Size the texture from
   `AVHWFramesContext.width/height`, crop with UVs, and clamp the UVs half a
   texel inside the picture: with a linear sampler the spike still differs
   by up to 6/255 on the last chroma row of 1080p because it samples into
   the padding.
9. **Colour.** The spike shader hard-codes BT.709 limited range and does no
   transfer function or chroma siting. The product must read
   `color_range`, `colorspace`, `color_primaries`, `color_trc` and
   `chroma_location` per frame (hardware frames carry them too) and treat
   MJPEG proxies as full range. Do not present display-referred values on an
   `Rgba16Float` surface: with the default colour space wgpu treats it as
   linear extended sRGB (the first non-sRGB surface format offered here was
   `Rgba16Float`; the spike picks `Bgra8Unorm` explicitly).
10. **Seeking.** Seek backward to a keyframe, flush, decode forward; verify
    the first output pts is not after the target and retry earlier if it is
    (open GOP, failure 5); set `skip_frame = AVDISCARD_NONREF` for packets
    before the target and back to default from the target packet on.
11. **wgpu-hal coupling.** `texture_from_dmabuf_fd`, `texture_from_raw`,
    `open_with_callback`, `add_wait_semaphore` and
    `create_texture_from_hal(.., initial_state)` are unsafe wgpu-hal APIs of
    wgpu 30.0.2 and may change between wgpu releases; `ash` must stay on the
    version wgpu-hal uses (0.38). `ffmpeg-sys-next` binds the Vulkan handle
    types as plain `u64` / opaque pointers (transmute to `ash` handles) and
    `AVVkFrameFlags` as a Rust enum (do not read combined flag values through
    it).
12. **winit.** The wgpu instance was created with the event loop's display
    handle (`InstanceDescriptor::new_with_display_handle`); creation without
    it was not tried. `current_monitor()` returned no refresh rate at window
    creation. Hyprland sends two resize events right after mapping.
13. **Vulkan Video decode and the kernel.** Do not ship or benchmark FFmpeg's
    Vulkan hwaccel (`AV_HWDEVICE_TYPE_VULKAN` decode, including "decode +
    download") on kernel 7.2.15-zen1 / Mesa 26.2.6 with this GPU: about one
    process run in eight oopsed amdgpu. If the product ever enables it, it
    needs a watchdog, because the failure mode is a thread that silently
    never returns from an ioctl, not an error code.
14. **Device creation.** Path (a) works on a device from plain
    `adapter.request_device` (verified bit-exact). Only path (b) needs
    `open_with_callback`.
