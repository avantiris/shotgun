# 07 UI and key map

Everything here is `PROPOSED`: it is my design and the first thing to adjust
after the user's first test.

## One window, three states

1. **Library**: grid of projects. Type to filter, Enter/click opens the
   selected one (arrows select), `Ctrl + N` creates a project named after
   the typed text (Enter does the same when nothing matches). Changed in M1
   from a plain `N`: typing must go to the filter, so a bare letter cannot
   also be a command there.
2. **Project**: the editor.
3. No modal dialogs. Export, add-asset and history are panels/popovers that
   close with `Esc`.

Editor layout (single window; panels resize but do not float or re-dock):

```
+------------------+-------------------------------+------------------+
| Assets           |                               | Inspector        |
| (kind tabs,      |            Viewer             | (clip / title /  |
|  search, tiles)  |                               |  scenario chain) |
+------------------+-------------------------------+------------------+
| Timeline: video tracks above, audio tracks below, beat grid, markers|
+---------------------------------------------------------------------+
```

- The inspector always shows the selection: clip transform and keyframes,
  title text, audio track chain, or the scenario's match + look chain.
- Dropping a file or pasting a link **anywhere** in the window imports it.
  Tracks never have to be made first: a drop makes the ones it needs.
  Dropping on the timeline imports and places it in one step.

## Mouse

| Action | Result |
|--------|--------|
| Wheel on timeline | move along the sequence (asked for in feedback F4.5; it zoomed before) |
| Ctrl + wheel, or a pinch | zoom around the pointer (F4.5); H shows the whole sequence |
| Drag the line across a sound clip up or down | that clip's volume (F4.2), from +12 dB at the top to silence at the bottom, 0 dB a quarter down; in whole decibels, with Shift a tenth per point. All selected sound clips change by as much. One undo step per pull. The same is in the clip's menu (`Clip volume`) and the inspector |
| Middle drag | pan timeline / viewer |
| Click or drag on the ruler or on empty timeline | set / scrub playhead |
| Click clip | select it with its partner (picture + sound of one take); Shift adds, Alt selects it alone |
| Drag clip | move (snaps; hold Alt to disable snapping; hold Ctrl to put it over what is in the way, `PROPOSED`, added 2026-11-19) |
| Left / Right arrow | move the selected clips one frame earlier / later; with Shift ten frames (`PROPOSED`, added 2026-11-19) |
| Drag clip edge | trim (Ctrl = ripple) |
| Drag the small mark at a top corner of a selected clip | its fade in (left) or fade out (right); all selected clips follow, so picture and sound of a take fade together. The fade is drawn on the clip (`PROPOSED`, added 2026-11-19) |
| Drag in viewer | move the selected clip's picture; its corner handles size it, the knob above it turns it (added 2026-11-19) |
| Drag an asset from the panel onto the timeline | place it where it is let go: picture on the video track under the pointer, sound on an audio track; snaps like a moved clip (Alt disables). Where the spot is taken, or outside the tracks, it goes to the next free track, and a new track is made if there is none. A ghost shows the result before the button is released |
| Drop files from another application onto the timeline | import them and place them there, one after the other |
| `+ Video` / `+ Audio` above the track names | add a track (video on top, audio at the bottom) |
| Right click on the timeline | marker, title; add a video or audio track, remove the empty track under the pointer |
| Right click on a clip | remove its sound and keep the picture, or the reverse; its scenario; its volume; keyframe, remove keyframe, whip-pan, delete (each with its key written beside it) |
| Shift while dragging an asset onto a track | place only the part that belongs there (the sound on an audio track, the picture on a video track) |
| Right click on an asset | place it, or only its sound or picture, at the playhead; use it as the song; sync it (or all takes) to the song; lay it as a camera angle to sync by hand; remove it from the project |
| `shown` / `hidden` next to a video track name | take the track out of the picture: a camera angle to cut from |
| `M` / `S` next to an audio track name | mute, solo |

## Buttons for the less obvious keys (feedback F4.1, narrowed in F5.1)

What is not found by trying has a button with its key written on it, so
that the key is learnt by using the button. The classic actions (split,
cut off, delete, copy, cut, paste, undo, redo, zoom, play, stepping) have
no buttons: the user asked for them to be taken out again.

- Above the timeline: keyframe, remove keyframe, record movement,
  whip-pan, title, marker. Those that need a selected clip are grey
  without one.
- Top bar: checkpoint, history, export, keys, colour, mixer.
- Right click on a clip: keyframe, remove keyframe, whip-pan, delete, and
  the clip's volume, sound / picture and scenario.

Buttons and menu entries run the same action as the key (`Shell::act`), so
they cannot drift apart. The key shown is the one in force, the user's key
file included.

## Keys (left hand, bound by physical position)

Labels below are the **Swiss German (QWERTZ)** key caps. Bottom row is
`Y X C V B`, top row `Q W E R T`.

