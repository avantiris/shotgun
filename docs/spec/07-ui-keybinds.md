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
| Click on the ruler or on empty timeline; drag along the ruler | set / scrub playhead |
| Drag over empty timeline | a box: the clips it touches are selected, with Shift added to the selection (feedback F8.1; this drag scrubbed before). At most 2000 clips at once |
| Click clip | select it with its partner (picture + sound of one take); Shift adds, Alt selects it alone |
| Drag clip | move (snaps; hold Alt to disable snapping; hold Ctrl to put it over what is in the way, `PROPOSED`, added 2026-11-19) |
| Left / Right arrow | move the selected clips one frame earlier / later; with Shift ten frames (`PROPOSED`, added 2026-11-19) |
| Drag clip edge | trim (Ctrl = ripple) |
| Drag the small mark at a top corner of a selected clip | its fade in (left) or fade out (right); all selected clips follow, so picture and sound of a take fade together. The fade is drawn on the clip (`PROPOSED`, added 2026-11-19) |
| Drag in viewer | move the selected clip's picture or title; its corner handles size it, the knob above it turns it (pictures 2026-11-19, titles 2026-11-20) |
| Drag an asset from the panel onto the timeline | place it where it is let go: picture on the video track under the pointer, sound on an audio track; snaps like a moved clip (Alt disables). Where the spot is taken, or outside the tracks, it goes to the next free track, and a new track is made if there is none. Picture and sound of one take stay on two tracks next to each other (feedback F9.1): a video track and the audio track directly under it, the pair under the pointer or else the lowest free pair; a new pair is made on top, and a single new track right beside the other part's. A ghost shows the result before the button is released |
| Drop files from another application onto the timeline | import them and place them there, one after the other |
| `+ Video` / `+ Audio` above the track names | add a track (video on top, audio at the bottom) |
| Right click on the timeline | marker, title; add a video or audio track, remove the empty track under the pointer |
| Right click on a clip | remove its sound and keep the picture, or the reverse; its scenario; its volume; keyframe, remove keyframe, whip-pan, delete (each with its key written beside it) |
| Shift while dragging an asset onto a track | place only the part that belongs there (the sound on an audio track, the picture on a video track) |
| `Global` tab of the asset panel | the assets kept for every project (03-project-bundle.md); double-click or right click to copy one into this project, right click to remove it from the global folder |
| Right click on an asset | make it global; place it, or only its sound or picture, at the playhead; use it as the song; sync it (or all takes) to the song; lay it as a camera angle to sync by hand; remove it from the project |
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

## Inspector of a clip: three pages (feedback F6.1, F6.3; `PROPOSED`)

- **Settings**: what the clip is given. For a title, headed TITLE: its
  text first, then size, colour, shadow, font, then placement like any
  picture. `T` adds a title and puts the cursor into its text with
  everything selected; Esc leaves the field. A double click on a title in
  the timeline or "Edit text" in its menu does the same.
- **Transition**: the transition into the clip, all its settings on one page (kind, length up to 60 frames, and per kind: direction, blur, travel, speed-up, or the fade of a black one); shown by itself when B or Shift+B puts one on. Without one, buttons to add each kind.
- **Keyframes (n)**: every keyframe of the clip, in order: its time (click:
  the playhead goes there), the values it fixes, changeable in place, how
  they travel to the next keyframe (linear, smooth, hold; this replaced the
  "Between keys" row), and a button to remove it. More than 80 (a recorded
  movement) are listed up to there.

## Bars and beats (stage 3, M11; `PROPOSED`)

Under SEQUENCE in the inspector, "Bars and beats" gives the sequence a
tempo (beats per minute, beats per bar; "Off" takes it away again). With a
tempo the ruler counts bars instead of time, the lanes show the beats, and
clips snap to the grid. Nothing on the timeline moves when the tempo is set
or changed: clips stay at their place in time.

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
| Home / End (also Ctrl + D for the end) | jump to sequence start / end (Ctrl + A was the start until F8.1 made it select all) |
| Tab | toggle viewer full window |

Cutting

| Key | Action |
|-----|--------|
| S | split at playhead (selected clips, or all under playhead) |
| Q | ripple-trim: remove from clip start to playhead |
| E | ripple-trim: remove from playhead to clip end |
| X | ripple delete selection |
| Shift + X, Backspace, Del | delete, leave gap (Backspace asked for in F8.1) |
| Ctrl + A | select every clip |
| Shift + G | make a group of the selected clips; with one whole group selected, dissolve it (feedback F10.1). A click on a grouped clip selects the whole group (Alt + click: that clip alone), so it moves, copies and deletes as one. Grouped clips carry a light bar on their left edge. Also a button above the timeline and in the clip's menu |
| W | select clip under playhead (repeat: next track up) |
| C / V | copy selected clips / paste them at the playhead, on the same tracks, with what they were given (volume, fades, placement). A paste covers nothing (feedback F7.2): clips copied from one track go to that track if they fit at the playhead, otherwise to the nearest track of the kind with room there, otherwise to a new track. The copies stay selected |
| Ctrl + C / Ctrl + X | copy / cut (copy and take out, leaving the gap) the selected clips (feedback F4.4) |
| Ctrl + V | paste: the copied clips, if they were the last thing copied; if a link, file or image was copied anywhere since: **import it**. How it is told apart: copying clips puts a short text on the system clipboard as a mark, and Ctrl + V looks at what the clipboard holds now. In a text field the three keys belong to the text |
| G | add marker at the playhead (again on the same frame: remove it) |
| Y | undo (Shift + Y redo); Ctrl + Z / Ctrl + Shift + Z also work |

