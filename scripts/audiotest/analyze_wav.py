# Compact numeric analysis of rendered WAVs from soundapp_r --render.
# One line per file, no raw sample dumps.
#
#   python analyze_wav.py FILE --type sine --pitch 5 --tempo 0.8 --in-dur 10.0
#   python analyze_wav.py FILE --type clicks --tempo 0.5 --in-dur 10.0 --src-rms-db -13
#   python analyze_wav.py FILE --type music --tempo 1.25 --in-dur 9.9 --src-rms-db -18
#
# Metrics: duration ratio vs expected; dominant frequency + cents error (sine);
# THD+N dB relative to the fundamental (sine); spurious spectral peaks > -60 dBFS;
# discontinuity count (sample-delta outliers, click neighborhoods excluded for
# click trains); RMS dBFS; click-train transient smearing (ms) and echo count.
import argparse
import sys

import numpy as np
from scipy.io import wavfile
from scipy.signal import find_peaks


def load(path):
    rate, data = wavfile.read(path)
    if data.dtype == np.int16:
        x = data.astype(np.float64) / 32768.0
    elif data.dtype == np.int32:
        x = data.astype(np.float64) / 2147483648.0
    elif data.dtype == np.uint8:
        x = (data.astype(np.float64) - 128.0) / 128.0
    else:
        x = data.astype(np.float64)
    if x.ndim == 1:
        x = np.column_stack([x, x])
    return rate, x


def db(x):
    return 20.0 * np.log10(max(x, 1e-12))


