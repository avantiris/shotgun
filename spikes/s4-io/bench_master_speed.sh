#!/usr/bin/env bash
# S4 item 5 addendum: encode speed only (no quality metrics), run when the machine was quieter.
set -u
. "$(dirname "$0")/lib.sh"
O=$M/master/speed; mkdir -p $O
D=$(dur $SRC)
TAGS="-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv"
HWIN="-hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi"
run() { local name=$1; shift; local l=$(load1) t=$(now)
  ffmpeg -hide_banner -loglevel error -y "$@" $TAGS -c:a aac -b:a 256k -movflags +faststart $O/$name.mp4 || { echo -e "$name\tFAILED"; return; }
  local e=$(elapsed $t); awk -v n=$name -v e=$e -v d=$D -v sz=$(fsize $O/$name.mp4) -v l=$l -v l2=$(load1) 'BEGIN{printf "%s\t%.1f\t%.2f\t%.1f\t%.1f\t%s\t%s\n", n, e, d/e, 1799/e, sz/1e6, l, l2}'; }
echo -e "config\tenc_s\tx_rt\tfps\tMB\tload1_start\tload1_end"
run hevc_vaapi-10b-qp20 $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v hevc_vaapi -profile:v main10 -rc_mode CQP -qp 20
run av1_vaapi-10b-q60   $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v av1_vaapi -rc_mode CQP -global_quality 60
run h264_vaapi-qp18     $HWIN -i $SRC -c:v h264_vaapi -rc_mode CQP -qp 18
run svtav1-p8-crf24     -i $SRC -c:v libsvtav1 -preset 8 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0
run x264-slow-crf16     -i $SRC -c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p
run svtav1-p6-crf24     -i $SRC -c:v libsvtav1 -preset 6 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0
run x264-medium-crf18   -i $SRC -c:v libx264 -preset medium -crf 18 -pix_fmt yuv420p
run x265-10b-fast-crf18 -i $SRC -c:v libx265 -preset fast -crf 18 -pix_fmt yuv420p10le -tag:v hvc1 -x265-params log-level=error
run x265-10b-medium-crf18 -i $SRC -c:v libx265 -preset medium -crf 18 -pix_fmt yuv420p10le -tag:v hvc1 -x265-params log-level=error
echo done
