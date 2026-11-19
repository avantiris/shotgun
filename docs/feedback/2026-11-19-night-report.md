# Night of 2026-11-18 to 19: report

Written for Linus to read in the morning. State: branch `main`, last code
commit `78ffd19`, `scripts/check.sh` passes, nothing uncommitted.

Try it: `nix develop --command cargo run -p sg-app --release`, then F1 for
the keys. Your project "ananas" was opened and exported with this build on
a copy (`/disk/shotgun-dev/tmp/lib-real-copy`); the real library was not
touched.

## Your five points (round 3): all built

1. A link can be imported as sound only (Add, "Sound only"). Checked with a
   real YouTube link. A plain file link now also loses its picture.
2. Shift while dragging an asset places only picture or only sound; right
   click on a clip removes its sound or its picture.
3. Mixer (M): fader, pan, mute, solo, low cut, noise reduction, three-band
   EQ and compressor per track, limiter on the master, meters.
4. Sound clips show their waveform.
5. Buttons have padding.

## Built after that

- Stage 2 milestones M7 to M10 (multicam by camera sound, beat grid, cuts
  with keys 1 to 9, transitions, colour scenarios with match and looks,
  .cube tables, phone HDR, 10-bit masters, speed ramps, stabilisation).
- Editing: scrub is heard; Ctrl while moving a clip overwrites; arrow keys
  nudge clips; H fits the timeline; fades are drawn on clips and pulled at
  a mark; clips show their keyframes; keyframes can be linear, smooth or
  held; handles in the viewer size and turn a picture; Fit / Fill.
- Titles: fonts (drop a .ttf or .otf) and a shadow.
- Multicam: a take the automatic sync refuses can be laid and lined up by
  hand (asset menu).
- Keys can be changed in `~/.config/shotgun/keys.conf` (written on the
  first start by hand).
- Library tiles show a picture of the project; a finished export has a
  "Show in folder" button.

## Numbers (docs/perf-log.md)

| Budget | Measured | |
|---|---|---|
| Start to library, 300 ms | 119 ms | pass |
| Open 50 000 clips, 500 ms | 113 ms | pass |
| UI frame, 6.9 ms p99 | 0.3 to 1.8 ms up to 500 000 clips | pass |
| Scrub, proxy, 50 ms p95 | 5.7 to 13 ms | pass |
| Scrub, full quality, 150 ms p95 | 310 ms | **miss** |
| Three 4K60 layers | 0 late frames, at proxy quality only | pass with that limit |
| 4K60 master, realtime | 0.80x overall (was 0.41x) | **miss** |
| 1080p60 master | 3.1x | pass |

## Decided for you (all marked PROPOSED in the specs)

- The export check runs on the graphics card, not the processor (this is
  what moved 4K from 0.41x to 0.80x). It is a weaker check.
- Every jump while paused plays a tenth of a second of sound; no switch.
- Ctrl is the overwrite modifier for moves; arrows nudge; H fits.
- The sound of a take follows the picture's speed ramp (pitch moves).
- A GPU error no longer ends the program: it is logged, and an export
  running at that moment fails.
- Earlier in the night: camera takes on hidden tracks with a programme
  track above, limiter always on, noise reduction with half a second of
  lag, scenarios per asset, stabilisation all-or-nothing per clip.

## Problems to know about

- One automated run of about sixty ended with a GPU error I could not
  reproduce in twenty repeats. The cause is unknown.
- Three independent reviews found real bugs, now fixed (among them:
  "restore older state" failed in any project without a scenario; split
  and overwrite put ramped or sped-up clips out of place). What they found
  and I left is listed at the end of `docs/spec/08-milestones.md`; the one
  you will most likely meet: cutting to a camera angle replaces the
  programme to the end of that take, later cuts included.

## Not verified, needs you

Nothing was heard or watched by a person: how scrubbing, the mixer and
noise reduction sound, how transitions and looks appear in motion, every
real mouse gesture, real multi-camera footage, the file picker, the
clipboard, "Show in folder".
