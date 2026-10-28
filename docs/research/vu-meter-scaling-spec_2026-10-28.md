# Level-Meter Scaling for `soundApp_R` — Standards Review + Recommended Mapping Spec

## 0. Executive summary

Your instinct was half right and half a red herring, and the two halves live in different parts of the code.

**The stem meters are the "always hits the max" bug, and it is not a dB-scale problem at all.** `crates/soundapp/src/main.rs:5468` draws each of the 20 stem bars as `(engine.meter(self.mixer_stems[i].slot) * 6.0).clamp(0.0, 1.0)` — a **linear amplitude × 6**, hard-clamped. `crates/soundapp/src/mixer.rs:618-620` stores that value as a raw linear peak (`left.abs().max(right.abs())`). The bar therefore pins at **any stem peak ≥ −15.6 dBFS** and reads 38 % at −24 dBFS, 19 % at −30 dBFS. That is a pure unit error, and it fully explains "bass heavy tracks almost always hit the max" — a bass-heavy stem is loud *and* has high crest factor, so it saturates almost always.

**The spectrum display is tilted the wrong way for the same reason.** `crates/soundapp/src/dsp.rs:490-497` takes the **maximum** FFT bin magnitude per band (`max`, not power sum), over **integer-snapped** band edges (`dsp.rs:296-300`). Measured against a pink-noise reference: the current code tilts **+10.8 dB** from band 0 → band 32 and **+23.7 dB** from band 0 → band 63, because the bottom 7 bands (20–38 Hz) all read the *same* FFT bin (there are only **55 distinct band→bin sets** out of 64) and narrow low bands read a raw partial while wide high bands read one bin out of thirteen.

**On the "quiet parts hit higher than expected" complaint**, the dB scale itself is not wrong — the Monitoring VU bar is already dB-linear over −60…0 (`main.rs:591`). What is missing is (a) an **anchor**: there is no reference mark anywhere, so nothing on the meter means anything; (b) **fixed-time ballistics**: the RMS window is "all frames since the last redraw" (`dsp.rs:433`), i.e. 16 ms at 60 fps, 67 ms at 15 fps, and the peak decays by ×0.86 *per redraw* (`dsp.rs:428`) — so the same signal meters differently at a different frame rate; and (c) a **phantom level when paused**: the analyzer is fed a synthetic chirp (`main.rs:4669`), which reads −12…−18 dBFS RMS ≈ 70–80 % of the bar.

**Do not use BS.1770 LUFS to drive the stereo VU bars.** LUFS is a mono-summed, K-weighted *programme-loudness* metric with gates; it cannot represent per-channel or per-stem level, and K-weighting only cuts 31.5 Hz by −8.5 dB, so it does not solve the bass problem. Keep it where it already is (the master loudness readout) and fix the bar's integration window.

Everything below is verified against the code and against numerical simulations; all numbers are reproduced at the end.

---

## 1. The three standards, and which one a small mixer should copy

| | **VU** (ANSI C16.5-1942 / IEC 60268-17) | **PPM** (IEC 60268-18 digital, IEC 60268-10 analogue) | **LUFS** (ITU-R BS.1770-4/-5) | **dBFS** (AES17 / IEC 61606) |
|---|---|---|---|---|
| What it reads | **average rectified** voltage (not RMS, not peak) | **quasi-peak** (true peak with fixed integration) | **K-weighted programme loudness**, gated | absolute digital level |
| Rise / fall | **300 ms** to 99 %, 300 ms fall, 1–1.5 % overshoot | **10 ms** integration, **20 dB fall in 1.7 s** | momentary 400 ms, short-term 3 s, integrated = 400 ms blocks, 75 % overlap | — |
| Scale | −20 … +3 VU, **0 VU at 71 % of scale**, linear in voltage | 4 dB/division, divisions 1–7 (24 dB), "6" = intended max, over-range lamp | dB-like, working range ≈ −40 … −6 LUFS | linear in dB |
| Reference | 0 VU = **+4 dBu** = 1.228 V RMS into 600 Ω | digital alignment is implementation-defined | 0 LU = K-weighted full scale, offset **−0.691** | **0 dBFS = RMS of a full-scale sine** (AES17) |
| Digital alignment | **EBU R68: 0 VU ≡ −18 dBFS** (SMPTE RP 155: −20 dBFS) | — | −0.691 offset | — |
| Typical targets | −18 dBFS ≈ 0 VU ≈ "broadcast alignment" | — | EBU R128 = **−23 LUFS**; ATSC A/85 = **−24 LKFS**; streaming ≈ **−14 LUFS** (Apple Music ≈ −16) | 0 dBFS = clipping |

