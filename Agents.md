# soundApp_R & NUI Rust — Agent Briefing

> **START HERE:** Read **[`docs/handover_2026-10-24.md`](docs/handover_2026-10-24.md)** for the authoritative uncommitted working state and unverified items. Then read **[`docs/development_plan.md`](docs/development_plan.md)** for benchmarks, solved rendering/typography/FFmpeg traps, and the longer roadmap.
> **RESOLVED 2026-10-25 (ALPHATAP field box):** frameless-window DWM composition offset — content displaced down/right, metrics healthy. Root cause: winit's undecorated windows carry `WS_CAPTION|WS_THICKFRAME|…`, and this driver stack places the window surface at that style's frame origin while the client claims the whole window. Fixed by stripping the bits + suppressing non-client painting in `crates/nui/src/render.rs`. Measurements, verified matrix and the one open item (native drag/resize) in **[`docs/handover_alphatap-render-offset-2026-10-25.md`](docs/handover_alphatap-render-offset-2026-10-25.md)**.

**Last handover:** 2026-10-24 · **Remote:** `https://github.com/herrbasan/soundApp_R` (branch `master`; local HEAD `f72eca9` not yet pushed). NUI and SoundApp recovery checkpoints are committed. Native Parameters sizing is now content-driven and verified; remaining live-playback verification is listed in the handover.

## Overview

`soundApp_R` is the native Rust rewrite of SoundApp (reference Electron codebase at `D:\Work\_GIT\SoundApp`), paired with **`nui`** (`crates/nui`), a standalone, high-performance native Rust port of the `nui_wc2` (v3.0.0 `"slate"`, reference at `D:\Work\_GIT\nui_wc2` / `https://github.com/herrbasan/nui_wc2`) design system and component library.

The workspace has **three crates**: the reusable `nui` library, `nui-demo` (a standalone component playground), and `soundapp` (the player application).

## Reference Sources & Development Orchestration

- **Original Electron SoundApp:** `D:\Work\_GIT\SoundApp`. Inspect this codebase to verify reference behavior before porting features or changing parity-sensitive logic. Full Help content is in `D:\Work\_GIT\SoundApp\html\help.html`.
- **Original UI screenshots:** `docs/reference_ui/`. Compare Rust UI work against these captures; the set is still growing and is not yet a complete visual specification.
- The lead agent orchestrates development: break work into bounded tasks, integrate changes, verify results directly, and check in frequently.
- Use **`deepseek-flash-chat` for substantial grunt work** such as bulk inspection, repetitive transformations, test/harness execution, and screenshot comparison. Keep architecture, ambiguous decisions, integration, and final verification with the lead agent.
- `deepseek-flash-chat` is **multimodal**. For visual debugging, give it the relevant Rust capture and original reference image and explicitly ask it to inspect both images.

## Key Documents in `docs/`

| Document | Purpose |
|---|---|
| **[`docs/handover_2026-10-24.md`](docs/handover_2026-10-24.md)** | **Current working state** — implemented engines, accepted behavior, verification checkpoints, and final unverified latency change |
| **[`docs/development_plan.md`](docs/development_plan.md)** | Master roadmap & history — benchmarks, solved traps, Phase 1–3 checklist |
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
  scripts/                # PowerShell 5.1 benchmark/release harnesses (ASCII-only)
  installer/              # Inno Setup installer, icon assets mapping, association generator
  .github/workflows/      # Tag-driven Windows release build
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
                          #   waveform, polyline, Win32 clipboard, dropzone; flat_slider keeps the
                          #   touch-friendly hit rect independent from its painted/value-mapping track
        layout.rs         # Declarative layout engine (Length, cols, rows, layout_grid, card, nui_card,
                          #   content-sized nui_card_auto, input_group, form_field)
        window.rs         # Multi-Window Manager (WindowManager, ManagedWindow, shared TextSystem atlas_gen sync,
                          #   live OS-cursor/button probes for OLE file-drag overlays, taskbar group
                          #   minimize/restore, Shell tray icon with Show/Quit, fast OS focus)
    nui-demo/             # Standalone component playground -> nui_demo.exe (no audio/FFmpeg deps)
      src/main.rs         # 6-tab NUI playground (menu bar, collapsible sidebar, modal dialog,
                      # right-click context menu) + secondary UiState Inspector window (W)
                      # + dropzone preview (D) + live window drag/resize
    soundapp/             # SoundApp application crate -> soundapp_r.exe
      assets/icons/       # app.ico (embedded resource ID 1) + per-format association icons
      assets/metronome/   # bundled click WAVs
      build.rs            # Compiles FFmpeg bridge + vendored Rubber Band/libopenmpt; links FluidSynth,
                          #   embeds app.ico, accepts SOUNDAPP_FFMPEG_* dependency overrides
                          #   and stages runtime DLLs
      src/
        main.rs           # Winit ApplicationHandler, GUI-subsystem binary (no console window;
                          #   terminal re-attach for harness output), field perf log, adaptive
                          #   native-only MIDI/tracker Parameters, Monitoring, Mixer, Settings & Help,
                          #   single-instance pipe hand-off, tray icon, three-zone file-drop overlay
                          #   with live OS-cursor zone tracking; main/static windows CPU-rendered,
                          #   Monitoring/Mixer GPU-rendered
        single_instance.rs # Session-local mutex + named-pipe server; later launches forward a file
                          #   path or focus request to the running player, then exit
        perf.rs           # Field-performance log (perf_log.txt next to user.json): session marker,
                          #   first-frame / audio-started / open-to-track-ready ms, 10 s frame-pacing
                          #   windows (frames + >66 ms stalls) — works with no console attached
        audio.rs          # FFmpeg/libopenmpt/FluidSynth source engines + raw source ring + cpal output
                          #   + audible-edge Tape/Rubber Band chain (FFmpeg only) + cosine fades/DSP tap
                          #   + --render offline / --latency-probe harness (scripts/audiotest/)
        mixer.rs          # Independent 20-stem Mixer output graph: per-stem FFmpeg workers + 4 s rings,
                          #   synchronized prebuffer/transport/seek/loop, equal-power pan, mute/solo
                          #   (inaudible stems still consume transport frames), per-stem sync telemetry,
                          #   real VU meters, master gain; main player pauses and Mixer close destroys engine
        ffmpeg_bridge.cpp # C ABI bridge over SoundApp's FFmpegDecoder (decode, seek, metadata + embedded
                          #   cover art decode to 128x128 RGBA, rubberband/atempo filter graph)
        midi.rs           # FluidSynth 2.6.1 SoundFont synth + SMF parser/tempo map/sequencer
        tracker.rs        # libopenmpt 0.8.9 tracked-module source engine
        dsp.rs            # Real-time 2048-pt Hann FFT (64 log bands), stabilized oscilloscope,
                          #   stereo goniometer & BS.1770-4 K-weighted LUFS
        config.rs         # Persistent user.json configuration loader/writer
