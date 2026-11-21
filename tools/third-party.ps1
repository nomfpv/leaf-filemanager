# Regenerates THIRD-PARTY.md (cargo-about, rf-app package, Windows target).
#   tools\third-party.ps1
# Needs: cargo install cargo-about --locked --features cli
# Fails when a dependency carries a licence that about.toml does not accept: look at it, then add it.
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$env:CARGO_BUILD_JOBS = '5'
Push-Location $root
try {
    cargo about generate -m crates/rf-app/Cargo.toml -c about.toml -o THIRD-PARTY.md about.hbs
    if ($LASTEXITCODE -ne 0) { throw "cargo about failed ($LASTEXITCODE)" }
} finally { Pop-Location }
Write-Host "wrote THIRD-PARTY.md"
