# SoundApp — Rust Rewrite Assessment

**Date:** 2026-10-14
**Question:** Can SoundApp be rewritten as a native Rust app, and what would that take?
**Constraint:** Library use permitted at the ffmpeg level; prefer doing as much as possible natively.

---

## 1. What SoundApp is today (feature inventory)

Derived from the code, not the README.

**Playback engines (three, format-dispatched):**

| Engine | Formats | Pipeline |
|---|---|---|
| FFmpeg | MP3, FLAC, WAV, OGG, AAC, M4A, and anything libav\* handles | dual: Normal / Rubberband |
| MIDI | .mid .midi .kar .rmi | SoundFont synthesis |
| Tracker | MOD, XM, IT, S3M, 70+ | libopenmpt |

**Audio parameters:**
- FFmpeg: `mode` (tape/pitchtime), `tapeSpeed` (−12..+12 st), `pitch` (−12..+12 st), `tempo` (0.5..2.0×), `formant` preservation, `locked` (carry params across tracks).
- MIDI: `transpose`, `bpm` override, `metronome`, `soundfont` selection.
- Tracker: `pitch`, `tempo`, `stereoSeparation`, `interpolationFilter`.

**Subsystems:**
- Playlist with next/prev/shuffle/loop, auto-advance, track-ended detection.
- Waveform extraction + seek bar.
- Monitoring window: FFT spectrum (64 log bands), live waveform, goniometer, correlation, BS.1770 LUFS (momentary/short/integrated + LRA + PLR/true-peak) with K-weighting filters, stereo RMS/peak meters.
- Mixer window: multi-track (≈20 tracks), per-track gain/pan/mute, shared transport, VU.
- HQ mode (higher sample rates), output-device selection.
- Child windows: Parameters, Monitoring, Mixer, Settings, Help.
- System tray, "keep running in tray", global keyboard shortcuts.
- File-type association registration (Windows registry), Squirrel installer + auto-update.
- Theme (dark/light), NUI component-based UI.
- Idle disposal: destroy the audio engine after a pause timeout to reach 0% CPU; restore on next interaction.

---

## 2. Current stack, subsystem by subsystem

| Concern | How it is built today | Where |
|---|---|---|
| App shell | Electron 34 (main + renderer processes) | `js/app.js`, `package.json` |
| Windows | `BrowserWindow`, one per panel; custom focus-restoration hacks | `js/managers/window-manager.js` |
| UI | Hand-written HTML/CSS + vanilla JS + NUI web components (submodule) | `html/`, `css/`, `js/*/main.js` |
| Audio graph | **Web Audio API** — `AudioContext`, `AudioWorklet`, `GainNode`, `AnalyserNode` | `js/engines.js`, `js/mixer/*` |
| Streaming | `SharedArrayBuffer` ring buffer + `Atomics`, 100 ms decode chunks | `bin/win_bin/player-sab.js`, `ffmpeg-worklet-sab.js` |
| Decode | Native N-API C++ addon binding `libav*` (`avcodec/avformat/swresample`) | `libs/ffmpeg-napi-interface` |
| Metadata | Same addon (`getMetadata`), no ffprobe subprocess any more | `js/engines.js` |
| Pitch/time | Rubber Band compiled to WASM, run as an `AudioWorklet` | `libs/rubberband-wasm` |
| MIDI | `js-synthesizer` + libfluidsynth compiled to WASM worklet | `libs/midiplayer` |
| Tracker | libopenmpt compiled to WASM worklet | `libs/chiptune` |
| Monitoring DSP | `AnalyserNode` (FFT/time) + hand-rolled LUFS/BS.1770 in JS + a Web Worker | `js/monitoring/*` |
| Waveform | Custom JS sample scanning | `js/waveform_analyzer.js` |
| State | Ground-truth object in main; IPC sync; `State.get/set/subscribe` client | `js/app.js`, `js/state-client.js` |
| IPC | Electron `ipcMain/ipcRenderer` + `MessagePort` for 60 fps VU | everywhere |
| Device enum | Web Audio `enumerateDevices` | `js/settings/*` |
| File assoc | N-API Windows registry addon | `libs/native-registry` |
| Updates | Squirrel | `js/squirrel_startup.js`, `libs/electron_helper` |

Scale: ~12k lines of app JS/CSS/HTML (excluding vendored libs), plus the WASM/C++ dependencies.

---

## 3. The Rust mapping

Versions verified against crates.io on 2026-10-14.

