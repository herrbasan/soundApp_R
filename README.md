# soundApp_R

Native Rust audio player & analyzer built on **`nui`** (`crates/nui`), a native Rust port of the `nui_wc2` v3.0.0 `"slate"` design system with a dual GPU/CPU rendering pipeline and browser-grade typography.

## Features Built

- **`crates/nui` — Native NUI Component Library:**
  - **Dual-Backend Renderer:**
    - **GPU (`glow` OpenGL 3.3 Core):** Single-draw-call instanced SDF renderer for rounded rects, borders, soft drop shadows, hierarchical clip rects, and glyph atlas rendering.
    - **CPU Fallback (`softbuffer`):** Direct `u32` pixel blitter with static-layer caching and integer LUT gamma blending.
  - **Browser-Grade Typography (`swash` + `skrifa`):** OpenType kerning & ligatures, TrueType hinting, 4-bin horizontal subpixel positioning (`0.0, 0.25, 0.5, 0.75`), and linear-light gamma-corrected dark-mode contrast.
  - **Components:** `nui-tabs`, `nui-button` (`Primary`, `Outline`, `Ghost`, `Success`, `Danger`), `nui-button-container[variant="segmented"]`, `nui-input` (caret, drag-selection, horizontal scroll clip, clear button), `nui-select` (floating top-layer overlay popup), `nui-slider` (bipolar center tick mark, wheel fine-adjust, double-click reset), `nui-checkbox`, `nui-checkbox[variant="switch"]`, `nui-radio`, `nui-badge`.
  - **HiDPI & Vector UI Zoom:** Per-Monitor V2 DPI aware + live vector zoom (`65%`–`200%`).
- **`crates/soundapp` — Audio Engine & Shell:**
  - Lock-free `rtrb` SPSC ring buffer + `cpal` WASAPI output + streaming `symphonia` decode.

## Build & Run

```powershell
cargo build --release
.\target\release\soundapp_r.exe --tone
```

### Hotkeys & Flags

- **`B`**: Toggle GPU (`OpenGL 3.3 SDF`) <-> CPU (`Softbuffer Pixel Fallback`)
- **`F`**: Toggle Font Engine (`Swash Hinted+Gamma` <-> `Fontdue Legacy`)
- **`T`**: Toggle Dark / Light Theme
- **`Ctrl + MouseWheel`** (or **`+`** / **`-`** / **`0`**): Zoom UI scale
- **`--screenshot <path.png>`**: Save exact GPU/CPU framebuffer to PNG
- **`.\scripts\measure.ps1`**: Run startup, memory, and CPU benchmark suite
