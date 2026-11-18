# S2 UI stack

Question: which UI stack keeps UI frame time <= 6.9 ms p99 (144 Hz) regardless
of project size, with zero redraws when idle, on Wayland/Hyprland? Also:
physical-key input on the `ch` layout, file drag-and-drop, clipboard paste.

Date: 2026-11-18. Code: `spikes/s2-ui/` (standalone Cargo project, throwaway).
Logs and screenshots: `/disk/shotgun-dev/media/s2/`.

## Short answer

winit 0.30 + wgpu 30 + egui 0.37 for panels, with the timeline drawn by our
own instanced wgpu pipeline inside an egui paint callback, meets the budget:
CPU frame time p99 2.2 to 3.2 ms with 50 000 clips (5 runs) and 3.3 to 5.0 ms
with 500 000 clips (2 runs). The same timeline drawn with the egui painter
misses it: p99 12.2 to 13.4 ms at 50 000 clips. Idle is 0 redraws and 0.01 %
CPU. winit 0.30.13 has no Wayland drag-and-drop; a 390-line `wl_data_device`
handler on winit's own `wl_display` fixes that and also does clipboard paste
(both verified by run, see below).

## Environment

| Item | Value |
|------|-------|
| GPU / driver | AMD Radeon RX 7800 XT, RADV NAVI32, Mesa 26.2.6, wgpu Vulkan backend |
| Compositor | Hyprland 0.56.2, Wayland |
| Monitor used | HDMI-A-2, 1920x1080 @ 144.001 Hz, scale 1 (reported by winit at exit in all 19 benchmark runs) |
| Window | floating, 1800x1000 logical = physical px |
| Surface | `Bgra8Unorm`, alpha `Opaque`, present mode `Fifo` (one run `Mailbox`), `desired_maximum_frame_latency = 2` |
| Present modes offered | `Mailbox`, `Fifo`, `Immediate` |
| Keyboard layout | `ch` (`hyprctl getoption input:kb_layout`) |
| Toolchain | rustc 1.99.1, release profile `opt-level = 3` |

The machine was **not quiet**: other spikes ran at the same time (an `ffmpeg`
at 840 % CPU and `s1` runs were seen in `ps`; load average 4 to 15). The
benchmark script waits for < 12 % total CPU busy before each run and logs the
busy value before and after, but load during a run could not be excluded.
Treat max values and present-interval tails as upper bounds under load.

Not tested: the 2560x1440 @ 60 monitor, fractional scaling, real mouse
interaction with the timeline (zoom and scroll are scripted), GPU time
(only CPU time and frame pacing were measured), input-to-photon latency.

## Crate versions that built together

From `spikes/s2-ui/Cargo.lock`:

| Crate | Version | Note |
|-------|---------|------|
| winit | 0.30.13 | pulled in by egui-winit; 0.31 is still beta (0.31.0-beta.4) and egui does not use it |
| wgpu (core, hal, naga) | 30.0.2 | |
| egui, epaint | 0.37.0 | feature `default_fonts` must be enabled (see Gotchas) |
| egui-wgpu | 0.37.0 | |
| egui-winit | 0.37.0 | default features (brings smithay-clipboard 0.7.3 and arboard 3.7.0) |
| glyphon | 0.12.0 | with cosmic-text 0.19.0; only for the text comparison |
| wayland-client | 0.31.15 | for DnD and paste |
| wayland-backend | 0.3.17 | feature `client_system` (needed for `Backend::from_foreign_display`) |
| raw-window-handle | 0.6.2 | |
| bytemuck 1.25.2, pollster 1.0.2, libc 0.2.191 | | |

## What was built

`spikes/s2-ui/src/`:

- `main.rs`: the prototype. Left "Assets" panel, centre viewer (a 1280x720
  texture rendered by our own wgpu pass and shown through
  `Renderer::register_native_texture`), right inspector with six egui
  sliders, bottom timeline. Scripted zoom/scroll, idle/playback phases, key
  logging, timing statistics.
- `timeline.rs` + `timeline.wgsl`: synthetic project (N clips over 20 tracks,
  clip length 0.5 to 10 s, random gaps), the visible-clip lookup, and the
  instanced renderer (one 36-byte instance per quad, rounded-rect signed
  distance in the fragment shader, one `draw(0..4, 0..n)` call).
