"""Scan a f32 WAV's left channel: RMS per 250 ms bucket (debug helper)."""
import struct
import sys

path = sys.argv[1]
raw = open(path, "rb").read()
assert raw[0:4] == b"RIFF" and raw[8:12] == b"WAVE", "not a WAV"
pos = 12
fmt = None
data = None
while pos + 8 <= len(raw):
    cid = raw[pos : pos + 4]
    clen = struct.unpack("<I", raw[pos + 4 : pos + 8])[0]
    body = raw[pos + 8 : pos + 8 + clen]
    print(f"# chunk {cid} @{pos} len {clen}", file=sys.stderr)
    if cid == b"fmt ":
        fmt = struct.unpack("<HHIIHH", body[:16])
    elif cid == b"data":
        data = body
    pos += 8 + clen + (clen & 1)
ch = fmt[1]
rate = fmt[2]
print(f"# fmt: tag={fmt[0]} ch={ch} rate={rate} bits={fmt[5]} data_len={len(data)}", file=sys.stderr)
if fmt[0] == 3 and fmt[5] == 32:
    samples = struct.unpack(f"<{len(data) // 4}f", data)
elif fmt[0] == 1 and fmt[5] == 16:
    samples = [s / 32768.0 for s in struct.unpack(f"<{len(data) // 2}h", data)]
else:
    raise SystemExit(f"unsupported wav: tag={fmt[0]} bits={fmt[5]}")
left = samples[0::ch]
bucket = rate // 4
import math

for i in range(0, len(left) - bucket, bucket):
    w_ = left[i : i + bucket]
    rms = (sum(v * v for v in w_) / len(w_)) ** 0.5
    db = 20 * math.log10(rms) if rms > 0 else -240.0
    peak = max(abs(v) for v in w_)
    print(f"{i / rate:6.2f}s  rms {db:7.1f} dB  peak {peak:.4f}")
