# Capture what DWM actually composites: full desktop, or a window rect by PID.
param(
  [int]$TargetPid = 0,
  [string]$Out = "C:\DEV\GIT\soundApp_R\target\desktop.png",
  [switch]$FullDesktop
)
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type -Namespace S -Name W -MemberDefinition @"
[DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
[DllImport("user32.dll")] public static extern int GetSystemMetrics(int i);
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
public delegate bool EnumProc(IntPtr hwnd, IntPtr lp);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
"@

$x = 0; $y = 0; $w = 0; $h = 0
# PMv2 (-4) so CopyFromScreen returns physical pixels, matching the app's own framebuffer screenshots.
[void][S.W]::SetThreadDpiAwarenessContext([IntPtr](-4))
if (-not $FullDesktop -and $TargetPid -ne 0) {
  $found = @()
  $cb = [S.W+EnumProc]{ param($hw, $lp)
    $winPid = 0
    [void][S.W]::GetWindowThreadProcessId($hw, [ref]$winPid)
    if ($winPid -eq $TargetPid -and [S.W]::IsWindowVisible($hw)) { $script:found += $hw }
    return $true
  }
  [void][S.W]::EnumWindows($cb, [IntPtr]::Zero)
  if ($found.Count -eq 0) { Write-Error "no visible top-level window for pid $TargetPid"; exit 2 }
  $best = $null; $bestArea = -1
  foreach ($hw in $found) {
    $r = New-Object S.W+RECT
    [void][S.W]::GetWindowRect($hw, [ref]$r)
    $a = ($r.R - $r.L) * ($r.B - $r.T)
    if ($a -gt $bestArea) { $bestArea = $a; $best = $r }
  }
  $x = $best.L; $y = $best.T; $w = $best.R - $best.L; $h = $best.B - $best.T
} else {
  $w = [S.W]::GetSystemMetrics(0)
  $h = [S.W]::GetSystemMetrics(1)
}
if ($w -le 0 -or $h -le 0) { Write-Error "bad capture geometry ${w}x${h}"; exit 3 }

$bmp = New-Object System.Drawing.Bitmap $w, $h
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($x, $y, 0, 0, (New-Object System.Drawing.Size $w, $h))
$g.Dispose()
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "captured ${w}x${h} at ($x,$y) -> $Out"