Sources: ANSI C16.5-1942 / IEC 60268-17; IEC 60268-18; ITU-R BS.1770-4 & -5; EBU Tech 3341 & EBU R68 (alignment) & EBU R128 (loudness); SMPTE RP 155; AES17; IEC 61606; ATSC A/85.

**Verdict for a small desktop mixer:**

- **VU is the wrong instrument for the bars.** It is a *broadcast-alignment* instrument: it reads average rectified voltage with a 300 ms needle, its 0 VU point is tied to −18 dBFS by convention, and its −20…+3 VU face is a compressed voltage taper, not a digital scale. It also reads ≈0.9 dB *below* an RMS meter for a sine (0.6366·A vs 0.7071·A) and up to ~1 dB below on dense music — so a meter labelled "VU" that is really RMS will not agree with a real VU meter.
- **PPM is the right instrument for the *peak* element only** — it is the peak/overload indicator, and its 10 ms + 20 dB/1.7 s ballistics are exactly the "fast peak, slow fall, holds your attention" behaviour a mixer wants.
- **LUFS is the right instrument for exactly one readout**: "is this programme loud enough for the delivery target?" The app already has that (`main.rs:5242-5260`).
- **What professional mixers actually show** — Pro Tools, Logic, Cubase, Reaper, and hardware digital consoles — is a **hybrid**: a fast **peak** bar with a **peak-hold**, plus a **separate RMS/average readout or secondary bar**, on a **dBFS scale**, with a **clip indicator**. Some add a separate VU-calibrated scale (e.g. SSL, API, most consoles still offer a VU option) but that is a secondary "gain-riding" reference, not the primary meter.

**So: copy the DAW pattern — dBFS peak bar with hold + RMS body + clip LED + one labelled reference mark. Do not copy a literal VU.**

---

## 2. Peak vs RMS, and ballistics

| Element | Shows | Why |
|---|---|---|
| **Peak / quasi-peak** | instantaneous maximum sample excursion | the only thing that predicts clipping and inter-sample overs |
| **Peak hold** | max over 1–2 s, then slow fall | you must *see* a transient that flashed past |
| **RMS (short-term)** | average level over 300 ms | what "loudness" means for gain setting and for mixing balance |
| **True peak (dBTP)** | 4× oversampled peak, BS.1770-4 Annex 2 | inter-sample peaks that a sample-peak meter misses |
| **LUFS** | K-weighted gated programme loudness | delivery-target compliance only |

Standard ballistics worth copying:

- **PPM / quasi-peak (IEC 60268-18):** 10 ms integration, **20 dB fall in 1.7 s** (≈ 11.8 dB/s). This is the canonical "peak with hold" behaviour.
- **VU (IEC 60268-17):** 99 % of final value in **300 ms**, fall in 300 ms, 1–1.5 % overshoot.
- **DAW meters:** peak attack essentially instant; peak indicator holds ~1–2 s then falls at ~20 dB/s; the RMS number updates on a 100–300 ms window.

**What the app does today and why it misleads**

