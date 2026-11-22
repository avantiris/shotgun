"""Spike S8: Robust Video Matting through onnxruntime on the CPU.

rvm.py <model.onnx> <video in> <out> [downsample ratio] [max frames] [what]

Decodes with ffmpeg, runs the model frame by frame (it carries its own
memory from frame to frame), and writes with ffmpeg:
  what = matte  the matte as a grey video (default)
  what = cut    the picture with the matte as its alpha (VP9 with alpha, .webm)
  what = green  the person over green, to look at
Prints frames per second of the model alone and of the whole.
"""
import subprocess, sys, time, json
import numpy as np
import onnxruntime as ort

model, src, out = sys.argv[1:4]
ratio = float(sys.argv[4]) if len(sys.argv) > 4 else 0.25
limit = int(sys.argv[5]) if len(sys.argv) > 5 else 0
what = sys.argv[6] if len(sys.argv) > 6 else "matte"

probe = json.loads(subprocess.check_output(
    ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
     "stream=width,height,r_frame_rate", "-of", "json", src]))["streams"][0]
w, h, rate = probe["width"], probe["height"], probe["r_frame_rate"]

options = ort.SessionOptions()
options.intra_op_num_threads = int(sys.argv[7]) if len(sys.argv) > 7 else 0
session = ort.InferenceSession(model, options, providers=["CPUExecutionProvider"])

decode = subprocess.Popen(
    ["ffmpeg", "-v", "error", "-i", src, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
    stdout=subprocess.PIPE)
if what == "matte":
    fmt, codec = "gray", ["-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-pix_fmt", "yuv420p"]
elif what == "cut":
    fmt, codec = "rgba", ["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "24", "-row-mt", "1", "-cpu-used", "5"]
else:
    fmt, codec = "rgb24", ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"]
encode = subprocess.Popen(
    ["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", fmt, "-s", f"{w}x{h}", "-r", rate, "-i", "-",
     *codec, out], stdin=subprocess.PIPE)

state = [np.zeros([1, 1, 1, 1], dtype=np.float32)] * 4
down = np.array([ratio], dtype=np.float32)
frames, in_model, started = 0, 0.0, time.time()
size = w * h * 3
while True:
    raw = decode.stdout.read(size)
    if len(raw) < size or (limit and frames >= limit):
        break
    rgb = np.frombuffer(raw, dtype=np.uint8).reshape(h, w, 3)
    x = np.ascontiguousarray(rgb.transpose(2, 0, 1)[None].astype(np.float32) / 255.0)
    t = time.time()
    fgr, pha, *state = session.run(None, {
        "src": x, "r1i": state[0], "r2i": state[1], "r3i": state[2], "r4i": state[3], "downsample_ratio": down})
    in_model += time.time() - t
    alpha = (pha[0, 0] * 255.0 + 0.5).clip(0, 255).astype(np.uint8)
    if what == "matte":
        encode.stdin.write(alpha.tobytes())
    elif what == "cut":
        encode.stdin.write(np.dstack([rgb, alpha]).tobytes())
    else:
        a = alpha[..., None].astype(np.float32) / 255.0
        green = np.array([0, 200, 60], dtype=np.float32)
        encode.stdin.write((rgb * a + green * (1 - a)).astype(np.uint8).tobytes())
    frames += 1
decode.kill()
encode.stdin.close()
encode.wait()
whole = time.time() - started
print(f"{w}x{h} ratio {ratio}: {frames} frames, model {frames / max(in_model, 1e-9):.1f} fps, whole {frames / whole:.1f} fps")