```

### The three binaries

1. **`crates/nui`** — reusable library: `render.rs` with `kind = 0..5` (0 sharp rect, 1 Swash glyph, 2 Fontdue glyph, 3 SDF shadow, 4 AA capsule line, **5 RGBA image quad**), zero-flash reveal (`DWMWA_CLOAK (13)` + theme-colored `GCLP_HBRBACKGROUND` brush and first fill; windows are created **hidden** and only uncloaked after the first presented frame), Swash subpixel shaping, `layout_rotated_ccw45` for atlas-cached `-45°` text, widgets, layout, window manager.
2. **`crates/nui-demo`** → **`nui_demo.exe`** — standalone 6-tab NUI Component Playground (*Controls & Inputs*, *Layout & Cards*, *Vectors & Canvas*, *Typography & Icons*, *Virtual List 10k*, *Menus & Overlays*), wrapped in a real app shell: undecorated app header (empty middle = live window drag zone), `.nui-menu-bar` (File / View / Components / Diagnostics), an animated collapsible `nui-sidebar` + `nui-link-list[mode="fold"]`, a right-click `nui-context-menu`, and a modal `nui-dialog` that now owns every demo control the header used to carry. Plus a secondary `UiState` Inspector window (`W`) and dropzone preview (`D`); the window is resizable via edge/corner hit-testing. Zero audio/FFmpeg dependencies. **This is the iteration surface for `crates/nui` design work.**
3. **`crates/soundapp`** → **`soundapp_r.exe`** — minimalist, fast-opening, low-footprint FFmpeg audio player: `480×278` with controls, `480×221` without (`C`); proportional UI scaling via `+`/`-`/`0`, `Ctrl+Wheel`, or undecorated edge/corner drag. Windows GUI subsystem: double-clicked launches never open a console; launched from a terminal the binary re-attaches to it so harness output stays visible (redirected/piped stdout is untouched).

## Test Media & Remote

- **Primary test media:** `Y:\Test Files` (SMB share). Default startup file `Y:\Test Files\Murr_01.mp3`; fallback `D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a`.
- **GitHub remote:** `https://github.com/herrbasan/soundApp_R` (public).

## Core Design Principles & Rules

1. **Strict Crate Boundary (`crates/nui` vs `crates/soundapp`):**
   - `crates/nui` has **zero** audio or SoundApp dependencies (`winit`, `glow`, `glutin`, `softbuffer`, `swash`, `fontdue`, `tiny-skia`). Keep `nui` 100% general-purpose so it can be extracted into its own repository (`nui_rust`) at any time.
   - `crates/nui-demo` depends only on `nui` + `winit` — never on `soundapp`.
2. **Dark Mode by Default (Accessibility):**
   - Always default to `Theme::dark(rem)`. Light mode (`Theme::light(rem)`) is supported and toggleable (`X` key), but every preview, screenshot, and default launch must be dark mode.
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
   - The release exe is GUI-subsystem: PowerShell captures via pipes (`$out = & exe` returns immediately for GUI children — use `Start-Process -RedirectStandardOutput` or `scripts/measure.ps1`). Field timings land in `perf_log.txt` next to `user.json` regardless.
6. **Zero Idle CPU & Bounded Repaint Loop:**
   - Event-driven (`ControlFlow::Wait`) when static (`0.0%` CPU).
   - When animating, gate `window.request_redraw()` strictly on `Instant::now() >= self.next_frame` before returning `ControlFlow::WaitUntil(self.next_frame)`. Never call `request_redraw()` unconditionally inside `about_to_wait`.
   - Playing with only the player visible repaints at ~15 Hz (`66 ms`); Monitoring/Mixer visible → 60 Hz.
7. **Startup latency is a feature, and the first frame must not flash:**
   - FFmpeg audio decoders always open with `threads = 1` (development_plan §3 #9).
   - The player is revealed by uncloaking (`DWMWA_CLOAK`) after the first `swap_buffers`. Windows are created hidden by `Renderer::new` and shown (still cloaked) by `request_reveal()` — `.with_visible(false)` must never be used as the reveal mechanism itself, and windows must never be created visible-before-cloak (one white `WM_ERASEBKGND` frame reaches the compositor in that gap).
   - Audio stream opens first; metadata + cover art are probed on a detached thread.
   - First-frame after a fresh build reads high (~1 s, cold driver/disk caches); warm runs are ~280 ms — compare warm runs only.
