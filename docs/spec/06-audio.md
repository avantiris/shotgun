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
