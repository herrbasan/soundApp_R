# Build a runnable distribution of soundapp_r: everything the app needs at
# runtime, nothing else.
#
#   .\make-dist.ps1                 # assemble (and refresh) dist\soundapp_r\
#   .\make-dist.ps1 -NoZip         # skip the archive
#   .\make-dist.ps1 -NoSmokeTest   # skip the launch verification
#
# Contents:
#   soundapp_r.exe                  GUI-subsystem player
#   av*-62/60.dll, sw*-6/9.dll      FFmpeg runtime (LGPL) — staged by build.rs
#   libfluidsynth-3.dll, sndfile.dll  FluidSynth runtime (LGPL)
#   soundfonts\default.sf2          General MIDI SoundFont (reference tree)
#   metronome\*.wav                 metronome clicks (falls back to synthesized)
#   licenses\                       upstream license notices
#   README.txt                      what this is
#
# libopenmpt and Rubber Band are statically linked into the exe (no DLL).

param(
    [switch]$NoZip,
    [switch]$NoSmokeTest,
    [string]$SoundFont = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot  # repo root
Set-Location $root

$dist = Join-Path $root 'dist\soundapp_r'
$release = Join-Path $root 'target\release'
$refTree = 'D:\Work\_GIT\SoundApp'
$assetRoot = Join-Path $root 'crates\soundapp\assets'

# --- 0. Release build (skipped when fresh, forced by missing exe) ------------
$exe = Join-Path $release 'soundapp_r.exe'
if (-not (Test-Path $exe)) {
    Write-Output 'building release...'
    cargo build --release -p soundapp
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
}

# --- 1. Clean staging ---------------------------------------------------------
if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
New-Item -ItemType Directory -Path $dist | Out-Null
New-Item -ItemType Directory -Path (Join-Path $dist 'soundfonts') | Out-Null
New-Item -ItemType Directory -Path (Join-Path $dist 'metronome') | Out-Null
New-Item -ItemType Directory -Path (Join-Path $dist 'licenses') | Out-Null
New-Item -ItemType Directory -Path (Join-Path $dist 'icons') | Out-Null

# --- 2. Binary + runtime DLLs -------------------------------------------------
$dlls = @(
    'avformat-62.dll', 'avcodec-62.dll', 'avutil-60.dll', 'swresample-6.dll',
    'avfilter-11.dll', 'avdevice-62.dll', 'swscale-9.dll',
    'libfluidsynth-3.dll', 'sndfile.dll'
)
Copy-Item $exe $dist
foreach ($dll in $dlls) {
    $src = Join-Path $release $dll
    if (-not (Test-Path $src)) { throw "runtime DLL missing (build first): $src" }
    Copy-Item $src $dist
}

# --- 2b. File-association and application icons -------------------------------
$iconDir = Join-Path $assetRoot 'icons'
Copy-Item (Join-Path $iconDir '*.ico') (Join-Path $dist 'icons')

# --- 3. SoundFont (General MIDI; required for MIDI playback) ------------------
$sf2 = if ($SoundFont) {
    $SoundFont
} elseif ($env:SOUNDAPP_SOUNDFONT) {
    $env:SOUNDAPP_SOUNDFONT
} elseif (Test-Path (Join-Path $assetRoot 'soundfonts\default.sf2')) {
    Join-Path $assetRoot 'soundfonts\default.sf2'
} else {
    Join-Path $refTree 'bin\soundfonts\default.sf2'
}
if (-not (Test-Path $sf2)) {
    throw "SoundFont missing: $sf2 - pass -SoundFont, set SOUNDAPP_SOUNDFONT, or put a GM .sf2 into dist\soundapp_r\soundfonts\"
}
Copy-Item $sf2 (Join-Path $dist 'soundfonts\default.sf2')

# --- 4. Metronome clicks (optional; app falls back to synthesized clicks) -----
foreach ($name in 'metronome-high.wav', 'metronome-low.wav') {
    $local = Join-Path $assetRoot "metronome\$name"
    $dev = Join-Path $refTree "bin\metronome\$name"
    $src = if (Test-Path $local) { $local } else { $dev }
    if (Test-Path $src) {
        Copy-Item $src (Join-Path $dist "metronome\$name")
    } else {
        Write-Output "note: $name missing - metronome will use synthesized clicks"
    }
}

# --- 5. License notices (vendored sources + runtime libraries) ----------------
$lic = Join-Path $dist 'licenses'
Copy-Item (Join-Path $root 'crates\soundapp\vendor\rubberband\COPYING') (Join-Path $lic 'Rubber-Band-COPYING-GPL-2.0.txt')
Copy-Item (Join-Path $root 'crates\soundapp\vendor\rubberband\LICENSE-shim.md') $lic
Copy-Item (Join-Path $root 'crates\soundapp\vendor\fluidsynth\LICENSE') (Join-Path $lic 'FluidSynth-LGPL-2.1.txt')
Copy-Item (Join-Path $root 'crates\soundapp\vendor\libopenmpt\LICENSE') (Join-Path $lic 'libopenmpt-BSD-3-Clause.txt')
Copy-Item (Join-Path $root 'crates\soundapp\vendor\ffmpeg-decoder\LICENSE') (Join-Path $lic 'FFmpeg-Decoder-MIT.txt')

$notice = @"
soundapp_r distribution - third-party notices
=============================================

 FFmpeg (avformat/avcodec/avutil/swresample/avfilter/avdevice/swscale DLLs)
   LGPL-2.1-or-later. Runtime DLLs built from the reference SoundApp tree.
   Source: https://ffmpeg.org
 FluidSynth (libfluidsynth-3.dll, sndfile.dll)
   LGPL-2.1-or-later. Vendored: crates/soundapp/vendor/fluidsynth
 GeneralUser GS SoundFont (soundfonts/default.sf2)
   S. Christian Collins; permissive redistribution license.
   Source: https://archive.org/download/free-soundfonts-sf2-2019-04/GeneralUser%20GS%20v1.471.sf2
 libopenmpt
   BSD-3-Clause, statically linked. Vendored: crates/soundapp/vendor/libopenmpt
 ffmpeg-napi-interface decoder bridge
   MIT. Vendored: crates/soundapp/vendor/ffmpeg-decoder
 Rubber Band Library
   GPL-2.0-or-later, statically linked (R3 engine). Vendored:
   crates/soundapp/vendor/rubberband. Distributed combined works must comply
   with GPL-2.0-or-later; noncommercial use is permitted by the license.
 nui (this repository's UI library) and soundapp_r: project licensing is
   undecided yet - see docs/handover_2026-10-24.md section 9 item 7.
"@
[System.IO.File]::WriteAllText((Join-Path $lic 'NOTICES.txt'), $notice, (New-Object System.Text.UTF8Encoding $false))

# --- 6. README ----------------------------------------------------------------
$readme = @"
soundapp_r - native audio player
================================

Run: soundapp_r.exe   (double-click; no console window opens)

- Drop or open an audio file/folder: mp3/m4a/flac/wav/ogg/opus via FFmpeg,
  .mod/.s3m/.xm/.it via the built-in tracker engine, .mid/.midi via FluidSynth.
- C toggles the compact player. M monitoring, X mixer, P parameters,
  S settings, H help. +/-/0 or Ctrl+Wheel scales the UI.
- MIDI playback needs the bundled soundfonts\default.sf2 (replace with any
  General MIDI .sf2/.sf3 placed in that folder).
- Diagnostics: perf_log.txt (startup, file-open and frame-pacing timings)
  is written next to the exe when the folder is writable.
"@
[System.IO.File]::WriteAllText((Join-Path $dist 'README.txt'), $readme, (New-Object System.Text.UTF8Encoding $false))

# --- 7. Smoke test: the dist must run standalone ------------------------------
if (-not $NoSmokeTest) {
    $out = Join-Path $env:TEMP 'dist-smoke-out.txt'
    $err = Join-Path $env:TEMP 'dist-smoke-err.txt'
    $p = Start-Process -FilePath (Join-Path $dist 'soundapp_r.exe') `
        -ArgumentList '--seconds', '3', '--silence' `
        -WorkingDirectory $dist `
        -RedirectStandardOutput $out -RedirectStandardError $err -PassThru -Wait
    $stdout = Get-Content $out -Raw
    $stderr = Get-Content $err -Raw -ErrorAction SilentlyContinue
    # A headless runner has no audio device: the app then reports
    # `audio init failed: no output device` and never prints `output config`.
    # The smoke test is about the dist running standalone (window created,
    # frame presented, clean exit) — not about the machine having a sound card.
    $noDevice = "$stdout$stderr" -match 'no output device'
    $audioOk = ($stdout -match 'output config') -or $noDevice
    if ($p.ExitCode -ne 0 -or -not $audioOk -or $stdout -notmatch 'first frame') {
        Write-Output $stdout
        Get-Content $err
        throw "dist smoke test failed (exit $($p.ExitCode))"
    }
    Write-Output ($stdout.Trim() -split "`r?`n" | Where-Object { $_ -match 'output config|first frame' })
    if ($noDevice) {
        Write-Output 'smoke test: dist runs standalone (no audio device on this machine)'
    } else {
        Write-Output 'smoke test: dist runs standalone (silent output, no audio)'
    }
}

# --- 8. Archive ---------------------------------------------------------------
# The smoke test (or a manual run) leaves runtime state in the staging folder —
# the archive ships the app, not a used profile. Everything the app writes next
# to `user.json` is runtime state: config, the perf log and the event log.
Remove-Item (Join-Path $dist 'perf_log.txt'), (Join-Path $dist 'user.json'), `
    (Join-Path $dist 'event_log.txt') -Force -ErrorAction SilentlyContinue
if (-not $NoZip) {
    $zip = Join-Path $root 'dist\soundapp_r.zip'
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $dist '*') -DestinationPath $zip
}

# --- 9. Report ----------------------------------------------------------------
$files = Get-ChildItem $dist -Recurse -File
$mb = [math]::Round(($files | Measure-Object Length -Sum).Sum / 1MB, 1)
Write-Output "=================================================="
Write-Output ("dist ready: {0}  ({1} files, {2} MB)" -f $dist, $files.Count, $mb)
Get-ChildItem $dist -Recurse -File | ForEach-Object {
    Write-Output ("  {0,10:N1} KB  {1}" -f ($_.Length / 1KB), $_.FullName.Substring($dist.Length + 1))
}
if (-not $NoZip) {
    $zip = Get-Item (Join-Path $root 'dist\soundapp_r.zip')
    Write-Output ("archive:    {0} ({1} MB)" -f $zip.FullName, [math]::Round($zip.Length / 1MB, 1))
}