### 3.1 Audio I/O and the graph — the core replacement

| Need (today) | Rust approach | Crate (version / updated) | Notes |
|---|---|---|---|
| `AudioContext` output | OS audio backend | **cpal** 0.18.2 (2026-08) | WASAPI/CoreAudio/ALSA. Mature, 24M downloads. |
| High-level playback | *not advised* | rodio 0.23.0 | rodio's engine is mid-rewrite and mixes/decodes in ways not ideal for a custom graph — skip it for the main player |
| Graph (gain, routing, mixer, fades) | **build it** | — | Gain/pan/fade/routing are trivial DSP; matches the zero-dep ethos. The JS already hand-rolls fades and pan. |
| Sound (input to graph) | custom | `rtrb` 0.4.0 / `ringbuf` 0.5.3 / `triple_buffer` 9.0 | Lock-free SPSC ring buffers — the Rust equivalent of the SAB ring buffer, but typed and safe. |
| RT thread priority | as needed | `audio_thread_priority` 0.38.0 | Raises the callback thread priority per platform. |
| Device enumeration | cpal | cpal | `host.devices()` covers the output-device picker. |

**Key architectural win:** the JS `AudioWorklet` + `SharedArrayBuffer` + `Atomics` dance is *only there to get work off the main thread*. In Rust, cpal hands you a callback running on its own thread; a decoder thread fills a lock-free ring buffer the callback drains. This is the same shape the current code already implements (decode on one thread, ring buffer, consume on the audio thread) — but without the SAB/Atomics/MessagePort scaffolding.

### 3.2 Decoding — keep libav\*, drop the N-API layer

| Option | What | Trade |
|---|---|---|
| **ffmpeg-next** 9.0.0 (2026-08) | Rust bindings to the same `libav*` libraries | **Same decoder the app already ships.** Behavior-identical. Recommended for "universal format" parity. |
| rsmpeg 0.18 (ffmpeg 8.0) | Alternative FFI, heavier | Viable but less common |
| **Symphonia** 0.6.1 (MPL-2.0) | Pure Rust: FLAC/MP3/MP4/Vorbis/WAV + more | Zero C dependency, but **does not cover everything** (tracker formats, exotic codecs). Good as a fast path for common formats; keep ffmpeg as the universal fallback. |

**Recommendation:** ffmpeg-next for the universal path (the constraint explicitly allows ffmpeg), optionally Symphonia as a pure-Rust fast path later. This deletes the N-API addon entirely — the decode logic moves into Rust instead of C++ glued to Node.

### 3.3 Pitch/time (Rubberband)

| Option | Version | Assessment |
|---|---|---|
| `rubberband` crate | 0.1.5, **196 downloads**, 2026-09 | Too new/unproven to depend on. |
| Direct FFI to Rubber Band C API | — | You already ship Rubber Band (`rubberband-c.h`). Linking the C API is the lowest-risk path and keeps the exact sound. **GPL** — already the case today via the WASM build. |
| `signalsmith-stretch` | 0.1.3, 114k downloads | Permissive-licensed alternative; different sound, needs A/B. |

**Recommendation:** FFI to the Rubber Band C API (same library, same quality, drops the ~70 MB WASM heap and the worklet warmup dance; the stretcher runs on its own thread in Rust).

### 3.4 MIDI (SoundFont)

| Option | Version | Assessment |
|---|---|---|
| **rustysynth** | 1.3.7, active (2026-10) | **Pure Rust** SF2 synth. Fits the zero-dep ethos. |
| OxiSynth | pure Rust | Alternative pure-Rust SF2 synth. |
| fluidlite / fluidsynth crate | 0.2.1 / 0.0.1 (2018) | Stale, C dependency. Skip. |
| SMF parsing | `midly` 0.5.3 / `wmidi` 4.0.11 | MIDI file parsing + timing; you own the scheduler. |

**Risk:** rustysynth is a pure-Rust SF2 renderer and may not match libfluidsynth feature-for-feature (chorus/reverb, some SF2 opcodes, polyphony tuning). This is the one place where a straight port could *sound slightly different* — it needs ear-testing against the current engine. Fallback: FFI to libfluidsynth C API if parity fails.

### 3.5 Tracker (libopenmpt)

**No maintained Rust binding exists.** `openmpt` (0.3.1, 2017) and `openmpt-sys` (0.2.0, 2022) are abandoned.