| Location | Today | Problem |
|---|---|---|
| `dsp.rs:433` (`vu_l_db`) | RMS over the frames fed **since the last redraw** | window is 16 ms @60 fps, 67 ms @15 fps → the "VU" moves with the UI frame rate |
| `dsp.rs:428-430` (`peak_l_lin *= 0.86`) | peak decays **per redraw** | 0.86⁶⁰ ≈ 1.2·10⁻⁴ per second at 60 fps (instant vanish); 0.86¹⁵ ≈ 0.10 per second at 15 fps → peak-hold behaviour is frame-rate dependent, and there is **no hold at all** |
| `mixer.rs:572` (`levels[index] *= 0.985`) | decays **per audio callback** | −6.2 dB/s at 1024-frame buffers, −24.6 dB/s at 256-frame buffers → the same mix meters differently on different buffer sizes |
| `dsp.rs:226-228` (`to_db`) | `20·log10(|amp|)` | correct for the peak element; but bar = RMS and tick = peak, so for one full-scale sine the bar reads −3.01 dB and the tick 0.00 dB. That 3.01 dB is *correct* (it is the crest factor of a sine) — but it is **undocumented on the meter**, so it reads as an error |
| `dsp.rs:232-238` (`true_peak_update`) | envelope decaying ×0.999/sample (τ ≈ 21 ms), **no oversampling** | this is *not* a true peak and *not* a hold; the "TRUE PEAK" label at `main.rs:5260` is inaccurate |
| `main.rs:4669` | synthetic frames injected into the analyzer when the tap is empty | a paused player shows −12…−18 dBFS (bar ≈ 70–81 %) of fake signal |

---

## 3. How a spectrum display is actually built (and why yours pins at LF)

### 3.1 There are two different things you can display

| Display | Definition | White noise | Pink noise |
|---|---|---|---|
| **FFT magnitude / spectral density** (dB vs log f) | magnitude per bin | **flat** | **−3 dB/octave** slope |
| **Fractional-octave band level** (power integrated over each band) | √(∫ band) | **+3 dB/octave** (narrow low bands hold less energy) | **flat** ← the classic reference |

A professional RTA in "1/3-octave" mode shows the **band level**, which is why **pink noise is the flat reference curve**. That is the sanity test I used below.

### 3.2 Why raw FFT magnitudes pin at the low end — four independent causes

1. **Spectral tilt.** Real music is roughly pink (≈ −3 dB/octave); per-octave *band levels* are equal, so the low end genuinely carries as much band energy as the high end. Raw per-bin magnitudes tilt down 3 dB/octave, and any max-of-bin aggregation re-tilts it.
2. **Aggregation.** Taking `max` over the bins in a band is not a band level. A band with 13 bins shows the single loudest partial; a band with 1 bin shows the whole band. Maximum-of-N grows like √(ln N), so **wide high bands are under-read and narrow low bands over-read**.
3. **Bandwidth / ENBW mismatch.** A Hann window's ENBW is **1.5 bins** (35.2 Hz at 48 kHz / 2048). Band 0 is nominally 2.3 Hz wide. Comparing a 2.3 Hz band against a 35 Hz analysis kernel biases the low bands by up to **+11.9 dB**.
4. **Integer band edges.** `dsp.rs:296-300` uses `floor(f_lo)` / `ceil(f_hi)`, so consecutive bands **share** the edge bin, and the bottom bands collapse: **bands 0–6 (20–38 Hz) all read bin 1–2** — seven identical bars, and only **55 distinct band→bin sets** exist for 64 bands.

Your normalisation constant, by contrast, is **correct**: `dsp.rs:457` `norm = 1/(FFT_SIZE*0.25)` = 0.001953 vs the textbook `2/(N·CG)` = 0.001954 — a ratio of 1.000, i.e. **0.00 dB error**. A full-scale sine reads 0.00 dBFS in your scaling. Do not touch it.

### 3.3 Per-band vs global normalisation

Use **absolute dBFS band level** as the physical quantity. Do **not** auto-normalise per band (that destroys the information — every band would read the same). Two display modes are standard:

- **Absolute (level) mode:** band level in dBFS. Self-calibrating: a full-scale sine reads 0.00 dBFS at the top of the scale.
- **Relative (balance) mode:** `band_db − max(band_db)` in the frame, so the display shows spectral *shape* only. Useful for comparing two stems; useless for setting gain.

### 3.4 A/C/K weighting in a spectrum display

- **A-weighting (IEC 61672)** is the right choice for a *"how will this sound / how loud does it feel"* overlay. Per-band offsets (dB): −52.4 @20 Hz, −41.5 @31.5, −21.1 @100, −2.0 @1 k, ≈ +1.0 @4 k, −3.2 @8 k, −11.4 @20 k. Applying it makes bass-heavy content read as less dominant, because A-weighting removes −39 dB at 31.5 Hz.
- **K-weighting (BS.1770)** is *not* a spectrum weighting — it is the loudness weighting, and it only removes **−8.5 dB at 31.5 Hz**. Your own biquads at `dsp.rs:305-309` are the BS.1770-4 K pair (verified: −3.93 dB @50 Hz, +3.97 dB @4 kHz). Using K for the display would leave the bass problem essentially untouched.
- **Z / flat** is for raw analysis. Leave that as the default; offer **A** as an overlay.

