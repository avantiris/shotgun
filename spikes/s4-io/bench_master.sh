#!/usr/bin/env bash
# S4 item 5: master export encoders. For each config: encode the 4K60 source, then measure
# VMAF (4K model) + PSNR + SSIM against the source (every 3rd frame: n_subsample=3). TSV on stdout.
# Usage: bench_master.sh [name-filter-regex]
set -u
. "$(dirname "$0")/lib.sh"
O=$M/master; mkdir -p $O
D=$(dur $SRC)
FILTER=${1:-.}
TAGS="-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv"
AUD="-c:a aac -b:a 256k"
MUX="-movflags +faststart"
HWIN="-hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi"

measure() { # file -> "vmaf_mean vmaf_harm vmaf_min psnr_y ssim"
  local f=$1 j=${1%.*}.vmaf.json
  ffmpeg -hide_banner -loglevel error -i $f -i $SRC -an -lavfi \
    "[0:v]setpts=PTS-STARTPTS,format=yuv420p10le[d];[1:v]setpts=PTS-STARTPTS,format=yuv420p10le[r];[d][r]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=16:n_subsample=3:feature=name=psnr|name=float_ssim:log_fmt=json:log_path=$j" -f null - || { echo "- - - - -"; return; }
  jq -r '.pooled_metrics | "\(.vmaf.mean) \(.vmaf.harmonic_mean) \(.vmaf.min) \(.psnr_y.mean) \(.float_ssim.mean)"' $j
}
run() { # name ext -- input-args -- output-args
  local name=$1; shift
  [[ $name =~ $FILTER ]] || return
  local out=$O/$name.mp4
  local l=$(load1) t=$(now)
  ffmpeg -hide_banner -loglevel error -y "$@" $TAGS $AUD $MUX $out || { echo -e "$name\tFAILED"; return; }
  local e=$(elapsed $t) sz=$(fsize $out)
  local nf=$(ffprobe -v error -select_streams v:0 -show_entries stream=nb_frames -of csv=p=0 $out)
  read vm vh vmin ps ss <<< "$(measure $out)"
  awk -v n="$name" -v e="$e" -v d="$D" -v sz="$sz" -v vm="$vm" -v vh="$vh" -v vmin="$vmin" -v ps="$ps" -v ss="$ss" -v l="$l" -v nf="$nf" \
    'BEGIN{printf "%s\t%.1f\t%.2f\t%.1f\t%.1f\t%.1f\t%.2f\t%.2f\t%.2f\t%.2f\t%.4f\t%s\t%s\n", n, e, d/e, nf/e, sz/1e6, sz*8/d/1e6, vm, vh, vmin, ps, ss, nf, l}'
}
echo -e "config\tenc_s\tx_rt\tfps\tMB\tMbit_s\tvmaf_mean\tvmaf_harm\tvmaf_min\tpsnr_y\tssim\tframes\tload1"
X5="log-level=error"
# NOTE: no -tag:v hvc1 on the hevc_vaapi rows: it yields an undecodable MP4 on this GPU (docs/spikes/s4-io.md 5.3).
# The first run had it; valid hevc_vaapi numbers come from bench_master_hevc.sh.
run x264-slow-crf16     -i $SRC -c:v libx264 -preset slow   -crf 16 -pix_fmt yuv420p
run x264-medium-crf18   -i $SRC -c:v libx264 -preset medium -crf 18 -pix_fmt yuv420p
run x264-veryfast-crf18 -i $SRC -c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p
run x265-10b-medium-crf18 -i $SRC -c:v libx265 -preset medium -crf 18 -pix_fmt yuv420p10le -tag:v hvc1 -x265-params $X5
run x265-10b-fast-crf18   -i $SRC -c:v libx265 -preset fast   -crf 18 -pix_fmt yuv420p10le -tag:v hvc1 -x265-params $X5
run x265-10b-slow-crf18   -i $SRC -c:v libx265 -preset slow   -crf 18 -pix_fmt yuv420p10le -tag:v hvc1 -x265-params $X5
run svtav1-p4-crf24     -i $SRC -c:v libsvtav1 -preset 4 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0
run svtav1-p6-crf24     -i $SRC -c:v libsvtav1 -preset 6 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0
run svtav1-p8-crf24     -i $SRC -c:v libsvtav1 -preset 8 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0
run hevc_vaapi-qp18     $HWIN -i $SRC -c:v hevc_vaapi -rc_mode CQP -qp 18
run hevc_vaapi-qp22     $HWIN -i $SRC -c:v hevc_vaapi -rc_mode CQP -qp 22
run hevc_vaapi-10b-qp20 $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v hevc_vaapi -profile:v main10 -rc_mode CQP -qp 20
run hevc_vaapi-vbr60    $HWIN -i $SRC -c:v hevc_vaapi -rc_mode VBR -b:v 60M -maxrate 80M
run h264_vaapi-qp18     $HWIN -i $SRC -c:v h264_vaapi -rc_mode CQP -qp 18
run h264_vaapi-qp22     $HWIN -i $SRC -c:v h264_vaapi -rc_mode CQP -qp 22
run av1_vaapi-q60       $HWIN -i $SRC -c:v av1_vaapi -rc_mode CQP -global_quality 60
run av1_vaapi-q100      $HWIN -i $SRC -c:v av1_vaapi -rc_mode CQP -global_quality 100
run av1_vaapi-vbr40     $HWIN -i $SRC -c:v av1_vaapi -rc_mode VBR -b:v 40M -maxrate 60M
run av1_vaapi-10b-q60   $HWIN -i $SRC -vf scale_vaapi=format=p010 -c:v av1_vaapi -rc_mode CQP -global_quality 60
run hevc_vaapi-qvbr-q20 $HWIN -i $SRC -c:v hevc_vaapi -rc_mode QVBR -global_quality 20 -b:v 60M -maxrate 80M
