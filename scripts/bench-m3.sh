#!/usr/bin/env bash
# Milestone M3 budgets: scrub (seek to proxy frame <= 50 ms p95), full-quality
# frame after pausing (<= 150 ms p95 for GOP <= 1 s), play start (<= 80 ms),
# and the audio engine's numbers.
# Run from the repo root:   nix develop --command bash scripts/bench-m3.sh
#
# The app runs hidden (app id shotgun-dev). Sound goes to a PipeWire null
# sink that this script creates and removes, never to the speakers.
set -eu
BIN=$CARGO_TARGET_DIR/release
MEDIA=/disk/shotgun-dev/media/s1
LIB=/disk/shotgun-dev/tmp/bench-m3-lib
cargo build --release -p sg-app -p sg-bench 2>&1 | tail -1
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do
  hyprctl keyword windowrule "match:class ^(shotgun-dev)\$, $r" >/dev/null
done
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=shotgun-null node.description="Shotgun test sink" media.class=Audio/Sink object.linger=true audio.position=[FL FR] priority.session=0 }' >/dev/null
sink_id() { pw-cli ls Node | awk '/^\tid [0-9]+,/ { id = $2 } /node.name = "shotgun-null"/ { sub(",", "", id); print id; exit }'; }
trap 'pw-cli destroy "$(sink_id)" >/dev/null 2>&1 || true' EXIT
echo "== machine: $(uptime | sed 's/.*load average/load average/'), commit $(git rev-parse --short HEAD)"

echo "== audio engine on the null sink, 20 s"
"$BIN/sg-bench" audio shotgun-null 20

# One app run per group of sources: 60 seeks to different frames with half a
# second between them (the full-quality frame is fetched once the playhead
# rests), then 12 play starts.
scrub() { # <label> <file>...
  label=$1; shift
  steps="new:Bench $label"; frames=0
  for f in "$@"; do
    steps="$steps;import:$MEDIA/$f.mp4;insert"
    n=$(ffprobe -v error -select_streams v:0 -show_entries stream=nb_frames -of csv=p=0 "$MEDIA/$f.mp4")
    r=$(ffprobe -v error -select_streams v:0 -show_entries stream=avg_frame_rate -of csv=p=0 "$MEDIA/$f.mp4")
    frames=$(( frames + n * 60 / ${r%/*} * ${r#*/} ))
    echo "  $f: $(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,avg_frame_rate -of csv=p=0 "$MEDIA/$f.mp4"), $n frames"
  done
  frame=37
  for _ in $(seq 60); do
    frame=$(( (frame * 7919 + 1031) % (frames - 2) ))
    steps="$steps;seek:$frame;wait:500"
  done
  for i in $(seq 12); do steps="$steps;seek:$(( i * frames / 14 ));wait:300;play;wait:400;play;wait:200"; done
  SHOTGUN_LIBRARY=$LIB SHOTGUN_APP_ID=shotgun-dev SHOTGUN_AUDIO=sink:shotgun-null timeout 600 "$BIN/shotgun" --script "$steps;quit" 2>&1 |
    grep -E 'seek proxy|seek full|play start|playback|audio  |WARN|ERROR' | sed 's/^\[INFO \] shotgun: /  /'
}
rm -rf "$LIB"
echo "== scrub and play, source with a 1 s GOP (the 150 ms full-quality budget applies)"
scrub gop60 uhd60_hevc8_gop60
echo "== scrub and play, long-GOP sources (01-product.md, open point)"
scrub longgop uhd60_hevc8 uhd60_hevc10 fhd60_h264
