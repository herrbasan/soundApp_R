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
const FFMPEG_INC_DEPS: &str =
    r"D:\Work\_GIT\SoundApp\libs\ffmpeg-napi-interface\deps\ffmpeg\include";
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

/// Rubber Band Library v4.0.0 translation units, in upstream `meson.build`
/// `library_sources` order minus the C-API wrapper (our shim replaces it).
/// Dependency-free configuration: builtin FFT + builtin BQResampler, so no
/// kissfft/speex/fftw TUs and no external libs.
const RUBBERBAND_SOURCES: [&str; 22] = [
    "src/RubberBandStretcher.cpp",
    "src/RubberBandLiveShifter.cpp",
    "src/faster/AudioCurveCalculator.cpp",
    "src/faster/CompoundAudioCurve.cpp",
    "src/faster/HighFrequencyAudioCurve.cpp",
    "src/faster/SilentAudioCurve.cpp",
    "src/faster/PercussiveAudioCurve.cpp",
    "src/faster/R2Stretcher.cpp",
    "src/faster/StretcherChannelData.cpp",
    "src/faster/StretcherProcess.cpp",
    "src/common/Allocators.cpp",
    "src/common/BQResampler.cpp",
    "src/common/FFT.cpp",
    "src/common/Log.cpp",
    "src/common/Profiler.cpp",
    "src/common/Resampler.cpp",
    "src/common/StretchCalculator.cpp",
    "src/common/sysutils.cpp",
    "src/common/mathmisc.cpp",
    "src/common/Thread.cpp",
    "src/finer/R3Stretcher.cpp",
    "src/finer/R3LiveShifter.cpp",
];

/// MSVC define set from upstream meson.build for the builtin-FFT /
/// builtin-resampler Windows build.
const RUBBERBAND_DEFINES: [&str; 6] = [
    "/D_WIN32",
    "/DNOMINMAX",
    "/D_USE_MATH_DEFINES",
    "/DUSE_BUILTIN_FFT",
    "/DUSE_BQRESAMPLER",
    "/DLACK_SINCOS",
];

/// libopenmpt TU groups, one per vendored VS2022 project. Each project's
/// Release|x64 `<PreprocessorDefinitions>` / `<LanguageStandard*>` is read
/// from the .vcxproj itself, so the define sets cannot drift from upstream.
///
/// The per-project split is load-bearing, not cosmetic: libopenmpt's own
/// define set contains `STB_VORBIS_HEADER_ONLY` (its TUs see declarations
/// only), while `ext/stb_vorbis.vcxproj` compiles `stb_vorbis.c` WITHOUT
/// that define — that TU is where the implementation symbols come from.
/// Compiling every TU with the libopenmpt set empties `stb_vorbis.c` and the
/// archive links with unresolved `stb_vorbis_*` symbols the moment any real
/// binary pulls the decoder (the standalone harness in target/openmpt-work
/// worked around exactly this by recompiling stb_vorbis.c separately).
struct OpenmptGroup {
    project: PathBuf,
    sources: Vec<PathBuf>,
    defines: String,
    /// `/std:` flag from the project's `<LanguageStandard>` (C++) or
    /// `<LanguageStandard_C>` (C) — libopenmpt TUs are C++20, ext decoders C17.
    std_flag: &'static str,
}

