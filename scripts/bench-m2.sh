#!/usr/bin/env bash
# Milestone M2 budget: asset usable after a local drop of a 1 GB file (<= 2 s).
# Run from the repo root:   nix develop --command bash scripts/bench-m2.sh
#
# "Usable" = the app has hashed and probed the file and added the asset to the
# project; the copy into the bundle continues afterwards. Cold = the file was
# evicted from the page cache first, so the read comes from the disk.
# The app runs hidden (app id shotgun-dev, see scripts/bench-m1.sh).
set -eu
BIN=$CARGO_TARGET_DIR/release
MEDIA=/disk/shotgun-dev/media
BIG=$MEDIA/m2/big1g.mp4
LIB=/disk/shotgun-dev/tmp/bench-m2-lib
cargo build --release -p sg-app 2>&1 | tail -1
mkdir -p "$MEDIA/m2"
# 1.05 GB of 4K60 HEVC: the spike S4 source repeated nine times.
[ -f "$BIG" ] || ffmpeg -v error -y -stream_loop 8 -i "$MEDIA/s4/src/src4k60.mp4" -c copy "$BIG"
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do
  hyprctl keyword windowrule "match:class ^(shotgun-dev)\$, $r" >/dev/null
done
echo "== machine: $(uptime | sed 's/.*load average/load average/'), commit $(git rev-parse --short HEAD), file $(stat -c %s "$BIG") bytes"
rm -rf "$LIB"
run() { # <cold|warm> <n>
  [ "$1" = cold ] && dd if="$BIG" iflag=nocache count=0 2>/dev/null
  # Quits after 8 s: the transcodes that follow are not what is measured here.
  SHOTGUN_LIBRARY=$LIB SHOTGUN_APP_ID=shotgun-dev timeout 60 "$BIN/shotgun" --exit-after 8 \
    --script "new:Drop $1 $2;import:$BIG" 2>&1 | grep -o 'asset usable [0-9]* ms' | sed "s/^/$1: /"
  sync
}
for n in 1 2 3; do run cold $n; done
for n in 1 2 3; do run warm $n; done
