# Performance log

One entry per milestone (or fix) for every budget in `spec/04-performance.md`
it touches: date, commit, command, number, budget, pass or miss. Newest last.

## 2026-11-18, M1, commit bd69f81

Command: `nix develop --command bash scripts/bench-m1.sh visible`
(raw output of this run: `/disk/shotgun-dev/tmp/bench-m1.out`).
Machine state: load average 1.4 (1 min) / 2.8 (5 min); the user was watching a
video in a browser. Release build. Library and projects on `/disk` (SATA SSD).

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Start to interactive library, warm (32 projects), `main()` to first presented frame | median 71.6 ms, max 81.5 ms (n=10, hidden workspace); 73.5 ms in the one visible run | 300 ms | pass |
| Same, wall time from before `exec` until the process has exited again | median 86 ms, max 96 ms (n=10) | 300 ms | pass |
| Open project, 50 000 clips + 50 018 undo steps: open request to presented editor frame | median 108.6 ms, max 129.8 ms (n=10, hidden); 131.3 ms in the one visible run (includes waiting for the next vblank) | 500 ms | pass |
| Open project, empty: same | median 7.5 ms, max 8.2 ms (n=10) | 500 ms | pass |
| Open bundle without window (`sg-bench open`), 50 000 clips | p50 92.0 ms, max 100.0 ms (n=10) | 500 ms | pass |
| Edit op apply on 50 000 clips / 20 tracks: split, trim, move, insert, ripple delete | p99 0.97 to 3.75 us, max 47 us (n=20 000 each) | 1 ms | pass |
| Ripple on all 20 tracks at once (one batch op) | p50 17.6 us, p99 32.6 us, max 92 us | 1 ms | pass |
| Undo of each of the above | p99 0.90 to 29.0 us, max 180 us | 1 ms | pass |
| Journal append (serialise + `write`), 820-byte records | p50 4.95 us, p99 14.7 us, max 145 us (n=50 000) | 1 ms | pass |
| Idle CPU, library on screen, visible and unfocused, 2.4 s | 0 CPU ticks on every thread, 3 frames presented in 3.0 s (`scripts/idle-check.sh shotgun-vis 3`, one run). An earlier visible 3 s run reported 0.51 s process CPU in total with the same 3 frames; it had no per-thread sampling, was not reproduced, and is unexplained | ~0 % | pass, with that one open question |

Not a budget, for information:

- Journal `fsync` (writer thread, once a second while editing): 22 to 41 ms
  (n=5). It never runs on the UI thread.
- Start-up breakdown (log line of the app): Vulkan adapter 46 to 66 ms, device
  3 to 13 ms, window 1 to 2 ms, surface + egui 5 to 9 ms. The adapter query is
  most of the start time.
- In the app, a project rename took 0.3 us to apply and 1.1 us to journal
  (performance overlay, key left of `1`).

Not measured:

- A **cold** start (page cache empty, first start after boot). Dropping the
  page cache needs root. All start numbers above are warm.
- UI frame time. M1 draws only panels; the timeline renderer and its
  50 000-clip frame budget are M4. The first frame of a session costs about
  6.6 ms CPU (font atlas), seen once in the overlay.
- Open time with a journal much longer than the project (for example 1 M
  records on a 50 000-clip project). Open reads the whole journal to rebuild
  the undo history, so it grows with history length: 15 MB / 50 000 records
  are inside the 92 ms above. Journal compaction (03, "retained horizon") is
  not built yet and is what bounds this.

## 2026-11-18, M2, commit 8e12f98

Command: `nix develop --command bash scripts/bench-m2.sh`.
Machine state: load average 4.0; the user was watching a video. Release build.
File: 1 045 170 304 bytes of 4K60 HEVC on `/disk` (SATA SSD), dropped into a
project on the same disk.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Asset usable after local drop, 1 GB, file **not** in the page cache | 1834, 1838, 1837 ms | 2 s | pass, with 8 % to spare: this is the disk reading 1 GB (about 570 MB/s) |
| Same, file in the page cache | 354, 350, 421 ms | 2 s | pass |

