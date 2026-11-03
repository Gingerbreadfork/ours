# The logo generator

`python3 build.py ../../docs/assets` rewrites every logo file from the
parameters in `final_params.py`: the mark, the mono mark, the favicon, the
avatar and the two wordmark lockups. The wordmark letters are outlined from
Noto Serif Bold with fontTools, so the SVGs never depend on a viewer font.
`docs/assets/LOGO.md` says which file to use where.

It needs Python 3 with fontTools, numpy and Pillow, and ImageMagick on the
path for the measurements.
