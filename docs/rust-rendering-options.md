# SoundApp — Rust Rendering Options (decision doc)

**Date:** 2026-10-14
**Context:** The POC (`rust-poc/`) is built and measured. Measurements showed the render path — not the audio — decides whether the native UI can compete with the Electron app. This doc records what the numbers say, what the research says, and the resulting choice.

---

## 1. What we measured in the POC (the data that forces this decision)

All figures are % of **one** core, window 720 × 417 @ scale 1.5, 10 s windows.

| Case | CPU |
|---|---|
| idle window (no audio, no repaint loop) | 0.0 % |
| audio path (cpal + streaming decode + resample) | 0.5–0.8 % |
| **60 fps, present only, no drawing** (`--nopaint`) | **~1.6 %** |
| 60 fps, full UI | ~5.7 % |

Render cost vs number of UI primitives, 60 fps:

| primitives | tiny-skia per-rect | direct hand-written blit |
|---|---|---|
| 0 | 3.9 % | 5.8 %\* |
| 500 | 7.8 % | 6.7 % |
| 2000 | 13.9 % | 6.6 % |
| 8000 | 26.4 % | 10.3 % |

\* Single-run noise; the two `0` readings bracket the same floor.

**Two hard facts extracted:**

1. **A full-window `softbuffer` present has a fixed per-frame cost of ~1.6 % of a core** at 720 × 417, 60 ×/s, independent of what is drawn. This is a *floor*: no drawing optimisation reaches below it.
2. **Drawing cost depends entirely on the backend.** `tiny_skia::fill_rect` with anti-aliasing cost ~1400 cycles for a 9 × 7 px rectangle (~2.8 % per 1000 rects at 60 fps). Bypassing it with a hand-written row blit made 8000 primitives ~2.6× cheaper. So "UI complexity is free" is **false for a naive CPU raster path** — but it can be made near-flat with the right blitter.

**This is the crux:** Electron/Chromium renders complex UI cheaply because it composites on the **GPU** and re-rasterises only damaged tiles. Our CPU-raster + full-buffer-present path is a different cost model — it pays per pixel and per op, every frame.

---

## 2. What the research says

### 2.1 `softbuffer` has no partial/dirty-region present

The documented cycle is full-buffer only: `resize` → mutate `&mut [u32]` → `present()` [softbuffer docs/README]. No API presents a sub-rectangle, and Windows reportedly goes through `BitBlt` with an extra CPU copy. Partial updates or a cheaper present would mean going **around** `softbuffer` into DirectComposition / DXGI (`Present1` with dirty rects) via the `windows` crate.

→ Our measured ~1.6 %/frame floor at 720 × 417 is **inherent to `softbuffer`**, not a bug in our code.

### 2.2 egui is cheap for complex UI because it offloads to the GPU

egui's frame pipeline is: run UI code → `tessellate(shapes)` → **textured triangle meshes** → backend paints them. Text goes through a **glyph atlas** with incremental uploads (`TexturesDelta`), and primitives sharing a texture + clip rectangle **batch into few draw calls**. It repaints only on interaction/animation, and tessellation can run on `rayon`.

→ That is the same strategy as Chromium: **CPU does layout/tessellation, GPU does pixels.** It is why complex UI stays flat in cost.

### 2.3 The Rust rendering backends (verified on crates.io, 2026-10-14)

