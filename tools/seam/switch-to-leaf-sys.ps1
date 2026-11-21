<#
.SYNOPSIS
  Linux seam, step A3 (mechanical part): the app reaches the operating system through leaf-sys, not rf-win.

.DESCRIPTION
  In crates/rf-app (every .rs file: src, tests, benches, examples) the identifier `rf_win` becomes `leaf_sys`
  (`rf_win::x` paths, `use rf_win;`, `rf_win as`, `extern crate rf_win`, and the paths inside comments and test strings).
  The manifests: the workspace gets `leaf-sys` in [workspace.dependencies], and rf-app depends on `leaf-sys` instead of `rf-win`.

  Idempotent and independent of line numbers: run it on any later version of the tree (for example after merging another
  branch that still writes `rf_win::`) and it changes only what is still to change. A second run prints 0.
  Files keep their encoding (a UTF-8 BOM stays or stays away) and their line ends.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\seam\switch-to-leaf-sys.ps1
  powershell -ExecutionPolicy Bypass -File tools\seam\switch-to-leaf-sys.ps1 -Check   # count only, change nothing (exit 1 if anything is left)
#>
param(
    [string]$Root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [switch]$Check
)
$ErrorActionPreference = 'Stop'

function Read-Text([string]$path) {
    $bytes = [IO.File]::ReadAllBytes($path)
    $bom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF
    $start = if ($bom) { 3 } else { 0 }
    $text = (New-Object System.Text.UTF8Encoding $false).GetString($bytes, $start, $bytes.Length - $start)
    return @{ Text = $text; Bom = $bom }
}

function Write-Text([string]$path, [string]$text, [bool]$bom) {
    $enc = New-Object System.Text.UTF8Encoding $bom
    [IO.File]::WriteAllText($path, $text, $enc)
}

$app = Join-Path $Root 'crates\rf-app'
if (-not (Test-Path (Join-Path $app 'Cargo.toml'))) { throw "not a leaf tree: $Root" }

# 1. sources
$ident = New-Object System.Text.RegularExpressions.Regex '(?<![A-Za-z0-9_])rf_win(?![A-Za-z0-9_])'
$total = 0
$files = 0
Get-ChildItem $app -Recurse -File -Filter *.rs | Where-Object { $_.FullName -notmatch '\\target\\' } | Sort-Object FullName | ForEach-Object {
    $f = Read-Text $_.FullName
    $n = $ident.Matches($f.Text).Count
    if ($n -gt 0) {
        $total += $n
        $files++
        Write-Host ("{0,5}  {1}" -f $n, $_.FullName.Substring($Root.Length + 1))
        if (-not $Check) { Write-Text $_.FullName $ident.Replace($f.Text, 'leaf_sys') $f.Bom }
    }
}

# 2. manifests
$manifest = 0
$ws = Join-Path $Root 'Cargo.toml'
$w = Read-Text $ws
if ($w.Text -notmatch '(?m)^leaf-sys\s*=') {
    $nl = if ($w.Text.Contains("`r`n")) { "`r`n" } else { "`n" }
    $line = 'leaf-sys = { path = "crates/leaf-sys" }'
    $m = [regex]::Match($w.Text, '(?m)^leaf-unix\s*=.*$')
    if (-not $m.Success) { $m = [regex]::Match($w.Text, '(?m)^rf-win\s*=.*$') }
    if (-not $m.Success) { throw 'workspace Cargo.toml: neither a leaf-unix nor an rf-win dependency line to put leaf-sys after' }
    $at = $m.Index + $m.Length
    $manifest++
    Write-Host "    1  Cargo.toml (workspace dependency leaf-sys)"
    if (-not $Check) { Write-Text $ws ($w.Text.Substring(0, $at) + $nl + $line + $w.Text.Substring($at)) $w.Bom }
}
$am = Join-Path $app 'Cargo.toml'
$a = Read-Text $am
$rfwin = New-Object System.Text.RegularExpressions.Regex '(?m)^rf-win(\.workspace[ \t]*=[ \t]*true|[ \t]*=[^\r\n]*)'
if ($rfwin.IsMatch($a.Text)) {
    $manifest++
    if ($a.Text -match '(?m)^leaf-sys(\.workspace|\s*=)') {
        Write-Host "    1  crates\rf-app\Cargo.toml (rf-win dependency dropped, leaf-sys is there)"
        $new = [regex]::Replace($a.Text, '(?m)^rf-win(\.workspace[ \t]*=[ \t]*true|[ \t]*=[^\r\n]*)\r?\n', '')
    } else {
        Write-Host "    1  crates\rf-app\Cargo.toml (rf-win dependency -> leaf-sys)"
        $new = $rfwin.Replace($a.Text, 'leaf-sys.workspace = true', 1)
    }
    if (-not $Check) { Write-Text $am $new $a.Bom }
}

$verb = if ($Check) { 'to change' } else { 'changed' }
Write-Host ("switch-to-leaf-sys: {0} rf_win site(s) in {1} file(s) {2}, {3} manifest edit(s)" -f $total, $files, $verb, $manifest)
if ($Check -and ($total + $manifest) -gt 0) { exit 1 }
