# Deterministic toggle probe for soundApp_r.
#
# Posts real WM_KEYDOWN/WM_KEYUP messages to the app's main HWND instead of
# using SendKeys: no focus stealing, and nothing is typed into whatever window
# happens to be foreground. Exercises the N/M window toggles and copies
# event_log.txt to target/probe_log.txt with per-run counts.
#
#   .\scripts\toggle-probe.ps1              # 3 rounds of M then N
#   .\scripts\toggle-probe.ps1 -Rounds 6
#
# Synthetic presses verify the plumbing only — focus loss, Alt-Tab, the tray and
# real keyboard delivery stay field-test territory (see
# docs/window-toggle-diagnostics.md).
param([int]$Rounds = 3)

$ErrorActionPreference = 'Continue'

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class Win {
    public delegate bool EnumProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();

    public static List<IntPtr> WindowsOf(uint pid) {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == pid) list.Add(h);
            return true;
        }, IntPtr.Zero);
        return list;
    }
    public static string Title(IntPtr h) {
        var sb = new StringBuilder(256);
        GetWindowTextW(h, sb, 256);
        return sb.ToString();
    }
    public static void Key(IntPtr h, int vk, int scan, bool down) {
        long lp = 1L | ((long)scan << 16);
        if (!down) lp |= 0xC0000000L;
        PostMessageW(h, down ? 0x0100u : 0x0101u, (IntPtr)vk, (IntPtr)lp);
    }
}
'@

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\soundapp_r.exe'
$log = Join-Path $root 'target\release\event_log.txt'
$out = Join-Path $root 'target\probe_log.txt'

$p = Start-Process -FilePath $exe -ArgumentList '--silence', '--static' -PassThru
Start-Sleep -Milliseconds 2500

$wins = [Win]::WindowsOf([uint32]$p.Id)
foreach ($h in $wins) {
    Write-Output ("win 0x{0:x} vis={1} title='{2}'" -f [int64]$h, [Win]::IsWindowVisible($h), [Win]::Title($h))
}
$main = $wins | Where-Object { [Win]::Title($_) -eq 'SoundApp' } | Select-Object -First 1
if (-not $main) {
    Write-Output 'MAIN WINDOW NOT FOUND'
    Stop-Process -Id $p.Id -Force
    exit 1
}

foreach ($round in 1..$Rounds) {
    [Win]::Key($main, 0x4D, 0x32, $true); Start-Sleep -Milliseconds 60
    [Win]::Key($main, 0x4D, 0x32, $false); Start-Sleep -Milliseconds 700
    [Win]::Key($main, 0x4E, 0x31, $true); Start-Sleep -Milliseconds 60
    [Win]::Key($main, 0x4E, 0x31, $false); Start-Sleep -Milliseconds 700
}
Start-Sleep -Milliseconds 500
Stop-Process -Id $p.Id -Force
Copy-Item $log $out -Force

$c = Get-Content $out
$count = { param($pat) ($c | Select-String -Pattern $pat | Measure-Object).Count }
Write-Output ("toggles={0} reveals={1} retries={2} stalls={3} dup={4} bounce={5}" -f `
        (& $count 'toggle \w+ src'), (& $count 'reveal DONE'), (& $count 'RETRY'), `
        (& $count 'STALL'), (& $count 'DUP-PRESS'), (& $count 'BOUNCE'))
Write-Output "log: $out"
