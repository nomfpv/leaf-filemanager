# Captures a window's client area to a PNG, also when the window is off-screen (LEAF_OFFSCREEN=1).
# PrintWindow with PW_RENDERFULLCONTENT (falls back to PW_CLIENTONLY). CopyFromScreen cannot see
# off-screen windows, so it is not used.
#   capture-window.ps1 -ProcessId <pid> -Out <png>
#   capture-window.ps1 -Title <part of the title> -Out <png>
# Exit 0: PNG written. Exit 1: no window / capture failed. Exit 2: the picture is one colour (blank).
param(
    [int]$ProcessId = 0,
    [string]$Title = "",
    [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = "Stop"
if ($ProcessId -eq 0 -and $Title -eq "") { Write-Host "capture-window: give -ProcessId or -Title"; exit 1 }

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class CapWin {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [StructLayout(LayoutKind.Sequential)] public struct PT { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref PT p);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    public static List<IntPtr> Find(uint pid, string title) {
        var l = new List<IntPtr>();
        EnumWindows((h, _) => {
            if (!IsWindowVisible(h)) return true;
            uint p; GetWindowThreadProcessId(h, out p);
            var sb = new StringBuilder(512); GetWindowText(h, sb, 512);
            if (sb.Length == 0) return true;
            if (pid != 0 && p != pid) return true;
            if (title != "" && sb.ToString().IndexOf(title, StringComparison.OrdinalIgnoreCase) < 0) return true;
            l.Add(h); return true;
        }, IntPtr.Zero);
        return l;
    }
}
"@
Add-Type -AssemblyName System.Drawing
[void][CapWin]::SetProcessDPIAware()

$wins = [CapWin]::Find([uint32]$ProcessId, $Title)
if ($wins.Count -eq 0) { Write-Host "capture-window: no visible titled window found"; exit 1 }
$h = $wins[0]
$cr = New-Object CapWin+RECT
[void][CapWin]::GetClientRect($h, [ref]$cr)
$w = $cr.Right - $cr.Left; $ht = $cr.Bottom - $cr.Top
if ($w -le 0 -or $ht -le 0) { Write-Host "capture-window: the window has no client area (minimised?)"; exit 1 }

# true when the bitmap has more than one colour
function Test-NotBlank([System.Drawing.Bitmap]$bmp) {
    $first = $bmp.GetPixel(0, 0).ToArgb()
    $step = 7
    for ($y = 0; $y -lt $bmp.Height; $y += $step) {
        for ($x = 0; $x -lt $bmp.Width; $x += $step) {
            if ($bmp.GetPixel($x, $y).ToArgb() -ne $first) { return $true }
        }
    }
    return $false
}

# PW_RENDERFULLCONTENT = 2, PW_CLIENTONLY = 1; PrintWindow draws the whole window (frame included),
# so for the client area the full-window bitmap is cropped by the client offset.
$wr = New-Object CapWin+RECT
[void][CapWin]::GetWindowRect($h, [ref]$wr)
$fullW = $wr.Right - $wr.Left; $fullH = $wr.Bottom - $wr.Top
$result = $null
foreach ($flags in 2, 3, 1) {
    $bmp = New-Object System.Drawing.Bitmap($fullW, $fullH)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    $ok = [CapWin]::PrintWindow($h, $dc, [uint32]$flags)
    $g.ReleaseHdc($dc); $g.Dispose()
    if ($ok -and (Test-NotBlank $bmp)) { $result = $bmp; $used = $flags; break }
    $bmp.Dispose()
}
if ($null -eq $result) {
    Write-Host "capture-window: BLANK picture (one colour or PrintWindow failed) for window $h"
    exit 2
}
# crop to the client area (PW_CLIENTONLY already returns just that)
if ($used -band 1) {
    $crop = $result
} else {
    $pt = New-Object CapWin+PT
    [void][CapWin]::ClientToScreen($h, [ref]$pt)
    $left = [Math]::Max(0, $pt.X - $wr.Left); $top = [Math]::Max(0, $pt.Y - $wr.Top)
    $crop = $result.Clone((New-Object System.Drawing.Rectangle($left, $top, [Math]::Min($w, $fullW - $left), [Math]::Min($ht, $fullH - $top))), $result.PixelFormat)
}
$dir = Split-Path -Parent $Out
if ($dir -ne "" -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
$crop.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Host ("capture-window: wrote {0} ({1}x{2}, PrintWindow flags {3})" -f $Out, $crop.Width, $crop.Height, $used)
exit 0
