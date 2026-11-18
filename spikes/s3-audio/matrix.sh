#!/usr/bin/env bash
# Full measurement set (inside `nix develop`): ./matrix.sh [secs]   -> results/*.txt  (about 20 min)
cd "$(dirname "${BASH_SOURCE[0]}")"
s=${1:-30}
r() { ./run.sh "$@" > /dev/null; }
# 1. idle desktop: every backend at every requested quantum
for q in 64 128 256 1024; do
  r pw-stream-$q $s pw_engine $q $s
  S3_LINK=1 r pw-filter-$q $s pw_filter_engine $q $s
  r cpal-pipewire-$q $s cpal_engine pipewire $q $s
  r cpal-jack-$q $s cpal_engine jack $q $s
  r cpal-alsa-$q $s cpal_engine alsa $q $s
done
# 2. floor, non-RT variant, capture / duplex at the same quantum
r pw-stream-32 $s pw_engine 32 $s
r pw-stream-128-nort $s pw_engine 128 $s --no-rt
r pw-stream-128-capture $s pw_engine 128 $s --capture
S3_LINK=duplex r pw-filter-128-duplex $s pw_filter_engine 128 $s --duplex
r cpal-pipewire-128-capture $s cpal_engine pipewire 128 $s --capture
# 3. CPU load: 16 busy loops on 16 cores (the spike.s own `burn 16`), then a parallel cargo build
export S3_LOAD=burn
r load-pw-stream-64 $s pw_engine 64 $s
r load-pw-stream-128 $s pw_engine 128 $s
r load-pw-stream-256 $s pw_engine 256 $s
S3_LINK=1 r load-pw-filter-128 $s pw_filter_engine 128 $s
r load-cpal-pipewire-128 $s cpal_engine pipewire 128 $s
r load-cpal-jack-128 $s cpal_engine jack 128 $s
r load-cpal-alsa-128 $s cpal_engine alsa 128 $s
export S3_LOAD=cargo
r cargoload-pw-stream-128 $s pw_engine 128 $s

# 4. repeats and build-load at other quanta (added after the first full pass)
unset S3_LOAD
r pw-stream-128-r2 $s pw_engine 128 $s
r cpal-pipewire-64-r2 $s cpal_engine pipewire 64 $s
export S3_LOAD=cargo
r cargoload-pw-stream-256 $s pw_engine 256 $s
r cargoload-pw-stream-128-r2 $s pw_engine 128 $s
r cargoload-pw-stream-64 $s pw_engine 64 $s
rm -rf /disk/shotgun-dev/target-s3/loadbuild
echo extras done
