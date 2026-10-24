# Vendored: Rubber Band Library v4.0.0

Source: https://github.com/breakfastquay/rubberband tag `v4.0.0`
(exact-tag tarball; only `rubberband/`, `src/` minus `jni/`+`test/`,
`COPYING`, `CHANGELOG`, `COMPILING.md`, `meson.build` are kept).

## Licence — read before distributing

`COPYING` is the **GPL v2** ("or later"). Rubber Band Library is dual-licensed:
GPL-2.0-or-later, or a commercial licence from Particular Programs Ltd
(https://breakfastquay.com/rubberband/). Linking it into `soundapp_r` makes the
combined work GPL-2.0+ unless a commercial licence is obtained. The Electron
reference SoundApp already shipped Rubber Band through its GPL FFmpeg build.

## Build

Compiled into the `ffmpeg_bridge` static lib by `crates/soundapp/build.rs`
with the upstream MSVC define set for the dependency-free configuration:
builtin FFT (`USE_BUILTIN_FFT`), builtin BQResampler (`USE_BQRESAMPLER`),
plus `LACK_SINCOS` and `_WIN32 NOMINMAX _USE_MATH_DEFINES`.

`shim.cpp` is authored here (not upstream): a flat C ABI over
`RubberBandStretcher` (R3/finer engine, real-time mode) for the Rust side.

## Why R3 (finer) rather than R2

R3 is the high-quality engine with true formant preservation
(`OptionFormantPreserved`). It replaces the previous MIT Signalsmith Stretch
stage after A/B listening found Signalsmith clearly worse on music. The
reference Electron app used the R2 engine through FFmpeg's `rubberband`
filter; R3 is the same library's better engine.
