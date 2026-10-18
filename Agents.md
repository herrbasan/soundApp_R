# soundApp_R & NUI Rust — Agent Briefing

> **START HERE:** Read **[`docs/development_plan.md`](docs/development_plan.md)** first. It holds the complete project state, measured benchmarks, solved rendering/typography/FFmpeg traps (§3), the Phase 1–3 roadmap (§4), and the **next-session priorities (§6)**.

**Last handover:** 2026-10-17 · **Remote:** `https://github.com/herrbasan/soundApp_R` (branch `master`, HEAD `0fa94b5`)

## Overview

`soundApp_R` is the native Rust rewrite of SoundApp (reference Electron codebase at `D:\Work\_GIT\SoundApp`), paired with **`nui`** (`crates/nui`), a standalone, high-performance native Rust port of the `nui_wc2` (v3.0.0 `"slate"`, reference at `D:\Work\_GIT\nui_wc2` / `https://github.com/herrbasan/nui_wc2`) design system and component library.

The workspace has **three crates**: the reusable `nui` library, `nui-demo` (a standalone component playground), and `soundapp` (the player application).

## Key Documents in `docs/`

| Document | Purpose |
|---|---|
| **[`docs/development_plan.md`](docs/development_plan.md)** | **Master Handover & Roadmap** — current state, benchmarks, solved traps, ordered Phase 1–3 checklist, **§6 next-session priorities** |
| [`docs/FINDINGS.md`](docs/FINDINGS.md) | Initial M1–M4 POC benchmark numbers and 6 core findings |
| [`docs/rust-rewrite-assessment.md`](docs/rust-rewrite-assessment.md) | Full feature inventory of Electron SoundApp and Rust crate mapping |
| [`docs/rust-native-ui-exploration.md`](docs/rust-native-ui-exploration.md) | Bare-metal UI tier analysis and widget inventory |
| [`docs/rust-rendering-options.md`](docs/rust-rendering-options.md) | CPU vs GPU rendering cost model analysis |

## Workspace Architecture (3 crates)

```
soundApp_R/
  Cargo.toml              # Workspace root — members: crates/nui, crates/nui-demo, crates/soundapp
  Agents.md               # This file (LLM architectural briefing)
  README.md               # User-facing documentation
  docs/                   # Feasibility, rendering options, native UI exploration, measured findings
  scripts/                # PowerShell 5.1 benchmark harnesses (ASCII-only)
  crates/
    nui/                  # Reusable UI library crate (zero audio/app dependencies)
      assets/fonts/       # OpenSans-Regular, OpenSans-SemiBold, MaterialIcons-Regular
      src/
        lib.rs            # Public exports
        render.rs         # Unified Scene display-list + Dual Backend (GPU OpenGL 3.3 SDF + CPU Softbuffer,
                          #   kind = 0..5; kind 4 = AA capsule line, kind 5 = RGBA image quad) +
                          #   DWMWA_CLOAK zero-flash window reveal
        text.rs           # Swash OpenType shaping + Skrifa hinting + 4x subpixel + linear-light Gamma LUT
                          #   + layout_rotated_ccw45 (-45 deg atlas-cached ribbon text)
        widgets.rs        # nui_wc2 v3.0.0 "slate" theme tokens, interactive widgets, virtual_list,
                          #   waveform, polyline, Win32 clipboard, dropzone
        layout.rs         # Declarative layout engine (Length, cols, rows, layout_grid, card, nui_card,
                          #   input_group, form_field)
        window.rs         # Multi-Window Manager (WindowManager, ManagedWindow, shared TextSystem atlas_gen sync)
    nui-demo/             # Standalone component playground -> nui_demo.exe (no audio/FFmpeg deps)
      src/main.rs         # 6-tab NUI playground (menu bar, collapsible sidebar, modal dialog,
                      # right-click context menu) + secondary UiState Inspector window (W)
                      # + dropzone preview (D) + live window drag/resize
    soundapp/             # SoundApp application crate -> soundapp_r.exe
      build.rs            # Compiles ffmpeg_bridge.cpp against FFmpeg 7+ headers/libs and stages runtime DLLs
      src/
        main.rs           # Winit ApplicationHandler, minimalist player window, Parameters, Monitoring,
                          #   Mixer, Settings & Help windows
        audio.rs          # FFmpeg decode thread + lock-free rtrb ring buffer + cpal output + cosine fades
                          #   + 8-tap Lanczos resampling + WSOLA pitch/tempo/formant
        ffmpeg_bridge.cpp # C ABI bridge over SoundApp's FFmpegDecoder (decode, seek, metadata + embedded
                          #   cover art decode to 128x128 RGBA, rubberband/atempo filter graph)
        midi.rs           # PARKED on disk (not compiled): pure-Rust SF2 SoundFont synth & SMF sequencer
        tracker.rs        # PARKED on disk (not compiled): ProTracker .mod parser & Paula synthesizer
        dsp.rs            # Real-time 2048-pt Hann FFT (64 log bands), stabilized oscilloscope,
                          #   stereo goniometer & BS.1770-4 K-weighted LUFS
        config.rs         # Persistent user.json configuration loader/writer
```

### The three binaries

