# S4 spike helpers. Source from the other scripts. Run via: nix develop --command bash spikes/s4-io/<script>
M=/disk/shotgun-dev/media/s4
SRC=$M/src/src4k60.mp4
VADEV=/dev/dri/renderD128
now() { echo "$EPOCHREALTIME"; }
# elapsed <start> -> seconds with 3 decimals
elapsed() { awk -v a="$1" -v b="$EPOCHREALTIME" 'BEGIN{printf "%.3f", b-a}'; }
load1() { cut -d' ' -f1 /proc/loadavg; }
fsize() { stat -c %s "$1"; }
dur() { ffprobe -v error -show_entries format=duration -of csv=p=0 "$1"; }