In code the map is written in winit `KeyCode` names, which follow US key
positions. The only left-hand difference: the key cap `Y` (bottom left) is
`KeyCode::KeyZ`, and the key cap `Z` (right of `T`) is `KeyCode::KeyY`. So
"Y = undo" below is `KeyZ`, and "Ctrl + Z" is `Ctrl + KeyY`.

Transport

| Key | Action |
|-----|--------|
| Space | play / pause |
| Enter, or double-click an asset | put the selected asset at the end of the timeline (M3; placing at the playhead comes with M4) |
| A / D | step one frame back / forward (hold to shuttle) |
| Shift + A / D | jump to previous / next cut or marker |
| Ctrl + A / D | jump to sequence start / end |
| Tab | toggle viewer full window |

Cutting

| Key | Action |
|-----|--------|
| S | split at playhead (selected clips, or all under playhead) |
| Q | ripple-trim: remove from clip start to playhead |
| E | ripple-trim: remove from playhead to clip end |
| X | ripple delete selection |
| Shift + X | delete, leave gap |
| W | select clip under playhead (repeat: next track up) |
| C / V | copy selected clips / paste them at the playhead, on the same tracks, with what they were given (volume, fades, placement). The copies go down over what is in their place, like a move with Ctrl (`PROPOSED`, 2026-11-19; a paste without room was refused before) and stay selected |
| Ctrl + C / Ctrl + X | copy / cut (copy and take out, leaving the gap) the selected clips (feedback F4.4) |
| Ctrl + V | paste: the copied clips, if they were the last thing copied; if a link, file or image was copied anywhere since: **import it**. How it is told apart: copying clips puts a short text on the system clipboard as a mark, and Ctrl + V looks at what the clipboard holds now. In a text field the three keys belong to the text |
| G | add marker at the playhead (again on the same frame: remove it) |
| Y | undo (Shift + Y redo); Ctrl + Z / Ctrl + Shift + Z also work |

Animation and creation

| Key | Action |
|-----|--------|
| F | add / update keyframe for the selected layer at the playhead |
| Shift + F | remove keyframe at playhead |
| R | live record: plays; drag the selected clip's picture in the viewer and the motion is recorded as keyframes. R again or pause ends it |
| T | add a 3 s title at the playhead, on the topmost video track that is free there (a new track if none is) |
| B | add transition on the nearest cut (whip-pan by default) |

Panels and output

| Key | Action |
|-----|--------|
| 1 to 9 | cut the programme to camera angle 1 to 9 at the playhead, also while playing; a click on an angle in the monitor under the viewer does the same. (Focusing panels by number is not built.) |
| Ctrl + Q | add asset (opens link/file popover) |
| Del | remove the selected asset from the project |
| Ctrl + E | export panel; Ctrl + Shift + E repeats the last export |
| Ctrl + G | export selection/range as GIF to the clipboard |
| Ctrl + S | name a checkpoint (saving itself is automatic) |
| Ctrl + H | history: saved states and checkpoints, restore any of them |
| Ctrl + W | close project, back to library |
| K, or the Colour button | colour panel in place of the inspector: scenarios, match strength and hero, look chain |
| J / Shift + J | A/B: look chain / match of the shown scenario off and on |
| B | whip-pan on the nearest cut; again removes it (kind and settings in the clip inspector) |
| M, or the Mixer button | mixer in place of the inspector: per audio track a level meter, fader (double-click: 0 dB), pan, mute, solo, low cut, noise reduction; master fader |
| F1, or the Keys button | every key and what it does |
| ` (key left of 1) | performance overlay (with Ctrl while a text field has focus, e.g. in the library) |
| H | show the whole sequence in the timeline (`PROPOSED`, added 2026-11-19) |
| F2 | rename the project |
| Esc | cancel / close popover |

Modifiers follow one rule: **Shift = bigger/alternate, Ctrl = ripple or
app-level, Alt = disable snapping / fine adjustment.**

The key map lives in one table in code and is written to a config file so it
can be changed without rebuilding. Built 2026-11-19: the table is in
`sg-ui/src/keymap.rs`; `~/.config/shotgun/keys.conf` (or the file named by
`SHOTGUN_KEYS`) is read at start. `PROPOSED` form: the file holds only
changes. On the first start by hand it is written with every built-in key
as a comment line (`# editor S = Split`); removing the `#` and editing the
line changes that key. An action named in the file has only the keys given
there, a key given there is taken from whatever had it, and
`none = <action>` leaves an action without a key. Keys are named by
position with US names. F1 shows the keys in force, the file's path and
the lines it did not understand. Labels on buttons that mention a key
("Keyframe (F)") still show the built-in key. Automated runs never write
the file and `scripts/run-hidden.sh` points them away from the user's.

Rule for text fields: while one has focus, keys without Ctrl go to the text;
Ctrl bindings and Esc keep working.