"Usable" is measured in the app from the import request to the `AddAsset` op:
the file is hashed (BLAKE3 on all cores) and probed by then. The copy into the
bundle and all derived files follow in the background.

For information, small files (same run as the screenshots, one each): a 7 MB
MP4, a 40 kB MP3 and a PNG were usable after 139 to 239 ms, of which about
130 ms is the `ffprobe` process starting.

History of this number: the first implementation copied while hashing and
reported the asset after the copy. That took 3.4 s cold and between 0.9 and
4.7 s warm (the write competes with the read on one SATA disk and stalls when
the kernel throttles dirty pages), a miss. Hashing first and copying afterwards
fixed it.

Not measured: a source on the NVMe home disk (it has no room for a 1 GB test
file), a file larger than RAM, several large drops at once.

## 2026-11-18, M3, commit 417b707

Commands: `nix develop --command bash scripts/bench-m3.sh` (audio engine on a
PipeWire null sink, then the app, hidden, driven by script) and
`sg-bench seek <file> vaapi|<threads> <skip>` for the decoder alone.
Machine state: load average 2.3 to 4.9; the user was watching a video in a
browser the whole time. Release build. Test media: `/disk/shotgun-dev/media/s1`.

Scrub and play times are measured inside the preview engine, from the seek or
play command to the composited frame; the display adds up to one refresh
(7 ms at 144 Hz, 17 ms at 60 Hz) that is not in these numbers.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Scrub: seek to proxy frame, 4K60 HEVC source with 1 s GOP (72 seeks) | p50 3.1 ms, p95 6.4 ms, max 32.0 ms | 50 ms p95 | pass |
| Scrub: seek to proxy frame, three long-GOP sources (74 seeks) | p50 3.3 ms, p95 8.6 ms, max 280 ms (first frame of a file: the decoder is opened) | 50 ms p95 | pass |
| Full-quality frame after a seek, 4K60 HEVC, **1 s GOP** (72 seeks, 0.5 s apart) | p50 174.5 ms, p95 332.3 ms, max 472 ms | 150 ms p95 | **MISS** |
| Full-quality frame after a seek, long-GOP 4K HEVC 8/10-bit and 1080p H.264 (71 seeks) | p50 139.2 ms, p95 441.8 ms, max 852 ms | none (open point in 01) | - |
| Play start: Space to sound running with the frame on screen (12 starts each run) | p50 2.1 ms, p95 2.4 ms, max 5.0 ms | 80 ms | pass |
| Audio stream start alone (`sg-bench audio`, 10 starts) | p50 0.78 ms, max 16.1 ms | 80 ms | pass |
| Playback of the proxy, 12 x 0.4 s | 276 frames, 0 late or skipped | 0 dropped | pass (proxy quality, one layer) |

Audio engine (PipeWire null sink, 20 s): quantum 256 frames (5.33 ms), 4035
callbacks, 0 graph xruns, 0 ring underruns, worst `process()` 10.8 us,
realtime thread SCHED_RR, heard position within 1 sample (0.02 ms) of
wall-clock time. The null sink reports 0 frames of output delay; the real
device reported 256 frames in spike S3.

Why the full-quality budget is missed, and what was tried:

- Software decode of 4K HEVC cannot do it: p50 388 ms, p99 467 ms for the
  1 s GOP file with all cores (`sg-bench seek uhd60_hevc8_gop60.mp4 0 0`),
  1158 ms with one thread. Skipping non-reference frames did not help.
- Decoding on the GPU through VAAPI and reading back only the wanted frame
  brings the decoder alone to p50 99 to 106 ms, p99 138 to 149 ms
  (`sg-bench seek ... vaapi 1`, three runs): inside the budget.
- But that is with seeks back to back. With 0.5 s between seeks, as when a
  person clicks around, the same decoder takes p50 262 ms, p99 323 ms
  (`SG_SEEK_GAP_MS=500 sg-bench seek ... vaapi 1`); with 0.1 s gaps p50
  122 ms, p99 265 ms. The decode hardware slows down or powers down when it
  idles. The app numbers above are this case.
