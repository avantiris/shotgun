# Spikes

A spike answers a specific risky question with throwaway code and measured
numbers. Code lives in `spikes/<id>-<name>/` (own Cargo project, not part of
the product workspace). The result lives in `docs/spikes/<id>-<name>.md` and
ends with a **Decision** section. Specs are updated to match decisions.

Rules for spike work:

- Run everything through the dev shell: `nix develop --command <cmd>` from the repo root.
- Build output goes to `/disk/shotgun-dev/target-<id>` (set `CARGO_TARGET_DIR`);
  test media goes to `/disk/shotgun-dev/media`. Nothing large on the home disk
  (it has < 18 GB free) and nothing large in the repo.
- Report measured numbers with the command that produced them. If something
  could not be measured or did not work, say so plainly; do not estimate.
- Spike code is throwaway: no polish, but it must build and run as documented.
- **Do not disturb the user's desktop** (they are working on it):
  - Windows open without focus (Hyprland rule `noinitialfocus` plus
    `workspace <n> silent` for the spike's window class), are never raised, and
    close themselves. Remove runtime rules afterwards with
    `hyprctl keyword windowrule unset ...` for those rules only; do not run
    `hyprctl reload`.
  - Hyprland sends no frame callbacks to windows on hidden workspaces, so
    everything that does not measure frame pacing goes to a hidden dedicated
    workspace. Frame-pacing measurements run on a dedicated workspace on the
    **WQHD monitor (HDMI-A-1, 2560x1440@60)**, as the user asked. The FHD 144 Hz
    monitor (HDMI-A-2) may be used for occasional, short 144 Hz checks (the
    "no missed vblank at 144 Hz" budget), unfocused and self-closing: a single
    run when a milestone needs the number, never repeated loops or long
    sessions. The user must be able to keep working most of the time.
    Untested option to evaluate first: a Hyprland headless output
    (`hyprctl output create headless`) so no physical monitor is used at all.
  - Anything that injects pointer/keyboard input or writes the clipboard runs
    inside a nested or headless compositor with its own `WAYLAND_DISPLAY`,
    never on the real session. If that is not possible, do not run it: leave a
    test binary and instructions for the user instead.
  - Audio tests that do not need the real device play into a null sink.

| Id | Question | Result |
|----|----------|--------|
| S1 | Hardware decode to a wgpu texture: which path works on RADV (VAAPI DMA-BUF import, FFmpeg Vulkan decode, or download/upload), at what speed, and which FFmpeg Rust bindings build against FFmpeg 9? Seek latency of long-GOP vs intra proxy. | `s1-decode.md` |
| S2 | UI stack: egui + custom instanced wgpu timeline vs alternatives. Frame time with 50 000 clips at 144 Hz, zero idle redraw, physical-key input on the `ch` layout, Wayland file drag-and-drop and clipboard paste. | `s2-ui.md` |
| S3 | Audio: `pipewire-rs` vs `cpal` for a low-latency PipeWire node. Achievable quantum, callback jitter, xruns, reported latency. | `s3-audio.md` |
| S4 | Import/export tooling: yt-dlp, Giphy and Google Photos links; VFR detection; proxy codec choice; encoder choice for masters and size-targeted copies; GIF to Wayland clipboard. | `s4-io.md` |
| S5 | Versioning large media: git-lfs vs plain git vs manifest-only, by disk use and commit time. | `s5-git.md` |
