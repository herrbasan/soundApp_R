# SoundApp Rust POC — Findings

**Date:** 2026-10-14 · **Author:** session with David · **Status:** M1–M4 complete and measured

A native Rust proof of concept living in `rust-poc/`, built to answer four questions before committing
to a rewrite: **is a native window fast to load, is buffered playback instant, is the footprint low, and
what does a native UI cost?** The Electron app in the repo root is untouched and remains the reference.

Deep dives live in the repo docs (this file is the summary + the data):

- `docs/rust-rewrite-assessment.md` — the full feature/stack inventory and the Rust mapping per subsystem
- `docs/rust-native-ui-exploration.md` — the bare-metal UI exploration and the POC plan
- `docs/rust-rendering-options.md` — the rendering decision doc (backends, cost models)

---

## 1. Verdict

| Question | Answer | Evidence |
|---|---|---|
| Native window? | **Yes** — `winit`, no webview, no Electron | §3.1 |
| Quick to load? | **Yes** — first frame ~20–27 ms from process start | §3.1 |
| Instant playback? | **Yes** — first audio ~22–27 ms; latency independent of file length | §3.1 |
| Low footprint? | **Yes** — 2.3 MB binary, ~20–25 MB working set | §3.1 |
| Low CPU? | **Audio: yes (~0.5 %, below measurement noise). 60 fps UI: no — ~7 % of one core.** | §3.2, §3.3 |

**The one real problem is the 60 fps UI path**, and it is a *design* problem, not an implementation one
(§5). The audio path matches the reference; the render path does not, and cannot under a CPU full-window
present model.

---

## 2. What was built

```
rust-poc/
  Cargo.toml            # winit, softbuffer, tiny-skia, fontdue, cpal, rtrb, symphonia
  src/main.rs           # winit app, software renderer, widget-free layout, --flags
  src/audio.rs          # cpal output + lock-free ring + streaming decode
  src/text.rs           # fontdue bitmap text with a glyph cache
  scripts/measure.ps1       # POC benchmark (auto-quits, sweeps every leftover process)
  scripts/measure-proc.ps1  # benchmark ANY process (used for the ffmpeg reference)
```

Architecture (single process; the Electron three-process design is gone):

```
UI thread   winit (ApplicationHandler) ──► Pixmap ──► softbuffer Surface ──► window
audio       cpal callback ◄── rtrb ring (8 s) ◄── producer thread (tone | symphonia decode)
```

### 2.1 Reproducing every number

```powershell
cd rust-poc
cargo build --release
.\scripts\measure.ps1                      # idle
.\scripts\measure.ps1 -Tone -Static        # audio only
.\scripts\measure.ps1 -Silence -Static     # cpal stream only
.\scripts\measure.ps1 -File <audio> -Fps 60
.\scripts\measure.ps1 -Tone -Fps 60 -Ops 8000   # + synthetic UI complexity
& .\scripts\measure-proc.ps1 -Exe ffplay -Args @('-nodisp','-autoexit','<audio>')
```

Flags: `--tone | --silence | <file>`, `--static` (no repaint loop), `--nopaint` (present only),
`--fps N`, `--ops N`, `--seconds N` (**always auto-quits**). Esc closes the window.

> **Measurement noise:** this machine has 24 logical cores with other services running. Repeat runs
> vary by roughly **±0.5–1.0 % of one core**. Treat anything below ~1 % as "at the noise floor" and
> prefer the *differences* between rows over absolute values.

---

## 3. Measured results

Window 720 × 417 physical (scale 1.5), release build, 8 s sample windows. Device output:
**48 000 Hz, 2 ch, F32, buffer 480 frames** (10 ms period).

### 3.1 Startup and footprint

| Case | first frame | first audio | working set | private | binary |
|---|---|---|---|---|---|
| idle | 26.8 ms | — | 20.0 MB | 8.5 MB | 2.3 MB |
| silence | 21.3 ms | — | 21.9 MB | 11.9 MB | |
| 440 Hz tone | 20.3 ms | **22.1 ms** | 24.8 MB | 11.9 MB | |
| m4a streaming | 24.9 ms | **26.8 ms** | 25.4 MB | 12.2 MB | |

For scale: the Electron app is a ~150–300 MB bundle at ~100–250 MB idle, and needs a hidden engine
process plus an idle-disposal state machine to approach 0 % CPU when idle.

### 3.2 Audio path isolation

| Case | CPU (one core) |
|---|---|
| idle window, no audio | ~0–1 % (noise) |
| cpal stream open, nothing fed (`--silence`) | ~0.2–1.0 % (noise) |
| + producer feeding a sine (`--tone --static`) | **0.4 %** |
| + symphonia decode + scalar resample (m4a) | **0.4 %** |

The audio path sits **at or below the noise floor** — i.e. indistinguishable from zero at this
measurement precision.

### 3.3 Render path

| Case (60 fps) | CPU (one core) |
|---|---|
| present only, no drawing (`--nopaint`) | ~2.6–3.9 % |
| full UI | **~6.4–7.2 %** |
| + 2 000 extra primitives | ~8.0 % |
| + 8 000 extra primitives | ~9.9 % |

### 3.4 Reference — ffmpeg CLI, same file

| Command | CPU (one core) | WS |
|---|---|---|
| `ffmpeg -re -i file -f null -` (realtime decode) | ~0 % | 14.9 MB |
| `ffplay -nodisp file` (decode **+ audio out**) | ~0 % | 14.9 MB |
| POC audio path | ~0.4 % | 25.4 MB |

---

