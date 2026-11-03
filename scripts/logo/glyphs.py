"""Turn a word into SVG outline paths with fontTools, so the wordmark needs no font."""
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.boundsPen import BoundsPen

def outline(font_path, text, size, x=0.0, baseline=0.0, tracking=0.0, wght=None, kern=True, fmt="{:.2f}"):
    f = TTFont(font_path)
    loc = {"wght": wght} if wght is not None and "fvar" in f else None
    gs = f.getGlyphSet(location=loc) if loc else f.getGlyphSet()
    upm = f["head"].unitsPerEm
    s = size / upm
    cmap = f.getBestCmap()
    names = [cmap[ord(c)] for c in text]
    pairs = {}
    if kern and "kern" in f:
        for t in f["kern"].kernTables:
            pairs.update(t.kernTable)
    d_all, cur = [], x
    boxes = []
    for i, n in enumerate(names):
        pen = SVGPathPen(gs, ntos=lambda v: fmt.format(v).rstrip("0").rstrip("."))
        tp = TransformPen(pen, (s, 0, 0, -s, cur, baseline))
        gs[n].draw(tp)
        d_all.append(pen.getCommands())
        bp = BoundsPen(gs)
        gs[n].draw(TransformPen(bp, (s, 0, 0, -s, cur, baseline)))
        boxes.append(bp.bounds)
        adv = gs[n].width * s
        if i + 1 < len(names):
            adv += pairs.get((n, names[i + 1]), 0) * s
        cur += adv + tracking
    return d_all, boxes, cur

if __name__ == "__main__":
    import sys
    d, b, w = outline(sys.argv[1], sys.argv[2], 100)
    print(b, w)
