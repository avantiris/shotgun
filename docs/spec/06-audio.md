# 06 Audio engine

Stage 1 needs: clip audio playback, per-track gain/pan/mute/solo, fades, a
master bus, metering, and A/V sync. The engine is nevertheless built as the
core of the future DAW, because retrofitting realtime constraints is a rewrite.

## Backend

- Decided by spike S3 (`docs/spikes/s3-audio.md`): the `pipewire` crate 0.10
  with a `pw_stream` (`AUTOCONNECT | MAP_BUFFERS | RT_PROCESS`, F32LE, 48 kHz).
  Not cpal: its PipeWire host reports two quanta of delay instead of one and
  exposes no sample position, quantum or xrun information.
  The app appears as a normal node; no user configuration. Realtime priority
  comes from PipeWire's RT module through RTKit (verified: SCHED_RR in every
  run); the only system requirement is a running rtkit daemon.
  Direct ALSA is a later option behind the same backend trait. The system has
  no JACK library installed; JACK interop is not needed for stage 1.
- Default quantum **256 frames** at 48 kHz (5.3 ms reported output delay),
  configurable 64-1024. Reason: during editing the output delay is compensated
  for video sync, so lower buys nothing, and 256 was the only size with zero
  lost cycles under a parallel build (64 and 128 each lost one; 64/128/256 were
  all clean under pure CPU load). Recording in the DAW stage switches to
  64 or 128 (1.3 / 2.7 ms out, ~6 ms reported round trip at 128). The spike
  ran on a loaded machine and saw unexplained 0.3-1 s graph stalls in a few
  runs; M3 must re-measure xruns on a quiet machine before this is final.
- `pw_filter` (needed for multi-port DAW routing) performs the same but has no
  safe wrapper in the crate and is not auto-linked; it is the DAW-stage path.
- It **measures** rather than assumes: reports actual quantum, sample rate,
  device latency from PipeWire, xrun count and worst-case `process()` time,
  visible in the performance overlay. A loopback round-trip test tool is
  planned for the DAW stage.

## Realtime rules

In `process()`: no allocation, no locks, no syscalls, no logging, no unbounded
loops. Parameter changes arrive through lock-free queues; graph changes are
compiled on another thread and swapped atomically at a block boundary.
Debug builds assert on allocation in the realtime thread.

## Graph

```
clip sources -> track (insert chain, gain, pan) -> buses -> master (insert chain) -> output
```

- **Insert chain** on every track and bus from day one. Built-in processors
  (gain, EQ, compressor, limiter later) implement the `Processor` trait that
  hosted plugins will implement too:
  `prepare(sample_rate, max_block)`, `process(audio, events, transport)`,
  `latency()`, `tail()`, parameter and state access.
- Plugin delay compensation is part of the graph compiler from the start.
- Sidechain inputs are part of the `Processor` port model from the start.
- Event (MIDI/note) ports exist in the trait but are unused until the DAW stage.
- Processing in 32-bit float, 48 kHz internal rate by default.

## Sources

Clip audio is read from the conformed 48 kHz float files by a disk thread into
per-clip ring buffers ahead of the playhead. The realtime thread only copies.
Speed changes use a high-quality time-stretch/resample stage in the source.

## Transport and sync

- The audio engine's sample counter `N` is the master clock. The viewer presents
  the video frame whose time matches the sample position being heard (output
  latency compensated). From S3: in each callback `Stream::time()` gives `now`
  (CLOCK_MONOTONIC ns), `delay`, `buffered` and `rate`; the frame heard at time
  `T` is `N + (T - now) * rate - (delay + buffered)`. Measured drift of this
  extrapolation: under 0.25 ms over 30 s. Quantum and xrun flags come from
  `spa_io_position.clock` via `io_changed`.
- Scrubbing plays short audio grains so cuts can be found by ear.
- Seek/loop are sample-accurate.

## Later (DAW stage, not built now)

- Recording with input monitoring and latency compensation.
- Piano roll / MIDI tracks, instruments.
- Plugin hosting: CLAP and VST3 (native Linux builds), LV2; Windows-only VSTs
  via a bridge such as yabridge. Plugins run with crash isolation where possible.
- Mixing/mastering tools, loudness (LUFS) metering and targets.

## As built in M3 (`sg-audio`)

- Backends behind one engine: PipeWire stream (default), `null` (a thread
  that keeps time and discards the sound; `SHOTGUN_AUDIO=null`) and `manual`
  (the caller pulls blocks; tests). `SHOTGUN_AUDIO=sink:<node name>` targets a
  specific sink.
- The stream is active only while playing, so the desktop's audio graph runs
  at our quantum only then and an idle app costs no callbacks.
- The disk thread keeps one ring buffer **per track** (half a second ahead),
  not per clip as written above: it lays the track's clips into the stream
  itself. The realtime thread runs each track's insert chain (today: gain and
  pan) and sums. Clip gain is applied by the disk thread.
