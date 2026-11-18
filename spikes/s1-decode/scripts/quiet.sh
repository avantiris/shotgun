#!/usr/bin/env bash
# Waits (max $1 seconds, default 1500) for the 1-minute load average to drop below $2 (default 4),
# then runs the realtime-sensitive measurements once. The machine is shared with other spikes.
#   nix develop --command spikes/s1-decode/scripts/quiet.sh
S=/disk/shotgun-dev/target-s1/release/s1
M=/disk/shotgun-dev/media/s1
deadline=$(( $(date +%s) + ${1:-1500} )); thr=${2:-4}
while :; do
  l=$(cut -d' ' -f1 /proc/loadavg)
  awk -v l=$l -v t=$thr 'BEGIN{exit !(l<t)}' && break
  [ $(date +%s) -ge $deadline ] && { echo "# never quiet: load $l"; exit 1; }
  sleep 10
done
load() { echo "# load: $(cut -d' ' -f1-3 /proc/loadavg)  $(date +%T)"; }
# Vulkan Video decode intermittently oopses the amdgpu kernel driver on this machine
# (docs/spikes/s1-decode.md, "What failed" 6), so those runs are opt-in: S1_VULKAN=1.
run() {
  if [[ "$*" =~ vulkan|vkdec ]] && [[ "${S1_VULKAN:-0}" != 1 ]]; then echo "SKIPPED (set S1_VULKAN=1): $*"; return; fi
  timeout -s KILL ${T:-120} $S "$@" 2>/dev/null | cut -c1-420
}
load
run multi $M/uhd60_hevc8.mp4 vaapi 3 paced
run multi $M/uhd60_hevc8.mp4 vulkan 3 paced
load
for p in dmabuf vkdec; do for f in uhd60_hevc8.mp4 uhd60_hevc10.mp4; do run import $M/$f $p; done; done
run import $M/uhd60_hevc8.mp4 cpu-vulkan
run import $M/uhd60_hevc8.mp4 cpu-sw
load
for f in uhd60_hevc8.mp4 uhd60_hevc10.mp4 uhd30_h264.mp4 fhd60_h264.mp4; do run decode $M/$f sw; run decode $M/$f vaapi; run decode $M/$f vulkan; done
load
run seek $M/uhd60_hevc8.mp4 vaapi
run seek $M/uhd60_hevc8.mp4 vaapi skipnonref
run seek $M/uhd60_hevc8_gop60.mp4 vaapi
run seek $M/uhd60_hevc8_gop60.mp4 vaapi skipnonref
run seek $M/uhd60_hevc8_gop60.mp4 vulkan
run seek $M/proxy540_h264_intra.mp4 sw threads=1
run seek $M/proxy540_h264_intra.mp4 vaapi
run seek $M/proxy540_mjpeg.mov sw threads=1
load
run multi $M/uhd60_hevc8.mp4 vaapi 3
run multi $M/uhd60_hevc8.mp4 vulkan 3
load
