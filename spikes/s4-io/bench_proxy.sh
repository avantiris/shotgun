#!/usr/bin/env bash
# S4 item 3: proxy codec comparison. Output: TSV on stdout.
# Decode+scale is done on the GPU (VAAPI) for all variants so that only the encoder differs.
set -u
. "$(dirname "$0")/lib.sh"
O=$M/proxy; mkdir -p $O
D=$(dur $SRC)
HW="-hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi"

enc() { # name res ext vf_tail enc-args...
  local name=$1 res=$2 ext=$3 vft=$4; shift 4
  local w=${res%x*} h=${res#*x}
  local out=$O/$name-$h.$ext   # separate line: all words of one `local` are expanded before any assignment
  local l=$(load1) t=$(now)
  ffmpeg -hide_banner -loglevel error -y $HW -i $SRC -an -vf "scale_vaapi=w=$w:h=$h$vft" "$@" $out || { echo "FAILED $name $res" >&2; return; }
  local e=$(elapsed $t)
  local sz=$(fsize $out)
  # decode speed: all threads and 1 thread
  t=$(now); ffmpeg -hide_banner -loglevel error -i $out -f null - ; local dmt=$(elapsed $t)
  t=$(now); ffmpeg -hide_banner -loglevel error -threads 1 -i $out -f null - ; local dst=$(elapsed $t)
  awk -v n="$name" -v r="$res" -v e="$e" -v d="$D" -v sz="$sz" -v dmt="$dmt" -v dst="$dst" -v l="$l" \
    'BEGIN{printf "%s\t%s\t%.2f\t%.1f\t%.1f\t%.1f\t%.0f\t%.0f\t%s\n", n, r, e, d/e, sz/1e6*60/d, sz*8/d/1e6, 1800/dmt, 1800/dst, l}'
}

echo -e "codec\tres\tenc_s\tenc_x_rt\tMB_per_min\tMbit_s\tdec_fps_mt\tdec_fps_1t\tload1"
# baseline: decode + scale + download only
for res in 960x540 1280x720; do
  w=${res%x*} h=${res#*x}; t=$(now)
  ffmpeg -hide_banner -loglevel error $HW -i $SRC -an -vf "scale_vaapi=w=$w:h=$h,hwdownload,format=nv12" -f null -
  e=$(elapsed $t); awk -v r=$res -v e=$e -v d=$D 'BEGIN{printf "baseline-null\t%s\t%.2f\t%.1f\t-\t-\t-\t-\t-\n", r, e, d/e}'
done
DL=",hwdownload,format=nv12"
for res in 960x540 1280x720; do
  enc mjpeg-q3      $res mov "$DL" -c:v mjpeg -q:v 3 -pix_fmt yuvj420p -color_range pc
  enc mjpeg-q5      $res mov "$DL" -c:v mjpeg -q:v 5 -pix_fmt yuvj420p -color_range pc
  enc x264-intra    $res mp4 "$DL" -c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 1 -pix_fmt yuv420p
  enc vaapi-intra   $res mp4 ""    -c:v h264_vaapi -g 1 -bf 0 -rc_mode CQP -qp 22
  enc x264-gop8     $res mp4 "$DL" -c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 8 -keyint_min 8 -sc_threshold 0 -bf 0 -pix_fmt yuv420p
  enc vaapi-gop8    $res mp4 ""    -c:v h264_vaapi -g 8 -bf 0 -rc_mode CQP -qp 22
  enc prores-proxy  $res mov "$DL" -c:v prores_ks -profile:v 0 -pix_fmt yuv422p10le -vendor apl0
  enc dnxhr-lb      $res mov "$DL" -c:v dnxhd -profile:v dnxhr_lb -pix_fmt yuv422p
done