- Not tried: keeping the decode hardware busy while the user scrubs; the
  zero-copy import of spike S1 (saves the read-back, not the idle penalty);
  an intra mezzanine of originals (the open point in 01).
- What the user sees meanwhile: the proxy frame after 3 to 7 ms, replaced by
  the full-quality frame when it arrives.

Not measured:

- Anything through the real sound device or seen on a real monitor: the app
  ran on a hidden workspace with sound into a null sink. A/V sync, picture
  smoothness and audible glitches need a person.
- The xrun matrix of S3 on a quiet machine (still open in 08, stage 0).
- "3 simultaneous 4K60 HEVC layers, 0 dropped": M5.

## 2026-11-18, M4, commit ae30869

Command: `nix develop --command bash scripts/bench-m4.sh`.
Machine state: load average 4.5 to 4.7; the user was watching a video. Release
build. Window 1600x900, rendered off-screen (the app was on a hidden
workspace), 1440 frames per run, three runs each.

What is measured: the CPU time of a whole UI frame (input, UI build with the
timeline lookup and instance fill, tessellation, buffer upload, encoding,
submit) while the timeline zooms between "whole sequence" and 200 points per
second and sweeps across the sequence, the script of spike S2.

| Project | CPU frame time p50 | p99 (three runs) | max | Quads | Budget (p99) | Result |
|---------|-------------------:|-----------------:|----:|------:|-------------:|--------|
| 10 clips, 2 tracks | 0.21 ms | 0.26, 0.34, 0.38 ms | 1.19 ms | 13 | 6.9 ms | pass |
| 50 000 clips, 20 tracks (rows 10 pt high, no labels) | 0.28 ms | 0.66, 0.69, 0.72 ms | 1.30 ms | 7453 | 6.9 ms | pass |
| 50 000 clips, 4 tracks (rows 40 pt high, labels) | 0.27 ms | 0.39, 0.45, 0.47 ms | 0.96 ms | 1497 | 6.9 ms | pass |
| 500 000 clips, 20 tracks (10 x the budgeted size) | 0.31 ms | 1.49, 1.63, 2.98 ms | 4.50 ms | 7453 | 6.9 ms | pass |

These are lower than the spike's 2.2 to 3.2 ms: the spike redrew a 1280x720
viewer texture and six sliders every frame and laid out up to 200 changing
labels; here the viewer is a texture the player owns and the synthetic clips
all carry the same short label.

Not measured:

- Frame pacing on a real monitor (missed vblanks at 144 Hz). The numbers
  above are CPU time per frame, not presentation intervals.
- Frame time during real mouse interaction (drag, wheel): the benchmark sets
  zoom and scroll directly.
- A project with thousands of distinct, long labels.
- Git: commit time of the auto-commit was not re-measured in the app; S5
  measured 12 ms idle and 29 to 43 ms under load for the same commands.

## 2026-11-18, M5, commit 0128fda

Command: `nix develop --command bash scripts/bench-m5.sh`.
Machine state: load average 7.7 (1 min) at the start; the user was watching a
video and a release build had just finished. App hidden, sound into the null
backend.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Realtime playback, 3 simultaneous 4K60 HEVC layers (8-bit, 10-bit, 1 s GOP), 3 x 10 s | 1797 frames composited, 0 late or skipped | 0 dropped | pass, **at proxy quality** |
| Play start with three layers | 2.9 to 3.4 ms (n=3) | 80 ms | pass |

What this does and does not show:

- The three layers play from their 540p all-intra proxies, decoded in
  software and uploaded. That is the adaptive preview the architecture
  allows; it is not three 4K decodes. Full-resolution realtime playback needs
  the zero-copy VAAPI import of spike S1, which is still not built.
- "Late" is counted by the preview engine (a frame composited without its
  exact picture, or after skipping a frame number). Frames the compositor
  presented late or dropped on the way to the monitor are not visible to it;
  the app was hidden.
