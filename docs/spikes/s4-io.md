# S4 Import/export tooling

Questions: how to import from links (yt-dlp, Giphy, Google Photos, plain HTTP), how to
probe and detect variable frame rate, which proxy codec to scrub on, which conform
(mezzanine) target, which encoders for masters and size-targeted copies, and how to get a
GIF/MP4 onto the Wayland clipboard.

Status: measured on 2026-11-18. Statements are measured unless marked **not tested** or
**reasoned**.

## Environment and caveats

| Item | Value |
|------|-------|
| Machine | AMD Ryzen 7 3800X (8C/16T), 16 GB RAM, RX 7800 XT (Mesa VAAPI, `/dev/dri/renderD128`), Hyprland |
| Tools (dev shell) | ffmpeg-full 9.0.2 (libvmaf, libx264, libx265, libsvtav1, VAAPI), yt-dlp 2026.10.26, gifski 1.34.0, wl-clipboard 2.3.0, mediainfo 26.10 |
| VAAPI encode entrypoints (`vainfo`) | H.264 CB/Main/High, HEVC Main + Main10, AV1 Profile0 |
| Media location | `/disk/shotgun-dev/media/s4/` (SATA SSD, ext4) |
| Scripts | `spikes/s4-io/*.sh`, `*.py`, `clip-multi/` (run with `nix develop --command bash spikes/s4-io/<script>`) |

**Load caveat (applies to every timing in this document).** Other spike agents were
compiling Rust and encoding video on the same machine during the whole session; the
1-minute load average was between 4 and 45 on 16 threads. Each table row carries the
`load1` value at the start of that measurement where the script recorded it. Speeds are
therefore lower bounds, and rows measured at different loads are only roughly comparable.
Sizes and quality scores are not affected.

**Test source.** `src/src4k60.mp4`: 3840x2160, 59.94 fps CFR, HEVC Main 8-bit yuv420p,
BT.709 limited, 30.01 s, 1799 frames, 30.7 Mbit/s video + AAC, 116 MB. It is real camera
footage (nature, pans, a spider close-up): 35 s of the public YouTube video
`LXb3EKWsInQ` ("COSTA RICA IN 4K 60fps HDR"), SDR VP9 rendition (format 315), re-encoded
once with `libx265 -preset fast -crf 12`. Consequence: the source has already been
through YouTube's encoder, so it is softer than fresh camera footage; encoder sizes for
noisy phone footage will be higher than the numbers here.

```
yt-dlp --no-playlist -f "bv*[dynamic_range=SDR][protocol=https]+ba" --download-sections "*60-100" \
  -o "cr/%(id)s-sdr.%(ext)s" "https://www.youtube.com/watch?v=LXb3EKWsInQ"
ffmpeg -ss 2 -t 30 -i yt/cr/LXb3EKWsInQ-sdr.webm -vf "fps=60000/1001,setpts=N*1001/60000/TB" -frames:v 1800 \
  -c:v libx265 -preset fast -crf 12 -pix_fmt yuv420p -tag:v hvc1 -x265-params "log-level=error:keyint=60" \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv \
  -c:a aac -b:a 256k -af "asetpts=PTS-STARTPTS,apad=whole_dur=30.03" -t 30.03 -movflags +faststart src/src4k60.mp4
```

---

## 1. Link import

### 1.1 YouTube via yt-dlp

Works without cookies or login on this machine. yt-dlp solved YouTube's JS challenge with
its bundled deno (`[youtube] [jsc:deno] Solving JS challenges using deno`); the nixpkgs
package brings deno along.

Tested videos: `jNQXAC9IVRw` (19 s, 240p) downloaded completely; `LXb3EKWsInQ` (5 min,
up to 2160p60 HDR) format selection simulated, two sections downloaded.

**Format selection** (`-s --print "%(format_id)s|%(ext)s|%(vcodec)s|..."`):

| Selection | `jNQXAC9IVRw` | `LXb3EKWsInQ` |
|-----------|---------------|---------------|
| default (`bv*+ba/b`) | 395+251: AV1 320x240 15 fps + Opus, **.webm** | 701+251: **AV1 10-bit HDR10** 3840x2160 60 fps (24.6 Mbit/s) + Opus, .webm |
| `-f "bv*[dynamic_range=SDR]+ba/b"` | (same as default) | 315+251: VP9 8-bit SDR 3840x2160 60 fps (24.0 Mbit/s) + Opus, .webm |
| `-S "hdr:SDR"` | (same) | 315+251 (same result as the filter) |
| `-S "res,fps,hdr:SDR,vcodec,br"` | - | 628+251-drc: m3u8 VP9 + **DRC audio**. Wrong: putting `br` in a custom sort promotes the dynamic-range-compressed audio track. Do not sort by bitrate yourself. |
| `-S "vcodec:h264,res,acodec:m4a"` | - | 299+140: H.264 **1920x1080** 60 fps + AAC, .mp4. H.264 stops at 1080p on YouTube, so "MP4-compatible" costs resolution. |

- Best quality = yt-dlp's default selection. It prefers resolution, then fps, then HDR,
  then codec (AV1 > VP9 > H.264). The result is AV1 or VP9 with Opus in WebM/Matroska.
- This FFmpeg decodes all of it: the AV1 file (`av1`, Main, yuv420p / yuv420p10le) and
  VP9 files were probed and transcoded here. `-hwaccels`/hardware decode of AV1/VP9 is
  S1's topic and was not measured.
- For an HDR upload the default picks the **HDR10** rendition (`color_transfer=smpte2084`,
  `bt2020nc`, mastering display + content light side data present, verified with
  ffprobe on a 5 s section). If stage 1 has no HDR pipeline, select SDR with
  `-S "hdr:SDR"`; resolution and frame rate stay the same and YouTube's own tone-mapped
  SDR rendition is used.
- WebM/Matroska stores timestamps in milliseconds: ffprobe reports
  `r_frame_rate=avg_frame_rate=19001/317` for the 59.94 fps downloads and packet deltas
  alternate 16/17 ms. Such files are CFR in content but need the tolerance described in
  section 2.3 and a snap to the nominal rate.

**Progress, machine-readable** (verified):

```
yt-dlp --no-playlist --newline \
  --progress-template "download:SGPROG %(progress)j" \
  --progress-template "postprocess:SGPP %(progress.status)s %(progress.postprocessor)s" \
  --print after_move:"SGFILE %(filepath)s" --no-simulate \
  --write-info-json -o "<tmpdir>/%(id)s.%(ext)s" <url>
```

- Each `SGPROG` line is one JSON object: `status` (`downloading`/`finished`),
  `downloaded_bytes`, `total_bytes` or `total_bytes_estimate`, `speed`, `eta`, `elapsed`,
  `filename`. Individual fields also work:
  `"download:SGPROG %(progress.downloaded_bytes)s %(progress.total_bytes)s %(info.format_id)s"`.
- Video and audio are downloaded one after the other; each restarts at 0 %. Use
  `%(info.format_id)s` (or the filename) to tell the two phases apart; the total size of
  both is in the info JSON (`filesize_approx`).
- `SGPP started Merger` / `SGPP finished Merger` / `MoveFiles` mark the mux step;
  `SGFILE <path>` is the final file. `--print` implies simulate, hence `--no-simulate`.
- `--download-sections "*60-100"` hands the download to ffmpeg: only one `finished`
  progress line was emitted (no intermediate progress), and the resulting WebM claimed
  `duration=50 s` for 40 s of video. Section download is not suitable for import.
- Wall time for the 19 s video including extraction and merge: 6.6 s.

**Metadata for auto-labelling** (verified, from `--write-info-json` or `-J`/`--print`):
`title`, `uploader`, `channel`, `upload_date` (`YYYYMMDD`), `duration`, `webpage_url`,
`extractor_key` (`Youtube`, `Generic`, ...), `id`, `format_id`, `vcodec`, `acodec`,
`width`, `height`, `fps`, `dynamic_range`, `thumbnail`, `description`, `categories`.
Label = `title`; source URL = `webpage_url`. The info JSON is 82 kB for the 19 s video
(it lists every format); store only the fields above in the asset index.

### 1.2 Giphy

Test item: `https://giphy.com/gifs/cat-funny-JIX9t2j0ZTN9S` (id `JIX9t2j0ZTN9S`).

| What | Result |
|------|--------|
| `yt-dlp --list-extractors \| grep -i giphy` | nothing: yt-dlp 2026.10.26 has **no Giphy extractor** |
| `yt-dlp <page URL>` | falls back to the generic extractor and returns `giphy.mp4`: H.264 **480x480, 325 kbit/s, 41 kB**. Works, but it is the worst rendition. Title `Cat Working GIF - Find & Share on GIPHY`. |
| `yt-dlp https://media.giphy.com/media/<id>/giphy.gif` | also returns `giphy.mp4`: Giphy answers a browser-like `Accept: text/html` request on the media URL with the HTML page (verified with `curl -H "Accept: text/html,..."` -> `content-type: text/html`) |
| `curl -L https://media.giphy.com/media/<id>/<rendition>` (default `Accept: */*`) | real bytes, see below |

Renditions fetched with plain curl:

| Rendition | HTTP | Format | Size | Bytes |
|-----------|------|--------|------|------:|
| `source.gif` | 200 | GIF 89a, **720x720**, 25 frames, 25 fps | original upload | 7 712 339 |
| `giphy-hd.mp4` | 200 | H.264 yuv420p 720x720, 2.47 Mbit/s | lossy, no alpha | 309 822 |
| `giphy.gif` | 200 | GIF 480x480, 15 frames | downsized | 1 799 229 |
| `giphy.webp` | 200 | animated WebP 480x480 **with alpha** (`argb`) | downsized | 242 026 |
| `giphy.mp4` | 200 | H.264 480x480, 325 kbit/s | downsized, lossy | 41 711 |
| `source.mp4` | **404** | - | not present for this item | - |

- Best source = **`source.gif`** (the original upload: full resolution, full frame count,
  lossless relative to what the uploader provided, transparency kept). The MP4 renditions
  are much smaller but are lossy re-encodes in yuv420p without alpha; `giphy.webp` keeps
  alpha but is the downsized 480 px version.
- FFmpeg 9 decodes the animated WebP (`codec_name=webp_anim`, 15 frames, `argb`) and the
  GIFs (`bgra`).
