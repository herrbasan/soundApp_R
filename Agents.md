# soundApp_R & NUI Rust — Agent Briefing

> **START HERE:** Read **[`docs/development_plan.md`](docs/development_plan.md)** first. It contains the complete project state, measured benchmarks, solved rendering/typography gotchas, and the phased roadmap (**Phase 1.1–1.5 -> Phase 2 -> Phase 3**).

## Overview

`soundApp_R` is the native Rust rewrite of SoundApp (reference Electron codebase at `D:\Work\_GIT\SoundApp`, test media in `D:\Work\_GIT\SoundApp\_Material\`), paired with **`nui`** (`crates/nui`), a standalone, high-performance native Rust port of the `nui_wc2` (v3.0.0 `"slate"`, reference at `D:\Work\_GIT\nui_wc2` / `https://github.com/herrbasan/nui_wc2`) design system and component library.

## Key Documents in `docs/`

| Document | Purpose |
|---|---|
| **[`docs/development_plan.md`](docs/development_plan.md)** | **Master Handover & Roadmap** — current state, benchmarks, solved traps, and ordered Phase 1–3 checklist |
| [`docs/FINDINGS.md`](docs/FINDINGS.md) | Initial M1–M4 POC benchmark numbers and 6 core findings |
| [`docs/rust-rewrite-assessment.md`](docs/rust-rewrite-assessment.md) | Full feature inventory of Electron SoundApp and Rust crate mapping |
| [`docs/rust-native-ui-exploration.md`](docs/rust-native-ui-exploration.md) | Bare-metal UI tier analysis and widget inventory |
| [`docs/rust-rendering-options.md`](docs/rust-rendering-options.md) | CPU vs GPU rendering cost model analysis |

## Workspace Architecture

```
soundApp_R/
  Cargo.toml              # Workspace root
  Agents.md               # This file (LLM architectural briefing)
  README.md               # User-facing documentation
  docs/                   # Feasibility, rendering options, native UI exploration, measured findings
  scripts/                # PowerShell 5.1 benchmark harnesses (ASCII-only)
  crates/
    nui/                  # Reusable UI library crate (zero audio/app dependencies)
      assets/fonts/       # OpenSans-Regular, OpenSans-SemiBold, MaterialIcons-Regular
      src/
        lib.rs            # Public exports
        render.rs         # Unified Scene display-list + Dual Backend (GPU OpenGL 3.3 SDF + CPU Softbuffer)
        text.rs           # Swash OpenType shaping + Skrifa hinting + 4x subpixel + linear-light Gamma LUT
        widgets.rs        # nui_wc2 v3.0.0 "slate" theme tokens & interactive widgets
    soundapp/             # SoundApp application crate (`soundapp_r` binary)
      src/
        main.rs           # Winit ApplicationHandler, NUI component showcase & player shell
        audio.rs          # Lock-free rtrb ring buffer + cpal output + streaming decode
```

## Core Design Principles & Rules

1. **Strict Crate Boundary (`crates/nui` vs `crates/soundapp`):**
   - `crates/nui` has **zero** audio or SoundApp dependencies (`winit`, `glow`, `glutin`, `softbuffer`, `swash`, `fontdue`, `tiny-skia`). Keep `nui` 100% general-purpose so it can be extracted into its own repository (`nui_rust`) at any time.
2. **Dark Mode by Default (Accessibility):**
   - Always default to `Theme::dark(rem)`. Light mode (`Theme::light(rem)`) is supported and toggleable (`T` key), but every preview, screenshot, and default launch must be dark mode.
3. **Dual-Backend Rendering (`src/render.rs`):**
   - **GPU Backend (`BackendKind::Gpu`):** Single-draw-call (`glDrawArraysInstanced`) SDF shader for sharp/rounded rects, borders, soft drop shadows (`kind = 3`), and R8 atlas glyphs (`kind = 1` Swash gamma-boosted, `kind = 2` Fontdue legacy), with per-instance `clip_rect`.
   - **CPU Pixel Fallback (`BackendKind::Cpu`):** Direct `u32` (`0x00RRGGBB`) blitter over `softbuffer` with static-layer caching (`static_cache: Vec<u32>`) and integer LUT linear-light gamma blending.
4. **Browser-Grade Typography (`src/text.rs`):**
   - Uses `swash` (OpenType `kern`/`liga` shaping + TrueType hinting + 4-bin horizontal subpixel positioning `0.0, 0.25, 0.5, 0.75`) + shaped-run caching + `2048x2048` glyph atlas.
   - Never clear the glyph atlas mid-frame; `TextSystem::prepare_frame()` flushes at frame start if capacity is low.
   - Material Icons are centered using `TextSystem::layout_icon_centered` (true rasterized pixel bounding box `w, h`), never estimated font baselines.
5. **Visual Verification via `--screenshot`:**
   - Run `.\target\release\soundapp_r.exe --screenshot .\target\preview.png --seconds 1` to capture the exact GPU framebuffer via `glReadPixels` and inspect with `view_image`.
6. **Zero Idle CPU & Bounded Repaint Loop:**
   - Event-driven (`ControlFlow::Wait`) when static (`0.0%` CPU).
   - When animating, gate `window.request_redraw()` strictly on `Instant::now() >= self.next_frame` before returning `ControlFlow::WaitUntil(self.next_frame)`. Never call `request_redraw()` unconditionally inside `about_to_wait`.
