# Installs a pre-push hook that runs tools\prepush.ps1 (blocks the push on failure).
$ErrorActionPreference = 'Stop'
$top = (& git rev-parse --show-toplevel)
$common = (& git rev-parse --path-format=absolute --git-common-dir)
$hooks = Join-Path $common 'hooks'
if (-not (Test-Path -LiteralPath $hooks)) { New-Item -ItemType Directory -Path $hooks | Out-Null }
$script = "tools/prepush.ps1"
$hook = @"
#!/bin/sh
# leaf pre-push: leak and hygiene checks
exec powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$script"
"@
$path = Join-Path $hooks 'pre-push'
[System.IO.File]::WriteAllText($path, ($hook -replace "`r`n", "`n") + "`n", (New-Object System.Text.UTF8Encoding($false)))
Write-Host ("installed " + $path)