- All rendition names found in the page HTML: `source.gif`, `giphy.gif`, `giphy.mp4`,
  `giphy.webp`, `giphy-hd.mp4`, `giphy-downsized-small.mp4`, `giphy-preview.*`,
  `200.*`, `200w.*`, `100.*`, `480w_s.jpg`.
- Only one Giphy item was tested. Whether `source.mp4` exists for items uploaded as video
  was **not tested**.

### 1.3 Google Photos share links

yt-dlp has **no Google Photos extractor** (`--list-extractors` lists only `GoogleDrive`,
`GoogleDrive:Folder`, `video.google:search`). `yt-dlp https://photos.app.goo.gl/<id>`
follows the redirect and ends with `ERROR: Unsupported URL: https://photos.google.com/share/...`.

Plain HTTP, tested without login on public share links found on the web
(`photos.app.goo.gl/LsiQNDAp6K8DZAap8`, 24 photos; `photos.app.goo.gl/CSV7NDstShTUwUZq5`,
4 photos; a third link returned 404):

1. `curl -sL -A "<browser UA>" https://photos.app.goo.gl/<id>` -> 200, redirects to
   `https://photos.google.com/share/<album>?key=<key>`, about 1.1 MB of HTML. `og:title`
   holds the album title.
2. The HTML embeds one record per item:
   `["<mediaKey>",["https://lh3.googleusercontent.com/pw/<token>",<width>,<height>,...`.
   `spikes/s4-io/gphotos_list.py` extracts them with one regex (24 and 4 items found,
   matching the albums).
3. Appending a size suffix to the item URL:

| Suffix | Result for a 4898x3265 Canon photo |
|--------|-------------------------------------|
| (none) | 82 202 bytes, small preview |
| `=d` | **894 165 bytes, original file**: full EXIF (`manufacturer=Canon, model=...`), `content-disposition: attachment;filename="IMG_5653.jpg"` |
| `=s0`, `=w4898-h3265`, `=w16383-h16383` | 879 266 bytes: full resolution but re-encoded, EXIF stripped (`software=Google`) |
| `=dv` (video download) | 404 on a photo (expected) |

Conclusion:

- **Photos: feasible.** Original bytes with original file name, no login, via `=d`.
- **Videos: not tested end to end.** Neither test album contained a video and I had no
  share link with one. From Google's Library API documentation, `=dv` on a video base URL
  returns a transcoded MP4 ("high quality"), not necessarily the original upload, and
  there are public reports of it failing or returning reduced files for some videos.
  The share page marks video items with an extra field; the heuristic in
  `gphotos_list.py` (`"76647426"` key) is from memory of the page format and matched
  nothing in the two photo-only albums, so it is **unverified**.
