# Captures leaf ON SCREEN over a backdrop picture of our own (so the glass shows and nothing private is behind it).
#   look-capture.ps1 -Wall wall.png -Out app.png [-W 1280] [-H 800] [-X 200] [-Y 120] [-Exe target\fast\leaf.exe] [-Theme dark]
# The backdrop is a borderless, non-activating window showing -Wall at the window's rectangle; leaf starts parked off-screen
# (LEAF_OFFSCREEN, throwaway config, LEAF_MOCKUP_DEMO), is moved over the backdrop without taking the focus, captured with
# CopyFromScreen of its rectangle only, and both windows are closed. On screen for about 3 s.
param(
    [Parameter(Mandatory = $true)][string]$Wall,
    [Parameter(Mandatory = $true)][string]$Out,
    [int]$W = 1280,
    [int]$H = 800,
    [int]$X = 200,
    [int]$Y = 120,
    [string]$Exe = "",
    [int]$SettleMs = 3500,
    [int]$OnScreenMs = 1800,
    [string]$Script = "",
    [string]$Settings = "",
    [int]$Border = 7,
    # the window's own 1 px frame line left, right and below the client area: the window is made that much larger there and placed
    # so that the captured W x H rectangle is the client area alone (0 = the old capture, frame lines included; the top line lies
    # over the client area and stays)
    [int]$Frame = 1
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @"
using System; using System.Drawing; using System.Windows.Forms; using System.Runtime.InteropServices;
public class Backdrop : Form {
    public Backdrop(Image img, int x, int y, int w, int h) {
        FormBorderStyle = FormBorderStyle.None; StartPosition = FormStartPosition.Manual; ShowInTaskbar = false;
        Bounds = new Rectangle(x, y, w, h); BackgroundImage = img; BackgroundImageLayout = ImageLayout.Stretch; TopMost = true;
    }
    protected override bool ShowWithoutActivation { get { return true; } }
    protected override CreateParams CreateParams { get { var p = base.CreateParams; p.ExStyle |= 0x08000000 | 0x80 | 0x8; return p; } }
}
public static class Win {
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint f);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll")] public static extern int GetWindowTextLength(IntPtr h);
    [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr h, uint flags);
    // true when leaf is the top window at every point of a grid over the rectangle (nothing else on top: no other program's window
    // gets into the picture)
    public static bool OnTop(IntPtr leaf, int x, int y, int w, int h) {
        for (int i = 0; i < 9; i++) for (int j = 0; j < 7; j++) {
            var p = new POINT { X = x + 4 + (w - 8) * i / 8, Y = y + 4 + (h - 8) * j / 6 };
            if (GetAncestor(WindowFromPoint(p), 2) != leaf) return false;
        }
        return true;
    }
    public static IntPtr Find(uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid && IsWindowVisible(h) && GetWindowTextLength(h) > 0) { found = h; return false; } return true; }, IntPtr.Zero);
        return found;
    }
}
"@
[Win]::SetProcessDPIAware() | Out-Null
if ($Exe -eq "") { $Exe = [IO.Path]::Combine((Split-Path -Parent $MyInvocation.MyCommand.Path), "..", "target", "fast", "leaf.exe") }
$cfg = Join-Path $env:TEMP ("leaf-look-cfg-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
New-Item -ItemType Directory -Force $cfg | Out-Null
[IO.File]::WriteAllText((Join-Path $cfg "settings.toml"), "preview_open = false`ndecimal_sizes = true`n$Settings")
Set-Content -Encoding utf8 -Path (Join-Path $cfg "settings.toml") -Value "preview_open = false`ndecimal_sizes = true`n$Settings"
$env:LEAF_CONFIG_DIR = $cfg; $env:LEAF_MULTI_INSTANCE = "1"; $env:LEAF_OFFSCREEN = "1"; $env:LEAF_MOCKUP_DEMO = "1"; $env:LEAF_MUTE = "1"
if ($Script -ne "") { $env:LEAF_CHECK_SCRIPT = (Resolve-Path $Script).Path } else { Remove-Item Env:LEAF_CHECK_SCRIPT -ErrorAction SilentlyContinue }
$p = Start-Process (Resolve-Path $Exe).Path -PassThru
$form = $null
try {
    $sw = [Diagnostics.Stopwatch]::StartNew(); $hwnd = [IntPtr]::Zero
    while ($hwnd -eq [IntPtr]::Zero -and $sw.ElapsedMilliseconds -lt 20000) { Start-Sleep -Milliseconds 50; $hwnd = [Win]::Find([uint32]$p.Id) }
    if ($hwnd -eq [IntPtr]::Zero) { throw "no leaf window" }
    # size it while still parked (the layout settles off-screen)
    # (Windows 11 keeps an invisible 7 px resize border left, right and below the visible frame: the frame itself is W x H)
    [Win]::SetWindowPos($hwnd, [IntPtr]::Zero, 0, 0, $W + 2 * $Border + 2 * $Frame, $H + $Border + $Frame, 0x0002 -bor 0x0004 -bor 0x0010) | Out-Null
    Start-Sleep -Milliseconds $SettleMs
    $img = [System.Drawing.Image]::FromFile((Resolve-Path $Wall).Path)
    $form = New-Object Backdrop($img, $X, $Y, $W, $H)
    $form.Show(); [System.Windows.Forms.Application]::DoEvents()
    # leaf over the backdrop, topmost for the capture, never activated
    [Win]::SetWindowPos($hwnd, [IntPtr](-1), $X - $Border - $Frame, $Y, $W + 2 * $Border + 2 * $Frame, $H + $Border + $Frame, 0x0010) | Out-Null
    $t = [Diagnostics.Stopwatch]::StartNew()
    while ($t.ElapsedMilliseconds -lt $OnScreenMs) { [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 30 }
    $tries = 0
    while (-not [Win]::OnTop($hwnd, $X, $Y, $W, $H) -and $tries -lt 10) { Start-Sleep -Milliseconds 200; $tries++ }
    if (-not [Win]::OnTop($hwnd, $X, $Y, $W, $H)) { throw "another window covers leaf: no picture taken" }
    $bmp = New-Object System.Drawing.Bitmap($W, $H)
    $g = [System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($X, $Y, 0, 0, (New-Object System.Drawing.Size($W, $H))); $g.Dispose()
    # (asked again after the copy: a window that came up during it would be in the picture)
    if (-not [Win]::OnTop($hwnd, $X, $Y, $W, $H)) { $bmp.Dispose(); throw "another window came over leaf: picture dropped" }
    $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    # back off the screen before closing
    [Win]::SetWindowPos($hwnd, [IntPtr](-2), 40000, $Y, 0, 0, 0x0001 -bor 0x0010) | Out-Null
    "captured $Out ($($t.ElapsedMilliseconds) ms on screen)"
} finally {
    if ($form) { $form.Close(); $form.Dispose() }
    if (-not $p.HasExited) {
        $h2 = [Win]::Find([uint32]$p.Id); if ($h2 -ne [IntPtr]::Zero) { [Win]::PostMessage($h2, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
        if (-not $p.WaitForExit(5000)) { Stop-Process -Id $p.Id -Force }
    }
    Remove-Item -Recurse -Force $cfg -ErrorAction SilentlyContinue
}
