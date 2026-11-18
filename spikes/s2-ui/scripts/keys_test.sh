#!/usr/bin/env bash
# Physical-vs-logical key check with synthetic evdev input (needs write access to /dev/uinput).
#   nix develop --command bash spikes/s2-ui/scripts/keys_test.sh
# Taps the keys at evdev positions KEY_Y(21) KEY_Z(44) KEY_SEMICOLON(39) KEY_LEFTBRACE(26)
# KEY_GRAVE(41) KEY_MINUS(12) KEY_A(30) and Shift+KEY_2. Keys are only sent while the
# spike window has focus.
export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s2
B=$CARGO_TARGET_DIR/release
hyprctl keyword windowrule 'match:class ^(shotgun-s2)$, float on, monitor HDMI-A-2, size 1800 1000, center on' >/dev/null
( sleep 1.5; hyprctl dispatch focuswindow 'class:^(shotgun-s2)$' >/dev/null
  "$B/inject" --require-class shotgun-s2 t21 t44 t39 t26 t41 t12 t30 d42 t3 u42 ) &
"$B/s2-ui" --mode keys --secs 6 --clips 500 2>&1 | grep -E '^KEY Pressed|inject'
wait
hyprctl devices | grep -B2 -A8 shotgun-s2-inject | grep -E 'Keyboard|shotgun|keymap' | head -4
