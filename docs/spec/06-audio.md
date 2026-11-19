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
- Low cut (second-order high-pass) and noise reduction run on the disk
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
