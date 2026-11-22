# A plain local install without Velopack: copies dist\stage (made by tools\package.ps1) to %LOCALAPPDATA%\Programs\leaf and makes a
# Start menu shortcut "leaf". The dev copy in target\ is never touched. Updates do not apply to this kind of install (it is not
# a Velopack install): run this script again with a newer stage.
# "Retry as administrator" does not work in this kind of install: the folder belongs to the user, so leaf-helper.exe could be
# swapped by any program of the user, and leaf refuses to start it elevated from there (it says so). It needs a per-machine install
# (Program Files, writable only by administrators): that is -AllUsers.
#   tools\install-local.ps1 [-Uninstall] [-Force]            per user, no elevation
#   tools\install-local.ps1 -AllUsers [-Uninstall] [-Force]  for all users: %ProgramFiles%\leaf, Start menu for everyone, an entry in
#                                                            Windows' Apps list. Asks for administrator permission ONCE (the normal
#                                                            UAC prompt, from this script, never silently).
#   -DryRun prints what would be done and changes nothing (no prompt, no files, no registry).
param(
    [switch]$Uninstall,
    [switch]$Force,
    [switch]$AllUsers,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$stage = Join-Path $root 'dist\stage'
$dest = Join-Path $env:LOCALAPPDATA 'Programs\leaf'
$lnk = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\leaf.lnk'
if ($AllUsers) {
    $dest = Join-Path $env:ProgramFiles 'leaf'
    $lnk = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\leaf.lnk'
}
$uninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\leaf'

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
    foreach ($name in 'leaf', 'leaf-look', 'leaf-open', 'leaf-nmhost', 'leaf-mcp', 'leaf-helper') {
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

if ($AllUsers) {
    if ($Uninstall) {
        $u = Join-Path $PSScriptRoot 'uninstall-all-users.ps1'
        $a = @{}; if ($Force) { $a['Force'] = $true }; if ($DryRun) { $a['DryRun'] = $true }
        & $u @a
        exit $LASTEXITCODE
    }
    $isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    function Step([string]$what) { if ($DryRun) { Write-Host "would: $what" } else { Write-Host $what } }
    Step "stop leaf programs that run from $dest"
    if (-not (Test-Path (Join-Path $stage 'leaf.exe'))) {
        if ($DryRun) { Write-Host "(note: nothing staged in $stage yet: run tools\package.ps1 before the real install)" }
        else { throw "nothing staged: run tools\package.ps1 first (it makes $stage)" }
    }
    if (-not $isAdmin) {
        Step 'ask for administrator permission once (Windows UAC prompt), then run this same install elevated'
        if ($DryRun) {
            Step "replace the contents of $dest with the staged files; owner Administrators, access inherited from Program Files (users: read and execute)"
            Step "write $dest\uninstall.ps1"
            Step "create the all-users Start menu shortcut $lnk"
            Step "write the uninstall entry $uninstallKey (DisplayName, DisplayVersion, InstallLocation, UninstallString, ...)"
            Step 'per-user data (settings, journals, caches) stays in the user folders; nothing there is touched'
            exit 0
        }
        $argv = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-AllUsers')
        if ($Force) { $argv += '-Force' }
        Write-Host 'leaf needs administrator permission once to write to Program Files: Windows will ask.'
        $child = Start-Process -FilePath (Get-Process -Id $PID).Path -ArgumentList $argv -Verb RunAs -Wait -PassThru
        exit $child.ExitCode
    }
    # (elevated now, or a dry run as administrator)
    if ($DryRun) {
        Step "replace the contents of $dest with the staged files; owner Administrators, access inherited from Program Files (users: read and execute)"
        Step "write $dest\uninstall.ps1"
        Step "create the all-users Start menu shortcut $lnk"
        Step "write the uninstall entry $uninstallKey"
        exit 0
    }
    try {
        Stop-Installed
        if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
        New-Item -ItemType Directory -Force $dest | Out-Null
        Copy-Item (Join-Path $stage '*') $dest -Recurse -Force
        Copy-Item (Join-Path $PSScriptRoot 'uninstall-all-users.ps1') (Join-Path $dest 'uninstall.ps1') -Force
        # owner Administrators (S-1-5-32-544); the access list is the one Program Files hands down (users read and execute)
        & icacls.exe $dest /reset /T /C /Q | Out-Null
        & icacls.exe $dest /setowner '*S-1-5-32-544' /T /C /Q | Out-Null
        Write-Host "copied $((Get-ChildItem $stage).Count) files to $dest"
        $sh = New-Object -ComObject WScript.Shell
        $s = $sh.CreateShortcut($lnk)
        $s.TargetPath = Join-Path $dest 'leaf.exe'
        $s.WorkingDirectory = $env:SystemDrive + '\'
        $s.Description = 'leaf file manager'
        $s.Save()
        Write-Host "Start menu shortcut for all users: $lnk"
        $ver = (Get-Item (Join-Path $dest 'leaf.exe')).VersionInfo.ProductVersion
        $size = [int]((Get-ChildItem $dest -Recurse -File | Measure-Object Length -Sum).Sum / 1KB)
        New-Item -Path $uninstallKey -Force | Out-Null
        $props = @{
            DisplayName = 'leaf'; DisplayVersion = "$ver"; Publisher = 'leaf'; InstallLocation = $dest
            DisplayIcon = (Join-Path $dest 'leaf.exe')
            UninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$dest\uninstall.ps1`""
        }
        foreach ($k in $props.Keys) { New-ItemProperty -Path $uninstallKey -Name $k -Value $props[$k] -PropertyType String -Force | Out-Null }
        New-ItemProperty -Path $uninstallKey -Name NoModify -Value 1 -PropertyType DWord -Force | Out-Null
        New-ItemProperty -Path $uninstallKey -Name NoRepair -Value 1 -PropertyType DWord -Force | Out-Null
        New-ItemProperty -Path $uninstallKey -Name EstimatedSize -Value $size -PropertyType DWord -Force | Out-Null
        Write-Host "uninstall entry: $uninstallKey (Windows Apps list)"
        $old = Join-Path $env:LOCALAPPDATA 'Programs\leaf'
        if ($env:LOCALAPPDATA -and (Test-Path (Join-Path $old 'leaf.exe'))) {
            Write-Host "note: a per-user copy exists in $old. Remove it with tools\install-local.ps1 -Uninstall (not elevated); leaf re-points its Windows registrations to this install the next time it starts."
        }
        Write-Host 'done. Per-user settings stay in the user folders. Updates: run the installer again.'
    } catch {
        Write-Host "install failed: $_"
        Read-Host 'press Enter to close'
        exit 1
    }
    Read-Host 'press Enter to close'
    exit 0
}
if (-not $DryRun -and (Test-Path (Join-Path $env:ProgramFiles 'leaf\leaf.exe'))) {
    Write-Host 'note: leaf is also installed for all users. A per-user copy beside it works, but leaf then points its registrations at whichever copy started last; remove one of them.'
}

if ($DryRun) {
    Write-Host "would: copy $stage to $dest and create the Start menu shortcut $lnk$(if ($Uninstall) { ' (uninstall: remove them)' })"
    exit 0
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
