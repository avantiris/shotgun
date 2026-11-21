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
  position in the timeline, insert edits (overwrite was added 2026-11-19: a move with Ctrl held goes over what is in the way, `PROPOSED`; without Ctrl a move onto occupied
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
  (title fonts were added 2026-11-19: a .ttf or .otf file dropped onto the
  window goes to `assets/font/` and is chosen under Font in the title's
  settings; preview and export read the same file), a keyframe lane in the timeline
  (since 2026-11-19 a clip shows a mark at each of its keyframes; they
  cannot be dragged there).
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
  exports (a "Show in folder" button beside the news of a finished export
  was added 2026-11-19; not tried, since it opens a window on the desktop).
  Export reads the constant-rate
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
  is exact at its middle), choosing the first beat of the bar, a
  tempo-changing grid editor. Added later the same night: a take the
  automatic match cannot place is laid by hand ("Camera angle at the
  playhead, synced by hand" in the asset menu): picture on a camera track,
  sound on a muted track below, moved together by dragging or the arrow
  keys until the waveforms line up (`PROPOSED`). **Not verified with real footage**: every
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
  The picture follows the ramp and, since later the same night, so does the
  sound of the same take (resampled along the curve, so its pitch moves
  with the speed; the waveform drawn on the clip does not show it). The
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
  depth.


- **GPU error at start: found and fixed (2026-11-19, after round 4).** One automated run out of
  about sixty that night ended with a GPU error from wgpu (the program
  stopped; nothing was lost, the journal had every edit). The run opened a
  project, selected clips, took an off-screen picture and started a master
  export. The message was not kept, and twenty repeats of the same run
  were clean. Since then a GPU error is written to the log and the program
  goes on (`sg_render::report_gpu_errors`); an export during which one is
  reported fails rather than keeping a file with a wrong frame. If it
  shows again the log has the cause.
  It showed again and the log had it: "Failed to wait for GPU to come idle
  before reconfiguring the Surface". wgpu wants the queue empty when a
  surface is configured and gives up if another thread submits work during
  its wait; here the compositor resizes the new window while the player
  thread makes its first pictures. The window layer now catches that
  refusal and tries again (`gfx.rs`, `configure`). Thirty repeats of a run
  that had failed one time in four: the race was met twice, retried, and
  no error reached the log.

- **Slow export with one asset on two layers: found and fixed
  (2026-11-20).** A project where a clip lay over a copy of itself
  exported at 0.44x realtime (decode 33.8 ms a frame). Not the source's
  frame rate, as first suspected: the export kept one decoder per asset,
  and two layers showing the same asset at different places sent it back
  and forth, a seek for every frame. Now a decoder (and a picture) per
  asset and per layer showing it at once; the player's decode workers keep
  up to two per asset likewise. The same ten seconds: 3.01x realtime,
  decode 1.3 ms a frame, the file byte for byte the same.

- **Reviewed 2026-11-19.** Three independent reads of the night's code
  found the defects fixed in commits `2cd9e73`, `0395eae` and the one after.
  Left as they are, known:
  - A cut to a camera angle does not carry a speed or stabilise setting of
    the angle's clip. (Changed after round 4: the cut now reaches to the
    next cut of the programme, not to the end of the take, so later cuts
    stay.)
  - A share copy aims at its size limit rather than staying under a
    sensible bitrate: a short clip is made as large as the limit allows.
  - Real 10-bit frames are used when a scenario grades, not for per-clip
    exposure/warmth alone and not for HDR sources.
  - During a whip or zoom a picture smaller than the frame (bars, picture
    in picture) fills the frame with mirrored copies.
  - "Sync all takes" analyses all takes at once (one thread each), and the
    analyses and the stabiliser cannot be cancelled from their job row.
  - "Cuts to the beat" assumes programme clips at normal speed.
  - Head trims of a clip with a speed ramp move its source linearly.
  - The export check by the graphics card may accept a damaged stream the
    processor would reject (see 01, open points).
  - The sync likeness rule may refuse a correct match on takes longer than
    about 19 minutes.
## Stage 3: DAW

Started 2026-11-20. What the user decided, the design and the milestones
M11 to M16 are in `09-daw.md`. In short: M11 tempo and bars, M12 vocal
chain, M13 recording, M14 plugins (VST3, Kontakt) and notes, M15 vocal
chain second version, M16 mixing and mastering.

