# soundApp_R

Native Rust audio player & NUI component library. The workspace holds three crates: a reusable native UI library (`crates/nui`), a standalone component playground (`crates/nui-demo`), and the FFmpeg-based player (`crates/soundapp`). NUI is a native Rust port of the `nui_wc2` v3.0.0 `"slate"` design system with a dual GPU/CPU rendering pipeline and browser-grade typography.

**Repository:** <https://github.com/herrbasan/soundApp_R>

## What's Inside

- **`crates/nui` — Native NUI Component Library (zero audio dependencies):**
  - **Dual-Backend Renderer:**
    - **GPU (`glow` OpenGL 3.3 Core):** Single-draw-call instanced SDF renderer for rounded rects, borders, soft drop shadows, hierarchical clip rects, and glyph atlas rendering.
    - **CPU Fallback (`softbuffer`):** Direct `u32` pixel blitter with static-layer caching and integer LUT gamma blending.
  - **Browser-Grade Typography (`swash` + `skrifa`):** OpenType kerning & ligatures, TrueType hinting, 4-bin horizontal subpixel positioning (`0.0, 0.25, 0.5, 0.75`), and linear-light gamma-corrected dark-mode contrast.
  - **Components (`ui.*`):** `tabs`, `button` (`Primary`, `Outline`, `Ghost`, `Success`, `Danger`), `segmented`, `text_input` (caret, drag-selection, horizontal scroll clip, clear button, Win32 clipboard), `select` (window-bounded, scrollable top-layer overlay popup), `slider` (bipolar center tick mark, wheel fine-adjust, double-click reset), `flat_slider` (hit target independent of the painted track), `checkbox`, `switch`, `radio`, `badge`, `virtual_list` (10k rows), `polyline`, `waveform`, `dropzone_overlay`.
  - **Primitives:** `kind = 0..5` — sharp/rounded rects, SDF soft shadows, Swash/Fontdue glyphs, AA capsule lines (`kind = 4`), and RGBA image quads (`kind = 5`); plus `layout_rotated_ccw45` for atlas-cached `-45°` text.
  - **HiDPI & Vector UI Zoom:** Per-Monitor V2 DPI aware + live vector zoom (`65%`–`200%`), with `DWMWA_CLOAK` zero-flash window reveal.
- **`crates/nui-demo` — NUI Component Playground (`nui_demo.exe`):**
  - Five tabs: *Controls & Inputs*, *Layout & Cards*, *Vectors & Canvas*, *Typography & Icons*, *Virtual List 10k*.
  - Secondary `UiState` Inspector window (`W`) and file-drop preview (`D`). No audio or FFmpeg dependencies — this is where `crates/nui` components are designed and polished.
