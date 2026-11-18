#!/usr/bin/env bash
# S4 item 3: method C only, repeated. One ffprobe process, one open, 200 keyframe seeks each
# followed by decoding one frame. ms/seek = (wall - ffprobe startup) / 200, median of 5 runs.
set -u
. "$(dirname "$0")/lib.sh"
ms() { awk -v a=$1 -v b=$EPOCHREALTIME 'BEGIN{printf "%.1f\n", (b-a)*1000}'; }
medn() { sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
IV=$(awk 'BEGIN{srand(42); for(i=0;i<200;i++) printf "%s%.3f%%+#1", (i?",":""), rand()*29.5}')
SP=$(for i in $(seq 20); do t=$(now); ffprobe -hide_banner -loglevel error -version >/dev/null; ms $t; done | medn)
echo "ffprobe startup median ${SP} ms  load1=$(load1)"
echo -e "file\tms_per_seek_default_threads\tms_per_seek_1_thread\topen_only_ms"
for f in "$@"; do
  a=$(for r in 1 2 3 4 5; do t=$(now); ffprobe -v error -select_streams v:0 -read_intervals "$IV" -show_entries frame=pts_time -of csv=p=0 "$f" >/dev/null; ms $t; done | medn)
  b=$(for r in 1 2 3 4 5; do t=$(now); ffprobe -v error -threads 1 -select_streams v:0 -read_intervals "$IV" -show_entries frame=pts_time -of csv=p=0 "$f" >/dev/null; ms $t; done | medn)
  o=$(for r in 1 2 3 4 5; do t=$(now); ffprobe -v error -show_entries format=duration -of csv=p=0 "$f" >/dev/null; ms $t; done | medn)
  awk -v f=$(basename $f) -v a=$a -v b=$b -v o=$o -v sp=$SP 'BEGIN{printf "%s\t%.2f\t%.2f\t%.1f\n", f, (a-o)/200, (b-o)/200, o-sp}'
done
