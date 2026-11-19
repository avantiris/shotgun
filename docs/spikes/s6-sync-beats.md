# S6 Sync by camera sound, and beat detection

Date: 2026-11-19 (night run, no user present). Machine: the user's desktop,
load average 2 to 3.

## Question

M7 needs two things no earlier spike covered:

1. Find where a take belongs in the song from the sound its camera recorded,
   precisely enough to cut on (a frame at 60 fps is 16.7 ms; the aim was 2 ms).
2. Find the beats of the song for a grid to snap to.

Can both be done in-process, in pure Rust, fast enough to run as an import job,
and can the code tell when it does not know?

## Method

The code under test is the product code (`crates/sg-media/src/analysis.rs`);
the spike project includes it by path and runs it on synthetic material with
known truth (`crates/sg-media/src/analysis_testdata.rs`).

```
CARGO_TARGET_DIR=/disk/shotgun-dev/target-s6 nix develop --command \
  cargo run --release --manifest-path spikes/s6-sync-beats/Cargo.toml
```

- **Songs**: drums, bass, chords and a lead at 72 to 174 bpm, 180 to 200 s,
  arranged so that no two bars are alike (notes chosen at random; every
  eight bars the arrangement changes: full, no drums, no lead).
- **Camera recordings**: a stretch of the song as a phone in a room would
  record it: five echoes (23 to 190 ms), no bass and little treble
  (250 Hz to 4.5 kHz), room noise that swells and fades at 20, 6, 0 and -6 dB
  below the music, automatic gain and clipping, and a clock that differs
  from the song's by 0, +120 or -300 parts per million.
  Lengths 12, 45 and 150 s; one start position is 3.2 s *before* the song.
  108 recordings in all, plus 9 that do not belong to the song (other songs,
  some at the same tempo, and room noise alone).

### Sync, as built

1. *Propose.* The rise of the energy in 32 frequency bands, 100 times a
   second, for song and take; the bands are correlated one by one over
   every possible position (FFT) and the results added. The 12 best
   positions go on.
2. *Decide.* Song and take at 8 kHz without bass. Up to 40 half-second
   stretches of the take are correlated with the song around a candidate;
   the position and clock difference along which *all* stretches are most
   alike together wins. Its mean correlation is the "likeness". One stretch
   alone fits in many places (a held note repeats every few milliseconds,
   the same drum hit every bar); all at once fit only where the take belongs.
3. *Trust.* The answer is used without asking if the likeness is at least
   0.15 and at least 1.4 times that of the next best position.

Two earlier versions failed and are why it is built this way: correlating
one summed onset curve put most takes a whole number of bars off (the drums
dominate it), and choosing the position on which most single stretches agree
put takes 15 or 29 ms off (one period of the bass note).

### Beats, as built

Onset curve at 187 values per second; tempo from its autocorrelation with a
preference for 120 bpm; beats by dynamic programming (Ellis 2007). Then an
evenly spaced comb is slid over the curve in steps of a hundredth of a
percent of the tempo and a quarter frame of position; if it collects at
least 85 % of what the tracked beats collect, the song is "steady" and the
comb is the grid.

## Results

Sync, 108 recordings that belong to the song:

| | |
|---|---|
| Placed correctly | 100 |
| Largest error among those | 1.47 ms (typically 0.3 to 0.6 ms) |
| Placed wrongly | 8, **none of them trusted** (all failed the trust rule) |
| Correct but not trusted | 4 |
| Clock difference (takes of 45 s and more) | within 22 ppm of the truth |
| Lowest likeness of a correct match at -6 dB | 0.16 |
| Time per take | 0.44 s (12 s take) to 1.26 s (150 s take) against a 200 s song |

The 8 wrong ones: seven are the 12 s takes that start 3.2 s before the song
and so hold under 7 s of music; one is a 45 s take matched to another
passage with the same arrangement (likeness 0.22 against 0.22).

Recordings that do not belong to the song: highest likeness 0.28, but never
more than 0.04 ahead of the next position; none trusted.

Beats:

| True bpm | Found | Steady | Beats found / true | Median error | Worst error | Time |
|---|---|---|---|---|---|---|
| 72 | 72.00 | yes | 215 / 214 | 1.9 ms | 3.3 ms | 0.48 s |
| 96 | 96.00 | yes | 286 / 285 | 2.1 ms | 4.5 ms | 0.48 s |
| 110 | 110.00 | yes | 328 / 327 | 1.6 ms | 4.7 ms | 0.47 s |
| 128 | 128.00 | yes | 380 / 380 | 1.1 ms | 3.1 ms | 0.50 s |
| 140 | 140.00 | yes | 416 / 416 | 1.0 ms | 2.5 ms | 0.46 s |
| 174 | **87.00** | yes | 259 / 517 | 2.3 ms | 5.6 ms | 0.47 s |
| 87.5 | 87.50 | yes | 261 / 260 | 1.1 ms | 3.4 ms | 0.48 s |

174 bpm is found at half tempo: every found beat is a true beat, every
second true beat is missing.

### One check with real music (added after M9)

The soundtrack of a YouTube video (5 min 14 s, imported with "Sound only")
as the song, and three excerpts of it made into "camera takes" with ffmpeg
(three echoes, band-limited to 250 Hz to 4.5 kHz, pink noise mixed in,
limiter, AAC at 96 kbit/s), synced in the app with "Sync all takes":

| Take | Starts at | Noise amplitude | Result |
|------|-----------|-----------------|--------|
| 35 s | 47.25 s | 0.12 | placed at frame 2835 = 47.25 s, exact |
| 50 s | 121.6 s | 0.3 | placed at frame 7296 = 121.6 s, exact |
| 25 s | 200.0 s | 0.5 (louder than the music) | not placed: no clear match |

Beats of the same music: 123.6 bpm, reported as *not* steady (film-style
music without a drum track); the grid then follows the tracked beats. No
truth to compare with.

Still not a recording made by a microphone in a room.

## What this does not show

- **No real recording was used.** Everything above is synthetic. Real rooms,
  real phones (noise suppression, codecs), a band playing *along* to the
  song rather than the song from a speaker, and wind are untested. The first
  real shoot is the test; the trust rule exists so that a failure shows as
  "not sure" instead of a wrong cut.
- A take that plays the song from a speaker far away is late by the travel
  time of sound (3 ms per metre). That is physically in the recording; no
  correction is attempted.
- Takes with under about 10 s of the song in them.
- Songs whose tempo changes, songs without percussion, swing: the tracker
  handles them in principle (it falls back to the tracked beats when the
  comb does not fit) but no such material was made.
- Whether the first beat of the bar can be found (not attempted).

## Decision

- Build M7 on this code: `sg_media::analysis::find_offset` and `find_beats`,
  run on the job pool from the conformed 48 kHz audio the import already
  makes. Dependency: `rustfft` 6 (pure Rust).
- A take is placed automatically only when the match is trusted; otherwise
  the user is told and nothing is placed.
- A take is laid so that its *middle* is exact; with the clock differences
  measured on phones (tens to a few hundred ppm) a 3 minute take is then at
  most 27 ms out at its ends for 300 ppm. The measured difference is shown.
  Correcting it (resampling the take's video timing) is left for M10 (speed
  ramps) and listed as open.
- Tempo octave (half or double) is a one-click choice for the user, not
  something the detector tries to guess harder.
- The beat grid is the comb when the song is steady, the tracked beats
  otherwise.
