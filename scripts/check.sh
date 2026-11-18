#!/usr/bin/env bash
# The checks a commit must pass: format, lints as errors, all tests.
# Run from the repo root:   nix develop --command bash scripts/check.sh
# Exits non-zero on the first failure, so it can gate a commit:
#   nix develop --command bash scripts/check.sh && git commit ...
set -euo pipefail
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
