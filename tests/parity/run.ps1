<#
.SYNOPSIS
    Run the parity suite for a phase and write a §9 report (TEST-STRATEGY §9).

.DESCRIPTION
    P0 skeleton: asserts the workspace builds, records the §9 header (commit,
    corpus manifest hash, tool versions, device, files tested/skipped) and lists
    the golden coverage known so far from the newest golden manifest. Later
    phases flesh out the per-check table. Reports go to
    target/parity/reports/P<n>-<timestamp>.md and are never committed.

.PARAMETER Phase
    Phase label, e.g. P0, P1.

.PARAMETER GoldenManifest
    Golden manifest JSON to summarise. Default: newest in tests/parity/manifests.

.PARAMETER Device
    Output device description for the header. Default from ADR-009.

.PARAMETER SkipBuild
    Skip the cargo build assertion (for fast iteration).

.EXAMPLE
    ./run.ps1 -Phase P0
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Phase,
    [string]$GoldenManifest,
    [string]$Device = 'Senary Audio (built-in), shared mix 48 kHz/24-in-32 (ADR-009)',
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ReportDir   = Join-Path $RepoRoot 'target\parity\reports'
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
if (-not (Test-Path $ReportDir)) { New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null }

# P1 has a dedicated decode-parity suite (p1.ps1). Delegate to it so
# `run.ps1 -Phase P1` exercises lyra-decode against the golden references.
if ($Phase -eq 'P1') {
    $p1 = Join-Path $PSScriptRoot 'p1.ps1'
    $fwd = @{}
    if ($GoldenManifest) { $fwd['GoldenManifest'] = $GoldenManifest }
    return & $p1 @fwd
}

function Get-ToolVersion([string]$name, [string]$arg) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if (-not $cmd) { return "${name}: not found" }
    try { return (& $cmd.Source $arg 2>&1 | Select-Object -First 1 | Out-String).Trim() }
    catch { return "${name}: unknown" }
}

$commit = (& git -C $RepoRoot rev-parse --short HEAD 2>$null | Out-String).Trim()
$branch = (& git -C $RepoRoot rev-parse --abbrev-ref HEAD 2>$null | Out-String).Trim()

# Newest golden manifest if none specified.
if (-not $GoldenManifest) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($latest) { $GoldenManifest = $latest.FullName }
}

$corpusHash = 'n/a'
$goldenSummary = '_No golden manifest found._'
$golden = $null
if ($GoldenManifest -and (Test-Path $GoldenManifest)) {
    $golden = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json
    $corpusHash = $golden.corpusHash
}

# --- Build assertion -------------------------------------------------------
$buildStatus = 'skipped'
if (-not $SkipBuild) {
    Write-Host 'run.ps1: cargo build --workspace ...' -ForegroundColor Cyan
    Push-Location $RepoRoot
    try {
        & cargo build --workspace 2>&1 | Out-Null
        $buildStatus = if ($LASTEXITCODE -eq 0) { 'PASS' } else { 'FAIL' }
    }
    finally { Pop-Location }
}

$flacVer   = Get-ToolVersion 'flac' '--version'
$ffVer     = Get-ToolVersion 'ffmpeg' '-version'
$probeExe  = @('release', 'debug') | ForEach-Object { Join-Path $RepoRoot "target\$_\lyra-probe.exe" } | Where-Object { Test-Path $_ } | Select-Object -First 1
$probeVer  = if ($probeExe) { (& $probeExe '--version' 2>&1 | Out-String).Trim() } else { 'not-built' }

# --- Golden coverage table -------------------------------------------------
$rows = @()
$tested = 0; $skipped = 0
if ($golden) {
    foreach ($f in $golden.files) {
        $name = Split-Path $f.path -Leaf
        if ($null -ne $f.agree) {
            $mark = if ($f.agree) { 'PASS' } else { 'FAIL' }
            $tested++
        } else {
            $mark = 'n/a (non-FLAC)'
            $skipped++
        }
        $rows += "| ``$name`` | $($f.sampleRate) Hz / $($f.bits)-bit / $($f.channels)ch | $mark | $($f.fb2kStatus) |"
    }
}

$stamp      = (Get-Date).ToString('yyyyMMdd-HHmmss')
$reportPath = Join-Path $ReportDir "$Phase-$stamp.md"

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine("# Parity report - $Phase")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| Field | Value |')
[void]$sb.AppendLine('|---|---|')
[void]$sb.AppendLine("| Generated (UTC) | $((Get-Date).ToUniversalTime().ToString('o')) |")
[void]$sb.AppendLine("| Commit | $commit (branch $branch) |")
[void]$sb.AppendLine("| Corpus manifest hash | $corpusHash |")
[void]$sb.AppendLine("| Golden manifest | $(if ($GoldenManifest) { Split-Path $GoldenManifest -Leaf } else { 'none' }) |")
[void]$sb.AppendLine("| Machine | $env:COMPUTERNAME |")
[void]$sb.AppendLine("| Device | $Device |")
[void]$sb.AppendLine("| flac | $flacVer |")
[void]$sb.AppendLine("| ffmpeg | $($ffVer -replace '\|','/') |")
[void]$sb.AppendLine("| lyra-probe | $probeVer |")
[void]$sb.AppendLine("| Workspace build | $buildStatus |")
[void]$sb.AppendLine("| Files tested / skipped | $tested / $skipped |")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('## Golden coverage (libFLAC vs ffmpeg agreement)')
[void]$sb.AppendLine('')
if ($rows.Count -gt 0) {
    [void]$sb.AppendLine('| File | Spec | libFLAC=ffmpeg | fb2k golden |')
    [void]$sb.AppendLine('|---|---|---|---|')
    foreach ($r in $rows) { [void]$sb.AppendLine($r) }
} else {
    [void]$sb.AppendLine($goldenSummary)
}
[void]$sb.AppendLine('')
[void]$sb.AppendLine('## Pending human')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('- foobar2000 Converter golden (`LyraGolden` preset) - not run (ADR check `GET /api/player` first).')
[void]$sb.AppendLine('- Loopback A/B and DAC indicator checks (TEST-STRATEGY §5).')
[void]$sb.AppendLine('')
$gate = if ($buildStatus -eq 'FAIL' -or ($rows | Where-Object { $_ -match '\| FAIL \|' })) { 'FAIL' } else { 'PASS (skeleton)' }
[void]$sb.AppendLine("## Gate: $gate")

Set-Content -LiteralPath $reportPath -Value $sb.ToString() -Encoding UTF8
Write-Host "run.ps1: wrote report $reportPath" -ForegroundColor Cyan
$reportPath
