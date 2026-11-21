<#
.SYNOPSIS
  Linux seam, step A3 (mechanical part 2): the app reaches our GPUI Windows fork's knobs through its toolkit shim.

.DESCRIPTION
  In crates/rf-app/src, outside src/platform/ (the shim itself) and src/bin/ (other crates, where `crate::` would mean the bin),
  every `gpui_windows::` path becomes `crate::platform::gpui::` (`gpui_windows::hybrid::mode()`, `use gpui_windows::dnd_hint;`,
  `gpui_windows::KEEP_ACTIVE_LOOK`, ...). The log target name "gpui_windows=info" is a string, not a path, and is not touched.

  Idempotent and independent of line numbers: safe to re-run on any later version of the tree; a second run prints 0.
  Files keep their encoding and line ends.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\seam\switch-gpui.ps1
  powershell -ExecutionPolicy Bypass -File tools\seam\switch-gpui.ps1 -Check   # count only (exit 1 if anything is left)
#>
param(
    [string]$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [switch]$Check
)
$ErrorActionPreference = 'Stop'

$src = Join-Path $Root 'crates\rf-app\src'
if (-not (Test-Path $src)) { throw "not a leaf tree: $Root" }
$skip = @((Join-Path $src 'platform\'), (Join-Path $src 'bin\'))

$path = New-Object System.Text.RegularExpressions.Regex '(?<![A-Za-z0-9_:])gpui_windows::'
$total = 0
$files = 0
Get-ChildItem $src -Recurse -File -Filter *.rs | Sort-Object FullName | ForEach-Object {
    $full = $_.FullName
    if ($skip | Where-Object { $full.StartsWith($_, [StringComparison]::OrdinalIgnoreCase) }) { return }
    $bytes = [IO.File]::ReadAllBytes($full)
    $bom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
    $start = if ($bom) { 3 } else { 0 }
    $text = (New-Object System.Text.UTF8Encoding $false).GetString($bytes, $start, $bytes.Length - $start)
    $n = $path.Matches($text).Count
    if ($n -gt 0) {
        $total += $n
        $files++
        Write-Host ("{0,5}  {1}" -f $n, $full.Substring($Root.Length + 1))
        if (-not $Check) { [IO.File]::WriteAllText($full, $path.Replace($text, 'crate::platform::gpui::'), (New-Object System.Text.UTF8Encoding $bom)) }
    }
}
$verb = if ($Check) { 'to change' } else { 'changed' }
Write-Host ("switch-gpui: {0} gpui_windows:: site(s) in {1} file(s) {2}" -f $total, $files, $verb)
if ($Check -and $total -gt 0) { exit 1 }
