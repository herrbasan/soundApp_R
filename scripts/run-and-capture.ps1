# Launch a nui binary, capture the DWM-composited window, and keep its own framebuffer screenshot.
# Compares the two: framebuffer = what the app drew, composed = what the screen actually shows.
param(
  [string]$Exe = "C:\DEV\GIT\soundApp_R\target\release\nui_demo.exe",
  [string[]]$ExeArgs = @("--seconds","25","--screenshot","C:\DEV\GIT\soundApp_R\target\fb.png"),
  [string]$Tag = "run",
  [int]$CaptureDelaySec = 5,
  [int]$PostCaptureSec = 1
)
$root = "C:\DEV\GIT\soundApp_R\target"
$composed = Join-Path $root "$Tag-composed.png"
$log = Join-Path $root "$Tag-out.txt"

$p = Start-Process -FilePath $Exe -ArgumentList $ExeArgs -PassThru -RedirectStandardOutput $log -RedirectStandardError "$root\$Tag-err.txt"
Start-Sleep -Seconds $CaptureDelaySec
if (-not $p.HasExited) {
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root "shot.ps1") -TargetPid $p.Id -Out $composed
  & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root "probe-window.ps1") -TargetPid $p.Id
} else {
  Write-Output "PROCESS EXITED EARLY code=$($p.ExitCode)"
  if (Test-Path "$root\$Tag-err.txt") { Get-Content "$root\$Tag-err.txt" -Tail 20 }
}
Start-Sleep -Seconds $PostCaptureSec
if (-not $p.HasExited) { Stop-Process -Id $p.Id -ErrorAction SilentlyContinue }
Write-Output "captured: $composed"
