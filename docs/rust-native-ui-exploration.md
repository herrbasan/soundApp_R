# SoundApp — Native UI Exploration (bare-metal Rust)

**Date:** 2026-10-14
**Context:** Companion to `docs/rust-rewrite-assessment.md`. Decision taken: the webview UI is out (NUI is outdated anyway); we want the UI as close to the metal as is sane, and we will prove it with a POC inside this repo.

---

## 1. The finding that changes the calculus

The usual reason *not* to hand-roll a UI toolkit — text input — **does not apply here.**

A full inventory of the current UI (all six windows) found:

| Control type | Count | Notes |
|---|---|---|
| Checkbox | 6 | formant, lock, metronome, dark theme, controls, tray, HQ |
| Dropdown (`<select>`) | 8 | soundfont, output device, buffer size, threads, prebuffer, loudness target |
| Button | 6 | browse, register/unregister files, set default, copy/export JSON |
| Custom drag-slider | many | NUI `drag_slider` — pitch/speed/volume/etc. |
| Canvas (custom-drawn) | 5 | spectrum, waveform, live wave, goniometer, correlation |
| **Text entry** | **1** | `defaultDirInput` — and it is `readonly` (filled by a native folder picker) |

No `<textarea>`, no `contenteditable`, no IME, no bidi. The app is **display-heavy and text-input-free**.

Text input is exactly where from-scratch immediate-mode UI collapses (IME, bidi, editing models, accessibility — the research is unanimous). This app never crosses that line. A hand-rolled toolkit is therefore viable here in a way it is not for a text-heavy app.

**Corollary:** the app's visual identity is already 100% custom (a hand-drawn CSS skin — dark panels, Material Icons glyphs, a rem-based scale). A native toolkit inherits *nothing* it must match, so "it won't look native" is not a cost — it's the status quo.

---

## 2. What the POC must prove

From David (2026-10-14), in order:
1. **Native Windows app** — a real native window, no webview, no Electron.
2. **Quick to load** — measure time-to-first-frame from process start.
3. **Instant buffered playback** — decode + audio out with no perceptible start latency.
4. **Low footprint** — measure RSS and idle CPU.
5. **Then UI** — what is achievable low-footprint and high-performance.

---

## 3. The bare-metal spectrum

