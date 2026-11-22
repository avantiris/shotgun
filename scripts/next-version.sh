#!/usr/bin/env bash
# Prints the version the next release gets, or nothing when the commits
# since the last release do not call for one. Run from the repo root.
#
# The commits decide (conventional commits, https://www.conventionalcommits.org):
#   fix: perf:                         0.3.1 -> 0.3.2
#   feat:                              0.3.1 -> 0.4.0
#   feat!: or "BREAKING CHANGE:"       1.3.1 -> 2.0.0  (below 1.0.0: 0.3.1 -> 0.4.0)
#   anything else (docs:, chore:, ...) no release
# With no release yet, the first one gets the version in Cargo.toml.
set -euo pipefail

# The newest release by number, not by ancestry: a release's commit may not
# be on main (the workflow tags what it built, then brings main up to date).
last=$(git tag --list 'v[0-9]*' --sort=-v:refname | head -n 1)
if [ -z "$last" ]; then
  sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1
  exit 0
fi

bump=0 # 0 none, 1 patch, 2 minor, 3 major
while IFS= read -r -d '' message; do
  subject=${message%%$'\n'*}
  level=0
  if [[ $subject =~ ^[a-z]+(\([^\)]*\))?!: ]] || grep -qE '^BREAKING[ -]CHANGE: ' <<<"$message"; then
    level=3
  elif [[ $subject =~ ^feat(\([^\)]*\))?: ]]; then
    level=2
  elif [[ $subject =~ ^(fix|perf)(\([^\)]*\))?: ]]; then
    level=1
  fi
  if [ "$level" -gt "$bump" ]; then bump=$level; fi
done < <(git log -z --format=%B "$last..HEAD")

IFS=. read -r major minor patch <<<"${last#v}"
# Below 1.0.0 nothing is promised yet: a breaking change is a minor step.
if [ "$bump" -eq 3 ] && [ "$major" -eq 0 ]; then bump=2; fi
case $bump in
  3) echo "$((major + 1)).0.0" ;;
  2) echo "$major.$((minor + 1)).0" ;;
  1) echo "$major.$minor.$((patch + 1))" ;;
esac
