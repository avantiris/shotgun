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
- Status: open

### N2.5 A way to use AI on the video
- Said: "If that is somehow possible give me a potential AI integration for the video editor so that I could use AI to enhance the Video (e.g. change background etc) I right now have no idea about what options there are, if possible it should be a local vendored model but if that doesnt work and there is no opensource stuff that is really good then just use an API optional to this provider so I can use it selectively."
- Kind: idea
- Area: other
- Status: open
