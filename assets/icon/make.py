#!/usr/bin/env python3
"""Draws the program's icon: the shadow of a shotgun on a white floor.

    python3 assets/icon/make.py > assets/icon/shotgun.svg

The gun is a flat outline (side view, muzzle to the right) that is laid on
the floor and looked at from low down, which is a projection of a plane:
SVG's own transforms cannot do that, so the points are projected here.
"""

import math

SIZE = 512

# Where the corners of the outline's box (0,0)-(1000,300) land in the icon:
# the butt near and large at the lower left, the muzzle far and small.
BOX = [(0, 0), (1000, 0), (1000, 300), (0, 300)]
FLOOR = [(34, 322), (436, 70), (474, 136), (106, 468)]


def solve(rows):
    """Gaussian elimination with pivoting; rows are [coefficients..., value]."""
    n = len(rows)
    for col in range(n):
        pivot = max(range(col, n), key=lambda r: abs(rows[r][col]))
        rows[col], rows[pivot] = rows[pivot], rows[col]
        for r in range(n):
            if r != col:
                factor = rows[r][col] / rows[col][col]
                rows[r] = [a - factor * b for a, b in zip(rows[r], rows[col])]
    return [rows[i][n] / rows[i][i] for i in range(n)]


def projection(src, dst):
    rows = []
    for (x, y), (u, v) in zip(src, dst):
        rows.append([x, y, 1, 0, 0, 0, -u * x, -u * y, u])
        rows.append([0, 0, 0, x, y, 1, -v * x, -v * y, v])
    a, b, c, d, e, f, g, h = solve(rows)

    def project(point):
        x, y = point
        w = g * x + h * y + 1
        return ((a * x + b * y + c) / w, (d * x + e * y + f) / w)

    return project


def dense(points, step=6.0):
    """More points along every edge: a straight edge stays straight under
    the projection, but a curve made of few points would show its corners."""
    out = []
    for (x0, y0), (x1, y1) in zip(points, points[1:] + points[:1]):
        n = max(1, int(math.hypot(x1 - x0, y1 - y0) / step))
        out += [(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n) for i in range(n)]
    return out


def arc(cx, cy, rx, ry, a0, a1, n=24):
    return [
        (cx + rx * math.cos(math.radians(a0 + (a1 - a0) * i / n)),
         cy + ry * math.sin(math.radians(a0 + (a1 - a0) * i / n)))
        for i in range(n + 1)
    ]


def rounded(x0, y0, x1, y1, r):
    return (
        arc(x1 - r, y0 + r, r, r, -90, 0, 8)
        + arc(x1 - r, y1 - r, r, r, 0, 90, 8)
        + arc(x0 + r, y1 - r, r, r, 90, 180, 8)
        + arc(x0 + r, y0 + r, r, r, 180, 270, 8)
    )


# A pump-action shotgun from the side. Each part is a closed outline; they
# overlap and are filled as one shape.
STOCK = [
    (300, 96), (150, 104), (20, 112), (6, 122), (0, 150), (4, 232), (16, 244),
    (60, 240), (150, 206), (228, 176), (262, 172), (276, 196), (286, 236),
    (300, 250), (330, 246), (336, 228), (322, 190), (318, 168), (330, 160),
]
RECEIVER = rounded(292, 88, 500, 166, 10)
BARREL = rounded(480, 88, 1000, 118, 6)
MAGAZINE = rounded(480, 124, 930, 154, 10)
BAND = rounded(892, 96, 916, 150, 4)
PUMP = rounded(590, 114, 800, 180, 18)
SIGHT = rounded(976, 76, 992, 100, 3)
GUARD_OUTER = arc(392, 164, 50, 46, 0, 180, 30)
# (Wound the other way: a hole.)
GUARD_INNER = arc(392, 164, 38, 34, 180, 0, 30)
TRIGGER = [(388, 164), (398, 164), (392, 190), (382, 196)]

PARTS = [STOCK, RECEIVER, BARREL, MAGAZINE, BAND, PUMP, SIGHT, GUARD_OUTER + GUARD_INNER, TRIGGER]


def paths(project):
    """One path per part: in one path, parts wound in opposite directions
    would cut holes into each other where they overlap."""
    out = []
    for part in PARTS:
        points = [project(p) for p in dense(part)]
        d = "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in points) + " Z"
        out.append(f'<path fill-rule="evenodd" d="{d}"/>')
    return "\n      ".join(out)


def main():
    project = projection(BOX, FLOOR)
    # The shadow gets lighter along the gun, from the near end to the far one
    # (a colour, not an opacity: the parts overlap and would add up).
    (x0, y0), (x1, y1) = project((0, 150)), project((1000, 110))
    print(f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" width="{SIZE}" height="{SIZE}">
  <defs>
    <linearGradient id="floor" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff"/>
      <stop offset="1" stop-color="#eceef1"/>
    </linearGradient>
    <linearGradient id="shade" gradientUnits="userSpaceOnUse" x1="{x0:.1f}" y1="{y0:.1f}" x2="{x1:.1f}" y2="{y1:.1f}">
      <stop offset="0" stop-color="#0b0c0e"/>
      <stop offset="0.55" stop-color="#25282d"/>
      <stop offset="1" stop-color="#6d727b"/>
    </linearGradient>
    <filter id="soft" filterUnits="userSpaceOnUse" x="0" y="0" width="{SIZE}" height="{SIZE}">
      <feGaussianBlur stdDeviation="1.6"/>
    </filter>
    <filter id="spill" filterUnits="userSpaceOnUse" x="0" y="0" width="{SIZE}" height="{SIZE}">
      <feGaussianBlur stdDeviation="14"/>
    </filter>
    <clipPath id="tile">
      <rect x="16" y="16" width="480" height="480" rx="108"/>
    </clipPath>
  </defs>
  <rect x="16" y="16" width="480" height="480" rx="108" fill="url(#floor)"/>
  <g clip-path="url(#tile)">
    <g fill="#0b0c0e" opacity="0.16" filter="url(#spill)">
      {paths(project)}
    </g>
    <g fill="url(#shade)" filter="url(#soft)">
      {paths(project)}
    </g>
  </g>
  <rect x="16.5" y="16.5" width="479" height="479" rx="107.5" fill="none" stroke="#0b0c0e" stroke-opacity="0.12"/>
</svg>""")


if __name__ == "__main__":
    main()
