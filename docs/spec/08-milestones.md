# 08 Milestones

Each milestone ends with: code builds without warnings, tests pass, the feature
is exercised in the running app, relevant budgets from `04-performance.md` are
measured and logged in `docs/perf-log.md`, and a commit is made.

## Stage 0: harness (this stage)

- [x] Requirements agreed (01)
- [x] Specs 02-07
- [x] Nix dev shell
- [x] Spikes S1-S5 run, results in `docs/spikes/`, specs updated to match
- [ ] Quiet-machine re-runs after a reboot (all spikes ran under heavy load
      and partly on a crashed GPU driver): S1 `scripts/quiet.sh` (3 paced
      4K60 streams, window playback), S2 `bench.sh main`, S3 xrun matrix.
      May be done as the first step of M3 instead of blocking M1/M2.

## Stage 1: casual editing (first user test)

| M | Scope | Proves |
|---|-------|--------|
| M1 (done) | Workspace + crates, window, library view, create/open project, bundle layout, journal + snapshot model in `sg-core` | start/open budgets, edit-op budget |
| M2 (done) | Import: drag-drop, file picker, paste link/file/image, yt-dlp, probe, hash, thumbnails, peaks, conform, proxy; asset panel | asset-usable budget |
| M3 (done) | Playback: decode pool, GPU viewer, audio engine on PipeWire, A/V sync, scrubbing, frame cache | scrub/play budgets, audio latency numbers |
| M4 (done) | Timeline: multi-track, split/trim/ripple/move/snap, selection, markers, undo/redo, autosave, git auto-commits + checkpoints | 50k-clip UI budget |
| M5 | Compositing: transform, crop, opacity, speed, titles, keyframes (classic + live record), basic audio fades/gain | realtime with layers |
| M6 | Export: 16:9 master, 9:16 with reframe, compressed share copy, GIF to clipboard; export panel + repeat-last | export speed, output verified by probing the file |

Status:

- **M1 done 2026-11-18** (commit `bd69f81`, numbers in `docs/perf-log.md`).
  Deliberately left for later milestones: git auto-commits and checkpoints
  (M4, only `git init` + the first commit exist), gesture coalescing in the UI
  (the journal format has the `group` field for it), journal compaction, the
  key map config file (the table is in `sg-ui/src/keymap.rs`), poster frames
  in the library (M2, tiles are coloured placeholders).

- **M2 done 2026-11-18** (commit `8e12f98`). Not verifiable without a person:
  a real drag from a file manager or browser, a real Ctrl+V after copying
  in another application, and the system file picker (the code paths behind
  them are exercised by script). Not built: HTTP resume for direct downloads,
  Google Photos videos (still unverified, see 01), tags in the UI, the
  cancel-and-captured-stderr rule is met but a failed step only shows its
  last error line in the banner.

- **M3 done 2026-11-18** (commit `417b707`), with one missed budget: the
  full-quality frame after a seek into 4K HEVC takes 175 ms (p50) and 332 ms
  (p95) against 150 ms, because the decode hardware slows down when it idles
  between seeks (`docs/perf-log.md`). Built differently from the spikes'
  plan, on purpose: frames are decoded to system memory and uploaded
  (software for proxies, VAAPI with read-back for full-quality frames); the
  zero-copy import of S1 is left for M5, where three 4K layers must play in
  real time. Playback shows proxies. Not built: audio grains while
  scrubbing, plugin delay compensation (no processor with latency exists
  yet), the user-adjustable A/V offset. Not verifiable without a person:
  sound through the real device, A/V sync, smoothness on a real monitor.

- **M4 done 2026-11-18** (commit `ae30869`). Changed or added on the way:
  picture and sound of a take are kept together by a rule (same asset, same
  time, same source position), not by a link stored in the project; Alt+click
  picks one clip alone. Moves and trims show a ghost and are applied when the
  button is released, so the viewer does not follow a drag live. Not built:
  dragging an asset from the panel onto the timeline (Enter or a double-click
  places it at the playhead instead), dropping a file directly onto a
  position in the timeline, overwrite and insert edits (a move onto occupied
  space is refused), waveforms and thumbnails on clips, track add/remove/
  mute in the UI, renaming markers, the key map config file, journal
  compaction. After restoring an older state, media of assets that state does
  not know stays in `assets/` instead of moving to the trash. Not verifiable
  without a person: every mouse gesture on the timeline (they are covered by
  tests of the planning code and by scripted key actions, not by real input).

Then the user tests and gives feedback. Feedback is triaged into fixes before
stage 2 starts.

## Stage 2: music video (second user test)

| M | Scope |
|---|-------|
| M7 | Auto-sync takes to the song, beat detection + grid + snap, live multicam switching |
| M8 | Whip-pan transition engine (+ slide/zoom variants) |
| M9 | Scenarios, match stage, look chain modules, presets, A/B |
| M10 | Speed ramps, stabilisation, background render cache, 10-bit masters |

## Stage 3: DAW

Recording, MIDI/piano roll, plugin hosting (CLAP/VST3/LV2), mixing/mastering.
Planned separately once stage 2 is accepted; the audio engine from M3 is its
foundation.
