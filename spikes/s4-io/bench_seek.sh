#!/usr/bin/env bash
# S4 item 3: random-seek timing over the same 50 pseudo-random positions per file.
#  A) per-process: 50 x `ffmpeg -ss T -i f -frames:v 1 -f null -`  (median/p95 wall ms; includes
#     ffmpeg-full process start, see the "startup" row; subtract it mentally)
#  B) (do NOT run on 4K long-GOP files: 50 parallel decoders were OOM-killed on 16 GB) one process, 50 inputs: `ffmpeg -ss T1 -i f -ss T2 -i f ... -map N:v -frames:v 1 -f null -`
#     -> (wall - startup) / 50 = open + accurate seek + decode to target frame
#  C) one process, one open: ffprobe -read_intervals "T1%+#1,T2%+#1,..." -show_frames
#     -> (wall - startup) / 50 = keyframe seek + decode 1 frame (NOT frame-accurate for GOP > 1)
set -u
. "$(dirname "$0")/lib.sh"
POS=$(awk 'BEGIN{srand(42); for(i=0;i<50;i++) printf "%.3f\n", rand()*29.5}')
stats() { sort -n | awk '{a[NR]=$1; s+=$1} END{printf "%.1f\t%.1f\t%.1f", a[int((NR+1)/2)], a[int(NR*0.95)], a[NR]}'; }
ms() { awk -v a=$1 -v b=$EPOCHREALTIME 'BEGIN{printf "%.1f\n", (b-a)*1000}'; }
SU=$(for i in $(seq 30); do t=$(now); ffmpeg -hide_banner -loglevel error -version >/dev/null; ms $t; done | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')
SP=$(for i in $(seq 30); do t=$(now); ffprobe -hide_banner -loglevel error -version >/dev/null; ms $t; done | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')
echo "startup median: ffmpeg -version ${SU} ms, ffprobe -version ${SP} ms, load1=$(load1)"
echo -e "file\tA_median_ms\tA_p95_ms\tA_max_ms\tA_median_minus_startup\tB_ms_per_seek\tC_ms_per_seek\tload1"
for f in "$@"; do
  l=$(load1)
  A=$(for p in $POS; do t=$(now); ffmpeg -hide_banner -loglevel error -ss $p -i "$f" -an -frames:v 1 -f null - ; ms $t; done | stats)
  args=(); maps=(); i=0
  for p in $POS; do args+=(-ss $p -i "$f"); maps+=(-map $i:v:0 -frames:v 1 -f null -); i=$((i+1)); done
  t=$(now); ffmpeg -hide_banner -loglevel error "${args[@]}" "${maps[@]}"; B=$(ms $t)
  iv=$(echo $POS | tr ' ' '\n' | awk '{printf "%s%s%%+#1", (NR>1?",":""), $1}')
  t=$(now); n=$(ffprobe -v error -select_streams v:0 -read_intervals "$iv" -show_entries frame=pts_time -of csv=p=0 "$f" | wc -l); C=$(ms $t)
  echo "$A" | awk -v f=$(basename $f) -v su=$SU -v sp=$SP -v B=$B -v C=$C -v l=$l -v n=$n -F'\t' '{printf "%s\t%s\t%s\t%s\t%.1f\t%.1f\t%.1f\t%s\n", f, $1, $2, $3, $1-su, (B-su)/50, (C-sp)/n, l}'
done
