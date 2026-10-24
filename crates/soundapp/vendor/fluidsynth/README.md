# FluidSynth 2.6.1 (vendored, official prebuilt Windows x64)

- **Source:** official GitHub release asset
  `fluidsynth-v2.6.1-win10-x64-cpp11.zip`
  from https://github.com/FluidSynth/fluidsynth/releases/tag/v2.6.1
  (published 2026-09-19, the self-contained C++11-OSAL build — GLib was removed
  upstream in 2.6.0, so the DLL has no runtime deps beyond OS components and
  libsndfile).
- **Asset SHA-256:** `fab7a2e4b85675b66970f97a39bbc239729c5e0f237198b5922a6a73cbc8677c`
- **License:** LGPL-2.1 (`LICENSE` here). We consume the C API via dynamic
  linking only — no static linking, no source modification — which satisfies
  the LGPL for the MIT-licensed app. Keep this LICENSE file distributed with
  the binary.

## Layout (subset of the official package)

| Path | What | Kept because |
|---|---|---|
| `include/` | `fluidsynth.h` + `fluidsynth/*.h` | public C API headers |
| `lib/libfluidsynth-3.lib` | MSVC x64 import library | link target (name is `libfluidsynth-3`, NOT `fluidsynth`) |
| `bin/libfluidsynth-3.dll` | runtime DLL | must sit next to the exe |
| `bin/sndfile.dll` | libsndfile runtime | **required**: `libfluidsynth-3.dll` imports `sndfile.dll` at load time (verified with `dumpbin /dependents`) |

Dropped from the official zip: `fluidsynth.exe` (CLI player), `SDL3.dll`
(audio driver for the CLI exe only — not in `libfluidsynth-3.dll`'s import
table), cmake/pkgconfig files.

## Link + staging contract for `crates/soundapp/build.rs`

```rust
// after the existing link instructions:
let fs_vendor = manifest_dir.join("vendor").join("fluidsynth");
println!("cargo:rustc-link-search=native={}", fs_vendor.join("lib").display());
println!("cargo:rustc-link-lib=dylib=libfluidsynth-3");
// stage runtime DLLs next to the produced binaries (same loop as RUNTIME_DLLS):
//   vendor/fluidsynth/bin/libfluidsynth-3.dll
//   vendor/fluidsynth/bin/sndfile.dll
```

Both DLLs go into `target/<profile>/` and `target/<profile>/deps/` exactly like
the FFmpeg DLL set (`RUNTIME_DLLS` pattern in build.rs).
