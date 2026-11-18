#!/usr/bin/env bash
# Spike S2 benchmark matrix. Run from the repo root:
#   nix develop --command bash spikes/s2-ui/bench.sh [main|extra]
# Each run opens a window for ~10 s. Logs go to /disk/shotgun-dev/media/s2/logs.
# Before each run it waits (max 3 min) until the machine is quiet (<12 % of all
# cores busy over 1 s), because other work on the machine distorts frame times;
# the busy value seen right before the run is printed and logged.
set -u
export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s2
cd "$(dirname "$0")"
cargo build --release 2>&1 | tail -1
BIN=$CARGO_TARGET_DIR/release/s2-ui
LOG=/disk/shotgun-dev/media/s2/logs; mkdir -p "$LOG"
# Float the window on the 144 Hz monitor (runtime rule, gone after `hyprctl reload`).
hyprctl keyword windowrule 'match:class ^(shotgun-s2)$, float on, monitor HDMI-A-2, size 1800 1000, center on' >/dev/null
busy() { # percent of all CPUs busy over 1 s
  read -r _ a b c d e f g _ < /proc/stat; t1=$((a+b+c+d+e+f+g)); i1=$((d+e))
  sleep 1
  read -r _ a b c d e f g _ < /proc/stat; t2=$((a+b+c+d+e+f+g)); i2=$((d+e))
  echo $(( 100 - 100*(i2-i1)/(t2-t1) ))
}
run() {
  name=$1; shift
  for _ in $(seq 180); do b=$(busy); [ "$b" -lt 12 ] && break; done
  echo "system busy before run: $b %" > "$LOG/$name.log"
  "$BIN" "$@" >> "$LOG/$name.log" 2>&1
  echo "busy_after=$(busy)%" >> "$LOG/$name.log"
  echo "[$name busy_before=$b% $(tail -1 "$LOG/$name.log")] $(grep -E '^RESULT' "$LOG/$name.log")"
}
if [ "${1:-main}" = custom50k ]; then
  for rep in 3 4 5; do run "main-50000-custom-r$rep" --clips 50000 --timeline custom --labels egui --secs 10; done
elif [ "${1:-main}" = main ]; then
  for rep in 1 2; do
    for clips in 500 50000 500000; do
      for tl in egui custom; do
        run "main-$clips-$tl-r$rep" --clips $clips --timeline $tl --labels egui --secs 10
      done
    done
  done
else
  run extra-custom-nolabels   --clips 50000 --timeline custom --labels none --secs 10
  run extra-custom-glyphon    --clips 50000 --timeline custom --labels glyphon --secs 10
  run extra-custom-eguiclip   --clips 50000 --timeline custom --labels egui-clip --secs 10
  run extra-egui-nolabels     --clips 50000 --timeline egui --labels none --secs 10
  run extra-custom-mailbox    --clips 50000 --timeline custom --labels egui --present mailbox --secs 10
  run extra-custom-500k-glyph --clips 500000 --timeline custom --labels glyphon --secs 10
  run extra-custom-500k-nolab --clips 500000 --timeline custom --labels none --secs 10
fi