- `text.rs`: clip labels through glyphon, for comparison only.
- `wl_data.rs`: Wayland drag-and-drop and clipboard paste next to winit.
- `bin/dnd_test.rs`: the small manual DnD/paste test window.
- `bin/inject.rs`: synthetic evdev input through `/dev/uinput`.
- `bench.sh`, `scripts/*.sh`: the exact runs below.

Visible-clip lookup (used identically by both timeline variants): per track,
clips are sorted, non-overlapping arrays (`starts`, `ends`). First visible
clip by `partition_point` (binary search), then a linear walk to the right
edge. Clips narrower than 2 physical px are merged into one "dense run" quad
per 2 px bucket; the end of each bucket is again found by binary search. So a
track never emits more than about `width_px / 2` quads however many clips it
has. Measured maximum: 14 811 quads (50 000 clips) and 17 396 quads (500 000
clips) for 20 tracks at about 1784 px timeline width.

Screenshot: `/disk/shotgun-dev/media/s2/s2-prototype-50k-custom.png`.

## Commands

All from the repo root. Note that the dev shell's `shellHook` sets
`CARGO_TARGET_DIR=/disk/shotgun-dev/target`, which overrides a value set
outside `nix develop`; it has to be exported inside the shell (the scripts do
that).

```sh
# build
nix develop --command bash -c 'export CARGO_TARGET_DIR=/disk/shotgun-dev/target-s2; cd spikes/s2-ui && cargo build --release'

# frame-time matrix (each run opens a window for 10 s on HDMI-A-2)
nix develop --command bash spikes/s2-ui/bench.sh main        # 500 / 50 000 / 500 000 x egui / custom, 2 reps
nix develop --command bash spikes/s2-ui/bench.sh custom50k   # 3 more reps of 50 000 custom
nix develop --command bash spikes/s2-ui/bench.sh extra       # labels none / glyphon / per-label clip, mailbox

# one run by hand
/disk/shotgun-dev/target-s2/release/s2-ui --clips 50000 --timeline custom --labels egui --secs 10
#   --timeline egui|custom   --labels none|egui|egui-clip|glyphon   --present fifo|mailbox|immediate
#   --mode script|idle|keys  --size WxH

# idle / playback redraw test (14.5 s)
/disk/shotgun-dev/target-s2/release/s2-ui --mode idle --clips 50000

# physical vs logical keys with synthetic evdev input
nix develop --command bash spikes/s2-ui/scripts/keys_test.sh

# clipboard paste (URL, PNG, file list) without a human
nix develop --command bash spikes/s2-ui/scripts/paste_test.sh

# file drag from Nautilus with a synthetic pointer (takes over the mouse for ~3 s)
nix develop --command bash spikes/s2-ui/scripts/dnd_drive.sh
```

`bench.sh` adds a runtime Hyprland rule
(`hyprctl keyword windowrule 'match:class ^(shotgun-s2)$, float on, monitor HDMI-A-2, size 1800 1000, center on'`)
so the window floats on the 144 Hz monitor. It disappears with `hyprctl reload`.

## Frame times

Scripted 10 s run: zoom oscillates on a log scale between "whole project
visible" and 200 px/s (period 5 s) while the view scrolls across 90 % of the
project (period 7 s). Every frame is redrawn, the viewer texture is
re-rendered every frame, the first 30 frames are excluded.

- **CPU** = egui input + UI build (includes the timeline lookup and instance
  fill) + tessellation + texture/buffer upload + command encoding +
  `queue.submit`. It excludes `get_current_texture` (p99 0.09 to 0.16 ms in the 15 main
  runs) and `present` (p99 0.15 to 0.73 ms).
- **Interval** = time between successive returns of `Queue::present`. This is
  our pacing, not scan-out time; wgpu exposes no presentation feedback. One
  refresh is 6.94 ms. `>10.4` counts intervals longer than 1.5 refreshes,
  that is, frames that missed a vblank.
- **Busy** = % of all cores busy in the second before / after the run.

All times in ms.

