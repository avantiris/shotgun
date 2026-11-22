"""sg-sky: finds the sky in a video and puts another one there.

sg-sky <model.onnx> <video in> <video out> <sky> <from seconds> <frames> [threads]

<sky>: storm, sunset, night (the picture with that sky, as H.264 in
.mp4) or clear (the picture with the sky transparent, VP9 with an alpha
channel in .webm, for a sky of one's own on the track below).
<frames>: how many to make from <from seconds> on; 0 is to the end.

How: a model made for it (skyseg, a U2-Net; MIT) marks the sky in a small
copy of the picture, every few frames. That mark is fitted to the edges
of each full frame (a guided filter), and widened to what has the sky's
colour right beside it (the gaps between leaves). The new sky is drawn
here, from noise, and moves against the camera's motion (measured from
frame to frame), so that it stays in place behind the picture. At the
edges the new sky is darkened by what was there, so that branches stay
branches; the rest of the picture is graded towards the new light.

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


class Clouds:
    """A sky drawn from noise: one tile of cloud-like noise, looked at
    through a made-up perspective, lit from one side, in a style's colours."""

    STYLES = {
        # The sky seen through the gaps (overhead and at the horizon), the
        # clouds' thin edges and their thick middles, how much of the sky
        # is cloud (the noise level where cloud begins and where it is
        # thick), and the grade of the rest of the picture: gain,
        # saturation, tint.
        "storm": dict(top=(0.24, 0.27, 0.33), low=(0.66, 0.66, 0.63), edge=(0.46, 0.48, 0.53),
                      core=(0.07, 0.078, 0.10), cover=(0.16, 0.64),
                      gain=0.74, sat=0.62, tint=(0.93, 0.98, 1.06), stars=False),
        "sunset": dict(top=(0.13, 0.12, 0.30), low=(1.0, 0.56, 0.18), edge=(1.0, 0.60, 0.26),
                       core=(0.11, 0.05, 0.11), cover=(0.38, 0.80),
                       gain=0.82, sat=0.95, tint=(1.10, 0.96, 0.84), stars=False),
        "night": dict(top=(0.004, 0.008, 0.026), low=(0.03, 0.05, 0.10), edge=(0.13, 0.17, 0.27),
                      core=(0.004, 0.006, 0.014), cover=(0.42, 0.85),
                      gain=0.36, sat=0.50, tint=(0.80, 0.94, 1.22), stars=True),
    }

    def __init__(self, style, width, height, rate):
        self.style = self.STYLES[style]
        self.rate = rate
        # Drawn at about 640 points across and enlarged: clouds are soft.
        self.w = min(width, 640)
        self.h = max(2, round(self.w * height / width))
        self.up = Resize(self.w, self.h, width, height)
        random = np.random.default_rng(20261122)
        # Noise whose large shapes are strong and whose small ones are weak
        # (a spectrum falling with the frequency): it repeats at its borders.
        n = 1024
        fy, fx = np.fft.fftfreq(n)[:, None], np.fft.rfftfreq(n)[None, :]
        f = np.sqrt(fx * fx + fy * fy)
        f[0, 0] = 1.0
        spectrum = np.fft.rfft2(random.standard_normal((n, n))) / f**1.75
        spectrum[0, 0] = 0.0
        tile = np.fft.irfft2(spectrum, s=(n, n))
        lo, hi = np.percentile(tile, [1, 99])
        self.tile = np.clip((tile - lo) / (hi - lo), 0, 1).astype(np.float32)
        self.n = n
        ys, xs = np.mgrid[0:self.h, 0:self.w].astype(np.float32)
        self.x = (xs + 0.5) / self.w - 0.5
        self.y = (ys + 0.5) / self.h
        self.aspect = width / height
        stars = random.random((n, n)) > 0.9985
        self.stars = (stars * random.random((n, n)) ** 2).astype(np.float32)

    def sample(self, tile, u, v):
        """The tile at (u, v), one tile being 1 wide; it repeats."""
        n = self.n
        return cv2.remap(tile, (u * n) % n, (v * n) % n, cv2.INTER_LINEAR, borderMode=cv2.BORDER_WRAP)

    def draw(self, frame, camera):
        """The sky of frame `frame`, with the camera turned by `camera`
        (x, y as parts of the picture's width and height) since the first."""
        s = self.style
        t = frame / self.rate
        x, y = self.x + camera[0], self.y + camera[1]
        # Looking up at a flat layer of cloud: far away towards the
        # horizon, near overhead.
        depth = 1.0 / (1.3 - np.clip(y, -0.5, 1.1))
        u = x * self.aspect * depth * 0.30
        v = depth * 0.55
        drift = t * 0.0030
        warp = self.sample(self.tile, u * 0.6 + 0.31, v * 0.6 + 0.17 - drift * 0.5) - 0.5
        pu, pv = u + warp * 0.08 + drift, v + warp * 0.08
        fine = self.sample(self.tile, u * 2.9 + 0.5 + drift * 2.2, v * 2.9 + 0.2)
        noise = self.sample(self.tile, pu, pv) * 0.86 + fine * 0.14
        thick = smooth(noise, s["cover"][0], s["cover"][1])
        # Lit from the upper left: the side of a cloud that faces the light is brighter.
        towards = self.sample(self.tile, pu - 0.006, pv - 0.006) * 0.86 + fine * 0.14
        facing = np.clip((noise - towards) * 7.0, -0.5, 0.5)
        height = np.clip(y / 1.0, 0, 1)[..., None]
        behind = np.array(s["top"], np.float32) * (1 - height) + np.array(s["low"], np.float32) * height
        if s["stars"]:
            stars = self.sample(self.stars, x * 0.9 + 0.5, y * 0.9 / self.aspect + 0.5)
            behind = behind + (stars * (1 - height[..., 0]) * 6.0)[..., None]
        # Thin at its edge, where the light comes through; dark in the middle.
        middle = smooth(thick, 0.15, 0.95)[..., None]
        body = np.array(s["edge"], np.float32) * (1 - middle) + np.array(s["core"], np.float32) * middle
        body = body * (1.0 + facing[..., None] * (1.2 - middle))
        # Far clouds take the colour of the air in front of them.
        body = body * (1 - 0.55 * height**4) + behind * (0.55 * height**4)
        edge = smooth(thick, 0.0, 0.30)[..., None]
        sky = behind * (1 - edge) + body * edge
        return self.up(np.clip(sky, 0, 1).astype(np.float32))