- [ ] M11 Tempo and bars
  - Built 2026-11-20: a sequence can have a tempo (`Tempo`: thousandths of
    a beat per minute, beats per bar; `Op::SetTempo`, a `tempo` line in the
    `.sg` file). Set under SEQUENCE in the inspector ("Bars and beats").
    With one, the ruler counts bars, the lanes show beats and bars, and
    moving and trimming snap to quarter beats, beats or bars, whichever
    is far enough apart on screen to aim at.
  - Loop (2026-11-21): L or "Loop" above the timeline plays a stretch
    over and over: the note clip whose piano roll is open, else the
    selected clips, else what lies between the markers around the
    playhead (the stretch is fixed when the loop is switched on and
    shown as a bar along the ruler). The engine goes back to the start
    to the sample (`Engine::set_loop`: no block is read across the end,
    and the clock goes round with the sound); notes held across the end
    are let go there. Playback started after the stretch plays on; a
    recording is not looped under. Checked: an engine test (a recording
    that says where it is: every round begins on the right sample), and
    in the app through PipeWire into a null sink: a note at the start of
    a two-bar clip came 192 000 samples apart five times running, with
    0 underruns. With a picture on the timeline (1080p60, a two-second
    stretch between markers, four rounds): 2 of 538 frames late.
  - Metronome (2026-11-20): Shift+K or "Metronome" above the timeline;
    clicks on the beats while playing (the first of a bar higher), placed
    by whole-number arithmetic so that they do not drift; never in an
    export.
  - Under the viewer a sequence with a tempo shows bar, beat and tempo
    before the time.
  - At the timeline (2026-11-21): above it, after the tool buttons, "Set
    tempo", or the tempo and the beats per bar once there is one, and
    "Grid": auto (as before, by zoom), bar, 1/2 to 1/32, and 1/8, 1/16 and
    1/32 as triplets. A chosen grid is what edges snap to at every zoom
    and what is drawn while its lines have room. The grid is a setting
    of the view, not of the project (`PROPOSED`). The piano roll offers
    1/8 and 1/16 triplets too.
