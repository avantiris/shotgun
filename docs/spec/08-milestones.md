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
| M5 (done) | Compositing: transform, crop, opacity, speed, titles, keyframes (classic + live record), basic audio fades/gain | realtime with layers |
| M6 (done) | Export: 16:9 master, 9:16 with reframe, compressed share copy, GIF to clipboard; export panel + repeat-last | export speed, output verified by probing the file |

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
  real time. Playback shows proxies. Not built: (audio grains while scrubbing
  were added 2026-11-19, see 06)
  plugin delay compensation (no processor with latency exists
  yet), the user-adjustable A/V offset. Not verifiable without a person:
  sound through the real device, A/V sync, smoothness on a real monitor.

- **M4 done 2026-11-18** (commit `ae30869`). Changed or added on the way:
  picture and sound of a take are kept together by a rule (same asset, same
  time, same source position), not by a link stored in the project; Alt+click
  picks one clip alone. Moves and trims show a ghost and are applied when the
  button is released, so the viewer does not follow a drag live. Added after
  feedback round 2: dragging an asset onto the timeline, file drops placed
  on the timeline, track buttons and menu. Not built at M4:
  dragging an asset from the panel onto the timeline (Enter or a double-click
  places it at the playhead instead), dropping a file directly onto a
  position in the timeline, overwrite and insert edits (a move onto occupied
  space is refused), waveforms and thumbnails on clips, track add/remove/
  mute in the UI, renaming markers, (the key map file was added 2026-11-19, see 07), journal
  compaction. After restoring an older state, media of assets that state does
  not know stays in `assets/` instead of moving to the trash. Not verifiable
  without a person: every mouse gesture on the timeline (they are covered by
  tests of the planning code and by scripted key actions, not by real input).

- **M5 done 2026-11-18** (commit `0128fda`). The three-layer realtime budget
  is met at proxy quality only (`docs/perf-log.md`). Built: position,
  scale, rotation, opacity (all keyframeable, linear interpolation), crop,
  free easing curves (added 2026-11-19: each stretch between two keyframes is Linear, Smooth or Hold, chosen in the inspector under "Between keys", for picture values, speed ramp and volume alike; and scale and rotate handles in the viewer: corners size, the knob above turns; dragging moves;
  easing curves (scale and rotate handles in the viewer were added 2026-11-19: corners size, the knob above turns; dragging moves;
  [added 2026-11-19: the fade fields now fade the picture too, and a split
  gives both halves their share of the properties]
  scale and rotation are inspector fields), picture fades (the fade fields
  act on sound only), keyframes for volume [added 2026-11-19: `F` on a
  sound clip keys its volume] (the value at the clip start was
  used), time-stretch (a speed change resamples, so pitch follows speed),
  title fonts other than the built-in one, a keyframe lane in the timeline.
  A clip's length does not change when its speed does. Splitting a clip
  copies no properties to the right half. Not verifiable without a person:
  dragging in the viewer and live record (real pointer input), and how any
  of it looks in motion.

- **M6 done 2026-11-18** (commits `d2124d5`, `5fa02d0`), with one missed
  budget: a 4K60 master renders at 57 fps (0.95x realtime), and the whole
  export with its check decode takes 2.4 times the length of the video
  (`docs/perf-log.md`). 1080p60 exports at 1.8x realtime in all. Built:
  master, vertical (the frame turned upright, pictures fill it), share copy
  under a size, GIF; export panel, repeat-last, GIF key; the sequence frame
  can be set to 1080p, 4K, 9:16 or 1:1. Not built: reframing per clip for
  the vertical export beyond the clip's own position and scale (the export
  crops the centre), choosing a range for master and share copy [added
  2026-11-19: a checkbox in the export panel] (they exported
  the whole sequence; only the GIF takes the selection or marker range),
  export of 10-bit pictures as real 10 bits (the file is 10-bit, the
  pipeline in front of the encoder is 8-bit until M10), a queue of several
  exports, a "reveal in file manager" action. Export reads the constant-rate
  conform of a variable-rate source instead of its original (`PROPOSED`,
  see 02). Not verifiable without a person: the keys and the panel with real
  input, the clipboard hand-over into a chat or browser, how the files look
  and sound in a player and on a phone.

- **Input without a person (added 2026-11-19).** Several entries above say
  that mouse and key input could not be verified without a person. Since
  then `crates/sg-ui/tests/flows.rs` drives the whole UI headless through
  egui's own input handling (egui_kittest): clicks on labelled controls,
  right-click menus, key presses, drags and the wheel. Covered there:
  placing assets from their menu, dragging an asset onto the timeline,
  selecting, moving and trimming clips by dragging, the cutting keys
  (S, Q, E, X) and undo, clicking the ruler, wheel zoom, dragging the
  picture in the viewer, titles, the add popover, creating a project in the
  library, track buttons and menus, the mixer, scenarios and looks, the
  song and sync menus, transitions, export. Still only a person can judge:
  what it looks and sounds like, real drag-and-drop from another program,
  the system file picker, the clipboard, and anything about timing by hand
  and ear.

Then the user tests and gives feedback. Feedback is triaged into fixes before
stage 2 starts.

## Stage 2: music video (second user test)

| M | Scope |
|---|-------|
| M7 (done) | Auto-sync takes to the song, beat detection + grid + snap, live multicam switching |
| M8 (done) | Whip-pan transition engine (+ slide/zoom variants) |
| M9 (done, in part) | Scenarios, match stage, look chain modules, presets, A/B |
| M10 (done, in part) | Speed ramps, stabilisation, background render cache, 10-bit masters |

Status:

- **Feedback rounds 2 and 3 (2026-11-18/19)**, between the stages: drag and
  drop onto the timeline, track buttons, sound-only import and placement,
  waveforms, the mixer with noise reduction, button padding
  (`docs/feedback/`).

