#!/usr/bin/env python3
"""Draws the program's icon: the shadow of a shotgun on a white floor.

    python3 assets/icon/make.py > assets/icon/shotgun.svg

A logo: one flat, heavy shape without details (no trigger, no sights, no
gradients), which still reads at 16 pixels. The gun is a flat outline
(side view, muzzle to the right) that is laid on the floor and looked at
from low down, which is a projection of a plane: SVG's own transforms
cannot do that, so the points are projected here.
"""

import math

SIZE = 512
INK = "#111214"

# Where the corners of the outline's box (0,0)-(1000,300) land in the icon:
# the butt near and large at the lower left, the muzzle far and smaller.
BOX = [(0, 0), (1000, 0), (1000, 300), (0, 300)]
FLOOR = [(44, 318), (420, 70), (476, 152), (108, 430)]


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


def arc(cx, cy, r, a0, a1, n=10):
    return [
        (cx + r * math.cos(math.radians(a0 + (a1 - a0) * i / n)),
         cy + r * math.sin(math.radians(a0 + (a1 - a0) * i / n)))
        for i in range(n + 1)
    ]


def rounded(x0, y0, x1, y1, r):
    return (
        arc(x1 - r, y0 + r, r, -90, 0)
        + arc(x1 - r, y1 - r, r, 0, 90)
        + arc(x0 + r, y1 - r, r, 90, 180)
        + arc(x0 + r, y0 + r, r, 180, 270)
    )


# A pump-action shotgun from the side, as four heavy blocks that overlap:
# what makes it a shotgun and nothing else.
STOCK = [(330, 66), (40, 100), (0, 124), (0, 236), (30, 258), (90, 258), (250, 196), (330, 196)]
RECEIVER = rounded(300, 60, 540, 196, 22)
BARREL = rounded(500, 76, 1000, 146, 20)
PUMP = rounded(620, 76, 820, 196, 30)

PARTS = [STOCK, RECEIVER, BARREL, PUMP]


def paths(project):
    out = []
    for part in PARTS:
        points = [project(p) for p in dense(part)]
        d = "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in points) + " Z"
        # (A stroke of the same colour rounds the corners and closes the seams between the blocks.)
        out.append(f'<path d="{d}"/>')
    return "\n    ".join(out)


def main():
    project = projection(BOX, FLOOR)
    print(f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" width="{SIZE}" height="{SIZE}">
  <rect x="16" y="16" width="480" height="480" rx="108" fill="#ffffff"/>
  <g fill="{INK}" stroke="{INK}" stroke-width="10" stroke-linejoin="round">
    {paths(project)}
  </g>
  <rect x="16.5" y="16.5" width="479" height="479" rx="107.5" fill="none" stroke="{INK}" stroke-opacity="0.12"/>
</svg>""")


if __name__ == "__main__":
    main()
