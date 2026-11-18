#!/usr/bin/env bash
# S4 item 6: size-targeted share copy. Two-pass bitrate from target size, incl. audio + mux margin.
#   video_bps = (target_bytes * 8 * (1 - MARGIN)) / duration - audio_bps
# TSV on stdout. Usage: bench_target.sh [vmaf]   (vmaf: also compute VMAF, slow)
set -u
. "$(dirname "$0")/lib.sh"
O=$M/target; mkdir -p $O
D=$(dur $SRC)
MARGIN=0.02
WITH_VMAF=${1:-no}
vbr() { awk -v t=$1 -v d=$D -v a=$2 -v m=$MARGIN 'BEGIN{printf "%d", t*8*(1-m)/d - a*1000}'; }   # -> bits/s
row() { local name=$1 target=$2 out=$3 e=$4 vb=$5 l=$6 sz=$(fsize $3) vm=-
  if [ $WITH_VMAF = vmaf ]; then
    ffmpeg -hide_banner -loglevel error -i $out -i $SRC -an -lavfi "[0:v]fps=60000/1001,scale=3840:2160:flags=bicubic,format=yuv420p10le,setpts=N/(60000/1001)/TB[d];[1:v]format=yuv420p10le,setpts=N/(60000/1001)/TB[r];[d][r]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=16:n_subsample=5:log_fmt=json:log_path=${out%.*}.json" -f null - && vm=$(jq -r '.pooled_metrics.vmaf.mean' ${out%.*}.json)
  fi
  awk -v n=$name -v t=$target -v sz=$sz -v e=$e -v d=$D -v vb=$vb -v vm=$vm -v l=$l 'BEGIN{printf "%s\t%d\t%d\t%.2f%%\t%s\t%d\t%.1f\t%.2f\t%s\t%s\n", n, t, sz, sz/t*100, (sz<=t?"OK":"OVER"), vb/1000, e, d/e, vm, l}'; }
TAGS="-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv"
x264() { # name target w h fps abr
  local name=$1 t=$2 vf="fps=$5,scale=$3:$4:flags=lanczos" ab=$6 vb=$(vbr $2 $6) out=$O/$1.mp4 l=$(load1) s=$(now)
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vf $vf -c:v libx264 -preset slow -b:v $vb -pass 1 -passlogfile $O/$name -pix_fmt yuv420p -an -f null - &&
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vf $vf -c:v libx264 -preset slow -b:v $vb -pass 2 -passlogfile $O/$name -pix_fmt yuv420p $TAGS -c:a aac -b:a ${ab}k -movflags +faststart $out
  row $name $t $out $(elapsed $s) $vb $l; }
x265() {
  local name=$1 t=$2 vf="fps=$5,scale=$3:$4:flags=lanczos" ab=$6 vb=$(vbr $2 $6) out=$O/$1.mp4 l=$(load1) s=$(now)
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vf $vf -c:v libx265 -preset medium -b:v $vb -x265-params "pass=1:stats=$O/$name.x265:log-level=error" -pix_fmt yuv420p10le -an -f null - &&
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vf $vf -c:v libx265 -preset medium -b:v $vb -x265-params "pass=2:stats=$O/$name.x265:log-level=error" -pix_fmt yuv420p10le -tag:v hvc1 $TAGS -c:a aac -b:a ${ab}k -movflags +faststart $out
  row $name $t $out $(elapsed $s) $vb $l; }
svt() { # 1-pass VBR (SVT-AV1 has no 2-pass through ffmpeg in a single output chain worth relying on)
  local name=$1 t=$2 vf="fps=$5,scale=$3:$4:flags=lanczos" ab=$6 vb=$(vbr $2 $6) out=$O/$1.mp4 l=$(load1) s=$(now)
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vf $vf -c:v libsvtav1 -preset 6 -b:v $vb -svtav1-params "rc=1:tune=0" -pix_fmt yuv420p10le $TAGS -c:a libopus -b:a ${ab}k -movflags +faststart $out
  row $name $t $out $(elapsed $s) $vb $l; }
vaapi() { # 1-pass hardware VBR
  local name=$1 t=$2 ab=$6 vb=$(vbr $2 $6) out=$O/$1.mp4 l=$(load1) s=$(now)
  ffmpeg -hide_banner -loglevel error -y -hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi -i $SRC -vf "fps=$5,scale_vaapi=w=$3:h=$4" -c:v h264_vaapi -rc_mode VBR -b:v $vb -maxrate $((vb*3/2)) $TAGS -c:a aac -b:a ${ab}k -movflags +faststart $out
  row $name $t $out $(elapsed $s) $vb $l; }
echo -e "name\ttarget_bytes\tactual_bytes\tpct\tfits\tvideo_kbps\ttotal_s\tx_rt\tvmaf\tload1"
MB10=10000000; MB25=25000000; MIB8=$((8*1024*1024))
x264  x264-10MB-720p30    $MB10 1280 720 30000/1001 96
x264  x264-25MB-1080p60   $MB25 1920 1080 60000/1001 128
x264  x264-8MiB-720p30    $MIB8 1280 720 30000/1001 96
x264  x264-10MB-1080p30   $MB10 1920 1080 30000/1001 96
x265  x265-10MB-1080p30   $MB10 1920 1080 30000/1001 96
svt   svtav1-10MB-1080p30 $MB10 1920 1080 30000/1001 96
vaapi h264vaapi-10MB-720p30 $MB10 1280 720 30000/1001 96
vaapi h264vaapi-25MB-1080p60 $MB25 1920 1080 60000/1001 128