| Tier | Stack | Crate(s), version (verified 2026-10-14) | Cost | Fit for SoundApp |
|---|---|---|---|---|
| **1. Absolute metal** | Raw Win32: window class, message loop, GDI/Direct2D | `windows` 0.62 | Enormous platform code; Windows-only | No — loses the Linux build for no gain over tier 2 |
| **2. CPU raster (recommended)** | `winit` → `softbuffer` → `tiny-skia` → own text + widget layer | winit 0.30, softbuffer 0.4, tiny-skia 0.12, fontdue 0.9 (or cosmic-text 0.19 + swash 0.2) | Build the widget/event layer | **Yes** — the "bare to the metal" answer; no GPU dependency, tiny binary |
| **3. GPU raster** | `winit` + `wgpu`, own shaders | wgpu 30 | Significant boilerplate | Only if 60 fps meters strain CPU (they won't at this size) |
| **4. Thin library (fallback)** | `winit` + `egui` (library mode) | egui/eframe 0.36, egui-winit | Least work; you own the styling | Fallback if the custom widget layer becomes a time sink |
| **5. Declarative DSL** | `slint` (ships its own software renderer) | slint 1.18 | DSL + codegen; licensing (GPL/royalty) | No — DSL + against the native-language ethos |
| **6. Retained framework** | `iced` | iced 0.14 (pre-1.0) | Retained model, heavier | No |

**Chosen: tier 2.** It is the genuinely "bare to the metal" path that still runs on Linux, and the app's control-light, text-input-free UI removes tier 2's only real hazard.

---

## 4. The tier-2 stack, concretely

```
process
 ├─ UI thread ── winit event loop (ApplicationHandler)
 │                └─ per window: softbuffer::Surface  ← blit
 │                                    ▲
 │                              tiny_skia::Pixmap  ← draw (rects, paths, glyphs)
 │                                    ▲
 │                              own widget/event layer
 │
 └─ audio: cpal callback thread ── drains rtrb ring buffer ──► device
                                   ▲
        decode thread ── symphonia (POC) / ffmpeg-next (production) ── pushes
```

**Crates (all verified on crates.io, 2026-10-14):**

| Layer | Crate | Version | Note |
|---|---|---|---|
| Window / input / multi-window | `winit` | 0.30.13 | `ApplicationHandler`; `with_decorations(false)` + `drag_window()` for frameless |
| Framebuffer present | `softbuffer` | 0.4.8 | CPU buffer → window; brings no rasterizer of its own |
| 2D raster | `tiny-skia` | 0.12.0 | rects, paths, gradients, blur, clipping into a `Pixmap` |
| Text | `fontdue` | 0.9.4 | ASCII labels + icon codepoints; no shaping needed → tiny |
| Audio out | `cpal` | 0.18.2 | WASAPI on Windows |
| Ring buffer | `rtrb` | 0.4.0 | lock-free SPSC — the Rust replacement for the SAB/Atomics dance |
| Decode (POC) | `symphonia` | 0.5/0.6 | pure Rust; production uses `ffmpeg-next` 9.0 for universal formats |
| Tray | `tray-icon` | 0.26.1 | *not* part of winit |
| Global hotkeys | `global-hotkey` | 0.8.0 | *not* part of winit |
| Native file dialog | `rfd` | 0.17.2 | the "Browse…" button |
| File associations | `winreg` / `windows` | 0.56 / 0.62 | Windows registry |
| Config paths | `directories` | 6.0.0 | roaming/local dirs |
| Accessibility (optional) | `accesskit` (+ `accesskit_winit`) | 0.25 / 0.34 | if screen-reader support is ever required |

**What winit does *not* give you** (own crates required): tray icons, global hotkeys, native menus, and anything drawn inside the window.

---

## 5. What we build ourselves (the real cost)

The rasterization is the easy part. The work is the layer above it:

- **Widgets:** button, checkbox, dropdown (popup list), drag-slider, progress bar, list/scroll, tooltip.
- **Event layer:** hit-testing, hover/active/focus states, mouse capture for drag, keyboard focus order, wheel/scroll.
- **Layout:** the current CSS uses grid + flow; a simple constraint/flow layout covers all six windows.
- **Repaint scheduling:** event-driven redraw (`ControlFlow::Wait`) + a ~60 fps `WaitUntil` tick only while meters/transport animate → **0 % CPU when idle, natively** (this is the app's existing "0 % CPU" goal, achieved without Electron's hidden-process workaround).
- **Scale:** the current rem-based `--space-base` becomes a single `scale: f32` multiplied into every metric. Trivial.
- **Damage tracking** is the known cost of software rendering — but at this app's size (small windows, few animated regions) full-window redraw while animating is comfortably within budget; damage tracking is an optimisation to add later *if measured*.
- **Accessibility:** contrast and scale are ours to guarantee (relevant — cataracts). `accesskit` remains available if needed.

---

## 6. Risks

1. **Widget-layer scope** — the largest single chunk of the rewrite. Mitigated by the small, fixed control set above.
2. **Text stack churn** — egui rewrote its whole text stack in 0.34–0.35; pin versions. `fontdue` (no shaping) is stable and sufficient here.
3. **Multi-window mechanics** — winit 0.30 creates windows *inside* the running loop (`ActiveEventLoop::create_window`), routes by `WindowId`, and keeps a `HashMap<WindowId, State>`. Each window = one `Surface` + one `Pixmap` + the shared font atlas.
4. **Frameless drag** — `window.drag_window()` / `drag_resize_window()`; X11 borderless focus is WM-dependent (Windows is the target, so fine).
5. **Cross-platform** — winit/softbuffer/tiny-skia/fontdue are all cross-platform, so the `linux_bin` story survives.

---

## 7. POC plan (in-repo, `rust-poc/`)

The old app stays untouched in the repo so we always have it to compare against.

| Milestone | Proves | Status |
|---|---|---|
| **M1 — native window + software UI** | "native, quick to load, low footprint"; tier-2 rendering | ✅ built |
| **M2 — audio out** | cpal callback + `rtrb` ring buffer + test tone | ✅ built |
| **M3 — streaming decode + instant playback** | symphonia → ring → cpal; time-to-first-sample | ✅ built |
| **M4 — measure** | startup ms, RSS, idle CPU, playback CPU | ✅ measured (below) |
| **M5 — UI breadth** | the real control set (slider, dropdown, checkbox) + a live meter | next |

Each milestone is independently runnable. `rust-poc/scripts/measure.ps1` reproduces the numbers.

### 7.1 Measured results (2026-10-14)

Release build, `rust-poc/target/release/soundapp-poc.exe` = **2.3 MB**. Window 720×417 physical (scale 1.5).

| Scenario | first frame | first audio | working set | private | CPU (1 core) | fps |
|---|---|---|---|---|---|---|
| idle (no audio) | 26.5 ms | — | 19.9 MB | 8.5 MB | **0.0 %** | — |
| 440 Hz tone + 60 fps UI | 20.4 ms | 22.4 ms | 22.7 MB | 10.0 MB | 7.3 % | 62 |
| m4a streaming + 60 fps UI | 19.5 ms | **21.4 ms** | 23.2 MB | 10.0 MB | 8.8 % | 62 |

Compare: the Electron app is a ~150–300 MB bundle at ~100–250 MB idle, and needs a hidden engine process plus idle-disposal to approach 0 % CPU.

### 7.2 Findings (three real gotchas, found only by measuring)

1. **`request_redraw()` inside `about_to_wait` defeats `ControlFlow::WaitUntil`.** The first version ran at **~3300 fps / 96 % of a core** — the loop never actually slept. Gating the redraw on the frame deadline fixed it (122 % → 5.7 %). This is the kind of bug that "looks fine" until measured.
2. **Spinning while the ring buffer is full cost 30 % of a core** just to feed a sine. `thread::sleep` on full dropped audio-only CPU to **0.5 %**.
3. **Whole-file decode is not "instant".** Decoding the m4a up front took **326 ms and 80 MB**. Streaming packet-by-packet into the ring gives **21 ms and 23 MB** — and the latency is independent of file length.

**Conclusion: the tier-2 hypothesis holds with margin.** A native, webview-free, software-rendered UI on this engine is ~23 MB and **under 10 % of one core while animating at 60 fps**, 0 % idle, ~20 ms to first frame and first sample.

### 7.3 Where the CPU actually goes (isolation, 10 s windows)

At 60 fps, "the UI is expensive" is too coarse. Isolating each layer (via `--silence`, `--static`, `--nopaint`, `--fps`):

| Case | CPU (1 core) |
|---|---|
| window open, no audio, no repaint loop | **0.0 %** |
| cpal stream open, nothing fed (`--silence`) | 0.2–0.3 % |
| + producer thread feeding a sine (`--tone`) | 0.5 % |
| + symphonia decode + scalar resample (m4a) | 0.8 % |
| 60 fps, frame publish only, no drawing (`--nopaint`) | 2.6 % |
| 60 fps, full UI | 5.7–6.2 % |

Device: 48 kHz, 2 ch, F32, **buffer 480 frames** (10 ms period).

So at 60 fps: **audio ≈ 0.5 %, publish ≈ 1.6 %, draw ≈ 3.1 %.**

### 7.4 Reference: ffmpeg CLI (same file)

| Command | CPU (1 core) | WS |
|---|---|---|
| `ffmpeg -re -i file -f null -` (realtime decode) | ~0 % (<0.05) | 15 MB |
| `ffplay -nodisp file` (decode **+ audio out**) | ~0 % (<0.05) | 15 MB |
| POC audio path, same file | 0.8 % | 23 MB |

**Two conclusions, both load-bearing:**

1. **The audio plumbing is the POC's own fault, and is now understood.** Feeding a sine cost **0.9 %** purely from the producer's 5 ms wake/sleep cycle (~200 wakeups/s). A larger ring (8 s) and a 20 ms yield cut it to 0.5 %, and a 1 ms poll had previously cost ~1 %. cpal itself is only 0.2–0.3 % (`--silence`). The remaining ~0.3 % on the file is **symphonia + a scalar linear resampler** — both POC stand-ins; production uses `ffmpeg-next` (the same libav the Electron app already uses) and `rubato` (SIMD), which is what the ffmpeg rows above cost.
2. **At 60 fps the floor is the full-window publish, not the drawing.** An empty 720×417 frame presented 60×/s costs **~1.6 % of a core** with `softbuffer`. Below that needs either a GPU present, partial-region updates, or not repainting the whole window at 60 fps. The **player window has no fast animation** (its progress bar moves slowly) and can be event-driven (~0 %); only the analysis visualizers genuinely need 60 fps.

---

## 8. Verdict

Tier 2 (CPU raster, own widget layer) is the right "bare to the metal" answer **for this app specifically**, because the app is text-input-free and already custom-skinned. The genuine risk is scope of the widget layer, not feasibility or performance. The POC exists to convert that from opinion into measurement.
