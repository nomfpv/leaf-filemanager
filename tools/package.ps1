# Release build and installer: builds leaf, stages every file an installed leaf needs in dist\stage, then (when the Velopack
# tool `vpk` is on PATH) packs an installer and update packages into dist\release. Never uploads anything.
#   tools\package.ps1 [-Version 0.9.5] [-NoBuild] [-Sign <certificate thumbprint>]
# Without `vpk` the staged folder is still made (tools\install-local.ps1 installs from it). Install the tool once with:
#   dotnet tool install -g vpk
# Not signed unless -Sign is given: Windows SmartScreen warns about an unsigned installer (docs\packaging.md).
param(
    [string]$Version = '',
    [switch]$NoBuild,
    [string]$Sign = ''
)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$stage = Join-Path $root 'dist\stage'
$out = Join-Path $root 'dist\release'
$rel = Join-Path $root 'target\release'

# version: the workspace's, unless given
if (-not $Version) {
    $m = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $m) { throw 'no version in Cargo.toml' }
    $Version = $m.Matches[0].Groups[1].Value
}
if ($Version -notmatch '^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') { throw "version '$Version' is not semver (1.2.3)" }

# (a) build: leaf.exe, leaf-look.exe, leaf-open.exe (rf-app's bins), the two helper programs and the administrator helper
if (-not $NoBuild) {
    Push-Location $root
    try {
        cargo build --release -p rf-app -p leaf-mcp -p leaf-nmhost -p leaf-helper
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally { Pop-Location }
}

# (b) stage: a clean folder with exactly what an install holds. (leaf-helper.exe, the administrator helper, is only used from a
# per-machine install: in a folder the user can write to, leaf does not start it elevated.)
$files = @('leaf.exe', 'leaf-look.exe', 'leaf-open.exe', 'leaf-mcp.exe', 'leaf-nmhost.exe', 'leaf-helper.exe')
foreach ($f in $files) {
    if (-not (Test-Path (Join-Path $rel $f))) { throw "$f is missing in $rel (build first, or drop -NoBuild)" }
}
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
foreach ($f in $files) { Copy-Item (Join-Path $rel $f) $stage }
# the browser extension's two loadable folders (Settings > Advanced > Downloads opens them): built from extension\src, no tools needed
& powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File (Join-Path $root 'extensionuild.ps1') | Out-Null
if ($LASTEXITCODE -ne 0) { throw "extensionuild.ps1 failed ($LASTEXITCODE)" }
foreach ($b in 'chromium', 'firefox') {
    $src = Join-Path $root "extension\dist\$b"
    if (-not (Test-Path (Join-Path $src 'manifest.json'))) { throw "extension\dist\$b has no manifest.json" }
    $dst = Join-Path $stage "extension\dist\$b"
    New-Item -ItemType Directory -Force $dst | Out-Null
    Copy-Item (Join-Path $src '*') $dst -Recurse
}
Copy-Item (Join-Path $root 'LICENSE') $stage
Copy-Item (Join-Path $root 'THIRD-PARTY.md') $stage
Write-Host "staged $($files.Count) programs + licence files in $stage (version $Version)"

# (c) installer, when vpk is there
$vpk = Get-Command vpk -ErrorAction SilentlyContinue
if (-not $vpk) {
    Write-Host ''
    Write-Host 'vpk (the Velopack tool) is not on PATH, so no installer was made. The staged folder is ready:'
    Write-Host '  - local copy now:  tools\install-local.ps1'
    Write-Host '  - installer:       dotnet tool install -g vpk   (needs the .NET SDK), then run this script again with -NoBuild'
    exit 0
}
New-Item -ItemType Directory -Force $out | Out-Null
$packArgs = @(
    'pack',
    '--packId', 'leaf',                                   # shortcuts get the AppUserModelID velopack.leaf (installer.rs APP_ID)
    '--packVersion', $Version,
    '--packDir', $stage,
    '--mainExe', 'leaf.exe',
    '--packTitle', 'leaf',
    '--icon', (Join-Path $root 'crates\rf-app\assets\leaf.ico'),
    '--outputDir', $out
)
if ($Sign) {
    # signtool with this certificate from the store; the timestamp server is contacted only when signing
    $packArgs += @('--signParams', "/sha1 $Sign /fd sha256 /tr http://timestamp.digicert.com /td sha256")
}
& vpk @packArgs
if ($LASTEXITCODE -ne 0) { throw "vpk pack failed ($LASTEXITCODE)" }
Write-Host ''
Write-Host "release files in $out (nothing was uploaded):"
Get-ChildItem $out | ForEach-Object { Write-Host "  $($_.Name)" }
if (-not $Sign) { Write-Host 'not signed: Windows SmartScreen will warn when this installer is first run.' }
