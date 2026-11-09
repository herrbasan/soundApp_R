# Volume-persistence probe: launches the app on a silent source, sends real
# ArrowDown volume keys, HARD-KILLS the process, then checks that the reduced
# master_vol reached user.json.
#
#   .\scripts\volume-persist-probe.ps1

$ErrorActionPreference = 'Stop'
$cfgPath = 'D:\Work\_GIT\soundApp_R\target\release\user.json'
$cfg = Get-Content $cfgPath -Raw | ConvertFrom-Json
$before = [double]$cfg.master_vol
Write-Output "before: master_vol=$before"

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Win32 {
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    public static IntPtr FindByPid(uint target) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid == target) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@

$p = Start-Process -FilePath 'D:\Work\_GIT\soundApp_R\target\release\soundapp_r.exe' `
    -ArgumentList '--silence','--seconds','30' -PassThru
Start-Sleep -Milliseconds 2500

# VK_DOWN=0x28, scancode 0x48. Posted key messages need no keyboard focus.
$hwnd = [Win32]::FindByPid([uint32]$p.Id)
if ($hwnd -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window found for pid'; $p.Kill(); exit 1 }
$WM_KEYDOWN = 0x0100; $WM_KEYUP = 0x0101
$lpDown = [IntPtr]0x480001; $lpUp = [IntPtr][uint32]'0xC0480001'
foreach ($i in 1..4) {
    [void][Win32]::PostMessage($hwnd, $WM_KEYDOWN, [IntPtr]0x28, $lpDown)
    Start-Sleep -Milliseconds 120
    [void][Win32]::PostMessage($hwnd, $WM_KEYUP, [IntPtr]0x28, $lpUp)
    Start-Sleep -Milliseconds 180
}
Start-Sleep -Milliseconds 800

# Hard kill — no graceful exit, no chance to save on quit.
if (-not $p.HasExited) { $p.Kill(); $p.WaitForExit() }

$cfg2 = Get-Content $cfgPath -Raw | ConvertFrom-Json
$after = [double]$cfg2.master_vol
Write-Output "after hard kill: master_vol=$after"
$expected = [math]::Round($before - 0.20, 4)
if ([math]::Abs($after - $expected) -lt 0.001) {
    Write-Output "PASS: volume persisted across hard kill ($before -> $after)"
} else {
    Write-Output "FAIL: expected ~$expected"
}
