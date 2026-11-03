"""The chosen parameters for the mark and the favicon."""
from bear import *
from measure import bbox

def centred(p, extent=61.0):
    b = bbox(p)
    return scale(shift(dict(p), 32 - (b[0] + b[2]) / 2, 32 - (b[1] + b[3]) / 2), extent / max(b[2] - b[0], b[3] - b[1]))

# The mark: head tilted 7 degrees toward the word, oval eyes, no mouth.
MARK0 = var(tilt=7, hry=23.2, ear_r=7.8, ear_a=40, ear_d=1.1, hy=38, ey=36.2, my=46.9,
            nw=9.6, nh=6.1, nrt=1.3, nrb=2.1)
MARK = centred(MARK0)

# The favicon: a bigger head in the box, bigger eyes and nose, a smaller tilt, eyes placed on the 16 px grid.
FAVB = var(tilt=5, hry=22.6, hy=40.6, hrx=27.6, ear_r=8.2, ear_a=38, ear_d=1.16, pivot=(32, 40), hx=32,
           ex=10.2, ey=38, erx=3.5, ery=4.2, my=49.4, mrx=12.6, mry_t=9.2, mry_b=9.2, nw=11.6, nh=7.2,
           nrt=1.6, nrb=2.6, ndy=-4.2)
FAV = shift(scale(shift(dict(FAVB), -0.96, -4.69), 1.0609), -0.5, 1.0)
FAV["ex"] += 0.5; FAV["ey"] += 0.5
