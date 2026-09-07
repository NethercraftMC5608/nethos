#!/usr/bin/env python3
"""M3 step B/C programmatic screendump check: PPM -> M3_PIXELS_OK.

Usage:
    python3 scripts/m3-check.py /tmp/m3shot.ppm            # flat slate (step B)
    python3 scripts/m3-check.py --gradient /tmp/m3shot.ppm # slate ramp (step C)

Reads a QEMU `screendump` PPM (P6) and compares it against the shell's
dark-slate wallpaper colours (payload/shell/style.css:1225-1230).

Flat mode: every pixel must be #14181f (the gradient's first stop).
Prints:

    M3_PIXELS_OK <distinct> <slate-fraction> <mean-r> <mean-g> <mean-b>

Gradient mode: each row must equal the piecewise-linear ramp through the
three exact linear-layer stops (#14181f 0%, #11151c 60%, #0e1116 100%),
recomputed here with the same integer formula as kernel/init/m3fb.c's
row_color (thousandths of height, k in 0..256 per segment). Prints:

    M3_GRADIENT_OK <distinct> <match-fraction> <mismatched-rows>

In both modes exit 0 only on a full match; anything else is still printed
but exits 1, so the numbers -- not the verdict -- are the result.
"""
import re
import sys
from collections import Counter

SLATE = (0x14, 0x18, 0x1F)
STOPS = ((0x14, 0x18, 0x1F), (0x11, 0x15, 0x1C), (0x0E, 0x11, 0x16))


def row_color(y, h):
    t = (y * 1000) // (h - 1) if h > 1 else 0
    a, b, lo, hi = STOPS[0], STOPS[1], 0, 600
    if t >= 600:
        a, b, lo, hi = STOPS[1], STOPS[2], 600, 1000
    k = ((t - lo) * 256) // (hi - lo)
    return tuple((ca * (256 - k) + cb * k + 128) // 256 for ca, cb in zip(a, b))


def load(path):
    raw = open(path, "rb").read()
    m = re.match(rb"P6\n(\d+) (\d+)\n(\d+)\n", raw)
    if not m:
        return None, f"M3_PIXELS_FAIL not-P6 magic={raw[:2]!r} bytes={len(raw)}"
    w, h, mx = int(m.group(1)), int(m.group(2)), int(m.group(3))
    px = raw[m.end():]
    if len(px) < w * h * 3:
        return None, f"M3_PIXELS_FAIL short dims={w}x{h} bytes={len(px)}"
    return (w, h, px), None


def flat(path):
    got, err = load(path)
    if err:
        print(err)
        return 1
    w, h, px = got
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
    print(
        "M3_PIXELS_OK %d %.4f %.2f %.2f %.2f"
        % (distinct, frac, rsum / n, gsum / n, bsum / n)
    )
    return 0 if (distinct == 1 and frac == 1.0) else 1


def gradient(path):
    got, err = load(path)
    if err:
        print(err)
        return 1
    w, h, px = got
    n = w * h
    cnt = Counter()
    bad_rows = 0
    match = 0
    for y in range(h):
        want = row_color(y, h)
        ok = True
        for x in range(w):
            k = y * w + x
            got_px = (px[3 * k], px[3 * k + 1], px[3 * k + 2])
            cnt[got_px] += 1
            if got_px == want:
                match += 1
            else:
                ok = False
        if not ok:
            bad_rows += 1
    print(
        "M3_GRADIENT_OK %d %.4f %d" % (len(cnt), match / n, bad_rows)
    )
    return 0 if (match == n) else 1


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["--gradient"]:
        sys.exit(gradient(args[1]))
    sys.exit(flat(args[0]))