| Crate | Version | Model | Notes |
|---|---|---|---|
| `tiny-skia` | 0.12.0 | CPU raster | What the POC uses. Linebender. Strong on x86-64. |
| `vello_cpu` | 0.3.0 | CPU raster, SIMD + multithreaded | Linebender. Benchmarks competitive with/above Skia & Cairo; **more mature than Vello's GPU compute path**; 3–6× from multithreading. |
| `vello` | 0.11.0 | GPU compute 2D | Highest throughput; the compute renderer is flagged experimental upstream. |
| `vello_gpu` | (in repo) | CPU prep + GPU raster | No compute shaders required — broader GPU compatibility. |
| `skia-safe` | 0.153.3 | CPU + GPU (Skia/Graphite) | Most mature ecosystem, heaviest dependency, C++ build. |
| `wgpu` | 30.0.1 | GPU API | The backend a GPU renderer would use. |
| `glow` | 0.18.0 | OpenGL API | Lighter GPU backend; what eframe can use. |
| `epaint` | 0.36.2 | egui's 2D layer | Tessellation + atlas; usable in library mode. |
| `femtovg` | 0.27.0 | GPU 2D (NanoVG-like) | Used by Slint's GL backend. |
| `blend2d` | 0.3.0 (2019) | CPU raster | Abandoned in Rust; ignore. |

---

## 3. The two coherent architectures

| | **(A) CPU raster, full-window present** | **(B) GPU-composited (egui / vello / skia)** |
|---|---|---|
| Built from | `winit` + `softbuffer` + own blitter | `winit`/`wgpu` + `epaint` or `vello_gpu` |
| Per-frame cost | ∝ window area × fps + draw ops | CPU: layout + tessellation only; GPU does pixels |
| Complex UI | costs more (but can be made near-flat with a good blitter) | ≈ flat (batched draw calls) |
| Animated 60 fps, full window | floor ~1.6 % publish @720×417 | present ≈ free (GPU) |
| Static UI | ~0 % (event-driven) | ~0 % (repaint-on-demand) |
| Dependencies | minimal, no GPU | wgpu/glow, shaders, GPU driver |
| Binary / memory | 2.3 MB / 23 MB | larger |
| Matches Electron's cost model? | no | **yes** |

**Key asymmetry:** the fixed ~1.6 % present floor in (A) exists **only** because a CPU buffer is pushed to a window 60 ×/s. Both (A) and (B) are ~0 % when the UI is static — and most of this app's UI *is* static.

---

## 4. Recommendation

1. **Keep the CPU path for the player and all static/event-driven UI.** It costs ~0 % when nothing animates, which is the majority of the app's life. This is where the 23 MB / 2.3 MB / 20 ms wins live.
2. **Use a good blitter, not `tiny_skia::fill_rect`, in the hot path** — hand-written fills for pixel-aligned rects (already demonstrated: 8000 primitives went 26.4 % → 10.3 %). Keep `tiny_skia` (or `vello_cpu`) only for anti-aliased shapes and text.
3. **Do not chase 60 fps full-window CPU redraws.** If a window genuinely needs continuous 60 fps with rich content (the monitoring visualizers), give *that window* a GPU path — `epaint`'s tessellation model over `wgpu`/`glow`, or `vello_gpu`.
4. **Prefer `vello_cpu` over `tiny_skia`** if staying CPU-only for AA shapes: it is SIMD + multithreaded and currently the stronger Rust CPU renderer.
5. **Do not adopt egui's widget set** merely for its renderer; if we want its renderer model we can use `epaint` + our own widget layer, keeping styling control.

**Net:** the "bare-metal CPU-only" premise is right for a *player*; it is the wrong tool for a *continuously animated* window. Choose per window, not per app.

---

## 5. Open questions to settle before implementation

- Does `epaint` (tessellation) work standalone over `wgpu` without adopting egui's `Context`/widget tree? (Research says the contract is `shapes → tessellate → triangles`; need a concrete minimal integration.)
- `vello_gpu` readiness vs the experimental `vello` compute renderer — which is production-safe?
- Is the ~1.6 %/frame `softbuffer` floor reducible just by *not* presenting a full window (e.g. a smaller window, or presenting only when content changes)?
- Do we actually need 60 fps anywhere? (The player does not; a level meter at 30 fps is visually identical.)
- Real-world `BitBlt` vs DMA behaviour of `softbuffer` on this machine's driver.