fn openmpt_groups(root: &Path) -> Vec<OpenmptGroup> {
    let projects = [
        "build/vs2022win10/libopenmpt-small.vcxproj",
        "build/vs2022win10/ext/minimp3.vcxproj",
        "build/vs2022win10/ext/miniz.vcxproj",
        "build/vs2022win10/ext/stb_vorbis.vcxproj",
    ];
    projects
        .iter()
        .map(|rel| {
            let project = root.join(rel);
            let xml = std::fs::read_to_string(&project)
                .unwrap_or_else(|e| panic!("failed to read {}: {e}", project.display()));
            let base = project.parent().unwrap();
            let mut sources = Vec::new();
            let mut rest = xml.as_str();
            while let Some(pos) = rest.find("<ClCompile Include=\"") {
                rest = &rest[pos + 20..];
                let end = rest.find('"').expect("unterminated ClCompile Include");
                let path = base.join(rest[..end].replace('\\', "/"));
                if !path.exists() {
                    panic!(
                        "libopenmpt source listed by MSVC project is missing: {}",
                        path.display()
                    );
                }
                println!("cargo:rerun-if-changed={}", path.display());
                sources.push(path);
                rest = &rest[end + 1..];
            }
            assert!(
                !sources.is_empty(),
                "{} lists no ClCompile entries",
                project.display()
            );

            // The single Release|x64 <ItemDefinitionGroup> carries this
            // project's define set and language standard.
            let (defines, std_flag) = release_x64_settings(&xml, &project);
            OpenmptGroup {
                project,
                sources,
                defines,
                std_flag,
            }
        })
        .collect()
}

/// Extract the Release|x64 `PreprocessorDefinitions` (as a ready `/D` string)
/// and the language-standard flag from a vendored .vcxproj.
fn release_x64_settings(xml: &str, project: &Path) -> (String, &'static str) {
    let mut rest = xml;
    while let Some(pos) = rest.find("<ItemDefinitionGroup") {
        let open_end = pos
            + rest[pos..]
                .find('>')
                .expect("unterminated ItemDefinitionGroup");
        let header = &rest[pos..open_end];
        let Some(body_end) = rest[open_end..].find("</ItemDefinitionGroup>") else {
            break;
        };
        let body = &rest[open_end..open_end + body_end];
        if header.contains("'Release|x64'") {
            let element = |name: &str| -> Option<String> {
                let open = format!("<{name}>");
                let start = body.find(&open)? + open.len();
                let end = start + body[start..].find("</")?;
                Some(body[start..end].to_string())
            };
            let mut defines = String::new();
            if let Some(list) = element("PreprocessorDefinitions") {
                for entry in list.split(';') {
                    // `%(...)` inheritance tokens are MSBuild-only.
                    if entry.is_empty() || entry.starts_with("%(") {
                        continue;
                    }
                    defines.push_str(&format!(" /D{entry}"));
                }
            } else {
                panic!(
                    "{} Release|x64 has no PreprocessorDefinitions",
                    project.display()
                );
            }
            let std_flag = match element("LanguageStandard").as_deref() {
                Some("stdcpp20") => "/std:c++20",
                Some(other) => panic!(
                    "unhandled LanguageStandard {other:?} in {}",
                    project.display()
                ),
                None => match element("LanguageStandard_C").as_deref() {
                    Some("stdc17") => "/std:c17",
                    Some(other) => {
                        panic!(
                            "unhandled LanguageStandard_C {other:?} in {}",
                            project.display()
                        )
                    }
                    None => "",
                },
            };
            return (defines, std_flag);
        }
        rest = &rest[open_end + body_end..];
    }
    panic!(
        "{} has no Release|x64 ItemDefinitionGroup",
        project.display()
    );
}

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

/// FluidSynth runtime DLLs (vendored, dynamically linked — LGPL). Both must
/// sit beside the executable: `libfluidsynth-3.dll` is the synth, `sndfile.dll`
/// is its SF3 loader dependency.
const FLUIDSYNTH_DLLS: [&str; 2] = ["libfluidsynth-3.dll", "sndfile.dll"];

