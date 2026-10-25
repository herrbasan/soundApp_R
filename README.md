# soundApp_R

Native Rust audio player & NUI component library. The workspace holds three crates: a reusable native UI library (`crates/nui`), a standalone component playground (`crates/nui-demo`), and the FFmpeg-based player (`crates/soundapp`). NUI is a native Rust port of the `nui_wc2` v3.0.0 `"slate"` design system with a dual GPU/CPU rendering pipeline and browser-grade typography.

**Repository:** <https://github.com/herrbasan/soundApp_R>

## What's Inside

- **`crates/nui` — Native NUI Component Library (zero audio dependencies):**
  - **Dual-Backend Renderer:**
    - **GPU (`glow` OpenGL 3.3 Core):** Single-draw-call instanced SDF renderer for rounded rects, borders, soft drop shadows, hierarchical clip rects, and glyph atlas rendering.
    - **CPU Fallback (`softbuffer`):** Direct `u32` pixel blitter with static-layer caching and integer LUT gamma blending.
  - **Browser-Grade Typography (`swash` + `skrifa`):** OpenType kerning & ligatures, TrueType hinting, 4-bin horizontal subpixel positioning (`0.0, 0.25, 0.5, 0.75`), and linear-light gamma-corrected dark-mode contrast.
  - **Components (`ui.*`):** `tabs`, `button` (`Primary`, `Outline`, `Ghost`, `Success`, `Danger`), `segmented`, `text_input` (caret, drag-selection, horizontal scroll clip, clear button, Win32 clipboard), `select` (floating top-layer overlay popup), `slider` (bipolar center tick mark, wheel fine-adjust, double-click reset), `checkbox`, `switch`, `radio`, `badge`, `virtual_list` (10k rows), `polyline`, `waveform`, `dropzone_overlay`.
  - **Primitives:** `kind = 0..5` — sharp/rounded rects, SDF soft shadows, Swash/Fontdue glyphs, AA capsule lines (`kind = 4`), and RGBA image quads (`kind = 5`); plus `layout_rotated_ccw45` for atlas-cached `-45°` text.
  - **HiDPI & Vector UI Zoom:** Per-Monitor V2 DPI aware + live vector zoom (`65%`–`200%`), with `DWMWA_CLOAK` zero-flash window reveal.
- **`crates/nui-demo` — NUI Component Playground (`nui_demo.exe`):**
  - Five tabs: *Controls & Inputs*, *Layout & Cards*, *Vectors & Canvas*, *Typography & Icons*, *Virtual List 10k*.
  - Secondary `UiState` Inspector window (`W`) and file-drop preview (`D`). No audio or FFmpeg dependencies — this is where `crates/nui` components are designed and polished.
- **`crates/soundapp` — Minimalist Player (`soundapp_r.exe`):**
  - Fast-opening FFmpeg decode + lock-free `rtrb` ring buffer + `cpal` WASAPI output, cosine fades, 8-tap Lanczos resampling, and Rubber Band pitch/tempo/formant processing.
  - **Independent multi-track Mixer (`M`):** a separate output graph and transport with up to 20 streaming stems, synchronized start/seek/loop, per-stem gain/equal-power pan/mute/solo, real VU meters, master gain, and recursive drop-to-add/replace. Opening Mixer pauses the main player; closing Mixer destroys its engine.
  - Native MIDI and tracker parameters, including SoundFont selection, file-tempo display, tracker channel VU, and click/Shift-click channel solo.
  - Implicit sibling-folder playlist on single-file open, recursive folder drop, and a three-zone drop overlay (*Add to current*, *Replace playlist*, *Open in Mixer*; `Ctrl` = non-recursive).
  - Embedded cover art decoded to `128×128` RGBA from the same FFmpeg metadata open, with a `-45°` format sash.
  - `480×278` with controls, `480×221` without (`C`), proportional UI scaling.

## Build & Run

```powershell
cargo build --release
.\target\release\soundapp_r.exe          # player (default file: Y:\Test Files\Murr_01.mp3)
.\target\release\nui_demo.exe            # NUI component playground
```

### Player Hotkeys & Flags

- **`Space`** play/pause · **`Left`/`Right`** seek · **`Up`/`Down`** volume
- **`C`**: Show/hide the transport bar (`480×278` ↔ `480×221`)
- **`B`**: Toggle GPU (`OpenGL 3.3 SDF`) <-> CPU (`Softbuffer Pixel Fallback`)
- **`F`**: Toggle Font Engine (`Swash Hinted+Gamma` <-> `Fontdue Legacy`)
- **`T`**: Toggle Dark / Light Theme (dark by default)
- **`P` / `N` / `M` / `S` / `H`**: Parameters / Monitoring / Mixer / Settings / Help windows
- **`Ctrl + MouseWheel`** (or **`+`** / **`-`** / **`0`**, or drag a window edge/corner): Zoom UI scale
- **`--screenshot <path.png> --seconds <n>`**: Save the exact GPU/CPU framebuffer to PNG
- **`--open-window <key>`**: Open a secondary window for capture

### Test Media

Primary: `Y:\Test Files` (SMB share; default startup file `Murr_01.mp3`). Fallback reference media: `D:\Work\_GIT\SoundApp\_Material\`.

### Benchmarks

- **`.\scripts\measure.ps1`**: Startup, memory, CPU, and GPU benchmark suite (GPU Engine counters for the measured process)
- **`.\scripts\measure-proc.ps1`**: Per-process measurement pass
- **`.\scripts\make-dist.ps1`**: Assembles a runnable distribution (`dist\soundapp_r\` + zip): exe, runtime DLLs, GM SoundFont, metronome clicks, license notices; launches a standalone smoke test