- A seek, a start or an edit builds a new graph with fresh buffers on the disk
  thread and hands it over through one atomic slot; the callback adopts it at
  a block boundary and parks the old one for the disk thread to free.
- Clock: the callback publishes (position, monotonic time, delay) behind a
  sequence lock; `Engine::position()` extrapolates from it.
- Not built yet: plugin delay compensation,
  the user-adjustable A/V offset, sidechain and event ports (the `Processor`
  trait has only audio in/out so far), allocation asserts in debug builds.

## As built after feedback round 3 (mixer)

- Every audio track has mixer settings in the project (`TrackSettings`:
  fader in dB, pan, mute, solo, low cut, noise reduction) and the sequence
  has a master fader; each change is an operation with an inverse
  (`SetTrackMix`, `SetMasterGain`) and is written to the `.sg` file as a
  `mix {json}` line under the track and a `master <dB>` line.
- Fader, pan and mute (and solo, which mutes the others) act at once: the
  realtime thread reads them from atomics and spreads a change over one
  block. No new graph is built for them, so nothing is refilled.
- Low cut (fourth-order high-pass since 2026-11-21, see below) and noise reduction run on the disk
  thread, before the ring buffer, not in the realtime graph. `PROPOSED`
  reason: the noise reduction is RNNoise (crate `nnnoiseless`), a neural
  network made for speech with 20 ms of delay and an FFT that may allocate.
  On the disk thread its delay is hidden by reading the source 960 samples
  ahead, so tracks stay aligned without delay compensation in the graph. The
  price: a change of these two settings is heard after the buffered half
  second. Switching noise reduction on or off builds a new graph.
- Meters: the realtime thread keeps the peak per track (first 32 audio
  tracks) and of the mix in atomics; the UI reads and resets them each frame.
- Export renders sound through the same engine, so the mixer is in the file.
- Added later the same night: a three-band equaliser (shelf at 120 Hz, a
  bell with a movable centre, shelf at 8 kHz) and a compressor (threshold,
  ratio; 5 ms up, 120 ms down, both channels together) per track, on the
  disk thread after the noise reduction; and a limiter on the master in the
  realtime graph, always on, with its ceiling at full scale: a mix that
  stays below it passes untouched, one that would clip is turned down and
  the mixer says by how much (`PROPOSED`: always on, no switch).
- Not built: sends and buses, volume automation,
  loudness (LUFS) metering, a noise reduction suited to music, the
  `Processor` chain being editable by the user.

## Presets and clip volume (feedback round 4, 2026-11-19)

- Each strip of the mixer has a `Preset` button: Flat, Voice, Voice in a
  noisy room, Voice-over, Camera sound outdoors, Music, Music under a voice,
  Bass and kick, Bright and clear, Telephone. A preset sets low cut, noise
  reduction, equaliser and compressor and leaves fader, pan, mute and solo
  alone; it is one `SetTrackMix` operation, so one undo. The button names
  the preset the strip is at, or "Own" once a value was changed by hand.
  The values (`PROPOSED`, in `sg-ui/src/mixer.rs`) are common starting
  points, not measured on the user's material. The compressor has no
  make-up gain, so a preset with a low threshold makes the track quieter.
- The volume of a single clip (`ClipProps::gain_db`, there since M4) is
  shown as a line across each sound clip in the timeline and changed by
  pulling it (07-ui-keybinds.md). A clip with volume keyframes moves as a
  whole: every keyframe by the same amount.

## De-esser (2026-11-20, stage 3)

Per track, on the disk thread after the equaliser and before the
compressor (so that added air is tamed and the compressor does not pump on
every S). `PROPOSED` method: the sound above 4.5 kHz and the whole are
followed (1 ms up, 30 ms down); where the top is more than about a third
of the whole it is an S, and a high shelf at 4.5 kHz goes down by up to
12 dB times the amount (2 ms in, 60 ms out). A ratio, not a level: nothing
to aim, and the same on a quiet take as on a loud one.

## Pitch correction (2026-11-20, stage 3)

Per track, on the disk thread after the noise reduction and before the
equaliser. `PROPOSED` method, the classic time-domain one: every 256
frames the period of the newest 2048 frames is measured (likeness with
itself shifted, at a quarter of the rate, 70 to 400 Hz, refined between
samples); the note it makes is compared with the nearest note of the key
and the sound is read from a ring buffer faster or slower by that ratio;
when the reading point has drifted half a period from its place it jumps
one period with a crossfade. 1024 frames of constant delay, which the
feed reads ahead like the noise reduction's; switching the correction on
or off builds a new graph for that reason. What has no pitch passes
unchanged.

## Leveller, saturation, ducking (2026-11-20, stage 3)

- A leveller (the phrase's level, up within 5 ms and down within 80 ms,
  against the average of the sounding parts over 10 s; the difference
  taken back, at most 12 dB either way; the hand moves down within 20 ms
  and up within 300 ms) and, last, a saturation (`tanh`, unity gain for
  quiet sound).