- No colour chain exists yet (M9), so "+ colour chain" of the budget line is
  not covered.

Not measured: compositing cost per layer on the GPU, playback with keyframed
transforms on every layer, title rasterisation time (done once per text and
size on the render thread).

## 2026-11-18, M6, commit 5fa02d0

Command: `nix develop --command bash scripts/bench-m6.sh`.
Machine state: load average 8.9 (1 min) at the start; the user was working on
the desktop. Exports ran through `sg-bench export` (the code the export panel
runs), no window.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| 4K60 HEVC master, one 4K60 HEVC clip, 20 s: render and encode | 57.9 and 56.8 fps (0.96x and 0.95x realtime) | >= realtime where the encoder allows | **miss, by 4 %** |
| Same, whole export including the check decode | 47.6 and 47.0 s for 20 s (0.42x) | (same line) | **miss** |
| 1080p60 master, two video layers + title + sound, 20 s | 229 fps render; 10.9 s in all (1.84x) | (no line; for comparison) | |
| 1080p60 vertical 1080x1920 | 228 fps render; 9.2 s in all (2.18x) | | |
| Share copy, 10 MB target | 9 850 995 bytes (98.5 %), 720p30, 28.9 s | never over the target | pass |
| Share copy, 25 MB target | 24 629 630 bytes (98.5 %), 1080p60, 32.2 s | never over the target | pass |
| GIF, 3 s | 632 kB, 480x270, 20 fps, 3.4 s | | |

Every file was probed: masters are `hevc` / `hvc1` / `yuv420p10le` / 60 fps
with the exact frame count (1200) and AAC sound where the sequence has
sound; share copies are `h264` / `yuv420p`.

Per frame at 4K, the three stages run side by side: decode 10 ms, upload and
GPU colour conversion 6 ms plus compositing and read-back 9 ms (one thread),
feeding the encoder 17 ms. The encoder process is the slowest stage: alone,
reading the same frames from a file, it takes 10.8 ms per frame (93 fps), so
it is slowed by sharing the GPU and memory with the other stages. The check
decode of the finished 4K file (software, 10-bit, 129 Mbit/s) takes 26 s for
20 s of video and is what makes the whole export 0.42x.

How it got here (4K60, same clip): 13 fps as first written (RGBA read-back,
colour conversion to 10-bit on the CPU inside ffmpeg, one thread doing
everything, CPU conversion of every decoded frame to RGBA); 20 fps with NV12
made on the GPU; 36 fps with three stages and the 10-bit step moved to the
GPU encoder's scaler; 57 fps with decoded planes converted on the GPU.
Decoding through VAAPI with read-back was tried and is slower (13 fps).

Not measured: export on a quiet machine; 4K with several layers; 10-bit
sources (they still take the CPU conversion path); long exports (hours);
the software fallback encoder; the clipboard hand-over (never run
automatically, by rule).

## 2026-11-19, M7 (and feedback round 3), commit 7c8971e + angle monitor

Commands: `scripts/bench-m4.sh`; the spike run of
`docs/spikes/s6-sync-beats.md`; scripted runs of the app on a synthetic
multicam project (`/disk/shotgun-dev/media/m7`: a 2 minute song at 128 bpm and
three "phone" recordings of it, made by `spikes/s6-sync-beats write`).
Machine state: night, load average 2 to 4, app hidden, sound into the null
backend. No budget in `04-performance.md` names sync, beats or angle
switching; the UI frame-time budget is the one this work could have broken.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| UI frame time p99, 10 clips | 0.28 to 0.41 ms | 6.9 ms | pass |
| UI frame time p99, 50 000 clips on 20 tracks | 0.64 ms | 6.9 ms | pass |
| UI frame time p99, 50 000 clips on 4 tracks | 0.38 to 0.42 ms | 6.9 ms | pass |
| UI frame time p99, 500 000 clips | 1.43 to 1.78 ms | 6.9 ms | pass |
| UI frame time p99, multicam project with waveform (1500 columns) and beat grid | 0.60 ms | 6.9 ms | pass |
| Find a take in a 200 s song (spike, release build) | 0.44 s (12 s take) to 1.26 s (150 s take) | none | |
| Find the beats of a 180 s song | 0.46 to 0.50 s | none | |
| Position error of a synced take (synthetic) | at most 1.47 ms; in the app each take landed on the expected frame | none (a frame is 16.7 ms) | |
| Cutting between 3 angles during playback, 4 cuts in 7 s | 424 frames composited, 1 late | 0 dropped (general playback budget) | **1 late frame** |
| Same before the angle monitor kept all angles decoded | 428 frames, 8 late | | |

