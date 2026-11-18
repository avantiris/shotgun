#!/usr/bin/env bash
# Runs the S1 benchmark matrix. Usage (repo root):
#   nix develop --command spikes/s1-decode/scripts/bench.sh [decode|import|seek|multi|all]
# Build first: spikes/s1-decode/run.sh decode <file> sw
S=/disk/shotgun-dev/target-s1/release/s1
M=/disk/shotgun-dev/media/s1
what=${1:-all}
load() { echo "# load: $(cut -d' ' -f1-3 /proc/loadavg)  $(date +%T)"; }
# Vulkan Video decode intermittently oopses the amdgpu kernel driver on this machine
# (docs/spikes/s1-decode.md, "What failed" 6), so those runs are opt-in: S1_VULKAN=1.
run() {
  if [[ "$*" =~ vulkan|vkdec ]] && [[ "${S1_VULKAN:-0}" != 1 ]]; then echo "SKIPPED (set S1_VULKAN=1): $*"; return; fi
  timeout -s KILL ${T:-120} $S "$@" 2>/dev/null | cut -c1-420
}

if [[ $what == decode || $what == all ]]; then
  echo "## decode"; load
  for f in uhd60_hevc8.mp4 uhd60_hevc10.mp4 uhd30_h264.mp4 fhd60_h264.mp4 proxy540_h264_intra.mp4 proxy540_hevc_intra.mp4 proxy540_mjpeg.mov; do
    run decode $M/$f sw
    [[ $f == proxy* ]] && run decode $M/$f sw threads=1
    [[ $f == proxy540_mjpeg.mov ]] && continue
    run decode $M/$f vaapi
    T=200 run decode $M/$f vaapi download
    run decode $M/$f vulkan
    run decode $M/$f vulkan download
  done
  echo "# hevc intra proxy with AV_HWACCEL_FLAG_ALLOW_PROFILE_MISMATCH"
  S1_ALLOW_PROFILE_MISMATCH=1 run decode $M/proxy540_hevc_intra.mp4 vaapi
  S1_ALLOW_PROFILE_MISMATCH=1 run decode $M/proxy540_hevc_intra.mp4 vulkan
  load
fi
if [[ $what == import || $what == all ]]; then
  echo "## import (decode -> wgpu texture -> YUV->RGB into RGBA16F target)"; load
  for f in uhd60_hevc8.mp4 uhd60_hevc10.mp4; do
    for p in dmabuf dmabuf-nocache vkdec cpu-vulkan cpu-sw; do run import $M/$f $p; done
    run import $M/$f cpu-vaapi frames=300
  done
  for f in uhd30_h264.mp4 fhd60_h264.mp4 proxy540_h264_intra.mp4; do
    for p in dmabuf vkdec cpu-sw; do run import $M/$f $p; done
  done
  run import $M/proxy540_mjpeg.mov cpu-sw
  load
fi
if [[ $what == seek || $what == all ]]; then
  echo "## seek (100 random targets, fixed seed)"; load
  run seek $M/uhd60_hevc8.mp4 vaapi
  run seek $M/uhd60_hevc8.mp4 vulkan
  T=300 run seek $M/uhd60_hevc8.mp4 sw
  run seek $M/uhd60_hevc10.mp4 vaapi
  run seek $M/uhd30_h264.mp4 vaapi
  run seek $M/fhd60_h264.mp4 vaapi
  for f in proxy540_h264_intra.mp4 proxy540_hevc_intra.mp4 proxy540_mjpeg.mov; do
    run seek $M/$f sw threads=1
    run seek $M/$f sw
  done
  run seek $M/proxy540_h264_intra.mp4 vaapi
  run seek $M/proxy540_h264_intra.mp4 vulkan
  run seek $M/proxy540_h264_intra.mp4 vaapi download
  run seek $M/proxy540_h264_intra.mp4 vulkan download
  load
fi
if [[ $what == multi || $what == all ]]; then
  echo "## multi-stream 4K60 HEVC"; load
  for mode in vaapi vulkan; do
    run multi $M/uhd60_hevc8.mp4 $mode 3 paced
    run multi $M/uhd60_hevc8.mp4 $mode 3
    run multi $M/uhd60_hevc8.mp4 $mode 4
    run multi $M/uhd60_hevc8.mp4 $mode 6
  done
  run multi $M/uhd60_hevc10.mp4 vaapi 3 paced
  run multi $M/uhd60_hevc10.mp4 vulkan 3 paced
  load
fi
