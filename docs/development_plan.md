# soundApp_R & NUI Rust — Master Development Plan & Handover

**Created:** 2026-10-14  
**Workspace:** `D:\Work\_GIT\soundApp_R`  
**Reference Electron App:** `D:\Work\_GIT\SoundApp` (untouched reference for features, assets, binaries, and test media in `D:\Work\_GIT\SoundApp\_Material\`)  
**Reference Web UI Library:** `D:\Work\_GIT\nui_wc2` (`https://github.com/herrbasan/nui_wc2`, v3.0.0 `"slate"` theme)

---

## 1. Mission & Strategy

We are building **two things simultaneously** inside a single Cargo workspace (`crates/nui` + `crates/soundapp`):

1. **`crates/nui` (NUI Rust):** A standalone, reusable, zero-bloat native Rust UI library modeled directly on David's `nui_wc2` v3.0.0 `"slate"` design system (`NUI/css/nui-theme.css`).
   - **Strict compiler boundary:** `crates/nui` has **zero** audio or SoundApp dependencies. It depends only on windowing (`winit`), GPU/CPU surface presentation (`glow`, `glutin`, `softbuffer`), and typography (`swash`, `fontdue`, `tiny-skia`).
   - Once proven across SoundApp's windows, `crates/nui` can be moved out into its own repository (`herrbasan/nui_rust`) with zero refactoring.
2. **`crates/soundapp` (`soundapp_r.exe`):** The native Rust port of SoundApp.
   - Replaces Electron's 3-process architecture (Main + Hidden Chromium Engine Window + Renderer Windows), `AudioWorklet` + `SharedArrayBuffer` + `Atomics` plumbing, and the idle-disposal state machine with a single native process: **1 UI thread + 1 `cpal` audio callback thread + background decoder threads**.

---

## 2. What Is Already Built & Verified (Baseline State)

### 2.1 Implemented Code (`crates/nui` & `crates/soundapp`)

| File | What Is Implemented |
|---|---|
| `crates/nui/src/render.rs` | **Unified `Scene` Display-List + Dual Backend (`Renderer`):**<br>• **GPU Backend (`glow` OpenGL 3.3 Core):** Renders the entire frame (sharp rects, SDF rounded rects, borders, soft SDF drop shadows `kind=3`, and R8 atlas glyphs `kind=1,2`) in **1 single `glDrawArraysInstanced` call**. Enforces per-instance `clip_rect` `[cx0, cy0, cx1, cy1]` in the fragment shader.<br>• **CPU Pixel Fallback (`softbuffer`):** Direct `u32` (`0x00RRGGBB`) blitter with `static_cache: Vec<u32>` layer caching, corner-only SDF evaluation, and integer LUT linear-light gamma blending.<br>• **Top-Layer Overlay Queue (`begin_overlay` / `end_overlay` / `finish_frame`):** Dropdown popups (`nui-select`) and tooltips render after all base widgets in painter's order.<br>• **Direct Screenshot Readback:** `--screenshot <path.png>` reads the live GPU framebuffer via `glReadPixels` (or CPU buffer) and writes a lossless PNG for visual verification. |
| `crates/nui/src/text.rs` | **Browser-Grade Typography (`TextSystem`):**<br>• **`Swash` Engine (default):** OpenType `kern` + `liga` shaping (`ShapeContext`), TrueType auto-hinting (`ScaleContext::hint(true)` via Google Chrome's `skrifa`), 4-bin horizontal subpixel positioning (`0.0, 0.25, 0.50, 0.75` px), shaped-run caching (`shape_cache`), and linear-light gamma + stem-darkening contrast boost for dark backgrounds.<br>• **`Fontdue` Engine (legacy comparison):** Unhinted, integer-snapped, naive sRGB alpha blend (toggleable live with `F` key).<br>• **Pixel-Bounding-Box Icon Centering (`layout_icon_centered`):** Centers `MaterialIcons-Regular.ttf` glyphs using their exact rasterized bitmap dimensions `(w, h)`.<br>• **Hit-Testing & Measurement:** `measure()`, `byte_x_offset()`, `hit_test_byte()` for accurate `nui-input` caret placement and drag selection. |
| `crates/nui/src/widgets.rs` | **`nui_wc2` v3.0.0 `"slate"` Theme & Interactive Components (`Theme`, `UiState`, `Ui`):**<br>• `Theme::dark(rem)` (default) & `Theme::light(rem)` with exact `nui-theme.css` color tokens (`base`, `shade1..9`, `border1..4`, `highlight` `rgb(76,132,229)`, `highlight_dim` `rgb(67,112,191)`, semantic palettes).<br>• `ui.tabs()` — horizontal tab bar with `border2` underline and `2px` `highlight` active indicator.<br>• `ui.button()` — `Primary`, `Outline`, `Ghost`, `Success`, `Danger` variants + active click flash.<br>• `ui.segmented()` — `nui-button-container[variant="segmented"]` joined control.<br>• `ui.text_input()` + `ui.input_label()` — single-line editable input with click-to-caret, drag selection, horizontal scroll clip, `Backspace`/`Delete`/Arrows/`Home`/`End`/`Ctrl+A`, and trailing `×` clear button.<br>• `ui.select()` — dropdown control with seamless clip-rounded trigger/popup seam, top-layer overlay popup, hover/selected checkmark rows, and outside-click close.<br>• `ui.slider()` — `0.25rem` pill track, bipolar center tick mark (`min < 0 && max > 0`), `1rem` circular thumb scaling `1.34x` on drag, wheel fine-tuning, and double-click / right-click reset to default.<br>• `ui.checkbox()`, `ui.switch()`, `ui.radio()`, `ui.badge()` (6px semantic left stripe + status dot). |
| `crates/soundapp/src/audio.rs` | **`cpal` + `rtrb` Lock-Free Audio Pipeline:**<br>• 8-second lock-free SPSC ring buffer (`rtrb`), WASAPI output stream (`48 kHz, 2 ch, F32, 480-frame buffer`), streaming packet-by-packet `symphonia` decoder thread, and muted `--tone` test generator. |
| `crates/soundapp/src/main.rs` | **Interactive Showcase & Player Shell (`soundapp_r.exe`):**<br>• Native Windows 11 DWM drop shadow (`with_undecorated_shadow(true)`), rounded window corners (`CornerPreference::Round`), and dark border.<br>• Per-Monitor V2 HiDPI awareness + live vector UI zoom (`Ctrl + Wheel` or `+`/`-`/`0`, `75%`–`200%`).<br>• 3 interactive tabs (`NUI Components`, `Audio Parameters`, `Typography A/B`) + live 18-band spectrum + scrubbable transport bar. |

### 2.2 Measured Benchmarks (24-Logical-Core Machine, `1290×735` Physical Window @ 1.5× DPI)

| Workload | Backend | First Frame | First Audio | Working Set | CPU (1 Core) | CPU (Machine) |
|---|---|---:|---:|---:|---:|---:|
| **Idle** (event-driven, full UI) | **CPU (`softbuffer`)** | **29–35 ms** | — | **28.4 MB** | **0.0 %** | **0.00 %** |
| **Idle** (event-driven, full UI) | **GPU (`glow` GL 3.3)** | 155–175 ms | — | 81.7 MB | **0.0 %** | **0.00 %** |
| **10 fps Progress-Bar Cadence + Audio** | **CPU (`softbuffer`)** | **30–35 ms** | **32–37 ms** | **36–38 MB** | **1.0 – 2.6 %** | **0.04 – 0.11 %** |
| **60 fps Full UI + Spectrum + Audio** | **CPU (`softbuffer` + static cache)** | **29.3 ms** | **31.2 ms** | **35.7 MB** | **4.7 %** | **0.20 %** |
| **60 fps Full UI + Spectrum + Audio** | **GPU (`glow` GL 3.3 SDF)** | 153–160 ms | 155–161 ms | 85–95 MB | **2.6 – 4.2 %** | **0.11 – 0.18 %** |
| **60 fps + 8,000 Extra Primitives** | **GPU (`glow` GL 3.3 SDF)** | 151.6 ms | 153.5 ms | 90.8 MB | **3.6 – 4.2 %** | **0.15 – 0.18 %** |

---

## 3. Hard-Won Lessons & Gotchas (Do Not Re-Introduce)

1. **Never call `window.request_redraw()` unconditionally inside `about_to_wait`:**
   - Doing so wakes the `winit` event loop immediately, defeating `ControlFlow::WaitUntil` and spinning at ~3,300 fps / 96% of a core. Always gate `request_redraw()` on `Instant::now() >= self.next_frame`.
2. **Premultiply `fill` and `border` colors BEFORE `mix()` in SDF border shaders/blitters:**
   - When `v_fill` is transparent `(0,0,0,0)` (e.g., `ButtonVariant::Outline` or `nui-radio` interior) and `v_border` is opaque, `mix(v_fill, v_border, border_mix)` in un-premultiplied space interpolates RGB toward black and then multiplies by `col.a` a second time — creating a 50% dark halo on borders. Always compute `fill_pm = vec4(fill.rgb * fill.a, fill.a)` and `border_pm = vec4(border.rgb * border.a, border.a)` first.
3. **Never wipe the glyph atlas (`TextSystem::alloc_atlas`) mid-frame:**
   - Wiping `self.atlas.fill(0)` when an allocation overflows mid-frame erases glyphs that were already emitted into `scene.instances` earlier in the same frame. Always check capacity at the start of a frame via `TextSystem::prepare_frame()`.
4. **Center Material Icons by their rasterized pixel bounding box, not font metrics:**
   - `MaterialIcons-Regular.ttf` glyphs sit at varying vertical offsets inside the `em` square. `TextSystem::layout_icon_centered` uses `img.placement.width` and `img.placement.height` so icons (`✓`, `×`, `⌄`, `▶`) are centered to the exact pixel.
5. **Snap text `baseline` and `x` to `.round()` physical pixels:**
   - `Swash` hints horizontal stems relative to an integer baseline. Passing fractional `baseline` coordinates blurs hinted horizontal stems across two pixel rows.
6. **Per-Corner Border Radius via Clip (`push_clip`):**
   - Instead of bloating `QuadInstance` with 4 separate corner radii, `border-radius: r r 0 0` (open `nui-select` trigger) is drawn by clipping to `(x, y, w, h)` and drawing `rounded_rect(x, y, w, h + r, r)`. `border-radius: 0 0 r r` (open popup) clips to `(x, y, w, h)` and draws `rounded_rect(x, y - r, w, h + r, r)`.
7. **PowerShell 5.1 Script Rules (`scripts/*.ps1`):**
   - Keep `.ps1` files strictly ASCII (an UTF-8 em-dash without BOM breaks PS 5.1 CP1252 parsing). Always wrap benchmark process launches in `try / finally { $p.Kill() }`.

---

## 4. Phased Development Plan

### Phase 1 — Complete the `crates/nui` Foundation

Goal: Make building complex multi-window UIs in `crates/nui` declarative and complete so zero manual `x, y` math is required in application code.

- [x] **1.1 Declarative Layout Engine (`crates/nui/src/layout.rs`):**
  - Implement a lightweight stack/flex/grid layout cursor (`ui.with_column()`, `ui.cols()`, `ui.rows()`, `ui.layout_grid(cols)`, `ui.take_form_row()`, `ui.input_group()`, `ui.nui_card()`, `ui.spacer()`, `Length::Px` / `Length::Rem` / `Length::Fill(weight)`), matching `nui_wc2` v3.0.0 (`16px` rem base, `2.5rem` form controls, `1rem` horizontal padding, responsive `1/2/3+` column breakpoints).
  - Refactor `crates/soundapp/src/main.rs` to use the layout builder instead of manual coordinate math.
- [x] **1.2 Scrollable & Virtualized List (`nui-list` / `ui.scroll_area()` & `ui.virtual_list()`):**
  - Vertical scroll container with `push_clip`, mouse-wheel scrolling, clamping, and a draggable scrollbar thumb (`--space-scrollbar-width: 0.5rem`).
  - Virtualized row callback `ui.virtual_list(id, rect, row_count, row_height, |ui, idx, row_rect| ...)` so a 10,000-track playlist only emits `QuadInstance`s for the ~15 visible rows.
- [ ] **1.3 Anti-Aliased Vector Line & Waveform Primitive (`kind = 4` in `render.rs`):**
  - Add `kind = 4` (2D capsule line segment `(x0, y0) -> (x1, y1)` with thickness `w`) to `QuadInstance` in `FRAG_SRC` and `rasterize_slice`.
  - Add `ui.waveform()` (min/max peak columns + playhead scrub) and `ui.polyline()` (`nui-graph` sparkline / FFT curve / oscilloscope / goniometer) on top of it.
- [ ] **1.4 Clipboard, Keyboard Focus Ring & File Drop (`nui-dropzone`):**
  - Native Windows clipboard (`Ctrl+C`, `Ctrl+X`, `Ctrl+V`) in `ui.text_input()`.
  - `Tab` / `Shift+Tab` focus traversal + `:focus-visible` `2px` `--color-highlight` outline ring.
  - `WindowEvent::HoveredFile` / `DroppedFile` wired to an `nui-dropzone` dashed overlay (`NUI/css/nui-theme.css` line 2170).
- [ ] **1.5 Multi-Window Manager inside `crates/nui`:**
  - Support multiple native `winit` windows (`Player`, `Parameters`, `Monitoring`, `Mixer`, `Settings`, `Help`) in a single process (`HashMap<WindowId, WindowEntry>`), sharing the `TextSystem` glyph cache.
  - Implement `hide()` / `show()` fast toggling (matching SoundApp's `P`, `N`, `M`, `S`, `H` window behavior).

---

### Phase 2 — SoundApp Core Audio Engine (`crates/soundapp`)

Goal: Full parity with the Electron app's 3 playback engines and audio parameters, testable headless or from the UI.

- [ ] **2.1 Transport, Seeking & Fade State Machine:**
  - Commands: `Load(PathBuf)`, `Play`, `Pause`, `Seek(f64)`, `SetVolume(f32)`, `SetParams(...)`.
  - Built-in click-free cosine/linear fade ramps in the audio callback (`12 ms` fade-out on pause/stop/seek, `15 ms` fade-in on play), matching the SAB player's anti-crackle behavior.
  - Playlist manager: next/prev/shuffle/loop, track-ended auto-advance, and fast background waveform peak extraction.
- [ ] **2.2 Universal FFmpeg Decoding & HQ Resampling:**
  - Wire `ffmpeg-next` (or direct `libavformat`/`libavcodec`/`libswresample` FFI against `D:\Work\_GIT\SoundApp\bin\win_bin`) for universal format support + metadata extraction (`getMetadata`), keeping `symphonia` as an optional pure-Rust fast path.
  - Wire `rubato` for SIMD sample-rate conversion (`HQ Mode` + output device selection via `cpal`).
- [ ] **2.3 Real-Time Pitch / Tempo / Formant (Rubber Band FFI):**
  - Bind `rubberband-c.h` (`D:\Work\_GIT\SoundApp\libs\rubberband`) on the decoder thread.
  - Supports `mode: Tape` (playback rate resample) vs `mode: PitchTime` (`pitch: -12..+12 st`, `tempo: 0.5..2.0x`, `formant: bool`, `locked: bool` across track changes) without the 70 MB WASM heap or 1-second worklet warmup delay.
- [ ] **2.4 MIDI Engine (`rustysynth` + `midly`):**
  - Pure-Rust SF2 SoundFont synthesis using `.sf2` files from `D:\Work\_GIT\SoundApp\bin\soundfonts`.
  - Parameters: `transpose` (`-12..+12`), `bpm` override, `metronome` click track, and live SoundFont switching.
  - A/B ear-test against `js-synthesizer` / FluidSynth; fallback to `libfluidsynth` C FFI if needed.
- [ ] **2.5 Tracker Engine (`libopenmpt` C FFI):**
  - Bind `libopenmpt` C API for `.mod`, `.xm`, `.it`, `.s3m` (70+ formats; test files in `D:\Work\_GIT\SoundApp\_Material\mod\`).
  - Parameters: `pitch`, `tempo`, `stereoSeparation` (`0..200%`), `interpolationFilter`, and per-channel VU extraction.

---

### Phase 3 — SoundApp Windows, DSP Visualizers & OS Integration

- [ ] **3.1 Main Player Window (`player`):**
  - Cover art / file info panel, interactive waveform seekbar, transport row, playlist drawer, and keyboard shortcuts (`Space`, `Left`/`Right`, `Up`/`Down` volume, `M`/`N`/`P`/`S`/`H`, `Ctrl +/-` scale).
- [ ] **3.2 Parameters Window (`P`):**
  - Adaptive tabs (`Audio` / `MIDI` / `Tracker`) automatically switching to match the loaded file type, plus `Lock` toggle across tracks.
- [ ] **3.3 Monitoring Window (`N` — Real-Time DSP):**
  - Lock-free analysis tap from the `cpal` callback feeding an analysis ring buffer.
  - `realfft` 64-band logarithmic FFT spectrum, live oscilloscope waveform, stereo goniometer (Lissajous), phase correlation meter (`-1..+1`), stereo RMS/Peak VU bars, and BS.1770-4 LUFS (`Momentary`, `Short-Term`, `Integrated`, `LRA`, `True Peak` with K-weighting biquad filters).
- [ ] **3.4 Mixer Window (`M`):**
  - Multi-track synchronized preview (up to 20 stems; test media in `D:\Work\_GIT\SoundApp\_Material\mixer_media\`), per-track gain, pan, mute, solo, and VU meter.
- [ ] **3.5 Settings (`S`), Help (`H`) & Platform Shell:**
  - Persistent `user.json` config, system tray (`tray-icon`) with "Keep running in tray", global media hotkeys (`global-hotkey`), and Windows file-type associations (`winreg`).

---

## 5. Milestone Commit Plan (Rollback Checkpoints)

Every milestone commit must **compile cleanly (`cargo build --release`)**, **pass runtime/visual verification (`--screenshot` in dark mode)**, and represent a self-contained unit of meaning we can safely `git reset --hard` to if a subagent or FFI step goes sideways.

| Milestone | Scope | Verification Gate before Commit |
|---|---|---|
| **`m0-baseline`** (`0169810`) | Initial workspace: `crates/nui` (GPU/CPU renderer, `Swash` text, `nui_wc2` v3.0.0 widgets) + `crates/soundapp` shell | Already committed & verified |
| **`m1-layout-and-lists`** (Phase 1.1–1.2) | Declarative layout engine (`layout.rs`: `column`, `row`, `grid`, `form_row`, `card`) + `scroll_list` / `virtual_list` (10k rows); `main.rs` refactored off manual `x, y` math | Release build + dark-mode `--screenshot` visual parity check |
| **`m2-vectors-and-input`** (Phase 1.3–1.4) | `kind = 4` AA capsule line primitive (GPU shader + CPU blitter), `ui.waveform()`, `ui.polyline()`, Windows clipboard (`Ctrl+C/X/V`), `Tab` focus ring, `nui-dropzone` | Release build + `--screenshot` of waveform/sparkline + input/dropzone test |
| **`m3-multi-window`** (Phase 1.5) | Multi-window manager (`HashMap<WindowId, WindowEntry>`) sharing `TextSystem` + fast `hide()`/`show()` toggling (`P`, `N`, `M`, `S`, `H`) | Release build + multi-window toggle verification *(Completes Phase 1)* |
| **`m4-transport-and-ffmpeg`** (Phase 2.1–2.2) | Transport/seek/cosine-fade state machine, playlist manager, background peak extraction, FFmpeg decoding/metadata + `rubato` HQ resampling | Headless + UI playback test across audio formats & sample rates |
| **`m5-rubberband-midi-tracker`** (Phase 2.3–2.5) | Rubber Band pitch/tempo/formant FFI, MIDI SF2 engine (`rustysynth` + `midly`), and Tracker engine (`libopenmpt` FFI) | Playback verification on `.mp3`/`.flac`, `.mid`, and `.mod`/`.xm`/`.it`/`.s3m` *(Completes Phase 2)* |
| **`m6-player-and-params-ui`** (Phase 3.1–3.2) | Production Main Player window (cover art, waveform seekbar, playlist drawer, shortcuts) + adaptive Parameters window (`P`) | Release build + dark-mode `--screenshot` of Player & Parameters windows |
| **`m7-dsp-mixer-and-shell`** (Phase 3.3–3.5) | Monitoring window (`N`: 64-band FFT, oscilloscope, goniometer, LUFS), 20-stem Mixer (`M`), Settings (`S`), Help (`H`), tray, global hotkeys, file associations | Full end-to-end verification + benchmark pass via `scripts/measure-proc.ps1` |

