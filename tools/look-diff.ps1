# Compares a mockup render with an app capture of the same size, region by region.
#   look-diff.ps1 -Mockup m.png -App a.png [-Diff d.png] [-Side s.png] [-Tolerance 40]
# Writes a difference image (grey = same, red = differs by more than the tolerance, the sum of the three channel differences),
# a side-by-side picture (mockup left, app right) and prints the share of differing pixels per region. Regions are the Canopy
# layout at 1280x800 (scaled to other sizes by width and height); -Regions "name=x,y,w,h;..." replaces them.
param(
    [Parameter(Mandatory = $true)][string]$Mockup,
    [Parameter(Mandatory = $true)][string]$App,
    [string]$Diff = "",
    [string]$Side = "",
    [int]$Tolerance = 40,
    [string]$Regions = "tabs=0,0,1280,58;address=8,60,1264,46;typebar=8,106,1264,26;sidebar=8,142,236,606;toolbar=252,142,1020,48;left pane=252,198,506,550;right pane=766,198,506,550;status=8,756,1264,38"
)
$ErrorActionPreference = "Stop"
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @"
using System; using System.Drawing; using System.Drawing.Imaging; using System.Runtime.InteropServices;
public static class LookDiff {
    static int[] Px(Bitmap b) {
        var d = b.LockBits(new Rectangle(0, 0, b.Width, b.Height), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
        var a = new int[b.Width * b.Height]; Marshal.Copy(d.Scan0, a, 0, a.Length); b.UnlockBits(d); return a;
    }
    // per-pixel mask (1 = differs) and the diff picture
    public static byte[] Mask(Bitmap m, Bitmap a, int tol, Bitmap outp) {
        var pm = Px(m); var pa = Px(a); var mask = new byte[pm.Length]; var o = new int[pm.Length];
        for (int i = 0; i < pm.Length; i++) {
            int x = pm[i], y = pa[i];
            int d = Math.Abs(((x >> 16) & 255) - ((y >> 16) & 255)) + Math.Abs(((x >> 8) & 255) - ((y >> 8) & 255)) + Math.Abs((x & 255) - (y & 255));
            int g = (((x >> 16) & 255) + ((x >> 8) & 255) + (x & 255)) / 6;
            if (d > tol) { mask[i] = 1; o[i] = unchecked((int)0xFF000000) | (Math.Min(255, 128 + d / 2) << 16) | (g / 3 << 8) | (g / 3); }
            else o[i] = unchecked((int)0xFF000000) | (g << 16) | (g << 8) | g;
        }
        if (outp != null) {
            var dd = outp.LockBits(new Rectangle(0, 0, outp.Width, outp.Height), ImageLockMode.WriteOnly, PixelFormat.Format32bppArgb);
            Marshal.Copy(o, 0, dd.Scan0, o.Length); outp.UnlockBits(dd);
        }
        return mask;
    }
    public static double Share(byte[] mask, int w, int h, int rx, int ry, int rw, int rh) {
        long n = 0, on = 0;
        for (int y = Math.Max(0, ry); y < Math.Min(h, ry + rh); y++)
            for (int x = Math.Max(0, rx); x < Math.Min(w, rx + rw); x++) { n++; on += mask[y * w + x]; }
        return n == 0 ? 0 : 100.0 * on / n;
    }
}
"@
Add-Type -AssemblyName System.Drawing
$m = [System.Drawing.Bitmap]::FromFile((Resolve-Path $Mockup))
$a0 = [System.Drawing.Bitmap]::FromFile((Resolve-Path $App))
$w = $m.Width; $h = $m.Height
# the app picture at the mockup's size (a capture at another DPI scale)
$a = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($a); $g.InterpolationMode = "HighQualityBicubic"; $g.DrawImage($a0, 0, 0, $w, $h); $g.Dispose()
$mm = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($mm); $g.DrawImage($m, 0, 0, $w, $h); $g.Dispose()
$d = New-Object System.Drawing.Bitmap($w, $h)
$mask = [LookDiff]::Mask($mm, $a, $Tolerance, $d)
if ($Diff -ne "") { $d.Save($Diff, [System.Drawing.Imaging.ImageFormat]::Png) }
if ($Side -ne "") {
    $s = New-Object System.Drawing.Bitmap(($w * 2 + 16), $h)
    $g = [System.Drawing.Graphics]::FromImage($s); $g.Clear([System.Drawing.Color]::FromArgb(24, 24, 24))
    $g.DrawImage($mm, 0, 0, $w, $h); $g.DrawImage($a, $w + 16, 0, $w, $h); $g.Dispose()
    $s.Save($Side, [System.Drawing.Imaging.ImageFormat]::Png)
}
$sx = $w / 1280.0; $sy = $h / 800.0
$total = 0.0; $n = 0
"{0,-12} {1,7}" -f "region", "differs"
foreach ($r in $Regions.Split(";")) {
    $name, $v = $r.Split("=")
    $q = $v.Split(",") | ForEach-Object { [double]$_ }
    # (regions right of the sidebar stretch with the width; heights with the height)
    $rx = [int]($q[0] * $sx); $ry = [int]($q[1] * $sy); $rw = [int]($q[2] * $sx); $rh = [int]($q[3] * $sy)
    $share = [LookDiff]::Share($mask, $w, $h, $rx, $ry, $rw, $rh)
    "{0,-12} {1,6:N1} %" -f $name, $share
    $total += $share; $n++
}
"{0,-12} {1,6:N1} %" -f "whole", ([LookDiff]::Share($mask, $w, $h, 0, 0, $w, $h))
