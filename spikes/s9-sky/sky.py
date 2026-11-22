"""Spike S9: a sky matte from the skyseg model (U2-Net, 320x320) through onnxruntime.

sky.py <model.onnx> <out dir> <picture>...

For each picture: the model's raw matte, the matte fitted to the
picture's edges (a guided filter), and the picture with the sky made
dark red, side by side, to look at.
"""
import subprocess, sys, time
import numpy as np
import onnxruntime as ort

MEAN = np.array([0.485, 0.456, 0.406], dtype=np.float32)
STD = np.array([0.229, 0.224, 0.225], dtype=np.float32)


def read(path):
    probe = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height",
                            "-of", "csv=p=0", path], capture_output=True, text=True).stdout.strip().split(",")
    w, h = int(probe[0]), int(probe[1])
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
                         capture_output=True).stdout
    return np.frombuffer(raw, dtype=np.uint8).reshape(h, w, 3)


def resize(image, w, h):
    """Bilinear, by ffmpeg-free numpy: good enough for mattes and model input."""
    ih, iw = image.shape[:2]
    ys = (np.arange(h) + 0.5) * ih / h - 0.5
    xs = (np.arange(w) + 0.5) * iw / w - 0.5
    y0 = np.clip(np.floor(ys).astype(int), 0, ih - 1); y1 = np.clip(y0 + 1, 0, ih - 1)
    x0 = np.clip(np.floor(xs).astype(int), 0, iw - 1); x1 = np.clip(x0 + 1, 0, iw - 1)
    fy = np.clip(ys - y0, 0, 1).astype(np.float32); fx = np.clip(xs - x0, 0, 1).astype(np.float32)
    if image.ndim == 3:
        fy = fy[:, None, None]; fx = fx[None, :, None]
    else:
        fy = fy[:, None]; fx = fx[None, :]
    top = image[y0][:, x0] * (1 - fx) + image[y0][:, x1] * fx
    bottom = image[y1][:, x0] * (1 - fx) + image[y1][:, x1] * fx
    return top * (1 - fy) + bottom * fy


def box(a, r):
    """Mean over a (2r+1) square, edges by what is there."""
    h, w = a.shape[:2]
    c = np.cumsum(a, axis=0, dtype=np.float64)
    c = np.concatenate([np.zeros_like(c[:1]), c], axis=0)
    lo = np.clip(np.arange(h) - r, 0, h); hi = np.clip(np.arange(h) + r + 1, 0, h)
    a = (c[hi] - c[lo]) / (hi - lo).reshape(-1, *([1] * (a.ndim - 1)))
    c = np.cumsum(a, axis=1)
    c = np.concatenate([np.zeros_like(c[:, :1]), c], axis=1)
    lo = np.clip(np.arange(w) - r, 0, w); hi = np.clip(np.arange(w) + r + 1, 0, w)
    out = (c[:, hi] - c[:, lo]) / (hi - lo).reshape(1, -1, *([1] * (a.ndim - 2)))
    return out.astype(np.float32)


def fit(guide_full, matte_small, small_w=480, r=12, eps=1e-3):
    """Guided filter, the fast kind: line fits at low size, applied to the full picture's grey."""
    h, w = guide_full.shape[:2]
    small_h = max(2, round(small_w * h / w))
    grey_full = guide_full.astype(np.float32) @ np.array([0.299, 0.587, 0.114], dtype=np.float32) / 255.0
    g = resize(grey_full, small_w, small_h)
    p = resize(matte_small, small_w, small_h)
    mg, mp = box(g, r), box(p, r)
    a = (box(g * p, r) - mg * mp) / (box(g * g, r) - mg * mg + eps)
    b = mp - a * mg
    a, b = box(a, r), box(b, r)
    return np.clip(resize(a, w, h) * grey_full + resize(b, w, h), 0, 1)


def main():
    model, out = sys.argv[1:3]
    session = ort.InferenceSession(model, providers=["CPUExecutionProvider"])
    name = session.get_inputs()[0].name
    for path in sys.argv[3:]:
        rgb = read(path)
        h, w = rgb.shape[:2]
        t0 = time.time()
        x = (resize(rgb.astype(np.float32) / 255.0, 320, 320) - MEAN) / STD
        raw = session.run(None, {name: np.ascontiguousarray(x.transpose(2, 0, 1)[None].astype(np.float32))})[0][0, 0]
        t1 = time.time()
        fitted = fit(rgb, raw)
        t2 = time.time()
        print(f"{path}: raw {raw.min():.2f}..{raw.max():.2f} mean {raw.mean():.3f}; model {1000*(t1-t0):.0f} ms, fit {1000*(t2-t1):.0f} ms")
        raw_full = np.clip(resize(raw, w, h), 0, 1)
        tint = np.array([120, 10, 10], dtype=np.float32)
        shown = (rgb * (1 - fitted[..., None]) + tint * fitted[..., None]).astype(np.uint8)
        grey = lambda m: np.repeat((m * 255).astype(np.uint8)[..., None], 3, axis=2)
        sheet = np.concatenate([rgb, grey(raw_full), grey(fitted), shown], axis=1)
        stem = path.rsplit("/", 1)[-1].rsplit(".", 1)[0]
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{sheet.shape[1]}x{h}",
                        "-i", "-", "-vf", "scale=iw/3:-2", f"{out}/{stem}-sky.png"], input=sheet.tobytes())


main()