## 4. Findings (each cost real time to discover; none were visible by reading the code)

1. **`request_redraw()` inside `about_to_wait` defeats `ControlFlow::WaitUntil`.** The first version
   ran at **~3 300 fps / 96 % of a core** because the loop never slept. Gating the redraw on the frame
   deadline took the 60 fps case from **122 % → 5.7 %**. An animation loop that "looks fine" can be
   running 55× too fast.

2. **A producer thread that busy-waits to feed a ring buffer costs real CPU.** A 1 ms poll cost ~1 % of
   a core; a 5 ms poll still cost 0.9 % — for generating a *sine*. An 8 s ring with a 20 ms yield fixed
   it (0.9 % → 0.5 %). The wake/sleep cycle, not the work, was the cost.

3. **Whole-file decode is not "instant".** Decoding the m4a up front: **326 ms, 80 MB**. Streaming
   packet-by-packet into the ring: **27 ms, 25 MB** — and the latency no longer scales with file length.

4. **`tiny_skia::fill_rect` costs ~1 400 cycles for a 9×7 rectangle.** Routing every UI rectangle
   through it made cost scale linearly with op count (~2.8 % per 1 000 rects at 60 fps; 8 000 primitives
   = 26.4 %). A hand-written row blit for pixel-aligned rects made the same 8 000 primitives cost
   **9.9 %** — roughly 8× cheaper per op, and near-flat in complexity.

5. **`softbuffer` presents the whole buffer, always.** There is no partial/dirty-region present (and on
   Windows it reportedly goes through `BitBlt` with an extra CPU copy). The fixed ~1.6–3 % per frame at
   720 × 417 is therefore **inherent to the presentation model**, not to our drawing.

6. **Symphonia needs explicit features for MP4/AAC.** The default feature set has no `isomp4` reader —
   the failure is `no suitable format reader found`, which reads like a corrupt file rather than a
   missing feature.

### 4.1 PowerShell / tooling gotchas hit along the way

- A **non-ASCII em-dash in a `.ps1`** breaks parsing under PowerShell 5.1 (the file is read as CP1252).
  Scripts must be ASCII-only.
- **`powershell -File script.ps1 -Args 'a','b','c'`** does not deliver an array — the values arrive as
  one string. Use a direct invocation (`& .\script.ps1 -Args @('a','b','c')`) when passing arrays.
- `Start-Process -ArgumentList @()` with an empty array throws; build the splat conditionally.

---

## 5. The rendering finding, and what it means

**Electron's UI is cheap because it composites on the GPU and re-rasterises only damaged tiles.** Our
CPU raster + full-window present is a different cost model: it pays per pixel and per op, every frame.
The POC's numbers make this concrete:

- **fixed per frame:** ~1.6–3 % to present a 720 × 417 window 60 ×/s (the floor);
- **variable per frame:** drawing, which is near-flat *only* with a hand-written blitter.

egui (and Chromium) avoid the floor entirely by tessellating shapes into textured triangles, batching
into few draw calls over a shared glyph atlas, uploading only texture deltas, and letting the **GPU**
do the pixels. That is why complex UI stays cheap there and does not here.

**So the earlier question — "is the render cost a one-time tax?" — has a precise answer:**

- The **publish** is a fixed tax (∝ window area × fps). ~1.6–3 % at 720 × 417 @ 60 fps.
- The **draw** is *not* fixed under a naive CPU raster path — it scales with op count — but with a
  direct blitter it becomes close to flat (8 000 primitives ≈ +2.7 %).

**Recommendation (details in `docs/rust-rendering-options.md`):**

1. Keep the **CPU path for the player and all static UI** — it is ~0 % when nothing animates, which is
   most of the app's life, and that is where the 2.3 MB / 20 MB / 20 ms wins live.
2. Use a **hand-written blitter** in the hot path, not `tiny_skia::fill_rect`; keep `tiny_skia` (or,
   preferably, `vello_cpu`) only for anti-aliased shapes and text.
3. **Do not chase 60 fps full-window CPU redraws.** If a window genuinely needs continuous 60 fps with
   rich content (the monitoring visualizers), give *that window* a GPU path — `epaint` tessellation over
   `wgpu`/`glow`, or `vello_gpu`.
4. **Choose per window, not per app.**

---

## 6. POC shortcuts vs the production choices they stand in for

| POC | Production | Why it matters |
|---|---|---|
| `symphonia` (pure Rust) | `ffmpeg-next` (libav) | Universal format support; matches the ffmpeg reference cost. **Also the answer to "pure Rust audio?" — no: call libav.** |
| scalar linear resampler | `rubato` | SIMD resampling |
| hand-drawn vector icons | `MaterialIcons-Regular.ttf` codepoints | Same icon set as the current app |
| single crate | split `core` / `app` | UI-agnostic engine, testable headless |
| no seek / playlist / params / monitoring / mixer / MIDI / tracker | — | M5+ |

---

## 7. Open questions before committing

1. **Do we need 60 fps anywhere?** The player does not — its progress bar moves ~1 px/s and can be
   event-driven (~0 %). If only the visualizers animate, and 30 fps is visually fine, the CPU path may
   never need to be fast and the GPU detour may be unnecessary.
2. **Does `epaint`'s tessellation work standalone over `wgpu`** without adopting egui's `Context` and
   widget tree? This is the one integration to prove before choosing a GPU path.
3. Is `vello_gpu` production-ready, or is `vello_cpu` the only mature Vello option today?
4. Can the present floor be avoided by **not presenting a full window** (smaller window, or presenting
   only on change) rather than by going to the GPU?
