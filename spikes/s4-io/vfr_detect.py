#!/usr/bin/env python3
"""S4 item 2: VFR detection from packet timestamps.
Usage: ffprobe -v error -select_streams v:0 -show_entries stream=time_base:packet=pts -of json FILE | python3 -I vfr_detect.py
Rule: sort PTS, take deltas; a delta is 'off' if it differs from the median delta by more than
max(1 time-base tick, 0.5 % of the median) (the 1-tick allowance absorbs ms rounding in MKV/WebM).
VFR if any delta is off. Prints one JSON line.
"""
import json, sys
d = json.load(sys.stdin)
num, den = map(int, d["streams"][0]["time_base"].split("/"))
pts = sorted(int(p["pts"]) for p in d["packets"] if "pts" in p)
dl = [b - a for a, b in zip(pts, pts[1:])]
if not dl:
    print(json.dumps({"vfr": False, "packets": len(pts)})); sys.exit()
sd = sorted(dl)
med = sd[len(sd) // 2]
tol = max(1, med * 0.005)
off = [x for x in dl if abs(x - med) > tol]
tb = num / den
print(json.dumps({
    "vfr": bool(off), "packets": len(pts), "off_deltas": len(off),
    "median_fps": round(1 / (med * tb), 4) if med else None,
    "avg_fps": round((len(pts) - 1) / ((pts[-1] - pts[0]) * tb), 4),
    "min_delta_ms": round(sd[0] * tb * 1000, 3), "max_delta_ms": round(sd[-1] * tb * 1000, 3),
}))
