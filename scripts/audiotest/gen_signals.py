# Test signals for the soundapp_r offline render harness.
# Writes 48 kHz stereo 16-bit WAVs into the given directory:
#   sine_1k_10s.wav    1 kHz sine, 10 s, 0.5 amplitude
#   sweep_10s.wav      logarithmic 20 Hz -> 20 kHz chirp, 10 s
#   clicks_10s.wav     single-sample impulse every 250 ms, 10 s
import sys
import os
import wave

import numpy as np

RATE = 48000
DUR = 10.0


def write_wav(path, stereo):
    xi = (np.clip(stereo, -1.0, 1.0) * 32767.0).astype("<i2")
    with wave.open(path, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(xi.tobytes())


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "target/audiotest"
    os.makedirs(out_dir, exist_ok=True)
    n = int(RATE * DUR)
    t = np.arange(n) / RATE

    sine = 0.5 * np.sin(2.0 * np.pi * 1000.0 * t)
    write_wav(os.path.join(out_dir, "sine_1k_10s.wav"), np.column_stack([sine, sine]))

    f0, f1 = 20.0, 20000.0
    k = np.log(f1 / f0) / DUR
    phase = 2.0 * np.pi * f0 * (np.exp(k * t) - 1.0) / k
    sweep = 0.5 * np.sin(phase)
    write_wav(os.path.join(out_dir, "sweep_10s.wav"), np.column_stack([sweep, sweep]))

    clicks = np.zeros(n)
    period = int(0.25 * RATE)
    clicks[::period] = 0.9
    write_wav(os.path.join(out_dir, "clicks_10s.wav"), np.column_stack([clicks, clicks]))

    print("wrote sine_1k_10s.wav sweep_10s.wav clicks_10s.wav into " + out_dir)


if __name__ == "__main__":
    main()