---

## 4. Should BS.1770 LUFS drive the stereo VU bars?

**No.** Four reasons, all verifiable in your own code:

1. **It is a programme measure, not a channel measure.** BS.1770 sums the channel powers (`z = Σ Gᵢzᵢ` with G = 1.0) and your implementation does the same (`dsp.rs:601-602`). You get **one number for L+R**. Your stereo VU bars need L and R separately; LUFS structurally cannot supply them.
2. **It is gated.** BS.1770's momentary value is a 400 ms K-weighted mean with an absolute gate at −70 LUFS and a relative gate at −10 LU (`dsp.rs:559-576`). A gate is a *compliance* concept; a level meter must never blank out.
3. **K-weighting barely touches bass.** −8.5 dB @31.5 Hz, −3.9 dB @50 Hz. A bass-heavy track still reads near-full on LUFS. It does not address the complaint.
4. **The reference scale is wrong.** LUFS is read against delivery targets (−14 / −23 / −24), with a useful range of ≈ −40…−6 LUFS. Your bars are dBFS-referenced over −60…0. Mixing them on one face produces numbers that mean different things.

**Keep LUFS where it already is.** Your LUFS implementation is structurally correct: offset −0.691 (`dsp.rs:30`), K-weighting pair (`dsp.rs:305-309`), momentary 400 ms (`dsp.rs:311`), short-term 3 s (`dsp.rs:312`), channel sum and gating (`dsp.rs:559-576`, `dsp.rs:601-602`).

> **One real defect to fix while you are there:** `dsp.rs:349` sets `block_len: (rate * 0.1)` — 100 ms **non-overlapping** blocks for the integrated measurement. BS.1770-4 requires **400 ms blocks with 75 % overlap** (100 ms step). Change `block_len` to `rate * 0.4` and emit a block every `rate * 0.1` samples.

---

## 5. Recommended mapping spec

### 5.1 Meter scale definition (both the stereo L/R pair and every stem)

```
SCALE_TOP_DBFS  =  0.0
SCALE_FLOOR_DBFS= -60.0
SCALE_SPAN_DB   =  60.0
VU_REF_DBFS     = -18.0        // EBU R68: 0 VU  ==  -18 dBFS RMS

// dB reference: peak-referenced dBFS (AES17 convention: a full-scale sine
// has peak 0 dBFS and RMS -3.01 dBFS)
db(amp) = 20.0 * log10(max(amp, 1e-12))

// normalised height in 0..1  (linear in dB, floor at -60)
u(db) = clamp((db - SCALE_FLOOR_DBFS) / SCALE_SPAN_DB, 0.0, 1.0)
      = clamp((db + 60.0) / 60.0, 0.0, 1.0)
```

### 5.2 Two elements per meter (this is the key structural change)

```
// ---- BODY: RMS, VU-class 300 ms integration -------------------------
// one-pole on mean-square; tau = 77 ms  =>  99 % in 300 ms (rise),
//                                        99 % fall in 300 ms
ms   += (x*x - ms) * (1.0 - exp(-1.0 / (0.077 * fs)))
rms    = sqrt(ms)
rms_db = db(rms)                 // full-scale sine -> -3.01 dBFS

// ---- TICK: sample peak with PPM-style hold --------------------------
peak_lin = max(peak_lin, |x|)                       // instant attack
hold_s += dt;  if hold_s > 1.5 { hold_s = 0; }      // 1.5 s hold
// after hold expires, decay in the dB domain at 20 dB per 1.7 s (11.76 dB/s):
peak_db = max(db(peak_lin), peak_db - 11.76 * dt)

// ---- CLIP LED -------------------------------------------------------
clip = clip || (|x| >= 0.9990)     // -0.0087 dBFS; latched, manual reset
```

Heights:

```
u_body = u(rms_db)
u_tick = u(peak_db)
```

### 5.3 Scale furniture and zones

