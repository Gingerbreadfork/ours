"""Render helpers for measuring marks at small sizes."""
import subprocess, os, tempfile
import numpy as np
from PIL import Image
from bear import *

TMP = tempfile.mkdtemp()

def render_svg_text(text, size, bg="none"):
    f = os.path.join(TMP, "m.svg"); o = os.path.join(TMP, "m.png")
    open(f, "w").write(text)
    subprocess.run(["magick", "-background", bg, "-density", "1152", f, "-resize", "1024x1024",
                    "-filter", "Box", "-resize", f"{size}x{size}", "-background", bg, "-flatten" if bg != "none" else "+repage", o], check=True)
    return np.asarray(Image.open(o).convert("RGBA"), dtype=float)

def render(p, size, bg="none"):
    return render_svg_text(svg(colour_body(var(**p))), size, bg)

def bbox(p):
    a = render(p, 1024)[..., 3]
    ys, xs = np.where(a > 8)
    return [round(float(v) / 16, 2) for v in (xs.min(), ys.min(), xs.max() + 1, ys.max() + 1)]

def eye_score(p, rows=(5, 11)):
    a = render(p, 16, "white")[..., :3].mean(axis=2)
    r0, r1 = rows
    return float(np.sort(a[r0:r1, 2:8].ravel())[:2].sum() + np.sort(a[r0:r1, 8:14].ravel())[:2].sum())
