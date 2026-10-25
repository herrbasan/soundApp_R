"""Measure the DWM composition offset between the app framebuffer and a screen capture.

Positive dy = composed content sits LOWER than the framebuffer; positive dx = further RIGHT.
Kept as a standalone script so no heavy image work runs inside the editor host.
"""
import sys
from PIL import Image


def profiles(path, w, h):
    im = Image.open(path).convert("L")
    px = im.load()
    rows = []
    for y in range(h):
        s = 0
        n = 0
        for x in range(0, w, 6):
            s += px[x, y]
            n += 1
        rows.append(s / n)
    cols = []
    for x in range(w):
        s = 0
        n = 0
        for y in range(0, h, 6):
            s += px[x, y]
            n += 1
        cols.append(s / n)
    return rows, cols


def best_shift(ref, shot, rng):
    n = len(ref)
    m = len(shot)
    best, best_score = 0, float("inf")
    for d in range(-rng, rng + 1):
        lo = max(40, -d)
        hi = min(n - 40, m - d)
        if hi - lo < 100:
            continue
        s = 0.0
        for i in range(lo, hi):
            s += abs(ref[i] - shot[i + d])
        sc = s / (hi - lo)
        if sc < best_score:
            best_score, best = sc, d
    return best, best_score


def main():
    ref_path, shot_path = sys.argv[1], sys.argv[2]
    out_path = sys.argv[3] if len(sys.argv) > 3 else None

    a = Image.open(ref_path).convert("L")
    b = Image.open(shot_path).convert("L")
    w = min(a.width, b.width)
    h = min(a.height, b.height)

    ar, ac = profiles(ref_path, w, h)
    br, bc = profiles(shot_path, w, h)
    shot_gray = b.convert("L").tobytes()
    dy, dy_res = best_shift(ar, br, 80)
    dx, dx_res = best_shift(ac, bc, 80)

    pa, pb = a.load(), b.load()

    def mean_diff(ox, oy):
        s = 0.0
        n = 0
        for y in range(40, h - 100, 6):
            for x in range(40, w - 40, 6):
                sx, sy = x + ox, y + oy
                if 0 <= sx < w and 0 <= sy < h:
                    s += abs(pa[x, y] - pb[sx, sy])
                    n += 1
        return s / n if n else -1.0

    band = 0
    for y in range(h):
        if br[y] > 8:
            band = y
            break

    # A capture of an empty/unpainted window has no structure; never report a shift for it.
    mean = sum(shot_gray) / len(shot_gray)
    var = sum((v - mean) ** 2 for v in shot_gray) / len(shot_gray)
    stdev = var ** 0.5
    verdict = "BLANK CAPTURE - measurement void" if stdev < 3.0 else "ok"

    lines = [
        f"ref  {a.width}x{a.height}",
        f"shot {b.width}x{b.height}",
        f"vertical   dy = {dy:+d}  (residual {dy_res:.2f})",
        f"horizontal dx = {dx:+d}  (residual {dx_res:.2f})",
        f"meanAbsDiff at (0,0)     = {mean_diff(0, 0):.2f}",
        f"meanAbsDiff at (dx,dy)   = {mean_diff(dx, dy):.2f}",
        f"composed pure-black top band = {band} px",
        f"shot luminance mean={mean:.1f} stdev={stdev:.2f}",
        f"verdict: {verdict}",
    ]
    text = "\n".join(lines)
    print(text)
    if out_path:
        with open(out_path, "w", encoding="utf-8") as f:
            f.write(text + "\n")


if __name__ == "__main__":
    main()