**Recommendation:** bind libopenmpt's **stable C API** (`libopenmpt.h`) yourself with `bindgen` + a build script. This is exactly the "ffmpeg-class" dependency the constraint allows. ~1–2 days of FFI + a safe wrapper. Playback is offline-to-ring-buffer, same as today.

### 3.6 Monitoring / analysis

| Need | Rust | Version |
|---|---|---|
| FFT | `realfft` 3.5 / `rustfft` 6.4 | active |
| LUFS/BS.1770 | `ebur128` 0.1.10 (C lib binding) **or** port the existing JS | the JS LUFS logic is already hand-written and can be ported directly |
| Goniometer / spectrum / waveform | pure DSP + your draw calls | trivial |

The analysis math is the *easiest* part to port — it is already vanilla JS with no browser API beyond the `AnalyserNode` data feed, which cpal replaces.

### 3.7 Resampling / HQ mode

- `rubato` 5.0.1 (pure Rust, active) for sample-rate conversion. Replaces the Web Audio context sample-rate juggling and the "detect max sample rate" probe.

### 3.8 UI shell — **the fork** (see §6)

### 3.9 Platform integration

| Need | Rust |
|---|---|
| Tray | `tray-icon` 0.26.1 (active) or the Tauri plugin |
| Global shortcuts | `global-hotkey` (Tauri ecosystem) |
| File-type association | `winreg` / the `windows` crate 0.62 |
| Installer + auto-update | Tauri updater, or MSI/NSIS via `cargo-wix`/`cargo-bundle`; Squirrel has no Rust equivalent |
| Single instance / window state | `single-instance`, `tauri-plugin-store` |

---

## 4. What collapses (Electron-only scaffolding that disappears)

These exist *because of Electron* and evaporate in Rust:

- **Three-process design** (main / hidden engine window / renderers). In Rust: one process, an audio thread, a UI thread. The whole `createEngineWindow` / `sendToEngine` / `restoreEngineIfNeeded` machinery goes away.
- **Idle disposal to reach 0% CPU** — a workaround for a hidden Chromium renderer costing CPU. A parked audio callback + no active rendering is already ~0%.
- **MessagePort + SharedArrayBuffer + `Atomics`** for 60 fps VU — becomes a plain channel or shared struct.
- **`window-manager.js` focus hacks** (`setAlwaysOnTop` toggle, blur-before-show) — native window focus is the OS's job.
- **IPC state-sync layer** (`state-client.js`, `broadcastState`, channel bookkeeping) — state lives in one address space.
- **The WASM/worklet warmup gymnastics** (Rubberband 1 s startup delay + `_countingEnabled`, MIDI mute-during-load blips, tracker auto-play quirks). Native libraries do not have an audio-callback-vs-main-thread disconnect to work around.

That is a large fraction of the codebase's *complexity* — though not its line count.

---

## 5. The hard problems, ranked

1. **The audio graph itself.** Web Audio gives gain/pan/routing/fades/mixer/analysis for free. In Rust you build the graph, the fade ramps, the mixer summing, and the ring-buffer handoff. Well-trodden, but it is real work and the place where a subtle bug = crackle.
2. **MIDI sound parity.** rustysynth vs libfluidsynth is the one likely audible difference. Needs A/B ear-testing.
3. **Tracker FFI.** Unmaintained bindings mean you own the libopenmpt wrapper.
4. **The UI** — the size of this problem depends entirely on the §6 decision.
5. **Format breadth.** ffmpeg-next solves it, but the build now links libav\* (already true) — packaging those DLLs is the same problem as today.
6. **Rubberband licensing.** GPL. Already true today, but a native app facing distribution should re-confirm the licensing story (commercial licence exists).
7. **Auto-update / installer.** Squirrel is Electron-specific; you re-platform this.

---

## 6. The UI fork — the single biggest decision

The backend rewrite is bounded and well-understood. The UI is where the scope explodes or not.

### Option A — Tauri 2 (Rust backend + system webview)
Reuse the existing HTML/CSS/JS + NUI UI almost verbatim; replace Electron with Tauri; replace the audio layer with cpal-based Rust.
- **Crates:** `wry` 0.57, `tao` 0.37 (both very active).
- **Pros:** by far the least work; keeps the UI you already like; multi-window and tray are first-class; bundle ~5–15 MB vs Electron's ~150 MB; memory ~50–100 MB vs ~150–250 MB.
- **Cons:** still a webview (WebView2 on Windows) — not "native UI." Per-window WebView memory matters if you keep 5 windows.
- **Honest framing:** this is "native backend, webview frontend." It kills Electron, which is the biggest single win, without rewriting the UI.

