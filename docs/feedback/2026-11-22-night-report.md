# Night of 2026-11-22: report

Written for Linus to read in the morning. State: branch `main`,
`scripts/check.sh` passes, nothing uncommitted. Your library was read
(your takes and a copy of your project were measured) and not written to.

To get all of it: close Shotgun and start it again from a **new**
`nix develop` shell (`nix develop --command cargo run -p sg-app --release`).
A new shell matters this time: the program now brings two more tools
(`curl`, `sg-matte`), and a shell opened before tonight does not have them.

Nothing below was heard or looked at by a person. What "checked" means is
said for each point.

I stopped at about 03:30, not at 08:00. What is left on the roadmap
(pitch bend, plugin parameters over time, pitch correction by hand,
LV2, a GPU path for the cut-out) needs Kontakt running, a MIDI keyboard,
your graphics card under load or your ears; I did not want to start
Kontakt beside your open Shotgun or build more that nobody can check.

## First thing to do in your project

1. Press **Voice** again on your voice track (A3). The chain changed, so
   the settings from before no longer fit.
2. Then bring the **beat's fader (A2) from -29 dB to about -9 dB**. The
   voice now comes out at a normal level (-17 dB) instead of far too low,
   so with the beat at -29 it would be 24 dB under the voice. Shotgun says
   this in the notice at the bottom after "Voice"; it does not move the
   fader itself.
3. Your project folder `untitled-3` becomes `musikerrr` when you go back to
   the library.

## 1. Vocal chain and automatic settings (your point 2)

Measured on copies of your seven takes (`sg-bench voice <file>`).

**What was wrong with "Voice" on your takes**

- It saw **no noise at all**. Your takes have mains hum 11 to 15 dB under
  the voice. The listening only looked for noise 35 dB below the loudest
  moment, so the hum counted as voice and no noise reduction was set.
- Its sums for compressor and fader were made for the chain as it was a
  week ago, and nothing checked the result. The fader ended at +12 dB.

**What it does now**

- Finds a floor of noise close under the voice and tells hum (below
  120 Hz) from hiss. Hum raises the low cut to 105 Hz; hiss sets the
  noise reduction.
- Runs the take through the chain with the proposed settings, measures
  what comes out, and corrects threshold, make-up and noise reduction
  until level, evenness and quiet pauses are right. About two seconds.
- Tells you in the notice what no setting can repair: hum from the
  input, a voice too close to the noise, a take that clips.
- Clipping is also said where it happens: "clipped" stays beside the
  input level (Settings, "Level") until you click it away, and a take
  that reached full scale says so when it lands.

| Your take of 23:10 | Raw | After "Voice" (exported and measured) |
|---|---|---|
| Voice level | -21.5 dB | -17.0 dB |
| Loud phrases above the average | 10.9 dB | 10.2 dB |
| Between the phrases | -32 dB (hum) | -89 dB |

**Changes in the chain itself**

- The compressor has a **make-up gain** (it only turned down before).
- The leveller is given the take's own level by "Voice". It then sounds
  the same wherever playback starts, and a voice that gets louder over a
  take (you moving towards the microphone) is brought back.
- The leveller no longer pushes up breath and room noise far below the
  voice.
- Without "Voice", a leveller that starts in a pause no longer takes the
  room's noise for the track's level.

A second reader (an agent) went over the night's code; the faults it
found are fixed but for two small ones (all listed in
`2026-11-22-night-brief.md`).

## 2. The look (your point 3)

One set of rules for every control, in `crates/sg-ui/src/theme.rs`:

- Flat dark faces, one-pixel borders, nearly square corners (1.5 points).
- Pointing at a control lights its border; pressing fills it orange; what
  is switched on has a warm dark face. Nothing grows or is pill-shaped.
- Popovers and windows have the same border and a short shadow.

Checked on screenshots of library, editor, mixer, chain, export and clip
panel. Whether you like it is yours to say; colours and radius are four
numbers at the top of that file.

## 3. DAW basics that were missing (your point 4)

Each has a headless test; the ones marked * also ran in the real program.

- **MIDI files** *: drop a `.mid` on the window: one new track per track
  of the file, each with a note clip; the sequence takes the file's tempo
  if it has none. "MIDI" in the piano roll writes the open clip as a file.
- **Velocity lane** * under the piano roll: a bar per note; press or pull
  through the lane to set them.
- **Other tracks' notes** * show faintly in the piano roll, so the bass
  is seen while the melody is written.
- **The key of a beat** *: right-click the beat, "Find its key". Yours
  comes out as C minor. The piano roll then shows that scale and the
  pitch correction of voice tracks is set to it. I could not check the
  answer against a known key; tell me if it is wrong.
- **A scale in the piano roll** *: "Key" in the corner above the keys
  dims the rows that are not in the scale you choose.