- This relies on an undocumented page format that can change without notice. The
  fallback when parsing fails must be an explicit message ("download in the browser and
  drop the file"), not a silent failure.

### 1.4 Direct media URL

Tested with `https://test-videos.co.uk/vids/bigbuckbunny/mp4/h264/1080/Big_Buck_Bunny_1080_10s_30MB.mp4`
(30 704 510 bytes), `https://www.gstatic.com/webp/gallery/1.jpg` and a 14.7 MB JPEG from
`upload.wikimedia.org`.

| Method | Result |
|--------|--------|
| `curl -fL --retry 3 --connect-timeout 10 -A <ua> -o f.part -w "%{json}" -s <url>` | 200, `content_type` and `size_download` in the JSON, 3.2 s; rename `.part` on success |
| Resume: `curl -fL -C - -o f.part <url>` after truncating to 20 MB | `206`, downloaded the remaining 10 704 510 bytes, file identical (`cmp`) |
| 404 with `-f` | exit code 22, no output file created |
| `yt-dlp <mp4 url>` | works through the generic extractor, same progress template, file byte-identical to curl, `title` = file name stem |
| `yt-dlp <jpg url>` | also downloads it unchanged (generic extractor, direct file) |
| `yt-dlp` on `upload.wikimedia.org` | **HTTP 429** (the host rejects yt-dlp's client); curl with an own User-Agent got 200 |

Two of three first-choice sample URLs were dead (404/403): link import needs clear error
reporting from day one.

---

## 2. Probing

### 2.1 Asset-index probe (one call, about 0.15 to 0.4 s, 4.5 kB JSON)

```
ffprobe -v error -print_format json -show_format -show_streams <file>
```

Verified fields per video stream (on H.264 MP4 with rotation, HEVC MP4, AV1/VP9 WebM, GIF, WebP):

| Need | Field | Notes |
|------|-------|-------|
| codec | `codec_name`, `profile`, `codec_tag_string` | |
| pixel format | `pix_fmt` | alpha visible here (`bgra`, `argb`, `yuva444p12le`) |
| bit depth | `bits_per_raw_sample` | `"8"` for H.264; **`null` for the AV1 10-bit file**. Derive depth from `pix_fmt` when missing. |
| size | `width`, `height`, `sample_aspect_ratio` | |
| colour tags | `color_range`, `color_space`, `color_transfer`, `color_primaries`, `chroma_location` | absent keys = untagged |
| rotation | `side_data_list[]` entry `side_data_type="Display Matrix"` with `rotation` (integer degrees) | verified: `ffmpeg -display_rotation 90 -i in -c copy rot90.mp4` -> `"rotation":90` |
| HDR metadata | `side_data_list[]`: `Mastering display metadata`, `Content light level metadata` | present at stream level for the AV1 HDR10 WebM. Do **not** infer HDR from these: the SDR BT.709 rendition of the same video also carried `Content light level` (max 1100) and it survived into every transcode. Use `color_transfer` (`smpte2084`, `arib-std-b67`). |
| frame rates | `r_frame_rate`, `avg_frame_rate`, `time_base` | see 2.3 |
| duration | stream `duration` (absent in WebM/MKV streams), `format.duration` | |
| frame count | `nb_frames` | MP4/MOV/GIF only; `N/A` for WebM/MKV -> use the packet count from the scan in 2.3 |
| scan type | `field_order` | |
| audio | `codec_name`, `sample_rate`, `channels`, `channel_layout`, `bit_rate` | |
| container | `format.format_name`, `size`, `bit_rate`, `tags` | |

### 2.2 First-frame probe (only when frame-level data is needed)

```
ffprobe -v error -select_streams v:0 -read_intervals "%+#1" -show_frames \
  -show_entries frame=pict_type,interlaced_frame,top_field_first,color_transfer,side_data_list -print_format json <file>
```

Decodes one frame: 446 ms (AV1 HDR WebM) and 536 ms (4K HEVC) wall, of which about 133 ms
is ffprobe process start (see 3.2). Gives SEI-only HDR metadata and the interlace flags.

### 2.3 Variable-frame-rate detection

Test files (`spikes/s4-io/vfr_make.sh`, all 1280x720 H.264 derived from the source):
`cfr.mp4`, `cfr.mkv` (remux of cfr.mp4), `vfr_drop.mp4` (every 5th frame + 20 % random
dropped), `vfr_jitter.mp4` (30 fps with about 1 % random drops; the intended timestamp
jitter was rounded away by the 1001/30000 time base, so this file is "30 fps with 6
gaps"), `vfr_late.mp4` (CFR for 24 s, drops only afterwards), `vfr_onedrop.mp4` (exactly
one missing frame at 15 s). Big files (`vfr_time.sh`): `big.mp4` / `big.mkv` = the 4K
source looped 30x with `-stream_loop 29 -c copy` (3.48 GB, 901 s, 53 970 frames). The loop
joins are not frame-aligned, so these files contain **29 irregular gaps (38 ms instead of
16.7 ms) in 15 minutes**: an accidental but realistic "almost CFR" case.

Methods:

- **H** header compare: `r_frame_rate` vs `avg_frame_rate` from the probe in 2.1.
- **M** `mediainfo --Inform="Video;%FrameRate_Mode%"`.
- **S** full packet scan:
  `ffprobe -v error -select_streams v:0 -show_entries stream=time_base:packet=pts -of json <file> | python3 -I spikes/s4-io/vfr_detect.py`
  Rule: sort PTS, take deltas, a delta is off if it differs from the median delta by more
  than `max(1 time-base tick, 0.5 % of the median)`; VFR if any delta is off.
- **W** windowed scan: same as S with `-read_intervals "%+20"` (first 20 s only).

| File | truth | H: `r` / `avg` | H verdict | M | S | W (first 20 s) |
|------|-------|----------------|-----------|---|---|----------------|
| cfr.mp4 | CFR | 60000/1001 / 60000/1001 | CFR | CFR | CFR (0 off) | - |
| cfr.mkv | CFR | 19001/317 / 19001/317 | CFR | CFR | CFR (deltas 16/17 ms, 0 off) | - |
| vfr_drop.mp4 | VFR | 60000/1001 / 34080000/901901 | VFR | VFR | VFR (460 off) | - |
| vfr_jitter.mp4 | VFR | 30000/1001 / 29800/1001 | VFR | VFR | VFR (6 off) | - |
| vfr_late.mp4 | VFR | 60000/1001 / 101700000/1800799 | VFR | VFR | VFR (77 off) | - |
| vfr_onedrop.mp4 | VFR | 60000/1001 / 107880000/1800799 | VFR | VFR | VFR (1 off) | - |
| big.mp4 (29 gaps) | VFR | 60000/1001 / 323820000/5406109 | VFR | VFR | VFR (29 off) | **CFR (wrong)** |
| big.mkv (29 gaps) | VFR | 21923/366 / 21923/366 | **CFR (wrong)** | **CFR (wrong)** | VFR (29 off) | **CFR (wrong)** |
| YouTube VP9 .webm | CFR | 19001/317 / 19001/317 | CFR | CFR | CFR | - |
| giphy.gif (delays 60/70 ms) | CFR-ish | 15/1 / 50/3 | **VFR** | (empty) | CFR within 1 tick | - |

Timings on the 3.48 GB files (cold = page cache for the file dropped with
`dd if=<f> iflag=nocache count=0`; load1 = 16.8):

| Method | big.mp4 cold / warm | big.mkv cold / warm |
|--------|--------------------:|--------------------:|
| probe 2.1 (`-show_format -show_streams`) | 0.20 s / 0.15 s | 0.40 s / 0.36 s |
| M mediainfo | 0.14 s / 0.08 s | 0.22 s / 0.25 s |
| S full packet scan (JSON + python) | **6.72 s / 2.68 s** | **7.61 s / 2.69 s** |
| `ffprobe -count_packets` (frame count only) | 7.19 s / 2.16 s | 7.33 s / 2.08 s |
| W first 20 s | 0.64 s / 0.46 s | 0.58 s / 0.44 s |
| S with `-of csv` + `sort` + `awk` instead of python | - / 7.57 s | - / 7.36 s |

Findings:

- Only the **full packet scan** was right in every case. It reads the whole file
  (ffprobe has no index-only mode), so cold it runs at disk speed (3.48 GB in about 7 s on
  the SATA SSD) and warm at about 1.3 GB/s. During import the file has just been copied
  and hashed, so it is in the page cache: about 2.7 s per 3.5 GB / 54 000 packets.
- The header compare is free and was correct for every **MP4/MOV** file, including a
  single dropped frame in 30 s and 29 gaps in 15 min, because the MP4 demuxer computes
  `avg_frame_rate` from the sample table. It is blind for Matroska/WebM (both rates are
  estimates from the first packets) and it reports GIFs with alternating delays as VFR.
- mediainfo agrees with the header compare (wrong on the MKV) and adds nothing.
- Scanning only the start is wrong whenever irregularities come later.
- Not tested: reading the MP4 sample table through libavformat's index API
  (`avformat_index_get_entry`) to avoid reading packet data. If S1's bindings expose it,
  it would make the scan header-speed for MP4/MOV.

---

## 3. Proxy codec for scrubbing

Script: `spikes/s4-io/bench_proxy.sh` (pipeline), `bench_proxy_enc.sh` (encoder only),
`bench_seek.sh`, `bench_seek_c.sh`. All proxies keep the source frame rate (59.94) and
have no audio.

### 3.1 Encode pipeline, size, decode speed

Pipeline for every row: VAAPI decode + GPU scale, then either GPU encode or download +
software encode:

```
ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128 -hwaccel_output_format vaapi -i src4k60.mp4 -an \
  -vf "scale_vaapi=w=960:h=540,hwdownload,format=nv12" <encoder args> out      # software encoders
  -vf "scale_vaapi=w=960:h=540" -c:v h264_vaapi ...                           # VAAPI encoder
```

| Variant | Encoder args |
|---------|--------------|
| mjpeg-q3 / q5 | `-c:v mjpeg -q:v 3` (or 5) `-pix_fmt yuvj420p -color_range pc` (.mov) |
| x264-intra | `-c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 1 -pix_fmt yuv420p` |
| vaapi-intra | `-c:v h264_vaapi -g 1 -bf 0 -rc_mode CQP -qp 22` |
| x264-gop8 | `-c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 8 -keyint_min 8 -sc_threshold 0 -bf 0 -pix_fmt yuv420p` |
| vaapi-gop8 | `-c:v h264_vaapi -g 8 -bf 0 -rc_mode CQP -qp 22` |
| prores-proxy | `-c:v prores_ks -profile:v 0 -pix_fmt yuv422p10le -vendor apl0` (.mov) |
| dnxhr-lb | `-c:v dnxhd -profile:v dnxhr_lb -pix_fmt yuv422p` (.mov) |

Second run of `bench_proxy.sh` (load1 4 to 9). `enc x rt` = 30.01 s / wall time of the whole
pipeline; decode fps = `ffmpeg [-threads 1] -i proxy -f null -` (1799 frames / wall).

| Codec | Res | Pipeline x realtime | MB per minute | Mbit/s | SW decode fps, all threads | SW decode fps, 1 thread |
|-------|-----|--------------------:|--------------:|-------:|---------------------------:|------------------------:|
| (decode+scale only, to null) | 540p | 4.1 | - | - | - | - |
| mjpeg-q3 | 540p | 3.9 | 245.7 | 32.8 | 812 | 820 |
| mjpeg-q5 | 540p | 4.0 | 176.1 | 23.5 | 975 | 954 |
| x264-intra | 540p | 3.7 | 94.7 | 12.6 | 3557 | 767 |
| vaapi-intra | 540p | 4.2 | 206.6 | 27.6 | 1617 | 206 |
| x264-gop8 | 540p | 2.7 | 40.4 | 5.4 | 3750 | 1448 |
| vaapi-gop8 | 540p | 4.2 | 59.2 | 7.9 | 2036 | 511 |
| prores-proxy (prores_ks) | 540p | 1.5 | 211.7 | 28.2 | 3136 | 702 |
| dnxhr-lb | 540p | 3.9 | 176.8 | 23.6 | 3797 | 883 |
| (decode+scale only, to null) | 720p | 4.1 | - | - | - | - |
| mjpeg-q3 | 720p | 4.2 | 367.3 | 49.0 | 531 | 527 |
| mjpeg-q5 | 720p | 4.2 | 264.3 | 35.2 | 624 | 623 |
| x264-intra | 720p | 4.4 | 149.1 | 19.9 | 2848 | 504 |
| vaapi-intra | 720p | 5.1 | 299.1 | 39.9 | 1227 | 139 |
| x264-gop8 | 720p | 4.3 | 58.4 | 7.8 | 2639 | 954 |
| vaapi-gop8 | 720p | 5.3 | 81.7 | 10.9 | 1508 | 350 |
| prores-proxy (prores_ks) | 720p | 0.9 | 336.1 | 44.8 | 1863 | 422 |
| dnxhr-lb | 720p | 3.7 | 294.6 | 39.3 | 2532 | 545 |

The first run (load1 7 to 23) gave the same sizes and pipeline speeds of 3.9 to 5.3x
(prores_ks 0.9 to 1.1x). **The pipeline is bounded by decoding the 4K60 HEVC source
(about 4x realtime = 245 fps through VAAPI + scale + download), not by the proxy
encoder**, except for `prores_ks`.

Encoder-only speed (`bench_proxy_enc.sh`: input is pre-scaled raw NV12 from disk, load1
3.5 to 15):

| Encoder | 540p fps (x realtime at 59.94) | 720p fps (x realtime) |
|---------|-------------------------------:|----------------------:|
| mjpeg -q:v 3 | 1275 (21.3x) | 910 (15.2x) |
| libx264 intra veryfast | 1009 (16.8x) | 743 (12.4x) |
| libx264 intra ultrafast | 1262 (21.1x) | 861 (14.4x) |
| h264_vaapi intra (hwupload) | 975 (16.3x) | 764 (12.7x) |
| libx264 gop8 veryfast | 795 (13.3x) | 506 (8.4x) |
| h264_vaapi gop8 (hwupload) | 1049 (17.5x) | 757 (12.6x) |
| prores_ks proxy | 91 (1.5x) | 54 (0.9x) |
| prores_aw proxy | 691 (11.5x) | 447 (7.5x) |
| dnxhd dnxhr_lb | 1150 (19.2x) | 656 (10.9x) |

`-threads 16 -thread_type frame` made no difference for mjpeg (1199 vs 1275 fps).

### 3.2 Random seek

`bench_seek_c.sh`: one ffprobe process, one open, **200 seeks** to pseudo-random
positions (fixed seed) each followed by decoding one frame:
`ffprobe -v error [-threads 1] -select_streams v:0 -read_intervals "T1%+#1,T2%+#1,..." -show_entries frame=pts_time -of csv=p=0 <file>`;
ms/seek = (wall - wall of a plain open) / 200, median of 5 runs, page cache warm, load1 17.

| File | ms per seek+decode, default threads | ms per seek+decode, 1 thread |
|------|------------------------------------:|-----------------------------:|
| mjpeg-q3 540p | 1.14 | 1.17 |
| mjpeg-q5 540p | 1.03 | 1.00 |
| x264-intra 540p | 1.36 | 1.33 |
| vaapi-intra 540p | 5.04 | 5.01 |
| x264-gop8 540p (keyframe only, see note) | 1.59 | 2.54 |
| vaapi-gop8 540p (keyframe only) | 7.18 | 5.07 |
| prores-proxy 540p | 1.41 | 1.42 |
| dnxhr-lb 540p | 1.08 | 1.10 |
| mjpeg-q3 720p | 1.92 | 1.95 |
| mjpeg-q5 720p | 1.71 | 1.61 |
| x264-intra 720p | 2.06 | 2.08 |
| vaapi-intra 720p | 7.83 | 7.50 |
| x264-gop8 720p (keyframe only) | 3.25 | 3.29 |
| prores-proxy 720p | 2.81 | 2.25 |
| dnxhr-lb 720p | 1.78 | 1.77 |
| **src4k60.mp4** (HEVC, GOP 60, software decode, keyframe only) | 99.33 | 99.72 |

Notes:

- For the GOP-8 files and the source this method lands on the keyframe at or before the
  target, so it does **not** include decoding forward to the exact frame. Frame-accurate
  cost for GOP 8 is up to 7 more frames; from the measured 1-thread decode speed
  (1448 fps at 540p) that is at most about 5 ms extra. This is derived, not measured.
- The per-process variant asked for in the brief (`bench_seek.sh` method A:
  50 x `ffmpeg -ss T -i f -frames:v 1 -f null -`) is dominated by process start:
  `ffmpeg -version` alone takes **133 ms** (median of 30; ffmpeg-full links several hundred
  libraries). Medians were 135 to 188 ms for all proxies (p95 144 to 344 ms) and 927 ms
  for the 4K source. After subtracting start-up the proxies differ by noise (2 to 55 ms),
  so method A cannot rank them. It does show that **shelling out to ffmpeg per frame
  costs at least 133 ms** and is not an option for anything interactive.
- Method B (one ffmpeg process with 50 `-ss T -i f` inputs) measured decoder
  initialisation (16 frame threads per input: 12 to 27 ms per input for H.264, ProRes and
  DNxHR, 3.4 to 3.8 ms for MJPEG) rather than seeking, and on the 4K source 50 parallel HEVC decoders were
  **killed** (`Killed`, most likely out of memory; kernel log not checked). Not used.

### 3.3 Reading

- Every all-intra proxy seeks and decodes a frame in 1 to 2 ms on one core at 540p, i.e.
  far inside a 144 Hz frame (6.9 ms), except `h264_vaapi` intra (5 to 8 ms: CABAC at 28
  to 40 Mbit/s).
- x264 all-intra is the smallest intra format by a factor of 1.9 to 2.6 (94.7 MB/min vs
  177 to 246 MB/min at 540p), decodes 767 fps on one thread and 3557 fps with frame
  threads, and it is H.264 High 4:2:0 8-bit, which the GPU can also decode if S1 picks a
  hardware path.
- MJPEG has the simplest decoder and the best single-thread speed (820 to 954 fps), but
  no gain from threads, the largest files, and full-range JPEG colour that must be
  handled explicitly.
- GOP-8 H.264 is 2.3x smaller again (40 MB/min) and still cheap, but needs a decoder
  that tracks GOP state for backward scrubbing.
- 720p costs about 1.5x the size and 1.5x the decode time of 540p for every codec.

---

## 4. Conform (mezzanine) for VFR / GIF / odd sources

Script: `spikes/s4-io/bench_conform.sh` (`1` = opaque video, `2` = alpha).

### 4.1 Opaque video (VFR phone footage etc.)

Each candidate converts to constant frame rate with the `fps` filter
(`-vf fps=<rate>`, which duplicates/drops frames to the target grid) and encodes the 4K60
source. VMAF (4K model, every 5th frame) and PSNR-Y against the source; decode fps is
software, all threads.

| Candidate | Args (after `-vf fps=60000/1001`) | Encode fps (x realtime) | GB per minute (4K60) | Mbit/s | VMAF | PSNR-Y | SW decode fps | load1 |
|-----------|-----------------------------------|------------------------:|---------------------:|-------:|-----:|-------:|--------------:|------:|
| x264 intra | `-c:v libx264 -preset veryfast -crf 14 -g 1 -pix_fmt yuv420p` | 52 (0.86x) | 1.25 | 166 | 98.05 | 49.73 | 187 | 29 |
| x264 GOP 15 | `-c:v libx264 -preset veryfast -crf 14 -g 15 -bf 0 -pix_fmt yuv420p` | 37 (0.62x) | **0.40** | 54 | 99.28 | 50.93 | 195 | 28 |
| h264_vaapi intra | VAAPI decode, `-c:v h264_vaapi -g 1 -bf 0 -rc_mode CQP -qp 14` | 164 (2.73x) | 2.09 | 279 | 99.85 | 52.55 | 121 | 23 |
| hevc_vaapi intra | VAAPI decode, `-c:v hevc_vaapi -g 1 -bf 0 -rc_mode CQP -qp 14` | 166 (2.78x) | 1.94 | 259 | 99.97 | 54.09 | **52** | 26 |
| hevc_vaapi GOP 15 | VAAPI decode, `-c:v hevc_vaapi -g 15 -bf 0 -rc_mode CQP -qp 14` | 159 (2.65x) | 0.56 | 75 | 99.89 | 53.50 | **48** | 30 |
| DNxHR HQ | `-c:v dnxhd -profile:v dnxhr_hq -pix_fmt yuv422p` | 34 (0.56x) | **13.10** | 1746 | 100.00 | 55.77 | 233 | 25 |
| ProRes 422 | `-c:v prores_aw -profile:v 2 -pix_fmt yuv422p10le` | 51 (0.86x) | 6.35 | 847 | 99.89 | 57.20 | 188 | 14 |

CFR check: the four VFR test files from 2.3 were conformed with
`-vf "fps=<rate>,format=nv12,hwupload" -c:v hevc_vaapi -rc_mode CQP -qp 14 -g 15 -bf 0`
(software decode + upload) and re-scanned: all four come out with 0 off deltas,
`r_frame_rate = avg_frame_rate` = 60000/1001 (30000/1001 for the 30 fps file) and
1798/1799/1799/900 frames for 30.0 s. A GIF with alternating 60/70 ms delays conformed
with `-vf fps=50 -c:v utvideo -pix_fmt gbrap` gives 50 frames at exactly 20 ms.

Reading:

- The intermediate codecs (DNxHR HQ, ProRes 422) cost 6.4 to 13.1 GB per minute of 4K60:
  ten minutes of conformed phone footage would take 64 to 131 GB on a 458 GB disk.
- The VAAPI encoders are the fastest by 3 to 5x (and their speed here is bounded by the
  4K60 decode, see section 3) and score VMAF 99.85 to 99.97 at qp 14.
- HEVC at these bitrates decodes at only 48 to 52 fps in software (16 threads, under
  load), below the 59.94 fps needed. **An HEVC conform is only usable if the timeline
  decodes it on the GPU (spike S1).** H.264 files decode at 121 to 195 fps in software.
- x264 intra at crf 14 `veryfast` scored lower (VMAF 98.05) than x264 GOP 15 at the same
  crf (99.28) while being 3x larger; intra-only x264 is the wrong tool here.
- 10-bit conform was not run in this table; `hevc_vaapi -profile:v main10` with
  `scale_vaapi=format=p010` works (section 5 rows). H.264 has no hardware 10-bit path.

### 4.2 Alpha (GIF / WebP with transparency)

Synthetic reference: 720x720, 29.97 fps, 10 s (300 frames) of the footage with a moving
soft-edged circular alpha mask (alpha 0 to 255), stored as FFV1 `bgra`. Each candidate
is encoded from it; alpha and RGB PSNR are measured against the reference
(`inf` = bit-exact). load1 36 to 45 (speeds are lower bounds).

| Codec (container) | Args | Decoded pix_fmt | Encode fps | MB per minute | Decode fps | Alpha PSNR | RGB PSNR |
|-------------------|------|-----------------|-----------:|--------------:|-----------:|-----------:|---------:|
| ProRes 4444 `prores_ks` (.mov) | `-profile:v 4 -pix_fmt yuva444p10le -alpha_bits 16` | yuva444p12le | 27 | 1218 | 130 | 62.3 dB | 61.3 dB |
| ProRes 4444 `prores_aw` (.mov) | `-profile:v 4 -pix_fmt yuva444p10le` | yuva444p12le | 48 | 974 | 175 | 62.3 dB | 54.1 dB |
| Ut Video (.mkv) | `-c:v utvideo -pix_fmt gbrap` | gbrap | 58 | 1043 | 229 | inf | inf |
| FFV1 (.mkv) | `-c:v ffv1 -level 3 -slices 16 -threads 16 -pix_fmt bgra` | bgra | 61 | 625 | 97 | inf | inf |
| QuickTime Animation (.mov) | `-c:v qtrle -pix_fmt argb` | argb | 44 | 2903 | 202 | inf | inf |
| PNG in MOV | `-c:v png -pix_fmt rgba` | rgba | 34 | 1132 | 336 | inf | inf |
| CineForm (.mov) | `-c:v cfhd -quality film1 -pix_fmt gbrap12le` | gbrap12le | 142 | 588 | 122 | 48.3 dB | 49.5 dB |
| VP9 + alpha (.webm) | `-c:v libvpx-vp9 -pix_fmt yuva420p -crf 15 -b:v 0` | **yuv420p** | 12 | 24 | 370 | **0.2 dB (alpha lost on decode)** | 40.1 dB |
| hevc_vaapi (control, no alpha) | | yuv420p | 101 | 55 | 142 | 0.2 dB | 41.3 dB |

Real transparent sources converted to 25 fps CFR (`-vf fps=25`), sizes in bytes:

| Source | Source bytes | FFV1 bgra | PNG/MOV | ProRes 4444 | Ut Video | qtrle |
|--------|-------------:|----------:|--------:|------------:|---------:|------:|
| `giphy.webp` 480x480, alpha, 1 s | 242 026 | 1 865 544 | 3 656 170 | 4 381 789 | 6 085 801 | 8 351 786 |
| `source.gif` 720x720, 1 s | 7 712 339 | 18 639 436 | 16 593 814 | 28 874 117 | 20 261 538 | 32 609 766 |

- VP9 with alpha is written correctly by libvpx but FFmpeg's native `vp9` decoder returns
  `yuv420p` (alpha dropped). Not usable as an intermediate.
- Lossless RGBA is affordable because transparent sources are GIFs/WebPs/stickers: small
  frames and seconds, not minutes (1 s of 720x720 GIF = 17 to 33 MB).
- FFV1 is the smallest lossless option (625 MB/min on footage, 1.9 MB for the WebP) but
  the slowest to decode (97 fps at 720x720 with slices). Ut Video decodes 2.4x faster
  and is 1.1 to 3.3x larger. PNG decodes fastest here (336 fps) at Ut Video-like size.

---

## 5. Master export encoders

Script: `spikes/s4-io/bench_master.sh` and `bench_master_hevc.sh`. Every row encodes the
full 4K60 source (1799 frames, 30.01 s) to MP4 with AAC 256k, colour tags and faststart:

```
ffmpeg -i src4k60.mp4 <video args> \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv \
  -c:a aac -b:a 256k -movflags +faststart out.mp4
# VAAPI rows: decode on the GPU as well
ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128 -hwaccel_output_format vaapi -i src4k60.mp4 <video args> ...
```

Quality: libvmaf, model `vmaf_4k_v0.6.1`, plus PSNR-Y and SSIM, against the HEVC source,
both converted to yuv420p10le, every 3rd frame (`n_subsample=3`) except where noted:

```
ffmpeg -i out.mp4 -i src4k60.mp4 -an -lavfi "[0:v]setpts=PTS-STARTPTS,format=yuv420p10le[d];[1:v]setpts=PTS-STARTPTS,format=yuv420p10le[r];\
[d][r]libvmaf=model=version=vmaf_4k_v0.6.1:n_threads=16:n_subsample=3:feature=name=psnr|name=float_ssim:log_fmt=json:log_path=out.json" -f null -
```

### 5.1 Size and quality (speeds in this table were measured under heavy load, see 5.2)

| Config | Video args | Wall s | x realtime | MB | Mbit/s | VMAF mean | VMAF min | PSNR-Y | SSIM | load1 |
|--------|-----------|-------:|-----------:|---:|-------:|----------:|---------:|-------:|-----:|------:|
| x264 slow crf 16 (VMAF on all frames) | `-c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p` | 136.4 | 0.22 | 108.6 | 29.0 | 98.98 | 96.32 | 50.45 | 0.9997 | 2.6 |
| x264 slow crf 16 (repeat) | same | 158.2 | 0.19 | 108.6 | 29.0 | 98.97 | 96.32 | 50.44 | 0.9997 | 18 |
| x264 medium crf 18 | `-preset medium -crf 18` | 170.2 | 0.18 | 88.1 | 23.5 | 98.35 | 95.35 | 49.59 | 0.9996 | 38 |
| x264 veryfast crf 18 | `-preset veryfast -crf 18` | 69.8 | 0.43 | 66.2 | 17.7 | 96.38 | 93.47 | 47.96 | 0.9992 | 31 |
| x265 10-bit fast crf 18 | `-c:v libx265 -preset fast -crf 18 -pix_fmt yuv420p10le -tag:v hvc1` | 345.5 | 0.09 | 39.3 | 10.5 | 98.73 | 95.03 | 51.02 | 0.9995 | 22 |
| x265 10-bit medium crf 18 | `-preset medium`, rest same | 532.9 | 0.06 | 38.7 | 10.3 | 98.63 | 95.83 | 50.86 | 0.9994 | 26 |
| SVT-AV1 preset 8 crf 24 | `-c:v libsvtav1 -preset 8 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0` | 153.8 | 0.20 | 44.9 | 12.0 | 98.49 | 93.92 | 50.64 | 0.9994 | 37 |
| SVT-AV1 preset 6 crf 24 | `-preset 6`, rest same | 235.5 | 0.13 | 36.0 | 9.6 | 98.99 | 95.97 | 50.94 | 0.9994 | 23 |
| SVT-AV1 preset 4 crf 24 | `-preset 4`, rest same | 333.6 | 0.09 | 35.3 | 9.4 | 99.00 | 96.03 | 50.99 | 0.9994 | 23 |
| hevc_vaapi 8-bit qp 18 | `-c:v hevc_vaapi -rc_mode CQP -qp 18` | 15.3 | 1.96 | 104.8 | 27.9 | 99.20 | 96.90 | 50.74 | 0.9996 | 39 |
| hevc_vaapi 8-bit qp 22 | `-qp 22` | 15.3 | 1.97 | 68.3 | 18.2 | 98.63 | 96.25 | 49.91 | 0.9994 | 22 |
| **hevc_vaapi 10-bit qp 20** | `-vf scale_vaapi=format=p010 -c:v hevc_vaapi -profile:v main10 -rc_mode CQP -qp 20` | 16.3 | 1.84 | 81.0 | 21.6 | **99.55** | **97.75** | 50.07 | 0.9995 | 28 |
| hevc_vaapi 10-bit qp 24 | `-qp 24`, rest same | 15.2 | 1.98 | 46.8 | 12.5 | 98.55 | 95.61 | 48.67 | 0.9992 | 22 |
| hevc_vaapi VBR 60M | `-c:v hevc_vaapi -rc_mode VBR -b:v 60M -maxrate 80M` | 15.5 | 1.93 | 214.9 | 57.3 | 99.85 | 97.33 | 52.74 | 0.9998 | 29 |
| h264_vaapi qp 18 | `-c:v h264_vaapi -rc_mode CQP -qp 18` | 21.9 | 1.37 | 95.6 | 25.5 | 99.18 | 96.37 | 50.31 | 0.9995 | 21 |
| h264_vaapi qp 22 | `-qp 22` | 21.5 | 1.40 | 54.9 | 14.6 | 97.84 | 93.57 | 48.68 | 0.9991 | 38 |
| av1_vaapi 8-bit q 60 | `-c:v av1_vaapi -rc_mode CQP -global_quality 60` | 13.5 | 2.22 | 46.7 | 12.5 | 98.29 | 95.89 | 49.37 | 0.9992 | 18 |
| av1_vaapi 8-bit q 100 | `-global_quality 100` | 10.0 | 2.99 | 25.3 | 6.8 | 95.94 | 92.71 | 47.55 | 0.9986 | 15 |
| av1_vaapi VBR 40M | `-rc_mode VBR -b:v 40M -maxrate 60M` | 9.6 | 3.12 | 140.1 | 37.3 | 99.28 | 90.43 | 51.73 | 0.9996 | 39 |
| **av1_vaapi 10-bit q 60** | `-vf scale_vaapi=format=p010 -c:v av1_vaapi -rc_mode CQP -global_quality 60` | 8.2 | 3.64 | 49.1 | 13.1 | 99.05 | 96.47 | 49.30 | 0.9993 | 38 |
| av1_vaapi 10-bit q 80 | `-global_quality 80`, rest same | 9.0 | 3.33 | 34.1 | 9.1 | 98.18 | 95.07 | 48.45 | 0.9991 | 29 |

Not measured: `libx265 -preset slow` (aborted: `medium` already took 533 s for 30 s of
video under load).

### 5.2 Encode speed, repeated on a quieter machine

`spikes/s4-io/bench_master_speed.sh`: same commands, encode only, run after the other
agents had mostly finished (load1 at the start of the series 9.9; the later values of
about 20 are this benchmark's own 16 encoder threads). Sizes are identical to 5.1.

| Config | Wall s | x realtime | fps |
|--------|-------:|-----------:|----:|
| hevc_vaapi 10-bit qp 20 | 9.5 | **3.16** | 189.6 |
| av1_vaapi 10-bit q 60 | 7.9 | **3.80** | 227.8 |
| h264_vaapi qp 18 | 16.0 | 1.88 | 112.6 |
| SVT-AV1 preset 8 crf 24 | 93.8 | 0.32 | 19.2 |
| SVT-AV1 preset 6 crf 24 | 126.6 | 0.24 | 14.2 |
| x264 slow crf 16 | 113.8 | 0.26 | 15.8 |
| x264 medium crf 18 | 81.6 | 0.37 | 22.1 |
| x265 10-bit fast crf 18 | 151.1 | 0.20 | 11.9 |
| x265 10-bit medium crf 18 | 260.2 | 0.12 | 6.9 |

The VAAPI rows decode the 4K60 HEVC source on the same GPU; section 3 measured that
decode + scale + download alone runs at about 4x realtime, so the hardware encoders are
close to the decode limit of this test. In the app the frames come from the renderer.

Reading (one clip, source already compressed once by YouTube, so VMAF is close to
saturation for every row above 98.5; differences below about 0.5 VMAF should not be
over-interpreted):

- No software encoder beat the hardware encoders on quality here. x264 `slow` crf 16
  reaches VMAF 98.98 at 29.0 Mbit/s and 0.26x realtime; `hevc_vaapi` 10-bit qp 20 reaches
  99.55 (and the best minimum, 97.75) at 21.6 Mbit/s and 3.16x realtime; `h264_vaapi`
  qp 18 reaches 99.18 at 25.5 Mbit/s.
- The software encoders win on size: SVT-AV1 preset 6 crf 24 gives VMAF 98.99 at
  9.6 Mbit/s, x265 `fast`/`medium` crf 18 give 98.6 to 98.7 at 10.3 to 10.5 Mbit/s.
  The closest hardware row is `av1_vaapi` 10-bit q 60: 99.05 at 13.1 Mbit/s, 16x faster
  than SVT-AV1 preset 6.
- SVT-AV1 preset 4 is not worth it over preset 6 (35.3 vs 36.0 MB, VMAF 99.00 vs 98.99).
  x265 `medium` is not better than `fast` here (38.7 vs 39.3 MB, VMAF 98.63 vs 98.73).
- `av1_vaapi` VBR 40M had the lowest minimum frame score (90.4) despite the highest
  bitrate of the AV1 rows; constant-QP modes were steadier.

### 5.3 `hevc_vaapi` + `-tag:v hvc1` is broken on this machine

The first run used `-tag:v hvc1` on all HEVC rows. For libx265 that is fine. For
`hevc_vaapi` the MP4 was undecodable: VMAF 9.4 to 10.4, PSNR-Y 6.4 to 7.9 dB, frames are
green garbage (looked at `dbg/hv.png`), software decoder prints `CABAC_MAX_BIN` errors.
Isolated on 2 s encodes (PSNR against the source):

| Variant | PSNR |
|---------|-----:|
| `hevc_vaapi`, default tag (`hev1`), .mp4 | 51.82 dB (correct) |
| `hevc_vaapi -tag:v hvc1` | **7.54 dB** |
| same + `-movflags +faststart` / + audio / `-sei 0` / software decode + `hwupload` / `-flags:v -global_header` | 7.54 to 7.56 dB (all broken) |
| `hevc_vaapi` to raw `.hevc`, then `ffmpeg -r 60000/1001 -i x.hevc -c copy -tag:v hvc1 out.mp4` | 51.82 dB |
| `hevc_vaapi` to `hev1` MP4, then `ffmpeg -i in.mp4 -c copy -tag:v hvc1 -movflags +faststart out.mp4` | 51.82 dB |

Full-length check of the remux route on the 10-bit qp 20 file: 0.44 s for the remux, tag
`hvc1`, 0 decode errors, PSNR identical to the `hev1` file. With `hvc1` the muxer relies
on the parameter sets in the container header only; the ones FFmpeg generates for the
header evidently do not match what the AMD encoder puts in the stream (root cause not
investigated further). `h264_vaapi` and `av1_vaapi` MP4 output decoded correctly
(VMAF 95.9 to 99.3 above).

Other encoder findings: `hevc_vaapi -rc_mode ICQ` fails ("received no packets");
`QVBR` and `VBR` work; `h264_vaapi -bf 2` produces a byte-identical file to the default
(no B-frames); `av1_vaapi` accepts 10-bit input (`p010`) and produced a valid Main-profile
10-bit stream, although the brief listed AV1 encode as 8-bit only.

### 5.4 Colour tags and faststart (verified)

With `-color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv
-movflags +faststart` as output options, checked on the libx264, libx265, libsvtav1,
h264_vaapi, hevc_vaapi (`hev1` and remuxed `hvc1`) and av1_vaapi (8- and 10-bit) outputs:

- ffprobe: `color_range=tv, color_space=bt709, color_transfer=bt709, color_primaries=bt709`.
- mediainfo: `BT.709/BT.709/BT.709/Limited`, source `Container / Stream`, i.e. the tags are
  in the MP4 `colr` atom **and** in the bitstream (H.264 VUI / AV1 sequence header).
  `ffprobe -v trace` shows one `colr` atom.
- Atom order `ftyp, moov, free, mdat` and mediainfo `IsStreamable=Yes`: faststart applied.
- `ffprobe -v trace` atom order was looked at for the libx264, h264_vaapi and av1_vaapi files;
  the others were checked with mediainfo `IsStreamable` only.

In the real export the frames come from the app's renderer, so the tags must describe
what the renderer outputs (BT.709 limited for SDR), not what the source had.

---

## 6. Size-targeted share copy

Scripts: `spikes/s4-io/bench_target.sh` (simple formula), `bench_target2.sh` (refined).
Clip: the full 30.01 s source, downscaled.

### 6.1 Simple formula

`video_bps = target_bytes * 8 * (1 - 0.02) / duration - audio_bps`, then

```
ffmpeg -y -i src -vf "fps=30000/1001,scale=1280:720:flags=lanczos" -c:v libx264 -preset slow -b:v $VB -pass 1 -passlogfile L -pix_fmt yuv420p -an -f null -
ffmpeg -y -i src -vf "fps=30000/1001,scale=1280:720:flags=lanczos" -c:v libx264 -preset slow -b:v $VB -pass 2 -passlogfile L -pix_fmt yuv420p \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv -c:a aac -b:a 96k -movflags +faststart out.mp4
```

| Encoder, target, format | Target bytes | Actual bytes | % of target | Video kbit/s asked | Wall (both passes) | load1 |
|-------------------------|-------------:|-------------:|------------:|-------------------:|-------------------:|------:|
| libx264 2-pass, 10 MB, 720p30, AAC 96k | 10 000 000 | 9 527 641 | 95.28 % | 2516 | 76.0 s | 32 |
| libx264 2-pass, 25 MB, 1080p60, AAC 128k | 25 000 000 | 23 798 313 | 95.19 % | 6402 | 111.1 s | 31 |
| libx264 2-pass, 8 MiB, 720p30, AAC 96k | 8 388 608 | 7 977 566 | 95.10 % | 2095 | 69.6 s | 40 |
| libx264 2-pass, 10 MB, 1080p30, AAC 96k | 10 000 000 | 9 509 667 | 95.10 % | 2516 | 86.6 s | 37 |
| libx265 2-pass (`-x265-params pass=1/2:stats=..`), 10 MB, 1080p30 | 10 000 000 | 9 539 563 | 95.40 % | 2516 | 200.4 s | 45 |
| libsvtav1 1-pass VBR (`-svtav1-params rc=1`), 10 MB, 1080p30, Opus | 10 000 000 | 7 255 828 | **72.56 %** | 2516 | 57.5 s | 31 |
| h264_vaapi 1-pass `-rc_mode VBR -b:v $VB -maxrate 1.5x`, 10 MB, 720p30 | 10 000 000 | 9 321 699 | 93.22 % | 2516 | **6.5 s** | 38 |
| h264_vaapi 1-pass VBR, 25 MB, 1080p60 | 25 000 000 | 22 760 045 | 91.04 % | 6402 | **6.5 s** | 35 |

Nothing overshot. The two-pass software encoders land consistently at 95.1 to 95.4 %.
Where the 4.7 % goes (measured on the 10 MB / 720p30 file): x264 delivered 2446.5 kbit/s
for 2516 asked (-2.8 %), the native AAC encoder delivered 82.5 kbit/s for 96 asked
(-14 %; 109.8 for 128), the 2 % margin, and MP4 overhead of about 24 kB (0.25 %).
SVT-AV1's single-pass VBR missed the bitrate by 27 % on this 30 s clip and is not usable
for size targeting this way.

### 6.2 Refined method (audio first, then one corrective pass 2)

1. Encode the audio alone: `ffmpeg -i src -vn -c:a aac -b:a 96k a.m4a`; take its real size.
2. `video_budget_bytes = target * (1 - 0.01) - audio_bytes - 0.003 * target`;
   `VB = video_budget_bytes * 8 / duration`.
3. Two-pass x264 as above, muxing the finished audio with `-i a.m4a -map 0:v:0 -map 1:a:0 -c:a copy`.
4. If the file is over the target or under 98 % of it: measure the real video payload
   (`ffmpeg -i out.mp4 -map 0:v -c copy -f h264 - | wc -c`), set
   `VB2 = VB * video_budget_bytes / video_payload_bytes`, and repeat **pass 2 only**
   (pass-1 stats are reused).

| Target | After step 3 | After step 4 | Wall incl. correction | load1 |
|--------|-------------:|-------------:|----------------------:|------:|
| 10 MB, 720p30 | 9 639 293 (96.39 %) | **9 901 829 (99.02 %)** | 99.8 s | 32 to 39 |
| 25 MB, 1080p60 | 24 026 230 (96.10 %) | **24 736 808 (98.95 %)** | 144.4 s | 39 |
| 8 MiB, 720p30 | 8 077 579 (96.29 %) | **8 308 876 (99.05 %)** | 104.6 s | 39 to 42 |

Three of three landed within 1.05 % under the target after one correction and none went
over. Only this one clip (30 s) was tested; very short clips (a few seconds) have
proportionally larger container overhead and less room for rate control, so the
over-target check in step 4 must stay as a loop (bounded, e.g. 3 iterations).
Quality (VMAF) of the size-targeted files was not measured.

---

## 7. GIF export and clipboard

### 7.1 GIF pipeline quality

`spikes/s4-io/bench_gif.sh`: 5 s of the source starting at 12 s (spider close-up, shallow
depth of field, smooth bright gradients). PSNR/SSIM are computed in RGB against the same
scaled frames stored lossless (FFV1). Time includes seeking/decoding the 4K source
(identical for all rows). load1 20 to 36.

Commands (`PRE="fps=20,scale=480:-2:flags=lanczos"`):

```
# A ffmpeg, one global palette, error-diffusion dither
ffmpeg -ss 12 -t 5 -i src -an -filter_complex "$PRE,split[a][b];[a]palettegen=stats_mode=full[p];[b][p]paletteuse=dither=sierra2_4a" out.gif
# B ffmpeg, diff palette, ordered dither, rectangle updates
ffmpeg ... -filter_complex "$PRE,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" out.gif
# C ffmpeg, new palette per frame
ffmpeg ... -filter_complex "$PRE,split[a][b];[a]palettegen=stats_mode=single[p];[b][p]paletteuse=new=1:dither=sierra2_4a" out.gif
# D gifski, frames piped as y4m
ffmpeg -ss 12 -t 5 -i src -an -vf "$PRE,format=yuv444p" -f yuv4mpegpipe - \
  | gifski --quiet --y4m-color-override bt709 --fps 20 --quality 90 -o out.gif -
```

| Pipeline | Size | Time | Bytes | PSNR (RGB) | SSIM |
|----------|------|-----:|------:|-----------:|-----:|
| A ffmpeg global palette + sierra2_4a | 480 px, 20 fps | 26.1 s | 8 928 053 | 29.16 | 0.878 |
| B ffmpeg diff palette + bayer + rectangle | 480, 20 | 20.5 s | 6 971 470 | 29.92 | 0.923 |
| C ffmpeg per-frame palette + sierra2_4a | 480, 20 | 18.7 s | 10 026 707 | 29.77 | 0.894 |
| D gifski `--quality 100` | 480, 20 | 8.7 s | 8 117 225 | **34.64** | 0.934 |
| D gifski `--quality 90` | 480, 20 | 8.4 s | 6 583 535 | 33.16 | 0.911 |
| D gifski `--quality 70` | 480, 20 | 8.2 s | 4 269 233 | 30.25 | 0.849 |
| D gifski `--quality 90 --lossy-quality 60 --motion-quality 80` | 480, 20 | 8.5 s | 4 858 235 | 30.26 | 0.840 |
| A | 640 px, 25 fps | 46.9 s | 19 063 510 | 29.61 | 0.856 |
| B | 640, 25 | 33.6 s | 14 028 922 | 30.86 | 0.917 |
| C | 640, 25 | 51.3 s | 21 084 522 | 30.47 | 0.878 |
| D gifski `--quality 100` | 640, 25 | 11.2 s | 16 753 646 | **34.62** | 0.921 |
| D gifski `--quality 90` | 640, 25 | 11.0 s | 13 384 681 | 33.29 | 0.897 |
| D gifski `--quality 70` | 640, 25 | 10.5 s | 8 450 180 | 30.40 | 0.831 |
| D gifski q90 + lossy 60 + motion 80 | 640, 25 | 11.1 s | 9 731 472 | 30.45 | 0.822 |
| gifski q90, y4m **without** `--y4m-color-override bt709` | 480, 20 | 9.4 s | 6 552 203 | 31.94 | 0.905 |
| gifski q90, PNG frames as input | 480, 20 | 14.0 s | 6 536 927 | 32.07 | 0.906 |

- gifski at quality 90 is smaller than the best ffmpeg variant (B) and 3.2 to 3.3 dB
  better, and 2 to 4x faster end to end.
- Visual check (frame 50, 200x150 crop at 2x, all variants side by side,
  `gif/cmp.png`): gifski q90 is nearly indistinguishable from the reference at 2x zoom;
  ffmpeg B (bayer) shows posterised steps in the bright background gradient and is
  visibly darker (mean RGB of the frame 126/133/107 vs reference 131/137/113); ffmpeg A
  keeps the gradient but with visible dither noise.
- **y4m carries no matrix tag**: without `--y4m-color-override bt709` gifski decodes
  BT.709 footage as BT.601 (mean green 140 vs 137 in the reference, PSNR -1.2 dB). With
  the override the mean colour matches the reference (132/137/113).
- PNG input costs 5.6 s more for 100 frames and scored 1.1 dB lower than y4m with
  override (frame mean 130/136/112, slightly darker than the reference). I did not find
  out why; y4m + override is both faster and closer.
- A 5 s 480 px GIF of real footage is 6.6 MB at quality 90: GIF export should show the
  resulting size and offer width/fps/quality, because chat apps commonly cap uploads.

### 7.2 Wayland clipboard

Backed up the user's clipboard first (9 offered types, 4 bytes of text copied from the
browser) and restored it afterwards as `text/plain` (the Chromium-specific types and
`text/html` were not restored; `wl-copy` can only offer one type). **The user's machine
runs `wl-paste --watch cliphist store` and `wl-clip-persist --clipboard both`**; both
affect the results below, and cliphist has recorded the test items (a 1.8 MB GIF and a
0.3 MB MP4, several times) in the clipboard history.

Tested with `giphy/direct-giphy.gif` (1 799 229 bytes) and `giphy/direct-giphy-hd.mp4`:

| Invocation | `wl-paste --list-types` | Round trip |
|------------|-------------------------|------------|
| `wl-copy --type image/gif < a.gif` | `image/gif` | `wl-paste --type image/gif` byte-identical |
| `wl-copy < a.gif` (no type) | `image/gif` (auto-detected) | - |
| `printf 'file://%s\r\n' "$PWD/a.gif" \| wl-copy --type text/uri-list` | `text/uri-list text/plain text/plain;charset=utf-8 TEXT STRING UTF8_STRING` | URI returned (use `wl-paste -n`; plain `wl-paste` appends a newline) |
| `printf 'copy\nfile://%s' "$PWD/a.gif" \| wl-copy --type x-special/gnome-copied-files` | `x-special/gnome-copied-files` | content returned |
| `wl-copy --type video/mp4 < a.mp4` | `video/mp4` | byte-identical |
| `clip-multi /abs/a.gif image/gif` (own program, see below) | `text/uri-list x-special/gnome-copied-files image/gif text/plain;charset=utf-8` + `text/plain UTF8_STRING TEXT STRING` | all four payloads correct, GIF byte-identical |
| `clip-multi /abs/a.mp4 video/mp4` | same with `video/mp4` | MP4 byte-identical |

Findings:

- `wl-copy` offers **exactly one MIME type per invocation** (plus the text aliases when
  the type is textual). A paste target that wants a file reference and one that wants
  image bytes cannot both be served by one `wl-copy` call; a second call replaces the
  first.
- Offering several types at once works from our own process: `spikes/s4-io/clip-multi`
  (about 30 lines, `wl-clipboard-rs` 0.9.5, `Options::copy_multi` with one `MimeSource` per
  type) offered `text/uri-list` + `x-special/gnome-copied-files` + raw bytes +
  `text/plain` in one selection, verified with `wl-paste`. Build:
  `cd spikes/s4-io/clip-multi && CARGO_TARGET_DIR=/disk/shotgun-dev/target-s4 cargo build --release`.
- **Lifetime.** A Wayland selection is served by the process that owns it; when that
  process exits the selection is gone unless a clipboard manager took a copy. `wl-copy`
  therefore forks and stays alive until another client takes the selection. On this
  machine `wl-clip-persist` takes the selection over within 0.5 s: both
  `wl-copy --foreground` and `clip-multi` had already exited (ownership lost) when checked
  after 0.5 to 0.7 s, and the data was still pasteable afterwards. What happens
  **without** a persistence daemon could not be tested here without stopping the user's
  daemon; per the protocol the content would vanish with the owner.
- `wl-clip-persist` and `cliphist` each read the full payload of every offered type.
  Offering a 25 MB MP4 as raw bytes means it is copied through pipes into both daemons
  on every copy. Not measured.

**Reasoned, not tested in a real paste target** (no browser/Discord/Telegram paste was
performed):

- Browsers and Electron apps (Discord, Element, Slack) expose clipboard *files* to the
  page when the selection offers `text/uri-list` with `file://` URIs; that is the path
  that uploads the original file with its name and keeps a GIF animated or a video a
  video. For `image/*` bytes Chromium decodes the image and hands pages a **PNG**, so
  `image/gif` bytes alone are expected to paste as a still frame in Chromium-based
  targets. Raw `video/mp4` bytes are not a clipboard type browsers map to anything.
- Telegram Desktop and other Qt apps read `text/uri-list` as file URLs (Qt
  `QMimeData::urls()`); GTK/GNOME file managers read `x-special/gnome-copied-files`
  (`copy\nfile://...`), KDE/Qt file managers read `text/uri-list`.
- So the type set to offer is: `text/uri-list` (primary), `x-special/gnome-copied-files`,
  the raw bytes under the real MIME type only for GIF/PNG (useful for image editors and
  native apps), and `text/plain` with the path as a last resort. Raw bytes for video
  should be left out.
- The URI must point to a file that stays on disk (`exports/`), must be percent-encoded
  (`clip-multi` does not do that), and must be an absolute path.

**Needs a human check:** paste after "GIF to clipboard" and "MP4 to clipboard" into
Discord (browser and desktop), Telegram Desktop, a browser upload field and the file
manager in use, with `clip-multi`'s type set.

---

## What failed or did not work

- `hevc_vaapi` + `-tag:v hvc1` produces an undecodable file on this GPU (section 5).
- yt-dlp has no Giphy and no Google Photos extractor; its generic fallback returns
  Giphy's worst rendition and fails on Google Photos.
- Google Photos video download: not tested (no share link with a video).
- `--download-sections` gives no progress and wrong container duration.
- A custom yt-dlp sort containing `br` selected the DRC audio track.
- Per-process `ffmpeg -ss` seek timing is meaningless for proxies (133 ms process start).
- 50 simultaneous 4K HEVC decoders in one ffmpeg process were killed (`Killed`; most likely out of memory on 16 GB, kernel log not checked).
- ffmpeg's native VP9 decoder ignores VP9 alpha (section 4).
- `hevc_vaapi -rc_mode ICQ` is not supported by the driver ("received no packets").
- `h264_vaapi -bf 2` gave a byte-identical file to no `-bf`: B-frames are not used.
- A bash bug in the first proxy run (`local a=.. out=$a..` expands before assigning)
  overwrote the 540p files with the 720p ones; fixed and rerun, numbers above are from
  the rerun.
- First alpha PSNR run paired wrong frames (Matroska ms timestamps vs MOV); fixed by
  forcing identical timestamps on both inputs.
- `libx265 -preset slow` was not measured (aborted for time), and VMAF of the size-targeted
  files was not measured.
- SVT-AV1 single-pass VBR missed the requested bitrate by 27 % (section 6).
- Machine load from other agents: all speeds are lower bounds; 5.2 repeats the master
  encoder speeds on a quieter machine, the proxy/conform/GIF/size-target speeds were not repeated.

---

## Decision

### D1 Link import

Route by URL, in this order:

| URL | Method |
|-----|--------|
| `giphy.com/gifs/...`, `giphy.com/stickers/...`, `media*.giphy.com/media/...` | Extract the id (last `-`-separated token of the page slug, or the path segment before the file name on media URLs). `GET https://media.giphy.com/media/<id>/source.gif` with a non-HTML `Accept` (`*/*`), fall back to `giphy.gif`. Never the MP4. Label from the page `<title>` if a page URL was pasted. |
| `photos.app.goo.gl/...`, `photos.google.com/share/...` | `GET` the page with a browser User-Agent, extract items with the regex in `spikes/s4-io/gphotos_list.py`, download each with `<url>=d` (photos). Videos: try `<url>=dv`, **unverified**. On any parse failure show "Google Photos link could not be read, download the file in the browser and drop it here". |
| Response `Content-Type` is `video/*`, `image/*` or `audio/*` (HEAD, or first GET) | Plain HTTP download to `<name>.part`, resume with `Range`, rename on success. Own User-Agent. |
| everything else | yt-dlp, default format selection |

yt-dlp invocation:

```
yt-dlp --no-playlist --newline \
  --progress-template "download:SGPROG %(progress)j" \
  --progress-template "postprocess:SGPP %(progress.status)s %(progress.postprocessor)s" \
  --print after_move:"SGFILE %(filepath)s" --no-simulate \
  --write-info-json -o "<project>/assets/video/.tmp-<job>/%(id)s.%(ext)s" <url>
```

- Keep yt-dlp's default selection (best resolution/fps; AV1 or VP9 + Opus in WebM). This
  FFmpeg decodes it. Add `-S "hdr:SDR"` for as long as the app has no HDR pipeline;
  remove it when `05-color.md` handles PQ/HLG input.
- Do not use `--download-sections`, do not put `br` in a custom `-S`.
- Label = `title`; store `webpage_url`, `uploader`, `upload_date`, `extractor_key`, `id`
  in the index file.
- YouTube WebM is CFR content with millisecond timestamps: it passes the VFR scan with
  the tolerance below and its rate must be snapped (59.94 from `avg_fps` 59.9399), so it
  does **not** need a conform.

### D2 Probe

```
ffprobe -v error -print_format json -show_format -show_streams <file>
```

plus, for every video asset, one packet scan that yields VFR status and frame count:

```
ffprobe -v error -select_streams v:0 -show_entries stream=time_base:packet=pts -of json <file>
```

- Rule (implemented in `spikes/s4-io/vfr_detect.py`): sort PTS, deltas, median;
  off = `|delta - median| > max(1 tick, 0.5 % of median)`; **VFR if any delta is off**.
- Frame rate of a CFR asset = `(packets - 1) / (last_pts - first_pts)` snapped to the
  nearest of 23.976, 24, 25, 29.97, 30, 48, 50, 59.94, 60, 100, 119.88, 120 when within
  0.1 %; otherwise keep the exact rational from `avg_frame_rate`.
- The scan costs about 2.7 s per 3.5 GB right after import (file in page cache) and 7 s
  cold. Run it as part of import step 2 in the job pool; until it finishes show the asset
  as usable with the header verdict (`r_frame_rate != avg_frame_rate` -> assume VFR;
  only meaningful for MP4/MOV).
- Bit depth from `pix_fmt` (not `bits_per_raw_sample`); rotation from the
  `Display Matrix` side data `rotation`; HDR from `color_transfer`, not from side data.
- GIF and animated WebP always go through the alpha conform (D4), regardless of the scan.

### D3 Proxy

**H.264 all-intra, 960x540 (height 540, width by aspect), 8-bit 4:2:0, source frame rate,
no audio, MP4.**

```
ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128 -hwaccel_output_format vaapi -i <src> -an \
  -vf "scale_vaapi=w=-2:h=540,hwdownload,format=nv12" \
  -c:v libx264 -preset veryfast -tune fastdecode -crf 20 -g 1 -pix_fmt yuv420p \
  -movflags +faststart <project>/.shotgun/cache/proxy/<id>.mp4
```

(`scale_vaapi=w=-2:h=540` verified; colour tags of the source carry over to the proxy,
verified with ffprobe. For VFR sources build the proxy from the conform, not the original.)

Measured: 94.7 MB per minute at 59.94 fps (12.6 Mbit/s), 1.3 ms seek + decode on one
core, 767 fps single-thread and 3557 fps multi-thread software decode, encoder 1009 fps;
end to end 3.7 to 4.4x realtime from a 4K60 HEVC source, bounded by the source decode.
Reasons over the alternatives: smallest all-intra format (MJPEG, ProRes proxy, DNxHR LB
are 1.9 to 2.6x larger), same seek cost as MJPEG/DNxHR within 0.4 ms, every frame
independently decodable (no GOP bookkeeping for reverse scrubbing), and it is plain
H.264 High so a GPU decode path stays possible. 720p proxies cost 1.57x the size
(149 MB/min) and 2.1 ms per seek; offer 720p as a setting, not as the default.

Use the libav API in-process for proxy playback. Starting an ffmpeg process per frame
costs 133 ms before any work is done.

### D4 Conform

Opaque video (VFR, odd codecs/containers):

```
# 8-bit source
ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128 -hwaccel_output_format vaapi -i <src> \
  -vf "fps=<snapped rate>" -c:v hevc_vaapi -rc_mode CQP -qp 14 -g 15 -bf 0 \
  -color_primaries <src> -color_trc <src> -colorspace <src> -color_range <src> \
  -c:a copy <project>/.shotgun/cache/conform/<id>.mp4
# 10-bit source: -vf "fps=<snapped rate>,scale_vaapi=format=p010" ... -c:v hevc_vaapi -profile:v main10 ...
# keep the default hev1 tag; never pass -tag:v hvc1 to hevc_vaapi (section 5.3)
```

Measured at 4K60 8-bit: 0.56 GB per minute (75 Mbit/s), VMAF 99.89, PSNR-Y 53.5 dB,
159 fps = 2.65x realtime under load. **Condition:** the timeline must decode HEVC on the
GPU (S1); software decode of this file reached only 48 fps. If S1 ends with software
decode only, use instead

```
-vf "fps=<rate>" -c:v libx264 -preset veryfast -crf 14 -g 15 -bf 0 -pix_fmt yuv420p
```

(0.40 GB/min, VMAF 99.28, 195 fps software decode, 37 fps encode under load; 8-bit only,
so 10-bit HDR phone footage would be truncated: not acceptable once HDR matters).

Not verified for the hevc_vaapi conform: the combined `fps` + `scale_vaapi` filter chain
on hardware frames with a real VFR 10-bit phone file (the CFR check used software decode
+ `hwupload` on 8-bit files), and `-c:a copy` of phone audio into MP4.

Transparent sources (GIF, animated WebP, APNG):

```
ffmpeg -i <src> -vf "fps=<rate>" -c:v utvideo -pix_fmt gbrap <project>/.shotgun/cache/conform/<id>.mkv
```

`<rate>` (proposal; only `fps=50` and `fps=25` were run) = 100 / smallest GIF delay in
centiseconds, capped at 50 (GIF timestamps are 1/100 s). Lossless RGBA, every frame a keyframe,
229 fps decode at 720x720 under load, 20 MB per second for a 720x720 GIF, 6 MB per
second for a 480x480 WebP sticker. FFV1 is 1.1 to 3.3x smaller but decodes 2.4x slower
and was not tested with `-g 1`; ProRes 4444 is not lossless and larger; VP9 alpha does
not survive FFmpeg's decoder.

### D5 Master export

Encoder settings below assume SDR BT.709 output from the renderer. All presets: MP4,
`-movflags +faststart`, AAC 256k (`-c:a aac -b:a 256k`; audio quality was not evaluated
in this spike), and the four colour flags.

**Default "top quality": HEVC Main10, hardware, constant QP 20.**

```
# as measured (file input, GPU decode); in the app the input is the renderer's frame stream
ffmpeg -hwaccel vaapi -hwaccel_device /dev/dri/renderD128 -hwaccel_output_format vaapi -i <input> \
  -vf scale_vaapi=format=p010 -c:v hevc_vaapi -profile:v main10 -rc_mode CQP -qp 20 \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv \
  -c:a aac -b:a 256k -movflags +faststart tmp-hev1.mp4
ffmpeg -i tmp-hev1.mp4 -c copy -tag:v hvc1 -movflags +faststart <project>/exports/<name>.mp4    # 0.44 s for 30 s of 4K60
```

Measured: 21.6 Mbit/s (81 MB for 30 s of 4K60), VMAF 99.55 / min 97.75, 3.16x realtime
including the 4K60 decode. Two rules that are not optional: never pass `-tag:v hvc1` to
`hevc_vaapi` directly (5.3; remux instead, or ship `hev1`), and after every hardware
export decode the result once and compare a few frames (or at least check for decoder
errors) before reporting success. The 10-bit path was fed from an 8-bit source here
(`scale_vaapi=format=p010`); feeding real 10-bit frames from the renderer is untested.

**Alternative 1, faster and smaller: AV1 10-bit, hardware.**

```
-vf scale_vaapi=format=p010 -c:v av1_vaapi -rc_mode CQP -global_quality 60
```

13.1 Mbit/s (-39 % size), VMAF 99.05 / min 96.47, 3.80x realtime. Use `-global_quality`
with `av1_vaapi`; `-qp 60` produced a 2.4x larger 1 s test file, so the two options are
not equivalent.

**Alternative 2, maximum compatibility (H.264 High 8-bit):**

```
-c:v h264_vaapi -rc_mode CQP -qp 18                 # 25.5 Mbit/s, VMAF 99.18, 1.88x realtime
-c:v libx264 -preset slow -crf 16 -pix_fmt yuv420p  # 29.0 Mbit/s, VMAF 98.98, 0.26x realtime, no GPU needed
```

**Alternative 3, smallest file (archive/upload on a slow line):**

```
-c:v libsvtav1 -preset 6 -crf 24 -pix_fmt yuv420p10le -svtav1-params tune=0   # 9.6 Mbit/s, VMAF 98.99, 0.24x realtime
```

Why hardware is the default although "software = quality" is the usual rule: on this
clip the hardware encoders were equal or better on every measured score at 7 to 27x the
speed, and speed is the project's first priority. What this spike cannot show: behaviour
on noisy or dark phone footage, on grain, and at lower bitrates, where hardware encoders
are known to fall behind. **Needs a human check:** export one dark/noisy real clip with
the default and with `libx264 -preset slow -crf 16`, and compare by eye before the
default is frozen in `01-product.md`.

### D6 Size-targeted share copy

H.264 High 8-bit + AAC in MP4 (plays inline everywhere), two-pass libx264, method 6.2:

```
ffmpeg -y -i <render> -vn -c:a aac -b:a 96k a.m4a                       # 128k for targets >= 25 MB
# VB = ((TARGET * 0.99) - size(a.m4a) - TARGET * 0.003) * 8 / DURATION
ffmpeg -y -i <render> -vf "fps=<fps>,scale=-2:<h>:flags=lanczos" -c:v libx264 -preset slow -b:v $VB -pass 1 -passlogfile L -pix_fmt yuv420p -an -f null -
ffmpeg -y -i <render> -i a.m4a -map 0:v:0 -map 1:a:0 -vf "fps=<fps>,scale=-2:<h>:flags=lanczos" -c:v libx264 -preset slow -b:v $VB -pass 2 -passlogfile L \
  -pix_fmt yuv420p -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv -c:a copy -movflags +faststart out.mp4
# if size(out) > TARGET or < 0.98 * TARGET: VB = VB * video_budget / video_payload(out); repeat pass 2 (max 3 times)
```

Lands at 98.95 to 99.05 % of the target (10 MB, 25 MB, 8 MiB on the 30 s clip); none of
the 14 files produced by either method exceeded its target. Takes 100 to 144 s for 30 s of video under load
(three x264 `slow` encodes); use `-preset medium` or drop the correction pass if that is
too slow. Quick mode: `h264_vaapi -rc_mode VBR -b:v $VB -maxrate 1.5*$VB`, one pass,
6.5 s, landed at 91 to 93 % (never over in 2 runs). Do not size-target with SVT-AV1
single-pass VBR (-27 %).

Resolution/frame-rate ladder used in the tests (worked, quality not scored): video
bitrate >= 6 Mbit/s -> 1080p60; 2 to 6 Mbit/s -> 720p30 or 1080p30; below that reduce
further. This ladder is a proposal, not a measured optimum.

"MB" must be defined in the UI: the tests used 10 000 000 bytes and 8 388 608 bytes; pick
the stricter decimal interpretation unless the destination's limit is known to be MiB.

### D7 GIF and clipboard

```
ffmpeg -i <render> -an -vf "fps=<fps>,scale=<w>:-2:flags=lanczos,format=yuv444p" -f yuv4mpegpipe - \
  | gifski --quiet --y4m-color-override bt709 --fps <fps> --quality 90 -o <project>/exports/<name>.gif -
```

Defaults 480 px wide, 20 fps, quality 90 (6.6 MB for 5 s of detailed footage, PSNR 33.2
dB vs 29.9 dB for the best ffmpeg palette pipeline at a larger size). Show the resulting
size; offer quality 70 (-35 % size) and smaller width as the way to shrink.
`--y4m-color-override bt709` is mandatory.

Clipboard: do not shell out to `wl-copy` (one MIME type only). Own the selection from the
app process (the UI toolkit's Wayland data source, or `wl-clipboard-rs` `copy_multi` as in
`spikes/s4-io/clip-multi`) and offer, for a file that already exists in `exports/`:

| MIME type | Payload | For |
|-----------|---------|-----|
| `text/uri-list` | `file:///abs/percent-encoded/path\r\n` | browsers, Electron apps (Discord), Qt apps (Telegram), KDE file managers |
| `x-special/gnome-copied-files` | `copy\nfile:///abs/path` | GTK file managers |
| `image/gif` (GIF only; `image/png` for stills) | file bytes | image editors, native apps |
| `text/plain;charset=utf-8` | absolute path | terminals, fallback |

No raw `video/mp4` bytes. The app must keep serving the selection while it runs; after
exit the content survives only if a clipboard manager (here: `wl-clip-persist`) took it.
Offering the four types and reading them back is verified; **actual pasting into
Discord/Telegram/browser/file manager is not verified and needs a human check.**
