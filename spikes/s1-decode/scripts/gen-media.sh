#!/usr/bin/env bash
# Generates S1 test media. Run: nix develop --command spikes/s1-decode/scripts/gen-media.sh
set -euo pipefail
M=/disk/shotgun-dev/media/s1
mkdir -p "$M"
FF="ffmpeg -hide_banner -loglevel error -y"
# testsrc2 has moving elements; temporal noise gives the encoder realistic work.
SRC4K60="testsrc2=size=3840x2160:rate=60,noise=alls=12:allf=t"
SRC4K30="testsrc2=size=3840x2160:rate=30,noise=alls=12:allf=t"
SRC1080="testsrc2=size=1920x1080:rate=60,noise=alls=12:allf=t"
T=20

[ -f $M/uhd60_hevc8.mp4 ] || $FF -f lavfi -i "$SRC4K60" -t $T -pix_fmt yuv420p -c:v libx265 -preset fast \
  -b:v 45M -x265-params "keyint=250:min-keyint=250:scenecut=0:log-level=error" -tag:v hvc1 \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/uhd60_hevc8.mp4
[ -f $M/uhd60_hevc10.mp4 ] || $FF -f lavfi -i "$SRC4K60" -t $T -pix_fmt yuv420p10le -c:v libx265 -preset fast -profile:v main10 \
  -b:v 45M -x265-params "keyint=250:min-keyint=250:scenecut=0:log-level=error" -tag:v hvc1 \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/uhd60_hevc10.mp4
[ -f $M/uhd30_h264.mp4 ] || $FF -f lavfi -i "$SRC4K30" -t $T -pix_fmt yuv420p -c:v libx264 -preset fast -profile:v high \
  -b:v 45M -g 250 -keyint_min 250 -sc_threshold 0 \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/uhd30_h264.mp4
[ -f $M/fhd60_h264.mp4 ] || $FF -f lavfi -i "$SRC1080" -t $T -pix_fmt yuv420p -c:v libx264 -preset fast -profile:v high \
  -b:v 12M -g 250 -keyint_min 250 -sc_threshold 0 \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/fhd60_h264.mp4
# All-intra 960x540 proxy candidates, derived from the 4K60 HEVC file (same frame count/timestamps).
[ -f $M/proxy540_h264_intra.mp4 ] || $FF -i $M/uhd60_hevc8.mp4 -vf scale=960:540 -pix_fmt yuv420p -c:v libx264 -preset fast -profile:v high \
  -g 1 -crf 20 -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/proxy540_h264_intra.mp4
[ -f $M/proxy540_hevc_intra.mp4 ] || $FF -i $M/uhd60_hevc8.mp4 -vf scale=960:540 -pix_fmt yuv420p -c:v libx265 -preset fast \
  -profile:v main -x265-params "keyint=1:log-level=error" -crf 22 -tag:v hvc1 \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/proxy540_hevc_intra.mp4
[ -f $M/proxy540_mjpeg.mov ] || $FF -i $M/uhd60_hevc8.mp4 -vf scale=960:540 -pix_fmt yuvj420p -c:v mjpeg -q:v 4 $M/proxy540_mjpeg.mov
ls -la $M
# Same 4K60 HEVC content with a 1 s GOP (typical camera / phone material) for the seek comparison.
[ -f $M/uhd60_hevc8_gop60.mp4 ] || $FF -f lavfi -i "$SRC4K60" -t $T -pix_fmt yuv420p -c:v libx265 -preset fast \
  -b:v 45M -x265-params "keyint=60:min-keyint=60:scenecut=0:log-level=error" -tag:v hvc1 \
  -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/uhd60_hevc8_gop60.mp4
# 1080p all-intra H.264 proxy candidate (is a full-HD proxy still inside the 50 ms scrub budget?)
[ -f $M/proxy1080_h264_intra.mp4 ] || $FF -i $M/uhd60_hevc8.mp4 -vf scale=1920:1080 -pix_fmt yuv420p -c:v libx264 -preset fast -profile:v high \
  -g 1 -crf 20 -color_range tv -colorspace bt709 -color_primaries bt709 -color_trc bt709 $M/proxy1080_h264_intra.mp4
ls -la $M