Keep your existing zone boundaries (they are the conventional digital channel-strip zoning and you already draw them at `main.rs:607-609`):

```
green   -60 .. -12 dBFS
amber   -12 ..  -3 dBFS
red     -3  ..   0 dBFS
```

**Add one labelled reference mark that is missing today** — this is what makes the meter mean something:

```
0 VU reference line at -18 dBFS (EBU R68)  -> u = ( -18 + 60 ) / 60 = 0.700
```

Label it "0 VU" / "−18". It gives every other reading a meaning ("this stem is 6 dB under alignment", "this stem is 4 dB over 0 VU"). Your existing scale labels are 0/−12/−24/−36/−48 (`main.rs:5196-5206`); add the −18 mark alongside them.

### 5.4 Optional numeric readout

Add a small numeric field next to each meter showing `rms_db` and `peak_db` to 0.1 dB. This is what DAWs do and it removes all remaining ambiguity ("even quieter parts hit higher than I would expect" is a *readability* problem as much as a scaling problem).

### 5.5 Per-stem mixer bars — the actual bug fix

Replace `main.rs:5468`:

```rust
// BEFORE — linear amplitude x 6, clamped. Pins at any peak >= -15.6 dBFS.
let lvl = (engine.meter(self.mixer_stems[i].slot) * 6.0).clamp(0.0, 1.0);

// AFTER — dBFS scale, identical to the master meter.
let (rms_db, peak_db) = engine.meter_db(self.mixer_stems[i].slot); // dBFS
let lvl = u(rms_db);            // body height
let tick = u(peak_db);          // peak-hold tick
```

`mixer.rs` must therefore keep, per stem: a 300 ms mean-square accumulator (per channel, if you want true stereo stems) and a peak with a 1.5 s hold, instead of today's single linear peak with a per-callback `*= 0.985` (`mixer.rs:572`, `mixer.rs:618-620`). Also replace that per-callback decay with the **time-based** ballistics above.

Note that today the mixer stores only `max(|L|,|R|)` (`mixer.rs:618`) — one mono bar per stem. If you want genuine **stereo per-stem** VU, keep L and R separately (2 × RMS + 2 × peak per stem = 160 accumulators for 20 stems; trivial cost).

### 5.6 Spectrum — 64 bands

**Step 1 (band formation).** Replace max-of-bin with a **fractional-edge power sum**, divided by the window ENBW:

```
// per bin: a_k = amplitude of a sinusoid at bin k that would produce X_k
a_k = 2 * |X_k| / (N * CG)          // CG = 0.4998 for Hann ; N*0.25 in the app is equivalent
p_k = a_k^2

// per band, with FRACTIONAL edges (no integer snapping):
band_power = sum over k of  (overlap_width(k) / df) * p_k      // uniform density within the bin
band_level = sqrt( band_power / ENBW )                        // ENBW = 1.5 for Hann
band_db    = 20 * log10(max(band_level, 1e-12))               // dBFS
```

**Step 2 (FFT length).** Raise `FFT_SIZE` from 2048 to **8192** (`dsp.rs:19`). At 48 kHz that is 5.86 Hz bins (170 ms display latency — fine). Below ~100 Hz a 2048-point FFT simply does not have the resolution, and no amount of arithmetic fixes that.

**Step 3 (sample rate).** `main.rs:2600` hardcodes `DspAnalyzer::new(48_000)` while the device rate comes from `config.sample_rate()` (`audio.rs:610`). At 44.1 kHz the bin→Hz mapping, the K-weighting coefficients (defined for 48 kHz) and the integrated-LUFS block length are all wrong. Pass the real rate.

**Step 4 (input).** `dsp.rs:371` feeds the mono sum `(l + r) * 0.5` into the FFT. A hard-panned stem loses 6 dB and out-of-phase content cancels. Either compute the spectrum per channel, or keep the sum and label it "M".

**Step 5 (mapping).** Band levels under the corrected algorithm sit far below 0 dBFS — for pink noise at unit mean-square, each of the 64 bands measures ≈ **−16.7 dBFS**; for real music at −14 dBFS overall RMS, band levels land around **−30 dBFS** (bass-heavy: −20…−25 dBFS; HF: −40…−45 dBFS). Therefore:

