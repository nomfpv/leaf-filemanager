<#
.SYNOPSIS
  Repeatable leaf benchmark: folder open (openbench, 100k and 10k files) and copy (copy_vs).
.DESCRIPTION
  Builds release (unless -SkipBuild), creates test data if missing, runs the benchmarks,
  writes one JSON and one Markdown table to OutDir (never inside the repo) and checks the
  budgets from the plan. With -Baseline, runs baseline and new alternately (A/B pairs).
  Exit code 0 = within budget, 1 = a budget or baseline check missed, 2 = setup/run error.
.PARAMETER DataDir   Bench data root (default $env:LEAF_BENCH). Holds files100k, files10k, copysrc.
.PARAMETER OutDir    Results folder (default $env:LEAF_NOTES\bench).
.PARAMETER Baseline  Optional path of a baseline app exe to compare against.
.PARAMETER SkipBuild Use the existing release build.
.PARAMETER Runs      Runs per folder-open measurement; run 1 is discarded (default 6).
.PARAMETER NoiseMs   Allowed settled-time difference against the baseline (default 50).
.PARAMETER NoiseMB   Allowed private-memory difference against the baseline (default 2).
.PARAMETER CopyRounds Rounds for copy_vs (default 3).
.PARAMETER With10k   Also measure the 10,000-file folder (doubles the number of app launches). Off by default.
.PARAMETER WithCopy  Also run the copy comparison (slow: Windows' own copy takes about a minute per round). Off by default; use it when copy code changed.
#>
[CmdletBinding()]
param(
    [string]$DataDir = $env:LEAF_BENCH,
    [string]$OutDir = $(if ($env:LEAF_NOTES) { Join-Path $env:LEAF_NOTES 'bench' } else { '' }),
    [string]$Baseline = '',
    [switch]$SkipBuild,
    [int]$Runs = 4,
    [double]$NoiseMs = 50,
    [double]$NoiseMB = 2,
    [int]$CopyRounds = 3,
    [switch]$WithCopy,
    [switch]$With10k
)

$ErrorActionPreference = 'Continue'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$ci = [System.Globalization.CultureInfo]::InvariantCulture

# Budgets (plan section 5.1)
$Budget = @{ Settled100k = 210.0; Settled10k = 227.0; PrivateMB = 84.0; CopyOursMs = 3700.0 }

function Die([string]$msg) { Write-Host ("bench: " + $msg) -ForegroundColor Red; exit 2 }

if (-not $DataDir) { Die "no -DataDir and LEAF_BENCH is not set" }
if (-not $OutDir) { Die "no -OutDir and LEAF_NOTES is not set" }
if ($Runs -lt 2) { Die "-Runs must be at least 2 (run 1 is discarded)" }

$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$null = New-Item -ItemType Directory -Force -Path $DataDir
$null = New-Item -ItemType Directory -Force -Path $OutDir
$DataDir = (Resolve-Path $DataDir).Path
$OutDir = (Resolve-Path $OutDir).Path
if ($OutDir.TrimEnd('\').StartsWith($repo.TrimEnd('\') + '\', [StringComparison]::OrdinalIgnoreCase)) {
    Die "OutDir is inside the repository; results must be stored outside it"
}
if ($Baseline -and -not (Test-Path -LiteralPath $Baseline -PathType Leaf)) { Die "baseline exe not found: $Baseline" }

# ---- build ----
$target = Join-Path $repo 'target\release'
if (-not $SkipBuild) {
    Push-Location $repo
    try {
        Write-Host "bench: cargo build --release -p rf-app"
        & cargo build --release -p rf-app
        if ($LASTEXITCODE -ne 0) { Die "cargo build rf-app failed" }
        Write-Host "bench: cargo build --release -p rf-win (examples)"
        & cargo build --release -p rf-win --example openbench --example copy_vs --example thread_cpu
        if ($LASTEXITCODE -ne 0) { Die "cargo build rf-win examples failed" }
    } finally { Pop-Location }
}
$app = $null
foreach ($n in @('leaf.exe')) {
    $c = Join-Path $target $n
    if (Test-Path -LiteralPath $c) { $app = $c; break }
}
if (-not $app) { Die "no release app exe in $target (leaf.exe)" }
$openbench = Join-Path $target 'examples\openbench.exe'
$copyvs = Join-Path $target 'examples\copy_vs.exe'
foreach ($e in $openbench, $copyvs) { if (-not (Test-Path -LiteralPath $e)) { Die "missing $e (build without -SkipBuild)" } }

# ---- test data ----
$exts = @('txt', 'jpg', 'png', 'pdf', 'docx', 'mp4', 'zip', 'rs', 'md', 'log', 'json', 'csv')
function Ensure-Files([string]$dir, [int]$count) {
    $have = 0
    if (Test-Path -LiteralPath $dir) { $have = [System.IO.Directory]::GetFiles($dir).Length }
    if ($have -eq $count) { return }
    if ($have -ne 0) { Die "$dir has $have files, expected $count; remove it so it can be recreated" }
    Write-Host ("bench: creating " + $count + " empty files in " + $dir)
    $null = New-Item -ItemType Directory -Force -Path $dir
    for ($i = 0; $i -lt $count; $i++) {
        $name = 'file_{0:D6}.{1}' -f $i, $exts[$i % $exts.Length]
        [System.IO.File]::Create((Join-Path $dir $name)).Dispose()
    }
}
$dir100k = Join-Path $DataDir 'files100k'
$dir10k = Join-Path $DataDir 'files10k'
Ensure-Files $dir100k 100000
if ($With10k) { Ensure-Files $dir10k 10000 }

# copy source: 20,000 small files in 100 sub-folders
$copySrc = Join-Path $DataDir 'copysrc'
$copyDst = Join-Path $DataDir 'copydest'
if ($WithCopy -and -not (Test-Path -LiteralPath $copySrc)) {
    Write-Host "bench: creating 20000 small files in $copySrc"
    $payload = New-Object byte[] 1024
    (New-Object System.Random 1).NextBytes($payload)
    for ($d = 0; $d -lt 100; $d++) {
        $sub = Join-Path $copySrc ('d{0:D3}' -f $d)
        $null = New-Item -ItemType Directory -Force -Path $sub
        for ($f = 0; $f -lt 200; $f++) {
            [System.IO.File]::WriteAllBytes((Join-Path $sub ('f{0:D4}.bin' -f $f)), $payload)
        }
    }
}
if ($WithCopy) { $null = New-Item -ItemType Directory -Force -Path $copyDst }

# ---- helpers ----
function Median([double[]]$v) {
    if ($v.Count -eq 0) { return [double]::NaN }
    $s = @($v | Sort-Object)
    $n = $s.Count
    if ($n % 2 -eq 1) { return [double]$s[($n - 1) / 2] }
    return ([double]$s[$n / 2 - 1] + [double]$s[$n / 2]) / 2.0
}

# openbench prints: "<kind> run<i>: settled N ms | window N ms | cpu N ms | private N MB | shared S"
# or "<kind> run<i>: ERROR <msg>". Returns $null on ERROR / unparsable.
function Invoke-OpenBench([string]$exe, [string]$folder) {
    $lines = @(& $openbench rf $exe $folder 1)
    foreach ($l in $lines) {
        $m = [regex]::Match([string]$l, 'settled\s+([\d.]+)\s+ms\s*\|\s*window\s+([\d.]+)\s+ms\s*\|\s*cpu\s+([\d.]+)\s+ms\s*\|\s*private\s+([\d.]+)\s+MB')
        if ($m.Success) {
            return [pscustomobject]@{
                Settled = [double]::Parse($m.Groups[1].Value, $ci)
                Window = [double]::Parse($m.Groups[2].Value, $ci)
                Cpu = [double]::Parse($m.Groups[3].Value, $ci)
                PrivateMB = [double]::Parse($m.Groups[4].Value, $ci)
            }
        }
    }
    Write-Host ("  openbench gave no result: " + ($lines -join ' / ')) -ForegroundColor Yellow
    return $null
}

function Summarize($samples) {
    $ok = @($samples | Where-Object { $_ })
    if ($ok.Count -eq 0) { return $null }
    return [ordered]@{
        n = $ok.Count
        settled_ms = [math]::Round((Median ($ok | ForEach-Object { $_.Settled })), 1)
        window_ms = [math]::Round((Median ($ok | ForEach-Object { $_.Window })), 1)
        cpu_ms = [math]::Round((Median ($ok | ForEach-Object { $_.Cpu })), 1)
        private_mb = [math]::Round((Median ($ok | ForEach-Object { $_.PrivateMB })), 1)
        raw = @($ok | ForEach-Object { @($_.Settled, $_.Window, $_.Cpu, $_.PrivateMB) -join '/' })
    }
}

function Measure-Set([string]$label, [string]$folder) {
    Write-Host ("bench: " + $label + " (" + $Runs + " runs, run 1 discarded" + $(if ($Baseline) { ', A/B alternating' } else { '' }) + ")")
    $new = @(); $base = @()
    for ($i = 1; $i -le $Runs; $i++) {
        # alternate which side goes first so drift hits both equally
        $order = if ($Baseline -and ($i % 2 -eq 0)) { @('base', 'new') } elseif ($Baseline) { @('new', 'base') } else { @('new') }
        foreach ($side in $order) {
            $exe = if ($side -eq 'new') { $app } else { $Baseline }
            $r = Invoke-OpenBench $exe $folder
            if ($i -gt 1) { if ($side -eq 'new') { $new += $r } else { $base += $r } }
            if ($r) { Write-Host ("  run {0} {1}: settled {2} window {3} cpu {4} private {5}" -f $i, $side, $r.Settled, $r.Window, $r.Cpu, $r.PrivateMB) }
        }
    }
    return [pscustomobject]@{ New = (Summarize $new); Base = $(if ($Baseline) { Summarize $base } else { $null }) }
}

$r100 = Measure-Set 'files100k' $dir100k
$r10 = if ($With10k) { Measure-Set 'files10k' $dir10k } else { $null }

# ---- copy ----
# copy_vs prints "round R ours   : N ms" / "round R windows: N ms" and "median ours N ms, windows N ms"
$copyLines = @()
if ($WithCopy) {
    Write-Host ("bench: copy_vs (" + $CopyRounds + " rounds)")
    $copyLines = @(& $copyvs $copyDst $CopyRounds $copySrc)
}
$copy = $null
foreach ($l in $copyLines) {
    $m = [regex]::Match([string]$l, 'median ours\s+([\d.]+)\s+ms,\s*windows\s+([\d.]+)\s+ms')
    if ($m.Success) {
        $copy = [ordered]@{
            ours_ms = [double]::Parse($m.Groups[1].Value, $ci)
            windows_ms = [double]::Parse($m.Groups[2].Value, $ci)
            rounds = @($copyLines | Where-Object { $_ -match '^round ' })
        }
    }
}
if ($WithCopy -and -not $copy) { Write-Host ("  copy_vs gave no median: " + ($copyLines -join ' / ')) -ForegroundColor Yellow }

# ---- checks ----
$checks = New-Object System.Collections.Generic.List[object]
function Check([string]$name, [bool]$pass, [string]$detail) {
    $script:checks.Add([ordered]@{ name = $name; pass = $pass; detail = $detail })
    $tag = if ($pass) { 'PASS' } else { 'MISS' }
    Write-Host ("  [" + $tag + "] " + $name + ": " + $detail) -ForegroundColor $(if ($pass) { 'Green' } else { 'Red' })
}
function Budget-Check([string]$name, $value, [double]$limit, [string]$unit) {
    if ($null -eq $value) { Check $name $false "no measurement"; return }
    Check $name ($value -le $limit) ("{0} {1} (budget <= {2} {1})" -f $value, $unit, $limit)
}
Write-Host "bench: budgets"
Budget-Check 'open 100k settled' $r100.New.settled_ms $Budget.Settled100k 'ms'
if ($r10) { Budget-Check 'open 10k settled' $r10.New.settled_ms $Budget.Settled10k 'ms' }
Budget-Check 'private memory after 100k' $r100.New.private_mb $Budget.PrivateMB 'MB'
if ($WithCopy) { if ($copy) { Budget-Check 'copy 20k files (ours)' $copy.ours_ms $Budget.CopyOursMs 'ms' } else { Check 'copy 20k files (ours)' $false 'no measurement' } }
if ($Baseline) {
    Write-Host ("bench: against baseline (noise tolerance " + $NoiseMs + " ms, " + $NoiseMB + " MB)")
    foreach ($pair in @(@('100k', $r100), @('10k', $r10))) {
        if (-not $pair[1]) { continue }
        $n = $pair[1].New; $b = $pair[1].Base
        if (-not $n -or -not $b) { Check ("baseline " + $pair[0]) $false 'no measurement'; continue }
        Check ("vs baseline " + $pair[0] + " settled") ($n.settled_ms -le $b.settled_ms + $NoiseMs) ("{0} ms vs {1} ms" -f $n.settled_ms, $b.settled_ms)
        Check ("vs baseline " + $pair[0] + " private") ($n.private_mb -le $b.private_mb + $NoiseMB) ("{0} MB vs {1} MB" -f $n.private_mb, $b.private_mb)
    }
}

# ---- report ----
$hash = (& git -C $repo rev-parse --short HEAD)
$dirty = (& git -C $repo status --porcelain)
if ($dirty) { $hash = $hash + '-dirty' }
$now = Get-Date
$stamp = $now.ToString('yyyyMMdd-HHmmss')
$base = Join-Path $OutDir ("bench-" + $stamp + "-" + $hash)
$failed = @($checks | Where-Object { -not $_.pass }).Count

$result = [ordered]@{
    timestamp = $now.ToString('o')
    commit = $hash
    app = [System.IO.Path]::GetFileName($app)
    baseline = $(if ($Baseline) { [System.IO.Path]::GetFileName($Baseline) } else { $null })
    runs = $Runs
    files100k = $r100
    files10k = $r10
    copy = $copy
    checks = $checks
    passed = ($failed -eq 0)
}
$result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath ($base + '.json') -Encoding UTF8

$md = New-Object System.Text.StringBuilder
function Row($label, $s) {
    if ($s) { return ("| {0} | {1} | {2} | {3} | {4} |" -f $label, $s.settled_ms, $s.window_ms, $s.cpu_ms, $s.private_mb) }
    return ("| {0} | - | - | - | - |" -f $label)
}
[void]$md.AppendLine("# leaf bench " + $stamp)
[void]$md.AppendLine("")
[void]$md.AppendLine("Commit: " + $hash + "  |  App: " + [System.IO.Path]::GetFileName($app) + "  |  Runs: " + $Runs + " (run 1 discarded, median of the rest)")
[void]$md.AppendLine("")
[void]$md.AppendLine("| Set | settled ms | window ms | CPU ms | private MB |")
[void]$md.AppendLine("|---|---|---|---|---|")
[void]$md.AppendLine((Row 'files100k' $r100.New))
if ($r10) { [void]$md.AppendLine((Row 'files10k' $r10.New)) }
if ($Baseline) {
    [void]$md.AppendLine((Row 'files100k (baseline)' $r100.Base))
    if ($r10) { [void]$md.AppendLine((Row 'files10k (baseline)' $r10.Base)) }
}
[void]$md.AppendLine("")
if ($copy) {
    [void]$md.AppendLine("| Copy 20k files | ours ms | windows ms |")
    [void]$md.AppendLine("|---|---|---|")
    [void]$md.AppendLine(("| median ({0} rounds) | {1} | {2} |" -f $CopyRounds, $copy.ours_ms, $copy.windows_ms))
    [void]$md.AppendLine("")
}
[void]$md.AppendLine("| Check | Result | Detail |")
[void]$md.AppendLine("|---|---|---|")
foreach ($c in $checks) { [void]$md.AppendLine(("| {0} | {1} | {2} |" -f $c.name, $(if ($c.pass) { 'pass' } else { 'MISS' }), $c.detail)) }
Set-Content -LiteralPath ($base + '.md') -Value $md.ToString() -Encoding UTF8

Write-Host ("bench: wrote " + $base + ".json and .md")
if ($failed -gt 0) { Write-Host ("bench: " + $failed + " check(s) missed") -ForegroundColor Red; exit 1 }
Write-Host "bench: all checks passed" -ForegroundColor Green
exit 0
