#!/usr/bin/env bash
# Scripted file drag from Nautilus into dnd_test using a synthetic pointer
# (/dev/uinput button + hyprctl cursor warps). Takes over the mouse for ~3 s.
#   nix develop --command bash spikes/s2-ui/scripts/dnd_drive.sh
# The file-row offset (+260,+87 inside the Nautilus window) matches Nautilus 4x
# list view on this machine; adjust if the drag starts on empty space.
export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s2
B=$CARGO_TARGET_DIR/release
M=/disk/shotgun-dev/media/s2; mkdir -p "$M/dnd-src" "$M/logs"
[ -e "$M/dnd-src/drag me ä.txt" ] || echo hello > "$M/dnd-src/drag me ä.txt"
geo() { hyprctl clients | awk -v c="$1" -v t="$2" '/^\tat: /{at=$2} /^\tsize: /{sz=$2} /^\tclass: /{cl=$2} /^\ttitle: /{ if (cl==c && index($0,t)) {print at","sz; exit} }'; }
[ -n "$(geo org.gnome.Nautilus dnd-src)" ] || { setsid nautilus --new-window "$M/dnd-src" >/dev/null 2>&1 & sleep 3; }
"$B/dnd_test" --secs 12 > "$M/logs/dnd.log" 2>&1 &
sleep 2.5
IFS=, read -r nx ny _ _ <<< "$(geo org.gnome.Nautilus dnd-src)"
IFS=, read -r tx ty tw th <<< "$(geo shotgun-s2-dnd 'DnD')"
[ -n "$nx" ] && [ -n "$tx" ] || { echo "windows not found"; exit 1; }
sx=$((nx+260)); sy=$((ny+87)); dx=$((tx+tw/2)); dy=$((ty+th/2))
echo "drag from $sx,$sy (Nautilus) to $dx,$dy (dnd_test)"
old=$(hyprctl cursorpos | tr -d ' ')
"$B/inject" c$sx,$sy s250 d272 s200 m3,2 m3,2 m3,2 m3,2 m3,2 m3,2 m3,2 m3,2 s300 \
            c$((dx-40)),$((dy-30)) m2,2 m2,2 m2,2 s200 c$dx,$dy m1,1 m1,1 m-1,-1 s500 u272 s300 c$old
wait
cat "$M/logs/dnd.log"