| Clips | Timeline | Labels | Present | Busy b/a % | CPU p50 | p95 | p99 | max | Int p50 | p95 | p99 | max | Frames | >10.4 | Log |
|------:|----------|--------|---------|-----------:|--------:|----:|----:|----:|--------:|----:|----:|----:|-------:|------:|-----|
| 500 | egui painter | egui | fifo | 5 / 17 | 0.72 | 1.38 | 1.78 | 2.47 | 6.94 | 7.62 | 8.20 | 10.07 | 1427 | 0 | `main-500-egui-r1` |
| 500 | egui painter | egui | fifo | 10 / 11 | 0.78 | 1.55 | 1.88 | 2.71 | 6.93 | 7.61 | 8.06 | 9.31 | 1429 | 0 | `main-500-egui-r2` |
| 500 | instanced | egui | fifo | 11 / 17 | 0.82 | 1.43 | 1.79 | 3.21 | 6.92 | 7.78 | 8.37 | 9.58 | 1429 | 0 | `main-500-custom-r1` |
| 500 | instanced | egui | fifo | 11 / 31 | 0.64 | 1.15 | 1.34 | 2.50 | 6.93 | 7.62 | 8.05 | 391.19 | 1374 | 1 | `main-500-custom-r2` |
| 50000 | egui painter | egui | fifo | 10 / 22 | 2.25 | 11.27 | **13.39** | 18.72 | 7.07 | 14.52 | 17.23 | 22.19 | 1184 | 205 | `main-50000-egui-r1` |
| 50000 | egui painter | egui | fifo | 5 / 3 | 2.28 | 11.21 | **12.24** | 15.23 | 7.03 | 14.15 | 16.79 | 19.10 | 1184 | 206 | `main-50000-egui-r2` |
| 50000 | instanced | egui | fifo | 10 / 22 | 1.31 | 2.30 | **2.98** | 18.92 | 6.93 | 8.07 | 17.39 | 24.37 | 987 | 15 | `main-50000-custom-r1` |
| 50000 | instanced | egui | fifo | 3 / 2 | 1.66 | 2.85 | **3.18** | 3.81 | 6.96 | 7.78 | 8.52 | 10.69 | 348 | 1 | `main-50000-custom-r2` |
| 50000 | instanced | egui | fifo | 4 / 8 | 1.15 | 1.93 | **2.29** | 3.61 | 6.94 | 7.46 | 8.57 | 9.88 | 1422 | 0 | `main-50000-custom-r3` |
| 50000 | instanced | egui | fifo | 8 / 8 | 1.57 | 2.74 | **3.12** | 4.58 | 6.94 | 8.01 | 8.80 | 9.28 | 1430 | 0 | `main-50000-custom-r4` |
| 50000 | instanced | egui | fifo | 8 / 19 | 1.28 | 1.96 | **2.25** | 2.91 | 6.94 | 7.27 | 7.75 | 10.01 | 1429 | 0 | `main-50000-custom-r5` |
| 500000 | egui painter | egui | fifo | 11 / 10 | 3.03 | 12.31 | **13.40** | 15.54 | 7.28 | 14.64 | 16.84 | 23.01 | 1092 | 291 | `main-500000-egui-r1` |
| 500000 | egui painter | egui | fifo | 4 / 7 | 2.79 | 12.05 | **13.19** | 14.56 | 7.48 | 15.22 | 19.29 | 21.93 | 1063 | 309 | `main-500000-egui-r2` |
| 500000 | instanced | egui | fifo | 11 / 12 | 1.70 | 2.82 | **3.28** | 9.24 | 6.92 | 7.91 | 8.52 | 16.40 | 1427 | 2 | `main-500000-custom-r1` |
| 500000 | instanced | egui | fifo | 6 / 8 | 2.43 | 4.25 | **5.05** | 7.96 | 6.94 | 8.69 | 9.86 | 12.27 | 1423 | 8 | `main-500000-custom-r2` |

Variants (one run each, `bench.sh extra`; this batch ran under visibly more
load than the rows above, e.g. `queue.submit` p50 0.8 to 1.3 ms instead of
0.2 ms, so compare these rows with each other, not with the table above):

