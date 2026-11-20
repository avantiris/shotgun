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
   - Confirmed by him the same day: "yes use the native instrument
     kontakt one... there is a repo from selimbucher which already does a
     lot of this as sample." That is
     `github.com/selimbucher/native-instruments` (GPL-3.0): Native Access 2
     and Kontakt 8 under Wine with a one-command installer (`ni`,
     `native-access`), and a yabridge built against the same Wine. Both are
     already installed on his machine (see "Kontakt" below).
3. **Hardware:** "Not decided yet". So: everything works with mouse and
   computer keyboard; a MIDI keyboard and an audio interface can be added
   later without a redesign.
4. **Where music lives:** in the same timeline as video and audio. The
   ruler can show bars and beats with a tempo; one project can be a video
   with its own music.

5. **A built-in vocal chain** (2026-11-20, his words): "besides vst
   plugins I can load, a BUILTIN vocalchain that is pretty basic but
   extremly good at detecting and automatically applying a style (main
   concern right now is german rap texts) this should also already provide
   sidechaining etc."
   - So: one built-in thing per vocal track, few controls, that listens to
     the recording and sets itself up for a style; German rap first; the
     beat ducking under the voice is part of it.

6. **Pitch correction and de-esser** (2026-11-20, his words): "I also
   want an automatic autotuner / suggestor and a deesser that finds the S
   sounds and a compressor etc".
   - So: pitch correction that proposes key and scale by itself and can
     be left to it ("automatic") or steered; a de-esser that finds the S
     sounds itself instead of being aimed by hand; the compressor.
   - State when he asked: one compressor per track and a limiter on the
     master existed; no de-esser, no pitch correction.

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

### Vocal chain (`PROPOSED`; his main concern, so it comes early)

One built-in processor on an audio track, "Vocal chain", with a style and
one amount control per stage; everything else is set by listening.

- **Stages, in order:** level into the chain; low cut; noise reduction;
  de-esser; equaliser (takes out mud and boxiness, adds presence and air);
  a fast compressor for peaks, then a slow one that levels; saturation;
  pitch correction (off, natural, hard: the "autotune" of modern rap, with
  key and scale); sends to a short delay and a plate reverb; limiter.
  **Ducking:** the tracks chosen as "the beat" are turned down a little
  while the voice sounds (sidechain), without a routing to set up.
- **Listening:** the takes on the track are measured once (and again when
  they change): loudness and peaks, noise floor between phrases, the
  voice's pitch range, its tonal balance against the style's target curve,
  how sharp the S sounds are, how much the level swings. From that the
  stages get their values: where the low cut sits, how much noise
  reduction, the equaliser's gains, the de-esser's band and threshold, the
  compressors' thresholds for the gain reduction the style wants.
- **Styles:** a style is a target (tonal curve, gain reduction, brightness,
  effects, ducking depth), not a list of fixed numbers. First: German rap
  lead (close, bright, hard-levelled, dry), doubles and ad-libs (thinner,
  wider, further back), sung hook with pitch correction. Which one fits is
  guessed from the take (rapped or sung: how steady the pitch is; how
  hard: level and peaks) and can be overridden.
- **Honest limit:** "extremely good" can only be judged by ear against
  records he likes. `OPEN`: two or three reference songs and a raw vocal
  recording of his to tune the styles against.

### Plugins

Kontakt 8 is a VST3 plugin, so VST3 hosting comes first:

1. **VST3** (native Linux builds, and Windows ones through yabridge, which
   presents them as native VST3).
2. **CLAP**.
3. **LV2**.

A plugin runs in its own process where the format allows it, so that a
crashing plugin costs one track's sound and not the project. Plugin state
is saved in the bundle. A spike comes before the build: loading a
yabridge-bridged VST3 from Rust, its editor window under Wayland, and
state save and restore.

### Kontakt (Native Instruments)

Found on his machine on 2026-11-20: `ni`, `native-access`, `yabridgectl`,
`yabridge-host.exe` and Wine 11.21 are installed (from the repository
above). No Wine prefix exists yet, and nothing is bridged.

- "Out of the box" then means: Shotgun looks for the prefix (`~/.wine-ni`
  or `NI_WINE_PREFIX`), runs `yabridgectl add` on its VST3 folder and
  `yabridgectl sync` by itself (job pool, output shown), and lists Kontakt
  among the instruments. Nothing to configure in Shotgun.
- What only he can do: start `native-access` once, log in to his account
  and install Kontakt. The home disk has 3.5 GB free and the prefix
  defaults to `~/.wine-ni`: it should be put on the large disk first
  (`NI_WINE_PREFIX=/disk/...`), or Kontakt's libraries will not fit.
- Shotgun does not ship or install any Native Instruments software.

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
| M12 | Vocal chain, first version: on a track of recorded or imported voice it measures the takes and sets itself up for a style (German rap lead first); a de-esser that finds the S sounds, two compressors, saturation, ducking of the beat; pitch correction that proposes key and scale; in the export. |
| M13 | Recording: arm, count-in, record, takes; latency measured and compensated; monitoring. |
| M14 | Plugins: VST3 hosting, Kontakt through the bridge found and set up by Shotgun; then CLAP and LV2. Instrument tracks, note clips and the piano roll come with it, with a built-in sampler so that notes sound without any plugin. |
| M15 | Vocal chain, second version: delay and reverb sends, more styles, pitch correction by hand per note, tuned against his references. |
| M16 | Mixing and mastering: editable insert chains, buses and sends, loudness metering, sound-only export and stems. |

Order changed on 2026-11-20 after his fifth answer: the voice first
(M12, M13), then the beat (M14).

After M12 the vocal chain can be tried on any recorded voice file; after
M13 on his own recordings; after M14 with a beat from Kontakt.

## Open

- `OPEN` Reference songs and a raw vocal take for tuning the vocal chain.
- `OPEN` Which plugins he owns or wants first: decides what the spikes for
  M15 and M16 test against.
- `OPEN` Whether a music project should hide the video parts of the screen.
