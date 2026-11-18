# S5 Versioning large media: plain git vs git-lfs vs manifest-only

Question: the project bundle is a git repo with small text files plus large immutable
media originals (multi-GB). Which storage model satisfies all of:

1. no doubling of disk use for originals,
2. automatic commits stay under 200 ms,
3. a remote can be added later,
4. zipping the folder yields the complete project.

Status: measured on 2026-11-18. Everything below is measured unless marked *not tested*.

## Environment

| Item | Value |
|------|-------|
| Disk | `/disk` = `/dev/sda1`, ext4 (`rw,relatime`), Samsung SSD 860 EVO 500GB (SATA) |
| Reflinks | **not supported**: `cp --reflink=always a b` -> `failed to clone ... Operation not supported` |
| CPU | AMD Ryzen 7 3800X (8 cores / 16 threads, `sha_ni`, `avx2`) |
| git / git-lfs | git 2.56.0, git-lfs 3.8.1 |
| b3sum | 1.8.7, from `nix shell nixpkgs#b3sum` (not added to the flake) |

**Load caveat.** Other spike agents were compiling Rust and encoding video on the same
machine during the main runs (1-minute load average 20 to 30 on 16 threads). Those
absolute times are pessimistic; the `load1` value is given with each run. The
latency-critical operations and the hash speeds were measured again at the end on a
mostly idle machine (sections "Recheck" and "Hashing speed"). Both sets are reported
because the app commits while it is itself busy (import, proxy generation, export).

## Test data

```
/disk/shotgun-dev/s5/raw/media/   cam-a.mp4 1500 MiB, cam-b.mp4 900 MiB, song.wav 400 MiB, broll.mov 200 MiB
                                  = 3 145 728 000 bytes, from /dev/urandom (incompressible, like real video)
/disk/shotgun-dev/s5/raw/text/    200 TOML-like files, 515 527 bytes in total
```

Generated with `dd if=/dev/urandom of=... bs=1M count=N`; the text files by a shell loop
(see `spikes/s5-git/bench.sh` header and the commands in this document).

## Variants

| Id | Model | Setup |
|----|-------|-------|
| a  | plain git objects, default config | `git init` |
| a0 | plain git objects, no compression | `git config core.compression 0; git config core.bigFileThreshold 1m` |
| b  | git-lfs, local storage | `git lfs install --local`; `.gitattributes`: `assets/video/** filter=lfs diff=lfs merge=lfs -text` (same for `assets/audio/**`) |
| c  | manifest-only | `.gitignore`: `.shotgun/`, `assets/video/`, `assets/audio/`, `assets/image/`; git tracks `assets/index/<id>.toml` containing the BLAKE3 hash, size and file name; removal = `mv` to `.shotgun/trash/` |

All variants use the bundle layout from `docs/spec/03-project-bundle.md`
(`assets/video`, `assets/audio`, `assets/index`, `sequences`, `.shotgun/`).

## Commands

```
nix develop --command bash spikes/s5-git/bench.sh a      # also: a0, b, c (c needs b3sum on PATH)
nix develop --command bash spikes/s5-git/recheck.sh      # latency re-measurement on the 4 repos
nix develop --command bash spikes/s5-git/c_push_lfs.sh   # variant c -> remote with LFS pointers
```

`bench.sh` per variant: init repo, commit 200 text files, copy the 4 media files into
`assets/`, `git add -A` + `git commit` (timed separately), 10 automatic commits touching 3
text files each (`git add -A; git commit`), 10 commits of one file by explicit path
(`git commit -m .. -- <path>`), 10x `git status --porcelain`, `touch` of the 1.5 GB file
followed by `git status`, removal of one asset + commit, restore of that asset from
history, and for a/a0 `git gc`. User/system git config is disabled
(`GIT_CONFIG_GLOBAL=/dev/null`).

Raw outputs: `/disk/shotgun-dev/s5/result-{a,a0,b,c}.txt`, `result-c-push.txt`.

## Results

### Import of 3.0 GB media (4 files)

| Variant | `git add -A` | `git commit` | extra step | load1 |
|---------|-------------:|-------------:|------------|------:|
| a  plain            | **147.6 s** | 0.021 s | - | 23.2 |
| a0 plain, no zlib   | 47.4 s | 0.038 s | - | 25.3 |
| b  git-lfs          | 42.9 s | 0.130 s | - | 24.8 |
| c  manifest-only    | 0.042 s | 0.045 s | BLAKE3 of the 4 files + writing 4 index files: 6.5 s (disk-bound, files not fully in page cache) | 28.3 |