Animation and creation

| Key | Action |
|-----|--------|
| F | add / update keyframe for the selected layer at the playhead |
| Shift + F | remove keyframe at playhead |
| Shift + K | metronome on or off: clicks on the beats while playing (needs a tempo), and one bar of them before a recording (stage 3, M11) |
| L | loop on or off: plays the open note clip, the selected clips or the stretch between the markers around the playhead over and over (stage 3, M11) |
| Shift + R | record sound: plays from the playhead and records the microphone; again, or pause, ends the take, which is laid on the timeline where it was played (stage 3, M13) |
| R in an audio track's header | recordings go onto this track, from the playhead on, with a bar of lead-in. Without one marked, the take goes onto the lowest audio track that is free from the playhead on, or a new one. Either way the track is named when the recording starts, and the take is drawn on it in red, with its level, while it is made |
| Double click on a track's name, or "Rename track" in its menu | type another name (Enter keeps it, Esc does not) |
| R | live record: plays; drag the selected clip's picture in the viewer and the motion is recorded as keyframes. R again or pause ends it |
| T | add a 3 s title at the playhead, on the topmost video track that is free there (a new track if none is) |
| B | whip-pan on the cuts of the selected clips (feedback F12.1): the cuts between two selected clips; if no two of them touch, the cut at the start of each, or at its end if nothing touches its start. If they all have it already, it is taken off. Another kind there is replaced. With nothing selected: the cut nearest the playhead |
| Shift + B | black on the same cuts as B; again removes it (feedback F11.1, F13.1). Hard by default: the picture cuts to black, six frames of it (three on each side of the cut), and black cuts to the next picture. "Fade" in the inspector sets the share of that time that fades instead (0 hard, 100 % a dip to black); the length goes from 2 to 60 frames. Dark, not transparent: tracks underneath do not show through |

In the piano roll (stage 3, M14; `PROPOSED`)

| Key | Action |
|-----|--------|
| A S D F G H J K L ; (by position) | with "Keyboard" on: the white keys from C; W E T Y U O P the black ones; Z and X an octave down and up. These keys are not shortcuts while it is on |
| Ctrl + click, Ctrl + drag | add a note to the selection or take it out; draw a box that selects what it touches |
| "Select" in its head | a drag on empty space draws the selecting box without Ctrl, and a click there makes no note |
| Right button | removes the note under it, and every note it is drawn over while held: one undo step |
| Ctrl + A, Ctrl + C, Ctrl + X, Ctrl + V | all notes; copy; cut; paste after the selected notes by the whole beats they span |
| Backspace / Delete | remove the selected notes |
| Wheel, Shift + wheel, Ctrl + wheel | up and down the keys; along the time; zoom along the time around the pointer |
| Alt + wheel, middle button drag | higher or lower rows; move the view both ways |
| Press or drag in the strip with the bar numbers | moves the playhead |

The roll goes on after the clip's end as far as the track is free (sixteen
bars at a time, darker). A note written or pasted there makes the clip
longer, to the end of its bar, in the same undo step.
| Esc | close the piano roll |

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
| The Assets button (top bar, beside the project's name) | hides the assets panel, or shows it again |
| "Files" and "Lyrics" at the top of the assets panel | the project's recordings, or its words: a text kept with the project (saved, undone and in checkpoints like the rest; put down when the typing pauses), with A- and A+ for the size of the letters, to read while recording |
| "Folder" above the assets; "Show in folder" in an asset's menu; "Open folder" in the export popover | the desktop's file manager at the project's assets folder, at the asset's file, at the exports folder |
| The Chain button | in place of the inspector: one audio track alone (the open piano roll's, or the selected clip's; another can be chosen in the panel), with what its sound goes through listed in the engine's order (source, low cut, noise reduction, tune, equaliser, de-esser, leveller, compressor, saturation, plugins, fader, sends, group, master) beside its strip. The stages before the plugins have two arrows each that move them up or down the chain (the order is part of the track's settings; default: the order above). A click on a stage's name (or on "Whole chain") opens its picture in a window over the editor that can be moved and closed, a second click closes it: the sound going into it (grey) and coming out of it (orange) moving past the playhead, 3.5 s behind and 0.4 s ahead, with a line below for how much the stage turns it up or down. The compressor's threshold is a yellow line in the picture that is pulled; low cut and equaliser have a curve with a handle each (double-click: back to nothing) and behind it the sound by pitch as it is heard, going in (grey) and coming out (orange). Looks and sizes `PROPOSED` |
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