Notes:

- In the multicam frame-time run the waveform only counts in the run where
  its peaks had been loaded before the measurement began (the third of
  three); the first two measured 24 quads and 0.48 ms.
- "Late" as in M5: a frame composited without its exact picture, counted by
  the preview engine. All angles play from proxies while playing.
- Nothing here was measured with real footage or a real recording; see the
  spike for what the synthetic recordings do and do not cover.

Not measured: sync and beat analysis inside the app on a long real song
(they run on their own thread; the UI does not wait), the monitor with more
than three angles, switching by mouse or keyboard in real time.

## 2026-11-19, M8, commit after ce0388b

Command: scripted runs of the hidden app on the synthetic multicam project
(`/disk/shotgun-dev/tmp/lib-m7/band`), `sg-bench export`. Load average 2 to 3.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Playback across a whip-pan (9 s around it, 1080p60 sequence, proxies, 3-angle monitor on) | 541 frames, 0 late | 0 dropped | pass |
| Export of 20 frames containing the whip-pan | 71 fps render | none | |

The blur takes 48 samples per pixel for the four frames of a transition; no
GPU time was measured separately. Preview and export frames of the
transition were compared side by side by eye (same picture); they are drawn
by the same function (`PlanLayer::draw`) and shader.

Not measured: a whip between 4K clips at full quality, many transitions in
a row, the zoom and slide kinds during playback.

## 2026-11-19, M9, commit after f6beb30

Command: scripted runs of the hidden app on `/disk/shotgun-dev/tmp/lib-m9/match`:
three 4 s clips of the same shot (Big Buck Bunny, 720p), one as it is and
two changed with ffmpeg (`exposure`, `colortemperature`, `eq` contrast and
saturation: about -0.5 EV cooler flatter, and +0.35 EV warmer harder), in
one scenario with the unchanged clip as the hero.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Playback with match and a four-module look (tone, split tone, vignette, grain), 1080p60 sequence, 6 s | 359 frames, 0 late | 0 dropped | pass |
| Export of the same, 120 frames | 229 fps render | none | |
| Measuring one asset (12 frames at 160 px) | not timed separately; the three finished within the 0.5 s wait of the script | none | |

Match quality, average colour of the picture area (sRGB, R G B) at the same
moment of the shot:

| Clip | Unmatched | Matched | Hero |
|------|-----------|---------|------|
| darker, cooler, flatter | 55 65 44 | 75 92 49 | 83 94 53 |
| brighter, warmer, harder | 101 106 40 | 85 88 58 | 83 94 53 |

Both are much closer than before; neither is equal. The second, whose
bright grass was clipped by the change, comes out a little pale. This is one
animated shot altered by filters that work on gamma-encoded values, not
several cameras on one scene: it says the arithmetic works, not how good
the match is on real takes (that stays `OPEN` in 05).

Not measured: the cost of the look per module on the GPU, 4K, more than one
graded layer at once.

## 2026-11-19, M10 part 1 (10-bit masters), commit after 7f181ce

