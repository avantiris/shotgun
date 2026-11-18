#!/usr/bin/env bash
# S5: re-measure the latency-critical operations on the four existing test repos back to back
# (so they share the same machine load). Run: nix develop --command bash spikes/s5-git/recheck.sh
set -u
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=shotgun GIT_AUTHOR_EMAIL=shotgun@localhost GIT_COMMITTER_NAME=shotgun GIT_COMMITTER_EMAIL=shotgun@localhost
N=20
st() { sort -n | awk '{a[NR]=$1} END{printf "median %6.1f  p95 %6.1f  max %6.1f ms", a[int((NR+1)/2)], a[int(NR*0.95)], a[NR]}'; }
echo "load1=$(cut -d' ' -f1 /proc/loadavg) n=$N"
for v in a a0 b c; do
  cd /disk/shotgun-dev/s5/repo-$v
  r1=$(for i in $(seq $N); do echo "r = $RANDOM" >> sequences/f001.toml; echo "r = $RANDOM" >> sequences/f002.toml; echo "r = $RANDOM" >> assets/index/f150.toml
      s=$EPOCHREALTIME; git add -A >/dev/null 2>&1; git commit -q -m "recheck $i" >/dev/null 2>&1; awk -v a=$s -v b=$EPOCHREALTIME 'BEGIN{print (b-a)*1000}'; done | st)
  r2=$(for i in $(seq $N); do echo "r = $RANDOM" >> sequences/f001.toml
      s=$EPOCHREALTIME; git commit -q -m "recheck path $i" -- sequences/f001.toml >/dev/null 2>&1; awk -v a=$s -v b=$EPOCHREALTIME 'BEGIN{print (b-a)*1000}'; done | st)
  r3=$(for i in $(seq $N); do s=$EPOCHREALTIME; git status --porcelain >/dev/null 2>&1; awk -v a=$s -v b=$EPOCHREALTIME 'BEGIN{print (b-a)*1000}'; done | st)
  echo "$v  add -A + commit: $r1 | commit -- path: $r2 | status: $r3"
done