fn main() {
    println!("cargo:rerun-if-changed=src/ffmpeg_bridge.cpp");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={FFMPEG_INC_SRC}/decoder.cpp");
    println!("cargo:rerun-if-changed={FFMPEG_INC_SRC}/decoder.h");
    println!("cargo:rerun-if-changed=vendor/rubberband/shim.cpp");
    for src in RUBBERBAND_SOURCES {
        println!("cargo:rerun-if-changed=vendor/rubberband/{src}");
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let cpp = manifest_dir.join("src").join("ffmpeg_bridge.cpp");
    let obj = out_dir.join("ffmpeg_bridge.obj");
    let lib = out_dir.join("ffmpeg_bridge.lib");
    // Vendored Rubber Band Library v4.0.0 (see vendor/rubberband/).
    let rb_root = manifest_dir.join("vendor").join("rubberband");
    let rb_shim = rb_root.join("shim.cpp");
    let rb_obj_dir = out_dir.join("rb");
    std::fs::create_dir_all(&rb_obj_dir).expect("failed to create rb obj dir");

    let vcvars = find_vcvars().unwrap_or_else(|| {
        panic!(
            "could not locate vcvars64.bat — install Visual Studio 2022 with the \
             \"Desktop development with C++\" workload, or set it under {}",
            SOUNDAPP_ROOT
        )
    });

    // --- Generate + run the compile batch file ---------------------------------
    // Three translation groups go into one static lib: the FFmpeg bridge, the
    // vendored Rubber Band Library v4.0.0 (upstream TU list, builtin FFT +
    // BQResampler, upstream MSVC defines) and our C-ABI shim over
    // RubberBandStretcher.
    let mut rb_paths = String::new();
    let mut rb_objs = String::new();
    for src in RUBBERBAND_SOURCES {
        rb_paths.push_str(&format!(" \"{}\"", rb_root.join(src).display()));
        let stem = Path::new(src).file_stem().unwrap().to_str().unwrap();
        rb_objs.push_str(&format!(
            " \"{}\"",
            rb_obj_dir.join(format!("{stem}.obj")).display()
        ));
    }
    rb_objs.push_str(&format!(" \"{}\"", rb_obj_dir.join("shim.obj").display()));
    let defines = RUBBERBAND_DEFINES.join(" ");
    let bat = out_dir.join("build_ffmpeg_bridge.bat");
    let bat_body = format!(
        "@echo off\r\n\
         call \"{vcvars}\" || exit /b 1\r\n\
         cl.exe /nologo /c /O2 /EHsc /std:c++17 /MD \
         /I\"{inc_deps}\" /I\"{inc_src}\" \
         \"{cpp}\" /Fo\"{obj}\" || exit /b 1\r\n\
         cl.exe /nologo /c /O2 /EHsc /std:c++17 /MD {defines} \
         /I\"{rb_inc_pub}\" /I\"{rb_inc_src}\" \
         \"{shim}\"{rb_paths} /Fo{rb_obj_dir}\\ || exit /b 1\r\n\
         lib.exe /nologo /OUT:\"{lib}\" \"{obj}\"{rb_objs} || exit /b 1\r\n",
        vcvars = vcvars.display(),
        inc_deps = FFMPEG_INC_DEPS,
        inc_src = FFMPEG_INC_SRC,
        cpp = cpp.display(),
        obj = obj.display(),
        defines = defines,
        rb_inc_pub = rb_root.display(),
        rb_inc_src = rb_root.join("src").display(),
        shim = rb_shim.display(),
        rb_paths = rb_paths,
        rb_obj_dir = rb_obj_dir.display(),
        rb_objs = rb_objs,
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
        panic!(
            "FFmpeg bridge archive was not produced at {}",
            lib.display()
        );
    }
    build_openmpt(&manifest_dir, &out_dir, &vcvars);

    // --- Link instructions ------------------------------------------------------
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=ffmpeg_bridge");
    println!("cargo:rustc-link-lib=static=openmpt");
    println!("cargo:rustc-link-search=native={FFMPEG_LIB_DIR}");
    for lib in LINK_LIBS {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    // FluidSynth 2.6.1 (vendored import lib + runtime DLLs).
    let fluidsynth_root = manifest_dir.join("vendor").join("fluidsynth");
    println!(
        "cargo:rustc-link-search=native={}",
        fluidsynth_root.join("lib").display()
    );
    println!("cargo:rustc-link-lib=dylib=libfluidsynth-3");

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

    // --- Stage the vendored FluidSynth DLLs the same way ------------------------
    for dll in FLUIDSYNTH_DLLS {
        let src = fluidsynth_root.join("bin").join(dll);
        if !src.exists() {
            panic!("required FluidSynth runtime DLL missing: {}", src.display());
        }
        for dest_dir in &destinations {
            if !dest_dir.exists() {
                continue;
            }
            let dest = dest_dir.join(dll);
            if files_identical(&src, &dest) {
                continue;
            }
            std::fs::copy(&src, &dest).unwrap_or_else(|e| {
                panic!("failed to copy {} → {}: {e}", src.display(), dest.display())
            });
        }
    }
}

/// Compile the vendored libopenmpt TU groups (see [`OpenmptGroup`]) into one
/// static lib. Sources, defines and language standards all come from the
/// vendored VS2022 projects instead of a second, drifting list here.
fn build_openmpt(manifest_dir: &Path, out_dir: &Path, vcvars: &Path) {
    let root = manifest_dir.join("vendor").join("libopenmpt");
    let groups = openmpt_groups(&root);
    let total: usize = groups.iter().map(|g| g.sources.len()).sum();
    assert!(
        total > 150,
        "unexpectedly small libopenmpt source list ({total})"
    );

    let obj_dir = out_dir.join("openmpt");
    std::fs::create_dir_all(&obj_dir).expect("failed to create openmpt obj dir");
    let lib = out_dir.join("openmpt.lib");
    let bat = out_dir.join("build_openmpt.bat");
    let rsp = out_dir.join("openmpt_lib.rsp");
    let mut body = format!(
        "@echo off\r\ncall \"{}\" || exit /b 1\r\n",
        vcvars.display()
    );
    let mut objects = Vec::new();
    let mut index = 0usize;
    for group in &groups {
        println!("cargo:rerun-if-changed={}", group.project.display());
        for source in &group.sources {
            let obj = obj_dir.join(format!("{index:03}.obj"));
            index += 1;
            body.push_str(&format!(
                "cl.exe /nologo /c /O2 /EHsc /MD {std_flag}{defines} \
                 /I\"{root}\" /I\"{inc}\" /I\"{src}\" /I\"{common}\" \
                 \"{source}\" /Fo\"{obj}\" || exit /b 1\r\n",
                std_flag = group.std_flag,
                defines = group.defines,
                root = root.display(),
                inc = root.join("include").display(),
                src = root.join("src").display(),
                common = root.join("common").display(),
                source = source.display(),
                obj = obj.display(),
            ));
            objects.push(format!("\"{}\"", obj.display()));
        }
    }
    body.push_str(&format!(
        "lib.exe /nologo /OUT:\"{}\" @\"{}\" || exit /b 1\r\n",
        lib.display(),
        rsp.display()
    ));
    std::fs::write(&rsp, objects.join("\r\n")).expect("failed to write openmpt lib response file");
    std::fs::write(&bat, body).expect("failed to write openmpt compile batch file");
    let output = Command::new("cmd.exe")
        .args(["/C", &bat.to_string_lossy()])
        .output()
        .expect("failed to spawn cmd.exe for libopenmpt compile");
    if !output.status.success() {
        panic!(
            "libopenmpt compile failed (exit {:?})\n--- stdout ---\n{}\n--- stderr ---\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    if !lib.exists() {
        panic!("libopenmpt archive was not produced at {}", lib.display());
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
        (Ok(ma), Ok(mb)) => {
            ma.len() == mb.len() && {
                match (std::fs::read(a), std::fs::read(b)) {
                    (Ok(ca), Ok(cb)) => ca == cb,
                    _ => false,
                }
            }
        }
        _ => false,
    }
}