Command: `sg-bench export <project> master`. Load average 3.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| 4K60 master, ungraded (8-bit frames widened on the GPU as before), 10 s | 60.5 fps render (1.01x); 24.2 s in all (0.41x) | >= realtime where the encoder allows | render pass at the line; whole export **miss** (check decode) |
| 4K60 master with real 10-bit frames from the compositor (forced, for comparison), 20 s | 33.5 fps render | same | **miss**: the encoder process takes 29.5 ms per frame to read and upload 24 MB |
| 1080p60 master with real 10-bit frames (graded project), 12 s | 160 to 190 fps render | none | |

Real 10-bit frames are therefore only used when a scenario grades the
picture (a look, or a match between two or more takes); an ungraded 8-bit
source has no values between its steps to keep. A 4K graded master is
below realtime. The earlier M6 figure of 57 fps for the ungraded case was
measured under load 9; this one under load 3.

## 2026-11-19, re-run after the night's additions, commit 64b2bbf

Commands: `scripts/bench-m3.sh`, `scripts/bench-m5.sh`. Load average 2 to 5.
Since the last run of these the layer shader gained the input transform
and look chain, and every jump while paused now asks the audio engine for a
scrub grain.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Scrub latency, proxy, 4K60 1 s GOP | p50 3.0, p95 5.7 ms | <= 50 ms p95 | pass |
| Scrub latency, full quality, same source | p50 195, p95 310 ms | <= 150 ms p95 | **miss** (as before: 175 / 332) |
| Scrub latency, proxy, long-GOP sources | p50 3.1, p95 13.2 ms | <= 50 ms p95 | pass |
| Play start (first frame) | p50 4.9, p95 6.6 ms | <= 80 ms | pass |
| Play start (audio, on a null sink) | p50 0.5, p99 18.6 ms | <= 80 ms | pass |
| Audio over 20 s | 0 xruns, 0 underruns, worst process() 5.6 us | 0 | pass |
| Three 4K60 layers playing (proxy quality) | 1796 frames, 0 late | 0 late | pass (proxy quality only, as before) |
| Scrub grains on a null sink (PipeWire) | three seeks 1 s apart: three sounds of 100 ms; three seeks 0.12 s apart: one unbroken 0.33 s | none | works |

## 2026-11-19, export check decoded by the graphics card, commit after 48cebf3

Command: `scripts/bench-m6.sh`. Load average 2.4 to 6.9. The check that an
exported file plays (decode all of it once) now uses VAAPI and falls back
to the processor if that fails; before, the processor decoded it.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| 4K60 master, 20 s, whole export | 25.1 s = 0.80x realtime (was 0.41x); render pass 62.8 fps | >= realtime where the encoder allows | **miss**, by a fifth; the render pass alone is at 1.05x |
| Check decode of that file alone | 5.2 s by VAAPI, 26.5 s by the processor | none | |
| 1080p60 master, two layers and a title, 20 s | 6.5 s = 3.09x (was 1.8x) | same | pass |
| 1080p60 vertical | 6.7 s = 2.98x | same | pass |
| Share copy under 10 MB / 25 MB | 26.4 s / 27.6 s = 0.76x / 0.73x | none (two-pass x264) | |
| GIF, 3 s | 2.6 s | none | |

## 2026-11-19, re-run of M1 and M4 after the night's additions, commit 5c8e43e

Commands: `scripts/bench-m1.sh`, `scripts/bench-m4.sh`. Since the last run
the timeline draws fades, fade marks and keyframe marks on clips, the key
file is read at start and the library asks for project pictures.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Start to library frame | median 119 ms, max 148 ms | <= 300 ms | pass |
| Open 50 000-clip project to editor frame | median 113 ms, max 128 ms | <= 500 ms | pass |
| Open bundle without a window, 50 000 clips | p99 102 ms | <= 500 ms | pass |
| UI frame time, 10 clips | p99 0.29 to 0.41 ms | <= 6.9 ms p99 | pass |
| UI frame time, 50 000 clips on 20 tracks | p99 0.66 ms | same | pass |
| UI frame time, 50 000 clips on 4 tracks, labels | p99 0.39 to 0.66 ms | same | pass |
| UI frame time, 500 000 clips | p99 1.55 to 1.83 ms | same | pass |

