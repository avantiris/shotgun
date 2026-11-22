# Night of 2026-11-22: the brief

Given at 00:35, to be worked on unattended until 08:00. No questions: decisions are taken and marked `PROPOSED`.

### N2.1 Finish what the specs and the roadmap leave undone
- Said: "if any milestone or anything in spec / roadmap is not done, do it."
- Kind: missing
- Area: other
- Status: open

### N2.2 The vocal chain and the compressor again; the automatic settings are not good
- Said: "analyze the full vocalchain and compressor things again for errors and also optimize the autosettings they are really not good they do not avoid whitenoise and do not properly fit (you can use the audiotracks I now recorded with a dogshit mic to test)"
- Kind: bug
- Area: audio
- Status: fixed (331d4c8) for the automatic settings; see `docs/spec/06-audio.md`, "Changed in the night of 2026-11-22"
- Found: on his takes the listening saw no noise at all (the hum was within the 35 dB it took for voice), so it set no noise reduction; it counted the hum as part of the voice's tone; and its sums for compressor and fader were for a chain that has changed since, and ended with the fader at +12 dB.
- Verified: `sg-bench voice` on copies of his seven takes (numbers in the spec), a test with a made-up noisy voice. Not heard by anyone.

### N2.3 One look for the whole UI, not GTK-like buttons
- Said: "optimize the full UI by making a more unified theme in a sense that buttons right now look a bit like GTK buttons, chose a style like a very \"edge based style\" or a very modern style but do not do this GTK like rounded button style from now. Give the buttons some feedback and some little details that it just looks more professional and finetuned."
- Kind: workflow
- Area: other
- Status: fixed (f4a3810)
- Now: one look for every control (`docs/spec/07-ui-keybinds.md`, "The look"): flat, one-pixel borders, nearly square corners, a lit border when pointed at, the accent while pressed.
- Verified: screenshots of library, editor, mixer, chain, export popover in a hidden run. A matter of taste: his to judge.

### N2.4 The basics of a DAW that were not asked for one by one
- Said: "Implement features expected for a audio DAW I didnt explicitly mention that will eventually be required (not every edgecase is required but like the most basics I missed for now)."
- Kind: missing
- Area: audio
- Status: partly (e72c89b, 0b1cb7a and the commit after)
- Built: MIDI files in and out, arrow keys on notes, tracks moved up and down, tap tempo, sustain pedal, takes kept on a muted track, punching into a looped stretch, a clip brought to full level, make-up gain. The list and what is still missing: `docs/spec/09-daw.md`, "Added without being asked for one by one".
- Verified: headless flows and unit tests for each. None of it was heard or used by a person.

### N2.5 A way to use AI on the video
- Said: "If that is somehow possible give me a potential AI integration for the video editor so that I could use AI to enhance the Video (e.g. change background etc) I right now have no idea about what options there are, if possible it should be a local vendored model but if that doesnt work and there is no opensource stuff that is really good then just use an API optional to this provider so I can use it selectively."
- Kind: idea
- Area: other
- Status: a first feature built (21def6b); the rest is a plan in `docs/spec/10-ai.md`
- Now: "Cut out the person (AI)" in a video asset's menu: a local model makes a copy of the video with a transparent background, to put over any other background. A survey of what else is possible locally and through services is in `docs/spikes/s8-ai-survey.md`, measurements in `docs/spikes/s8-matte.md`.
- Verified: a hidden run from import to the cut-out composited over another clip in the viewer (screenshot), including the model's download and check. On one clip only. Exported once with `sg-bench export` and a frame of the file looked at: the person is over the other clip there too. Not seen by a person.

## Checked later in the night

- N2.2, through the program: a copy of his take of 23:10 imported in a hidden run, "Voice" on its track (the settings written were those `sg-bench voice` gives), the sound exported with `sg-bench export ... sound` and the file measured: voice at -17.0 dB (raw: -21.5), loud phrases 10.2 dB above it (raw: 10.9), between the phrases -88.6 dB (raw: -32.3, mostly hum). Not heard by anyone.
