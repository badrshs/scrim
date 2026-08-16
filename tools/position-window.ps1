<#
.SYNOPSIS
    Move Scrim's window to a known position and size.

.DESCRIPTION
    Screenshot and click coordinates are only meaningful if the window is
    fully on screen and in a predictable place. Without this, a window that
    opens partly under the taskbar makes every scripted click land somewhere
    other than intended, which looks exactly like a broken interface.
#>
[CmdletBinding()]
param(
    [int]$X = 0,
    [int]$Y = 0,
    [int]$Width = 1440,
    [int]$Height = 880,
    [string]$Process = "scrim"
)

$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class Pos {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int max);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool repaint);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    public static IntPtr FindClass(uint pid, string cls) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p != pid || !IsWindowVisible(h)) return true;
            var sb = new StringBuilder(256); GetClassNameW(h, sb, 256);
            if (sb.ToString() == cls) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
"@

$proc = Get-Process -Name $Process -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { throw "$Process is not running" }

$hwnd = [Pos]::FindClass([uint32]$proc.Id, "Tauri Window")
if ($hwnd -eq [IntPtr]::Zero) { throw "could not find the interface window" }

[void][Pos]::MoveWindow($hwnd, $X, $Y, $Width, $Height, $true)
[void][Pos]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 500
Write-Host ("window placed at {0},{1} {2}x{3}" -f $X, $Y, $Width, $Height)
