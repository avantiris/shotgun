"""sg-sky: finds the sky in a video and makes it transparent.

sg-sky <model.onnx> <video in> <video out> <from seconds> <frames> [threads]

The output is the picture with the sky transparent (VP9 with an alpha
channel, .webm), for a sky of one's own on the track below.
<frames>: how many to make from <from seconds> on; 0 is to the end.

How: a model made for it (skyseg, a U2-Net; MIT) marks the sky in a small
copy of the picture, every few frames. That mark is fitted to the edges
of each full frame (a guided filter), and widened to what has the sky's
colour right beside it (the gaps between leaves).

One line "frame <n>" per frame on standard output, for the progress
shown in the program. Run by Shotgun from its job threads
(docs/spikes/s9-sky.md); the model file is fetched by the program.
"""
import json
import subprocess
import sys

import cv2
import numpy as np
import onnxruntime as ort

MEAN = np.array([0.485, 0.456, 0.406], dtype=np.float32)
STD = np.array([0.229, 0.224, 0.225], dtype=np.float32)
LUMA = np.array([0.299, 0.587, 0.114], dtype=np.float32)


def smooth(x, lo, hi):
    t = np.clip((x - lo) / (hi - lo), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


class Resize:
    """Resizing to one fixed size."""

    def __init__(self, iw, ih, w, h):
        self.size = (w, h)
        # (Made smaller: every point of the source counts. Made larger: straight lines between them.)
        self.how = cv2.INTER_AREA if w < iw else cv2.INTER_LINEAR

    def __call__(self, image):
        return cv2.resize(image, self.size, interpolation=self.how)


def box(a, r):
    """The mean over a square of 2r+1."""
    return cv2.blur(a, (2 * r + 1, 2 * r + 1), borderType=cv2.BORDER_REFLECT)


class Camera:
    """Whether the picture was cut between two frames: then they have no
    shift in common (phase correlation on a small grey copy)."""

    def __init__(self):
        self.before = None
        self.window = None
        self.at = np.zeros(2, dtype=np.float32)

    def step(self, grey):
        """Returns the offset so far and whether this frame begins another shot."""
        cut = False
        if self.before is not None:
            cut = float(np.abs(grey - self.before).mean()) > 0.16
            if not cut:
                if self.window is None:
                    h, w = grey.shape
                    self.window = (np.hanning(h)[:, None] * np.hanning(w)[None, :]).astype(np.float32)
                a = np.fft.rfft2((self.before - self.before.mean()) * self.window)
                b = np.fft.rfft2((grey - grey.mean()) * self.window)
                cross = a * np.conj(b)
                peak = np.fft.irfft2(cross / np.maximum(np.abs(cross), 1e-9), s=grey.shape)
                py, px = np.unravel_index(np.argmax(peak), peak.shape)
                h, w = grey.shape
                # (A clear peak only: water, leaves in the wind and a person walking by give none.)
                if peak[py, px] > 0.06:
                    dy = py - h if py > h // 2 else py
                    dx = px - w if px > w // 2 else px
                    if abs(dx) < w // 4 and abs(dy) < h // 4:
                        self.at += (dx / w, dy / h)
        self.before = grey
        return self.at.copy(), cut


def probe(src):
    stream = json.loads(subprocess.check_output(
        ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
         "stream=width,height,r_frame_rate:stream_side_data=rotation", "-of", "json", src]))["streams"][0]
    width, height, rate = stream["width"], stream["height"], stream["r_frame_rate"]
    # ffmpeg turns a picture that is stored on its side upright while decoding.
    if any(abs(int(float(d.get("rotation", 0)))) % 180 == 90 for d in stream.get("side_data_list", [])):
        width, height = height, width
    num, _, den = rate.partition("/")
    return width, height, rate, float(num) / float(den or 1)


