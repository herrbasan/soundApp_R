# Apply one DWM/window tweak to a running window, then capture what DWM composites.
# Two capture paths: PrintWindow(PW_RENDERFULLCONTENT) is z-order independent; the screen path
# verifies it actually owns the pixels via WindowFromPoint before trusting them.
param(
  [int]$TargetPid,
  [string]$Mode = "none",
  [string]$Tag = "exp",
  [int]$SettleMs = 900
)
$root = "C:\DEV\GIT\soundApp_R\target"
Add-Type -AssemblyName System.Drawing
Add-Type -Namespace E -Name W -MemberDefinition @"
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
public delegate bool EnumProc(IntPtr hwnd, IntPtr lp);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
[DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
[DllImport("user32.dll")] public static extern bool AdjustWindowRectExForDpi(ref RECT r, uint style, bool menu, uint ex, uint dpi);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
[DllImport("user32.dll")] public static extern int GetWindowLongW(IntPtr h, int idx);
[DllImport("user32.dll", EntryPoint="SetWindowLongPtrW")] public static extern IntPtr SetWindowLongPtrW(IntPtr h, int idx, IntPtr val);
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
[DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
[DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
[DllImport("user32.dll")] public static extern bool InvalidateRect(IntPtr h, IntPtr r, bool erase);
[DllImport("user32.dll")] public static extern bool UpdateWindow(IntPtr h);
[DllImport("dwmapi.dll")] public static extern int DwmSetWindowAttribute(IntPtr h, int attr, ref int val, int size);
[DllImport("dwmapi.dll")] public static extern int DwmExtendFrameIntoClientArea(IntPtr h, ref MARGINS m);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
[StructLayout(LayoutKind.Sequential)] public struct MARGINS { public int L, R, T, B; }
[StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
"@

[void][E.W]::SetThreadDpiAwarenessContext([IntPtr](-4))

$found = @()
$cb = [E.W+EnumProc]{ param($hw, $lp)
  $winPid = 0
  [void][E.W]::GetWindowThreadProcessId($hw, [ref]$winPid)
  if ($winPid -eq $TargetPid -and [E.W]::IsWindowVisible($hw)) { $script:found += $hw }
  return $true
}
[void][E.W]::EnumWindows($cb, [IntPtr]::Zero)
if ($found.Count -eq 0) { Write-Error "no visible window for pid $TargetPid"; exit 2 }

$hwnd = $null; $bestArea = -1
foreach ($h in $found) {
  $r = New-Object E.W+RECT
  [void][E.W]::GetWindowRect($h, [ref]$r)
  $a = ($r.R - $r.L) * ($r.B - $r.T)
  if ($a -gt $bestArea) { $bestArea = $a; $hwnd = $h }
}
$wr = New-Object E.W+RECT
[void][E.W]::GetWindowRect($hwnd, [ref]$wr)
$style = [E.W]::GetWindowLongW($hwnd, -16)
$dpi = [E.W]::GetDpiForWindow($hwnd)
$fr = New-Object E.W+RECT
$fr.L = 0; $fr.T = 0; $fr.R = 1000; $fr.B = 1000
[void][E.W]::AdjustWindowRectExForDpi([ref]$fr, $style, $false, 0, $dpi)
Write-Output ("hwnd=0x{0:X} rect={1},{2} {3}x{4} style=0x{5:X8} dpi={6} nativeFrame=({7},{8})" -f $hwnd.ToInt64(), $wr.L, $wr.T, ($wr.R - $wr.L), ($wr.B - $wr.T), $style, $dpi, (-$fr.L), (-$fr.T))

switch ($Mode) {
  "none" { }
  "framechanged" { [void][E.W]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, 0, 0, 0x0037) }
  "ncrendering" {
    $pol = 1
    $hr = [E.W]::DwmSetWindowAttribute($hwnd, 2, [ref]$pol, 4)
    Write-Output "DwmSetWindowAttribute(NCRENDERING_POLICY=DISABLED) hr=$hr"
    [void][E.W]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, 0, 0, 0x0037)
  }
  "extendframe" {
    $m = New-Object E.W+MARGINS
    $m.L = -1; $m.R = -1; $m.T = -1; $m.B = -1
    $hr = [E.W]::DwmExtendFrameIntoClientArea($hwnd, [ref]$m)
    Write-Output "DwmExtendFrameIntoClientArea(-1,-1,-1,-1) hr=$hr"
    [void][E.W]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, 0, 0, 0x0037)
  }
  "uncloak" {
    $off = 0
    $hr = [E.W]::DwmSetWindowAttribute($hwnd, 13, [ref]$off, 4)
    Write-Output "DWMWA_CLOAK(FALSE) hr=$hr"
  }
  "minrestore" {
    [void][E.W]::ShowWindow($hwnd, 6)
    Start-Sleep -Milliseconds 700
    [void][E.W]::ShowWindow($hwnd, 9)
    [void][E.W]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 500
  }
  "stripblur" {
    $st = [E.W]::GetWindowLongW($hwnd, -16)
    $new = $st -band (-bnot (0x00C00000 -bor 0x00040000 -bor 0x00080000 -bor 0x00020000 -bor 0x00010000))
    [void][E.W]::SetWindowLongPtrW($hwnd, -16, [IntPtr]$new)
    [void][E.W]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, 0, 0, 0x0037)
    Write-Output ("style 0x{0:X8} -> 0x{1:X8}; forcing WM_NCACTIVATE(0)" -f $st, ([E.W]::GetWindowLongW($hwnd, -16)))
    [void][E.W]::SendMessageW($hwnd, 0x0086, [IntPtr]::Zero, [IntPtr]::Zero)
    Start-Sleep -Milliseconds 600
  }
  "stripstyle" {
    # Remove the frame bits the way a plain undecorated window would have them.
    $st = [E.W]::GetWindowLongW($hwnd, -16)
    $new = $st -band (-bnot (0x00C00000 -bor 0x00040000 -bor 0x00080000 -bor 0x00020000 -bor 0x00010000))
    [void][E.W]::SetWindowLongPtrW($hwnd, -16, [IntPtr]$new)
    [void][E.W]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, 0, 0, 0x0037)
    $st2 = [E.W]::GetWindowLongW($hwnd, -16)
    Write-Output ("style 0x{0:X8} -> 0x{1:X8}" -f $st, $st2)
    $fr2 = New-Object E.W+RECT
    $fr2.L = 0; $fr2.T = 0; $fr2.R = 1000; $fr2.B = 1000
    [void][E.W]::AdjustWindowRectExForDpi([ref]$fr2, ([uint32]$st2), $false, 0, $dpi)
    Write-Output ("native frame origin after strip: dx={0} dy={1}" -f (-$fr2.L), (-$fr2.T))
  }
  default { Write-Error "unknown mode $Mode"; exit 2 }
}

