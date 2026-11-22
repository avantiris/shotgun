# AI enhancement for Shotgun: survey and recommendation

Date: 2026-11-22. Desk research only: nothing was downloaded, built or run
except `nix eval` queries against the flake's nixpkgs. No speed number below was
measured on this machine. "Not verified" means I found no source; "from memory"
means I know it but did not re-check it today.

## 1. What is realistic, ranked for a person filming himself

| # | Feature | Use for rap videos / talking to camera | Local on this machine? |
|---|---------|----------------------------------------|------------------------|
| 1 | Background removal (video matting) | Replace the room with a colour, picture, clip or blur; put text behind the person | Yes. Mature small models exist |
| 2 | Frame interpolation (slow motion) | Smooth slow motion from 30/60 fps footage; fits the existing speed ramps | Yes. RIFE runs on Vulkan |
| 3 | Upscaling | Low-resolution downloads (memes, yt-dlp), crops, 1080p into a 4K master | Yes, but slow (offline job) |
| 4 | Depth map (fake depth of field, fog, depth-based grade) | Phone footage looks like a large-sensor camera | Yes. Small model, permissive |
| 5 | Denoise / deblur / face restoration | Dark rooms, phone noise | Partly. Face models flicker and change the face; FFmpeg's classic denoisers cover most of the need |
| 6 | Object removal (inpainting) | Remove a mic stand, logo, person | Heavy, non-commercial licence, needs PyTorch on the GPU |
| 7 | Relighting | Match the person to a new background | Research code only; realistic only through an API |
| 8 | Generative edits (video-to-video, generated background/world) | "Put me on a rooftop at night" | Not well: 16 GB RAM and ROCm are the limit. API |

## 2. Open models that run without CUDA

Runtimes that exist for an AMD card on Linux:

