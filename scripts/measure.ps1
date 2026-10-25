# SoundApp POC — footprint / startup / CPU measurement.
#
#   .\measure.ps1 -Tone                 # 440 Hz tone (playing, ~60fps redraw)
#   .\measure.ps1                      # idle (no audio -> should settle to ~0% CPU)
#   .\measure.ps1 -File ..\_Material\mixer_media\Again\01.m4a
#
# Reports: time-to-first-frame, time-to-audio, working set, and CPU over a window.

param(
    [string]$File,
    [switch]$Tone,
    [switch]$Silence,
    [switch]$Static,
    [switch]$NoPaint,
    [string]$Backend = '',
    [string]$Font = '',
    [int]$Fps = 0,
    [int]$Ops = -1,
    [int]$WarmupMs = 2500,
    [int]$SampleMs = 3000,
    [int]$Seconds = 0,
    [switch]$NoGpu
)

$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\soundapp_r.exe' | Resolve-Path
$out = Join-Path $env:TEMP 'poc-stdout.txt'
$err = Join-Path $env:TEMP 'poc-stderr.txt'
if (Test-Path $out) { Remove-Item $out -Force }
if (Test-Path $err) { Remove-Item $err -Force }

# GPU Engine counters: locate the set by name fragment so localized Windows
# (e.g. "GPU-Engine") still resolves, then take the wildcard path template
# verbatim from the set and filter to this pid at sample time.
$gpuPath = $null
if (-not $NoGpu) {
    $set = Get-Counter -ListSet * -ErrorAction SilentlyContinue |
        Where-Object { $_.CounterSetName -match '^GPU[\s-]?Engine' } |
        Select-Object -First 1
    if ($set) {
        $gpuPath = @($set.Paths | Where-Object { $_ -match '\(\*\)' })[0]
    }
    if (-not $gpuPath) { Write-Output "note: GPU Engine counters not found - GPU sampling disabled" }
}

$argList = @()
if ($Backend) { $argList += '--backend'; $argList += $Backend }
if ($Font) { $argList += '--font'; $argList += $Font }
if ($Tone) { $argList += '--tone' }
elseif ($Silence) { $argList += '--silence' }
elseif ($File) { $argList += (Resolve-Path $File).Path }
if ($Static) { $argList += '--static' }
if ($NoPaint) { $argList += '--nopaint' }
if ($Fps -gt 0) { $argList += '--fps'; $argList += "$Fps" }
if ($Ops -ge 0) { $argList += '--ops'; $argList += "$Ops" }

# Auto-quit so the harness never leaves a process (or a tone) running.
if ($Seconds -le 0) { $Seconds = [int][math]::Ceiling(($WarmupMs + $SampleMs) / 1000.0) + 2 }
$argList += '--seconds'
$argList += "$Seconds"

# PS 5.1 Start-Process joins -ArgumentList with spaces and never quotes, so a
# path like "Y:\Test Files\x.mp3" would split into two arguments at the app.
$argList = $argList | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }

$heartbeat = [System.Diagnostics.Stopwatch]::StartNew()
$spArgs = @{
    FilePath               = $exe
    PassThru               = $true
    RedirectStandardOutput = $out
    RedirectStandardError  = $err
}
if ($argList.Count -gt 0) { $spArgs['ArgumentList'] = $argList }

# Everything from launch to kill is inside try/finally. The process is killed in
# the finally block, so a throw anywhere (Refresh, CIM, anything) can never
# orphan it — a leaked POC process means a tone that plays forever.
$p = $null
try {
    $p = Start-Process @spArgs
    Start-Sleep -Milliseconds $WarmupMs

    if ($p.HasExited) {
        Write-Output "process exited early (code $($p.ExitCode))"
        Get-Content $out
        exit 1
    }
    $p.Refresh()
    $wsMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
    $privMB = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
    $cpuA = $p.TotalProcessorTime
    $wallA = [System.Diagnostics.Stopwatch]::StartNew()

    # Sample window: CPU via the TotalProcessorTime delta below, GPU via one
    # GPU Engine snapshot per 500 ms summed over this process's engine
    # instances (3D / Copy / Video / ...).
    $gpuSamples = @()
    $gpuPid = "pid_$($p.Id)_"
    # Filter to this pid inside the counter path itself: fewer instances to
    # enumerate per snapshot, and a pid with no GPU activity raises quickly.
    $pidPath = if ($gpuPath) { $gpuPath -replace '\(\*\)', "(pid_$($p.Id)_*)" } else { $null }
    $sampleEnd = (Get-Date).AddMilliseconds($SampleMs)
    while ($true) {
        $remaining = ($sampleEnd - (Get-Date)).TotalMilliseconds
        if ($remaining -le 0) { break }
        if ($pidPath) {
            try {
                $sum = (Get-Counter $pidPath -ErrorAction Stop).CounterSamples |
                    Measure-Object CookedValue -Sum
                if ($sum.Sum) { $gpuSamples += [double]$sum.Sum }
            } catch { }
        }
        $sleepMs = [math]::Min(500.0, $remaining)
        if ($sleepMs -gt 0) { Start-Sleep -Milliseconds ([int]$sleepMs) }
    }

    # Watchdog: if the target ignored its own auto-quit, kill it rather than leak it.
    if (-not $p.HasExited -and $heartbeat.Elapsed.TotalSeconds -gt ($Seconds + 5)) {
        $p.Kill()
    }

    $p.Refresh()
    $cpuB = $p.TotalProcessorTime
    $wallMs = $wallA.Elapsed.TotalMilliseconds
    $cpuMs = ($cpuB - $cpuA).TotalMilliseconds
    # An app that exits during the window can report a rewound timer after
    # Refresh — clamp instead of printing a negative percentage.
    if ($cpuMs -lt 0) { $cpuMs = 0 }
    $pctOneCore = [math]::Round(100.0 * $cpuMs / $wallMs, 1)
    $exitedDuringSample = $p.HasExited

    $cores = (Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors
    $pctAllCores = [math]::Round($pctOneCore / $cores, 2)
} finally {
    if ($p -and -not $p.HasExited) {
        try { $p.Kill(); $p.WaitForExit() } catch { }
    }
}

Write-Output "=================================================="
Write-Output "SoundApp POC measurement"
Write-Output "args:            $(if ($argList.Count) { $argList -join ' ' } else { '<idle>' })"
Write-Output "working set:     $wsMB MB"
Write-Output "private bytes:   $privMB MB"
if ($exitedDuringSample) { Write-Output "note:            process exited during the sample window" }
Write-Output "CPU (one core):  $pctOneCore %   ($cores logical cores -> $pctAllCores % of machine)"
if ($gpuSamples.Count -gt 0) {
    $gpuAvg = [math]::Round(($gpuSamples | Measure-Object -Average).Average, 1)
    $gpuMax = [math]::Round(($gpuSamples | Measure-Object -Maximum).Maximum, 1)
    Write-Output "GPU (process):   avg $gpuAvg %   max $gpuMax %   ($($gpuSamples.Count) samples)"
} elseif (-not $NoGpu) {
    Write-Output "GPU (process):   no samples (no GPU Engine instances for this pid)"
}
Write-Output "wall to 2nd sample: $([math]::Round($heartbeat.Elapsed.TotalSeconds,1)) s"
Write-Output "--------------------------------------------------"
Write-Output "timings from stdout:"
Get-Content $out
if ((Test-Path $err) -and (Get-Item $err).Length -gt 0) {
    Write-Output "stderr:"
    Get-Content $err
}
