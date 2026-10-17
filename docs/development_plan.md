# soundApp_R & NUI Rust — Master Development Plan & Handover

**Created:** 2026-10-14  
**Updated:** 2026-10-17  
**Workspace:** `D:\Work\_GIT\soundApp_R`  
**Remote:** `https://github.com/herrbasan/soundApp_R` (branch `master`, HEAD `0fa94b5`)  
**Primary Test Media:** `Y:\Test Files` (SMB share; default `Y:\Test Files\Murr_01.mp3`, fallback `D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a`)  
**Reference Electron App:** `D:\Work\_GIT\SoundApp` (untouched reference for features, assets, binaries, and additional test media in `D:\Work\_GIT\SoundApp\_Material\`)  
**Reference Web UI Library:** `D:\Work\_GIT\nui_wc2` (`https://github.com/herrbasan/nui_wc2`, v3.0.0 `"slate"` theme)

---

## 1. Mission & Strategy

We are building **three things simultaneously** inside a single Cargo workspace (`crates/nui` + `crates/nui-demo` + `crates/soundapp`):

1. **`crates/nui` (NUI Rust):** A standalone, reusable, zero-bloat native Rust UI library modeled directly on David's `nui_wc2` v3.0.0 `"slate"` design system (`NUI/css/nui-theme.css`).
   - **Strict compiler boundary:** `crates/nui` has **zero** audio or SoundApp dependencies. It depends only on windowing (`winit`), GPU/CPU surface presentation (`glow`, `glutin`, `softbuffer`), and typography (`swash`, `fontdue`, `tiny-skia`).
   - Once proven across SoundApp's windows, `crates/nui` can be moved out into its own repository (`herrbasan/nui_rust`) with zero refactoring.
2. **`crates/nui-demo` (`nui_demo.exe`):** A standalone component playground — five tabs (*Controls & Inputs*, *Layout & Cards*, *Vectors & Canvas*, *Typography & Icons*, *Virtual List 10k*), a secondary `UiState` Inspector window (`W`) and a dropzone preview (`D`) — depending only on `nui` + `winit`. It exists so `crates/nui` component design and visual polish can be iterated **without** audio, FFmpeg or the player shell in the way.
3. **`crates/soundapp` (`soundapp_r.exe`):** The native Rust port of SoundApp.
   - Replaces Electron's 3-process architecture (Main + Hidden Chromium Engine Window + Renderer Windows), `AudioWorklet` + `SharedArrayBuffer` + `Atomics` plumbing, and the idle-disposal state machine with a single native process: **1 UI thread + 1 `cpal` audio callback thread + background decoder threads**.
   - FFmpeg-only decode pipeline (MIDI and tracker engines are parked on disk, see §2.2).

---

## 2. What Is Already Built & Verified (Baseline State)

### 2.1 Implemented Code (`crates/nui` & `crates/soundapp`)

| File | What Is Implemented |
|---|---|
| `crates/nui/src/render.rs` | **Unified `Scene` Display-List + Dual Backend (`Renderer`):**<br>• **GPU Backend (`glow` OpenGL 3.3 Core):** Renders the entire frame (sharp rects, SDF rounded rects, borders, soft SDF drop shadows `kind=3`, and R8 atlas glyphs `kind=1,2`) in **1 single `glDrawArraysInstanced` call**. Enforces per-instance `clip_rect` `[cx0, cy0, cx1, cy1]` in the fragment shader.<br>• **CPU Pixel Fallback (`softbuffer`):** Direct `u32` (`0x00RRGGBB`) blitter with `static_cache: Vec<u32>` layer caching, corner-only SDF evaluation, and integer LUT linear-light gamma blending.<br>• **Top-Layer Overlay Queue (`begin_overlay` / `end_overlay` / `finish_frame`):** Dropdown popups (`nui-select`) and tooltips render after all base widgets in painter's order.<br>• **Direct Screenshot Readback:** `--screenshot <path.png>` reads the live GPU framebuffer via `glReadPixels` (or CPU buffer) and writes a lossless PNG for visual verification. |
| `crates/nui/src/text.rs` | **Browser-Grade Typography (`TextSystem`):**<br>• **`Swash` Engine (default):** OpenType `kern` + `liga` shaping (`ShapeContext`), TrueType auto-hinting (`ScaleContext::hint(true)` via Google Chrome's `skrifa`), 4-bin horizontal subpixel positioning (`0.0, 0.25, 0.50, 0.75` px), shaped-run caching (`shape_cache`), and linear-light gamma + stem-darkening contrast boost for dark backgrounds.<br>• **`Fontdue` Engine (legacy comparison):** Unhinted, integer-snapped, naive sRGB alpha blend (toggleable live with `F` key).<br>• **Pixel-Bounding-Box Icon Centering (`layout_icon_centered`):** Centers `MaterialIcons-Regular.ttf` glyphs using their exact rasterized bitmap dimensions `(w, h)`.<br>• **Hit-Testing & Measurement:** `measure()`, `byte_x_offset()`, `hit_test_byte()` for accurate `nui-input` caret placement and drag selection. |
| `crates/nui/src/widgets.rs` | **`nui_wc2` v3.0.0 `"slate"` Theme & Interactive Components (`Theme`, `UiState`, `Ui`):**<br>• `Theme::dark(rem)` (default) & `Theme::light(rem)` with exact `nui-theme.css` color tokens (`base`, `shade1..9`, `border1..4`, `highlight` `rgb(76,132,229)`, `highlight_dim` `rgb(67,112,191)`, semantic palettes).<br>• `ui.tabs()` — horizontal tab bar with `border2` underline and `2px` `highlight` active indicator.<br>• `ui.button()` — `Primary`, `Outline`, `Ghost`, `Success`, `Danger` variants + active click flash.<br>• `ui.segmented()` — `nui-button-container[variant="segmented"]` joined control.<br>• `ui.text_input()` + `ui.input_label()` — single-line editable input with click-to-caret, drag selection, horizontal scroll clip, `Backspace`/`Delete`/Arrows/`Home`/`End`/`Ctrl+A`, and trailing `×` clear button.<br>• `ui.select()` — dropdown control with seamless clip-rounded trigger/popup seam, top-layer overlay popup, hover/selected checkmark rows, and outside-click close.<br>• `ui.slider()` — `0.25rem` pill track, bipolar center tick mark (`min < 0 && max > 0`), `1rem` circular thumb scaling `1.34x` on drag, wheel fine-tuning, and double-click / right-click reset to default.<br>• `ui.checkbox()`, `ui.switch()`, `ui.radio()`, `ui.badge()` (6px semantic left stripe + status dot). |
| `crates/soundapp/src/audio.rs` | **`cpal` + `rtrb` + FFmpeg Audio Pipeline:**<br>• 8-second lock-free SPSC ring buffer (`rtrb`), WASAPI output stream (`48 kHz, 2 ch, F32, 480-frame buffer`), FFmpeg decode thread (`sa_ffmpeg_open`, always `threads = 1`), cosine fades, 8-tap Lanczos resampling, WSOLA pitch/tempo/formant, EOF drain (`decoder_eof` → `track_ended`), and duplicate-callback filtering. |
| `crates/nui-demo/src/main.rs` | **NUI Component Playground (`nui_demo.exe`):**<br>• Five tabs — *Controls & Inputs*, *Layout & Cards*, *Vectors & Canvas*, *Typography & Icons*, *Virtual List 10k* — driving the same `Scene`/`Renderer`/`Ui` API as the player.<br>• Secondary `UiState` Inspector window (`W`) and file-drop preview (`D`); zero audio/FFmpeg dependencies (`nui` + `winit` only). |
| `crates/soundapp/src/main.rs` | **SoundApp Player Shell (`soundapp_r.exe`, `480x278` logical with controls / `480x221` without, min `480x221`):**<br>• Minimalist flat window (no nested card): `28px` top bar (play/pause, loop, shuffle, `n of m` counter, settings, close), `126px` cover tile with `-45°` extension ribbon, metadata rows (folder / file / format / bitrate / artist / album / title), flat scrub bar + status line, and an optional `55px` transport bar (`C`) with monitoring / parameters / volume / loop / shuffle / prev / play / next.<br>• Native Windows 11 DWM drop shadow (`with_undecorated_shadow(true)`), rounded corners (`CornerPreference::Round`), dark border; Per-Monitor V2 HiDPI + live zoom (`Ctrl + Wheel`, `+`/`-`/`0`).<br>• Implicit folder playlist: opening a file plays its sibling folder (index-matched), a folder recurses; three-zone drop overlay (add / replace / open in mixer, `Ctrl` = flat).<br>• NUI component showcase lives in `crates/nui-demo`. |

### 2.2 Landed 2026-10-17 — Three-Crate Split, Hardened FFmpeg Workflow & Zero-Flash Startup

**Workspace split.** The monolithic `crates/soundapp` UI showcase became **`crates/nui-demo` → `nui_demo.exe`** (5 tabs + `UiState` Inspector + dropzone preview, `nui` + `winit` only). `crates/soundapp` is now the minimalist player only. This is the intended surface for all future `crates/nui` visual work.

**Render / text additions in `crates/nui`:**
- **`kind = 5` RGBA image quad** (`Scene::set_image` + `Scene::image_quad`) with a scene-owned RGBA8 image uploaded per window — used for embedded cover art.
- **`TextSystem::layout_rotated_ccw45`** — rasterizes a horizontal run, bilinearly rotates it `-45°` into a cached square atlas glyph (`rot_cache`, cleared with the atlas); `Scene::text_rotated_ccw45` draws it. Verified visually on the format sash.
- **Zero-flash window reveal** (`DWMWA_CLOAK` + `#131313` `GCLP_HBRBACKGROUND`) — see §3 #8.

**Player workflow & FFmpeg pipeline (8 fixes + SMB speedups):**
1. **`threads = 1` in `sa_ffmpeg_open`** — eliminates 24-thread `avcodec_open2` `DLL_THREAD_ATTACH` contention on the Windows `LdrpLoaderLock` during `wglCreateContext`. Window startup `931 ms` → `295 ms` (§3 #9).
2. **Audio stream opens first** (`9.3–20.0 ms` over SMB) before a background single-open metadata + cover probe (`20.6–48.0 ms`).
3. **Zero-stat directory scan** — `DirEntry::file_type()` (cached `WIN32_FIND_DATAW`, no SMB stats) and immediate audio dispatch before `read_dir`.
4. **Ring-buffer drain before EOF** — `decoder_eof` + `track_ended` atomics owned by the `cpal` callback so the last ~8 s of buffered audio are never truncated; automatic playlist advance; auto-seek-to-0 on Play after EOF.
5. **No redundant FFmpeg filter re-inits** on parameter slider changes (each `sa_ffmpeg_set_*` flushed 1 s of decoded audio).
6. **`Seek` resets `RealTimePitchShifter`** and checks `sa_ffmpeg_seek != 0`.
7. **Zero-overhead DSP tap** — `dsp_enabled` atomic gated on Monitoring-window visibility + RT-safe `try_lock()`.
8. **15 Hz (`66 ms`) base repaint** while playing with only the player open (`60 Hz` only with Monitoring or Mixer visible; `0.0%` CPU `ControlFlow::Wait` when paused).

**Playlist & interaction:**
- `playlist_from_single_file` — implicit sibling-folder playlist, index-matched case-insensitively on `file_name()`.
- `playlist_from_folder(dir, recursive)` — recursive folder collection (`Ctrl` = flat).
- Three-zone drop overlay: *Add to current*, *Replace playlist*, *Open in Mixer*.
- Embedded cover art: `sa_ffmpeg_probe_metadata` decodes `AV_DISPOSITION_ATTACHED_PIC` in-memory from the **same** `AVFormatContext` open (`avcodec` + `swscale`) to `128×128` RGBA. No second open.

**Engines parked.** `midi.rs` and `tracker.rs` remain on disk but are **not compiled** (`symphonia` and `rustysynth` removed from `crates/soundapp/Cargo.toml`) while the FFmpeg core workflow is hardened.

### 2.3 Measured Benchmarks (24-Logical-Core Machine, `1290×735` Physical Window @ 1.5× DPI)

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
8. **Windows dark-mode zero-flash reveal: `DWMWA_CLOAK` + `GCLP_HBRBACKGROUND`, never `.with_visible(false)` with WGL:**
   - Creating the window hidden (`with_visible(false)`) breaks initial Per-Monitor V2 DPI surface sizing on Windows WGL — the GL viewport comes up unscaled (`1120×720` inside a `1680×1050` window) and the first `swap_buffers` stalls for ~750 ms in front of the user.
   - Instead: create the window visible but **cloaked** via `DwmSetWindowAttribute(hwnd, DWMWA_CLOAK (13), TRUE)` immediately after creation, set a dark background brush (`CreateSolidBrush(0x0013_1313)` → `SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND (…-10), brush)`) so any pre-GL paint is already the correct `base` colour, and uncloak inside `reveal_if_pending()` **after the first successful `swap_buffers`**. Call it from both `render_gpu` and `present_nopaint`.
   - Never reveal on a timer or on window creation — the reveal must be frame-driven.
9. **Windows `avcodec_open2` `threads = 0` contention with `wglCreateContext` (`LdrpLoaderLock`):**
   - Default FFmpeg audio decoder init spawns up to 24 worker threads; each trigger of `DLL_THREAD_ATTACH` acquires the Windows loader lock, which OpenGL context creation is also waiting on. The result is a ~600 ms stall before the first frame.
   - **Always pass `threads = 1` for audio decoders** in `sa_ffmpeg_open`. Audio decode does not benefit from frame threading, and the fix cut window startup from `931 ms` to `295 ms`.

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
- [x] **1.3 Anti-Aliased Vector Line & Waveform Primitive (`kind = 4` in `render.rs`):**
  - `Scene::line(x0, y0, x1, y1, thickness, color)` emits a `kind = 4` capsule segment (endpoints ride in `border_color`, radius in `params.x`); `Scene::dashed_rect()` builds dashed outlines from it. GPU: capsule SDF in `FRAG_SRC`; CPU: matching segment SDF in `rasterize_slice` (both blend via `GammaLut::blend_naive_srgb`).
  - `ui.polyline(rect, points, thickness, color)` (`nui-graph` sparkline / FFT curve / oscilloscope / goniometer) and `ui.waveform(id, rect, peaks, playhead)` (centre zero-line, played/unplayed bar tint, hover highlight, crisp `2px` white playhead, click/drag scrub).
  - Verified: `cargo test -p nui` `kind4_line_fills_a_band`; `--screenshot` pixel counts confirm ~33k px (40px test line) / ~14k px (polyline) on **both** GPU and CPU backends.
- [x] **1.4 Clipboard, Keyboard Focus Ring & File Drop (`nui-dropzone`):**
  - Zero-dependency Win32 clipboard (`clipboard_get_text` / `clipboard_set_text` via `user32`/`kernel32` FFI, `CF_UNICODETEXT`); `KeyCmd::{Copy, Cut, Paste, Tab, ShiftTab, Activate}` handled in `ui.text_input()`.
  - `Tab` / `Shift+Tab` traversal over the per-frame `focus_order`; `:focus-visible` `2px` `highlight` ring via `Ui::draw_focus_ring()` on button/checkbox/switch/slider/text-input/select; the ring hides on mouse press.
  - `WindowEvent::HoveredFile` / `HoveredFileCancelled` / `DroppedFile` wired to `ui.dropzone_overlay()` (full-window dim + dashed target + icon/title/path); dropped files load into the search field. `--drop-preview` / `--focus-preview` CLI flags for screenshot verification.
  - Verified: overlay darkens the frame on both backends (meanLuma `127.3` → `102.0` GPU / `104.6` CPU).
- [x] **1.5 Multi-Window Manager inside `crates/nui`:**
  - Support multiple native `winit` windows (`Player`, `Parameters`, `Monitoring`, `Mixer`, `Settings`, `Help`) in a single process (`HashMap<WindowId, WindowEntry>`), sharing the `TextSystem` glyph cache.
  - Implement `hide()` / `show()` fast toggling (matching SoundApp's `P`, `N`, `M`, `S`, `H` window behavior).
  - Landed as `crates/nui/src/window.rs`: `WindowSpec` / `ManagedWindow` / `WindowManager` (windows keyed by `key: &'static str`, `find_by_id_mut` for event routing) with fast `show` / `hide` / `toggle` (hide keeps window + GL context alive) and `Ui::secondary_window_header()` for the shared `3rem` undecorated-window chrome.
  - Multi-window atlas sync: `TextSystem::atlas_gen` (bumped on every atlas clear / glyph alloc) vs each `GpuState::uploaded_atlas_gen` re-uploads the R8 atlas per window; every `render_gpu` and GPU `present_nopaint` rebinds its own GL context first (`make_current`) since all windows render on one UI thread.
  - `soundapp` wiring: `P` / `N` / `M` / `S` / `H` keyboard shortcuts + a Primary/Outline toggle button row in the main window's left card, `Esc` / close-button hides secondary windows, and `--open-window <key>` (`--screenshot` captures that window).

---

### Phase 2 — SoundApp Core Audio Engine (`crates/soundapp`)

Goal: Full parity with the Electron app's 3 playback engines and audio parameters, testable headless or from the UI.

- [x] **2.1 Transport, Seeking & Fade State Machine:**
  - Commands: `Load(PathBuf)`, `Play`, `Pause`, `Seek(f64)`, `SetVolume(f32)`, `SetParams(...)`.
  - Built-in click-free cosine/linear fade ramps in the audio callback (`12 ms` fade-out on pause/stop/seek, `15 ms` fade-in on play), matching the SAB player's anti-crackle behavior.
  - Playlist manager: next/prev/shuffle/loop, track-ended auto-advance. *(Background waveform-peak extraction and the `Audio::peaks` plumbing were **removed** on 2026-10-17 — the shell has no waveform widget; the scrub bar is the seek surface.)*
- [x] **2.2 Universal FFmpeg Decoding & HQ Resampling:**
  - Direct C ABI bridge (`crates/soundapp/src/ffmpeg_bridge.cpp`, compiled by `build.rs` against FFmpeg 7+ headers/libs from `D:\Work\_GIT\SoundApp\bin\win_bin`) for universal format support, seeking and metadata extraction. **FFmpeg is the only decode path** — `symphonia` was dropped on 2026-10-17.
  - 8-tap Lanczos sample-rate conversion in the Rust resampler + output device selection via `cpal`.
- [x] **2.3 Real-Time Pitch / Tempo / Formant:**
  - Rust `RealTimePitchShifter` (WSOLA) for pitch/tempo/formant, with FFmpeg's `rubberband`/`atempo` filter graph in the bridge for the decode-side path.
  - Supports `mode: Tape` (playback rate resample) vs `mode: PitchTime` (`pitch: -12..+12 st`, `tempo: 0.5..2.0x`, `formant: bool`, `locked: bool` across track changes) without the 70 MB WASM heap or 1-second worklet warmup delay.
  - **Never call `sa_ffmpeg_set_*` for slider changes** — each call re-inits the FFmpeg filter graph and flushes ~1 s of decoded audio; parameter changes are applied in the Rust shifter instead.
- [ ] **2.4 MIDI Engine (`rustysynth` + `midly`): PARKED 2026-10-17** — `crates/soundapp/src/midi.rs` is kept on disk but not compiled; `rustysynth`/`symphonia` were removed from `crates/soundapp/Cargo.toml`. Re-enable in Priority 3 (§6).
  - Pure-Rust SF2 SoundFont synthesis using `.sf2` files from `D:\Work\_GIT\SoundApp\bin\soundfonts`.
  - Parameters: `transpose` (`-12..+12`), `bpm` override, `metronome` click track, and live SoundFont switching.
  - A/B ear-test against `js-synthesizer` / FluidSynth; fallback to `libfluidsynth` C FFI if needed.
- [ ] **2.5 Tracker Engine (`libopenmpt` C FFI): PARKED 2026-10-17** — `crates/soundapp/src/tracker.rs` (native ProTracker `.mod` parser + Paula synthesizer) is kept on disk but not compiled. Re-enable in Priority 3 (§6), or port the path to `libopenmpt` FFI for the wider `.xm`/`.it`/`.s3m` format set (test files in `D:\Work\_GIT\SoundApp\_Material\mod\`).

---

### Phase 3 — SoundApp Windows, DSP Visualizers & OS Integration

- [x] **3.1 Main Player Window (`player`):**
  - Minimalist flat `480x278` window (`480x221` with controls hidden via `C` / Settings): top bar, cover tile + format ribbon, folder/file/format/artist/album/title rows, scrub bar and volume status line, optional 55 px transport bar.
  - Implicit folder playlist (a file plays its sibling folder at the matched index; folders recurse) and a three-zone drop overlay: add to current, replace playlist, open in mixer (`Ctrl` = non-recursive).
  - Keyboard shortcuts (`Space`, `Left`/`Right`, `Up`/`Down` volume, `L` loop, `C` controls, `M`/`N`/`P`/`S`/`H`, `Ctrl +/-` scale).
- [x] **3.2 Parameters Window (`P`):**
  - Adaptive tabs (`Audio` / `MIDI` / `Tracker`) automatically switching to match the loaded file type, plus `Lock` toggle across tracks. *(The MIDI/Tracker tabs remain, but their engines are parked — see §2.2.)*
  - Audio: Tape/RubberBand mode, playback speed, pitch, formant preserve. MIDI: SoundFont, transpose, BPM override, metronome. Tracker: stereo separation, pitch, tempo, interpolation filter. Footer: lock + Reset All.
- [x] **3.3 Monitoring Window (`N` — Real-Time DSP):**
  - Lock-free analysis tap from the `cpal` callback feeding an analysis ring buffer.
  - 2048-point radix-2 Hann-windowed FFT mapped onto 64 logarithmic bands, live zero-crossing-stabilized oscilloscope, stereo goniometer (Lissajous), phase correlation meter (`-1..+1`), stereo RMS/Peak VU bars, and BS.1770-4 LUFS (`Momentary`, `Short-Term`, `Integrated`, `True Peak` with K-weighting biquad filters).
  - Landed as `crates/soundapp/src/dsp.rs` (`DspAnalyzer` + `DspSnapshot`); the tap is `audio::DspTap` (`Arc<Mutex<DspTap>>`, 4 s rolling window). Unit test `fft_resolves_a_1khz_sine` verifies peak/Hz, correlation, RMS and LUFS.
- [x] **3.4 Mixer Window (`M`):**
  - Multi-track synchronized preview (up to 20 stems; test media in `D:\Work\_GIT\SoundApp\_Material\mixer_media\`), per-track gain, pan, mute, solo, and VU meter.
  - Landed as a `960x560` channel-strip matrix: M/S cells, touch-friendly vertical fader + green VU, horizontal pan, per-strip remove, `+` add-stem column (9..20 stems), transport row (play/restart/loop), master-volume pill, and a scrubbable seekbar.
- [x] **3.5 Settings (`S`), Help (`H`) & Platform Shell:**
  - Persistent `user.json` config (`crates/soundapp/src/config.rs`: `dark_mode`, `ui_zoom`, `master_vol`, `chk_locked`, `sw_hq_mode`, `sw_keep_tray`, `selected_device`, `selected_rate`, `selected_soundfont`), saved via Settings' "Save Configuration" button and auto-saved on clean exit.
  - Help window with a two-column keyboard-shortcut table. *(System tray, global media hotkeys and file-type associations remain future work — the config surface and the "keep running in tray" preference are in place.)*

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
| **`m6-player-and-params-ui`** (Phase 3.1–3.2) | Production Main Player window (cover art, scrub bar, playlist, shortcuts) + adaptive Parameters window (`P`) | Release build + dark-mode `--screenshot` of Player & Parameters windows |
| **`m7-dsp-mixer-and-shell`** (Phase 3.3–3.5) | Monitoring window (`N`: 64-band FFT, oscilloscope, goniometer, LUFS), 20-stem Mixer (`M`), Settings (`S`), Help (`H`), tray, global hotkeys, file associations | Full end-to-end verification + benchmark pass via `scripts/measure-proc.ps1` |
| **`m8-ffmpeg-hardening`** (`0fa94b5`, 2026-10-17) | `crates/nui-demo` split (5-tab playground + Inspector + dropzone), `kind = 5` image quads + `-45°` atlas text, `DWMWA_CLOAK` zero-flash reveal, `threads = 1` FFmpeg open, zero-stat playlist scan, EOF ring-buffer drain, gate-able DSP tap, 15 Hz idle playback cadence, embedded cover art, three-zone drop | Release build + dark-mode `--screenshot` of player + demo; startup `295 ms`; clean tree on `origin/master` |

---

## 6. Next Session Priorities (2026-10-17 handover)

### Priority 1 — `crates/nui` Visual & Design Polish (in `crates/nui-demo`)

`crates/nui` works well as a technical base: the renderer, typography, layout engine and widget behaviour are solid. **The visual design of the component library is not yet at David's UI bar.** Spacing, proportions, border weights, typography scale, hover/focus/active states and the overall "slate" feel need refinement.

Work in **`nui_demo.exe`** — it is decoupled from audio and FFmpeg precisely so this iteration is fast. Verify every change with `--screenshot` in **dark mode** (light mode must still build and render, but dark is the default and the review surface). Keep the public `Ui`/`Scene` API stable; this is a styling and geometry pass, not an architecture change.

### Priority 2 — Core SoundApp Wild Testing & Polish

Stress-test the minimalist FFmpeg player against real material in `Y:\Test Files`:

- Format sweep (mp3, flac, m4a/aac, ogg/vorbis, opus, wav, wma, ape, mp4/mkv audio) and unusual sample rates/channel counts.
- Seeking behaviour (rapid scrub, seek while paused, seek across a track boundary), pitch/tempo/formant extremes, and parameter changes mid-playback.
- Playlist edge cases: single file in a folder, mixed-extension folders, unreadable/huge folders, recursive drop, dropped non-audio files.
- Secondary windows (`P` Parameters, `N` Monitoring, `M` Mixer, `S` Settings, `H` Help) — open/close cycles, DPI changes, multi-monitor moves, and resize behaviour under UI scaling.
- EOF/last-track behaviour, loop/shuffle combinations, and rapid track switching (the `transport_gen` guard must drop stale metadata/cover probes).

Fix what breaks; log anything structural in §3 before moving on.

### Priority 3 — Re-integrate MIDI & Tracker Engines

Once the FFmpeg core and the NUI visual pass are locked in, unpark `midi.rs` and `tracker.rs` (or port the tracker path to `libopenmpt` FFI, per the original Phase 2.5 plan). Restore their `Cargo.toml` dependencies and re-wire the adaptive Parameters tabs (`Audio` / `MIDI` / `Tracker`).