| Clips | Timeline | Labels | Present | Busy b/a % | CPU p50 | p95 | p99 | max | Int p50 | p95 | p99 | max | Frames | >10.4 | Log |
|------:|----------|--------|---------|-----------:|--------:|----:|----:|----:|--------:|----:|----:|----:|-------:|------:|-----|
| 50000 | instanced | none | fifo | 9 / 11 | 1.63 | 3.32 | 4.16 | 4.93 | 6.92 | 8.70 | 9.29 | 10.23 | 1427 | 0 | `extra-custom-nolabels` |
| 50000 | instanced | egui, clip rect per label | fifo | 11 / 14 | 1.80 | 3.50 | 4.50 | 5.98 | 6.99 | 8.98 | 10.27 | 15.15 | 1418 | 13 | `extra-custom-eguiclip` |
| 50000 | instanced | glyphon | fifo | 9 / 27 | 2.50 | 4.46 | 5.74 | 24.27 | 7.00 | 9.26 | 10.59 | 27.89 | 1405 | 19 | `extra-custom-glyphon` |
| 50000 | egui painter | none | fifo | 8 / 23 | 2.58 | 15.33 | 17.92 | 22.37 | 7.57 | 18.60 | 21.40 | 24.93 | 1067 | 260 | `extra-egui-nolabels` |
| 50000 | instanced | egui | mailbox | 8 / 13 | 2.37 | 4.16 | 5.19 | 7.58 | 6.98 | 8.96 | 9.74 | 12.20 | 1428 | 6 | `extra-custom-mailbox` |
| 500000 | instanced | none | fifo | 8 / 22 | 1.05 | 2.93 | 3.75 | 5.02 | 6.94 | 7.42 | 8.02 | 8.75 | 1427 | 0 | `extra-custom-500k-nolab` |
| 500000 | instanced | glyphon | fifo | 8 / 8 | 2.86 | 5.08 | 6.11 | 17.47 | 6.99 | 9.72 | 12.55 | 19.77 | 1364 | 54 | `extra-custom-500k-glyph` |

Where the time goes (p50 / p95 / p99 in ms):

| Run | UI build | of which timeline lookup + instance fill | of which egui labels | Tessellation | Encode + buffers | Submit | egui vertices p99 |
|-----|----------|------------------------------------------|----------------------|--------------|------------------|--------|-------------------|
| 500, instanced (`r1`) | 0.34 / 0.60 / 0.70 | 0.01 / 0.03 / 0.05 | 0.07 / 0.17 / 0.22 | 0.16 / 0.34 / 0.51 | 0.08 / 0.13 / 0.18 | 0.24 / 0.38 / 0.52 | 9 708 |
| 50 000, instanced (`r3`) | 0.76 / 1.48 / 1.67 | 0.03 / 1.21 / 1.24 | 0.04 / 1.05 / 1.33 | 0.09 / 0.17 / 0.23 | 0.05 / 0.13 / 0.19 | 0.18 / 0.31 / 0.41 | 11 528 |
| 500 000, instanced (`r1`) | 1.26 / 2.24 / 2.71 | 0.18 / 1.84 / 2.10 | 0.00 / 1.30 / 2.03 | 0.11 / 0.21 / 0.30 | 0.10 / 0.16 / 0.18 | 0.22 / 0.36 / 0.42 | 12 784 |
| 50 000, egui painter (`r2`) | 0.78 / 2.04 / 2.44 | (walk + painter calls) 0.49 / 1.32 / 1.54 | included | 0.57 / 7.60 / **8.79** | 0.14 / 1.08 / 1.36 | 0.26 / 1.99 / 2.61 | 195 116 |

Readings:

- The egui painter timeline fails because of **tessellation**: about 14 800
  rectangles become about 195 000 vertices on the CPU every frame (8.8 ms
  p99), plus the upload. It misses a vblank in 17 % of frames at 50 000 clips
  (205 of 1184). At 500 clips it is fine (p99 1.8 ms), so the cost is in the
  number of visible quads, not in egui itself.
- The instanced timeline uploads at most 17 396 x 36 bytes = 626 kB per frame
  and draws it in one call; encode + upload stays under 0.2 ms p99.
- Panels, sliders and viewer through egui cost about 0.7 ms CPU per frame in
  total (500-clip rows, p50 0.64 to 0.82 ms), so a hand-written UI could save
  well under 1 ms.
- The remaining timeline cost is the lookup at dense zoom levels (1.2 ms p95
  at 50 000 clips for 14 500 quads, 1.8 ms at 500 000) and laying out up to
  about 200 labels (1.0 to 1.3 ms p95). The scripted scroll replaces almost
  every label every frame at high zoom, which is harsher than real use.
- `main-50000-custom-r1` (max 18.9 ms, 15 missed vblanks, 987 frames) ran
  while the machine went from 10 % to 22 % busy. Its p99 is still 2.98 ms.
