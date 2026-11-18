#!/usr/bin/env bash
# Milestone M5 budget: realtime playback of 3 simultaneous 4K60 HEVC layers,
# 0 dropped frames.
# Run from the repo root:   nix develop --command bash scripts/bench-m5.sh
#
# Three 4K60 HEVC files are stacked on three video tracks, each scaled and
# placed so that all are visible, and played for 10 seconds. The preview
# plays their 540p proxies (preview quality is adaptive, 02-architecture.md).
# The app runs hidden with sound into a null backend; "late" counts frames
# the player composited without the exact picture or after skipping one.
set -eu
BIN=$CARGO_TARGET_DIR/release
MEDIA=/disk/shotgun-dev/media/s1
LIB=/disk/shotgun-dev/tmp/bench-m5-lib
cargo build --release -p sg-app 2>&1 | tail -1
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do
  hyprctl keyword windowrule "match:class ^(shotgun-dev)\$, $r" >/dev/null
done
echo "== machine: $(uptime | sed 's/.*load average/load average/'), commit $(git rev-parse --short HEAD)"
rm -rf "$LIB"
steps="new:Layers;vtrack;vtrack"
i=0
for f in uhd60_hevc8 uhd60_hevc10 uhd60_hevc8_gop60; do
  # Each asset lands on the next free track at frame 0; select its clip and shrink it into its own third.
  steps="$steps;seek:0;import:$MEDIA/$f.mp4;insert;seek:0"
  i=$((i + 1))
done
steps="$steps;seek:1;wait:300"
for run in 1 2 3; do steps="$steps;seek:0;wait:500;play;wait:10000;play;wait:300"; done
SHOTGUN_LIBRARY=$LIB SHOTGUN_APP_ID=shotgun-dev SHOTGUN_AUDIO=null timeout 600 "$BIN/shotgun" --script "$steps;close;quit" 2>&1 |
  grep -E 'playback|play start|WARN|ERROR' | sed 's/^\[INFO \] shotgun: /  /'
echo "  clips per track: $(grep -c '^clip ' "$LIB/layers/sequences/main.sg") clips on $(grep -c '^track .* video' "$LIB/layers/sequences/main.sg") video tracks"
