#!/usr/bin/env python3
"""M3 step B programmatic screendump check: PPM -> M3_PIXELS_OK.

Usage: python3 scripts/m3-check.py /tmp/m3shot.ppm

Reads a QEMU `screendump` PPM (P6), counts distinct colours, compares the
mean colour and the exact slate fraction against the shell's dark-slate
pixel #14181f (payload/shell/style.css:1229, first stop of the slate
wallpaper gradient). Prints one marker line:

    M3_PIXELS_OK <distinct> <slate-fraction> <mean-r> <mean-g> <mean-b>

Exit 0 only when the frame is uniform slate (distinct == 1 and fraction
== 1.0); anything else is still printed but exits 1, so the numbers --
not the verdict -- are the result.
"""
import re
import sys
from collections import Counter

SLATE = (0x14, 0x18, 0x1F)


def main(path):
    raw = open(path, "rb").read()
    m = re.match(rb"P6\n(\d+) (\d+)\n(\d+)\n", raw)
    if not m:
        print(f"M3_PIXELS_FAIL not-P6 magic={raw[:2]!r} bytes={len(raw)}")
        return 1
    w, h, mx = int(m.group(1)), int(m.group(2)), int(m.group(3))
    px = raw[m.end():]
    if len(px) < w * h * 3:
        print(f"M3_PIXELS_FAIL short dims={w}x{h} bytes={len(px)}")
        return 1
    n = w * h
    cnt = Counter()
    rsum = gsum = bsum = 0
    for k in range(n):
        r, g, b = px[3 * k], px[3 * k + 1], px[3 * k + 2]
        cnt[(r, g, b)] += 1
        rsum += r
        gsum += g
        bsum += b
    distinct = len(cnt)
    frac = cnt.get(SLATE, 0) / n
    mean = (rsum / n, gsum / n, bsum / n)
    print(
        "M3_PIXELS_OK %d %.4f %.2f %.2f %.2f"
        % (distinct, frac, mean[0], mean[1], mean[2])
    )
    return 0 if (distinct == 1 and frac == 1.0) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