class Camera:
    """How far the picture moved since the first frame, from the shift
    between each two frames (phase correlation on a small grey copy)."""

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
    if len(sys.argv) < 7:
        print(__doc__, file=sys.stderr)
        return 2
    model, src, out, style = sys.argv[1:5]
    start, count = float(sys.argv[5]), int(sys.argv[6])
    threads = int(sys.argv[7]) if len(sys.argv) > 7 else 8
    if style != "clear" and style not in Clouds.STYLES:
        print(f"no such sky: {style}", file=sys.stderr)
        return 2
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
    if style == "clear":
        codec = ["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "22", "-row-mt", "1",
                 "-cpu-used", "5", "-auto-alt-ref", "0"]
        pixels = "rgba"
    else:
        # (A key frame every half second: it is cut and scrubbed like any clip.)
        codec = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "15", "-pix_fmt", "yuv420p",
                 "-g", str(max(1, round(fps / 2))), "-color_primaries", "bt709", "-color_trc", "bt709",
                 "-colorspace", "bt709", "-movflags", "+faststart"]
        pixels = "rgb24"
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
    clouds = Clouds(style, width, height, fps) if style != "clear" else None
    if clouds is not None:
        look = clouds.style
        # Saturation, tint and gain of the rest of the picture as one sum per point.
        grade = (np.eye(3, dtype=np.float32) * look["sat"] + LUMA[None, :] * (1.0 - look["sat"]))
        grade = grade * (np.array(look["tint"], np.float32) * look["gain"])[:, None]
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
            moved, cut = camera.step(to_track(grey_small))
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

            if clouds is None:
                alpha = 255 - (to_full(matte) * 255.0 + 0.5).astype(np.uint8)
                # (Below 2 of 255 nothing can be seen: empty, so that it packs well.)
                alpha[alpha < 2] = 0
                cut_out = np.dstack([np.where(alpha[..., None] > 0, rgb, 0), alpha])
                encode.stdin.write(cut_out.tobytes())
            else:
                sky = clouds.draw(frames, moved)
                # At the edges the new sky is darkened by what the picture has
                # there (a branch stays a branch); well inside it stands alone.
                grey = cv2.transform(mid, LUMA[None, :])
                kept = np.clip(grey / np.maximum(cv2.transform(colour, LUMA[None, :]), 0.05), 0, 1)
                inside = smooth(up(box(mark, 4)), 0.80, 0.98)
                weight_sky = to_full((kept + (1.0 - kept) * inside) * matte)
                matte_full = to_full(matte)
                # The rest of the picture in the new light.
                result = cv2.transform(rgb.astype(np.float32), grade * (1.0 / 255.0))
                result *= (1.0 - matte_full)[..., None]
                sky *= weight_sky[..., None]
                result += sky
                np.clip(result, 0.0, 1.0, out=result)
                result *= 255.0
                encode.stdin.write(result.astype(np.uint8).tobytes())
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
