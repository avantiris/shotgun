#!/usr/bin/env bash
# Writes a version into Cargo.toml and Cargo.lock (the release workflow
# does this; the Nix package takes its version from Cargo.toml).
#   bash scripts/set-version.sh 0.4.0
set -euo pipefail
version=${1:?usage: set-version.sh <version>}
if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "not a version: $version" >&2
  exit 1
fi
# (The first `version =` in Cargo.toml is the workspace's.)
sed -i "0,/^version = \".*\"$/s//version = \"$version\"/" Cargo.toml
# The lock file names every crate of the workspace with its version; cargo
# refuses a lock file that disagrees when it may not write it (as in Nix).
awk -v version="$version" '
  ours { sub(/".*"/, "\"" version "\""); ours = 0 }
  /^name = "sg-/ { ours = 1 }
  { print }
' Cargo.lock > Cargo.lock.new
mv Cargo.lock.new Cargo.lock
