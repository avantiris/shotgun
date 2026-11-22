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

# Where the corners of the outline's box (0,0)-(760,250) land in the icon:
# the butt near and large at the lower left, the muzzle far and smaller.
BOX = [(0, 0), (760, 0), (760, 250), (0, 250)]
FLOOR = [(30, 340), (436, 78), (486, 170), (112, 478)]


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


def curve(p0, p1, p2, p3, n=16):
    """A cubic Bezier curve from p0 to p3 as points (without p0)."""
    out = []
    for i in range(1, n + 1):
        t = i / n
        k = ((1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t**3)
        out.append(tuple(sum(w * p[axis] for w, p in zip(k, (p0, p1, p2, p3))) for axis in (0, 1)))
    return out


# A short pump-action shotgun from the side, as one outline, clockwise
# from the muzzle's upper corner: the barrel's back, the comb, the butt,
# the stock's belly, the grip, the receiver, the pump and the barrel.
GUN = (
    [(760, 52), (250, 48)]
    + curve((250, 48), (180, 52), (90, 62), (0, 84))
    + [(0, 196)]
    + curve((0, 196), (70, 196), (150, 150), (215, 128))
    + curve((215, 128), (232, 150), (238, 190), (250, 206))
    + [(292, 206)]
    + curve((292, 206), (296, 180), (300, 150), (316, 132))
    + [(400, 132), (410, 150), (600, 150), (610, 118), (760, 118)]
)


def path(project):
    points = [project(p) for p in dense(GUN)]
    return "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in points) + " Z"


def main():
    project = projection(BOX, FLOOR)
    print(f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {SIZE} {SIZE}" width="{SIZE}" height="{SIZE}">
  <rect x="16" y="16" width="480" height="480" rx="108" fill="#ffffff"/>
  <path d="{path(project)}" fill="{INK}" stroke="{INK}" stroke-width="8" stroke-linejoin="round"/>
  <rect x="16.5" y="16.5" width="479" height="479" rx="107.5" fill="none" stroke="{INK}" stroke-opacity="0.12"/>
</svg>""")


if __name__ == "__main__":
    main()
