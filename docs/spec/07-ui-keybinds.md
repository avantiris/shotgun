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
  Dropping on the timeline imports and places it in one step.

## Mouse

| Action | Result |
|--------|--------|
| Wheel on timeline | zoom around cursor |
| Shift + wheel | scroll horizontally |
| Middle drag | pan timeline / viewer |
| Click or drag on the ruler or on empty timeline | set / scrub playhead |
| Click clip | select it with its partner (picture + sound of one take); Shift adds, Alt selects it alone |
| Drag clip | move (snaps; hold Alt to disable snapping) |
| Drag clip edge | trim (Ctrl = ripple) |
| Drag in viewer | move selected layer; corner = scale; outside corner = rotate |
| Right click | context menu for the thing under the cursor |

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
| C / V | copy selected clips / paste them at the playhead (same tracks; refused if there is no room) |
| Ctrl + V | paste; if the clipboard holds a link, file or image: **import it** |
| G | add marker at the playhead (again on the same frame: remove it) |
| Y | undo (Shift + Y redo); Ctrl + Z / Ctrl + Shift + Z also work |

Animation and creation

| Key | Action |
|-----|--------|
| F | add / update keyframe for the selected layer at the playhead |
| Shift + F | remove keyframe at playhead |
| R | live record: plays, and mouse drags in the viewer are recorded as motion |
| T | add title at playhead |
| B | add transition on the nearest cut (whip-pan by default) |

Panels and output

| Key | Action |
|-----|--------|
| 1 / 2 / 3 / 4 | focus Assets / Viewer / Inspector / Timeline (during live multicam: switch angle) |
| Ctrl + Q | add asset (opens link/file popover) |
| Del | remove the selected asset from the project |
| Ctrl + E | export panel; Ctrl + Shift + E repeats the last export |
| Ctrl + G | export selection/range as GIF to the clipboard |
| Ctrl + S | name a checkpoint (saving itself is automatic) |
| Ctrl + H | history: saved states and checkpoints, restore any of them |
| Ctrl + W | close project, back to library |
| ` (key left of 1) | performance overlay (with Ctrl while a text field has focus, e.g. in the library) |
| F2 | rename the project |
| Esc | cancel / close popover |

Modifiers follow one rule: **Shift = bigger/alternate, Ctrl = ripple or
app-level, Alt = disable snapping / fine adjustment.**

The key map lives in one table in code and is written to a config file so it
can be changed without rebuilding. (M1: the table exists in
`sg-ui/src/keymap.rs`; the config file is not written yet.)

Rule for text fields: while one has focus, keys without Ctrl go to the text;
Ctrl bindings and Esc keep working.
