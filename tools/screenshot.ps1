<#
.SYNOPSIS
    Capture a window to a PNG.

.DESCRIPTION
    Used to check that the picture and the HTML interface are compositing into
    one window. It grabs from the screen rather than asking the window to paint
    itself, because PrintWindow returns black for GPU-rendered video, which is
    exactly the content being verified.

.EXAMPLE
    powershell -File tools/screenshot.ps1 -Title Scrim -Out shot.png
#>
[CmdletBinding()]
param(
    [string]$Title = "Scrim",
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$DelaySeconds = 0
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Win32Cap {
    [DllImport("user32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    public static extern IntPtr FindWindowW(string cls, string title);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("dwmapi.dll")]
    public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

if ($DelaySeconds -gt 0) { Start-Sleep -Seconds $DelaySeconds }

# Pick the largest visible top-level window belonging to the process.
#
# MainWindowHandle is not usable here: the process owns several top-level
# windows and Windows hands back "Tao Thread Event Target", a 16x16 message
# sink, which produces a 16x16 screenshot of nothing.
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class WinList {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
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
"@ -ErrorAction SilentlyContinue

$hwnd = [IntPtr]::Zero
$best = 0
$proc = Get-Process -Name $Title -ErrorAction SilentlyContinue | Select-Object -First 1
if ($proc) {
    foreach ($h in [WinList]::ForPid([uint32]$proc.Id)) {
        $r = New-Object Win32Cap+RECT
        $sz = [Runtime.InteropServices.Marshal]::SizeOf([type]"Win32Cap+RECT")
        if ([Win32Cap]::DwmGetWindowAttribute($h, 9, [ref]$r, $sz) -eq 0) {
            $area = ($r.Right - $r.Left) * ($r.Bottom - $r.Top)
            if ($area -gt $best) { $best = $area; $hwnd = $h }
        }
    }
}
if ($hwnd -eq [IntPtr]::Zero) {
    $hwnd = [Win32Cap]::FindWindowW($null, $Title)
}
if ($hwnd -eq [IntPtr]::Zero) {
    throw "no window found for '$Title' (no such process, and no window with that title)"
}

[void][Win32Cap]::ShowWindow($hwnd, 9)   # SW_RESTORE
[void][Win32Cap]::SetForegroundWindow($hwnd)
Start-Sleep -Milliseconds 700            # let it come forward and repaint

# DWM's extended frame bounds exclude the invisible resize border, so the
# capture matches what the eye sees rather than including transparent margins.
$r = New-Object Win32Cap+RECT
$DWMWA_EXTENDED_FRAME_BOUNDS = 9
$size = [Runtime.InteropServices.Marshal]::SizeOf([type]"Win32Cap+RECT")
if ([Win32Cap]::DwmGetWindowAttribute($hwnd, $DWMWA_EXTENDED_FRAME_BOUNDS, [ref]$r, $size) -ne 0) {
    throw "DwmGetWindowAttribute failed"
}

$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
if ($w -le 0 -or $h -le 0) { throw "window has no area ($w x $h)" }

$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
$g.Dispose()

$dir = Split-Path -Parent $Out
if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

Write-Host ("captured {0}x{1} -> {2}" -f $w, $h, $Out)
