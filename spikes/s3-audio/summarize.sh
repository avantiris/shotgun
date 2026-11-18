#!/usr/bin/env bash
# Turns results/*.txt into a markdown table, in chronological order: ./summarize.sh [sink-ERR-baseline]
# Only the first RESULT block (the output stream) of each file is summarised.
# The sink's pw-top ERR counter is cumulative across runs (and shows 0 while the sink is
# suspended), so its per-run delta is computed against the previous run's final value.
cd "$(dirname "${BASH_SOURCE[0]}")/${RESULTS:-results}"
prev=${1:-154}
echo "| run (start time) | loadavg 1m before | frames/callback min/max | period deviation p50 / p99 / max (us) | callback max (us) | own: late / pos gaps / recover flags (steady-state events) | driver clock.xrun delta (frames) | pw-top ERR delta node / sink | reported output latency | callback thread |"
echo "|---|---|---|---|---|---|---|---|---|---|"
for f in $(grep -H -m1 '^### ' *.txt | sed 's/^\([^:]*\):.*(\(.*\))$/\2 \1/' | sort | awk '{print $2}'); do
  t=${f%.txt}
  ts=$(sed -n '1s/.*T\([0-9:]*\)+.*/\1/p' "$f")
  blk=$(awk '/^RESULT/{n++} n==1' "$f")
  fr=$(echo "$blk" | sed -n 's/.*frames\/callback min=\([0-9]*\) max=\([0-9]*\).*/\1\/\2/p')
  dev=$(echo "$blk" | sed -n 's/.*deviation.*p50=\([0-9.]*\) p99=\([0-9.]*\) p99.9=[0-9.]* max=\([0-9.]*\)/\1 \/ \2 \/ \3/p')
  cb=$(echo "$blk" | sed -n 's/.*callback duration us: avg=[0-9.]* max=\([0-9.]*\)/\1/p')
  own=$(echo "$blk" | sed -n 's/.*late(>1.5x period)=\([0-9]*\) position_gaps=\([0-9]*\) xrun_recover_flag=\([0-9]*\).*/\1 \/ \2 \/ \3/p')
  steady=$(echo "$blk" | sed -n 's/.*steady-state event count.*: \([0-9]*\)/\1/p')
  xr=$(echo "$blk" | sed -n 's/.*clock.xrun first=\([0-9]*\) last=\([0-9]*\).*/\1 \2/p' | awk '{print $2-$1}')
  thr=$(echo "$blk" | sed -n 's/.*callback thread: tid=[0-9]* comm=\([^ ]*\) policy=\([A-Z_]*\) rtprio=\([0-9]*\).*/\1 \2 \3/p')
  lat=$(echo "$blk" | sed -n 's/.*pw_time: delay=\([0-9-]*\) frames (\([0-9.]*\) ms).*/pw_time.delay \1 fr = \2 ms/p')
  [ -z "$lat" ] && lat=$(echo "$blk" | sed -n 's/.*cpal timestamp: playback-callback = \([0-9.]*\) ms/cpal playback-callback \1 ms/p')
  errn=$(grep -E "^  .*(s3-pw|cpal|alsa_playback).* ERR=" "$f" | head -1 | sed 's/.*delta=//')
  last=$(grep -E '^  alsa_output.pci-0000_0d.* ERR=' "$f" | sed 's/.*\.\.\([0-9]*\) delta.*/\1/')
  errs=$((last - prev)); prev=$last
  la=$(sed -n 's/^loadavg before: \([0-9.]*\).*/\1/p' "$f")
  case "$t" in *cpal-*|*duplex*) xr="n/a";; esac
  echo "| $t ($ts) | $la | $fr | $dev | $cb | $own ($steady) | $xr | $errn / $errs | $lat | $thr |"
done
