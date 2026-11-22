# Removes the all-users install of leaf made by tools\install-local.ps1 -AllUsers: %ProgramFiles%\leaf, the all-users Start menu
# shortcut and the entry in Windows' Apps list. It is also copied into the install as uninstall.ps1 (that is what the Apps list runs).
# Asks for administrator permission once (the normal UAC prompt) when it is not elevated. Settings, journals and caches in the
# user folders stay.
#   tools\uninstall-all-users.ps1 [-Force] [-DryRun]
param(
    [switch]$Force,
    [switch]$DryRun,
    [switch]$FromTemp
)
$ErrorActionPreference = 'Stop'
$dest = Join-Path $env:ProgramFiles 'leaf'
$lnk = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\leaf.lnk'
$uninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\leaf'

function Inside([string]$p, [string]$dir) {
    $p = [IO.Path]::GetFullPath($p); $dir = [IO.Path]::GetFullPath($dir).TrimEnd('\') + '\'
    $p.StartsWith($dir, [StringComparison]::OrdinalIgnoreCase)
}
function Step([string]$what) { if ($DryRun) { Write-Host "would: $what" } else { Write-Host $what } }

if ($DryRun) {
    Step "turn off 'Default file manager' for this user through $dest\leaf.exe (when it is registered there), unless -Force"
    Step 'ask for administrator permission once (Windows UAC prompt) when not elevated'
    Step "end leaf programs that run from $dest"
    Step "remove the folder $dest"
    Step "remove the shortcut $lnk"
    Step "remove the registry key $uninstallKey"
    Step 'settings, journals and caches in the user folders are kept'
    exit 0
}

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"")
    if ($Force) { $argv += '-Force' }
    Write-Host 'leaf needs administrator permission once to remove its files from Program Files: Windows will ask.'
    $child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList $argv -Verb RunAs -Wait -PassThru
    exit $child.ExitCode
}

# This script lives in the folder it deletes: run from a copy in the temp folder instead.
if (-not $FromTemp -and (Inside $PSCommandPath $dest)) {
    $copy = Join-Path ([IO.Path]::GetTempPath()) ("leaf-uninstall-" + [Guid]::NewGuid().ToString('N') + '.ps1')
    Copy-Item $PSCommandPath $copy
    $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$copy`"", '-FromTemp')
    if ($Force) { $argv += '-Force' }
    $child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList $argv -Wait -PassThru
    Remove-Item -Force $copy -ErrorAction SilentlyContinue
    exit $child.ExitCode
}

try {
    # the folder-handler launcher (per user) starts the leaf it was turned on for: turn it off with the installed copy first
    $reg = Join-Path $env:LOCALAPPDATA 'leaf-default-fm\leaf-exe.txt'
    if (Test-Path $reg) {
        $target = (Get-Content $reg -Raw).Trim()
        if ($target -and (Inside $target $dest)) {
            $installed = Join-Path $dest 'leaf.exe'
            if (Test-Path $installed) {
                & $installed default-fm off
                if ($LASTEXITCODE -ne 0 -and -not $Force) {
                    Write-Host 'could not turn off "Default file manager" (-Force removes the files anyway).'
                    Read-Host 'press Enter to close'
                    exit 1
                }
            } elseif (-not $Force) {
                Write-Host "leaf is still the default file manager through $target, which is not there to turn it off (-Force removes the files anyway)."
                Read-Host 'press Enter to close'
                exit 1
            }
        }
    }
    foreach ($name in 'leaf', 'leaf-look', 'leaf-open', 'leaf-nmhost', 'leaf-mcp', 'leaf-helper') {
        Get-Process -Name $name -ErrorAction SilentlyContinue | ForEach-Object {
            $p = $null
            try { $p = $_.MainModule.FileName } catch { }
            if ($p -and (Inside $p $dest)) {
                Write-Host "stopping $name (pid $($_.Id))"
                Stop-Process -Id $_.Id -Force
                $_.WaitForExit(5000) | Out-Null
            }
        }
    }
    if (Test-Path $dest) { Remove-Item -Recurse -Force $dest; Write-Host "removed $dest" } else { Write-Host "$dest was not there" }
    if (Test-Path $lnk) { Remove-Item -Force $lnk; Write-Host "removed $lnk" }
    if (Test-Path $uninstallKey) { Remove-Item -Recurse -Force $uninstallKey; Write-Host "removed $uninstallKey" }
    Write-Host 'settings, journals and caches in the user folders were kept.'
} catch {
    Write-Host "uninstall failed: $_"
    Read-Host 'press Enter to close'
    exit 1
}
Read-Host 'press Enter to close'
exit 0
