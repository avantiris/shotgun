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
