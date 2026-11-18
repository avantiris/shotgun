#!/usr/bin/env bash
# S4 item 6, refined method:
#   1. encode the audio alone (fast) and measure its real size (the native AAC encoder undershoots -b:a)
#   2. video_bytes_budget = target*(1-MARGIN) - audio_bytes - MUX (MUX = 0.3 % of target)
#   3. x264 two-pass at that bitrate, mux with the pre-encoded audio (-c:a copy)
#   4. correction: if the result is below 98 % (or above 100 %) of target, repeat pass 2 only
#      (the pass-1 stats are reused) with bitrate * (video budget / actual video bytes)
set -u
. "$(dirname "$0")/lib.sh"
O=$M/target2; mkdir -p $O
D=$(dur $SRC); MARGIN=0.01
TAGS="-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv"
echo -e "name\ttarget\tstep\tvideo_kbps\tbytes\tpct\ttotal_s\tload1"
go() { # name target w h fps abr
  local name=$1 t=$2 vf="fps=$5,scale=$3:$4:flags=lanczos" ab=$6 l=$(load1) s=$(now)
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vn -c:a aac -b:a ${ab}k $O/$name.m4a
  local abytes=$(fsize $O/$name.m4a)
  local budget=$(awk -v t=$t -v m=$MARGIN -v a=$abytes 'BEGIN{printf "%d", t*(1-m) - a - t*0.003}')
  local vb=$(awk -v b=$budget -v d=$D 'BEGIN{printf "%d", b*8/d}')
  ffmpeg -hide_banner -loglevel error -y -i $SRC -vf $vf -c:v libx264 -preset slow -b:v $vb -pass 1 -passlogfile $O/$name -pix_fmt yuv420p -an -f null -
  p2() { ffmpeg -hide_banner -loglevel error -y -i $SRC -i $O/$name.m4a -map 0:v:0 -map 1:a:0 -vf $vf -c:v libx264 -preset slow -b:v $1 -pass 2 -passlogfile $O/$name -pix_fmt yuv420p $TAGS -c:a copy -movflags +faststart $2; }
  p2 $vb $O/$name.mp4
  local sz=$(fsize $O/$name.mp4)
  echo -e "$name\t$t\tfirst (audio $abytes B)\t$((vb/1000))\t$sz\t$(awk -v s=$sz -v t=$t 'BEGIN{printf "%.2f%%", s/t*100}')\t$(elapsed $s)\t$l"
  local need=$(awk -v s=$sz -v t=$t 'BEGIN{print (s>t || s<0.98*t) ? 1 : 0}')
  if [ $need = 1 ]; then
    local vbytes=$(ffmpeg -hide_banner -loglevel error -i $O/$name.mp4 -map 0:v -c copy -f h264 - | wc -c)
    local vb2=$(awk -v vb=$vb -v b=$budget -v a=$vbytes 'BEGIN{printf "%d", vb*b/a}')
    p2 $vb2 $O/$name-corr.mp4
    sz=$(fsize $O/$name-corr.mp4)
    echo -e "$name\t$t\tcorrected pass 2\t$((vb2/1000))\t$sz\t$(awk -v s=$sz -v t=$t 'BEGIN{printf "%.2f%%", s/t*100}')\t$(elapsed $s)\t$(load1)"
  fi
}
go x264-10MB-720p30  10000000 1280 720 30000/1001 96
go x264-25MB-1080p60 25000000 1920 1080 60000/1001 128
go x264-8MiB-720p30  $((8*1024*1024)) 1280 720 30000/1001 96