- **Arrow keys on notes**: up/down a semitone (Shift: octave), left/right
  a sixteenth (Shift: a beat).
- **Takes are kept** *: what a new take covers goes onto a muted track
  beside it ("A3 takes") instead of being gone. Right-click a kept take,
  "Use this take on A3", to swap it back in.
- **Punching in** *: with a looped stretch on (L), a take replaces only
  that stretch, however early you start playing.
- **Another pitch or tempo** *: in an asset's menu; makes a copy of a beat
  in another key or at another tempo, each without the other changing.
- **Cuts in sound no longer click**: every clip's edge is eased over
  3 ms (not where a cut goes through one recording that plays on).
- **Tap tempo**: "Tap" beside the tempo.
- **Move track up / down** in a track's menu.
- **Sustain pedal** of a MIDI keyboard (not tried with a real pedal).
- **"Loudest moment to -1 dB"** in a clip's volume menu (normalise).

I told you yesterday that a volume per clip does not exist. That was
wrong: the line across a sound clip is its volume, and F sets volume
keyframes on it.

Still missing, and not built tonight: automation of a track's fader and
of plugin parameters over time; choosing between takes in one lane;
pitch bend; freezing a track.

## 4. AI on the picture (your point 5)

**Built: "Cut out the person (AI)"** in the menu of a video asset.

- It makes a new asset: the same video with everything but the person
  transparent. Put it on a track above a colour, a picture or another
  clip, and that is a changed background.
- For a clip that is already on the timeline: right-click it, "Show only
  the person (the cut-out)". It switches in place; its sound stays. Then
  put the new background on a track below it.
- "A copy out of focus" in the same menu makes a blurred copy of the
  video; with the cut-out on the track above it, the background looks out
  of focus (checked on the same clip).
- It runs **on your machine** (a model called Robust Video Matting, on
  the CPU). Nothing is uploaded. The model file (15 MB) is fetched once
  from its authors' page and checked before use.
- Checked from import to export on one clip (the meme clip from your
  `untitled-2`, copied): the person, hand and card are kept through heavy
  blur, over another clip in the viewer and in the exported file.
- It shows among the import jobs with its progress and can be stopped
  there.
- **Slow**: about 4 frames a second at 1080p, so roughly fifteen times
  the length of a 1080p60 clip. And it needs about 77 MB of cache per
  second of 1080p60.
- Made for one person in front of a camera. Hair and fast hands come out
  soft. Not tried: hair against a busy wall, 4K.

**Planned, not built** (`docs/spec/10-ai.md`, survey in
`docs/spikes/s8-ai-survey.md`): a faster GPU path for the cut-out, slow
motion by making frames in between, upscaling, a blurred background by
depth. Changing a picture by a sentence (another place, another light)
does not fit this machine; for that the plan is one optional action per
clip that sends it to a service, off unless you put a key in a file. The
survey's prices and model facts come from web pages and were not all
verified.

## 5. Roadmap leftovers (your point 1)

From the video side's old list: **six built-in film looks** (Colour, a
Film look module, "Add the built-in looks": teal and orange, bleach
bypass, warm film, cold night, faded, black and white). I looked at each
on one clip; whether they are any good on your footage is for your eye.

Also from the old lists: **tags for assets** (a field under an asset's
name in the inspector; the search finds them), to keep "verse 2, good"
on takes.

Done tonight from the "still to do" lists: takes kept, a punch-out
point, the sustain pedal, and **hearing yourself through the chain**:
with "Hear" on and a track marked "R", your input is heard through that
track's low cut, equaliser, de-esser, leveller, compressor and
saturation (not noise reduction, tune, plugins, reverb or echo: those
would make you late in your own ear or are not built for it). So the hum
in your headphones while recording goes with the low cut. Checked with a
test tone through two private test outputs, not with a microphone. The
stages in another order was done on the 21st. **Not done**: pitch bend,
plugin parameters as keyframes, pitch correction by hand per note, LV2,
loop points in the sampler. Each of these is more than a few hours and I
did not want to leave one half-built. M11 to M16 stay unticked: none of
stage 3 has been confirmed by you yet.

## Decided for you (all marked `PROPOSED` in the specs)

- "Voice" does not move other tracks' faders; it only says where the
  beat should be.
- Kept takes go onto a separate muted track, not into a lane.
- The sustain pedal is done in Shotgun, not passed to the plugin (works
  for every instrument; a sampled piano's own pedal sound is not used).
- The cut-out is a second asset, not a switch on the clip.
- The model for it is GPL-3.0 code: fine for your own use, a question
  only if you ever give Shotgun to others.

## Numbers (docs/perf-log.md)

- UI frame with the new look: p99 0.4 to 1.9 ms (budget 6.9).
- Audio engine 20 s on a null sink: 0 underruns, as before.
- Full-quality seek on 4K: still misses its budget, unchanged.
