# S9: another sky for a clip

Date: 2026-11-22. Asked for in feedback F17.2 ("make the sky have dark
and epic clouds"), measured on a copy of the video of his project
Selimdisstrack (1080p, 59.94 frames a second, many shots with sky: blue
with clouds, blown-out white, seen through leaves; and shots without).
Code: `spikes/s9-sky/sky.py` (first look), `tools/sg-sky.py` (what the
program runs).

## Decision

- **Finding the sky**: the skyseg model (a U2-Net trained for sky, from
  github.com/xiongzhu666/Sky-Segmentation-and-Post-processing, MIT
  licence; the ONNX file, 176 MB, from huggingface.co/JianyuanWang/skyseg,
  SHA-256 ab9c34c6...89aa1d39) on onnxruntime, CPU. It looks at a 320 by
  320 copy and gives a coarse mark.
- **The edge**: the mark is fitted to each frame with a guided filter
  whose guide is the picture's **blue** channel, at half the picture's
  size. With brightness as the guide, leaves in the sun were taken for a
  white sky (black holes in the crown); blue tells both a white and a
  blue sky from leaves, bark and skin. Then widened to what has the
  sky's colour right beside it.
- **The new sky is drawn, not footage**: noise with a falling spectrum,
  seen in a made-up perspective, lit from one side; three looks (storm,
  sunset, night with stars). Nothing to license or to ship. It moves
  against the camera's motion (phase correlation between frames), so it
  stays behind the picture in hand-held shots.
- **At the edge the new sky is darkened by what was there**, so thin
  branches stay; the rest of the picture is graded towards the new light.
- **Per clip, not per asset**: only the part of the video a clip shows
  is made, as a new asset that the clip then shows.

## Measured

| What | Result |
|---|---|
| The model alone, 8 threads | 330 to 420 ms per look; it looks at every fourth frame at 60 frames a second |
| The whole, 1080p60, storm | 180 frames in 48 s: about 4 frames a second, fifteen times the clip's length |
| With plain numpy instead of OpenCV for the sums | about 1 s per frame |
| Flicker | frame-to-frame change of the result follows the source's, with no steps where the model looks again (two shots, 48 frames each) |
| Camera tracking | a picture moved by 5 points is found as 5 points; another shot is found as a cut; the drawn sky moves by exactly as much |

Looked at on stills of twelve shots: blue sky behind trees, a house and
a person; a white sky beside a tree in the sun; sky through a dense
crown. All usable. Through the dense crown the sky is replaced in the
larger gaps only.

## What goes wrong

- **A plain bright wall is taken for sky.** The grey wall of a
  container behind him was marked as sky with full certainty. Nothing in
  the tool catches that; it is for clips that show sky.
- Shots without any sky are left alone by the model, but are still
  graded darker: use it only where there is sky.
- The drawn clouds are plausible, not photographic.

## Not measured

Whether it holds on a moving video for a person's eye (only stills and
numbers were looked at); 4K; upright video; water and reflections; a
GPU path.
