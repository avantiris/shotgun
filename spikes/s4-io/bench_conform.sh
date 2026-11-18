#!/usr/bin/env bash
# S4 item 4: conform (mezzanine) candidates.
#  Part 1: opaque video. 4K60 source -> CFR mezzanine; speed, size, PSNR/VMAF (n_subsample=5), sw decode fps.
#  Part 2: alpha. Synthetic 720x720 30 fps 10 s RGBA clip (real footage + moving alpha mask) ->
#          alpha-capable codecs; speed, size, decode fps, is the alpha plane preserved.
#          (PSNR inputs get identical timestamps via settb/setpts=N, otherwise mkv ms rounding
#          vs mov timestamps pairs the wrong frames.)
set -u
. "$(dirname "$0")/lib.sh"
O=$M/conform; mkdir -p $O
D=$(dur $SRC)
HWIN="-hwaccel vaapi -hwaccel_device $VADEV -hwaccel_output_format vaapi"
part=${1:-all}
if [ $part = all ] || [ $part = 1 ]; then
echo -e "codec\tenc_s\tenc_fps\tx_rt\tGB_per_min\tMbit_s\tvmaf\tpsnr_y\tdec_fps_sw\tload1"
p1() { local name=$1 ext=$2; shift 2; local out=$O/$name.$ext
  local l=$(load1) t=$(now)
  ffmpeg -hide_banner -loglevel error -y "$@" -an $out || { echo -e "$name\tFAILED"; return; }
  local e=$(elapsed $t); local sz=$(fsize $out)
  local j=$O/$name.json
  ffmpeg -hide_banner -loglevel error -i $out -i $SRC -an -lavfi "[0:v]setpts=PTS-STARTPTS,format=yuv420p10le[d];[1:v]setpts=PTS-STARTPTS,format=yuv420p10le[r];[d][r]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=16:n_subsample=5:feature=name=psnr:log_fmt=json:log_path=$j" -f null -
  t=$(now); ffmpeg -hide_banner -loglevel error -i $out -an -f null -; local dd=$(elapsed $t)
  awk -v n=$name -v e=$e -v d=$D -v sz=$sz -v vm=$(jq -r .pooled_metrics.vmaf.mean $j) -v ps=$(jq -r .pooled_metrics.psnr_y.mean $j) -v dd=$dd -v l=$l \
   'BEGIN{printf "%s\t%.1f\t%.0f\t%.2f\t%.2f\t%.0f\t%.2f\t%.2f\t%.0f\t%s\n", n, e, 1799/e, d/e, sz/1e9*60/d, sz*8/d/1e6, vm, ps, 1799/dd, l}'
}
p1 x264-intra-crf14-vf   mp4 -i $SRC -vf fps=60000/1001 -c:v libx264 -preset veryfast -crf 14 -g 1 -pix_fmt yuv420p
p1 x264-gop15-crf14-vf   mp4 -i $SRC -vf fps=60000/1001 -c:v libx264 -preset veryfast -crf 14 -g 15 -bf 0 -pix_fmt yuv420p
p1 h264_vaapi-intra-qp14 mp4 $HWIN -i $SRC -vf fps=60000/1001 -c:v h264_vaapi -g 1 -bf 0 -rc_mode CQP -qp 14
p1 hevc_vaapi-intra-qp14 mp4 $HWIN -i $SRC -vf fps=60000/1001 -c:v hevc_vaapi -g 1 -bf 0 -rc_mode CQP -qp 14
p1 hevc_vaapi-gop15-qp14 mp4 $HWIN -i $SRC -vf fps=60000/1001 -c:v hevc_vaapi -g 15 -bf 0 -rc_mode CQP -qp 14
p1 dnxhr-hq              mov -i $SRC -vf fps=60000/1001 -c:v dnxhd -profile:v dnxhr_hq -pix_fmt yuv422p
p1 prores-422            mov -i $SRC -vf fps=60000/1001 -c:v prores_aw -profile:v 2 -pix_fmt yuv422p10le
fi
if [ $part = all ] || [ $part = 2 ]; then
A=$O/alpha; mkdir -p $A
# RGBA test clip: downscaled footage, alpha = moving soft circle (0..255), stored lossless as reference
[ -f $A/ref.mkv ] || ffmpeg -hide_banner -loglevel error -y -t 10 -i $SRC -an -vf "fps=30000/1001,crop=2160:2160,scale=720:720,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='clip(255-2*hypot(X-360-200*sin(T*2),Y-360),0,255)'" -c:v ffv1 -level 3 -pix_fmt bgra $A/ref.mkv
NF=$(ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 $A/ref.mkv | tr -d ,)
echo -e "codec\tout_pix_fmt\tenc_s\tenc_fps\tMB_per_min\tdec_fps\talpha_psnr\trgb_psnr\tload1   (ref: $NF frames 720x720 rgba, $(fsize $A/ref.mkv) bytes ffv1)"
p2() { local name=$1 ext=$2; shift 2; local out=$A/$name.$ext
  local l=$(load1) t=$(now)
  ffmpeg -hide_banner -loglevel error -y -i $A/ref.mkv "$@" $out 2>$A/$name.err || { echo -e "$name\tFAILED: $(tail -1 $A/$name.err | cut -c1-120)"; return; }
  local e=$(elapsed $t); local sz=$(fsize $out)
  local pf=$(ffprobe -v error -select_streams v:0 -show_entries stream=pix_fmt -of csv=p=0 $out)
  t=$(now); ffmpeg -hide_banner -loglevel error -i $out -f null -; local dd=$(elapsed $t)
  local ap=$(ffmpeg -hide_banner -i $out -i $A/ref.mkv -lavfi "[0:v]settb=1/30000,setpts=N*1001,format=rgba,alphaextract[a];[1:v]settb=1/30000,setpts=N*1001,format=rgba,alphaextract[b];[a][b]psnr" -f null - 2>&1 | grep -o "average:[0-9.a-z]*" | tail -1)
  local rp=$(ffmpeg -hide_banner -i $out -i $A/ref.mkv -lavfi "[0:v]settb=1/30000,setpts=N*1001,format=rgb24[a];[1:v]settb=1/30000,setpts=N*1001,format=rgb24[b];[a][b]psnr" -f null - 2>&1 | grep -o "average:[0-9.a-z]*" | tail -1)
  awk -v n=$name -v pf=$pf -v e=$e -v nf=$NF -v sz=$sz -v dd=$dd -v ap=${ap#average:} -v rp=${rp#average:} -v l=$l 'BEGIN{printf "%s\t%s\t%.2f\t%.0f\t%.1f\t%.0f\t%s\t%s\t%s\n", n, pf, e, nf/e, sz/1e6*6, nf/dd, ap, rp, l}'
}
p2 prores4444-ks   mov -c:v prores_ks -profile:v 4 -pix_fmt yuva444p10le -alpha_bits 16
p2 prores4444-aw   mov -c:v prores_aw -profile:v 4 -pix_fmt yuva444p10le
p2 utvideo-rgba    mkv -c:v utvideo -pix_fmt gbrap
p2 ffv1-bgra       mkv -c:v ffv1 -level 3 -slices 16 -threads 16 -pix_fmt bgra
p2 qtrle-argb      mov -c:v qtrle -pix_fmt argb
p2 png-rgba        mov -c:v png -pix_fmt rgba
p2 cfhd-gbrap12    mov -c:v cfhd -quality film1 -pix_fmt gbrap12le
p2 vp9-yuva420p    webm -c:v libvpx-vp9 -pix_fmt yuva420p -crf 15 -b:v 0 -row-mt 1 -deadline good -cpu-used 4
p2 hevc_vaapi-noalpha mp4 -vaapi_device $VADEV -vf format=nv12,hwupload -c:v hevc_vaapi -rc_mode CQP -qp 14
fi
