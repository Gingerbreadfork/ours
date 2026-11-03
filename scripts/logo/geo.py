"""Small path builder: shapes as cubic paths, optionally rotated, so transforms bake into coordinates."""
import math

K = 0.5523  # cubic handle factor for a circle quadrant

def fmt(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s

class Rot:
    def __init__(self, deg=0.0, cx=32.0, cy=32.0):
        self.a = math.radians(deg); self.cx = cx; self.cy = cy
    def __call__(self, x, y):
        c, s = math.cos(self.a), math.sin(self.a)
        dx, dy = x - self.cx, y - self.cy
        return (self.cx + dx * c - dy * s, self.cy + dx * s + dy * c)

def blob(cx, cy, rx_l, rx_r, ry_t, ry_b, kt=K, kb=K, rot=None, ccw=False):
    """Closed four-quadrant shape: an ellipse when all handles are K, a squircle when larger."""
    rot = rot or Rot()
    T = (cx, cy - ry_t); R = (cx + rx_r, cy); B = (cx, cy + ry_b); L = (cx - rx_l, cy)
    segs = [  # clockwise in screen coordinates: top, right, bottom, left
        (T, (cx + rx_r * kt, cy - ry_t), (cx + rx_r, cy - ry_t * kt), R),
        (R, (cx + rx_r, cy + ry_b * kb), (cx + rx_r * kb, cy + ry_b), B),
        (B, (cx - rx_l * kb, cy + ry_b), (cx - rx_l, cy + ry_b * kb), L),
        (L, (cx - rx_l, cy - ry_t * kt), (cx - rx_l * kt, cy - ry_t), T),
    ]
    if ccw:
        segs = [(s[3], s[2], s[1], s[0]) for s in reversed(segs)]
    p0 = rot(*segs[0][0])
    d = f"M{fmt(p0[0])} {fmt(p0[1])}"
    for _, c1, c2, e in segs:
        c1, c2, e = rot(*c1), rot(*c2), rot(*e)
        d += f"C{fmt(c1[0])} {fmt(c1[1])} {fmt(c2[0])} {fmt(c2[1])} {fmt(e[0])} {fmt(e[1])}"
    return d + "Z"

def circle(cx, cy, r, rot=None, ccw=False):
    return blob(cx, cy, r, r, r, r, rot=rot, ccw=ccw)

def ellipse(cx, cy, rx, ry, rot=None, ccw=False):
    return blob(cx, cy, rx, rx, ry, ry, rot=rot, ccw=ccw)

def poly_path(pts, rot=None):
    """Pre-built list of ('M'|'L'|'C'|'Q', coords...) commands, rotated."""
    rot = rot or Rot()
    out = ""
    for cmd, *c in pts:
        if cmd == "Z":
            out += "Z"; continue
        xy = [rot(c[i], c[i + 1]) for i in range(0, len(c), 2)]
        out += cmd + " ".join(f"{fmt(x)} {fmt(y)}" for x, y in xy)
    return out

def nose(cx, cy, w, h, rot=None, ccw=False):
    """Rounded inverted triangle: flat soft top, round point at the bottom."""
    x0, x1 = cx - w / 2, cx + w / 2
    t, b = cy - h / 2, cy + h / 2
    r = h * 0.32
    pts = [("M", x0 + r, t), ("L", x1 - r, t),
           ("C", x1 + r * 0.35, t, x1 + r * 0.15, t + r * 1.3, x1 - r * 0.3, t + r * 1.6),
           ("L", cx + r * 0.55, b - r * 0.45),
           ("C", cx + r * 0.25, b + r * 0.05, cx - r * 0.25, b + r * 0.05, cx - r * 0.55, b - r * 0.45),
           ("L", x0 + r * 0.3, t + r * 1.6),
           ("C", x0 - r * 0.15, t + r * 1.3, x0 - r * 0.35, t, x0 + r, t), ("Z",)]
    if ccw:
        # reverse by rebuilding the same outline anticlockwise
        pts = [("M", x0 + r, t),
               ("C", x0 - r * 0.35, t, x0 - r * 0.15, t + r * 1.3, x0 + r * 0.3, t + r * 1.6),
               ("L", cx - r * 0.55, b - r * 0.45),
               ("C", cx - r * 0.25, b + r * 0.05, cx + r * 0.25, b + r * 0.05, cx + r * 0.55, b - r * 0.45),
               ("L", x1 - r * 0.3, t + r * 1.6),
               ("C", x1 + r * 0.15, t + r * 1.3, x1 + r * 0.35, t, x1 - r, t), ("Z",)]
    return poly_path(pts, rot)

def svg(body, vb=64, extra=""):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {vb} {vb}"{extra}>{body}</svg>\n'

def rounded_polygon(pts, radii, rot=None, ccw=False):
    """Polygon with a circular arc of the given radius at each corner. Arcs survive rotation unchanged."""
    rot = rot or Rot()
    n = len(pts)
    if ccw:
        pts = list(reversed(pts)); radii = list(reversed(radii))
    segs = []
    for i in range(n):
        p0, p1, p2 = pts[i - 1], pts[i], pts[(i + 1) % n]
        v1 = (p0[0] - p1[0], p0[1] - p1[1]); v2 = (p2[0] - p1[0], p2[1] - p1[1])
        l1 = math.hypot(*v1); l2 = math.hypot(*v2)
        u1 = (v1[0] / l1, v1[1] / l1); u2 = (v2[0] / l2, v2[1] / l2)
        ang = math.acos(max(-1, min(1, u1[0] * u2[0] + u1[1] * u2[1])))
        r = radii[i]
        t = r / math.tan(ang / 2)
        a = (p1[0] + u1[0] * t, p1[1] + u1[1] * t)
        b = (p1[0] + u2[0] * t, p1[1] + u2[1] * t)
        cross = v1[0] * v2[1] - v1[1] * v2[0]
        sweep = 1 if cross < 0 else 0
        segs.append((a, b, r, sweep))
    a0 = rot(*segs[0][0])
    d = f"M{fmt(a0[0])} {fmt(a0[1])}"
    for i, (a, b, r, sweep) in enumerate(segs):
        if i:
            pa = rot(*a); d += f"L{fmt(pa[0])} {fmt(pa[1])}"
        pb = rot(*b)
        d += f"A{fmt(r)} {fmt(r)} 0 0 {sweep} {fmt(pb[0])} {fmt(pb[1])}"
    return d + "Z"

def soft_nose(cx, cy, w, h, rt, rb, rot=None, ccw=False):
    """Inverted triangle nose with soft top corners and a rounder bottom."""
    t, b = cy - h / 2, cy + h / 2
    return rounded_polygon([(cx - w / 2, t), (cx + w / 2, t), (cx, b)], [rt, rt, rb], rot, ccw)
