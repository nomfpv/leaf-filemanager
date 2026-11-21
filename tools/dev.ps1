# Fast development loop: stop the dev instance, build the fast profile, start it isolated.
#   tools\dev.ps1 [-Path <folder>] [-NoRun]
# The dev copy runs with LEAF_MULTI_INSTANCE=1 and LEAF_CONFIG_DIR=target\dev-config, so it never
# touches the real settings, the registry (default file manager) or a running leaf.
param(
    [string]$Path = '',
    [switch]$NoRun
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$exe = Join-Path $root 'target\fast\leaf.exe'

# (a) stop only the leaf.exe that lives at this repo's target\fast\leaf.exe
Get-Process -Name leaf -ErrorAction SilentlyContinue | ForEach-Object {
    $p = $null
    try { $p = $_.MainModule.FileName } catch { }
    if ($p -and ([string]::Equals([System.IO.Path]::GetFullPath($p), [System.IO.Path]::GetFullPath($exe), [System.StringComparison]::OrdinalIgnoreCase))) {
        Write-Host "stopping dev instance (pid $($_.Id))"
        Stop-Process -Id $_.Id -Force
        $_.WaitForExit(5000) | Out-Null
    }
}

# (b) build
Push-Location $root
try {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    cargo build --profile fast -p rf-app
    $code = $LASTEXITCODE
    $sw.Stop()
} finally { Pop-Location }
if ($code -ne 0) { Write-Host "build FAILED after $([math]::Round($sw.Elapsed.TotalSeconds, 1)) s"; exit $code }
Write-Host "build ok in $([math]::Round($sw.Elapsed.TotalSeconds, 1)) s"
if ($NoRun) { exit 0 }

# (c) start, isolated
$cfg = Join-Path $root 'target\dev-config'
New-Item -ItemType Directory -Force $cfg | Out-Null
$env:LEAF_MULTI_INSTANCE = '1'
$env:LEAF_CONFIG_DIR = $cfg
if ($Path) { Start-Process -FilePath $exe -ArgumentList ('"' + $Path + '"') } else { Start-Process -FilePath $exe }
Write-Host "started $exe (config $cfg)"