- [ ] M12 Vocal chain, first version
  - Built 2026-11-20, the listening half: "Voice" on a mixer strip
    measures what the track's clips play (`sg_media::voice`: level, loud
    phrases, noise between phrases, pitch and how still it holds, six
    tonal bands) and sets the strip that exists (fader, low cut, noise
    reduction, three-band equaliser, compressor) for a style: rap lead,
    doubles and ad-libs, sung hook, guessed from the take or chosen. One
    undo step; the bar at the bottom says what was heard.
  - Checked: unit tests on made-up voices (a low dull noisy speaking
    voice, a clean bright sung one), a headless flow, and a run in the app
    on `media/f3/noisy.wav`. Not checked: any real voice, and nothing by ear.
  - Known limits: a recording without pauses gives no noise measurement,
    so noise reduction stays off; the targets of the styles are common
    practice, not tuned against records.
  - Built 2026-11-20: a de-esser on every audio track (`deess` in the
    track's settings, "De-esser" on the strip). It finds the S sounds
    itself: the moments at which more than a third of the sound lies above
    4.5 kHz, whatever the level, and turns only the top down, by up to
    12 dB. Listening measures how loud the S sounds are against the voice
    and sets it. Measured in an exported file (a vowel with bursts of
    noise): the bursts 5.7 dB quieter (their part above 4.5 kHz by 12),
    the vowel unchanged to a hundredth of a decibel.
  - Built 2026-11-20: the styles' targets set from six measured German
    rap vocals (09-daw.md, "Reference measurements").
  - Built 2026-11-20: pitch correction (`sg_audio::tune`, "Tune", key,
    scale and "Tune glide" on the strip). The voice's pitch is followed
    and moved to the nearest note of the key, at once (hard) or gently.
    Listening proposes the key from the notes that are held, switches the
    correction on for a sung hook and leaves a rap verse alone.
    Measured through the app into an exported file: a note sung at 225 Hz
    comes out at 220.1 Hz (the A it was flat of), and starts on the same
    sample as without correction (its 1024 frames of delay are read
    ahead). In tests: notes outside a key go to the nearest one inside,
    half the amount moves half the way, noise passes sample for sample.
    Keys heard in the two sung references: D minor and E major (not
    checked against the songs' real keys).
  - Limits of the pitch correction: one voice at a time (no chords), 70
    to 400 Hz, the voice's colour moves with its pitch (no formant
    correction), nothing by ear yet.
  - Built 2026-11-20: a leveller (the second compressor: phrases are
    pulled towards the track's own average, no threshold), saturation, and
    ducking: a track marked as voice makes every track with "Duck under
    voice" go down while it sounds (down within about 20 ms, back over
    300 ms). Listening marks the track as voice, sets leveller and
    saturation for the style and lets the other sounding tracks duck 3 dB,
    all in one undo step. On the strip the voice stages show on voice
    tracks only and the ducking on the others; "Voice" sits at the top.
  - Checked: the leveller brings 14 dB between two phrases to about 6 and
    leaves pauses alone; a beat under a voice goes down by the set 6 dB to
    within a hundredth and comes back; in the app a real rap vocal over a
    music clip was listened to, set up and exported. Not checked: anything
    by ear.
  - What the stages do (2026-11-21): under the de-esser, the compressor
    and the ducking field of a strip, while it plays, the decibels that
    stage takes off right now. The stages work half a second before
    their sound is heard, so each reading is kept with its place on the
    timeline and shown when that place is heard
    (`sg_audio::Meters::stages`). Checked by an engine test (nothing
    shown during a quiet second although the loud one after it is
    already worked out; 10.5 dB shown in the loud one, as threshold and
    ratio give). Not shown yet: noise reduction, the leveller and the
    pitch correction.
  - Listening again (2026-11-21): a strip that was set up by listening
    listens again by itself when other recordings are on its track than
    the ones it was set up from (a new take; cutting, moving or trimming
    the same recording does not count), as long as its settings are
    still the ones the listening gave. If they were changed by hand
    since, it does not touch them and offers "Listen again" on the strip
    instead. `PROPOSED`: that rule, and that the result is an undo step
    of its own. Which strips were set up by listening is remembered for
    the session, not in the project.
- [ ] M13 Recording
  - Built 2026-11-20: Shift+R or "Record sound" above the timeline plays
    from the playhead and records the default input (`SHOTGUN_AUDIO_IN`
    names another) until it is pressed again or playback is paused. The
    take is written by a thread of its own, imported into the project as
    an asset and laid, to the sample, where it was played: on the lowest
    audio track that is free there, or on a new one. Playback runs on past
    the end of the sequence while recording.
  - Where a take belongs: its first frame gets the position that was being
    heard when it arrived, less its buffer and the input's reported delay
    (`sg_audio::record`).
  - Measured on a loop without air (played into a null sink, recorded
    from that sink's monitor): the recorded sound lies 4 to 5 frames
    (0.1 ms) from where it was played, in the engine alone and through
    the whole application.
  - Not checked: a real microphone and real speakers (the measured loop
    has no converters and no air in it; a device that reports its delay
    wrongly would shift takes by that error), anything by ear.
  - Count-in (2026-11-20): with the metronome on, a recording starts one
    bar before the playhead, with clicks. Checked on the same loop: the
    recorded clicks lie 5 frames from their beats, also before the start
    of the timeline.
  - Input (2026-11-21): "Input" beside "Record sound" lists what PipeWire
    offers to record from (asked anew each time it is opened) and the
    system's default; the choice is kept in `settings.toml` in the
    library, as it belongs to the machine and not to a project
    (`PROPOSED`).
  - Level and monitoring (2026-11-21): "Level" beside the input opens it
    without recording and shows how loud it arrives (-60 dB to full
    scale, yellow from -9, red from -1); "Hear" adds it to what is heard,
    playing or not. Hearing is a button of its own and off at first
    (`PROPOSED`): through loudspeakers it feeds back. The input goes
    from its capture stream through a ring straight into the playback
    callback (after the master, unprocessed), which skips what piles up
    beyond two blocks. Checked on two null sinks (one engine plays a
    burst into the first, a second monitors it into the other, the take
    of that shows the delay): heard 312 frames (6.5 ms) late, the meter
    at the burst's level, 0 xruns; and in the app the same way. Not
    checked: a real interface, whose converters add their own delay.
    Not built: the input through the track's own chain (it is heard dry).
  - Recording onto a track (2026-11-21): "R" in an audio track's header
    marks it as the one that takes recordings. A take then goes there,
    over what is in its place (clips it covers go, clips it reaches into
    are shortened, one it lies inside is cut in two), and begins at the
    playhead the recording was asked for at; what was recorded while
    counting in stays in the file in front of the clip's start, so it
    can be pulled out again. That is punching in: playhead before the
    bad line, record, stop after it; the track keeps its strip settings.
    Without a marked track a take still gets a free track. Which track
    is marked is a setting of the view, not of the project (`PROPOSED`).
  - Still to do: several takes of the same stretch kept on one track to
    choose between (now the newer one replaces the older, which only
    undo brings back); a punch-out point set beforehand.
- [ ] M14 Plugins (VST3, Kontakt through the bridge) and notes
  - Built 2026-11-20:
    - `sg-plug`: every plugin runs in a process of its own
      (`shotgun-plugin`), driven over a pipe: blocks with notes, reset,
      state, its window (spike S7). VST3 only so far.
    - Model: a plugin per audio track and the notes of note clips, as
      operations with inverses (`SetTrackPlugin`, `SetClipNotes`), in the
      sequence file. `PROPOSED`, differing from the first design: no third
      kind of track. An instrument is an audio track with an instrument
      plugin; a note clip is a clip without an asset on an audio track.
    - Engine: plugins run on the disk thread (an instrument before the
      track's own stages, an effect after them), survive seeks, and have
      their state written into the bundle (`plugins/<track>.state`) when
      playback pauses, before an export and at the end. A plugin that does
      not load or dies is reported and its track goes on without it.
    - A built-in instrument (a small synthesiser, not the sampler first
      planned) plays the notes of a track that has no plugin.
    - UI: "Plugins" in the top bar (what was found; the Native Instruments
      set-up in three steps); "Add plugin", "Window" and "Off" on every
      mixer strip; "Add note clip here" in the timeline's menu; the piano
      roll in the viewer's place (click, draw, move, stretch, right click
      to remove, a grid of 1/4 to 1/32, Esc to close).
  - Checked with Surge XT (a native VST3 instrument and effect from the
    dev shell): notes start within a block of where they are written, the
    state round-trips, a killed plugin costs only its own sound, its
    window opens (on the hidden workspace) and closes; in the app a melody
    was written, exported with the built-in instrument and with Surge XT.
  - Not checked: Kontakt or any bridged Windows plugin (needs the user's
    login first), anything by ear, the piano roll with a real mouse.
  - CLAP (2026-11-20): `.clap` plugins are hosted by the same program
    behind the same requests (`clap-sys`; notes, state, the plugin's
    window, its timers and descriptors). Checked with Surge XT's CLAP
    build: the tests that pass for VST3 pass for it, and in the app a
    note clip played through it into a sound export.
  - Playing by hand (2026-11-21): the instrument of the open piano
    roll's track is played with the computer keyboard ("Keyboard" in the
    roll's head), with the mouse on the keys at its left, and a placed
    note is heard. "Record" writes what is played while the timeline
    plays into the clip; "Quantise" puts note starts on the grid; the
    selected note's velocity is a field in the head. In the engine the
    track is buffered four device blocks ahead instead of half a second
    (`TrackMix::by_hand`) and sounds while nothing plays. Checked: an
    engine test (a key is heard within 1200 frames, playing or not, and
    written notes still start where they belong), a headless flow, and
    in the app through PipeWire into a null sink: a key while stopped and
    one while playing were both in the recording of the sink, the second
    written into the clip at tick 1910 for a key pressed one second
    after the start (1920), 0 xruns and underruns. Not checked: how the
    delay feels under the fingers, and a slow plugin on the short buffer.
  - Several plugins on a track (2026-11-21): up to eight, in an order
    ("Up" moves one earlier, its name shows its window, "Off" takes it
    off). The sound runs through the effects in that order after the
    strip's own stages; an instrument among them plays the notes,
    wherever it stands in the list. `Op::SetTrackPlugins` sets the list
    as a whole (the older `SetTrackPlugin` is still read from journals);
    each plugin has an id, and its settings a file of its own
    (`plugins/<track>-<id>.state`). Checked: a flow, and in the app a
    note clip through Surge XT (CLAP, instrument) and Surge XT Effects
    (VST3) on one track into an export, with both state files written.
  - What a plugin is told (2026-11-21): the song's tempo and where on
    the timeline its next block starts (so that what it syncs to the
    tempo, arpeggios, delays, loops, falls on the song's bars; four-four
    time is assumed), and that it is playing. Of a VST3 plugin's buses
    only the first of each kind is used; the others (a sampler's separate
    outputs, a sidechain input) are switched off explicitly.
  - Along the clip (2026-11-21): Ctrl + wheel zooms the piano roll
    around the pointer, Shift + wheel moves along the clip, the wheel
    alone up and down the keys; a note's colour is the stronger the
    harder it is struck.
  - Note clips on the timeline (2026-11-21): a note clip keeps its notes
    when it is copied and pasted, dragged to another track or cut in two
    (they were lost in all three: notes are kept per track and per clip).
    Both halves of a cut clip have the notes, and a clip cut or trimmed
    at its start plays and shows them from that point on, as a sound
    clip plays its recording (before, trimming the start slid the notes
    along). `PROPOSED`: what a clip no longer reaches is kept, not
    deleted, so pulling the edge out again brings it back.
  - Several notes at once (2026-11-21): Ctrl+click adds a note to the
    selection, Ctrl+drag draws a box, Ctrl+A takes all. Selected notes
    move and stretch together, the delete key removes them, "Quantise"
    and the velocity field act on them. Ctrl+C and Ctrl+V put them
    again after themselves, by the whole beats they span (a bar repeats
    as a bar; again, and it repeats once more); Ctrl+X cuts.
  - Built-in sampler (2026-11-21): on a track with notes the strip says
    what plays them when no instrument plugin does, "Instrument": the
    synthesiser, or a recording of the project, played as recorded at
    one key (middle C unless changed) and higher or lower at the others
    (`sg_audio::sampler`: up to 16 voices, the first half minute of the
    recording held in memory, straight-line interpolation). "To its end"
    makes every note play the whole recording (drums, hits; set by
    itself for recordings under 1.5 s), otherwise the sound ends with
    the note. `Op::SetTrackSampler`, a `sampler` line in the `.sg` file.
    Checked: unit and engine tests on a recording that says where it is
    (as recorded at the root key, twice as fast an octave up), a flow,
    and in the app a 110 Hz note exported at 110 Hz from the root key
    and at 220 Hz from the key an octave above. Not built: several
    recordings on one track (a drum kit with a pad per key), loop
    points, and pitching without changing the length.
  - MIDI keyboards (2026-11-21): every MIDI source of the machine is
    listened to through the ALSA sequencer (`sg_audio::midi`; one
    plugged in later is taken up too) and plays what the computer
    keyboard plays, the instrument of the open piano roll, with the
    velocity of each key; "Record" writes it down with that velocity.
    Checked without hardware: `sg-bench midi <client> ...` stands in for
    a keyboard; in the app its two notes sounded and were written into
    the clip with their velocities and lengths. Not checked: a real
    keyboard. Not built: pedals, pitch bend, a choice of which keyboard.
  - Notes change while it plays without a new graph (2026-11-21): a
    note written, moved or removed (by hand or by "Record") no longer
    rebuilds the engine's graph, which cut every instrument off and left
    a gap; the tracks take the new notes as they are, and a note that
    was sounding and is gone is let go.
  - The thread that feeds the rings asks RTKit for a high priority
    (nice -11; not realtime, which would put file reads and plugin
    calls under RTKit's time limit). With that, and without a mistake of
    the same night (a new graph whenever the device's block size was
    first learnt), the run that had run dry two to nine times did not in
    five repeats. Without RTKit the thread keeps its ordinary priority
    and says so in the log.
  - Still to do: LV2 (not built: what exists as LV2 on Linux mostly exists
    as VST3 or CLAP too); plugin parameters as keyframes.
  - Slow and hung plugins (2026-11-21): a plugin starts on a thread of
    its own and joins the sound when it is ready (a sample library takes
    its time; the other tracks play meanwhile, and an export waits for
    it). One that does not answer a block within 4 s (3 minutes for
    loading and for its whole state, and for a block while its window is
    open) is ended like one that crashed: its track plays without it and
    the program says so. Checked with a stand-in host that loads and then
    never answers (`crates/sg-plug/tests/hang.rs`).
  - When the set-up goes wrong (2026-11-21): everything its steps print
    is kept in `native-instruments-setup.log` in the library (the panel
    shows the last lines and says where the file is), and "Check and
    repair" runs the tool's own diagnosis (`ni doctor --fix`: the
    prefix, what Native Access needs, the link the browser comes back
    through after the login). Checked against the stand-in tools only.
  - The bridge that comes with the program is the one used (2026-11-21):
    `yabridgectl` and a bridged plugin's loader both look for yabridge in
    the folders `NIX_PROFILES` names, which on this machine found the
    yabridge of the system profile (built against another Wine) and on a
    machine without one would find none. Both are now pointed at the
    bridge on the program's `PATH`. Checked: `yabridgectl sync` then
    names the loaders of the bundled bridge. Not checked: a bridged
    plugin making sound (docs/spikes/s7-plugin-host.md, "The bridge").
- [ ] M15 Vocal chain, second version
  - Built 2026-11-20: a reverb and an echo shared by all tracks, with a
    "Reverb" and an "Echo" amount on every mixer strip (after the fader).
    The echo repeats on the beat of the sequence's tempo (every 3/8 s
    without one). Both run in the realtime graph, so preview and export
    are the same sound. `PROPOSED`: one room and one echo with fixed
    character (about 1.8 s of tail; repeats 38 % of the one before,
    between 300 Hz and 4 kHz); nothing of them can be set yet.
  - Styles (2026-11-20): five now (rap lead, doubles, sung hook, melodic
    rap with hard pitch correction, ad-libs), and each sets its share of
    reverb and echo. `PROPOSED`: the two new ones and all send amounts
    are choices, not measured on references.
  - Tail (2026-11-21): a sound-only export goes on after the end of its
    range until what rings (room, echo, a note's release) has died away
    below -80 dB, 12 s at most; clips after the range are not played
    into it, and every track file is as long as the song. A film's sound
    still ends with its picture.
  - Settings (2026-11-21): on the master strip, how long the room rings
    (0.3 to 8 s; 1.5 s as before), the echo's time as a note value of
    the tempo (1/2, 1/4, dotted 1/8, 1/8, 1/16) and how much of each
    repeat is repeated (0 to 85 %). Kept with the sequence
    (`Op::SetSpace`, a `space` line in the `.sg` file); the room's length
    and the repeats change while it sounds, the echo's time takes a new
    graph. Checked: unit tests (a room set longer is louder a second
    later, an echo set to repeat nothing does not), a flow, and exports
    of the same note with the room at 0.4 s and at 5 s.
  - Still to do: pitch correction by hand per note.
- [ ] M16 Mixing and mastering
  - Built 2026-11-20:
    - Loudness in LUFS (`sg_audio::loudness`, ITU-R BS.1770): the last
      three seconds of the mix beside the master fader, and the whole
      song with every sound export.
    - "Sound only" in the export panel: WAV (24-bit), FLAC or MP3
      (320 kbit/s); "As mixed", or brought to -14 or -9 LUFS (the song is
      turned up or down by what is missing and mixed again; the limiter
      on the master holds the peaks); optionally every track as a file of
      its own in a folder beside the song, each as it sounds in the mix
      (its effects, its reverb and echo, ducked as in the mix).
    - `sg-bench export <project> sound[:wav|flac|mp3][:stems][:<LUFS>]`.
  - Checked: a full-scale 997 Hz tone reads 0.0 LUFS as the standard
    says; an export brought to -14 LUFS was measured by ffmpeg's own
    meter (`ebur128`) at -14.0 LUFS.
  - True peak (2026-11-21): a sound export with a loudness target is
    mastered through a limiter that looks at the wave between the
    samples (four points per sample, from twelve neighbours) and holds
    it under -1 dB, seeing 5 ms ahead so that the level comes down
    before a peak instead of at it. The loudness is then corrected up to
    four times, since what the limiter holds back is missing again.
    Playback, films and "As mixed" keep the plain limiter at full scale
    (`PROPOSED`): a finished song that is imported plays untouched.
    Checked with ffmpeg's meter on a reference vocal (-20.4 LUFS, peaks at
    full scale): to -14 it comes out at -14.1 LUFS, to -9 at -9.4 LUFS,
    both with a true peak of -1.2 dB (before: -10.4 LUFS and -0.9 dB).
  - Groups (2026-11-21): a strip can send its track through one of four
    groups (A to D) instead of straight to the master; a group that has
    tracks gets a level and a mute beside the master, and what its
    tracks send to room and echo follows it. `TrackSettings::group`,
    `Sequence::groups` with `Op::SetGroup`, a `group` line in the `.sg`
    file; summed and faded in the realtime graph, changed while playing
    without a new graph. Checked by an engine test and a flow. A group
    has no equaliser, compressor or meter of its own (`PROPOSED`: level
    and mute first).
  - Still to do: the strip's own stages in another order
    (plugins on a track can be reordered, M14); a
    loudness target far above the mix is reached only as far as the
    limiter allows (the result shows what was reached).