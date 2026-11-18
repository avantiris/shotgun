#!/usr/bin/env bash
# Idle CPU check: runs the app untouched for <secs> and prints the CPU ticks
# (1/100 s) of every thread shortly after start and shortly before exit. The
# two lines must be equal: an idle app burns nothing.
#   nix develop --command bash scripts/idle-check.sh [app id] [secs]
# App id shotgun-dev = hidden workspace (no redraws at all); shotgun-vis =
# floating and unfocused on HDMI-A-2 (rules set by scripts/bench-m1.sh).
id=${1:-shotgun-dev}; secs=${2:-3}
export SHOTGUN_LIBRARY=${SHOTGUN_BENCH_LIBRARY:-/disk/shotgun-dev/tmp/bench-lib} SHOTGUN_APP_ID=$id
"$CARGO_TARGET_DIR/release/shotgun" --exit-after "$secs" &
pid=$!
snap() {
  for t in /proc/$pid/task/*; do awk '{ print $2, $14 + $15 }' "$t/stat"; done |
    awk '{ a[$1] += $2 } END { for (k in a) printf "%s=%d ", k, a[k]; print "" }'
}
sleep 0.6; echo "t=0.6 s: $(snap)"
sleep "$(awk "BEGIN { print $secs - 1.0 }")"; echo "t=$(awk "BEGIN { print $secs - 0.4 }") s: $(snap)"
wait $pid
