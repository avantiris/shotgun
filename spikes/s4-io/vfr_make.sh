#!/usr/bin/env bash
# S4 item 2: create VFR/CFR test files from the 4K60 source.
set -u
. "$(dirname "$0")/lib.sh"
O=$M/vfr; mkdir -p $O
X="-c:v libx264 -preset veryfast -crf 22 -pix_fmt yuv420p -an"
F="ffmpeg -hide_banner -loglevel error -y -i $SRC"
# control: CFR mp4 and CFR mkv (mkv stores timestamps in ms -> 16/17 ms packet deltas)
$F -vf scale=1280:720 $X $O/cfr.mp4
ffmpeg -hide_banner -loglevel error -y -i $O/cfr.mp4 -c copy $O/cfr.mkv
# deterministic drops: every 5th frame missing plus ~20% random drops
$F -vf "scale=1280:720,select='gt(mod(n\,5)\,0)*gt(random(1)\,0.2)'" -fps_mode vfr $X $O/vfr_drop.mp4
# phone-like: nominal 30 fps, every timestamp jittered by 0..6 ms, ~1% drops
$F -vf "scale=1280:720,fps=30000/1001,select='gt(random(1)\,0.01)',setpts='PTS+random(2)*0.006/TB'" -fps_mode vfr -video_track_timescale 90000 $X $O/vfr_jitter.mp4
# CFR for the first 24 s, frame drops only after that (defeats "scan the first N seconds")
$F -vf "scale=1280:720,select='lt(t\,24)+gt(random(1)\,0.3)'" -fps_mode vfr $X $O/vfr_late.mp4
# one single dropped frame at t=15 s
$F -vf "scale=1280:720,select='not(eq(n\,900))'" -fps_mode vfr $X $O/vfr_onedrop.mp4
# rotated (display matrix) copy for the probe test
ffmpeg -hide_banner -loglevel error -y -display_rotation 90 -i $O/cfr.mp4 -c copy $O/rot90.mp4
ls -la $O
