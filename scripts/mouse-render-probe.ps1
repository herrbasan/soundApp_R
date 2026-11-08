# Diagnoses "the window stops repainting while the mouse moves".
#
# Runs the release exe with --trace (every presented secondary-window frame is
# logged as `frame <key> t=...`), parks the cursor over the target window and
# counts presented frames in phases: still, then escalating mouse-event rates.
#
# Reading it: frames per second must stay at the pacing cadence (~60 with a DSP
# window open) no matter how fast the mouse moves. A collapse towards zero is
# WM_PAINT starvation (RDW_INTERNALPAINT is only synthesised when the message
# queue is empty); a jump to hundreds of fps means every mouse move is forcing a
# full scene build, and on 2026-11-08 that also froze the Monitoring analysis
# (un-paced redraws injected stale synthetic DSP frames - see handover).
#
# ASCII only - Windows PowerShell 5.1.

param(
    [string]$WindowKey = 'monitoring',
    [string]$Exe = "$PSScriptRoot\..\target\release\soundapp_r.exe",
    [int]$PhaseMs = 2000,
    [string]$Rates = '15,5,1,0',
    [switch]$KeepOpen
)

$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;

public class MouseRenderProbe {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, int dx, int dy, uint d, UIntPtr e);

    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }

    public static IntPtr Find(uint pid, string needle) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint p;
            GetWindowThreadProcessId(h, out p);
            if (p != pid || !IsWindowVisible(h)) return true;
            StringBuilder sb = new StringBuilder(512);
            GetWindowTextW(h, sb, 512);
            if (sb.ToString().IndexOf(needle, StringComparison.OrdinalIgnoreCase) >= 0) {
                found = h;
                return false;
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    public static void Wiggle(int ms, int sleepMs) {
        var sw = System.Diagnostics.Stopwatch.StartNew();
        int i = 0;
        while (sw.ElapsedMilliseconds < ms) {
            int d = (i % 2 == 0) ? 6 : -6;
            mouse_event(0x0001, d, 0, 0, UIntPtr.Zero);
            if (sleepMs > 0) System.Threading.Thread.Sleep(sleepMs);
            i++;
        }
    }
}
"@

function Get-FrameStamps([string]$logPath) {
    if (-not (Test-Path $logPath)) { return @() }
    $lines = [System.IO.File]::ReadAllLines($logPath)
    $pattern = '^\[\s*([0-9.]+)ms[^\]]*\]\s+frame ' + [regex]::Escape($WindowKey) + ' '
    $rx = New-Object System.Text.RegularExpressions.Regex $pattern
    $out = New-Object System.Collections.Generic.List[double]
    foreach ($l in $lines) {
        $m = $rx.Match($l)
        if ($m.Success) { $out.Add([double]$m.Groups[1].Value) }
    }
    return $out.ToArray()
}

function Measure-Phase([double[]]$stamps, [int]$from, [int]$to, [string]$name) {
    $n = $to - $from
    $maxGap = 0.0
    for ($i = $from + 1; $i -lt $to; $i++) {
        $g = $stamps[$i] - $stamps[$i - 1]
        if ($g -gt $maxGap) { $maxGap = $g }
    }
    $fps = if ($n -gt 1) { [math]::Round($n / (($stamps[$to - 1] - $stamps[$from]) / 1000.0), 1) } else { 0 }
    [pscustomobject]@{ Phase = $name; Frames = $n; Fps = $fps; MaxGapMs = [math]::Round($maxGap, 1) }
}

# Single-instance pipe: a running player would swallow this launch.
Get-Process soundapp_r -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.Id -Force }
Start-Sleep -Milliseconds 400

$logPath = Join-Path (Split-Path -Parent (Resolve-Path $Exe)) 'event_log.txt'
$proc = Start-Process -FilePath (Resolve-Path $Exe) -ArgumentList @('--open-window', $WindowKey, '--trace') -PassThru

try {
    $hwnd = [IntPtr]::Zero
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Date) -lt $deadline) {
        $hwnd = [MouseRenderProbe]::Find([uint32]$proc.Id, 'Monitoring')
        if ($hwnd -ne [IntPtr]::Zero) { break }
        Start-Sleep -Milliseconds 200
    }
    if ($hwnd -eq [IntPtr]::Zero) { throw "window '$WindowKey' never appeared" }

    $r = New-Object MouseRenderProbe+RECT
    [void][MouseRenderProbe]::GetWindowRect($hwnd, [ref]$r)
    $cx = [int](($r.Left + $r.Right) / 2)
    $cy = [int](($r.Top + $r.Bottom) / 2)

    $saved = New-Object MouseRenderProbe+RECT
    [void][MouseRenderProbe]::GetCursorPos([ref]$saved)

    # Warm up: let playback start and the first frames/pacing settle.
    [void][MouseRenderProbe]::SetCursorPos($cx, $cy)
    Start-Sleep -Milliseconds 3000

    $c0 = (Get-FrameStamps $logPath).Count
    $marks = New-Object System.Collections.Generic.List[int]
    $names = New-Object System.Collections.Generic.List[string]
    $marks.Add($c0)

    Start-Sleep -Milliseconds $PhaseMs
    $marks.Add((Get-FrameStamps $logPath).Count)
    $names.Add('still')

    foreach ($rate in $Rates.Split(',')) {
        $r = [int]$rate
        [MouseRenderProbe]::Wiggle($PhaseMs, $r)
        $marks.Add((Get-FrameStamps $logPath).Count)
        $names.Add($(if ($r -le 0) { 'move@tight' } else { "move@${r}ms" }))
    }

    Start-Sleep -Milliseconds $PhaseMs
    $marks.Add((Get-FrameStamps $logPath).Count)
    $names.Add('still')

    [void][MouseRenderProbe]::SetCursorPos($saved.Left, $saved.Top)

    $s = Get-FrameStamps $logPath
    $rows = @()
    for ($i = 0; $i -lt $names.Count; $i++) {
        $rows += Measure-Phase $s $marks[$i] $marks[$i + 1] $names[$i]
    }
    $rows | Format-Table -AutoSize
} finally {
    if (-not $KeepOpen) {
        if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
    }
}
