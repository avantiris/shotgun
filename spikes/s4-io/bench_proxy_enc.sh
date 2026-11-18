#!/usr/bin/env bash
# S4 item 3b: encoder-only speed. Input is pre-scaled raw NV12 on disk, so the 4K HEVC decode
# (which bounds bench_proxy.sh at ~4x realtime) is out of the picture.
set -u
. "$(dirname "$0")/lib.sh"
O=$M/proxy; T=$M/proxy/enconly; mkdir -p $T
HW="-hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi"
echo -e "codec\tres\tenc_s\tenc_fps\tenc_x_rt(59.94)\tload1"
for res in 960x540 1280x720; do
  w=${res%x*} h=${res#*x}; raw=$T/raw-$h.nv12
  [ -f $raw ] || ffmpeg -hide_banner -loglevel error -y $HW -i $SRC -an -vf "scale_vaapi=w=$w:h=$h,hwdownload,format=nv12" -f rawvideo $raw
  IN="-f rawvideo -pix_fmt nv12 -s $res -r 60000/1001 -i $raw"
  cat $raw > /dev/null   # page cache
  run() { local name=$1 ext=$2; shift 2; local l=$(load1) t=$(now)
    ffmpeg -hide_banner -loglevel error -y "$@" $T/$name-$h.$ext || { echo "FAILED $name" >&2; return; }
    local e=$(elapsed $t); awk -v n=$name -v r=$res -v e=$e -v l=$l 'BEGIN{printf "%s\t%s\t%.2f\t%.0f\t%.1f\t%s\n", n, r, e, 1799/e, 1799/e/59.94, l}'; }
  run rawread-null  nut $IN -f null
  run mjpeg-q3      mov $IN -c:v mjpeg -q:v 3 -pix_fmt yuvj420p
  run mjpeg-q3-thr  mov $IN -c:v mjpeg -q:v 3 -pix_fmt yuvj420p -threads 16 -thread_type frame
  run x264-intra    mp4 $IN -c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 1 -pix_fmt yuv420p
  run x264-intra-uf mp4 $IN -c:v libx264 -preset ultrafast -tune fastdecode -crf 20 -g 1 -pix_fmt yuv420p
  run vaapi-intra   mp4 -vaapi_device $VADEV $IN -vf format=nv12,hwupload -c:v h264_vaapi -g 1 -bf 0 -rc_mode CQP -qp 22
  run x264-gop8     mp4 $IN -c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 8 -keyint_min 8 -sc_threshold 0 -bf 0 -pix_fmt yuv420p
  run vaapi-gop8    mp4 -vaapi_device $VADEV $IN -vf format=nv12,hwupload -c:v h264_vaapi -g 8 -bf 0 -rc_mode CQP -qp 22
  run prores-ks     mov $IN -c:v prores_ks -profile:v 0 -pix_fmt yuv422p10le
  run prores-aw     mov $IN -c:v prores_aw -profile:v 0 -pix_fmt yuv422p10le
  run dnxhr-lb      mov $IN -c:v dnxhd -profile:v dnxhr_lb -pix_fmt yuv422p
done
rm -f $T/raw-*.nv12
