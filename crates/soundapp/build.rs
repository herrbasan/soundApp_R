// Compiles the C bridge over the reference SoundApp FFmpeg decoder and wires up
// linking against the vendored FFmpeg 7 import libs.
//
// The actual compile+archive runs through `vcvars64.bat` in a generated batch
// file: `cl.exe` needs the MSVC environment (INCLUDE/LIB/PATH), and driving that
// from Rust would mean a nest of quoting. A .bat has no such failure mode.
//
// The FFmpeg runtime DLLs are copied next to every produced binary (the profile
// dir and its `deps/` subdir, where `cargo test` puts test harnesses) so both
// `soundapp_r.exe` and `cargo test` resolve avformat-62.dll etc. at load time.

use std::path::{Path, PathBuf};
use std::process::Command;

const SOUNDAPP_ROOT: &str = r"D:\Work\_GIT\SoundApp";
const FFMPEG_INC_DEPS: &str = r"D:\Work\_GIT\SoundApp\libs\ffmpeg-napi-interface\deps\ffmpeg\include";
const FFMPEG_INC_SRC: &str = r"D:\Work\_GIT\SoundApp\libs\ffmpeg-napi-interface\src";
const FFMPEG_LIB_DIR: &str = r"D:\Work\_GIT\SoundApp\libs\ffmpeg-napi-interface\deps\win\lib";
const FFMPEG_BIN_DIR: &str = r"D:\Work\_GIT\SoundApp\bin\win_bin";

/// Import libs the bridge links against (order matters: dependents first).
const LINK_LIBS: [&str; 6] = [
    "avformat",
    "avcodec",
    "avutil",
    "swresample",
    "swscale",
    "avfilter",
];

/// FFmpeg runtime DLLs that must sit beside the executable.
const RUNTIME_DLLS: [&str; 7] = [
    "avformat-62.dll",
    "avcodec-62.dll",
    "avutil-60.dll",
    "swresample-6.dll",
    "avfilter-11.dll",
    "avdevice-62.dll",
    "swscale-9.dll",
];

fn main() {
    println!("cargo:rerun-if-changed=src/ffmpeg_bridge.cpp");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={FFMPEG_INC_SRC}/decoder.cpp");
    println!("cargo:rerun-if-changed={FFMPEG_INC_SRC}/decoder.h");

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let cpp = manifest_dir.join("src").join("ffmpeg_bridge.cpp");
    let obj = out_dir.join("ffmpeg_bridge.obj");
    let lib = out_dir.join("ffmpeg_bridge.lib");

    let vcvars = find_vcvars().unwrap_or_else(|| {
        panic!(
            "could not locate vcvars64.bat — install Visual Studio 2022 with the \
             \"Desktop development with C++\" workload, or set it under {}",
            SOUNDAPP_ROOT
        )
    });

    // --- Generate + run the compile batch file ---------------------------------
    let bat = out_dir.join("build_ffmpeg_bridge.bat");
    let bat_body = format!(
        "@echo off\r\n\
         call \"{vcvars}\" || exit /b 1\r\n\
         cl.exe /nologo /c /O2 /EHsc /std:c++17 /MD \
         /I\"{inc_deps}\" /I\"{inc_src}\" \
         \"{cpp}\" /Fo\"{obj}\" || exit /b 1\r\n\
         lib.exe /nologo /OUT:\"{lib}\" \"{obj}\" || exit /b 1\r\n",
        vcvars = vcvars.display(),
        inc_deps = FFMPEG_INC_DEPS,
        inc_src = FFMPEG_INC_SRC,
        cpp = cpp.display(),
        obj = obj.display(),
        lib = lib.display(),
    );
    std::fs::write(&bat, bat_body).expect("failed to write compile batch file");

    let output = Command::new("cmd.exe")
        .args(["/C", &bat.to_string_lossy()])
        .output()
        .expect("failed to spawn cmd.exe for FFmpeg bridge compile");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        panic!(
            "FFmpeg bridge compile failed (exit {:?})\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}",
            output.status.code()
        );
    }
    if !lib.exists() {
        panic!("FFmpeg bridge archive was not produced at {}", lib.display());
    }

    // --- Link instructions ------------------------------------------------------
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=ffmpeg_bridge");
    println!("cargo:rustc-link-search=native={FFMPEG_LIB_DIR}");
    for lib in LINK_LIBS {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }

    // --- Stage the FFmpeg runtime DLLs next to the produced binaries ------------
    // OUT_DIR = target/<profile>/build/<pkg>-<hash>/out → up three is target/<profile>.
    let profile_dir = out_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("unexpected OUT_DIR layout")
        .to_path_buf();
    let mut destinations = vec![profile_dir.clone()];
    destinations.push(profile_dir.join("deps"));

    let bin_dir = Path::new(FFMPEG_BIN_DIR);
    for dll in RUNTIME_DLLS {
        let src = bin_dir.join(dll);
        if !src.exists() {
            panic!("required FFmpeg runtime DLL missing: {}", src.display());
        }
        for dest_dir in &destinations {
            if !dest_dir.exists() {
                continue;
            }
            let dest = dest_dir.join(dll);
            // Skip the copy when the destination is already byte-identical, so a
            // rebuild does not churn file mtimes (and thus downstream relinks).
            if files_identical(&src, &dest) {
                continue;
            }
            std::fs::copy(&src, &dest).unwrap_or_else(|e| {
                panic!("failed to copy {} → {}: {e}", src.display(), dest.display())
            });
        }
    }
}

/// Locate the MSVC VS2022 environment. Prefers `vswhere` (the supported way),
/// then falls back to the three well-known install roots.
fn find_vcvars() -> Option<PathBuf> {
    let vswhere = r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe";
    if Path::new(vswhere).exists() {
        let out = Command::new(vswhere)
            .args([
                "-latest",
                "-products",
                "*",
                "-requires",
                "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                "-property",
                "installationPath",
            ])
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            let bat = PathBuf::from(path).join("VC/Auxiliary/Build/vcvars64.bat");
            if bat.exists() {
                return Some(bat);
            }
        }
    }
    for edition in ["Community", "Professional", "Enterprise", "BuildTools"] {
        let bat = PathBuf::from(format!(
            r"C:\Program Files\Microsoft Visual Studio\2022\{edition}\VC\Auxiliary\Build\vcvars64.bat"
        ));
        if bat.exists() {
            return Some(bat);
        }
    }
    None
}

fn files_identical(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.len() == mb.len() && {
            match (std::fs::read(a), std::fs::read(b)) {
                (Ok(ca), Ok(cb)) => ca == cb,
                _ => false,
            }
        },
        _ => false,
    }
}
