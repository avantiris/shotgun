#!/usr/bin/env bash
# Key S1 numbers, each repeated (machine is shared with other spikes: compare repeats and the load line).
#   nix develop --command spikes/s1-decode/scripts/key.sh
S=/disk/shotgun-dev/target-s1/release/s1
M=/disk/shotgun-dev/media/s1
load() { echo "# load: $(cut -d' ' -f1-3 /proc/loadavg)  $(date +%T)"; }
# Vulkan Video decode intermittently oopses the amdgpu kernel driver on this machine
# (docs/spikes/s1-decode.md, "What failed" 6), so those runs are opt-in: S1_VULKAN=1.
run() {
  if [[ "$*" =~ vulkan|vkdec ]] && [[ "${S1_VULKAN:-0}" != 1 ]]; then echo "SKIPPED (set S1_VULKAN=1): $*"; return; fi
  timeout -s KILL ${T:-120} $S "$@" 2>/dev/null | cut -c1-420
}
for rep in 1 2 3; do
  echo "## repeat $rep"; load
  for p in dmabuf vkdec; do for f in uhd60_hevc8.mp4 uhd60_hevc10.mp4; do run import $M/$f $p; done; done
  run import $M/uhd60_hevc8.mp4 cpu-vulkan
  run decode $M/uhd60_hevc8.mp4 vaapi
  run decode $M/uhd60_hevc8.mp4 vulkan
  run decode $M/uhd60_hevc8.mp4 sw
  run multi $M/uhd60_hevc8.mp4 vaapi 3 paced | grep ^multi -B3
  run multi $M/uhd60_hevc8.mp4 vulkan 3 paced | grep ^multi -B3
  run seek $M/uhd60_hevc8.mp4 vaapi
  run seek $M/uhd60_hevc8.mp4 vaapi skipnonref
  run seek $M/uhd60_hevc8_gop60.mp4 vaapi
  run seek $M/uhd60_hevc8_gop60.mp4 vaapi skipnonref
  run seek $M/uhd60_hevc8_gop60.mp4 vulkan
  run seek $M/proxy540_h264_intra.mp4 sw threads=1
  run seek $M/proxy540_h264_intra.mp4 vaapi
  run seek $M/proxy540_mjpeg.mov sw threads=1
  load
done
