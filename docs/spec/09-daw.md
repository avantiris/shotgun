# 09 Music production (stage 3)

Stage 3 of Shotgun. What the user decided is written plainly; everything
marked `PROPOSED` is my design and the first thing to change on feedback.

## Decided with the user (2026-11-20)

Asked when stage 3 began; his answers, in his words where he wrote any:

1. **What first:** "Both about equally": writing notes for instruments and
   recording audio, a small version of each, then both deeper.
2. **Sound sources and effects:** all of: built-in ones to start with,
   native Linux plugins (CLAP, VST3, LV2), Windows VSTs through a bridge,
   and, added by him: "Native instrument integration that this works out
   of the box".
   - Read as: the Native Instruments products (Kontakt and the like) must
     load without the user setting anything up. They exist for Windows
     only, so this is the Windows-VST path made automatic. `OPEN`: to be
     confirmed with him that this is what he meant, and which products.
3. **Hardware:** "Not decided yet". So: everything works with mouse and
   computer keyboard; a MIDI keyboard and an audio interface can be added
   later without a redesign.
4. **Where music lives:** in the same timeline as video and audio. The
   ruler can show bars and beats with a tempo; one project can be a video
   with its own music.

Carried over from 01-product.md (agreed earlier): note editor, audio
tracks, recording, mixing and mastering, effect chains, plugin hosting,
sidechaining; one realtime engine; PipeWire native; low measured latency;
sample-accurate transport.

## Design (`PROPOSED`)

### Time

- A sequence gets a **tempo**: beats per minute and beats per bar. With a
  tempo the ruler shows bars and beats, and moving, trimming and drawing
  snap to the grid (bar, beat, 1/2 to 1/32, triplets).
- The model stays in integers. Note positions and lengths are in **ticks**,
  960 to the beat; audio stays in samples, video in frames. The tempo
  converts between them at the edges (drawing, engine), never in storage.
- One tempo per sequence first. Tempo changes along the song are a later
  step (a list of changes; the conversion functions already take a
  position).
- The beat grid of stage 2 (a song detected from a recording) and the tempo
  are two sources for the same grid; a sequence uses one of them.

### Tracks and clips

- A third kind of track next to video and audio: **instrument**. It holds
  note clips, has an instrument (built-in or a plugin) and the same mixer
  strip as an audio track.
- A **note clip** has a start and length in ticks and a list of notes
  (start, length, pitch, velocity) relative to its start. Notes are edited
  through operations with inverses like everything else; a note clip is
  stored in the sequence file as text.
- Audio tracks stay as they are. **Recording** adds a new audio asset to the
  project (in the bundle, like any import) and a clip on the armed track.

### Editing notes

- A double click on a note clip opens the **piano roll** in the place of
  the viewer (a music project has no picture to show; a video project
  keeps the viewer and shows the piano roll over the timeline's upper
  half). Draw, move, stretch, delete, velocity lane, the grid above.
- The computer keyboard plays notes while the piano roll has the focus
  (two rows as a piano), so that nothing has to be drawn blind.

### Sound

- Built-in first: a **sampler** (one sample or a multi-sample set per
  instrument, SoundFont 2 files read directly), a **drum kit** (one sample
  per pad), a small **subtractive synth**. They implement the `Processor`
  trait the effects already use, now with its event port.
- The realtime graph of M3 gets instrument nodes, note events with sample
  offsets, and per-track insert chains the user can edit (the equaliser,
  compressor and noise reduction of the mixer become movable inserts).
- Buses and sends; sidechain input on the compressor.

### Plugins

Hosting in this order, each behind the same `Processor` trait:

1. **CLAP** (open, simplest to host correctly).
2. **LV2** and **VST3** native Linux builds.
3. **Windows VST2/VST3** through yabridge and Wine, set up by Shotgun
   itself: it finds the plugins, runs the bridge and keeps its own Wine
   prefix, so that "out of the box" holds. Needs a spike before anything
   is promised: copy protection of commercial plugins (Native Access)
   under Wine is the known risk.

A plugin runs in its own process where the format allows it, so that a
crashing plugin costs one track's sound and not the project. Plugin state
is saved in the bundle.

### Recording

- Arm a track, count-in, record; takes are kept (a new take does not
  delete the last). Latency of the input path is measured and the take is
  placed so that it lines up with what was heard.
- Input monitoring through the engine with the measured round trip shown.
- Needs `pw_filter` ports (06-audio.md) and realtime scheduling.

### Mastering and output

- Loudness metering (LUFS, true peak) on the master; the limiter that
  exists gets a ceiling and a target.
- Export of the sound alone: WAV, FLAC, MP3, and stems per track or bus.

## Budgets (`PROPOSED`, to be added to 04-performance.md when measured)

- Key or pad to sound: 10 ms or less at a 256-sample quantum.
- No xruns with 32 instrument tracks of built-in instruments playing.
- Opening the piano roll and drawing a note: within the UI frame budget.
- A project with 100 000 notes opens and scrolls like one with 100.

## Milestones (`PROPOSED`)

| | What works at the end |
|---|---|
| M11 | Tempo and bars: a sequence has a tempo; the ruler shows bars and beats; clips snap to the grid; a metronome clicks while playing. |
| M12 | Notes: instrument tracks, note clips, the piano roll, the built-in sampler and drum kit; the computer keyboard plays; notes are in the export. |
| M13 | Recording: arm, count-in, record, takes; latency measured and compensated; monitoring. |
| M14 | Mixing: editable insert chains, buses and sends, sidechain, the synth, loudness metering, sound-only export and stems. |
| M15 | Plugins, native: CLAP, then LV2 and VST3; state in the bundle; a plugin crash does not take the project down. |
| M16 | Plugins, Windows: the bridge set up by Shotgun; the Native Instruments question answered by a spike first. |

After M12 and M13 ("a small version of each") the stage is ready for a
first test round.

## Open

- `OPEN` What "Native instrument integration" means exactly (see above).
- `OPEN` Which plugins he owns or wants first: decides what the spikes for
  M15 and M16 test against.
- `OPEN` Whether a music project should hide the video parts of the screen.
