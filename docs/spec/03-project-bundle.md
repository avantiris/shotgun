# 03 Project bundle, versioning, undo

## Library

`/disk/shotgun/` is the library (override only via `SHOTGUN_LIBRARY`, for tests).
Every direct subfolder containing `project.toml` is a project. The app opens
into a grid of these, sorted by last opened, with a poster frame each.

## Bundle layout (required, not configurable)

```
/disk/shotgun/<project-slug>/
  project.toml            name, id, format version, default sequence, settings
  assets/
    video/                originals, never modified after import
    audio/
    image/                stills and GIFs
    font/
    lut/
    index/<asset-id>.toml one small metadata file per asset (see below)
  sequences/<slug>.sg     one file per timeline
  scenarios/<slug>.toml   colour scenario: members' match data, hero, look chain
  looks/<slug>.toml       saved look-chain presets
  exports/                rendered outputs
  .shotgun/               local-only (git-ignored); only cache/ is regenerable
    journal/              edit journal (undo/redo history)
    cache/proxy/          proxies
    cache/conform/        conformed mezzanine + conformed audio
    cache/thumb/          thumbnail strips
    cache/peaks/          audio peak files
    cache/render/         background render cache
    trash/                removed assets, kept until garbage collection
  .gitignore              written by the app
  .gitattributes          written by the app
```

- "Zip the folder" yields the complete project. `.shotgun/cache` may be omitted
  from a zip; it is rebuilt on open. Originals are always inside `assets/`.
- The UI shows this structure in simplified form (Assets by kind, Sequences,
  Scenarios, Looks, Exports). No user-created folders. Assets are organised
  with labels/tags stored in the index, not with directories.

## Assets

- Asset id = content hash (BLAKE3) prefix. Importing the same file twice yields
  the same asset.
- File name = `<label-slug>-<id8>.<ext>`. The label is editable; renaming
  changes only the index entry, not the file (files are immutable).
- `index/<id>.toml` holds: label, tags, source (original path or URL, download
  date), probe data (streams, codec, resolution, frame rate, duration, colour
  tags, rotation), whether a conform was needed, and analysis results (colour
  statistics, beat/tempo for audio, sync offsets).
- Import pipeline (job pool, cancellable, progress shown on the asset tile):
  1. fetch/copy into `assets/<kind>/` (temp name, then atomic rename)
  2. hash, probe, write index entry -> **asset is usable from here**
  3. thumbnails + audio peaks + conformed audio
  4. conform if required (VFR, GIF, odd formats)
  5. proxy
  6. analysis (colour statistics, audio fingerprint for sync)
  The asset can be dropped on the timeline after step 2; preview quality
  improves as later steps finish.

### Import and export tooling (decided by spike S4, `docs/spikes/s4-io.md`)

Copy exact command lines from the spike document; summary:

- **Links:** yt-dlp for YouTube and other sites (default format selection,
  `-S "hdr:SDR"`, `--progress-template "download:%(progress)j"`,
  `--write-info-json` for the label). Giphy: fetch
  `https://media.giphy.com/media/<id>/source.gif` directly (yt-dlp returns the
  worst rendition). Google Photos share links: plain HTTP with `=d` returns
  the original photo; videos are unverified. Direct URLs: HTTP with resume.
- **VFR detection:** full packet-timestamp scan (about 2.7 s per 3.5 GB warm);
  frame-rate field comparison is blind for MKV/WebM.
- **Proxy:** 960x540 H.264 all-intra, `libx264 -preset veryfast -tune
  fastdecode -crf 20 -g 1` (about 95 MB per minute at 60 fps).
- **Conform:** `hevc_vaapi -rc_mode CQP -qp 14 -g 15 -bf 0` (0.56 GB/min at
  4K60, VMAF 99.9); software fallback x264 GOP 15 crf 14. Sources with alpha:
  Ut Video `gbrap` in MKV.
- **Master export default:** HEVC Main10 `hevc_vaapi` CQP 20 (21.6 Mbit/s,
  VMAF 99.55, 3.2x realtime). `PROPOSED` until the user compares it by eye
  with `libx264 -preset slow -crf 16` on real footage.
  **Never pass `-tag:v hvc1` to `hevc_vaapi`** (undecodable output on this
  GPU): encode as `hev1`, then remux with `-c copy -tag:v hvc1`. Every export
  is decode-checked before success is reported.
- **Size-targeted copy:** audio first, two-pass x264 on the remaining budget,
  one corrective pass; lands at about 99 % of target, never over.
- **GIF:** ffmpeg y4m piped to `gifski --y4m-color-override bt709 --quality 90`.
- **Clipboard:** the app owns the Wayland selection itself (`wl-clipboard-rs`
  `copy_multi`) and offers `text/uri-list`, `x-special/gnome-copied-files`,
  the raw bytes (`image/gif` / `video/mp4`) and `text/plain` together;
  `wl-copy` can offer only one type. Real paste targets are untested.

## Sequences

Text-based, diff-friendly, stable ordering so git diffs are meaningful.
Clips reference assets by id only. Times are stored as rational numbers
(frames at the sequence rate for video edits, samples for audio), never floats.

## Autosave and undo/redo

- Every edit is an **operation** (`sg-core`) with an inverse. Operations are
  appended to `.shotgun/journal/` immediately (target < 1 ms, fsync batched
  every ~1 s). There is no "save" action.
- Undo/redo walks the journal. History survives restart and is unlimited
  (journal is compacted behind a retained horizon, proposed: 30 days).
- Coalescing: continuous gestures (dragging, live keyframe recording, knob
  turns) become one undo step.
- Sequence/scenario files are rewritten from the snapshot on a debounce
  (proposed: 2 s idle, at most every 10 s while editing), atomically.

## Git

- `git init` on project creation. The app commits automatically:
  on close, before/after export, after an import batch, and every N minutes
  while there are changes (proposed: 5). Commit messages are generated from the
  journal ("split clip, 3 trims, add title").
- Named checkpoints: the user can name the current state (a git tag); the
  history view shows checkpoints and auto-commits and can restore any of them
  (restore = new commit, never a destructive reset).
- Large media in git, decided by spike S5 (`docs/spikes/s5-git.md`): media
  files under `assets/` are **git-ignored**; git tracks `assets/index/*.toml`,
  which carry the BLAKE3 content hash. Removed media moves to
  `.shotgun/trash/` so restoring an older state still finds its files.
  Measured on 3 GB of media: 1.00x disk use (plain git and git-lfs both 2.0x,
  `/disk` is ext4 without reflinks), `git add` 0.04 s (43-148 s for the
  others), text-only commit 12 ms idle and 29-43 ms under load, `git status`
  9 ms. BLAKE3 hashes at 3.6-16 GB/s.
- Consequences to implement: on open, verify that every indexed asset exists
  (by size/mtime; full hash on demand) and show missing ones clearly; the
  trash is **not** regenerable, so of `.shotgun/` only `cache/` may be dropped
  from a zip, and trash is emptied only by an explicit, confirmed clean-up.
- Remote later: a tested script (`spikes/s5-git/c_push_lfs.sh`) builds a
  commit with LFS pointer files, hardlinks media into `.git/lfs/objects` and
  pushes; verified against a local remote only.
- Remote-ready means: nothing in the layout prevents `git remote add` + push
  later. No remote UI in stage 1 or 2.