def main() -> int:
    if len(sys.argv) < 6:
        print(__doc__, file=sys.stderr)
        return 2
    model, src, out = sys.argv[1:4]
    start, count = float(sys.argv[4]), int(sys.argv[5])
    threads = int(sys.argv[6]) if len(sys.argv) > 6 else 8
    try:
        width, height, rate, fps = probe(src)
    except (subprocess.CalledProcessError, IndexError, KeyError, ValueError, ZeroDivisionError):
        print("the video could not be read", file=sys.stderr)
        return 1

    options = ort.SessionOptions()
    options.intra_op_num_threads = threads
    session = ort.InferenceSession(model, options, providers=["CPUExecutionProvider"])
    feed = session.get_inputs()[0].name
    seek = ["-ss", f"{start:.6f}"] if start > 0 else []
    limit = ["-frames:v", str(count)] if count > 0 else []
    decode = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-nostdin", *seek, "-i", src, "-an", *limit, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        stdout=subprocess.PIPE)
    codec = ["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "22", "-row-mt", "1",
             "-cpu-used", "5", "-auto-alt-ref", "0"]
    pixels = "rgba"
    encode = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-nostdin", "-y", "-f", "rawvideo", "-pix_fmt", pixels, "-s", f"{width}x{height}",
         "-r", rate, "-i", "-", *codec, out],
        stdin=subprocess.PIPE)

    # Sizes: the model's (320 square), a small one for the sums over
    # neighbourhoods (480 across), a middle one at which the sky's edge is
    # found (half the picture, at most 960 across), and the picture's.
    mid_w = min(width, max(480, min(960, width // 2)))
    mid_h = max(2, round(mid_w * height / width))
    small_w = min(mid_w, 480)
    small_h = max(2, round(small_w * height / width))
    to_mid = Resize(width, height, mid_w, mid_h)
    to_model = Resize(mid_w, mid_h, 320, 320)
    to_small = Resize(mid_w, mid_h, small_w, small_h)
    mark_to_small = Resize(320, 320, small_w, small_h)
    up = Resize(small_w, small_h, mid_w, mid_h)
    to_full = Resize(mid_w, mid_h, width, height)
    to_track = Resize(small_w, small_h, 192, 108)
    camera = Camera()
    # The model looks at about fifteen frames a second; in between its last mark is fitted to each frame.
    every = max(1, round(fps / 15))
    mark = None
    size, frames = width * height * 3, 0
    try:
        while True:
            raw = decode.stdout.read(size)
            if len(raw) < size:
                break
            rgb = np.frombuffer(raw, dtype=np.uint8).reshape(height, width, 3)
            mid = to_mid(rgb).astype(np.float32) * (1.0 / 255.0)
            small = to_small(mid)
            grey_small = cv2.transform(small, LUMA[None, :])
            # What tells sky from what stands before it better than
            # brightness does: blue. A white sky and a blue one both have
            # it; leaves in the sun, bark and skin have little.
            blue_small = np.ascontiguousarray(small[..., 2])
            _, cut = camera.step(to_track(grey_small))
            if mark is None or cut or frames % every == 0:
                x = (to_model(mid) - MEAN) / STD
                found = session.run(None, {feed: np.ascontiguousarray(x.transpose(2, 0, 1)[None])})[0][0, 0]
                mark = np.clip(mark_to_small(found), 0, 1)

            # The mark fitted to this frame's edges: straight lines between
            # blue and mark over small squares, applied to the picture's blue.
            r, eps = 6, 4e-4
            mean_g, mean_m = box(blue_small, r), box(mark, r)
            a = (box(blue_small * mark, r) - mean_g * mean_m) / (box(blue_small * blue_small, r) - mean_g * mean_g + eps)
            b = mean_m - a * mean_g
            blue = np.ascontiguousarray(mid[..., 2])
            fitted = np.clip(up(box(a, r)) * blue + up(box(b, r)), 0, 1)
            # Far from any edge the model is believed as it is.
            sure = up(np.abs(box(mark, 5) - 0.5) * 2.0)
            matte = np.where(sure > 0.96, up(mark), fitted)

            # What the sky looks like here, from the sky around: its colour.
            weight = box(mark, 24)
            colour = box(small * mark[..., None], 24) / np.maximum(weight, 1e-4)[..., None]
            total = max(float(mark.sum()), 1e-3)
            everywhere = (small * mark[..., None]).sum(axis=(0, 1)) / total
            colour = up(np.where(weight[..., None] > 0.02, colour, everywhere).astype(np.float32))
            # Widened to what has that colour right beside the sky: the gaps
            # between leaves, the glare around branches.
            away = cv2.absdiff(mid, colour)
            like = 1.0 - smooth(np.maximum(np.maximum(away[..., 0], away[..., 1]), away[..., 2]), 0.05, 0.14)
            near = smooth(up(box(mark, 3)), 0.08, 0.40)
            matte = smooth(np.maximum(matte, like * near), 0.04, 0.96)

            alpha = 255 - (to_full(matte) * 255.0 + 0.5).astype(np.uint8)
            # (Below 2 of 255 nothing can be seen: empty, so that it packs well.)
            alpha[alpha < 2] = 0
            cut_out = np.dstack([np.where(alpha[..., None] > 0, rgb, 0), alpha])
            encode.stdin.write(cut_out.tobytes())
            frames += 1
            print(f"frame {frames}", flush=True)
    except BrokenPipeError:
        decode.kill()
    finally:
        encode.stdin.close()
    # (A video that could not be read to its end must not pass for a short one.)
    if decode.wait() != 0:
        print("the video could not be read to its end", file=sys.stderr)
        return 1
    if encode.wait() != 0 or frames == 0:
        print("no picture could be written", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