- Changed 2026-11-21 (feedback F16.16, F16.18, F16.20, measured on a take
  of his with his settings: the loudest moments left the chain at -6.4 dB
  against -17 dB for nine tenths of the take, now -14.0 against -21.7):
  - The leveller is before the compressor, not after it.
  - It was 60 % of the difference against a 3 s average, at most 9 dB: a
    loud stretch of a few seconds became the average and passed.
  - Its hand used to follow the phrase both ways alike: the first
    syllable after a pause or a quiet phrase came out up to 5 dB too loud.
  - Both followers work in decibels and sank to the bottom of a pause, from
    where a word took 10 to 20 ms to be climbed up to; they now stay just
    below where they start to act.
  - The compressor comes up within 1 ms on a voice track (5 ms elsewhere,
    which lets a drum hit through): a voice is one short spike per cycle
    of its pitch, and at 5 ms the follower stayed far under them.
  - The low cut is fourth order (24 dB per octave), not second: mains hum
    at 50 Hz is 24 dB down with the cut at 100 Hz (it was 8 dB).
- Changed in the night of 2026-11-22 (feedback N2.2, on seven takes of
  his recorded through an on-board input, hum 11 to 15 dB under the voice):
  - Listening to a voice ("Voice" on a strip) now finds a floor of noise
    that is close under the voice (it was only looked for 35 dB below the
    loudest moment, so on these takes none was found and no noise
    reduction set), and tells hum (below 120 Hz) from hiss. Hum raises the
    low cut to 105 Hz; hiss sets the noise reduction (all of it from 20 dB
    under the voice) and holds back added air.
  - The proposal is then checked on the sound: the take (a minute of it)
    is run through the stages with the proposed settings and measured
    again, up to four times, and threshold, make-up and noise reduction
    are moved until the result is at the style's level, its loud phrases
    stand where the style has them and the pauses are quiet
    (`sg_play::fit_voice`). On his two long takes: -17.0 dB (aim -17),
    loud phrases 10.2 dB above the average (aim 12), pauses 68 and 71 dB
    under the voice (before: 15 and 11). About one second per take.
  - In that checking the noise reduction is stepped up only while a step
    makes the pauses clearer by 2 dB or more: on six finished vocals from
    records (whose "pauses" are the tail of a room, 30 to 38 dB under the
    voice) every one had ended at 100 % for nothing; they end at about
    half now, his noisy takes still at 100 %.
  - The compressor has a make-up gain (0 to 24 dB, `comp_makeup_db`),
    which the set-up uses instead of pushing the fader to its end.
  - The leveller can be given the level it levels to (`level_ref_db`
    in a track's settings; 0: it finds it while playing, from the first
    phrase on). "Voice" sets it: the average the leveller itself finds
    over the whole take behind the stages that come before it. With it
    the track sounds the same wherever playback starts, and a voice that
    gets louder over a take (he moved towards the microphone, feedback
    F16.15) is brought back to the take's level; without it a loud stretch
    that playback starts in is taken for the track's level. It is not
    measured again when stages before the leveller are changed by hand:
    "Voice" again does that.
  - The leveller pushes up in full only what is within 10 dB of the
    average, less down to 20 dB below it and nothing further down: breath
    and room noise came out up to 12 dB louder.
- The stages run in the order the track has them in (`order` in its mix
  settings, each of the eight once; feedback F16.22). The plugins, fader
  and sends stay after them.
- For the equaliser's and the low cut's picture the disk thread also takes
  the spectrum going into and out of that stage: 2048 samples every 1024,
  64 bands from 20 Hz to 20 kHz, kept for 5 s by timeline position
  (`PROPOSED`).
- The level after every stage of one track (the one whose chain is on
  screen) is kept per 5 ms of the timeline for 11 s (`sg-audio/scope.rs`),
  written by the disk thread as it lays the sound down and read by the UI
  around the position that is heard (`PROPOSED`).
- Ducking is done on the disk thread, not with a sidechain input in the
  realtime graph (`PROPOSED`): voice feeds are laid down first and leave
  their level along the timeline (peak per 64 frames, fader included, a
  muted voice none); a feed that ducks is filled only as far as the voices
  have been and follows that level. The price is the same as for the other
  disk-thread effects: a change is heard after the buffered half second.

## Scrub grains (added 2026-11-19)

- A move of the playhead while paused lets a tenth of a second of the mix at
  the new position be heard, faded over 5 ms at both ends. The engine gets a
  graph marked as a grain: the realtime thread plays that many frames and
  goes quiet by itself; the clock stays parked, so nothing "plays".
- Grains follow each other no faster than they last (105 ms), so none is cut
  off; the device stream is kept awake for 0.4 s after the last one.
- Checked on a null sink with the PipeWire backend: three seeks a second
  apart gave three sounds of 100 ms, three seeks in quick succession one
  unbroken sound of 0.33 s.
- `PROPOSED`: every jump while paused sounds (dragging, stepping, clicking
  in the ruler, Home/End), there is no switch for it, and the grain plays
  forwards at normal speed whatever the direction of the scrub. Not heard by
  a person yet.