```
// absolute mode (default)
u_band = clamp((band_db + 60.0) / 60.0, 0.0, 1.0)     // -60..0 dBFS, linear in dB
// optional relative / balance mode
u_band = clamp((band_db - max_band_db + 48.0) / 48.0, 0.0, 1.0)   // -48..0 dB relative
```

The existing renderer at `main.rs:4916` already computes `((snap.spectrum_db[i] + 60.0) / 60.0)` — **no rendering change is needed**; only the band values change (they become ~16 dB lower at LF and ~13 dB higher at HF, which is exactly the correction you want).

**Step 6 (optional A-weighting overlay).** Add the IEC 61672 A-weighting offset per band:

```
A(f) = 20*log10( (12194^2 * f^4) /
       ((f^2+20.6^2) * sqrt((f^2+107.7^2)(f^2+737.9^2)) * (f^2+12194^2)) ) + 2.00
```
with `f` in Hz; at 1 kHz this gives 0 dB by construction. Apply to `band_db` only for display.

**Alternative if you would rather not lengthen the FFT:** a **64-band constant-Q filterbank** (one 2nd-order bandpass per band, `f₀ = √(f_lo·f_hi)`, `Q = f₀/(f_hi−f_lo)`, peak gain 1) is what hardware analysers use. It measures band levels exactly (pink-flat to −4.6 dB, full-scale sine = −3.01 dBFS RMS), needs no FFT, and has no bin-edge artefacts. Cost: 64 biquads at 48 kHz ≈ 3 M sections/s — negligible. **But** the FFT fix above is a much smaller change and measured better (pink-flat to −0.5 dB at N = 8192), so **I recommend the FFT route**.

### 5.7 Cleanups that fall out of the above

| Item | Location | Change |
|---|---|---|
| Synthetic fallback shows a phantom level | `main.rs:4669` (`synth_dsp_frames`, `main.rs:467-478`) | feed silence, or skip the redraw entirely, when the tap is empty |
| `true_peak_update` is not a true peak | `dsp.rs:232-238`, label at `main.rs:5260` | either implement BS.1770-4 Annex 2 (4× oversample + LP) or rename the readout to "PEAK" |
| Integrated LUFS block size | `dsp.rs:349` | `rate * 0.4` block, emitted every `rate * 0.1` samples |
| Hardcoded analyzer rate | `main.rs:2600` | use `config.sample_rate()` |
| Tracker-channel VU | `tracker.rs:299-308`, `tracker.rs:800` | if you want module channels metered consistently with stems, reuse the same dBFS scale |

---

## 6. Why this is the right answer (one-line summary per standard)

- **VU** tells you *where the needle rests* in a broadcast-alignment world; its 0 VU is −18 dBFS and its face is a compressed voltage taper. Use it as the **reference mark** (−18 dBFS), not as the meter.
- **PPM** tells you *did I clip*; its 10 ms integration and 20 dB/1.7 s fall are exactly the peak-hold behaviour to copy.
- **LUFS** tells you *will this make the platform happy*; keep it as the master loudness number.
- **Your meter** should be a dBFS **peak-with-hold tick + 300 ms RMS body + clip LED + a 0 VU mark at −18 dBFS**, on a **linear-in-dB −60…0 scale**, with **time-based** ballistics.
- **Your spectrum** should be a **fractional-octave band level** (power-summed over fractional bin edges, ÷ ENBW, N = 8192) in **absolute dBFS**, optionally with an **A-weighting** overlay — not raw bin maxima.

---

## 7. Numerical verification (all figures in this report)

Reproduced with NumPy/SciPy at 48 kHz, 64 log bands 20 Hz–20 kHz.