Copying the 3.0 GB into `assets/` itself took 5.0 to 12.4 s (`cp`, same in all variants).
In variant c the hash is needed anyway (the asset id is the BLAKE3 hash), so the 6.5 s is
not a cost specific to c; a/a0/b pay it **in addition** to the `git add` time.

### Disk use (raw media = 3 145 728 000 bytes)

| Variant | worktree media | `.git` | whole project | factor vs raw |
|---------|---------------:|-------:|--------------:|--------------:|
| a  plain          | 3 145 728 000 | 3 181 190 030 | 6 327 431 010 | **2.01x** |
| a  after `git gc` (62.8 s) | 3 145 728 000 | 3 146 925 465 | - | 2.00x |
| a0 plain, no zlib | 3 145 728 000 | 3 146 782 140 | 6 293 023 120 | **2.00x** |
| b  git-lfs        | 3 145 728 000 | 3 146 019 840 | 6 292 260 924 | **2.00x** |
| c  manifest-only  | 3 145 728 000 | 292 568 | 3 146 534 090 | **1.00x** |

- `git lfs dedup` on this filesystem: `This system does not support de-duplication.`
- After removing the 200 MB asset and committing: a 6 117 856 626, a0 6 083 541 916,
  b 6 082 687 249 bytes (history keeps the copy; only the worktree copy goes away).
  c: 3 146 678 445 bytes (the file sits in `.shotgun/trash/` until garbage collection).
- Hack tested for b: replacing each `.git/lfs/objects/xx/yy/<sha256>` by a hardlink to the
  worktree file (`ln -f assets/video/x.mp4 .git/lfs/objects/..`) makes worktree and LFS
  store share inodes; `git status` stays clean and `git lfs fsck` reports `Git LFS fsck OK`.
  This is not an LFS feature, breaks as soon as LFS rewrites a worktree file (checkout),
  and a zip of the folder would still contain every file twice (zip does not keep
  hardlinks). Not recommended.

### Latency of automatic commits and status (first run, n=10)

| Variant | `git add -A` + `git commit` (3 text files changed) | `git commit -- <one path>` | `git status --porcelain` | `git status` after `touch` of the 1.5 GB file | load1 |
|---------|---:|---:|---:|---:|---:|
| a  | median 23.2 ms (add 8.5 + commit 14.7), max 37.4 | median 16.2, max 27.5 | median 6.5 ms | **6.0 s**, then 6 ms | 23 |
| a0 | median 45.2 ms (9.8 + 35.3), max 51.3 | median 25.4, max 44.2 | median 9.1 ms | **7.8 s**, then 14 ms | 25 |
| b  | median **189.8 ms** (16.4 + 173.4), max **216.4** | median 155.1, max **250.9** | median 11.8 ms | **26.2 s**, then 17 ms | 25 |
| c  | median 29.3 ms (6.2 + 23.1), max 61.6 | median 20.6, max 41.7 | median 9.5 ms | 9 ms | 28 |

- The `touch` test models an mtime change without content change (project copied,
  unzipped, restored from backup). Variants a/a0/b then re-read the file on the next
  `git status` (b pipes it through the LFS clean filter). Variant c never reads media.
- b: the commit cost is the LFS `post-commit` hook (starts the `git-lfs` Go binary). With
  hooks disabled (`git -c core.hooksPath=/dev/null commit ...`) commit by path took 11.4
  to 25.0 ms (n=7) versus 73.8 to 116.4 ms with hooks in the same minute (load1 20).

### Recheck: latency for all four repos back to back

`spikes/s5-git/recheck.sh`, n=20 per cell, all four repos in one run, load1 = 31.2 at
start (the machine was busier than during the first run):

| Variant | `git add -A` + `git commit` (3 text files) | `git commit -- <one path>` | `git status --porcelain` |
|---------|---|---|---|
| a  | median 37.2 ms, p95 66.2, **max 1359** | median 23.9, p95 44.5, max 51.6 | median 14.7, p95 27.3, max 31.0 |
| a0 | median 31.6 ms, p95 114.3, **max 3481** | median 18.7, p95 44.7, max 60.1 | median 9.8, p95 21.7, max 40.3 |
| b  | median **207.6** ms, p95 **299.4**, max **458.8** | median 185.5, p95 255.2, max 285.2 | median 12.6, p95 33.5, max 44.6 |
| c  | median 42.7 ms, p95 68.2, max 124.8 | median 21.3, p95 44.8, max 60.0 | median 7.9, p95 18.5, max 18.7 |