- `main-50000-custom-r2` has only 348 frames: frames stopped 2.4 s into the
  run and never resumed, with no long interval recorded. The window received
  `CursorLeft`; the likely cause is that the workspace was switched and
  Hyprland stopped sending frame callbacks (cause **unverified**). Its
  percentiles cover only the first 2.4 s.
- `main-500-custom-r2` has one 391 ms interval with CPU max 2.5 ms; the
  window received 301 pointer events and focus changes in that run. Cause
  **unverified** (not CPU time in our frame).
- An earlier, ungated run of the same matrix (logs overwritten) showed
  500 000-clip instanced at CPU p99 7.45 ms / max 19.2 ms while `ffmpeg` used
  8 cores. So under heavy external load the 500 000-clip case can exceed
  6.9 ms; 500 000 clips is 10x the budgeted project size.
- Mailbox was run once and showed no benefit in that run.

## Idle and playback redraw

`s2-ui --mode idle --clips 50000` (log `logs/idle.log`). Process CPU time is
`CLOCK_PROCESS_CPUTIME_ID` (all threads).

| Phase | Duration | Redraws | Window events | Process CPU |
|-------|---------:|--------:|--------------:|-------------|
| idle A | 3.00 s | **0** | 0 | 0.4 ms (0.01 % of one core) |
| playback A: non-UI thread calls `egui::Context::request_repaint()` 180 times at 60 Hz | 3.30 s | **360** | 0 | 303 ms (9.2 %) |
| playback B: non-UI thread sends 180 `EventLoopProxy` events, handler calls `Window::request_redraw()` | 3.30 s | **180** | 0 | 212 ms (6.4 %) |
| idle B | 3.00 s | **0** | 0 | 0.4 ms (0.01 %) |

- Event-driven idle works: `ControlFlow::Wait`, no redraw unless something
  asked for one.
- A non-UI thread can drive redraws. Through `EventLoopProxy` it is exactly
  one redraw per request.
- `Context::request_repaint()` produces **two** frames per call (egui
  repaints once more to settle). Do not use it as the playback tick.

The pointer was not over the window during this run, so "pointer moves cause
a redraw, then nothing" was not measured separately.

## Physical keys on the `ch` layout

winit 0.30.13 derives `KeyEvent::physical_key` from the raw evdev scancode
(`platform_impl/linux/common/xkb/keymap.rs`, `raw_keycode_to_physicalkey` =
keycode minus 8, fixed table), independent of the keymap. Confirmed by
injecting evdev key codes through `/dev/uinput` (the compositor applies the
`ch` layout to the virtual keyboard as to a real one). Output of
`scripts/keys_test.sh` (`logs/keys.log`):

| evdev code sent | `physical_key` | `logical_key` |
|-----------------|----------------|---------------|
| KEY_Y (21) | `KeyY` | `z` |
| KEY_Z (44) | `KeyZ` | `y` |
| KEY_SEMICOLON (39) | `Semicolon` | `ö` |
| KEY_LEFTBRACE (26) | `BracketLeft` | `ü` |
| KEY_GRAVE (41) | `Backquote` | `§` |
| KEY_MINUS (12) | `Minus` | `'` |
| KEY_A (30) | `KeyA` | `a` |
| Shift + KEY_2 (3) | `Digit2` | `"` |

Tested with synthetic evdev events, **not** with presses on the physical
keyboard.

Consequence for `07-ui-keybinds.md`: `KeyCode` names are US positions. The
key labelled `Y` on the Swiss keyboard (bottom-left, "undo" in the key map)
is `KeyCode::KeyZ`; the key labelled `Z` is `KeyCode::KeyY`. The key map
table in code must be written in `KeyCode` names and displayed through the
current layout's labels.

## Drag-and-drop and clipboard on Wayland

Findings from source:

- **winit 0.30.13 has no Wayland drag-and-drop.** Nothing under
  `src/platform_impl/linux/wayland/` references `wl_data_device`, and the
  0.30 changelog has no Wayland DnD entry. `WindowEvent::DroppedFile` is
  never emitted on Wayland, so egui's `dropped_files` stays empty there.
  winit 0.30 has no clipboard API either.
- egui-winit 0.37 pastes text through smithay-clipboard and images through
  arboard. It has no file-list paste.
