# Makes the two loadable folders: dist\chromium (Chrome, Edge, Brave) and dist\firefox. No tools needed: the sources are plain files.
#   powershell -ExecutionPolicy Bypass -File extension\build.ps1
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
foreach ($b in 'chromium', 'firefox') {
    $out = Join-Path $here "dist\$b"
    if (Test-Path $out) { Remove-Item -Recurse -Force $out }
    New-Item -ItemType Directory -Force $out | Out-Null
    Copy-Item -Recurse (Join-Path $here 'src\*') $out
    Copy-Item (Join-Path $here "manifest.$b.json") (Join-Path $out 'manifest.json')
    Copy-Item (Join-Path $here 'LICENSE-APACHE-2.0'), (Join-Path $here 'NOTICE') $out
    Write-Host "wrote $out"
}
