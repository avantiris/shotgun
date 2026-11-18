#!/usr/bin/env bash
# S4 item 2: timing of probe / VFR-detection methods on multi-GB files (cold = page cache
# for that file dropped with posix_fadvise via `dd iflag=nocache count=0`, warm = second run).
set -u
. "$(dirname "$0")/lib.sh"
B=$M/big; mkdir -p $B
[ -f $B/big.mp4 ] || ffmpeg -hide_banner -loglevel error -y -stream_loop 29 -i $SRC -c copy -movflags +faststart $B/big.mp4
[ -f $B/big.mkv ] || ffmpeg -hide_banner -loglevel error -y -i $B/big.mp4 -c copy $B/big.mkv
ls -la $B | awk 'NR>1{print $5, $9}'
D=/home/linus/Documents/repos/shotgun/spikes/s4-io
tm() { local s=$EPOCHREALTIME; "$@" > /dev/null 2>&1; awk -v a=$s -v b=$EPOCHREALTIME 'BEGIN{printf "%.2f", b-a}'; }
cold() { dd if=$1 iflag=nocache count=0 status=none; }
m1() { ffprobe -v error -print_format json -show_format -show_streams "$1"; }
m2() { ffprobe -v error -select_streams v:0 -show_entries stream=time_base:packet=pts -of json "$1" | python3 -I $D/vfr_detect.py; }
m3() { ffprobe -v error -select_streams v:0 -count_packets -show_entries stream=nb_read_packets -of csv=p=0 "$1"; }
m4() { ffprobe -v error -select_streams v:0 -read_intervals "%+20,25%%+5,50%%+5,75%%+5" -show_entries stream=time_base:packet=pts -of json "$1" | python3 -I $D/vfr_detect.py; }
m5() { mediainfo --Inform="Video;%FrameRate_Mode% %FrameRate% %FrameCount%" "$1"; }
echo "load1=$(load1)"
echo -e "file\tmethod\tcold_s\twarm_s\tresult"
for f in $B/big.mp4 $B/big.mkv; do
  for m in m1 m2 m3 m5; do
    cold $f; c=$(tm $m $f); w=$(tm $m $f); r=$($m $f 2>&1 | tr -d '\n' | cut -c1-150)
    [ $m = m1 ] && r="(json, $(m1 $f | wc -c) bytes)"
    echo -e "$(basename $f)\t$m\t$c\t$w\t$r"
  done
done