## 2026-11-19, after feedback round 4 (buttons, volume line), commit after 4081eb4

Command: `scripts/bench-m4.sh`. Since the last run the editor draws two
rows of buttons (about 30) and a volume line on every sound clip.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| UI frame time, 10 clips | p99 0.41 to 0.47 ms | <= 6.9 ms p99 | pass |
| UI frame time, 50 000 clips on 20 tracks | p99 0.78 to 0.85 ms | same | pass |
| UI frame time, 50 000 clips on 4 tracks, labels | p99 0.51 to 0.52 ms | same | pass |
| UI frame time, 500 000 clips | p99 2.25 to 4.89 ms (three runs) | same | pass, slower than the 1.55 to 1.83 ms before; one run of three stood out |

## 2026-11-20, export with one asset on two layers, commit after 6bc4863

Command: `sg-bench export /disk/shotgun-dev/tmp/lib-r9/pairs2 master 0 600`
(1080p60, a 30 fps H.264 clip on V1 and a copy of it overlapping on V2).

| Metric | Before | After | Budget | Result |
|--------|--------|-------|--------|--------|
| 1080p60 master, overall | 0.44x realtime | 3.01x | >= 1x | pass (was a miss nobody had measured) |
| Decode per frame | 33.8 ms | 1.3 ms | | |
| Render pass | 28 fps | 280 fps | | |

`scripts/bench-m5.sh` after the change to the player's decode workers:
1797 frames, 0 late.

## 2026-11-20, stage 3: recording, metronome, sends, loudness (after 726ec13)

Commands: `cargo nextest run -p sg-audio --run-ignored only a_take_lands`
(a loop without air: played into a null sink, recorded from its monitor);
`sg-bench audio shotgun-null 20` (one track sending to the reverb and the
echo, metronome on, loudness measured: everything the realtime thread
can be asked to do); `sg-bench export <project> sound:flac:stems:-14`.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| A take against where it was played (engine alone) | 4 frames, 0.1 ms | PROPOSED: within 1 ms | pass |
| The same through the whole application | 5 frames, 0.1 ms | | pass |
| Recorded metronome clicks against their beats | 5 frames | | pass |
| Worst realtime callback, quantum 256 (5.33 ms) | 125 us | under the quantum | pass |
| Graph xruns, ring underruns over 20 s | 0, 0 | 0 | pass |
| Audio start, p99 | 12.3 ms | <= 80 ms | pass |
| Sound export to -14 LUFS, by ffmpeg `ebur128` | -14.0 LUFS | within 0.5 LU | pass |
| 5 s song, FLAC with two tracks beside it | 0.8 s | | |

Not measured: a real microphone and speakers (a device that reports its
delay wrongly shifts takes by that error).

## 2026-11-21, stage 3 continued: playing by hand, loop, monitoring, mastering (commit 815cd3e)

Commands: `scripts/bench-m3.sh` (machine under load: load average 4 to 10,
a game running); `cargo nextest run -p sg-audio --run-ignored only`
(null sinks, no speakers, no microphone); the app hidden with its sound
into a null sink whose monitor was recorded (`scripts/run-hidden.sh` with
`SHOTGUN_AUDIO=sink:<null sink>`); `sg-bench midi shotgun-dev ...` as a
stand-in keyboard; ffmpeg's `ebur128=peak=true` on exported files.

