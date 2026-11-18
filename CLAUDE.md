# Shotgun

A fast, GPU-driven video editor for one user (Linus) on NixOS + Hyprland, with a
DAW planned inside the same app. Rust, wgpu/Vulkan, FFmpeg, PipeWire.

## Read before working

| File | When |
|------|------|
| `docs/spec/01-product.md` | always: what was agreed, non-goals, open points |
| `docs/spec/02-architecture.md` | before touching any crate: thread model, crate boundaries |
| `docs/spec/04-performance.md` | before any feature: budgets are hard requirements |
| `docs/spec/08-milestones.md` | to find what to build next and the definition of done |
| `docs/spec/03,05,06,07` | when working on bundle/git, colour, audio, UI/keys |
| `docs/spikes/*.md` | before choosing a crate or approach: measured decisions live here and override the specs where they differ |

Statements in specs marked `PROPOSED` are my design, not user decisions: fine
to improve, and the first candidates to change on user feedback. Statements
not marked are agreed with the user: do not change them without asking.

## Environment

- Everything runs in the Nix dev shell: `nix develop --command <cmd>` from the
  repo root. There is no system Rust toolchain.
- The home disk is nearly full. Build output is on `/disk/shotgun-dev/target`
  (set by the shell). Test media goes in `/disk/shotgun-dev/media`. Never put
  media, build output or caches in the repo or under `/home`.
- The user's real library is `/disk/shotgun`. Tests and experiments must set
  `SHOTGUN_LIBRARY` to a temp directory and never write to the real library.
- Flake files must be `git add`ed before `nix develop` sees them.
- The user works on this desktop while I run things. Automated app and spike
  runs must not take focus, the pointer or the clipboard: follow the "Do not
  disturb the user's desktop" rules in `docs/spikes/README.md`.
- Keyboard layout is Swiss German (QWERTZ). Bind keys by physical position.

## Commands

```
nix develop --command cargo build --workspace
nix develop --command cargo nextest run --workspace
nix develop --command cargo clippy --workspace --all-targets -- -D warnings
nix develop --command cargo fmt --all
nix develop --command cargo run -p sg-app --release
nix develop --command bash scripts/bench-m1.sh        # M1 budgets
nix develop --command bash scripts/bench-m2.sh        # M2: 1 GB drop usable
nix develop --command bash scripts/bench-m3.sh        # M3: scrub, play start, audio
nix develop --command bash scripts/bench-m4.sh        # M4: UI frame time, 10 to 500 000 clips
nix develop --command bash scripts/bench-m5.sh        # M5: three 4K60 layers playing
nix develop --command bash scripts/bench-m6.sh        # M6: export speed, outputs probed
nix develop --command bash scripts/check.sh           # fmt + clippy + tests; run before every commit
```

The binary is `shotgun` (`$CARGO_TARGET_DIR/release/shotgun --help`).
Automated runs: `SHOTGUN_LIBRARY=<temp dir> SHOTGUN_APP_ID=shotgun-dev` plus
`--script "new:Demo;import:/path/clip.mp4;asset:0;shot:/path/a.png;quit"`
(`--help` lists the steps; `open:` takes the project folder name). The script waits
for background jobs between steps, `shot` renders off-screen, and the
`shotgun-dev` window rules (see `docs/spikes/README.md`) keep the window on a
hidden workspace. Temporary libraries go in `/disk/shotgun-dev/tmp`.
Automated runs must also set `SHOTGUN_AUDIO=null` (or `sink:<a null sink>`):
never play test sound through the user's speakers. They must also set
`SHOTGUN_NO_CLIPBOARD=1`: exports of share copies and GIFs otherwise replace
the clipboard. `sg-bench export <project dir> <preset>` exports without the app.

## Rules that are easy to break

1. **The UI thread never blocks**: no file I/O, decoding, subprocesses, git or
   lock waits on it. It reads immutable snapshots and sends commands.
2. **The audio realtime thread never allocates, locks, logs or makes syscalls.**
3. **Preview and export use the same render code** (`sg-render`). A feature
   that only works in preview is not done.
4. **The timeline never references a path outside the project bundle.** Assets
   are referenced by id.
5. **Times are rational/integer** (frames, samples), never floating-point seconds
   in the model.
6. **Every model change is an operation with an inverse** going through the
   journal. No direct mutation, or undo and autosave break.
7. **Cost must not scale with project size** on interactive paths. If a loop
   visits all clips per frame, it is wrong.
8. `sg-core` has no I/O and no GPU; keep it pure so it stays fast to test.

## Working method

- Build milestone by milestone (`/milestone`). One milestone = one or more
  commits, each leaving the app runnable.
- Verify in the running app, not only with tests: launch it, exercise the
  feature, take a screenshot with `grim` where it helps. The app opens on the
  user's real desktop, so keep automated runs short and close them.
- Measure the budgets the milestone touches and append them to
  `docs/perf-log.md` with date, commit and command. Report misses as misses.
- Unit-test `sg-core` thoroughly (edit ops and their inverses, time maths,
  interval index). Property tests for op/inverse round trips.
- Rendering is tested by rendering known scenes to images and comparing with
  stored references; export is verified by probing the produced file.
- When a new technical risk appears, do a small spike under `spikes/` with a
  written result before building on an assumption.
- User feedback after a test round is recorded with `/feedback` before fixing.

## Conventions

- Crate prefix `sg-`. Errors: `thiserror` in libraries, `anyhow` only in
  `sg-app` and tools. No `unwrap()` outside tests except on proven invariants
  with a comment.
- `unsafe` only in FFI/GPU-import boundaries, each block with a `// SAFETY:` note.
- Comments explain why, not what. No commented-out code.
- External tools (`yt-dlp`, `git`, `gifski`, `wl-copy`) are called only from
  the job pool, with cancellation and captured stderr surfaced in the UI.
- Commit only when a milestone step is complete and verified.
