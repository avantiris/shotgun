"""sg-matte: cuts a person out of a video with Robust Video Matting.

sg-matte <model.onnx> <video in> <video out .webm> [threads]

Decodes with ffmpeg, runs the model frame by frame on the CPU (it keeps
its own memory from frame to frame, which is what keeps the edge still),
and writes the picture with the matte as its transparency: VP9 with an
alpha channel. Where the matte is empty the picture is made black, which
costs nothing to store. One line "frame <n>" per frame on standard
output, for the progress shown in the program.

Run by Shotgun from its job threads (docs/spikes/s8-matte.md); the model
file is fetched by the program, not by this script.
"""
import json
import subprocess
import sys

import numpy as np
import onnxruntime as ort


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__, file=sys.stderr)
        return 2
    model, src, out = sys.argv[1:4]
    threads = int(sys.argv[4]) if len(sys.argv) > 4 else 8
    try:
        stream = json.loads(subprocess.check_output(
            ["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
             "stream=width,height,r_frame_rate:stream_side_data=rotation", "-of", "json", src]))["streams"][0]
    except (subprocess.CalledProcessError, IndexError, KeyError, ValueError):
        print("the video could not be read", file=sys.stderr)
        return 1
    width, height, rate = stream["width"], stream["height"], stream["r_frame_rate"]
    # ffmpeg turns a picture that is stored on its side upright while decoding.
    turned = any(abs(int(float(d.get("rotation", 0)))) % 180 == 90 for d in stream.get("side_data_list", []))
    if turned:
        width, height = height, width
    # The model looks at a small copy to find the person (about 480 points
    # on the long side) and refines the edge at full size.
    ratio = np.array([min(1.0, max(0.125, 480.0 / max(width, height)))], dtype=np.float32)

    options = ort.SessionOptions()
    options.intra_op_num_threads = threads
    session = ort.InferenceSession(model, options, providers=["CPUExecutionProvider"])
    decode = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-nostdin", "-i", src, "-an", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        stdout=subprocess.PIPE)
    encode = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-nostdin", "-y", "-f", "rawvideo", "-pix_fmt", "rgba", "-s", f"{width}x{height}",
         "-r", rate, "-i", "-", "-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-b:v", "0", "-crf", "22",
         "-row-mt", "1", "-cpu-used", "5", "-auto-alt-ref", "0", out],
        stdin=subprocess.PIPE)
    state = [np.zeros([1, 1, 1, 1], dtype=np.float32)] * 4
    size, frames = width * height * 3, 0
    try:
        while True:
            raw = decode.stdout.read(size)
            if len(raw) < size:
                break
            rgb = np.frombuffer(raw, dtype=np.uint8).reshape(height, width, 3)
            picture = np.ascontiguousarray(rgb.transpose(2, 0, 1)[None].astype(np.float32) / 255.0)
            _, matte, *state = session.run(None, {
                "src": picture, "r1i": state[0], "r2i": state[1], "r3i": state[2], "r4i": state[3],
                "downsample_ratio": ratio})
            alpha = (matte[0, 0] * 255.0 + 0.5).clip(0, 255).astype(np.uint8)
            # (Below 2 of 255 nothing can be seen: empty, so that it packs well.)
            alpha[alpha < 2] = 0
            cut = np.dstack([np.where(alpha[..., None] > 0, rgb, 0), alpha])
            encode.stdin.write(cut.tobytes())
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
