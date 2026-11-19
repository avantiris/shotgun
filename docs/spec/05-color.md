# 05 Colour (stage 2)

The user's mental model comes from audio: first a compressor that brings every
take to the same level, then a vocal chain that shapes the sound. Colour works
the same way here.

## Scenario

A scenario is a named group of clips that should look like one place and time
(one "set-up" in a music video), regardless of camera, angle or take.

- A clip belongs to at most one scenario. Clips cut from the same asset
  inherit the asset's scenario by default.
- Scenarios are assigned by selecting clips/assets and pressing one key, shown
  as a coloured stripe on clips in the timeline.
- Stored in `scenarios/<slug>.toml`.

## Signal flow per clip

```
decode -> input transform -> MATCH (per clip) -> trim (per clip, small)
       -> LOOK CHAIN (per scenario) -> compositing -> output transform
```

Everything is computed in scene-linear floating point (RGBA16F). 8-bit sources
are promoted to float, so stacked adjustments do not band; output is dithered.

### Input transform

Uses the stream's colour tags (primaries, transfer, matrix, range) to get to
the working space. Phone HDR (HLG / Dolby Vision profile 8) is tone-mapped by
a fixed, good default. Untagged files assume Rec.709. The design leaves a slot
for camera log transforms later.

### Match ("compressor")

Goal: after this stage, all clips of a scenario have the same exposure, white
balance, contrast and saturation, so the look chain can treat them as one.

- At import (and on demand), each asset is analysed on sampled frames:
  luminance percentiles, white-balance estimate, chroma statistics in a
  perceptual space (Oklab), skin-tone cluster if present.
- Target = the hero clip's statistics if a hero is marked, otherwise a robust
  average of the scenario's members.
- Per clip, a small **parametric** correction is solved towards the target:
  exposure gain, white-balance gains, contrast around a pivot, saturation.
  Parametric (not per-pixel histogram mapping) so it cannot produce artefacts
  and stays stable over time.
- **Strength** 0-100 % (the "ratio"). Per-clip values are visible and can be
  nudged by hand; a clip can be excluded.
- Clips whose lighting changes within the take can be analysed per cut-range
  instead of per asset.

`OPEN`: whether parametric matching is enough on the user's real footage, or a
stronger statistical colour transfer is needed as an option. Decided in stage 2
with footage from the user.

### Look chain ("vocal chain")

A free, reorderable list of modules per scenario. Each module has 2-4 knobs, a
bypass and a mix amount. Initial modules:

| Module | Knobs |
|--------|-------|
| Balance | temperature, tint, exposure |
| Tone | contrast, highlights roll-off, shadows lift, pivot |
| Colour | saturation, vibrance (protects skin), hue shift of one chosen range |
| Split tone | shadow colour, highlight colour, balance |
| Film look / LUT | choose look or `.cube` from `assets/lut`, amount |
| Glow / halation | threshold, radius, amount, tint |
| Grain | amount, size, roughness |
| Vignette | amount, size, softness |
| Sharpen / soften | amount, radius |

- Chains can be saved to `looks/` and applied to other scenarios or projects.
- A/B: one key toggles the whole chain, another solos the match stage.
- The chain is compiled into as few shader passes as possible; pure colour
  modules fuse into one pass, spatial modules (glow, grain, sharpen) get their own.

## Whip-pan transition

Placed on a cut between two clips that have handles (spare frames past the cut).

- Duration 2-8 frames (default 4), centred on the cut by default.
- Direction (angle), default left/right alternating with the cut.
- Outgoing clip accelerates out, incoming decelerates in; easing is adjustable.
- Directional motion blur proportional to per-frame velocity, computed with
  multiple samples along the motion vector (true sub-frame accumulation, not a
  post blur), edges mirrored so no black borders appear.
- Optional: slight scale and rotation overshoot, chromatic split amount.
- Variants (slide, zoom punch) are the same engine with other parameters.

As built (M8): the transition is a property of the incoming clip
(`ClipProps::transition`), so it moves, copies and undoes with the clip. A
whip or zoom never shows two pictures at once: the outgoing picture is on
screen up to the cut and the incoming one from it, each displaced and blurred
by `Transition::shift`, which is why no handles are needed. The blur is 48
samples per pixel between the picture's place when the frame's shutter opens
and when it closes (dithered from pixel to pixel), averaged in linear light,
with the picture mirrored beyond its edges. The slide is the one kind that
draws both clips. Overshoot and chromatic split are not built.

## Other music-video quality defaults

- Sequence frame rate follows the footage; mixed rates are conformed without
  frame blending unless the user asks (blending ruins sharp cuts).
- Export masters: 10-bit by default even from 8-bit sources (grading creates
  in-between values), high bitrate, correct colour tags, loudness reported.
- Stabilisation and speed ramps are per clip and cached by the render cache.