| Measurement | Value |
|---|---|
| Stem bar pins at linear peak ≥ 1/6 = 0.16667 | **−15.6 dBFS** |
| Stem bar reading at −24 dBFS / −30 dBFS peak | 38 % / 19 % |
| Distinct band→bin sets among 64 bands (app, N = 2048) | **55**; bands 0–6 all → bins (1,2) |
| App `norm` (0.001953) vs textbook `2/(N·CG)` (0.001954) | ratio 1.000 → **0.00 dB**, correct |
| **App spectrum, pink noise: band0−band32 tilt** (24 realisations, N = 2048) | **+10.8 dB** |
| **App spectrum, pink noise: band0−band63 tilt** | **+23.7 dB** |
| **Corrected spectrum (fractional power sum ÷ ENBW, N = 8192): band0−band32 tilt** | **−0.55 dB** |
| **Corrected spectrum: band0−band63 tilt** | **−0.49 dB** |
| Corrected spectrum, spread over all 64 bands (pink) | 2.32 dB |
| Corrected spectrum, full-scale 1 kHz sine | **0.00 dBFS** (N = 8192), −0.01 dBFS (N = 2048) |
| Filterbank alternative, pink tilt 0→32 / 0→63 | −4.57 dB / +1.25 dB |
| Band level of pink noise at unit mean-square, per band | ≈ **−16.7 dBFS** |
| App VU bar vs peak tick, one full-scale sine | **3.01 dB** apart (RMS vs peak) |
| App peak decay `0.86` per redraw @60 fps | 0.86⁶⁰ ≈ **1.2·10⁻⁴ / s** (instant vanish) |
| App mixer decay `0.985` per callback | **−6.2 dB/s** @1024 frames, **−24.6 dB/s** @256 frames |
| App RMS window | all frames since last redraw (**16 ms** @60 fps, **67 ms** @15 fps) |
| Synthetic fallback feeding the analyser | RMS −12…−18 dBFS → bar **0.70–0.81** while paused |
| `true_peak_update` decay ×0.999/sample | τ ≈ **21 ms**, −418 dB/s → not a peak hold |
| App K-weighting (its own biquads, `dsp.rs:305-309`) | −8.49 dB @31.5 Hz, +3.34 dB @4 kHz |
| IEC 61672 A-weighting (computed) | −52.4 @20, −41.5 @31.5, −21.1 @100, −2.0 @1 k, −3.2 @8 k, −11.4 @20 k |
| 0 VU mark position on a −60…0 dBFS scale | u = (−18+60)/60 = **0.700** |

---

## 8. Gaps and uncertainties

1. **The 20-stem mixer has no stereo meters today** — `mixer.rs:618` keeps only `max(|L|,|R|)`. Your description says "stereo VU meters on a 20-stem mixer"; in the code the *stereo* L/R pair is the **master** meter in the Monitoring window (`main.rs:5195-5196`, fed from the post-fader tap at `audio.rs:753-761`) and the mixer strips have **one mono bar each** (`main.rs:5461-5480`). If you want per-stem stereo you must extend `mixer.rs` to keep L and R separately.
2. **The 300 ms RMS integration I recommend needs a real time base** — `dsp.rs` currently has no wall-clock; you will need either a sample-count-based window (fine: `0.3 * rate` samples) or `Instant::now()` deltas. All the ballistics above are stated per-second so either works.
3. **`DspAnalyzer::new(48_000)` (`main.rs:2600`) vs `config.sample_rate()` (`audio.rs:610`)** — I could not determine which sample rates the app actually opens devices at. If 44.1 kHz is possible, this is a live bug affecting the FFT, the K-weighting and the LUFS block length.
4. **No true-peak implementation exists**; I did not design a BS.1770-4 Annex 2 oversampling filter here because the exact 4× polyphase specification is a substantial piece of work. If the "dBTP" readout matters to you, that is a separate task.
5. **I did not verify** the IEC 60268-18 *digital* PPM's exact alignment of 0 dBFS onto the division scale (the analogue BBC Type II places "6" at the intended maximum). The ballistics and the 4 dB/division structure are solid; the alignment is implementation-defined and I did not want to assert a number I could not confirm. The 0 VU ≡ −18 dBFS anchor (EBU R68) is the well-established one.
6. **`main.rs:5097-5103` scale labels** — I confirmed the −12/−24/−36/−48 gridlines for the VU and the spectrum (`main.rs:4907`); if the VU scale label list lives at `main.rs:5196-5206` in your current tree, add the −18 mark there and to the gridline set.
7. **The 170 ms display latency** of the 8192-point FFT is a judgement call; if it feels sluggish, 4096 (85 ms) still gives a −0.5 dB pink tilt at the mid bands but keeps bands below ~60 Hz unreliable.