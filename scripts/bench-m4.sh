#!/usr/bin/env bash
# Milestone M4 budget: UI frame time <= 6.9 ms p99 for any project size.
# Run from the repo root:   nix develop --command bash scripts/bench-m4.sh
#
# The app renders 1440 frames off-screen (hidden window, app id shotgun-dev)
# while the timeline zooms between "whole sequence" and 200 points per second
# and sweeps across the sequence: the script of spike S2. The time is the CPU
# time of a whole UI frame: input, UI build, timeline lookup and instance
# fill, tessellation, buffer upload, encoding and submit.
set -eu
BIN=$CARGO_TARGET_DIR/release
LIB=/disk/shotgun-dev/tmp/bench-m4-lib
cargo build --release -p sg-app -p sg-bench 2>&1 | tail -1
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do
  hyprctl keyword windowrule "match:class ^(shotgun-dev)\$, $r" >/dev/null
done
echo "== machine: $(uptime | sed 's/.*load average/load average/'), commit $(git rev-parse --short HEAD)"
rm -rf "$LIB"
"$BIN/sg-bench" gen "$LIB" "Ten" 10 2 | head -1
"$BIN/sg-bench" gen "$LIB" "Fifty k on twenty" 50000 20 | head -1
"$BIN/sg-bench" gen "$LIB" "Fifty k on four" 50000 4 | head -1
"$BIN/sg-bench" gen "$LIB" "Half a million" 500000 20 | head -1
run() { # <folder> <label>
  for _ in 1 2 3; do
    SHOTGUN_LIBRARY=$LIB SHOTGUN_APP_ID=shotgun-dev SHOTGUN_AUDIO=null SHOTGUN_LOG=error timeout 300 "$BIN/shotgun" \
      --script "open:$1;timelinebench:1440;quit" | grep '^BENCH' | sed "s/^BENCH timeline/  $2:/"
  done
}
echo "== frame time, window 1600x900 (budget 6.9 ms p99)"
run ten "10 clips, 2 tracks (rows 40 pt, labels)"
run fifty-k-on-twenty "50 000 clips, 20 tracks (rows 10 pt, no labels)"
run fifty-k-on-four "50 000 clips, 4 tracks (rows 40 pt, labels)"
run half-a-million "500 000 clips, 20 tracks"