- **M7 done 2026-11-19** (commits `56efe99` spike S6, `7c8971e`, and the
  angle monitor after it). Built: a song per sequence with detected beats as
  a grid to snap to (tempo can be halved or doubled); takes found in the song
  by their camera sound and laid in time on hidden camera tracks; a monitor
  showing all angles at the playhead; keys 1 to 9 or a click cut the
  programme track to an angle, also during playback. Added later the same
  night: "Cuts to the beat" (every cut of the programme moves onto the
  nearest beat within a third of a beat, as a roll edit, so sync is kept)
  and "Whip every cut", both in the SONG section of the inspector. Design
  decisions taken
  without the user (night run), all `PROPOSED`: camera angles are ordinary
  video tracks marked hidden, the programme is the visible track above
  them, a cut overwrites the programme from the playhead to the end of the
  take; the camera sound of a synced take is not put on the timeline; takes
  whose match is not clear are not placed at all. Not built: correcting a
  clock difference between camera and song (measured and reported; the take
  is exact at its middle), choosing the first beat of the bar, syncing a
  take by hand when the automatic match fails (it can be dragged, with the
  waveform of its sound as a guide only if that sound is placed), a
  tempo-changing grid editor. **Not verified with real footage**: every
  number comes from synthetic recordings (`docs/spikes/s6-sync-beats.md`).
  Not verifiable without a person: cutting live by ear and eye, and whether
  the cuts land where they were meant.

- **M8 done 2026-11-19.** `B` puts a whip-pan on the cut nearest to the
  playhead (again: removes it); its direction alternates with the one
  before. The clip inspector sets kind (whip, slide, zoom), length, direction,
  blur, travel and speed-up. Built as specified in 05 with these
  differences: a whip or zoom shows one picture per frame (the outgoing one
  up to the cut, the incoming one after it), so it needs no spare frames
  beyond the cut; only the slide shows both and runs them past the cut
  (holding the last or first frame if the source ends). Not built: scale and
  rotation overshoot, chromatic split, moving the transition off the centre
  of the cut, transitions on audio (a cut in the sound stays a cut).
  Not verifiable without a person: how it feels in motion.

- **M9 done in part 2026-11-19.** Built: scenarios (right click on an asset
  or clip, stripe on the clips, `scenarios/<id>.toml`); the match
  (measurement of twelve frames per asset, a parametric correction of
  exposure, white balance, contrast and saturation towards a hero or the
  middle of all, strength 0 to 100 %); a reorderable look chain of up to
  eight modules with bypass and mix (Balance, Tone, Colour, Split tone,
  Vignette, Grain) run in one shader pass with the match; looks copied
  between scenarios and saved to `looks/`; A/B keys (`J` look, `Shift+J`
  match); the colour panel (`K`). Added later the same night: Sharpen /
  soften and Glow modules (they sample the picture around each pixel as it
  is after the match, not after the modules before them in the chain; the
  glow is 24 samples per pixel, not a multi-pass blur) and a per-clip trim
  (exposure, warmth) in the clip inspector; and the Film look module: a
  `.cube` look-up table (dropped onto the window, kept in `assets/lut/`)
  chosen per scenario and applied where the module sits in the chain.
  Also the input transform for phone HDR: HLG and PQ sources (BT.2020) are
  brought to standard range by a fixed tone map (ordinary white at 203 nits
  stays white, highlights above three quarters of white roll off), in the
  layer shader and in the measurement alike. Checked by a round trip: a clip
  converted to HLG with ffmpeg and shown next to its original (average
  colour 72 81 48 against 80 91 53). HDR frames are still decoded to 8 bits
  first. **Not built**: built-in film looks to choose from (only tables you bring),
  skin protection in the Colour module (its hue-shift knobs were added; vibrance
  protects what is already saturated, not skin as such), membership per
  clip (it is per asset), analysis
  per cut range, dithering of the
  output, the skin-tone part of the measurement. The A/B switches are part
  of the project (and of the export) rather than a view setting. Not
  verified: the match on real multi-camera footage (`docs/perf-log.md` has
  what was tried instead).

- **M10 done in part 2026-11-19.** Done: 10-bit masters with real 10-bit values where
  the picture is graded (the compositor's floating-point result leaves the
  GPU as P010). Sources are still decoded to 8 bits, so a 10-bit *source*
  is not yet carried through at full depth. Speed ramps: a keyframeable
  factor on a clip's speed ("Speed ramp" in the clip inspector; `F` sets a
  key at the playhead); the source position is the area under the curve.
  The picture follows the ramp, the sound of the take does not, and the
  clip keeps its length. Checked in the app: a ramp from 1 to 0.2 between
  frames 60 and 300 showed source second 4.4 at frame 600, as computed.
  Stabilisation: "Stabilise" in the clip inspector shows a copy of the
  asset with the shake taken out, made once per asset in the background by
  ffmpeg's vid.stab filters (two passes, automatic zoom to hide the
  borders) with a proxy of its own; preview and export read it. Checked on
  a still scene shaken by up to 40 pixels: the mean difference between
  consecutive exported frames fell from 36.5 to 3.0 (of 255). `PROPOSED`:
  it is all or nothing per clip with fixed settings (smoothing over 20
  frames); the whole asset is processed even for a short clip. Not built:
  the background render cache (nothing built so far needs it to play in
  real time at proxy quality), carrying 10-bit sources through at full
  depth, ramping the sound with the picture.

## Stage 3: DAW

Recording, MIDI/piano roll, plugin hosting (CLAP/VST3/LV2), mixing/mastering.
Planned separately once stage 2 is accepted; the audio engine from M3 is its
foundation.
