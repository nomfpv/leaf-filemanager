# Runs one scripted check of the real app, off-screen (tools\TESTING.md): throwaway config, LEAF_OFFSCREEN, a LEAF_CHECK_SCRIPT,
# and a picture for every `shot` line (taken with capture-window.ps1). Nothing appears on screen, nothing makes a sound.
#   win-flows.ps1 -Script flow.txt -Base <demo root> [-Settings settings.toml] [-Actions actions.toml] [-Shots <dir>]
#                 [-Open <folder>] [-Seconds 90] [-Exe <leaf.exe>]
# In the script (and in -Open) $BASE is replaced by -Base. -Settings / -Actions are copied into the throwaway config folder
# (settings.toml / actions.toml; the same replacement). Exit 0 = the app ended by itself (`quit`), 1 = it had to be stopped.
param(
    [Parameter(Mandatory = $true)][string]$Script,
    [Parameter(Mandatory = $true)][string]$Base,
    [string]$Settings = "",
    [string]$Actions = "",
    [string]$Shots = "",
    [string]$Open = "",
    [int]$Seconds = 90,
    [string]$Exe = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
if ($Exe -eq "") { $Exe = Join-Path $root "target\release\leaf.exe" }
if ($Shots -eq "") { $Shots = Join-Path $env:TEMP "leaf-flow-shots" }
if ($Open -eq "") { $Open = $Base }
New-Item -ItemType Directory -Force $Shots | Out-Null
$work = Join-Path $env:TEMP ("leaf-flow-" + [guid]::NewGuid().ToString("N").Substring(0, 8))
$cfg = Join-Path $work "cfg"
New-Item -ItemType Directory -Force $cfg | Out-Null
function Fill($src, $dst) { [IO.File]::WriteAllText($dst, (Get-Content -Raw $src).Replace('$BASE', $Base), (New-Object Text.UTF8Encoding($false))) }
Fill $Script (Join-Path $work "script.txt")
if ($Settings -ne "") { Fill $Settings (Join-Path $cfg "settings.toml") }
if ($Actions -ne "") { Fill $Actions (Join-Path $cfg "actions.toml") }

$env:LEAF_CONFIG_DIR = $cfg
$env:LEAF_MULTI_INSTANCE = "1"
$env:LEAF_OFFSCREEN = "1"
$env:LEAF_MUTE = "1"
$env:LEAF_CHECK_SCRIPT = Join-Path $work "script.txt"
$live = Join-Path $work "shots"; New-Item -ItemType Directory -Force $live | Out-Null
$env:LEAF_CHECK_SHOTDIR = $live
$env:RUST_LOG = "info"
$out = Join-Path $work "out.txt"; $err = Join-Path $work "err.txt"
$p = Start-Process $Exe -ArgumentList ('"' + $Open.Replace('$BASE', $Base) + '"') -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
$seen = @{}
$t0 = Get-Date
while (-not $p.HasExited -and ((Get-Date) - $t0).TotalSeconds -lt $Seconds) {
    Start-Sleep -Milliseconds 250
    if (Test-Path $out) {
        foreach ($l in (Get-Content $out -ErrorAction SilentlyContinue)) {
            if ($l -match '^LEAF_SHOT (\S+)' -and -not $seen.ContainsKey($Matches[1])) {
                $n = $Matches[1]; $seen[$n] = 1
                $tmp = Join-Path $work "$n.png"
                & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "capture-window.ps1") -ProcessId $p.Id -Out $tmp | Out-Null
                if (Test-Path $tmp) { Move-Item -Force $tmp (Join-Path $live "$n.png"); Copy-Item -Force (Join-Path $live "$n.png") (Join-Path $Shots "$n.png") }
            }
        }
    }
}
$code = 0
if (-not $p.HasExited) { Write-Host "win-flows: stopped after $Seconds s"; Stop-Process -Id $p.Id -Force; $code = 1 }
Start-Sleep -Milliseconds 300
Copy-Item $err (Join-Path $Shots "last-run.log") -Force -ErrorAction SilentlyContinue
Write-Host "win-flows: log $err ; work $work"
exit $code