- b exceeds 200 ms in the median under this load; a, a0 and c stay under 70 ms at p95
  apart from the outliers explained below.
- The single 1.4 s (a) and 3.5 s (a0) outliers were the first `git add -A` of the run.
  Likely cause (not verified): the 200 MB file restored by `git revert` earlier had an
  mtime equal to the index timestamp ("racily clean" entry), so git re-hashed it once.
  Variant c cannot have such outliers because git never stats or reads media.

Same script again on a mostly idle machine (other agents finished; load1 = 10.9 and
falling, nothing else CPU-bound running), n=20:

| Variant | `git add -A` + `git commit` (3 text files) | `git commit -- <one path>` | `git status --porcelain` |
|---------|---|---|---|
| a  | median 12.1 ms, p95 15.9, max 35.8 | median 8.0, p95 10.5, max 12.2 | median 4.3, p95 5.0, max 5.0 |
| a0 | median 11.3 ms, p95 15.5, max 18.4 | median 7.4, p95 11.3, max 11.7 | median 3.3, p95 5.5, max 5.5 |
| b  | median **45.4** ms, p95 64.3, max 105.0 | median 37.5, p95 40.5, max 41.0 | median 3.2, p95 4.8, max 5.0 |
| c  | median 11.6 ms, p95 15.9, max 30.9 | median 7.5, p95 10.5, max 18.9 | median 3.1, p95 4.9, max 5.1 |

- On an idle machine every variant is inside the 200 ms budget. git-lfs costs 4x plain
  git per commit (45 ms vs 12 ms) and is the only variant that crossed 200 ms when the
  machine was saturated.

### Remove and restore an asset

| Variant | remove + commit | restore from history | restored file identical |
|---------|---------------:|---------------------:|-------------------------|
| a  | 0.013 s (`git rm` + commit) | 0.958 s (`git revert`, writes 200 MB from the object store) | yes (`cmp`) |
| a0 | 0.051 s | 1.527 s | yes |
| b  | 0.260 s | 2.978 s | yes |
| c  | 0.055 s (`mv` to `.shotgun/trash/` + `rm` index file + commit) | 0.043 s (`git revert` + `mv` back from trash) | yes |

### Hashing speed, 1.5 GB file (1 572 864 000 bytes), page cache warm, median of 5

| Command | Idle machine (load1 11.9, falling) | Busy machine (load1 24.7) |
|---------|-----------------------------------:|--------------------------:|
| `b3sum cam-a.mp4` (mmap, all 16 threads) | **0.097 s = 16 137 MB/s** | 0.438 s = 3591 MB/s |
| `b3sum --num-threads 1 cam-a.mp4` | 0.445 s = 3533 MB/s | 1.115 s = 1410 MB/s |
| `b3sum --no-mmap cam-a.mp4` (single thread) | not rerun | 0.962 s = 1635 MB/s |
| `sha256sum cam-a.mp4` (coreutils, SHA-NI) | 0.921 s = 1708 MB/s | 1.858 s = 846 MB/s |
| `openssl dgst -sha256 cam-a.mp4` | not rerun | 2.376 s = 662 MB/s |
| `sha1sum cam-a.mp4` | 0.868 s = 1813 MB/s | 2.041 s = 771 MB/s |
| `cat cam-a.mp4 > /dev/null` | not rerun | 0.123 s = 12 807 MB/s |

BLAKE3 with all threads is 9.4x faster than SHA-256 on the idle machine (4.2x when busy);
single-threaded it is 2.1x faster. Min/max over the 5 idle runs: b3sum 0.094 to 0.104 s,
sha256sum 0.914 to 0.923 s. For a file that is not in the page cache the SATA SSD (about
500 MB/s sequential) is the limit for either hash: hashing the 3.0 GB right after the
copy took 6.5 s with b3sum.

### Variant c with a remote (prototype, tested end to end on a local `file://` remote)

`spikes/s5-git/c_push_lfs.sh` does, on the variant-c repo:

1. Build a temporary index from `HEAD` (`GIT_INDEX_FILE=.git/shotgun-export-index git read-tree HEAD`).
2. For every media file: compute SHA-256, write a git-lfs pointer blob
   (`version https://git-lfs.github.com/spec/v1` / `oid sha256:<hex>` / `size <n>`) with
   `git hash-object -w --stdin`, add it at the media path with
   `git update-index --add --cacheinfo 100644,<blob>,<path>`, and hardlink the media file
   to `.git/lfs/objects/<aa>/<bb>/<sha256>` (same filesystem, no extra disk use).
3. Add a `.gitattributes` with the LFS filter lines and a `.gitignore` without the media
   directories to that index, `git write-tree`, `git commit-tree <tree> -p HEAD`,
   `git update-ref refs/shotgun/export <commit>`.
4. `git lfs push origin refs/shotgun/export`, then
   `git push origin refs/shotgun/export:refs/heads/main`; remove the hardlinks.

Measured:

| Step | Time |
|------|-----:|
| SHA-256 of 3.0 GB + building the export commit | 40.0 s (files cold, load1 about 20; pure SHA-256 hashing is 0.9 to 1.9 s per 1.5 GB when cached) |
| `git lfs push` + `git push` to `file:///disk/shotgun-dev/s5/remote-c.git` | 0.53 s (git-lfs hardlinked the objects into the local remote; a network remote uploads 3 GB) |
| Project disk use while the hardlinks exist | 3 075 220 KiB (unchanged, 1.00x) |
| `git clone` of the remote + `git lfs install --local` + `git lfs pull` | 0.13 s + 46.3 s |
| Clone content | all 4 media files byte-identical (`cmp`), text history present |
| Local repo after the push | `git status --porcelain` empty, working branch untouched |

So a standard git + LFS remote (GitHub, Gitea, any LFS server) can receive a complete
project from a manifest-only repo. The clone is an ordinary LFS repo (and has the 2x disk
use of variant b). Not tested: a network LFS server, and mapping the *whole* history (the
prototype exports one commit on top of `HEAD`; exporting every commit works the same way
because each commit's `assets/index/*.toml` lists exactly which media files it needs, as
long as removed files are still in `.shotgun/trash/`).

Alternative, *not tested*: git-annex (`nix shell nixpkgs#git-annex`). It is the existing
tool for exactly this model (content-addressed files outside git, symlinks or pointer
files in git, many remote types), but it replaces worktree files by symlinks/pointers in
its default modes, which conflicts with "zip the folder and get the project" unless
unlocked mode is used, and adds a Haskell runtime dependency. Not evaluated further.

## What failed or surprised

- **git-lfs is the only variant that broke the 200 ms budget**: median 190 to 208 ms, max
  216 to 459 ms for a commit that touches only text while the machine was busy (45 ms
  median, 105 ms max when idle). The cost is the `post-commit` hook.
- **Plain git `git add` of 3 GB took 148 s** (zlib on incompressible data, single thread).
  With `core.compression 0` it still took 47 s, and git still stores a full second copy.
- `git gc` on the plain repo took 62.8 s (a) / 42.1 s (a0) and did not reduce size.
  Auto-gc would trigger this in the background at some point.
- `git status` after an mtime change re-reads media in a/a0/b: 6 to 26 s for one 1.5 GB file.
- `git lfs pointer --file=<f>` left 3 GB of temp files in `.git/lfs/tmp` during my hardlink
  experiment (removed by `git lfs prune`). One more reason not to script around LFS.
- In the first version of `c_push_lfs.sh` the clone contained pointer files instead of
  media because the clone had no LFS filter configured (`GIT_CONFIG_GLOBAL=/dev/null`);
  `git lfs install --local && git lfs pull` fixed it. A user cloning needs git-lfs installed.

## Requirement check

| Requirement | a / a0 plain git | b git-lfs | c manifest-only |
|-------------|------------------|-----------|-----------------|
| No doubling of disk use | **fails** (2.0x; no reflinks on ext4) | **fails** (2.0x; `git lfs dedup` unsupported) | passes (1.00x) |
| Auto commit < 200 ms | idle: passes (12 ms median, 36 ms max). Busy: 23 to 45 ms median, but one 1.4 s and one 3.5 s outlier | idle: passes (45 ms median, 105 ms max). Busy: **fails** (190 to 208 ms median, up to 459 ms) | idle: passes (12 ms median, 31 ms max). Busy: passes (29 to 43 ms median, 125 ms max) |
| Import cost on top of copy + hash | 47 to 148 s per 3 GB | 43 s per 3 GB | 0.09 s |
| Remote later | passes natively (but pushes multi-GB packs; hosts reject files > 100 MB) | passes natively | passes via export step (prototype above) |
| Zip = complete project | passes, but zip is 2x the size | passes, but zip is 2x the size | passes, 1x; history of *removed* media only if `.shotgun/trash/` is included |

