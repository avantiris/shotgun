#!/usr/bin/env bash
# S5 spike: compare (a) plain git, (a0) plain git compression=0, (b) git-lfs local, (c) manifest-only.
# Run: nix develop --command bash spikes/s5-git/bench.sh <variant>      variant in: a a0 b c
# Needs /disk/shotgun-dev/s5/raw/{media,text} (see docs/spikes/s5-git.md) and b3sum on PATH for (c).
set -u
V=$1
R=/disk/shotgun-dev/s5
RAW=$R/raw
W=$R/repo-$V
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null   # isolate from the user's git config
export GIT_AUTHOR_NAME=shotgun GIT_AUTHOR_EMAIL=shotgun@localhost GIT_COMMITTER_NAME=shotgun GIT_COMMITTER_EMAIL=shotgun@localhost
t() { # label cmd... -> prints "label<TAB>seconds"
  local label=$1; shift; local s=$EPOCHREALTIME
  "$@" >/dev/null 2>$R/err-$V.log || { echo "FAILED: $label: $(tail -1 $R/err-$V.log)"; }
  awk -v l="$label" -v a="$s" -v b="$EPOCHREALTIME" 'BEGIN{printf "%-44s\t%.3f s\n", l, b-a}'
}
med() { # label n cmd... -> median of n runs in ms
  local label=$1 n=$2; shift 2
  for i in $(seq $n); do local s=$EPOCHREALTIME; "$@" >/dev/null 2>&1; awk -v a="$s" -v b="$EPOCHREALTIME" 'BEGIN{print (b-a)*1000}'; done \
   | sort -n | awk -v l="$label" '{a[NR]=$1} END{printf "%-44s\tmedian %.1f ms  (min %.1f, max %.1f, n=%d)\n", l, a[int((NR+1)/2)], a[1], a[NR], NR}'
}
du_() { echo -e "du $1\t$(du -sbc $2 | tail -1 | cut -f1) bytes apparent, $(du -skc $2 | tail -1 | cut -f1) KiB on disk"; }

echo "=== variant $V  load1=$(cut -d' ' -f1 /proc/loadavg)  $(date +%T)"
rm -rf $W; mkdir -p $W/assets/video $W/assets/audio $W/assets/index $W/sequences $W/.shotgun/trash
cd $W
git init -q -b main .
case $V in
  a0) git config core.compression 0; git config core.bigFileThreshold 1m ;;
  b)  git lfs install --local >/dev/null
      printf 'assets/video/** filter=lfs diff=lfs merge=lfs -text\nassets/audio/** filter=lfs diff=lfs merge=lfs -text\n' > .gitattributes ;;
  c)  printf '.shotgun/\nassets/video/\nassets/audio/\nassets/image/\n' > .gitignore ;;
esac
[ $V != c ] && printf '.shotgun/\n' > .gitignore
cp $RAW/text/f0*.toml sequences/; cp $RAW/text/f1*.toml assets/index/ 2>/dev/null
echo 'name = "s5 test"' > project.toml
t "initial: git add -A (200 text files)" git add -A
t "initial: git commit" git commit -q -m "initial"

# --- import batch: 4 media files, 3.0 GB
t "copy media into assets/ (cp, same for all)" cp $RAW/media/cam-a.mp4 $RAW/media/cam-b.mp4 $RAW/media/broll.mov assets/video/
cp $RAW/media/song.wav assets/audio/
if [ $V = c ]; then
  s=$EPOCHREALTIME
  for f in assets/video/* assets/audio/*; do
    h=$(b3sum --no-names "$f"); printf 'blake3 = "%s"\nsize = %s\nfile = "%s"\n' "$h" "$(stat -c %s "$f")" "$f" > assets/index/${h:0:16}.toml
  done
  awk -v a="$s" -v b="$EPOCHREALTIME" 'BEGIN{printf "%-44s\t%.3f s\n", "b3sum 4 files + write index tomls", b-a}'
fi
t "media: git add -A" git add -A
t "media: git commit" git commit -q -m "import 4 assets"
du_ "worktree media" "assets/video assets/audio"
du_ ".git" .git
du_ "whole project" .

# --- automatic commit touching only text
for i in 1 2 3 4 5 6 7 8 9 10; do
  echo "edit = $i" >> sequences/f001.toml; echo "edit = $i" >> sequences/f002.toml; echo "edit = $i" >> assets/index/f150.toml
  s=$EPOCHREALTIME; git add -A >/dev/null 2>&1; m=$EPOCHREALTIME; git commit -q -m "auto $i" >/dev/null 2>&1
  awk -v a="$s" -v m="$m" -v b="$EPOCHREALTIME" 'BEGIN{printf "%.1f %.1f %.1f\n", (m-a)*1000, (b-m)*1000, (b-a)*1000}'
done | sort -n -k3 | awk '{a[NR]=$1; c[NR]=$2; t[NR]=$3} END{m=int((NR+1)/2); printf "%-44s\tmedian total %.1f ms (add %.1f + commit %.1f), max total %.1f ms, n=%d\n", "text-only auto commit (add -A; commit)", t[m], a[m], c[m], t[NR], NR}'
for i in 1 2 3 4 5 6 7 8 9 10; do
  echo "edit2 = $i" >> sequences/f001.toml
  s=$EPOCHREALTIME; git commit -q -m "auto path $i" -- sequences/f001.toml >/dev/null 2>&1
  awk -v a="$s" -v b="$EPOCHREALTIME" 'BEGIN{print (b-a)*1000}'
done | sort -n | awk '{a[NR]=$1} END{printf "%-44s\tmedian %.1f ms, max %.1f ms, n=%d\n", "text-only commit by explicit path", a[int((NR+1)/2)], a[NR], NR}'

med "git status --porcelain (clean)" 10 git status --porcelain
# mtime change on a big file without content change (e.g. after unzip / copy of the project)
touch assets/video/cam-a.mp4
t "git status after touch of 1.5 GB file (1st)" git status --porcelain
t "git status after touch (2nd)" git status --porcelain

# --- remove an asset
if [ $V = c ]; then
  s=$EPOCHREALTIME; mv assets/video/broll.mov .shotgun/trash/; rm $(grep -l broll.mov assets/index/*.toml)
  git add -A >/dev/null; git commit -q -m "remove broll"; awk -v a="$s" -v b="$EPOCHREALTIME" 'BEGIN{printf "%-44s\t%.3f s\n", "remove asset (mv to trash + commit)", b-a}'
else
  s=$EPOCHREALTIME; git rm -q assets/video/broll.mov; git commit -q -m "remove broll"; awk -v a="$s" -v b="$EPOCHREALTIME" 'BEGIN{printf "%-44s\t%.3f s\n", "remove asset (git rm + commit)", b-a}'
fi
du_ "whole project after removal" .
t "restore removed asset from history" bash -c 'if [ -d .shotgun/trash ] && [ -f .shotgun/trash/broll.mov ]; then git revert --no-edit HEAD -q 2>/dev/null || git revert --no-edit HEAD; mv .shotgun/trash/broll.mov assets/video/; else git revert --no-edit HEAD; fi'
cmp assets/video/broll.mov $RAW/media/broll.mov && echo "restored file identical"
[ $V = a ] || [ $V = a0 ] && { t "git gc (default)" git gc -q; du_ ".git after gc" .git; }
[ $V = b ] && { git lfs dedup 2>&1 | head -2; git lfs ls-files | head -5; }
echo "commits: $(git rev-list --count HEAD)   load1=$(cut -d' ' -f1 /proc/loadavg)"
