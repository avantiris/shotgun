#!/usr/bin/env bash
# Usage (from anywhere): spikes/s1-decode/run.sh <s1 args...>   (builds if needed)
# NOTE: the dev shell's shellHook exports CARGO_TARGET_DIR=/disk/shotgun-dev/target,
# so the spike's target dir must be set *inside* the shell, not before `nix develop`.
set -euo pipefail
cd "$(dirname "$0")/../.."
exec nix develop --command bash -c 'export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s1; cd spikes/s1-decode && cargo build --release 2>&1 | grep -E "^(warning: unused|error|  -->)" -A12 | head -${S1_ERRLINES:-150}; [ ${PIPESTATUS[0]} -eq 0 ] && exec $CARGO_TARGET_DIR/release/s1 "$@"' _ "$@" 2> >(grep -v -e SQLite -e "is dirty" >&2)