### Option B — Pure Rust UI (`egui` or `iced`)
- `egui` 0.36 (immediate-mode, easy, great for meters/visualizers, weak for big apps).
- `iced` 0.14 (Elm-style, clean, still pre-1.0, smaller widget set).
- **Pros:** truly native, single language, no webview, smallest footprint, best startup.
- **Cons:** the entire NUI-based UI, 5 windows, canvas visualizers, settings, help must be rebuilt from scratch. egui suits the *monitoring* window (immediate-mode drawing) but a settings/help/params UI in egui is a lot of manual layout. This roughly doubles the project.

### Option C — Hybrid
Rust core as a library (`SoundApp-core`) + a thin UI in either. Useful as a stepping stone: build and test the engine headless (CLI/unit tests) before committing to a UI.

**Recommendation:** Option A for the first pass (it captures ~80% of the "native" benefit — no Electron, no Web Audio, single process — at a fraction of the cost), with the engine written as a UI-agnostic Rust core so Option B remains open. If the goal is *specifically* "no webview anywhere," go C then B.

---

## 7. Recommended target architecture

```
soundapp/
  core/        # UI-agnostic: decode, graph, engines, mixer, DSP, state
    audio.rs       # cpal device + RT callback + ring-buffer consumer
    decode/        # ffmpeg-next (+ optional symphonia fast path)
    engines/       # ffmpeg, midi (rustysynth), tracker (libopenmpt FFI)
    dsp/           # gain/pan/fade/mixer/loudness/fft/resample
    state.rs       # ground truth, single owner
  shell/       # window/tray/shortcuts/registry/updater  (Tauri or native)
  ui/          # NUI web UI (Option A) or egui/iced screens (Option B)
```

- One process. Audio thread (cpal callback) drains lock-free ring buffers; decoder threads fill them; the UI sends commands over a channel.
- `SoundApp-core` has no UI dependency → testable headless → the "runtime is the only oracle" rule is satisfiable without a window.

---

## 8. Phased plan (each phase independently runnable)

1. **Engine spike (no UI).** cpal output + ffmpeg-next decode + ring buffer + gain/fade; play an MP3, seek, verify no underruns. *Proves the riskiest part.*
2. **Core file player.** Params (tapeSpeed/pitch/tempo), Rubber Band FFI, HQ resample via rubato. CLI or minimal window.
3. **Second and third engines.** rustysynth MIDI + libopenmpt FFI tracker; A/B parity against current app.
4. **DSP suite.** Monitoring (FFT/LUFS/gonio), waveform, mixer.
5. **Shell.** Windows/tray/shortcuts/registry/installer/updater (Tauri or native).
6. **UI.** Port (Option A) or rebuild (Option B).

---

## 9. Licensing notes (verify before shipping)

- **FFmpeg (libav\*)** — LGPL, or GPL if built with GPL components. Dynamic linking keeps you in LGPL territory. Already your situation.
- **Rubber Band** — GPL, or a commercial licence for closed distribution; not permitted on the Apple App Store without the commercial licence. Already your situation via the WASM build.
- **libopenmpt** — BSD-3 (permissive) — good.
- **rustysynth / symphonia / rubato / cpal / ebur128** — MIT/Apache-2.0/MPL-2.0 (Symphonia MPL-2.0; `ebur128` wraps libebur128, MIT). Permissive.
- Confirm each crate's actual license in `Cargo.toml` metadata before committing — the research pass flagged this as an open item.

---

## 10. Verdict

**Feasible, and the payoff is real.** The Rust ecosystem covers every distinct capability the app needs (verified versions above), and the rewrite would delete a large amount of Electron-shaped complexity rather than merely translate it.

**The two genuine risks are the audio graph (must be built, not imported) and MIDI sound parity (rustysynth ≠ libfluidsynth until proven).** Neither is a blocker; both are testable early.

**The decision that sets the project size is the UI:** reusing the web UI through Tauri 2 is a bounded, high-return job; a fully native egui/iced UI roughly doubles it. Write the audio core UI-agnostic and that decision can be deferred until the engine works.

**Suggested first move:** Phase 1 — a headless cpal + ffmpeg-next spike. If a Rust binary can play, seek, and pitch-shift an MP3 cleanly with no underruns, the rest follows.
