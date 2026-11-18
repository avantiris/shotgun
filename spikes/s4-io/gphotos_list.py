#!/usr/bin/env python3
"""S4 spike: list media items of a public Google Photos share page (already fetched HTML).
Usage: python3 -I gphotos_list.py page.html
Relies on the undocumented page structure: items appear as
["<mediaKey>",["https://lh3.googleusercontent.com/pw/<token>",<width>,<height>, ...], <timestamp_ms>, ...
A video item additionally carries a nested dict with key "76647426" in the same record.
"""
import re, sys, json
html = open(sys.argv[1], encoding="utf-8", errors="replace").read()
pat = re.compile(r'\["(AF1Qip[\w-]+)",\["(https://lh3\.googleusercontent\.com/pw/[\w-]+)",(\d+),(\d+)')
seen = {}
for m in pat.finditer(html):
    key, url, w, h = m.group(1), m.group(2), int(m.group(3)), int(m.group(4))
    if key in seen:
        continue
    tail = html[m.end():m.end() + 1500]
    nxt = pat.search(tail)
    rec = tail[:nxt.start()] if nxt else tail
    seen[key] = {"key": key, "url": url, "w": w, "h": h, "video": '"76647426"' in rec}
print(json.dumps(list(seen.values()), indent=1))