- winit-wayland 0.31.0-beta.4 contains `src/data_transfer.rs` (drag-and-drop
  and clipboard on `wl_data_device` via smithay-client-toolkit) and the 0.31
  changelog lists `ActiveEventLoop::clipboard` and a reworked DnD API. This
  was read, **not built or run**, and egui 0.37 does not use winit 0.31.

Workaround built and run (`src/wl_data.rs`, 389 lines): take the raw
`wl_display*` and `wl_surface*` from the winit window (raw-window-handle),
wrap the display with `Backend::from_foreign_display`, create a second
wayland-client event queue on it, bind `wl_seat` and `wl_data_device_manager`
(v3), create a `wl_data_device`, and run that queue on its own thread. winit
keeps doing everything else. Results go to the UI thread through
`EventLoopProxy`.

- Drag: on `enter` accept the best MIME type and `set_actions(copy|move,
  copy)`; on `drop` call `receive` with a pipe, read it on a short-lived
  thread, then `finish` and `destroy` the offer. Enter/motion/drop carry
  surface-local coordinates.
- Paste: keep the current `selection` offer and its MIME list; on paste call
  `receive` for the best type and read the pipe off-thread.
- MIME preference: `text/uri-list`, `image/png`, `image/jpeg`, `image/gif`,
  `image/webp`, `text/plain;charset=utf-8`, `UTF8_STRING`, `text/plain`,
  `STRING`.

Verified by run:

| Case | How | Result |
|------|-----|--------|
| File drag from **Nautilus** into the window | `scripts/dnd_drive.sh`: real Nautilus window, pointer driven by `/dev/uinput` button events and `hyprctl dispatch movecursor` | `DND enter on_our_surface=true mimes=[application/x-gtk-local-dnd, text/plain;charset=utf-8, text/uri-list, application/vnd.portal.filetransfer, application/vnd.portal.files]`, then `DND DROP at (471,516) mime=text/uri-list: file list ["/disk/shotgun-dev/media/s2/dnd-src/drag me ä.txt"] exists_on_disk=[true]` (`logs/dnd.log`) |
| Paste text URL | `wl-copy <url>`, `dnd_test --auto-paste` | `mime=text/plain;charset=utf-8`, URL intact |
| Paste image | `wl-copy -t image/png < clip.png` | `mime=image/png`, 1327 bytes = source file size, PNG magic |
| Paste file list | `wl-copy -t text/uri-list` with a percent-encoded path | path decoded (space and `ä`), exists on disk |
| Coexistence with egui-winit's own clipboard (smithay-clipboard on the same display) | `s2-ui` runs both | our handler received `selection` events in the 5 runs where the window got keyboard focus (of 23 logged runs); no protocol error in any run. Paste itself was only run in `dnd_test`, which has no egui |

Not verified, needs a human:

- A drag done by hand with the real mouse (the scripted one used a synthetic
  pointer; the compositor and Nautilus were real).
- Other sources: Dolphin, Thunar, a browser (dragging an image or a link),
  several files at once.
- Paste after copying in real applications ("Copy image" in the browser,
  Ctrl+C on files in a file manager). Only `wl-copy` was used as source.

One-line check for the user (window stays 120 s; drag files onto it, or copy
something and press Ctrl+V or P in it; results print to the terminal, the
window turns green while a drag hovers):

```sh
nix develop --command /disk/shotgun-dev/target-s2/release/dnd_test
```

## Label text: egui vs glyphon

Only as far as needed to choose. Same instanced timeline, about 200 labels at
most, 11 px.

| | egui text (`Painter::text`) | glyphon 0.12 (cosmic-text 0.19) |
|-|-----------------------------|----------------------------------|
| CPU p99 / max, 50 000 clips (same batch) | 4.16 / 4.93 ms without labels, 4.50 / 5.98 ms with labels and a clip rect per label | 5.74 / **24.27** ms |
| CPU p99 / max, 500 000 clips (same batch) | 3.75 / 5.02 ms without labels | 6.11 / **17.47** ms |
| Start-up | fonts embedded, no scan | `FontSystem::new()` scans system fonts: 17.0, 17.9 and 26.1 ms warm, **262.5 and 298.5 ms** in the first two runs (logs of those two were overwritten) |
| Clipping a label to its clip | needs a clip rect per label: egui draw batches go from 10 to 213 (max); or elide the string | per-area bounds, stays one draw call |
| Shaping | simple (no complex scripts) | full shaping (not exercised: labels were ASCII) |
| Looks | fine at 11 px, see `labels-compare-3x.png` (top egui, bottom glyphon; different fonts, so this is not a rasteriser comparison) | fine at 11 px |

