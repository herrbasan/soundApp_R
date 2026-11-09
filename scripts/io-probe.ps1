# Probe a player process: CPU (% of one core) AND disk read bytes over a
# steady window, after a warmup. Kills the process at the end.
#
#   .\scripts\io-probe.ps1 -Exe .\target\release\soundapp_r.exe -ProcArgs '<media>','--seconds','12'
#
# Paths containing spaces MUST carry their own quotes, or Start-Process splits
# them and the app opens the wrong fragment ('"Y:\Test Files\x.flac"').
#
# ReadTransferCount counts bytes the process asked the filesystem for
# (FFmpeg ring top-ups); SMB cache hits still count as process reads.

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string[]]$ProcArgs = @(),
    [int]$WarmupMs = 3000,
    [int]$SampleMs = 6000
)

$ErrorActionPreference = 'Stop'
$p = Start-Process -FilePath $Exe -ArgumentList $ProcArgs -PassThru
Start-Sleep -Milliseconds $WarmupMs
if ($p.HasExited) { Write-Output "exited during warmup (code $($p.ExitCode))"; exit 1 }

$p.Refresh()
$cpuA = $p.TotalProcessorTime
$procA = Get-CimInstance Win32_Process -Filter "ProcessId=$($p.Id)"
$ioA = $procA.ReadTransferCount
$opsA = $procA.ReadOperationCount
$wall = [System.Diagnostics.Stopwatch]::StartNew()

Start-Sleep -Milliseconds $SampleMs

$p.Refresh()
$cpuB = $p.TotalProcessorTime
$procB = Get-CimInstance Win32_Process -Filter "ProcessId=$($p.Id)"
$ioB = $procB.ReadTransferCount
$opsB = $procB.ReadOperationCount
$wsMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
$privMB = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
$cpuPct = [math]::Round(100.0 * ($cpuB - $cpuA).TotalMilliseconds / $wall.Elapsed.TotalMilliseconds, 1)
$readKbs = [math]::Round(($ioB - $ioA) / 1KB / ($SampleMs / 1000.0), 1)
$opsPerS = [math]::Round(($opsB - $opsA) / ($SampleMs / 1000.0), 1)
if (-not $p.HasExited) { try { $p.Kill(); $p.WaitForExit() } catch { } }
Write-Output ("cpu={0}% of one core | disk read={1} KB/s over {2}s ({3} read ops/s) | WS={4} MB | private={5} MB" -f $cpuPct, $readKbs, ($SampleMs / 1000), $opsPerS, $wsMB, $privMB)
