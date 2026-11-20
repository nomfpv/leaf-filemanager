# Pre-push checks for leaf. Run from anywhere inside the repository.
#   1. gitleaks over every commit reachable from a local branch but not from upstream/*
#   2. private-string search over the tracked tree of HEAD ($env:LEAF_NOTES\private-strings.txt)
#   3. no Co-Authored-By / Generated with in commit messages of that range
#   4. no CLAUDE.md, AGENTS.md or .claude/ among tracked files
# Exits non-zero on any failure. A missing prerequisite is a failure, never a silent pass.

$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$failures = New-Object System.Collections.Generic.List[string]
function Fail([string]$msg) { $script:failures.Add($msg); Write-Host ("FAIL: " + $msg) -ForegroundColor Red }
function Ok([string]$msg) { Write-Host ("ok:   " + $msg) -ForegroundColor Green }

$top = (& git rev-parse --show-toplevel 2>$null)
if ($LASTEXITCODE -ne 0 -or -not $top) { Write-Host "FAIL: not inside a git repository"; exit 1 }
Set-Location $top
Write-Host ("repo: " + $top)

# Range: all local branches, minus everything upstream/* already has.
$range = @('--branches', '--not', '--remotes=upstream')
$commits = @(& git rev-list @range)
Write-Host ("commits in range: " + $commits.Count)
if ($commits.Count -eq 0) { Fail "no commits in range (is there a local branch?)" }

# 1. gitleaks
$gl = $null
$cmd = Get-Command gitleaks -ErrorAction SilentlyContinue
if ($cmd) { $gl = $cmd.Source }
elseif ($env:LEAF_TOOLS) {
    $cand = Join-Path $env:LEAF_TOOLS 'gitleaks\gitleaks.exe'
    if (Test-Path -LiteralPath $cand) { $gl = $cand }
}
if (-not $gl) {
    Fail "gitleaks not found on PATH or in LEAF_TOOLS\gitleaks\gitleaks.exe"
} else {
    $cfg = Join-Path $top '.gitleaks.toml'
    & $gl git --config $cfg --no-banner --redact --log-opts ($range -join ' ') $top
    if ($LASTEXITCODE -eq 0) { Ok "gitleaks: no leaks in range" } else { Fail ("gitleaks exited with " + $LASTEXITCODE) }
}

# 2. private strings
$tracked = @(& git -c core.quotepath=off ls-files)
if (-not $env:LEAF_NOTES) {
    Fail "LEAF_NOTES is not set; cannot find private-strings.txt"
} else {
    $psf = Join-Path $env:LEAF_NOTES 'private-strings.txt'
    if (-not (Test-Path -LiteralPath $psf)) {
        Fail ("private-strings.txt not found at " + $psf)
    } else {
        $patterns = @(Get-Content -LiteralPath $psf | ForEach-Object { $_.Trim() } | Where-Object { $_ -and -not $_.StartsWith('#') })
        if ($patterns.Count -eq 0) {
            Fail "private-strings.txt has no patterns"
        } else {
            $regexes = @()
            foreach ($p in $patterns) {
                try { $regexes += New-Object System.Text.RegularExpressions.Regex($p, 'IgnoreCase') }
                catch { Fail ("invalid regex in private-strings.txt: " + $p) }
            }
            $hits = 0
            foreach ($f in $tracked) {
                $full = Join-Path $top $f
                if (-not (Test-Path -LiteralPath $full -PathType Leaf)) { continue }
                $fi = Get-Item -LiteralPath $full
                if ($fi.Length -gt 20MB) { continue }
                $n = 0
                try {
                    foreach ($line in [System.IO.File]::ReadLines($full)) {
                        $n++
                        foreach ($rx in $regexes) {
                            if ($rx.IsMatch($line)) {
                                $hits++
                                if ($hits -le 50) { Write-Host ("  private-string hit: " + $f + ":" + $n + " (pattern #" + ([array]::IndexOf($regexes, $rx) + 1) + ")") }
                            }
                        }
                    }
                } catch { }
            }
            if ($hits -gt 0) { Fail ("private-string search: " + $hits + " hit(s) in tracked files (patterns not printed)") }
            else { Ok ("private-string search: " + $regexes.Count + " pattern(s), no hits") }
        }
    }
}

# 3. commit messages
$bad = 0
foreach ($c in $commits) {
    $msg = (& git show -s --format=%B $c) -join "`n"
    if ($msg -match '(?i)co-authored-by' -or $msg -match '(?i)generated with') {
        $bad++
        Write-Host ("  bad commit message: " + $c.Substring(0, 10))
    }
}
if ($bad -gt 0) { Fail ("" + $bad + " commit message(s) contain Co-Authored-By or Generated with") }
else { Ok "commit messages clean" }

# 4. agent files
$agent = @($tracked | Where-Object { $_ -match '(^|/)(CLAUDE\.md|AGENTS\.md)$' -or $_ -match '(^|/)\.claude/' })
if ($agent.Count -gt 0) { Fail ("tracked agent files: " + ($agent -join ', ')) }
else { Ok "no agent instruction files tracked" }

Write-Host ""
if ($failures.Count -gt 0) {
    Write-Host ("prepush: " + $failures.Count + " check(s) FAILED") -ForegroundColor Red
    exit 1
}
Write-Host "prepush: all checks passed" -ForegroundColor Green
exit 0
