#!/usr/bin/env bash
# Scripted clipboard paste check (no human needed). From the repo root:
#   nix develop --command bash spikes/s2-ui/scripts/paste_test.sh
# Puts a URL, a PNG and a file list on the clipboard with wl-copy, starts dnd_test
# with --auto-paste and prints what the app received. Overwrites the clipboard
# (the previous plain-text content is restored at the end).
export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s2
BIN=$CARGO_TARGET_DIR/release/dnd_test
M=/disk/shotgun-dev/media/s2; mkdir -p "$M/dnd-src"
echo hello > "$M/dnd-src/drag me ä.txt"
saved=$(wl-paste -n 2>/dev/null)
one() { # the window must have keyboard focus to see the selection
  ( sleep 1.2; hyprctl dispatch focuswindow 'class:^(shotgun-s2-dnd)$' >/dev/null ) &
  "$BIN" --secs 4 --auto-paste 2.5 2>&1 | grep -E 'PASTE RESULT|FOCUS true|CLIPBOARD' | cut -c1-400
}
echo "== text URL";  wl-copy 'https://www.youtube.com/watch?v=dQw4w9WgXcQ'; one
echo "== image/png"; ffmpeg -loglevel error -y -f lavfi -i testsrc=size=320x180 -frames:v 1 "$M/clip.png"; wl-copy -t image/png < "$M/clip.png"; ls -l "$M/clip.png" | awk '{print "source file bytes:", $5}'; one
echo "== file list"; printf 'file://%s/dnd-src/drag%%20me%%20%%C3%%A4.txt\r\n' "$M" | wl-copy -t text/uri-list; one
[ -n "$saved" ] && wl-copy -- "$saved"
