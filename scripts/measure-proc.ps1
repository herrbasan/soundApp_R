# Measure any process: startup, working set, CPU over a window. Kills it at the end.
#
#   .\measure-proc.ps1 -Exe ffplay -Args '-nodisp','-autoexit','file.m4a'
#   .\measure-proc.ps1 -Exe ffmpeg -Args '-re','-i','file.m4a','-f','null','-'
#
# Reports CPU as % of ONE core (TotalProcessorTime delta / wall delta * 100).

param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [Alias('Args')][string[]]$ProcArgs = @(),
    [int]$WarmupMs = 2500,
    [int]$SampleMs = 4000
)

$ErrorActionPreference = 'Stop'
$cmd = Get-Command $Exe -ErrorAction Stop

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$spArgs = @{ FilePath = $cmd.Source; PassThru = $true }
if ($ProcArgs.Count -gt 0) { $spArgs['ArgumentList'] = $ProcArgs }

# Everything from launch to kill is inside try/finally: a throw anywhere can
# never orphan the measured process (an orphaned ffplay keeps playing).
$p = $null
try {
    $p = Start-Process @spArgs
    Start-Sleep -Milliseconds $WarmupMs

    if ($p.HasExited) {
        Write-Output "$Exe exited after $([math]::Round($sw.Elapsed.TotalMilliseconds)) ms (code $($p.ExitCode)) - too short to sample"
        exit 1
    }

    $p.Refresh()
    $wsMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
    $privMB = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
    $cpuA = $p.TotalProcessorTime
    $wallA = [System.Diagnostics.Stopwatch]::StartNew()
    Start-Sleep -Milliseconds $SampleMs
    $p.Refresh()
    $cpuB = $p.TotalProcessorTime
    $wallMs = $wallA.Elapsed.TotalMilliseconds
    $pct = [math]::Round(100.0 * ($cpuB - $cpuA).TotalMilliseconds / $wallMs, 1)
} finally {
    if ($p -and -not $p.HasExited) {
        try { $p.Kill(); $p.WaitForExit() } catch { }
    }
}

$argStr = $ProcArgs -join ' '
$label = $Exe + ' ' + $argStr
$msg = $label + '  =>  ' + $pct + ' % of one core,  ' + $wsMB + ' MB WS,  ' + $privMB + ' MB private'
Write-Output $msg