glyphon's spikes come from shaping every label that appears for the first
time (my cache holds shaped buffers per clip; a miss costs a full shape). The
first cold `FontSystem::new()` alone is the whole 300 ms cold-start budget.

## Alternatives, assessed on paper (not built)

Not built because the primary candidate passed. Statements about other
toolkits below are from their documentation and my prior knowledge and are
**unverified** on this machine.

- **Fully custom immediate-mode UI on wgpu.** Would remove egui's
  tessellation and its second renderer. Measured ceiling of that gain here:
  the whole egui side costs about 0.7 ms per frame, against a 6.9 ms budget.
  Cost: writing sliders, text fields (with IME), scroll areas, popovers,
  focus handling and layout ourselves. Not justified by the numbers.
- **iced 0.14.** Retained Elm-style model with a wgpu renderer on winit, so
  it inherits the same missing Wayland DnD from winit 0.30. A custom wgpu
  timeline is possible through its shader widget. No measured advantage to
  offset the more rigid architecture for a tool UI.
- **GPUI.** Has its own Wayland platform layer (with drag-and-drop) and its
  own renderer, not wgpu, so `sg-render`'s wgpu viewer and timeline would not
  share a device with the UI. Rules it out given the wgpu decision in 02.
- **vello.** A renderer, not a toolkit; does not solve widgets or input.

## What failed or surprised

- egui painter timeline: misses the budget (see table).
- All text rendered as boxes at first: egui 0.37 has `default = []`; the
  embedded fonts need the `default_fonts` feature.
- Segfault at exit in 4 of the first 12 runs (`coredumpctl`; `wl_proxy_destroy` called
  from `wayland_backend::...::ConnectionState::drop`): the extra
  wayland-client `Connection` was dropped after winit had closed the display.
  Fixed by stopping the data thread and dropping all our Wayland objects
  before the event loop is dropped. No crash in the roughly 30 runs after the fix.
- `Context::request_repaint()` from the playback thread doubled the frame
  count (360 redraws for 180 requests).
- glyphon: 15 to 24 ms worst frames and a 260 to 300 ms cold font scan.
- `CARGO_TARGET_DIR` set outside `nix develop` is overridden by the flake's
  `shellHook`; my first builds went to the shared `/disk/shotgun-dev/target`
  (dependency artifacts are still there, about 1.8 GB in that directory in
  total, not all mine).
- Frame-time tails were disturbed by other spikes running on the machine.

## Decision

**Stack**

- Window and input: `winit 0.30.13` (Wayland backend).
- GPU: `wgpu 30.0.2`, Vulkan backend, surface `Bgra8Unorm` (egui wants a
  non-sRGB target), alpha mode `Opaque`, `PresentMode::Fifo`,
  `desired_maximum_frame_latency = 2`.
- Panels and widgets: `egui 0.37.0` with feature `default_fonts`,
  `egui-wgpu 0.37.0`, `egui-winit 0.37.0`. No eframe: we own the event loop,
  the surface and the render pass.
- Timeline and viewer: our own wgpu pipelines.
- Drag-and-drop and import paste: `wayland-client 0.31.15` +
  `wayland-backend 0.3.17` (feature `client_system`) on winit's display.
- Labels: egui text for now. glyphon 0.12.0 builds with this set but is not
  adopted.

**Timeline renderer**

- Data: per track, sorted non-overlapping `starts` / `ends` arrays in the
  project snapshot (struct of arrays). First visible clip by binary search,
  then walk right. This is the same interval lookup `sg-render` needs.
- Level of detail: merge clips narrower than 2 physical px into one quad per
  2 px bucket, finding each bucket end by binary search. Quad count is then
  bounded by `tracks * width_px / 2`, independent of project size.
- Drawing: one instance per quad (rect, colour, corner radius, flags), one
  `write_buffer`, one instanced draw with a rounded-rect distance function in
  the fragment shader. Track backgrounds and selection go in the same
  buffer. Waveforms and thumbnails get their own instanced pipelines later.
