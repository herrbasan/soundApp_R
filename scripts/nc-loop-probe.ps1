# Verify that the native non-client drag/size loop still engages after the frame styles
# are stripped. winit's drag_resize_window posts WM_NCLBUTTONDOWN with an HT* code; the loop
# setting mouse capture is the observable proof that Windows accepted it.
param([int]$TargetPid)
Add-Type -Namespace N -Name W -MemberDefinition @"
[DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr lp);
public delegate bool EnumProc(IntPtr hwnd, IntPtr lp);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr ctx);
[DllImport("user32.dll")] public static extern IntPtr GetCapture();
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
[DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr wp, IntPtr lp);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
"@
[void][N.W]::SetThreadDpiAwarenessContext([IntPtr](-4))
$found = @()
$cb = [N.W+EnumProc]{ param($hw, $lp)
  $winPid = 0
  [void][N.W]::GetWindowThreadProcessId($hw, [ref]$winPid)
  if ($winPid -eq $TargetPid -and [N.W]::IsWindowVisible($hw)) { $script:found += $hw }
  return $true
}
[void][N.W]::EnumWindows($cb, [IntPtr]::Zero)
if ($found.Count -eq 0) { Write-Error "no visible window for pid $TargetPid"; exit 2 }
$hwnd = $null; $best = -1
foreach ($h in $found) {
  $r = New-Object N.W+RECT
  [void][N.W]::GetWindowRect($h, [ref]$r)
  $a = ($r.R - $r.L) * ($r.B - $r.T)
  if ($a -gt $best) { $best = $a; $hwnd = $h }
}
$rect = New-Object N.W+RECT
[void][N.W]::GetWindowRect($hwnd, [ref]$rect)

# HTBOTTOMRIGHT = 17, HTCAPTION = 2
foreach ($case in @(@{ n = 'HTCAPTION (drag)'; ht = 2 }, @{ n = 'HTBOTTOMRIGHT (resize)'; ht = 17 })) {
  $x = $rect.L + 40; $y = $rect.T + 20
  if ($case.ht -eq 17) { $x = $rect.R - 4; $y = $rect.B - 4 }
  $lp = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
  [void][N.W]::PostMessageW($hwnd, 0x00A1, [IntPtr]$case.ht, $lp)
  Start-Sleep -Milliseconds 250
  $cap = [N.W]::GetCapture()
  $engaged = ($cap -eq $hwnd)
  # Abort whichever modal loop may have started; harmless if none did.
  [void][N.W]::PostMessageW($hwnd, 0x001F, [IntPtr]::Zero, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 250
  Write-Output ("{0,-26} capture={1} engaged={2}" -f $case.n, $cap.ToInt64(), $engaged)
}