| Metric | Measured | Budget | Result |
|--------|----------|--------|--------|
| Worst realtime callback, quantum 256 (5.33 ms), after tonight's changes to it | 86 us | under the quantum | pass |
| Graph xruns, ring underruns over 20 s (`sg-bench audio`) | 0, 0 | 0 | pass |
| Audio start, p99 | 18.6 ms | <= 80 ms | pass |
| A take against where it was played | 10 frames, 0.2 ms | PROPOSED: within 1 ms | pass |
| A monitored input, from one null sink to the next | 300 to 312 frames, 6.2 to 6.5 ms late | none set | |
| A key played by hand until its sound is in the output (engine test, block of 256) | at most 1200 frames, 25 ms | none set | |
| A looped two-bar clip, five rounds, start of the note each round | 192 000 samples apart every time | to the sample | pass |
| Hand-played track running dry, three blocks ahead topped up every 5 ms | 5 times in 9 s, 18 times in 13.5 s | 0 | **MISS**, changed |
| The same, four blocks ahead topped up every 1 ms in pieces of 128 frames | 0 in six runs of 10 to 21 s; 2 and 3 in three runs during which other programs were started | 0 | **MISS** under load |
| The same with Surge XT (VST3, CLAP) as the instrument, looping, 14 s | 0, 0 | 0 | pass |
| Master to -14 LUFS / -9 LUFS from a vocal at -20.4 LUFS, true peak by ffmpeg | -14.1 LUFS / -9.4 LUFS, -1.2 dB true peak both | within 0.5 LU, at most -1 dB | pass |
| The same before the true-peak limiter and the repeated loudness passes | -10.4 LUFS for -9, -0.9 dB | | |
| Sampler: a 110 Hz recording from its root key and an octave above, exported | 110 Hz, 220 Hz | | pass |
| Full-quality frame after a seek, 4K60 HEVC, 1 s GOP | p95 334 ms | 150 ms p95 | MISS (as before, unchanged) |
| UI frame, 10 to 500 000 clips (`scripts/bench-m4.sh`, at c795d48, load average 7) | p99 0.5 to 3.9 ms | 6.9 ms p99 | pass |
| Start to the library frame (`scripts/bench-m1.sh`, at 4642688, with MIDI and RTKit asked at start) | median 146 ms, max 161 ms | 300 ms | pass |
| Open a 50 000-clip project to the editor frame | median 122 ms | 500 ms | pass |
| Film exports (`scripts/bench-m6.sh`): 1080p60 master / 4K60 master | 2.27x / 0.67x realtime, files probe as before (a game was using CPU and GPU; 4K was 0.80x on 2026-11-19, a known miss) | >= realtime where the encoder allows | 4K: MISS as before, not comparable tonight |

Afterwards (same night): the feeding thread asks RTKit for nice -11,
and a mistake was found that explains part of the above: since 815cd3e
the graph was rebuilt whenever the device's block size was first learnt,
a gap each time. With both changed, the run with the stand-in keyboard
and a recording of the sink beside it, which had run dry 3, 3 and 9
times, did not in five repeats. Under heavier load than this machine had
tonight it may still happen; then each time is one audible gap on that
track.

Not measured: anything through a real sound device, a real MIDI keyboard,
a bridged Windows plugin, how the delay of hand play feels.

## 2026-11-22 (night): after the night's work on chain, look and AI

| What | Measured | Budget | |
|------|----------|--------|---|
| UI frame, 10 to 500 000 clips (`scripts/bench-m4.sh`, at c3f4686, load average 4, with the new look) | p99 0.4 to 1.9 ms | 6.9 ms p99 | pass |
| "Voice": measuring a take and fitting its settings on it (`sg-bench voice`, takes of 15 to 30 s, up to six passes through the stages with noise reduction) | 0.2 to 2.5 s per take | none set | |
| Cutting a person out, 1080p60, `sg-matte` on the CPU with 8 threads, VP9 with transparency out (337 frames while a build ran) | 4 frames a second | none set | |
| The same, the model alone at 1080p / 960x540 / 640x360 (`spikes/s8-matte/rvm.py`) | 8.3 / 12.6 / 22.8 frames a second | none set | |
| Lossless copy a cut-out is played from (utvideo with transparency), 1080p60 | 77 MB per second of video | none set | |
| Export of a 4 s 1080p sequence with a cut-out over a clip (`sg-bench export ... master`) | 1.85x realtime, 175 frames a second in the render pass | >= realtime | pass |

Not measured tonight: playback budgets (M3, M5) with the stages' new
parts (fourth-order low cut, make-up, the taps for the chain's picture);
the picture of the chain costs ten passes over each block of one track
on the disk thread while its window is open.
