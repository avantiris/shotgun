# S10: a generative model for "change by a sentence" on this machine

2026-11-22, asked for in feedback F17.4 ("also spike / test the local
one"). Question: can an open model that repaints a clip by a sentence run
here (Radeon RX 7800 XT, 16 GB of video memory, no CUDA; 16 GB of memory;
NixOS) well enough to be offered beside the services?

## Result

**No, not as tried.** No video came out. The sampling itself works on
the card; the last step (turning the model's result into pictures) does
not: on the card it runs out of memory and then loses the device, on the
processor it would take about an hour and a half for two seconds of
video. The feature in the program does not depend on this: a local model
is one `command` entry among the models (`docs/spec/10-ai.md`), whenever
one works.

## What was tried

- Program: stable-diffusion.cpp with its Vulkan backend
  (`stable-diffusion-cpp-vulkan` of the flake's nixpkgs,
  `master-1043-3be2378`, from the binary cache; nothing was built).
- Model: Wan 2.1 VACE 1.3B (Apache 2.0), the smallest open model that
  takes a video as its guide. Files in `/disk/shotgun-dev/models/wan`
  (10.6 GB, of which the first is of no use):
  - `wan2.1-vace-1.3b-q8_0.gguf` (calcuis): **does not load** in this
    build ("tensor vace_patch_embedding.weight not in model metadata").
  - `wan2.1_vace_1.3B_fp16.safetensors` (Comfy-Org, 4.3 GB): loads.
  - `umt5-xxl-encoder-Q4_K_M.gguf` (city96, 3.7 GB), `wan_2.1_vae.safetensors`.
- Input: two seconds of his video (a copy), 29 frames of 832x480 at 16
  a second, with the sky painted grey by Shotgun's own sky mark, as the
  guide (`--control-video`); the sentence "dark dramatic sky with epic
  storm clouds".

| Step | Where | Time | Outcome |
|---|---|---|---|
| Reading the sentence (text encoder) | processor | 196 s | fine |
| Sampling, 20 steps | card (Vulkan) | 216 s | fine, 10.8 s a step |
| Result into pictures (VAE), whole | card | | out of memory: needs 18.2 GB, 14.2 free |
| The same in tiles of 416x240 | card | 42 s | **device lost**; the kernel reset the card's compute queue (`amdgpu ... ring comp_1.3.1 reset`). The desktop and his running Shotgun went on. |
| Second run: guide into the model's form (VAE), in tiles | processor, at most 9 GB | 600 s for each of 10 parts | stopped by me after two parts: an hour and a half before sampling would even begin |

## What this means

- About seven minutes for two seconds at 480p before the step that
  fails; a service does five to ten seconds at 1080p in about that time.
- The failing step is a fault of this build on this card, not a limit
  of the model: a newer stable-diffusion.cpp, or the small decoder it
  can use instead (`--tae`), may get through. Not tried: every try is
  another eight minutes with the card under load and may reset it again.
- This build cannot be told which part of the picture to leave alone
  (its mask for this model is fixed to "all of it"), so even a run that
  works repaints the person too. A sky that only replaces the sky needs
  the newer program (its script mode) or the PyTorch route.
- The PyTorch route (ComfyUI with ROCm) was not tried: several GB of
  wheels in an FHS environment, and reports of it stalling in 32 GB of
  memory, of which this machine has half.
- Quality could not be judged: there is no picture.

## Decision (`PROPOSED`)

Use a service for now. Try locally again when one of these holds: a
stable-diffusion.cpp in nixpkgs whose Vulkan decoder works on this card,
or more memory in the machine. The model files stay for that; they can
be deleted (`/disk/shotgun-dev/models/wan`).
