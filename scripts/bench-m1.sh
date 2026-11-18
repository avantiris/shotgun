#!/usr/bin/env bash
# Milestone M1 budgets: cold start to library, open project, edit op, journal append.
# Run from the repo root:   nix develop --command bash scripts/bench-m1.sh [visible]
#
# The app runs with the app id "shotgun-dev", which Hyprland rules (added
# here at runtime, gone after `hyprctl reload`) put on a hidden special
# workspace without focus. With "visible" one extra start and one extra open
# run float unfocused on HDMI-A-2 for a fraction of a second each, to confirm
# the hidden numbers with a window that is really on screen.
set -eu
BIN=$CARGO_TARGET_DIR/release
TMP=/disk/shotgun-dev/tmp
LIB=$TMP/bench-lib
REPS=10
cargo build --release -p sg-app -p sg-bench 2>&1 | tail -1

rule() { hyprctl keyword windowrule "match:class ^($1)\$, $2" >/dev/null; }
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do rule shotgun-dev "$r"; done

echo "== machine: $(uptime | sed 's/.*load average/load average/'), commit $(git rev-parse --short HEAD 2>/dev/null || echo none)"

echo "== edit ops (budget 1 ms)"
"$BIN/sg-bench" ops 50000
echo "== journal append on /disk (budget 1 ms)"
"$BIN/sg-bench" journal "$TMP" 50000

echo "== synthetic projects"
rm -rf "$LIB"
"$BIN/sg-bench" gen "$LIB" "Bench 50k" 50000
"$BIN/sg-bench" gen "$LIB" "Small" 10
for i in $(seq 1 30); do "$BIN/sg-bench" gen "$LIB" "Filler $i" 10 >/dev/null; done
echo "library: $(ls "$LIB" | wc -l) projects"
echo "== open bundle, no window (budget 500 ms)"
"$BIN/sg-bench" open "$LIB/bench-50k" $REPS
"$BIN/sg-bench" open "$LIB/small" $REPS

# Runs the app $REPS times; prints the app's own time (from main() to the
# presented frame) and the wall time from before exec to that line.
app() { # <label> <app id> <reps> <args...>
  label=$1; id=$2; reps=$3; shift 3
  for _ in $(seq "$reps"); do
    t0=$(date +%s%N)
    line=$(SHOTGUN_LIBRARY=$LIB SHOTGUN_APP_ID=$id SHOTGUN_LOG=error timeout 20 "$BIN/shotgun" "$@" | grep -m1 '^BENCH' || echo "BENCH failed")
    t1=$(date +%s%N)
    echo "$label wall_incl_exit_ms=$(( (t1 - t0) / 1000000 )) $line"
  done
}
summary() { # reads app() lines; prints min/median/max of a key
  awk -v key="$1" '{ for (i = 1; i <= NF; i++) if (split($i, kv, "=") == 2 && kv[1] == key) v[++n] = kv[2] }
    END { if (!n) { print "  " key ": no data"; exit }
          for (i = 1; i <= n; i++) for (j = i + 1; j <= n; j++) if (v[j] < v[i]) { t = v[i]; v[i] = v[j]; v[j] = t }
          printf "  %-18s n=%d min %.1f  median %.1f  max %.1f ms\n", key, n, v[1], v[int((n + 1) / 2)], v[n] }'
}

echo "== start to library frame, hidden workspace (budget 300 ms)"
app start shotgun-dev $REPS --bench-start | tee "$TMP/bench-start.txt" | summary library_frame_ms
summary wall_incl_exit_ms < "$TMP/bench-start.txt"
echo "== open 50 000-clip project to editor frame, hidden workspace (budget 500 ms)"
app open50k shotgun-dev $REPS --open bench-50k --bench-open | tee "$TMP/bench-open.txt" | summary open_ms
echo "== open small project to editor frame"
app opensmall shotgun-dev $REPS --open small --bench-open | summary open_ms

if [ "${1:-}" = visible ]; then
  for r in 'float on' 'monitor HDMI-A-2' 'size 1600 900' 'no_initial_focus on' 'no_focus on'; do rule shotgun-vis "$r"; done
  echo "== one visible run each on HDMI-A-2"
  app start-visible shotgun-vis 1 --bench-start
  app open50k-visible shotgun-vis 1 --open bench-50k --bench-open
fi