## What variant c loses compared with a/b

- **Git alone no longer guarantees that media exists.** A commit references media by
  hash; if a file is deleted behind the app's back, git cannot bring it back. The app
  must verify on open (existence + size from the index; full BLAKE3 only on demand, 0.1 to
  0.4 s per 1.5 GB cached, disk-bound otherwise) and report missing assets.
- **History restore of removed media depends on `.shotgun/trash/`.** Once the trash is
  garbage-collected, old commits that used the asset can only be restored without it.
  GC policy must therefore be "keep while any commit inside the retained history horizon
  references the hash" (reachability can be computed from `assets/index/*.toml` in old
  commits: `git log --diff-filter=D --name-only -- assets/index/`).
- **`git clone` / `git push` do not carry media** without the export step. A plain
  `git clone` of the bundle yields text only.
- **No git-level integrity check for media** (`git fsck` does not cover them). The BLAKE3
  hash in the index file replaces it.
- `.shotgun/` is described as "regenerable or local-only" in the bundle spec, but the
  trash is neither once history restore depends on it. See Decision.

## Decision

**Variant (c): media is git-ignored; git tracks one `assets/index/<id>.toml` per asset
containing the content hash.** It is the only variant that meets the disk requirement on
this machine (ext4, no reflinks), its `git add` at import is 3 orders of magnitude
faster (0.04 s vs 43 to 148 s for 3 GB), and its auto-commit latency equals plain git
and stays under 200 ms even on a saturated machine.

Concrete settings for the implementer:

`.gitignore` (written by the app):

```
.shotgun/
assets/video/
assets/audio/
assets/image/
```

Rules:

- Ignore by directory, not by extension. `assets/font/` and `assets/lut/` are small and
  can stay tracked by git directly; `assets/index/` is always tracked. `exports/` should
  be ignored as well (rendered outputs are large and regenerable); this was not part of
  the measurement, it follows from the same disk argument.
- `assets/index/<id>.toml` must contain at least: `blake3` (full 64-hex hash), `size`
  (bytes), `file` (relative path). Recommended in addition: `sha256`, filled lazily (at
  first export/push, 0.9 s per 1.5 GB idle), so LFS pointers can be generated without
  re-hashing.
- Hash with BLAKE3 multi-threaded over an mmap (what `b3sum` does; the `blake3` crate
  offers the same through its `rayon` and `mmap` features, not tested here): 16.1 GB/s
  idle and 3.6 GB/s under load, versus 1.7 and 0.85 GB/s for SHA-256.
- Removal: `rename()` the file into `.shotgun/trash/<id8>-<name>` (same filesystem,
  instant), delete the index file, commit. Restore: `git revert`/checkout of the index
  file + `rename()` back. Measured 43 to 55 ms including the commit.
- Trash GC: delete a trashed file only when no commit newer than the history horizon
  contains its index file.
- Auto-commit command: `git add -A && git commit -q -m <msg>` (12 ms median idle; 29 to
  43 ms median and 125 ms worst of 30 runs at load 28 to 31) or
  `git commit -q -m <msg> -- <paths>` (8 ms idle, 21 ms median and 60 ms worst under
  load). Both are below 200 ms. Do **not** install git-lfs hooks in project repos.
- Set per repo at `git init`: `git config gc.auto 0` is *not* needed for c (the object
  store stays tiny: 292 568 bytes after 24 commits of 200 text files).
- Zip: include `.shotgun/trash/` if the history of removed media should survive; exclude
  `.shotgun/cache/`. Spec change needed: the trash is local state that is **not**
  regenerable; either move it out of `.shotgun/` (e.g. `assets/.trash/`) or state the
  exception in `03-project-bundle.md`.
- Remote later: implement "export with LFS pointers" as in `spikes/s5-git/c_push_lfs.sh`
  (pointer blobs via plumbing into a separate ref, hardlink objects into
  `.git/lfs/objects`, `git lfs push`, `git push`, unlink). Nothing in the layout has to
  change for that. Needs a human check with a real network LFS remote before it is
  promised to the user.
