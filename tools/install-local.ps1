# A plain local install without Velopack: copies dist\stage (made by tools\package.ps1) to %LOCALAPPDATA%\Programs\leaf and makes a
# Start menu shortcut "leaf". The dev copy in target\ is never touched. Updates do not apply to this kind of install (it is not
# a Velopack install): run this script again with a newer stage.
#   tools\install-local.ps1 [-Uninstall] [-Force]
param(
    [switch]$Uninstall,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$stage = Join-Path $root 'dist\stage'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\leaf'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\leaf.lnk'

function Same([string]$a, [string]$b) {
    [string]::Equals([IO.Path]::GetFullPath($a).TrimEnd('\'), [IO.Path]::GetFullPath($b).TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
}
function Inside([string]$p, [string]$dir) {
    $p = [IO.Path]::GetFullPath($p); $dir = [IO.Path]::GetFullPath($dir).TrimEnd('\') + '\'
    $p.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase)
}

# refuse to run from inside the folder this script replaces
if ((Inside $root $dest) -or (Inside $PSScriptRoot $dest) -or (Inside (Get-Location).Path $dest)) {
    throw "run this from the repository, not from inside $dest"
}

# stop only a leaf that runs FROM the install folder (never the dev copy in target\)
function Stop-Installed {
    foreach ($name in 'leaf', 'leaf-look', 'leaf-open', 'leaf-nmhost', 'leaf-mcp') {
        Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
            $p = $null
            try { $p = $_.MainModule.FileName } catch { }
            if ($p -and (Inside $p $dest)) {
                Write-Host "stopping $name (pid $($_.Id)) from $dest"
                Stop-Process -Id $_.Id -Force
                $_.WaitForExit(5000) | Out-Null
            }
        }
    }
}

if ($Uninstall) {
    # The default-file-manager switch registers a launcher that starts the leaf it was turned on for: turn it off with the installed
    # copy (`leaf default-fm off`) before that copy goes, so Windows is not left with a program that is gone.
    $reg = Join-Path $env:LOCALAPPDATA 'leaf-default-fm\leaf-exe.txt'
    if (Test-Path $reg) {
        $target = (Get-Content $reg -Raw).Trim()
        if ($target -and (Inside $target $dest)) {
            $installed = Join-Path $dest 'leaf.exe'
            if (Test-Path $installed) {
                & $installed default-fm off
                if ($LASTEXITCODE -ne 0 -and -not $Force) {
                    Write-Host 'could not turn off "Default file manager" (-Force removes the files anyway).'
                    exit 1
                }
            } elseif (-not $Force) {
                Write-Host "leaf is still the default file manager through $target, which is not there to turn it off (-Force removes the files anyway)."
                exit 1
            }
        }
    }
    Stop-Installed
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest; Write-Host "removed $dest" } else { Write-Host "$dest was not there" }
    if (Test-Path $lnk) { Remove-Item -Force $lnk; Write-Host "removed $lnk" }
    Write-Host 'settings in %APPDATA% were kept.'
    exit 0
}

if (-not (Test-Path (Join-Path $stage 'leaf.exe'))) {
    throw "nothing staged: run tools\package.ps1 first (it makes $stage)"
}
Stop-Installed
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $stage '*') $dest -Recurse -Force
Write-Host "copied $((Get-ChildItem $stage).Count) files to $dest"

$sh = New-Object -ComObject WScript.Shell
$s = $sh.CreateShortcut($lnk)
$s.TargetPath = Join-Path $dest 'leaf.exe'
$s.WorkingDirectory = $env:USERPROFILE
$s.Description = 'leaf file manager'
$s.Save()
Write-Host "Start menu shortcut: $lnk"
Write-Host 'done. If leaf is the default file manager for the old copy, turn the switch off and on again in Settings so it points at the new one.'