- **`crates/soundapp` — Minimalist Player (`soundapp_r.exe`):**
  - Fast-opening FFmpeg decode + lock-free `rtrb` ring buffer + `cpal` WASAPI output, cosine fades, 8-tap Lanczos resampling, and Rubber Band pitch/tempo/formant processing.
  - **Independent multi-track Mixer (`M`):** a separate output graph and transport with up to 20 streaming stems, synchronized start/seek/loop, per-stem gain/equal-power pan/mute/solo, real VU meters, master gain, and recursive drop-to-add/replace. `Ctrl+Shift+D` opens live per-stem drift/buffer/underrun diagnostics. Opening Mixer pauses the main player; closing Mixer destroys its engine.
  - **Single instance:** normal launches hand the requested file to the already-running player through a named pipe, bring that player forward and exit. Harness/screenshot runs remain independent.
  - **Settings:** real output-device selection (reopens the output stream for player and Mixer at the device's default rate while preserving position), HQ oversampling, tray persistence with a Show/Quit tray icon, parameter lock and theme/transport options. Higher output rates are deliberately not offered: a 192 kHz shared-mode stream makes Windows resample continuously, even while paused.
  - Native MIDI and tracker parameters, including SoundFont selection, file-tempo display, tracker channel VU, and click/Shift-click channel solo.
  - Implicit sibling-folder playlist on single-file open, recursive folder drop, and a three-zone live-tracking drop overlay (*Add to current*, *Replace playlist*, *Open in Mixer*; `Ctrl` = non-recursive).
  - Embedded cover art decoded to `128×128` RGBA from the same FFmpeg metadata open, with a `-45°` format sash.
  - `480×278` with controls, `480×221` without (`C`), proportional UI scaling.

## Build & Run

```powershell
cargo build --release
.\target\release\soundapp_r.exe          # player (default file: Y:\Test Files\Murr_01.mp3)
.\target\release\nui_demo.exe            # NUI component playground
```

Portable user state lives beside the executable: `user.json`; MIDI SoundFonts (`.sf2`/`.sf3`) are loaded from the sibling `soundfonts\` folder. When that directory is not writable (a normal Program Files install), state moves to `%APPDATA%\SoundAppR\user.json`, SoundFonts are read from both `%APPDATA%\SoundAppR\soundfonts\` and the bundled `soundfonts\` folder, and **Parameters → Open Soundfonts Folder** opens the writable AppData folder.

`user.json` also remembers the window layout (positions, sizes, which utility windows were open), the selected output device, and the **default start folder**. With no file argument, the player opens the first audio file in that folder (and its folder becomes the playlist); the folder defaults to the Windows **Music** folder when unset. Set it in **Settings → Default start folder** (*Use Current* takes the playing file's folder, *Reset* returns to the OS Music folder).

Two diagnostic logs sit beside `user.json`, both truncated per session:

- `perf_log.txt` — startup, open-to-track-ready timing and frame pacing.
- `event_log.txt` — every key press with the decision it produced, window open/close/toggle transitions, and focus changes observed from the OS. Attach it to any report about shortcuts, windows or focus behaving oddly.

**Closed windows are remembered unless you clear them:** utility windows visible at exit reopen on the next launch; the Mixer deliberately does not (its stems come from the current playlist, which starts empty).

### Windows Release

```powershell
.\scripts\make-release.ps1 -Version 0.1.0
# optional: -Publish creates the matching GitHub release with gh
```

The release command builds the player, assembles the portable distribution, compiles the Inno Setup installer, creates the portable ZIP, and writes `dist\SHA256SUMS.txt`. It requires Inno Setup 6 (`ISCC_PATH` may point at the compiler). The installer defaults to a per-machine Program Files installation (elevation requested); `/CURRENTUSER` installs per user under Local AppData instead, with associations in HKCU. Both modes register SoundApp in Explorer's **Open with** list and Windows **Default apps**; neither forcibly replaces existing defaults. Choose SoundApp as the default in Windows Settings to use the bundled per-format icons.

Pushing a `v*` tag runs [`.github/workflows/release.yml`](.github/workflows/release.yml) on Windows and publishes the installer, portable ZIP, and checksums. CI downloads the pinned FFmpeg/SoundFont bundle from the `deps-v1` release via `scripts\prepare-ci-deps.ps1`.

### Player Hotkeys & Flags

- **`Space`** play/pause · **`Left`/`Right`** previous/next track · **`Ctrl+Left`/`Right`** seek −10 s / +10 s · **`Up`/`Down`** volume
- **`L`**: Toggle loop · **shuffle button** (header / transport bar): shuffle the current playlist — *Previous* walks back through what actually played
- **`C`**: Show/hide the transport bar (`480×278` ↔ `480×221`)
- **Rendering:** player and static utility windows default to the CPU Softbuffer backend for instant startup; Monitoring and Mixer use the GPU OpenGL backend. `--backend cpu|gpu` forces every window for benchmarking.
- **`F`**: Toggle Font Engine (`Swash Hinted+Gamma` <-> `Fontdue Legacy`)
- **`X`**: Toggle Dark / Light Theme (dark by default)
- **`P` / `N` / `M` / `S` / `H`**: Parameters / Monitoring / Mixer / Settings / Help windows — global toggles from any SoundApp window, so pressing `S` while Settings is focused closes it
- **`I`**: Show the currently playing file selected in Windows Explorer (works from any window)
- **`Ctrl+Shift+D`** in Mixer: toggle the `SYNC DEBUG` panel (drift, ring buffer, underruns, source time per stem; `COPY` snapshots it to the clipboard)
- **`Ctrl + MouseWheel`** (or **`+`** / **`-`** / **`0`**, or drag a window edge/corner): Zoom UI scale
- **`--screenshot <path.png> --seconds <n>`**: Save the exact GPU/CPU framebuffer to PNG
- **`--open-window <key>`**: Open a secondary window for capture
- **Taskbar:** clicking the app's taskbar button minimizes every visible SoundApp window; clicking again restores the group.
- **Tray:** with *Keep Running in System Tray* enabled, `Esc` / close hide the player; the tray icon's left click or **Show** menu restores it, **Quit** exits.

### Test Media

Primary: `Y:\Test Files` (SMB share; default startup file `Murr_01.mp3`). Fallback reference media: `D:\Work\_GIT\SoundApp\_Material\`.

### Benchmarks

- **`.\scripts\measure.ps1`**: Startup, memory, CPU, and GPU benchmark suite (GPU Engine counters for the measured process)
- **`.\scripts\measure-proc.ps1`**: Per-process measurement pass
- **`.\scripts\make-dist.ps1`**: Assembles a runnable distribution (`dist\soundapp_r\` + zip): exe, runtime DLLs, GM SoundFont, metronome clicks, license notices; launches a standalone smoke test