- **onnxruntime, CPU** (also with the OpenVINO CPU provider). Always works. 16 threads.
- **onnxruntime + MIGraphX** (AMD's GPU provider, needs ROCm). AMD lists the
  RX 7800 XT (gfx1101) as a supported ROCm card
  (https://rocm.docs.amd.com/en/7.10.0-preview/about/release-notes.html). The
  provider has FP16 and a compiled-model cache
  (https://onnxruntime.ai/docs/execution-providers/MIGraphX-ExecutionProvider.html).
  Not verified on NixOS.
- **onnxruntime + WebGPU provider** (Dawn on Vulkan; build flag
  `onnxruntime_USE_WEBGPU`, also shipped as a plugin library)
  (https://onnxruntime.ai/docs/execution-providers/WebGPU-ExecutionProvider.html).
  No AMD Linux speed numbers found.
- **ncnn on Vulkan**. The proven AMD path for Real-ESRGAN and RIFE.
- **Rust**: `ort` 2.0.0-rc.12/13 wraps onnxruntime and has `ep::MIGraphX` and
  `ep::WebGPU` (https://docs.rs/crate/ort/latest/source/src/ep/migraphx.rs).
  `burn-onnx` turns an ONNX file into Rust code at build time and runs on wgpu
  (https://burn.dev/blog/release-burn-onnx-0.21.0/); operator coverage for
  these models is not verified. `wonnx` (wgpu) looks unmaintained: last release
  more than two years ago (https://packages.ecosyste.ms/registries/crates.io/packages/wonnx).
  `tract` has GPU only for CUDA and Metal (https://github.com/sonos/tract).
- **PyTorch + ROCm**: needed for the research models (MatAnyone, ProPainter,
  relighting, Wan). See section 3: not in the binary cache.

### Background removal

| Model | Licence | Size / input | ONNX or ncnn | Notes |
|-------|---------|--------------|--------------|-------|
| **RVM** (Robust Video Matting, 2021) | **GPL-3.0** (code; weights' licence not stated) | MobileNetV3 and ResNet50; any resolution, works internally at 1/4 (HD) or 1/8 (4K). File sizes not on the page (from memory: about 15 MB and 100 MB) | Official ONNX, FP32 and FP16, opset 12, tested on onnxruntime CPU and CUDA | Humans only. Recurrent, so stable over time. No mask needed. 104 fps HD on a GTX 1080 Ti, tensor throughput only (https://github.com/PeterL1n/RobustVideoMatting). CPU: one 2026 study measured about 10 fps in a PyTorch pipeline (https://arxiv.org/pdf/2611.14558; resolution not confirmed). Hair is soft, not strand-accurate (from memory) |
| **MatAnyone 2** (CVPR 2026) | **NTU S-Lab 1.0: non-commercial** | PyTorch checkpoint, size not stated. Needs a first-frame mask (SAM 2 click) | Community ONNX at 512 and 1024 only (https://huggingface.co/zerochocobo/matanyone2_onnx), not verified to work | Best open quality: hair, motion, real footage (https://arxiv.org/abs/2512.11782v1, https://github.com/pq-yang/MatAnyone2). VRAM at 1080p not published; the demo caps input at 1080p for memory |
| **MatAnyone-E** (Oct 2026) | Not stated | 73 fps at 1080p, 3.1 GB at 4K, on an H200 with compilation | None found | About 7x faster than MatAnyone 2 (https://arxiv.org/abs/2610.35724). Repo linked; weights release not verified |
| **BiRefNet** | MIT | 1024x1024 (general), 2048 (HR-matting, 932 MB ONNX); lite 224 MB | ONNX exports exist (https://huggingface.co/PinkPixel/birefnet-hr-matting-onnx) | Image model, any object. 96 ms/frame FP32 on an RTX 4090. Per-frame, so edges can flicker on video. Best permissive still-image quality |
| **SAM 2.1** | Apache-2.0 | tiny 38.9 M params to large | Community ONNX exporters (https://github.com/vietanhdev/samexporter); video memory export not verified | Tracks any clicked object through a video. Hard mask, no hair. Good as the "which object" step (https://github.com/facebookresearch/sam2). SAM 3 has Meta's own licence, commercial use allowed (https://roboflow.com/model-licenses/segment-anything-3) |
| **MODNet** | Apache-2.0 | 6.5 M params, 512x512 | ONNX (https://huggingface.co/Xenova/modnet) | Portraits, per-frame, older, flickers |
| **BackgroundMattingV2** | MIT | 4K capable | ONNX (from memory) | Needs a photo of the empty background: only for a locked-off camera |
| RMBG-2.0 | **CC BY-NC 4.0** | 977 MB | ONNX | BiRefNet architecture; no reason to prefer it |

### The rest

| Feature | Model | Licence | Runtime on AMD | Notes |
|---------|-------|---------|----------------|-------|
| Slow motion | **RIFE** 4.25/4.26 (4.25 is the recommended default, https://github.com/styler00dollar/VapourSynth-RIFE-ncnn-Vulkan) | MIT | ncnn Vulkan | About 90 fps at 1080p x2 on an RTX 3070 Ti, model version unknown (https://www.svp-team.com/forum/viewtopic.php?pid=80176). No AMD number found. Warps on fast hands and occlusions |
| Upscale | **Real-ESRGAN** `realesr-general-x4v3` (real footage), `animevideov3` (cartoons) | ncnn build MIT; main repo BSD-3 (from memory) | ncnn Vulkan | Per-frame, works on frame folders (https://github.com/xinntao/Real-ESRGAN-ncnn-vulkan). Seconds per frame at 4K output expected; not verified. Can look "plastic" on faces |
| Upscale, best quality | SeedVR2 | Not verified | PyTorch, large VRAM | Temporally consistent; realistic only via API (fal: $0.001 per megapixel) |
| Depth | **Depth Anything V2 Small** | Apache-2.0 (Base/Large are **CC BY-NC**) | onnxruntime; ONNX 99 MB, FP16 50 MB, input 518 px (https://huggingface.co/onnx-community/depth-anything-v2-small) | Per-frame, flickers slightly |
| Depth, stable | **Video Depth Anything Small** | Apache-2.0 (Base/Large **CC BY-NC**) | ONNX about 60 MB FP16 (https://huggingface.co/TheoreticallyTim/theoreticallypose-vda) | Authors claim 30 fps real time (https://arxiv.org/html/2501.12375v3) |
| Faces | GFPGAN | Apache-2.0, with third-party parts to check | ONNX exists | Per-frame: flicker and identity drift on video |
| Faces | CodeFormer | **S-Lab: non-commercial** | PyTorch | Better than GFPGAN, same flicker problem |
| Object removal | ProPainter | **S-Lab: non-commercial** | PyTorch + ROCm | VRAM at 1080p not verified (https://github.com/sczhou/ProPainter). DiffuEraser is Apache-2.0 but depends on ProPainter |
| Relighting | IC-Light, RelightVid, Relit-LiVE and others | Mixed, mostly not verified | PyTorch, diffusion | Research code. Not for a local first version |
| Generative | Wan 2.2 / VACE | Apache-2.0 (from memory) | PyTorch + ROCm | A 7800 XT user reports stalls and failures with 32 GB RAM (https://qiita.com/asfdrwe/items/0b040349203eab71963c). With 16 GB RAM: do not plan on it |

## 3. What nixpkgs has (checked with `nix eval` against the flake's lock)

| Package | Version | In binary cache | Finding |
|---------|---------|-----------------|---------|
| `onnxruntime`, `python3Packages.onnxruntime` | 1.27.1 | yes | Built with **CPU and OpenVINO** (CPU/GPU/NPU; the GPU part is for Intel). `USE_MIGRAPHX=FALSE`, `USE_CUDA=FALSE`. No WebGPU flag in the package. `rocmSupport = true` exists as an override and pulls in MIGraphX, but means building onnxruntime from source |
| `rocmPackages.migraphx`, `rocmPackages.clr` | 7.2.4 | yes | ROCm scopes for gfx1100/1101/1102 exist |
| `python3Packages.torchWithRocm` | 2.15.0 | **no** | Would be a local build of PyTorch: hours, and risky with 16 GB RAM |
| `ncnn` | 20250503 | yes | Built with `NCNN_VULKAN=1`. No Python binding |
| `realesrgan-ncnn-vulkan` | 0.2.0 | yes | MIT. Models are bundled in the package |
| `realcugan-ncnn-vulkan` | 20220728 | yes | Anime upscaler |
| `rife-ncnn-vulkan` | not packaged | | |
| `video2x` | 6.4.0 | yes | AGPL-3.0. One CLI that bundles the Real-ESRGAN, Real-CUGAN and **RIFE** ncnn-Vulkan libraries and reads/writes video itself |
| `rembg` | 2.0.86 | yes | MIT. Image background removal on onnxruntime; downloads models to the home directory on first use (home disk is nearly full: redirect it) |
| `python3Packages.sam2` | 1.1.0 | not checked | Needs torch |
| `openvino` | 2026.4.0 | not checked | |

## 4. Hosted APIs (the optional fallback)

All of these take an uploaded video and return a new video. What leaves the
machine in every case: the clip's pictures (and its sound unless it is removed
first), the text prompt and any reference pictures. Retention and training
terms were **not verified** for any provider; read them before the first upload.

| Provider | What it can do | Price | Source |
|----------|----------------|-------|--------|
| **Runway API** | `aleph2`: edit a video by prompt (change background, relight, remove or add things, restyle). Also resells Kling, Veo, Wan, Seedance through one key | Aleph 2.0: $0.28 per second, minimum $0.56. Veo 3.1: $0.20 to $0.40/s. Kling O3: $0.13 to $0.17/s | https://docs.dev.runwayml.com/guides/pricing/ |
| **fal.ai** | Many models behind one API. Kling O3 Pro (swap subject or environment, keeps motion, 3 to 15 s clips), Happy Horse 1.0 (background swap by prompt, up to 60 s), upscalers | Kling O3 Pro $0.168/s. Happy Horse $0.14/s (720p), $0.28/s (1080p). Bytedance upscaler $0.0072/s (1080p). Wan 2.2 video-to-video $0.04 to $0.08/s | https://fal.ai/learn/tools/best-video-to-video-apis-2026 |
| **Google (Gemini API)** | Veo 3.1: generate, extend. Not a general "edit my clip" tool | $0.40/s standard, $0.10 to $0.12/s Fast (sources disagree on Lite; endpoint retirement dates around now) | https://benchlm.ai/media-pricing/veo |
| Kling direct, Luma (Ray modify), Replicate | Video edit / modify; Replicate hosts open models (MatAnyone, ProPainter) | **Not verified** | |

Rough cost: a 10-second background change is about $1.50 to $3. A whole
3-minute video is $30 to $50 and most models limit a clip to 10 to 60 seconds.
So the API is for single shots, not whole timelines. Output is at most 1080p
on most models and the person's face can change slightly: it is a generated
video, not the original pixels.

## 5. Recommendation

### First feature: background removal, local, as a matte per asset

**Model: RVM (MobileNetV3, ResNet50 as "better").** It needs no click, it is
made for exactly one moving person, it is stable over time, it is small, and it
has official ONNX files that run on the onnxruntime already in nixpkgs. It is
five years old and its hair edges are soft. **MatAnyone 2 is clearly better**
but is non-commercial, needs a first-frame mask, and needs either PyTorch on
ROCm (not in the cache) or an unverified community ONNX export limited to
1024 px. Treat it as the "best quality" tier to add once the pipeline exists.

Licences to decide on: RVM is GPL-3.0. That is fine for a private program; it
matters only if Shotgun is ever distributed. MatAnyone 2's licence forbids
commercial use, which may include monetised music videos: the user's call.

**Runtime: a separate helper process (PROPOSED: `sg-matte`, Rust with `ort`,
linked to nixpkgs' onnxruntime), started from the job pool like ffmpeg.** CPU
first, because it works today. A crash or a GPU hang in inference then cannot
take the editor down, and it does not compete with the preview for the GPU.
GPU inference is the spike's question, not an assumption.

**Fit with the editor:**

1. "Remove background" on a clip starts a job for the asset, like "Stabilise":
   progress, cancel, stderr shown.
2. The job decodes with FFmpeg, runs the model, and writes a grey-scale matte
   video with the asset's exact frame count and timing, plus a proxy-size one.
   PROPOSED location: beside the stabilised copies (`matte/<id>.mp4`), not in
   `cache/`, because it costs minutes to make.
3. The clip gets a property "background: keep / remove" (a journal op with an
   inverse). `sg-render` samples the matte as a second texture and multiplies
   alpha. Preview and export share this, so rule 3 holds.
4. Replacing the background needs nothing new: whatever is on the track below
   shows through (colour, picture, clip). "Blur background" is the same clip
   twice: blurred below, matted above; offer it as one switch.
5. Small edge controls in the shader: shrink/grow, feather, spill removal.

**Risks:** hair and fast hands look cut out; a second person or a held object
flickers in and out; CPU speed may be only a few frames per second at 4K60
(a 3-minute 4K60 take is 10 800 frames); matte and picture drifting by a frame
on variable-frame-rate sources (use the conform as input); GPU inference
stalling playback; ROCm on this kernel/driver (the amdgpu driver already
crashed once here with Vulkan video decode).

**The 2-hour spike should measure** (own clips: hair against a busy wall, fast
hand movement, dark room, 4K60 phone):

- Frames per second for RVM MobileNetV3 and ResNet50, at 1080p and 4K, on
  onnxruntime CPU and on its OpenVINO CPU provider (both in the cache). Peak RAM.
- The same on one GPU path, time-boxed to 30 minutes: ncnn Vulkan (convert with
  pnnx) or onnxruntime with `rocmSupport = true`. Record "did not build" as a result.
- End to end: ffmpeg decode, model, matte encode, for one 60-second clip.
- Quality by eye on four exported frames per clip, and a 10-second composite
  over a flat colour to see flicker.
- Whether the editor's playback keeps its frame budget while the job runs.
- If time is left: the MatAnyone 2 ONNX export on the same clips, CPU, to see
  the quality gap and whether it runs at all.

### Second: slow motion with RIFE

Through `video2x` (in nixpkgs, cached, Vulkan) or a small ncnn tool. A
per-asset processed file at 2x or 4x frame rate, same pattern as stabilisation;
the speed ramp then reads the denser file. Spike: fps on this card, artefacts
on fast hand movement.

### Third: upscaling with Real-ESRGAN

Same tool, same pattern, mainly for low-resolution downloads. After that:
depth-based background blur with Video Depth Anything Small (Apache-2.0), which
reuses the matte pipeline (a second grey-scale video per asset).

### Shape of the optional API integration

- Off by default. A key in a local config file outside every project and
  outside git (PROPOSED: `~/.config/shotgun/ai.toml`, mode 600). No key, no menu entry.
- One explicit action per clip ("Send to ... "), never automatic, never in export.
- Before sending, state exactly: which frames (the used range only, at most
  1080p, sound removed), how many seconds, to which company, the estimated
  price. One confirm.
- Runs in the job pool with progress and cancel. The result is downloaded and
  imported as a normal asset in the bundle, labelled with provider, model and
  prompt. The timeline never depends on the service afterwards.
- One provider first. Runway (Aleph 2.0 for edits) or fal.ai (many models, one
  key) are the two candidates; fal.ai is cheaper per second and more flexible,
  Runway's Aleph is a single well-defined editing model.

## Decision (proposed, for the user)

Build local background removal first: RVM through onnxruntime in a helper
process, a matte file per asset, alpha in the renderer. Run the spike above
before committing to model and runtime. Then RIFE slow motion, then Real-ESRGAN
upscaling. Keep generative edits and relighting for an optional, per-clip,
clearly announced API action.
