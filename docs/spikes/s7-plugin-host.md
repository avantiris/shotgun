# S7 Hosting VST3 plugins from Rust

Date: 2026-11-20. Code: `spikes/s7-plugin-host` (throwaway).

## Question

Kontakt 8 is a VST3 plugin (through yabridge it appears as a native Linux
VST3). Can Shotgun host VST3 itself, with the `vst3` crate's bindings
alone, or does it need a hosting library or another program?

## What was done

A 290-line program: loads a `.vst3` bundle (`Contents/x86_64-linux/*.so`),
calls `ModuleEntry` and `GetPluginFactory`, creates the first "Audio
Module Class", initialises it with a host object of our own
(`IHostApplication`), sets it up for 48 kHz, 32-bit float, blocks of 512,
activates its buses, and calls `process` 187 times (two seconds) with a
note-on at the start and a note-off after one second, handing the events
over through our own `IEventList`. Then reads and writes its state through
our own `IBStream`.

Command: `nix develop --command bash -c 'cd spikes/s7-plugin-host &&
CARGO_TARGET_DIR=/disk/shotgun-dev/target-s7 cargo run --release --
"<store path of surge-xt>/lib/vst3/Surge XT.vst3"'` (the plugin from
`nix build --inputs-from . nixpkgs#surge-XT`).

## Measured

| | Surge XT (instrument) | Surge XT Effects (effect) |
|---|---|---|
| Load and initialise | 154 ms | similar |
| Buses | 1 audio in, 3 audio out, 1 event in | 2 audio in, 1 audio out |
| Two seconds rendered in | 10 ms | 3 ms |
| Level while the note sounds | -21.9 dB | -19.7 dB (the test tone, passed) |
| Level after note-off | -42.5 dB, then silence | |
| State | 50 395 bytes, read and written back | 1 122 bytes |

Every call returned `kResultOk`. Both plugins keep their edit controller
in a class of its own (not the component itself).

## Not tried here

- The editor window (an X11 window handed to `IPlugView::attached`, and
  the `IRunLoop` a Linux plugin asks its host for).
- A yabridge-bridged plugin, Kontakt in particular: needs a Wine prefix
  with a plugin in it, which needs the user's Native Instruments login.
- Parameter automation, more than one output bus, sidechain inputs.

## Decision

Host VST3 ourselves with the `vst3` bindings (MIT/Apache, generated from
the SDK); no hosting library. Each plugin runs in a process of its own
(`shotgun-plugin`, crate `sg-plug`), talking to the engine over a pipe:
a plugin that crashes costs its track's sound, not the project, and its
editor window (X11) belongs to that process and not to the Wayland
window of the program. Blocks are processed on the disk thread like the
other track effects, ahead of playback, so no plugin code runs in the
realtime callback. CLAP and LV2 come after VST3.

## The bridge (2026-11-21)

Looked at with a throwaway Wine prefix (the Wine of ni-wine, inside a
virtual X server so that nothing appeared on the desktop) and the free
Dexed synthesiser's Windows VST3 in it, with a temporary `HOME`.

- `yabridgectl sync` printed that it set the plugin up with the loaders
  in `/etc/profiles/per-user/linus/lib`: the yabridge installed on the
  system (5.1.1), not the one of Shotgun's package (built against
  ni-wine's Wine). It finds them by going through `NIX_PROFILES`.
- The loader inside a bridged `.vst3` finds `libyabridge-vst3.so` the
  same way when the plugin is loaded, and that library looks for
  `yabridge-host.exe` on the `PATH`.
- So on a machine without a system yabridge neither would find anything,
  and on this one the wrong build would run against the prefix.

Fix: `sg_project::plugins::bridge` (for `yabridgectl`) and
`sg_plug::Client::start` (for the plugin's process) set `NIX_PROFILES`
to the yabridge found on the `PATH`. With that, `yabridgectl sync`
names the loaders of the bundled bridge.

**Not tried:** loading the bridged plugin and playing a note through it.
The run was stopped by the permission check of the session (it would
have executed a downloaded Windows program under Wine). The test is
there for when a prefix with a plugin exists:

```
SHOTGUN_TEST_BRIDGED=~/.vst3/yabridge/<Plugin>.vst3 \
  nix develop --command cargo test -p sg-plug --test host -- --ignored bridged --nocapture
```

It loads the plugin, plays a note, times 512-frame blocks through the
bridge, moves the state into a second instance and opens the window.

## Kontakt 8 through the bridge (2026-11-21, on the user's installation)

After the user had installed Native Access and Kontakt 8 through the
walkthrough, the test above was run on the real plugin
(`~/.vst3/yabridge/Kontakt 8.vst3`), its window kept on the hidden
workspace.

| | Result |
|---|---|
| Load and initialise | 10 to 21 s |
| Recognised as | instrument, with a window |
| 512 frames through the bridge | 80 to 100 us |
| State out and into a second instance | 8.7 kB, accepted |
| Its window | opens in 1 s, 1010 x 647, draws (a capture of it shows the browser with 892 presets) |
| Blocks, reset, tempo and state with the window open | all answer at once |

A new Kontakt has no instrument loaded, so a note is silent until one is
loaded in its window.

Two things had to be put right first:

- **Wine could load no graphics driver.** ni-wine pinned an older nixpkgs;
  its Wine had glibc 2.42, the system's Mesa drivers need 2.44. No OpenGL,
  no Vulkan, and Kontakt's window stopped at a message box, "Failed to
  initialize graphics backend for D3D11". While that box was up, asking the
  plugin for its state did not return (state is fetched on the plugin's
  window thread), which in the program would have stalled the sound.
  Fix: ni-wine follows this flake's nixpkgs (`flake.nix`); Wine is now
  11.21 with the system's glibc.
- **The plugin process inherited Shotgun's library path** (its own OpenGL
  and Vulkan loaders). Not the cause here, but wrong for other people's
  software: plugins are now started with the path from before the
  program's wrapper (`SHOTGUN_OUTER_LD_LIBRARY_PATH`).

What follows from changing the Wine build:

- Bridged plugins are copies of the bridge's loader; an old copy does not
  start with a new bridge ("Assertion `bridge' failed"). The program now
  sets them up again by itself when it finds old copies
  (`plugins::bridge_stale`).
- A Wine server of the old build that is still running for the prefix
  (Native Access leaves its licence service running) makes the new one
  fail at once ("terminate called without an active exception"). It has to
  be stopped once (`wineserver -k` with the prefix, or a log-out).

Still open: a call that needs the plugin's window thread (state, opening
the window) blocks every other call to that plugin while a modal box of
the plugin is up, because the host process answers one request at a time.
Processing sound on a thread of its own in `shotgun-plugin` would remove
that; not built.
