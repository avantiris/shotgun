# S8 Cutting a person out of a video on this machine

Date: 2026-11-22 (night). Question: can a person be cut out of a video
locally, without CUDA, well enough and fast enough to be a feature? Asked
for in feedback N2.5 ("AI to enhance the video, e.g. change background").

The survey of what exists (models, licences, runtimes, hosted services) is
`s8-ai-survey.md`. It was written by a research agent from web pages; what
it could not verify it marks as such, and none of its speed numbers are
from this machine. The numbers below are.

## What was run

- Model: Robust Video Matting, MobileNetV3, the authors' ONNX file
  (`rvm_mobilenetv3_fp32.onnx`, 15.0 MB, SHA-256 `88d45312...bd2828`) from
  their release page.
- Runtime: onnxruntime 1.27.1 from the flake's nixpkgs (CPU provider), in
  Python with numpy; ffmpeg decodes and encodes through pipes.
- Code: `spikes/s8-matte/rvm.py`. Command:
  `nix develop --command <python with onnxruntime> -I spikes/s8-matte/rvm.py <model> <in> <out> <ratio> <frames> <what> <threads>`.
- Machine: 16 threads, busy at the time (load average 9 to 11: the user's
  own Shotgun and builds were running).
- Clip: a phone video of a person waving a card at the camera, upscaled
  to 1920x1080 at 60 fps, strong motion blur: a hard case.

## Measured

| Picture | Inner ratio | Threads | Model alone | With decode and H.264 encode |
|---------|-------------|---------|-------------|------------------------------|
| 1920x1080 | 0.25 | all | 6.1 to 7.3 fps | 4.2 to 5.2 fps |
| 1920x1080 | 0.25 | 8 | 8.3 fps | 6.5 fps |
| 1920x1080 | 0.125 | 8 | 13.9 fps | 9.4 fps |
| 960x540 | 0.5 | all | 9.4 fps | 8.5 fps |
| 960x540 | 0.5 | 8 | 12.6 fps | 11.3 fps |
| 640x360 | 0.5 | all | 22.8 fps | 19.4 fps |

- The whole tool as built into the program (`sg-matte`, VP9 with
  transparency as output, 8 threads): 337 frames of 1080p in 85 s, 4 fps,
  while a build was running. So about fifteen times the length of a
  1080p60 clip, less for 30 fps or smaller pictures.
- Quality, by eye on single frames: the person, the hand and the card
  are kept through heavy blur; the background is gone; a faint line of
  the door frame stays beside the body in some frames. Hair on a busy
  wall was not tested (no such clip here).
- The lossless copy the editor plays transparent video from (utvideo,
  as for animated pictures) is 77 MB per second of 1080p60 (154 MB for
  the 2 s test). The file kept in the project is small (0.8 MB per second).

## Not measured

- Any GPU path (ncnn on Vulkan, onnxruntime with MIGraphX): not tried.
  The onnxruntime in the binary cache has no provider for an AMD card.
- The larger model (ResNet50) and newer ones (MatAnyone 2: better, but
  not free for commercial use and without a ready ONNX file).
- 4K sources. Rotated phone videos (handled in the tool, not tried).
- Whether playback keeps its budget while a cut-out is being made.

## Decision

Built as a first version (`PROPOSED`, see `docs/spec/10-ai.md`):
"Cut out the person (AI)" in an asset's menu makes a new asset, the video
with a transparent background, through a helper program on the CPU. It
reuses the import and playback of transparent animated pictures, so
preview and export need nothing new. The price is time (about fifteen
times the clip) and cache space (77 MB per second at 1080p60).

To do if he uses it: a GPU path; a matte kept beside the asset instead of
a second asset, which would save the space and allow edge controls; the
better model as a choice.
