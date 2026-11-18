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