- Integration: an `egui_wgpu::CallbackTrait` paint callback on the timeline
  rect, with the pipeline and buffers in `Renderer::callback_resources`.
  This keeps z-order with egui (labels, playhead, popovers above) without a
  second pass. egui sets the viewport to the callback rect, so instance
  coordinates are relative to the timeline rect. Blend state: premultiplied
  alpha, colours in gamma space, as egui does.
- Labels: egui `Painter` text, only for clips at least about 64 points wide,
  added after the callback shape. If label layout shows up in the overlay,
  cap the number of newly laid-out labels per frame.
- Ruler, playhead, markers: egui painter (a few dozen shapes).
- If the dense-zoom lookup (1.2 to 1.8 ms p95) ever matters, add a
  per-track coverage pyramid so the cost depends on pixels only. Not needed
  for the budget today.
- Viewer: `sg-render` renders into a texture; register its view once with
  `Renderer::register_native_texture` and show it as an egui image (as the
  prototype does), or draw it in a paint callback if it needs its own
  output transform.

**Drag-and-drop and clipboard on Wayland**

- Port `spikes/s2-ui/src/wl_data.rs` into `sg-ui`: second event queue on
  winit's `wl_display`, own thread with a `poll` loop (connection fd + a stop
  pipe), `wl_data_device` for the first seat.
- Drop: accept by the MIME preference list above, read the pipe off the UI
  thread, parse `text/uri-list` (percent-decode, `file://` only), send paths
  and the surface-local drop position to the UI thread. The position decides
  "import" versus "import and place on the timeline".
- Import paste (Ctrl+V on the physical `KeyV`): handled by us, not by egui,
  when no egui text field wants keyboard input: `text/uri-list` -> files,
  `image/*` -> image asset, text -> link. Keep egui-winit's clipboard feature
  for text fields (the two data devices coexisted in the prototype; the
  "does a text field want the key" routing itself was not built).
- Shutdown order is mandatory: stop the data thread and drop every
  wayland-client object before the winit event loop is dropped.
- Clipboard **write** (GIF export) was not part of this spike; see S4.
- When egui moves to winit 0.31, re-evaluate: its data-transfer API may make
  this module unnecessary (unverified).

**Event-driven redraw**

- `ControlFlow::Wait` always. Redraw only when requested:
  1. window events for which `egui_winit::State::on_window_event` returns
     `repaint`;
  2. egui's own requests: `Context::set_request_repaint_callback` forwards to
     an `EventLoopProxy` user event; zero delay -> `Window::request_redraw()`,
     non-zero delay -> `ControlFlow::WaitUntil`;
  3. application events from other threads (new viewer frame, job progress,
     drop/paste result): an `EventLoopProxy` user event whose handler calls
     `request_redraw()`.
- Playback: the render/playback thread sends one proxy event per new frame.
  Never `Context::request_repaint()` for this.
- Call `Window::pre_present_notify()` before presenting, so winit paces
  `RedrawRequested` by Wayland frame callbacks.
- Expect no frames at all while the window is hidden (other workspace).
  Nothing that must make progress (playback clock, export, autosave) may be
  driven by redraws.

**Gotchas**

- egui `default_fonts` feature, or all text is boxes.
- Key map in `KeyCode` (US positions): Swiss `Y` key = `KeyZ`, Swiss `Z` key
  = `KeyY`. Use `physical_key` for bindings and `logical_key` / `text` only
  for showing labels and for text entry.
- `get_current_texture` returns an enum in wgpu 30 (`Success`, `Suboptimal`,
  `Timeout`, `Occluded`, `Outdated`, `Lost`, `Validation`); presenting is
  `Queue::present(frame)`.
- Clipboard `selection` events only arrive while the window has keyboard
  focus. That is always true for Ctrl+V, but a paste triggered without focus
  finds nothing.
- An egui clip rect per label multiplies draw batches (10 -> 213 here).
- glyphon's `FontSystem::new()` must never run on the start-up path.
- Measure on a quiet machine; record load next to every perf number.
- Runtime Hyprland rules added by the scripts stay until `hyprctl reload`.

**Open items for a human**

1. Run `dnd_test` (command above): drag files by hand from the file manager
   and from the browser; copy an image in the browser and files in the file
   manager and paste.
2. Press a few real keys in `s2-ui --mode keys --secs 20` and compare with
   the table above.
3. Re-run `bench.sh main` when nothing else is running, to get clean tails.
