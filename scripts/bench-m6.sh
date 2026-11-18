#!/usr/bin/env bash
# Milestone M6 budget: export of a 4K H.265 master at realtime or faster
# "where the encoder allows", and every export verified by probing the file.
# Run from the repo root:   nix develop --command bash scripts/bench-m6.sh
#
# Builds two projects through the app (hidden, no sound device): a 4K60
# sequence with one 4K60 HEVC clip, and a 1080p60 sequence with a 1080p60
# clip, a title and a second layer. Then exports them with `sg-bench export`,
# which runs the same code as the export panel, and probes the results.
set -eu
BIN=$CARGO_TARGET_DIR/release
MEDIA=/disk/shotgun-dev/media/s1
LIB=/disk/shotgun-dev/tmp/bench-m6-lib
cargo build --release -p sg-app -p sg-bench 2>&1 | tail -1
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do
  hyprctl keyword windowrule "match:class ^(shotgun-dev)\$, $r" >/dev/null
done
echo "== machine: $(uptime | sed 's/.*load average/load average/'), commit $(git rev-parse --short HEAD)"
rm -rf "$LIB"
app() { SHOTGUN_LIBRARY=$LIB SHOTGUN_APP_ID=shotgun-dev SHOTGUN_AUDIO=null SHOTGUN_NO_CLIPBOARD=1 SHOTGUN_LOG=error timeout 600 "$BIN/shotgun" --script "$1"; }
app "new:Four K;size:3840x2160;import:$MEDIA/uhd60_hevc8_gop60.mp4;insert;close;quit"
app "new:Full HD;vtrack;import:$MEDIA/fhd60_h264.mp4;insert;seek:0;import:/disk/shotgun-dev/media/m2/av.mp4;insert;select;select;prop:scale=0.4;prop:x=0.28;prop:y=0.28;seek:60;title;close;quit"
probe() { ffprobe -v error -show_entries stream=codec_name,codec_tag_string,width,height,pix_fmt,avg_frame_rate,nb_frames:format=duration,size -of compact=p=0:nk=0 "$1" | tr '\n' ' '; echo; }
echo "== 4K60 master (budget: >= 1.0x realtime where the encoder allows)"
"$BIN/sg-bench" export "$LIB/four-k" master 2>/dev/null
"$BIN/sg-bench" export "$LIB/four-k" master 2>/dev/null
probe "$LIB/four-k/exports/four-k-master-1.mp4"
echo "== 1080p60, two layers and a title"
"$BIN/sg-bench" export "$LIB/full-hd" master 2>/dev/null
probe "$LIB/full-hd/exports/full-hd-master-1.mp4"
"$BIN/sg-bench" export "$LIB/full-hd" vertical 2>/dev/null
probe "$LIB/full-hd/exports/full-hd-vertical-1.mp4"
for mb in 10 25; do
  "$BIN/sg-bench" export "$LIB/full-hd" share:$mb 2>/dev/null
  f=$(ls -t "$LIB/full-hd/exports/"*share* | head -1); probe "$f"
  echo "  target $mb MB = $((mb * 1000000)) bytes; file is $(( $(stat -c %s "$f") * 1000 / (mb * 1000000) )) per mille of it"
done
"$BIN/sg-bench" export "$LIB/full-hd" gif 60 240 2>/dev/null
probe "$LIB/full-hd/exports/full-hd-gif-1.gif"