def analyze(path, args):
    rate, x = load(path)
    mono = x.mean(axis=1)
    n = len(mono)
    dur = n / rate
    out = {"file": path.replace("\\", "/").split("/")[-1]}

    tempo = args.tempo
    expect_dur = args.in_dur / tempo if args.in_dur else dur
    out["dur"] = f"{dur:7.2f}s"
    out["dur_ratio"] = dur / expect_dur if expect_dur > 0 else float("nan")

    # Analysis window: middle third, capped at 4 s.
    wlen = min(n // 3, 4 * rate)
    s = (n - wlen) // 2
    seg = mono[s : s + wlen]

    rms = np.sqrt(np.mean(np.square(x)))
    out["rms_db"] = db(rms)

    if args.type in ("sine", "clicks"):
        win = np.hanning(wlen)
        spec = np.fft.rfft(seg * win)
        mag = np.abs(spec)
        freqs = np.fft.rfftfreq(wlen, 1.0 / rate)
        f_exp = args.f0 * (2.0 ** (args.pitch / 12.0))

        if args.type == "sine":
            peak_i = int(np.argmax(mag[1:]) + 1)
            f_dom = freqs[peak_i]
            out["f_dom"] = f_dom
            out["cents"] = 1200.0 * np.log2(f_dom / f_exp) if f_exp > 0 else float("nan")

            # Fundamental cluster: +/- 0.5% + 4 bins around the dominant peak.
            tol_hz = max(0.005 * f_dom, 5 * rate / wlen)
            cluster = np.abs(freqs - f_dom) <= tol_hz
            p_tot = float(np.sum(mag[1:] ** 2))
            p_sig = float(np.sum(mag[cluster] ** 2))
            resid = max(p_tot - p_sig, 1e-24)
            out["thdn_db"] = 10.0 * np.log10(resid / p_sig) if p_sig > 0 else float("nan")

            # Spurious peaks above -60 dBFS, outside the fundamental cluster,
            # between 20 Hz and 20 kHz.
            cg = 0.5  # Hann coherent gain
            dbfs = 20.0 * np.log10(2.0 * mag / (wlen * cg) + 1e-15)
            band = (freqs >= 20.0) & (freqs <= 20000.0)
            peaks, _ = find_peaks(dbfs)
            spur = [
                i
                for i in peaks
                if band[i] and dbfs[i] > -60.0 and not cluster[i]
            ]
            out["spur"] = len(spur)
            out["spur_max_db"] = dbfs[spur[0]] if spur else float("nan")

    # Discontinuities: |x[n]-x[n-1]| outliers vs robust local scale.
    d = np.abs(np.diff(mono))
    med = np.median(d[d > 0]) if np.any(d > 0) else 1e-6
    thr = max(25.0 * med, 5e-3)
    bad = d > thr
    if args.type == "clicks":
        # Exclude +-4 ms around each expected click (a click is a legit delta).
        period_out = args.click_period / tempo
        t = np.arange(n) / rate
        for tc in np.arange(period_out, dur, period_out):
            bad[(t[:-1] >= tc - 0.004) & (t[:-1] <= tc + 0.004)] = False
    # Collapse runs of adjacent flagged samples into single events.
    events = 0
    prev = False
    for b in bad:
        if b and not prev:
            events += 1
        prev = b
    out["disc"] = events
    out["delta_thr"] = thr

    if args.type == "clicks":
        period_out = args.click_period / tempo
        spreads, echoes = [], []
        for tc in np.arange(period_out, dur - 0.3, period_out):
            a = max(0, int((tc - 0.08) * rate))
            b = min(n, int((tc + 0.08) * rate))
            env = np.abs(mono[a:b])
            if env.size == 0 or env.max() < 1e-4:
                continue
            above = env > 0.01 * env.max()
            idx = np.where(above)[0]
            if idx.size:
                spreads.append((idx[-1] - idx[0]) * 1000.0 / rate)
            pk, _ = find_peaks(env, height=0.1 * env.max(), distance=int(0.002 * rate))
            echoes.append(max(0, len(pk) - 1))
        out["smear_ms"] = float(np.median(spreads)) if spreads else float("nan")
        out["echoes"] = int(np.median(echoes)) if echoes else -1
        out["n_clicks"] = len(spreads)

    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("file")
    ap.add_argument("--type", default="sine", choices=["sine", "clicks", "music", "tape"])
    ap.add_argument("--pitch", type=float, default=0.0)
    ap.add_argument("--tempo", type=float, default=1.0)
    ap.add_argument("--f0", type=float, default=1000.0)
    ap.add_argument("--in-dur", type=float, default=0.0)
    ap.add_argument("--src-rms-db", type=float, default=float("nan"))
    ap.add_argument("--click-period", type=float, default=0.25)
    args = ap.parse_args()

    r = analyze(args.file, args)

    def g(k, fmt):
        v = r.get(k, None)
        return fmt.format(v) if v is not None else "-"

    line = (
        "{file:32s} dur {dur} x{dur_ratio:5.3f}".format(
            file=r["file"][:32], dur=r["dur"], dur_ratio=r["dur_ratio"]
        )
    )
    if "f_dom" in r:
        line += " | f {f_dom:8.1f}Hz ({cents:+6.0f}c)".format(f_dom=r["f_dom"], cents=r["cents"])
        line += " | THD+N {thdn_db:7.1f}dB".format(thdn_db=r["thdn_db"])
        line += " | spur {spur:3d} (max {spur_max_db:6.1f}dB)".format(
            spur=r["spur"], spur_max_db=(r["spur_max_db"] if r["spur"] else float("-inf"))
        )
    line += " | disc {disc:4d}".format(disc=r["disc"])
    if "smear_ms" in r:
        line += " | smear {smear_ms:6.1f}ms echoes {echoes:2d} (n={n_clicks})".format(
            smear_ms=r["smear_ms"], echoes=r["echoes"], n_clicks=r["n_clicks"]
        )
    line += " | rms {rms_db:6.1f}dBFS".format(rms_db=r["rms_db"])
    if not np.isnan(args.src_rms_db):
        line += " (src {src:6.1f}, delta {delta:+6.1f})".format(
            src=args.src_rms_db, delta=r["rms_db"] - args.src_rms_db
        )
    print(line)


if __name__ == "__main__":
    main()
