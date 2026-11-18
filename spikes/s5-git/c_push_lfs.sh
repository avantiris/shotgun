#!/usr/bin/env bash
# S5 spike: variant (c) "manifest-only" repo -> push to a remote WITH media, by synthesising a
# commit whose tree = HEAD tree + git-lfs pointer files for every (git-ignored) media file.
# LFS objects are hardlinked from the working files into .git/lfs/objects (no extra disk use,
# same filesystem), pushed, and the links are removed again.
# Run: nix develop --command bash spikes/s5-git/c_push_lfs.sh
set -eu
R=/disk/shotgun-dev/s5; W=$R/repo-c; REMOTE=$R/remote-c.git; CLONE=$R/clone-c
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=shotgun GIT_AUTHOR_EMAIL=shotgun@localhost GIT_COMMITTER_NAME=shotgun GIT_COMMITTER_EMAIL=shotgun@localhost
el() { awk -v l="$1" -v a="$2" -v b="$EPOCHREALTIME" 'BEGIN{printf "%-46s %.3f s\n", l, b-a}'; }
rm -rf $REMOTE $CLONE; git init -q --bare -b main $REMOTE
cd $W
git remote remove origin 2>/dev/null || true; git remote add origin file://$REMOTE
s=$EPOCHREALTIME
export GIT_INDEX_FILE=$W/.git/shotgun-export-index
git read-tree HEAD
links=()
for f in assets/video/* assets/audio/*; do
  [ -f "$f" ] || continue
  sha=$(sha256sum "$f" | cut -d' ' -f1); size=$(stat -c %s "$f")
  blob=$(printf 'version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize %s\n' $sha $size | git hash-object -w --stdin)
  git update-index --add --cacheinfo 100644,$blob,"$f"
  d=.git/lfs/objects/${sha:0:2}/${sha:2:2}; mkdir -p $d; ln -f "$f" $d/$sha; links+=($d/$sha)
done
attr=$(printf 'assets/video/** filter=lfs diff=lfs merge=lfs -text\nassets/audio/** filter=lfs diff=lfs merge=lfs -text\n' | git hash-object -w --stdin)
git update-index --add --cacheinfo 100644,$attr,.gitattributes
# the exported tree must not ignore the media dirs
ign=$(printf '.shotgun/\n' | git hash-object -w --stdin); git update-index --add --cacheinfo 100644,$ign,.gitignore
tree=$(git write-tree); commit=$(git commit-tree $tree -p HEAD -m "export with LFS pointers")
unset GIT_INDEX_FILE; rm -f $W/.git/shotgun-export-index
git update-ref refs/shotgun/export $commit
el "sha256 of media + build export commit" $s
du -sk . | awk '{print "project KiB on disk with hardlinked LFS objects: "$1}'
s=$EPOCHREALTIME
git lfs push origin refs/shotgun/export 2>&1 | tail -1
git push -q --no-verify origin refs/shotgun/export:refs/heads/main
el "git lfs push + git push (file:// remote)" $s
rm -f "${links[@]}"
du -sk $REMOTE | awk '{print "remote KiB: "$1}'
s=$EPOCHREALTIME
git clone -q file://$REMOTE $CLONE 2>&1 | tail -2
el "git clone (pointer files only: no LFS filter configured)" $s
s=$EPOCHREALTIME
# GIT_CONFIG_GLOBAL=/dev/null means the clone has no LFS filter; install it locally and fetch the media
( cd $CLONE && git lfs install --local >/dev/null && git lfs pull 2>&1 | tail -1 )
el "git lfs install --local + git lfs pull" $s
for f in assets/video/* assets/audio/*; do cmp "$f" "$CLONE/$f" && echo "identical in clone: $f"; done
git -C $CLONE log --oneline | head -3
git status --porcelain | head -3; echo "local status lines: $(git status --porcelain | wc -l) (0 = local repo untouched)"
