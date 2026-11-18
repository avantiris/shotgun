# 01 Product requirements

Shotgun is a single-user, Linux-native video editor for Linus, with a DAW planned
inside the same application. One interface serves all use cases; there are no
separate "modes" or workspaces to switch between.

Status of every statement below: **agreed with the user on 2026-11-18** unless
marked `PROPOSED` (my design, not yet reviewed) or `OPEN`.

## Priorities, in order

1. **Speed.** The app must stay fast as projects grow. Budgets are in
   `04-performance.md` and are hard requirements, not aspirations.
2. **Output quality.** Full-quality export is the default. Compression is opt-in.
3. **Fast workflow.** Few clicks, left-hand keyboard + mouse, no dialogs in the way.
4. **Opinionated structure.** One required project layout, no custom folders.

## Use case 1: music videos (stage 2)

- Multi-angle performance footage cut to one master song.
- **Whip-pan cut** between angle A and B: over roughly 2-6 frames A streaks out
  with directional motion blur and B streaks in from the same direction. Reads
  as a hard cut but feels like the camera whipped across. Parameters: duration,
  direction, blur amount, easing. Whip is the default; the engine is general
  enough that slide and zoom variants are parameter changes.
- **Two-stage colour** (see `05-color.md`):
  - *Match* ("compressor"): all clips of a scenario are normalised to one target.
    Default target is computed automatically; the user may mark a hero clip.
    One strength knob.
  - *Look chain* ("vocal chain"): a free, reorderable chain of small modules
    (2-4 knobs each) applied to the whole scenario. Bypass per module, presets,
    plus a small per-clip trim.
- Helpers: auto-sync takes to the song by camera audio, live multicam switching
  (keys switch angles during playback, cuts are recorded), beat grid with snap,
  keyframeable speed ramps, stabilisation.
- Footage: phone (8-bit or HDR HEVC, often variable frame rate) and mirrorless in
  standard 8-bit profiles. No log or RAW for now, but the pipeline must not
  preclude adding input transforms later.

## Use case 2: casual / meme editing (stage 1, built first)

- App opens straight into the **project library**.
- **Add asset** from: drag and drop from a file manager, file picker, pasted
  link (YouTube and anything yt-dlp supports, Giphy, Google Photos share links,
  direct media URLs), pasted image/file from the clipboard.
- Every asset is downloaded/copied **into the project**, labelled, probed and
  made edit-ready before it can be used. The timeline never references files
  outside the project.
- Fast cutting: split, ripple trim, ripple delete, move, snap.
- **Keyframe animation**, two ways:
  - Classic: set keyframe, move playhead, set keyframe; values interpolate.
  - Live: play the timeline and drag the asset with the mouse; the motion is
    recorded as keyframes (then simplified so it stays editable).
- Titles/text, multi-track video and audio, opacity, crop, transform, speed,
  custom effects.
- **Export** in one or two actions. Presets:
  - 16:9 master, source resolution up to 4K, top quality (default)
  - 9:16 vertical, with reframing of landscape footage in the editor
  - GIF to clipboard for short clips
  - Compressed share copy under a target size, copyable to the clipboard

## Use case 3: music production (later, architecture must be ready now)

Not implemented in stage 1 or 2. Planned: note editor, audio tracks, recording,
mixing and mastering, effect chains, VST plugin hosting, sidechaining.

What must be true **now** so this is an extension and not a rewrite:

- One realtime audio engine for video audio and the future DAW.
- Native PipeWire graph node, working without user configuration, also usable
  from JACK clients via PipeWire's JACK layer; ALSA direct as a later option.
- Low, measured latency: the engine negotiates a small quantum, reports the
  real round-trip latency and counts xruns.
- Per-track insert chains with a plugin interface from day one (built-in
  effects implement the same trait a hosted CLAP/VST3/LV2 plugin will).
- Sample-accurate transport; audio is the master clock for playback.

## Cross-cutting requirements

- **Project = bundle = git repo.** Fixed folder layout, zip it and you have the
  whole project. See `03-project-bundle.md`.
- **Autosave** continuously; unlimited undo/redo that survives restarts.
  Versioning via local git, kept remote-ready (no sync UI yet).
- **Library** lives at `/disk/shotgun`, one subfolder per project.
- **Input**: right hand on the mouse, left hand on the left side of a Swiss
  German (QWERTZ, `ch`) keyboard. Every frequent action is reachable there.
  See `07-ui-keybinds.md`.
- **Platform**: NixOS, Hyprland (Wayland), AMD RX 7800 XT (16 GB VRAM), 16
  cores, 16 GB RAM, monitors 2560x1440@60 and 1920x1080@144. Only this target
  matters; portability is not a goal.

## Explicit non-goals

- Windows/macOS/X11 support.
- Collaboration, cloud sync, accounts.
- Node-based compositing, 3D, a colour page with scopes-and-wheels complexity.
- Custom project folder structures or linked external media.

## Open points

- Google Photos links: public share links work for photos without login
  (S4). `OPEN`: videos from share links are unverified; needs a share link
  containing a video from the user.
- `OPEN` Default master encoder (hardware HEVC 10-bit) needs the user's eye
  test on real footage before it is final (S4).
- `OPEN`, now with numbers (M3, `docs/perf-log.md`): the full-quality frame
  after a seek into 4K HEVC arrives after 175 ms (median) to 330 ms (p95)
  even with a 1 s GOP, against the 150 ms budget, and after up to 850 ms
  with long GOPs. The proxy frame is on screen after 3 to 7 ms meanwhile.
  Decide after seeing it: accept, or ask for an intra mezzanine of originals
  (about 0.56 GB per minute of 4K60).
- `OPEN` Full-quality seek into long-GOP originals: proxy-first display
  (planned) versus an intra mezzanine of originals (S1, see 04).
- `OPEN` Shot-match quality on the user's real footage can only be judged with
  that footage; algorithm is validated in stage 2 with clips the user provides.
- `PROPOSED` Everything in `07-ui-keybinds.md` (layout and key map).
