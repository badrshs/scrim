<#
.SYNOPSIS
    Click a point inside Scrim's window, in window-relative coordinates.

.DESCRIPTION
    Used to exercise the real interface end to end: the same code path a person
    takes, rather than a test hook wired into the application. Coordinates are
    relative to the window's top-left as it appears on screen, which is what
    you read off a screenshot.

.EXAMPLE
    powershell -File tools/click.ps1 -X 1297 -Y 841
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][int]$X,
    [Parameter(Mandatory = $true)][int]$Y,
    [string]$Process = "scrim",
    [int]$SettleMs = 700
)

$ErrorActionPreference = "Stop"

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Clicker {
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint f, uint dx, uint dy, uint d, IntPtr e);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int s);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public const uint LEFTDOWN = 0x0002, LEFTUP = 0x0004;
    public static List<IntPtr> ForPid(uint want) {
        var res = new List<IntPtr>();
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == want && IsWindowVisible(h)) res.Add(h);
            return true;
        }, IntPtr.Zero);
        return res;
    }
}
"@

$proc = Get-Process -Name $Process -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { throw "$Process is not running" }

# Largest visible window, same rule the screenshot tool uses.
$hwnd = [IntPtr]::Zero
$best = 0
$size = [Runtime.InteropServices.Marshal]::SizeOf([type]"Clicker+RECT")
foreach ($h in [Clicker]::ForPid([uint32]$proc.Id)) {
    $r = New-Object Clicker+RECT
    if ([Clicker]::DwmGetWindowAttribute($h, 9, [ref]$r, $size) -eq 0) {
        $area = ($r.Right - $r.Left) * ($r.Bottom - $r.Top)
        if ($area -gt $best) { $best = $area; $hwnd = $h; $rect = $r }
    }
}
if ($hwnd -eq [IntPtr]::Zero) { throw "no visible window for $Process" }

[void][Clicker]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 250

$sx = $rect.Left + $X
$sy = $rect.Top + $Y
[void][Clicker]::SetCursorPos($sx, $sy)
Start-Sleep -Milliseconds 120
[Clicker]::mouse_event([Clicker]::LEFTDOWN, 0, 0, 0, [IntPtr]::Zero)
Start-Sleep -Milliseconds 60
[Clicker]::mouse_event([Clicker]::LEFTUP, 0, 0, 0, [IntPtr]::Zero)
Start-Sleep -Milliseconds $SettleMs

Write-Host ("clicked window({0},{1}) = screen({2},{3})" -f $X, $Y, $sx, $sy)
