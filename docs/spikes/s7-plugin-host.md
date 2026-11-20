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
