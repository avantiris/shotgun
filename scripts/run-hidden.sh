#!/usr/bin/env bash
# run-hidden.sh <library> <app args...>: runs the release app for an automated
# check without disturbing the desktop (docs/spikes/README.md).
# Hyprland rules for class shotgun-dev put it on a hidden special workspace
# without focus; screenshots are rendered off-screen by the app itself.
# The rules are runtime rules and vanish on a config reload: set them every time.
for r in 'workspace special:sgdev silent' 'no_initial_focus on' 'no_focus on' 'float on' 'size 1600 900'; do
  hyprctl keyword windowrule "match:class ^(shotgun-dev)\$, $r" >/dev/null
done
lib=$1; shift
export SHOTGUN_KEYS=${SHOTGUN_KEYS:-/disk/shotgun-dev/tmp/no-keys.conf} SHOTGUN_LIBRARY=$lib SHOTGUN_APP_ID=shotgun-dev SHOTGUN_AUDIO=${SHOTGUN_AUDIO:-null} SHOTGUN_NO_CLIPBOARD=1
timeout ${RUN_TIMEOUT:-60} "$CARGO_TARGET_DIR/release/shotgun" "$@"; echo "exit=$?"
