#!/usr/bin/env bash
# S4 item 7: GIF pipelines for a short clip (5 s from the 4K60 source, starting at 12 s).
# Reference for PSNR/SSIM = the same scaled frames stored lossless (ffv1 rgb).
set -u
. "$(dirname "$0")/lib.sh"
O=$M/gif; mkdir -p $O
SS=12; T=5
echo -e "pipeline\twidth\tfps\ttime_s\tbytes\tMB\tpsnr_rgb\tssim_rgb\tframes\tload1"
for spec in "480 20" "640 25"; do set -- $spec; W=$1; FPS=$2
  PRE="fps=$FPS,scale=$W:-2:flags=lanczos"
  ref=$O/ref-$W.mkv
  ffmpeg -hide_banner -loglevel error -y -ss $SS -t $T -i $SRC -an -vf "$PRE,format=gbrp" -c:v ffv1 $ref
  m() { # name out seconds
    local ps=$(ffmpeg -hide_banner -i $2 -i $ref -lavfi "[0:v]settb=1/1000,setpts=N,format=gbrp[a];[1:v]settb=1/1000,setpts=N,format=gbrp[b];[a][b]psnr" -f null - 2>&1 | grep -o "average:[0-9.a-z]*" | tail -1)
    local ss=$(ffmpeg -hide_banner -i $2 -i $ref -lavfi "[0:v]settb=1/1000,setpts=N,format=gbrp[a];[1:v]settb=1/1000,setpts=N,format=gbrp[b];[a][b]ssim" -f null - 2>&1 | grep -o "All:[0-9.]*" | tail -1)
    local nf=$(ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 $2 | tr -d ,)
    echo -e "$1\t$W\t$FPS\t$3\t$(fsize $2)\t$(awk -v s=$(fsize $2) 'BEGIN{printf "%.2f", s/1e6}')\t${ps#average:}\t${ss#All:}\t$nf\t$(load1)"; }
  t=$(now); ffmpeg -hide_banner -loglevel error -y -ss $SS -t $T -i $SRC -an -filter_complex "$PRE,split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=sierra2_4a" $O/ff-global-$W.gif; m ffmpeg-palette-global-sierra $O/ff-global-$W.gif $(elapsed $t)
  t=$(now); ffmpeg -hide_banner -loglevel error -y -ss $SS -t $T -i $SRC -an -filter_complex "$PRE,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" $O/ff-bayer-$W.gif; m ffmpeg-palette-diff-bayer5-rect $O/ff-bayer-$W.gif $(elapsed $t)
  t=$(now); ffmpeg -hide_banner -loglevel error -y -ss $SS -t $T -i $SRC -an -filter_complex "$PRE,split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=sierra2_4a" $O/ff-perframe-$W.gif; m ffmpeg-palette-per-frame-sierra $O/ff-perframe-$W.gif $(elapsed $t)
  for q in 100 90 70; do
    t=$(now); ffmpeg -hide_banner -loglevel error -ss $SS -t $T -i $SRC -an -vf "$PRE,format=yuv444p" -f yuv4mpegpipe - | gifski --quiet --y4m-color-override bt709 --fps $FPS --quality $q -o $O/gifski-q$q-$W.gif - ; m gifski-q$q $O/gifski-q$q-$W.gif $(elapsed $t)
  done
  t=$(now); ffmpeg -hide_banner -loglevel error -ss $SS -t $T -i $SRC -an -vf "$PRE,format=yuv444p" -f yuv4mpegpipe - | gifski --quiet --y4m-color-override bt709 --fps $FPS --quality 90 --lossy-quality 60 --motion-quality 80 -o $O/gifski-q90-lossy-$W.gif - ; m gifski-q90-lossy60-motion80 $O/gifski-q90-lossy-$W.gif $(elapsed $t)
done
# control: gifski without the colour override (y4m carries no matrix tag) and with PNG frame input
W=480; FPS=20; PRE="fps=$FPS,scale=$W:-2:flags=lanczos"; ref=$O/ref-$W.mkv
t=$(now); ffmpeg -hide_banner -loglevel error -ss $SS -t $T -i $SRC -an -vf "$PRE,format=yuv444p" -f yuv4mpegpipe - | gifski --quiet --fps $FPS --quality 90 -o $O/gifski-q90-nooverride-$W.gif - ; m gifski-q90-y4m-no-colour-override $O/gifski-q90-nooverride-$W.gif $(elapsed $t)
t=$(now); mkdir -p $O/png; rm -f $O/png/*.png; ffmpeg -hide_banner -loglevel error -ss $SS -t $T -i $SRC -an -vf "$PRE" $O/png/f%04d.png; gifski --quiet --fps $FPS --quality 90 -o $O/gifski-q90-png-$W.gif $O/png/f*.png; m gifski-q90-png-input $O/gifski-q90-png-$W.gif $(elapsed $t)
