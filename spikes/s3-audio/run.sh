#!/usr/bin/env bash
# usage (inside `nix develop`): ./run.sh <outfile-tag> <secs> <binary> <args...>   (secs must equal the secs argument given to the binary)
# Runs the engine, samples PipeWire's own view (pw-top ERR column, node Latency params)
# while it runs, and stores everything in results/<tag>.txt
set -u
cd "$(dirname "${BASH_SOURCE[0]}")"
source ./env.sh
tag=$1; shift
secs=$1; shift
bin=$1; shift
mkdir -p results
out=results/$tag.txt
{
  echo "### $tag: $bin $* ($(date -Is))"
  echo "loadavg before: $(cat /proc/loadavg)"
  echo "settings before: $(pw-metadata -n settings 0 2>/dev/null | grep -E 'quantum' | tr '\n' ' ')"
} > "$out"
loadpid=""
case "${S3_LOAD:-}" in
  burn) "$CARGO_TARGET_DIR/release/burn" 16 $((secs + 20)) > /dev/null 2>&1 & loadpid=$!; sleep 2;;
  cargo)  # parallel release build of this spike into a scratch target dir on /disk
          rm -rf /disk/shotgun-dev/target-s3/loadbuild
          CARGO_TARGET_DIR=/disk/shotgun-dev/target-s3/loadbuild cargo build --release -j 16 > /dev/null 2>&1 & loadpid=$!; sleep 2;;
esac
[ -n "$loadpid" ] && echo "load: $S3_LOAD (started before the engine)" >> "$out"
# PipeWire's own view for the whole run: pw-top starts before the engine
pw-top -b -n $((secs + 6)) > "$out.top" 2>&1 &
toppid=$!
sleep 2.5
"$CARGO_TARGET_DIR/release/$bin" "$@" >> "$out.run" 2>&1 &
pid=$!
sleep 1
if [ "${S3_LINK:-}" != "" ]; then
  # pw_filter nodes are not auto-linked by WirePlumber: link by hand (see doc)
  pw-link s3-pw-filter:out_FL alsa_output.pci-0000_0d_00.4.analog-stereo:playback_FL
  pw-link s3-pw-filter:out_FR alsa_output.pci-0000_0d_00.4.analog-stereo:playback_FR
  if [ "$S3_LINK" = duplex ]; then
    pw-link alsa_input.pci-0000_0d_00.4.analog-stereo:capture_FL s3-pw-filter:in_FL
    pw-link alsa_input.pci-0000_0d_00.4.analog-stereo:capture_FR s3-pw-filter:in_FR
  fi
fi
sleep 2
{
  echo "--- pw-dump (t=3s): our nodes and the default sink/source"
  pw-dump | jq -c '.[] | select(.type|endswith("Node")) | select((.info.props["node.name"]|tostring|test("s3-|alsa_output.pci-0000_0d|alsa_input.pci-0000_0d")) or (.info.props["application.name"]|tostring|test("s3|cpal|alsa plug-in|ALSA plug-in"))) | {id, name:.info.props["node.name"], app:.info.props["application.name"], node_latency:.info.props["node.latency"], state:.info.state, Latency:.info.params.Latency, ProcessLatency:.info.params.ProcessLatency}'
  echo "--- threads of the engine (t=3s): tid class rtprio nice comm"
  ps -Lo tid,cls,rtprio,ni,comm -p $pid
} >> "$out"
wait $pid
sleep 1.5
kill $toppid 2>/dev/null; wait $toppid 2>/dev/null
n=$(grep -c "^S  *ID" "$out.top")
[ -n "$loadpid" ] && { echo "load still running at engine exit: $(kill -0 $loadpid 2>/dev/null && echo yes || echo no)" >> "$out"; kill $loadpid 2>/dev/null; wait $loadpid 2>/dev/null; }
{
  echo "--- pw-top -b, whole run ($n iterations, starts 2.5 s before the engine, first iteration ignored): last running row per node"
  awk '/^S +ID/{it++} it>=2 && $1=="R" && $3>0 {row[$NF]=$0} END{for(k in row) print row[k]}' "$out.top"| grep -E "^S +ID|^R "
  echo "ERR per node, first..last value seen in this window (ERR is cumulative per node):"
  awk '/^S +ID/{it++} it>=2 && ($1=="R" || $1=="S" || $1=="I"){ k=$NF; if(!(k in f)) f[k]=$9+0; l[k]=$9+0 } END{for(k in f) printf "  %s ERR=%d..%d delta=%d\n", k, f[k], l[k], l[k]-f[k]}' "$out.top"
  echo "--- engine output"
  cat "$out.run"
  echo "loadavg after: $(cat /proc/loadavg)"
  echo "settings after: $(pw-metadata -n settings 0 2>/dev/null | grep -E 'quantum' | tr '\n' ' ')"
} >> "$out"
rm -f "$out.run" "$out.top"
cat "$out"