1. **`crates/nui`** — reusable library: `render.rs` with `kind = 0..5` (0 sharp rect, 1 Swash glyph, 2 Fontdue glyph, 3 SDF shadow, 4 AA capsule line, **5 RGBA image quad**), `DWMWA_CLOAK (13)` + dark `#131313` `GCLP_HBRBACKGROUND` zero-flash reveal, Swash subpixel shaping, `layout_rotated_ccw45` for atlas-cached `-45°` text, widgets, layout, window manager.
2. **`crates/nui-demo`** → **`nui_demo.exe`** — standalone 6-tab NUI Component Playground (*Controls & Inputs*, *Layout & Cards*, *Vectors & Canvas*, *Typography & Icons*, *Virtual List 10k*, *Menus & Overlays*), wrapped in a real app shell: undecorated app header (empty middle = live window drag zone), `.nui-menu-bar` (File / View / Components / Diagnostics), an animated collapsible `nui-sidebar` + `nui-link-list[mode="fold"]`, a right-click `nui-context-menu`, and a modal `nui-dialog` that now owns every demo control the header used to carry. Plus a secondary `UiState` Inspector window (`W`) and dropzone preview (`D`); the window is resizable via edge/corner hit-testing. Zero audio/FFmpeg dependencies. **This is the iteration surface for `crates/nui` design work.**
3. **`crates/soundapp`** → **`soundapp_r.exe`** — minimalist, fast-opening, low-footprint FFmpeg audio player: `480×278` with controls, `480×221` without (`C`); proportional UI scaling via `+`/`-`/`0`, `Ctrl+Wheel`, or undecorated edge/corner drag.

## Test Media & Remote

- **Primary test media:** `Y:\Test Files` (SMB share). Default startup file `Y:\Test Files\Murr_01.mp3`; fallback `D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a`.
- **GitHub remote:** `https://github.com/herrbasan/soundApp_R` (public).

## Core Design Principles & Rules

1. **Strict Crate Boundary (`crates/nui` vs `crates/soundapp`):**
   - `crates/nui` has **zero** audio or SoundApp dependencies (`winit`, `glow`, `glutin`, `softbuffer`, `swash`, `fontdue`, `tiny-skia`). Keep `nui` 100% general-purpose so it can be extracted into its own repository (`nui_rust`) at any time.
   - `crates/nui-demo` depends only on `nui` + `winit` — never on `soundapp`.
2. **Dark Mode by Default (Accessibility):**
   - Always default to `Theme::dark(rem)`. Light mode (`Theme::light(rem)`) is supported and toggleable (`T` key), but every preview, screenshot, and default launch must be dark mode.
3. **Dual-Backend Rendering (`nui/src/render.rs`):**
   - **GPU Backend (`BackendKind::Gpu`):** Single-draw-call (`glDrawArraysInstanced`) instanced renderer over `kind = 0..5` (rects, borders, soft shadows `kind = 3`, Swash/Fontdue glyphs `kind = 1/2`, AA capsule lines `kind = 4`, RGBA image quads `kind = 5`) with per-instance `clip_rect`.
   - **CPU Pixel Fallback (`BackendKind::Cpu`):** Direct `u32` (`0x00RRGGBB`) blitter over `softbuffer` with static-layer caching (`static_cache: Vec<u32>`) and integer LUT linear-light gamma blending.
4. **Browser-Grade Typography (`nui/src/text.rs`):**
   - Uses `swash` (OpenType `kern`/`liga` shaping + TrueType hinting + 4-bin horizontal subpixel positioning `0.0, 0.25, 0.5, 0.75`) + shaped-run caching + `2048x2048` glyph atlas.
   - `layout_rotated_ccw45` rasterizes a horizontal run, bilinearly rotates it `-45°` into a cached square atlas glyph (same generation counter as the atlas).
   - Never clear the glyph atlas mid-frame; `TextSystem::prepare_frame()` flushes at frame start if capacity is low.
   - Material Icons are centered using `TextSystem::layout_icon_centered` (true rasterized pixel bounding box `w, h`), never estimated font baselines.
5. **Visual Verification via `--screenshot`:**
   - Run `.\target\release\soundapp_r.exe --screenshot .\target\preview.png --seconds 1` to capture the exact GPU framebuffer via `glReadPixels` and inspect with `view_image`. Use `--open-window <key>` to capture a secondary window.
6. **Zero Idle CPU & Bounded Repaint Loop:**
   - Event-driven (`ControlFlow::Wait`) when static (`0.0%` CPU).
   - When animating, gate `window.request_redraw()` strictly on `Instant::now() >= self.next_frame` before returning `ControlFlow::WaitUntil(self.next_frame)`. Never call `request_redraw()` unconditionally inside `about_to_wait`.
   - Playing with only the player visible repaints at ~15 Hz (`66 ms`); Monitoring/Mixer visible → 60 Hz.
7. **Startup latency is a feature, and the first frame must not flash:**
   - FFmpeg audio decoders always open with `threads = 1` (development_plan §3 #9).
   - The player is revealed by uncloaking (`DWMWA_CLOAK`) after the first `swap_buffers` — never `winit`'s `.with_visible(false)` (§3 #8).
   - Audio stream opens first; metadata + cover art are probed on a detached thread.