Start-Sleep -Milliseconds $SettleMs

[void][E.W]::GetWindowRect($hwnd, [ref]$wr)
$w = $wr.R - $wr.L; $h = $wr.B - $wr.T

# Capture what DWM composites. Never InvalidateRect: a GL window that ignores WM_PAINT falls
# through to DefWindowProc's WM_ERASEBKGND, which paints the theme brush over the client area.
$pt = New-Object E.W+POINT
$pt.X = $wr.L + [int]($w / 2); $pt.Y = $wr.T + [int]($h / 2)
$under = [E.W]::WindowFromPoint($pt)
$top = [E.W]::GetAncestor($under, 2)   # GA_ROOT
$occluded = ($top -ne $hwnd)
Write-Output "target under centre: 0x$($top.ToInt64().ToString('X')) occludedByOther=$occluded"
if ($occluded) {
  # Raise without taking focus: flash to topmost, then back to normal z-order.
  [void][E.W]::SetWindowPos($hwnd, [IntPtr](-1), 0, 0, 0, 0, 0x0013)
  [void][E.W]::SetWindowPos($hwnd, [IntPtr](-2), 0, 0, 0, 0, 0x0013)
  Start-Sleep -Milliseconds 700
  $under = [E.W]::WindowFromPoint($pt)
  $top = [E.W]::GetAncestor($under, 2)
  Write-Output "after raise: 0x$($top.ToInt64().ToString('X')) occludedByOther=$($top -ne $hwnd)"
}
if ($top -eq $hwnd) {
  $bmp = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($wr.L, $wr.T, 0, 0, (New-Object System.Drawing.Size $w, $h))
  $g.Dispose()
  $out = Join-Path $root "$Tag-screen.png"
  $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  Write-Output ("captured {0}x{1} at ({2},{3}) -> {4}" -f $w, $h, $wr.L, $wr.T, $out)
} else {
  Write-Error "window could not be brought to front; capture refused"
  exit 3
}
