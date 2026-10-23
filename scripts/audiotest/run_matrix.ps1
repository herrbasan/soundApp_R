# Phase A instrument harness: render matrix + analysis table.
# ASCII only (PS 5.1 rule, development_plan section 3 #7).
#
#   powershell -File scripts\audiotest\run_matrix.ps1
#   powershell -File scripts\audiotest\run_matrix.ps1 -SkipGen   (reuse signals)
#
# Renders via soundapp_r --render (same DSP path as live playback) and prints
# one analysis line per render. Exits non-zero if any render fails.
param(
    [string]$Exe = (Join-Path $PSScriptRoot "..\..\target\release\soundapp_r.exe"),
    [string]$OutDir = (Join-Path $PSScriptRoot "..\..\target\audiotest"),
    [switch]$SkipGen
)
$ErrorActionPreference = "Stop"
$root = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$OutDir = [System.IO.Path]::GetFullPath((Join-Path $root "target\audiotest"))
$renderDir = Join-Path $OutDir "out"
New-Item -ItemType Directory -Force -Path $renderDir | Out-Null
$Exe = [System.IO.Path]::GetFullPath($Exe)
if (-not (Test-Path $Exe)) { throw "soundapp_r.exe not found: $Exe" }

if (-not $SkipGen) {
    python (Join-Path $PSScriptRoot "gen_signals.py") $OutDir
    if ($LASTEXITCODE -ne 0) { throw "gen_signals failed" }
}

$sine   = Join-Path $OutDir "sine_1k_10s.wav"
$clicks = Join-Path $OutDir "clicks_10s.wav"
$real   = "Y:\Test Files\Murr_01.mp3"
if (-not (Test-Path $real)) { $real = "D:\Work\_GIT\SoundApp\_Material\mixer_media\Again\01.m4a" }
if (-not (Test-Path $real)) { throw "no real test clip found" }

$analyzer = Join-Path $PSScriptRoot "analyze_wav.py"
$fail = 0

function Render($inFile, $outFile, $pitch, $tempo, $tape, $limit) {
    $args7 = @("--render", $inFile, "--out", $outFile)
    if ($tape -ne $null) { $args7 += @("--tape", "$tape") } else { $args7 += @("--pitch", "$pitch") }
    $args7 += @("--tempo", "$tempo")
    if ($limit -gt 0) { $args7 += @("--limit", "$limit") }
    & $Exe @args7 | Write-Host
    if ($LASTEXITCODE -ne 0) { Write-Host "RENDER FAILED: $inFile pitch=$pitch tempo=$tempo tape=$tape"; $script:fail++ }
}

function Analyze($file, $type, $pitch, $tempo, $inDur, $f0, $srcRms) {
    $a = @($file, "--type", $type, "--pitch", "$pitch", "--tempo", "$tempo", "--in-dur", "$inDur")
    if ($f0 -gt 0) { $a += @("--f0", "$f0") }
    if ($srcRms -ne $null) { $a += @("--src-rms-db", "$srcRms") }
    & python $analyzer @a
    if ($LASTEXITCODE -ne 0) { Write-Host "ANALYZE FAILED: $file"; $script:fail++ }
}

Write-Host "=== source references ==="
Analyze $sine "sine" 0 1.0 10.0 1000.0 $null
Analyze $clicks "clicks" 0 1.0 10.0 0 $null
Write-Host ""

Write-Host "=== matrix: sine 1 kHz, PitchTime (pitch x tempo) ==="
foreach ($p in @(-12, -5, 0, 5, 12)) {
    foreach ($t in @("0.5", "0.8", "1.0", "1.25", "1.5")) {
        $tag = "sine_p{0}_t{1}" -f $p, $t
        $out = Join-Path $renderDir "$tag.wav"
        Render $sine $out $p $t $null 0
        if (Test-Path $out) { Analyze $out "sine" $p $t 10.0 1000.0 $null }
    }
    Write-Host ""
}

Write-Host "=== matrix: click train, PitchTime ==="
foreach ($p in @(0, 5)) {
    foreach ($t in @("0.5", "1.0", "1.5")) {
        $tag = "clk_p{0}_t{1}" -f $p, $t
        $out = Join-Path $renderDir "$tag.wav"
        Render $clicks $out $p $t $null 0
        if (Test-Path $out) { Analyze $out "clicks" $p $t 10.0 0 $null }
    }
}
Write-Host ""

Write-Host "=== matrix: real clip (10 s limit), PitchTime ==="
foreach ($p in @(-5, 0, 5)) {
    foreach ($t in @("0.8", "1.25")) {
        $tag = "real_p{0}_t{1}" -f $p, $t
        $out = Join-Path $renderDir "$tag.wav"
        Render $real $out $p $t $null 10
        if (Test-Path $out) { Analyze $out "music" $p $t 10.0 0 -13.0 }
    }
}
Write-Host ""

Write-Host "=== sanity: tape mode (pitch/tempo coupled) ==="
foreach ($combo in @(@("5", "1.0", 1334.8), @("0", "0.5", 500.0))) {
    $tp = $combo[0]; $t = $combo[1]; $fexp = [double]$combo[2]
    $tag = "tape_p{0}_t{1}" -f $tp, $t
    $out = Join-Path $renderDir "$tag.wav"
    Render $sine $out 0 $t $tp 0
    if (Test-Path $out) { Analyze $out "sine" 0 $t 10.0 $fexp $null }
}

Write-Host ""
Write-Host "harness failures: $fail"
exit $(if ($fail -gt 0) { 1 } else { 0 })
