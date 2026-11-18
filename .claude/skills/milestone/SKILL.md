---
name: milestone
description: Implement the next Shotgun milestone (or the one named in the arguments, e.g. "M3") end to end, following the project's definition of done. Use when asked to continue building, build the next stage, or implement a specific milestone.
---

# Implement a milestone

Argument: optional milestone id (`M1`..`M10`). Without one, take the first
milestone in `docs/spec/08-milestones.md` that is not marked done.

## 1. Load context

- Read `docs/spec/08-milestones.md` for the scope.
- Read the specs that scope touches and every `docs/spikes/*.md` that covers
  the technology involved. Spike decisions override spec text.
- Read `docs/feedback/` for open items that belong to this milestone.
- Check `git log --oneline -15` and `git status` to see where the last session stopped.

## 2. Plan briefly

Write the steps as a short checklist in your reply (not a file). Each step
must leave the app building and runnable. If a step rests on something no
spike has verified, run a small spike first (`docs/spikes/README.md` rules).

## 3. Build, step by step

For each step:

1. Implement.
2. `cargo build`, `cargo clippy -- -D warnings`, `cargo nextest run` in the dev shell.
3. Exercise it in the running app (short run, then close it). Screenshot with
   `grim` when layout or rendering matters and look at the image.
4. Commit with a message that says what now works.

Keep to the rules in `CLAUDE.md` ("Rules that are easy to break").

## 4. Measure

Run the benchmarks for every budget in `docs/spec/04-performance.md` the
milestone touches. Append a dated entry to `docs/perf-log.md`: commit, command,
number, budget, pass/miss. If a budget is missed, try to fix it; if it is still
missed, say so in the log and the final report.

## 5. Close

- Tick the milestone in `docs/spec/08-milestones.md`.
- Update specs where the implementation deliberately differs, and say why.
- Final report to the user: what works now, how to try it (exact command and
  the 3-5 things worth trying), measured numbers, what is missing or known
  broken. If this milestone completes a stage, say that it is ready for a test
  round.
