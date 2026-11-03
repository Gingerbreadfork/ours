"""The bear, as parameters. Every output file is generated from here."""
import math
from geo import *

CLAY, CREAM, INK = "#c8562c", "#f8e7d2", "#2b1a14"

BASE = dict(tilt=-7.0, pivot=(32, 38), hx=32, hy=37, hrx=25.5, hry=22.5, kt=K, kb=0.58,
            ear_a=45, ear_r=8, mrx=10.5, mry_t=8, mry_b=8, mkt=K, my=46.3,
            nw=8.6, nh=5.8, nrt=1.7, nrb=2.2, ndy=-3.4, ex=9.2, ey=35.5, erx=2.7, ery=3.3)

def var(**kw):
    p = dict(BASE); p.update(kw); return p

def parts(p):
    rot = Rot(p["tilt"], *p["pivot"])
    a = math.radians(p["ear_a"])
    d = p.get("ear_d", 1.0)  # how far out the ear centre sits, as a share of the head radius
    el = (p["hx"] - p["hrx"] * math.sin(a) * d, p["hy"] - p["hry"] * math.cos(a) * d)
    er = (p["hx"] + p["hrx"] * math.sin(a) * d, el[1])
    mz = lambda ccw: blob(p["hx"], p["my"], p["mrx"], p["mrx"], p["mry_t"], p["mry_b"], p["mkt"], K, rot, ccw)
    eye = lambda s, ccw: ellipse(p["hx"] + s * p["ex"], p["ey"], p["erx"], p["ery"], rot, ccw)
    ns = lambda ccw: soft_nose(p["hx"], p["my"] + p["ndy"], p["nw"], p["nh"], p["nrt"], p["nrb"], rot, ccw)
    return dict(ears=circle(*el, p["ear_r"], rot) + circle(*er, p["ear_r"], rot),
                head=blob(p["hx"], p["hy"], p["hrx"], p["hrx"], p["hry"], p["hry"], p["kt"], p["kb"], rot),
                muzzle=mz(False), muzzle_ccw=mz(True), nose=ns(False),
                eyes=eye(-1, False) + eye(1, False), eyes_ccw=eye(-1, True) + eye(1, True))

def colour_body(p, body=CLAY, muz=CREAM, feat=INK, dx=0, dy=0, s=1):
    q = parts(p)
    g = f'<g transform="translate({fmt(dx)} {fmt(dy)}) scale({fmt(s)})">' if (dx or dy or s != 1) else ""
    out = (f'<path d="{q["ears"]}{q["head"]}" fill="{body}"/><path d="{q["muzzle"]}" fill="{muz}"/>'
           f'<path d="{q["nose"]}{q["eyes"]}" fill="{feat}"/>')
    return g + out + ("</g>" if g else "")

def mono_d(p):
    q = parts(p)
    # nonzero winding: head clockwise, muzzle and eyes anticlockwise as holes, nose clockwise inside the muzzle
    return q["ears"] + q["head"] + q["muzzle_ccw"] + q["eyes_ccw"] + q["nose"]

def shift(p, tx=0.0, ty=0.0):
    q = dict(p)
    q["hx"] += tx; q["hy"] += ty; q["my"] += ty; q["ey"] += ty
    q["pivot"] = (q["pivot"][0] + tx, q["pivot"][1] + ty)
    return q

LENGTHS = ("hrx", "hry", "ear_r", "mrx", "mry_t", "mry_b", "nw", "nh", "nrt", "nrb", "ndy", "ex", "erx", "ery")

def scale(p, s, cx=32.0, cy=32.0):
    q = dict(p)
    for k in LENGTHS:
        q[k] = p[k] * s
    q["hx"] = cx + (p["hx"] - cx) * s; q["hy"] = cy + (p["hy"] - cy) * s
    q["my"] = cy + (p["my"] - cy) * s; q["ey"] = cy + (p["ey"] - cy) * s
    q["pivot"] = (cx + (p["pivot"][0] - cx) * s, cy + (p["pivot"][1] - cy) * s)
    return q
