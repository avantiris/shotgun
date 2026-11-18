#!/usr/bin/env bash
# S4 item 5 addendum: hevc_vaapi re-run. `-tag:v hvc1` directly on hevc_vaapi output gives an
# undecodable MP4 on this GPU/driver (PSNR 7.5 dB, green garbage), so encode with the default
# hev1 tag and, where an hvc1 file is wanted, remux afterwards with -c copy -tag:v hvc1.
set -u
. "$(dirname "$0")/lib.sh"
O=$M/master; mkdir -p $O
D=$(dur $SRC)
TAGS="-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv"
AUD="-c:a aac -b:a 256k"; MUX="-movflags +faststart"
HWIN="-hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi"
measure() { local f=$1 j=${1%.*}.vmaf.json
  ffmpeg -hide_banner -loglevel error -i $f -i $SRC -an -lavfi \
    "[0:v]setpts=PTS-STARTPTS,format=yuv420p10le[d];[1:v]setpts=PTS-STARTPTS,format=yuv420p10le[r];[d][r]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=16:n_subsample=3:feature=name=psnr|name=float_ssim:log_fmt=json:log_path=$j" -f null - || { echo "- - - - -"; return; }
  jq -r '.pooled_metrics | "\(.vmaf.mean) \(.vmaf.harmonic_mean) \(.vmaf.min) \(.psnr_y.mean) \(.float_ssim.mean)"' $j; }
run() { local name=$1; shift; local out=$O/$name.mp4 l=$(load1) t=$(now)
  ffmpeg -hide_banner -loglevel error -y "$@" $TAGS $AUD $MUX $out || { echo -e "$name\tFAILED"; return; }
  local e=$(elapsed $t) sz=$(fsize $out)
  read vm vh vmin ps ss <<< "$(measure $out)"
  awk -v n="$name" -v e="$e" -v d="$D" -v sz="$sz" -v vm="$vm" -v vh="$vh" -v vmin="$vmin" -v ps="$ps" -v ss="$ss" -v l="$l" \
    'BEGIN{printf "%s\t%.1f\t%.2f\t%.1f\t%.1f\t%.1f\t%.2f\t%.2f\t%.2f\t%.2f\t%.4f\t1799\t%s\n", n, e, d/e, 1799/e, sz/1e6, sz*8/d/1e6, vm, vh, vmin, ps, ss, l}'; }
echo -e "config\tenc_s\tx_rt\tfps\tMB\tMbit_s\tvmaf_mean\tvmaf_harm\tvmaf_min\tpsnr_y\tssim\tframes\tload1"
run hevc_vaapi-hev1-qp18     $HWIN -i $SRC -c:v hevc_vaapi -rc_mode CQP -qp 18
run hevc_vaapi-hev1-qp22     $HWIN -i $SRC -c:v hevc_vaapi -rc_mode CQP -qp 22
run hevc_vaapi-hev1-10b-qp20 $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v hevc_vaapi -profile:v main10 -rc_mode CQP -qp 20
run hevc_vaapi-hev1-10b-qp24 $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v hevc_vaapi -profile:v main10 -rc_mode CQP -qp 24
run hevc_vaapi-hev1-vbr60    $HWIN -i $SRC -c:v hevc_vaapi -rc_mode VBR -b:v 60M -maxrate 80M
run av1_vaapi-10b-q80        $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v av1_vaapi -rc_mode CQP -global_quality 80
# remux to hvc1 and verify that the remuxed file still decodes to the same pictures
t=$(now); ffmpeg -hide_banner -loglevel error -y -i $O/hevc_vaapi-hev1-10b-qp20.mp4 -c copy -tag:v hvc1 -movflags +faststart $O/hevc_vaapi-hvc1-remux-10b-qp20.mp4; e=$(elapsed $t)
echo "remux hev1->hvc1: ${e}s tag=$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_tag_string -of csv=p=0 $O/hevc_vaapi-hvc1-remux-10b-qp20.mp4) psnr_vs_src=$(ffmpeg -hide_banner -i $O/hevc_vaapi-hvc1-remux-10b-qp20.mp4 -i $SRC -an -lavfi "[0:v]setpts=PTS-STARTPTS,format=yuv420p10le[a];[1:v]setpts=PTS-STARTPTS,format=yuv420p10le[b];[a][b]psnr" -f null - 2>&1 | grep -o "average:[0-9.a-z]*" | tail -1) decode_errors=$(ffmpeg -hide_banner -loglevel error -i $O/hevc_vaapi-hvc1-remux-10b-qp20.mp4 -an -f null - 2>&1 | wc -l)"
