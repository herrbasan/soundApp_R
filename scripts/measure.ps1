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
    [int]$Seconds = 0
)

$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot '..\target\release\soundapp_r.exe' | Resolve-Path
$out = Join-Path $env:TEMP 'poc-stdout.txt'
$err = Join-Path $env:TEMP 'poc-stderr.txt'
if (Test-Path $out) { Remove-Item $out -Force }
if (Test-Path $err) { Remove-Item $err -Force }

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

    Start-Sleep -Milliseconds $SampleMs

    # Watchdog: if the target ignored its own auto-quit, kill it rather than leak it.
    if (-not $p.HasExited -and $heartbeat.Elapsed.TotalSeconds -gt ($Seconds + 5)) {
        $p.Kill()
    }

    $p.Refresh()
    $cpuB = $p.TotalProcessorTime
    $wallMs = $wallA.Elapsed.TotalMilliseconds
    $cpuMs = ($cpuB - $cpuA).TotalMilliseconds
    $pctOneCore = [math]::Round(100.0 * $cpuMs / $wallMs, 1)

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
Write-Output "CPU (one core):  $pctOneCore %   ($cores logical cores -> $pctAllCores % of machine)"
Write-Output "wall to 2nd sample: $([math]::Round($heartbeat.Elapsed.TotalSeconds,1)) s"
Write-Output "--------------------------------------------------"
Write-Output "timings from stdout:"
Get-Content $out
if ((Test-Path $err) -and (Get-Item $err).Length -gt 0) {
    Write-Output "stderr:"
    Get-Content $err
}
