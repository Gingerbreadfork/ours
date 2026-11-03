"""Write every logo file from the parameters in final_params."""
import sys, os
from bear import *
from final_params import MARK, FAV
from glyphs import outline
from measure import bbox

OUT = sys.argv[1]
os.makedirs(OUT, exist_ok=True)
NOTO = "/usr/share/fonts/google-noto-vf/NotoSerif[wght].ttf"
WORD_DARK = "#f8e7d2"

def head(w, h, label="Ours"):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{fmt(w)}" height="{fmt(h)}" viewBox="0 0 {fmt(w)} {fmt(h)}" '
            f'role="img" aria-label="{label}"><title>{label}</title>')

def bear_paths(p):
    q = parts(p)
    return (f'<path fill="{CLAY}" d="{q["ears"]}{q["head"]}"/>'
            f'<path fill="{CREAM}" d="{q["muzzle"]}"/>'
            f'<path fill="{INK}" d="{q["nose"]}{q["eyes"]}"/>')

def write(name, text):
    open(os.path.join(OUT, name), "w").write(text)

write("logo-mark.svg", head(64, 64) + bear_paths(MARK) + "</svg>\n")
write("mark.svg", head(64, 64) + bear_paths(MARK) + "</svg>\n")
write("favicon.svg", head(64, 64) + bear_paths(FAV) + "</svg>\n")
# One colour: the face is cut out of the head by winding, so it prints as a single ink.
write("logo-mark-mono.svg", head(64, 64) +
      f'<path fill="{INK}" style="fill:var(--ours-mono,{INK})" d="{mono_d(MARK)}"/></svg>\n')

# The lockup: the bear's head is the height of a capital and sits on the baseline, ears above the caps.
CAP, OVERSHOOT, TRACK, GAP, PAD, SINK = 71.4, 2.5, -16, 0.22, 4.0, 0.02
PAIRS = {"Ou": -14, "ur": -4}  # optical spacing, in thousandths of an em

def lockup(word_fill):
    bb = bbox(MARK)
    s = (CAP + OVERSHOOT) / (2 * MARK["hry"])
    chin = (MARK["hy"] + MARK["hry"]) * s
    # the word on a baseline at y = 0 before the final offset
    glyph_x, boxes, ds = 0.0, [], []
    xs = []
    for i, ch in enumerate("Ours"):
        xs.append(glyph_x)
        _, b, adv = outline(NOTO, ch, 100, x=glyph_x, baseline=0, wght=700)
        boxes.append(b[0])
        glyph_x = adv + ((TRACK + PAIRS.get("Ours"[i:i + 2], 0)) * 0.1 if i < 3 else 0)
    bear_left = PAD
    bear_w = (bb[2] - bb[0]) * s
    word_left = bear_left + bear_w + GAP * CAP
    dx_word = word_left - boxes[0][0]
    bear_top = SINK * CAP - chin + bb[1] * s
    top = min(bear_top, min(b[1] for b in boxes)) - PAD
    bottom = max(SINK * CAP - chin + bb[3] * s, max(b[3] for b in boxes)) + PAD
    right = max(b[2] for b in boxes) + dx_word + PAD
    W, H = right, bottom - top
    base_y = -top
    bear = shift(scale(dict(MARK), s, 0, 0), bear_left - bb[0] * s, SINK * CAP - chin + base_y)
    d = "".join(outline(NOTO, ch, 100, x=xs[i] + dx_word, baseline=base_y, wght=700)[0][0] for i, ch in enumerate("Ours"))
    return head(W, H) + bear_paths(bear) + f'<path fill="{word_fill}" d="{d}"/></svg>\n', W, H

light, W, H = lockup(INK)
dark, _, _ = lockup(WORD_DARK)
write("wordmark-light.svg", light)
write("wordmark-dark.svg", dark)
print("lockup", round(W, 2), round(H, 2))

# Bot avatar: platforms crop to a circle, so the bear is fitted inside the inscribed circle on a cream square.
AV = scale(shift(dict(MARK), 0, 4.25), 0.8262, 32, 32)
write("logo-avatar.svg", head(64, 64) + f'<path fill="{CREAM}" d="M0 0H64V64H0Z"/>' + bear_paths(AV) + "</svg>\n")
