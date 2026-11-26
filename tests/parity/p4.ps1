<#
.SYNOPSIS
    P4.6 device-free substitute for "tap through the app == golden".

.DESCRIPTION
    The native backend (NativeBackend.ts) drives the merged `lyra-engine` through
    the Tauri `engine_*` commands. `lyra-play --offline` runs that SAME engine queue
    path with no audio device, producing exactly the bytes a device tap would record
    (ADR-004). This script renders 5 representative corpus files offline and proves
    each rendered span is bit-identical to an INDEPENDENT ffmpeg golden decode, which
    demonstrates the engine the app uses produces golden bytes.

    The real "through the running Tauri app with a muted device tap" remains a
    pending-human / pending-hardware item (needs the free render device + a human to
    drive the GUI, or the P2 muted tap when the device is free).

    Device-free and silent: no audio device is opened, so the sound rule does not
    apply. Reads the read-only corpus; writes only under target/parity/.

.PARAMETER MaxSeconds
    Per-file offline render cap (ADR-012 fast budget). Default 25.
#>
[CmdletBinding()]
param(
    [int]$MaxSeconds = 25,
    [string]$GoldenManifest
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
$TmpDir      = Join-Path $RepoRoot 'target\parity\p4-offline'
if (Test-Path $TmpDir) { Remove-Item -Recurse -Force $TmpDir }
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

function Resolve-Exe([string]$name) {
    foreach ($cfg in @('release', 'debug')) {
        $p = Join-Path $RepoRoot "target\$cfg\$name.exe"
        if (Test-Path $p) { return $p }
    }
    throw "$name.exe not found; build it first (cargo build --release -p $name)."
}
$Play  = Resolve-Exe 'lyra-play'
$Probe = Resolve-Exe 'lyra-probe'

# --- golden manifest + corpus-unchanged check (before) ---------------------
if (-not $GoldenManifest) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' |
        Sort-Object Name -Descending | Select-Object -First 1
    if (-not $latest) { throw 'No golden manifest found.' }
    $GoldenManifest = $latest.FullName
}
$manifest = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json

function Get-CorpusHash {
    # Same formula as golden.ps1 / p3.ps1: path|length|LastWriteTimeUtc('o').
    $lines = $manifest.files | ForEach-Object {
        $fi = Get-Item -LiteralPath $_.path -ErrorAction SilentlyContinue
        if ($fi) { "$($_.path)|$($fi.Length)|$($fi.LastWriteTimeUtc.ToString('o'))" } else { "$($_.path)|MISSING" }
    }
    [System.BitConverter]::ToString(
        [System.Security.Cryptography.SHA256]::Create().ComputeHash(
            [System.Text.Encoding]::UTF8.GetBytes(($lines -join "`n")))).Replace('-', '').ToLower()
}
$corpusBefore = Get-CorpusHash

# --- pick 5 representatives (incl. 16/44.1 and 24/192) ---------------------
$specs = @(@(44100, 16), @(44100, 24), @(48000, 24), @(96000, 24), @(192000, 24))
$picks = foreach ($s in $specs) {
    @($manifest.files | Where-Object { $_.ext -eq 'flac' -and $_.sampleRate -eq $s[0] -and $_.bits -eq $s[1] })[0]
}

$results = @()
$i = 0
foreach ($f in $picks) {
    $i++
    if (-not $f) { $results += [pscustomobject]@{ Spec = '(missing)'; Status = 'FAIL'; Note = 'no corpus file for spec' }; continue }
    $spec = "$($f.sampleRate)/$($f.bits)"
    $raw  = Join-Path $TmpDir ("offline_{0}.raw" -f $i)
    Write-Host "P4: offline render $spec  $(Split-Path $f.path -Leaf) ..." -ForegroundColor Cyan

    $playLog = Join-Path $TmpDir ("offline_{0}.log" -f $i)
    & $Play '--offline' $raw '--max-seconds' "$MaxSeconds" $f.path *>&1 |
        Out-File -LiteralPath $playLog -Encoding UTF8
    $fmtLine = (Select-String -LiteralPath $playLog -Pattern '^Format:' | Select-Object -First 1).Line
    if (-not $fmtLine) { $fmtLine = '' }
    $bitPerfect = ($fmtLine -match 'bit-perfect') -and ($fmtLine -notmatch 'NOT bit-perfect')

    if (-not (Test-Path $raw) -or (Get-Item $raw).Length -eq 0) {
        $results += [pscustomobject]@{ Spec = $spec; File = (Split-Path $f.path -Leaf); Status = 'FAIL'; Note = "no offline output (see $playLog)" }
        continue
    }

    $rawFmt = if ($f.bits -le 16) { 's16' } else { 's24' }
    $bits   = if ($f.bits -le 16) { 16 } else { 24 }
    $ch     = $f.channels

    # Independent golden: ffmpeg decode of the file (A) vs the offline render (B).
    $cmpJson = & $Probe 'compare' $f.path $raw '--engine' 'ffmpeg' '--raw-format' $rawFmt '--channels' "$ch" '--bits' "$bits" 2>&1
    $cmp = $null
    try { $cmp = ($cmpJson | Out-String | ConvertFrom-Json) } catch {}

    if (-not $cmp -or -not $cmp.result) {
        $results += [pscustomobject]@{ Spec = $spec; File = (Split-Path $f.path -Leaf); Status = 'FAIL'; Note = "compare produced no JSON: $cmpJson" }
        continue
    }
    $r = $cmp.result

    $firstDiff = $r.first_diff_frame
    $spanOk = ($null -eq $firstDiff) -and ($r.max_abs_diff -eq 0) -and ($r.frames_b -gt 0)
    $status = if ($spanOk -and $bitPerfect) { 'PASS' } else { 'FAIL' }
    $note = "offline {0}/{1} frames (full={2}), span bit-exact={3}, maxAbsDiff={4}, firstDiff={5}, bit-perfect={6}, {7}" -f `
        $r.frames_b, $r.frames_a, $r.bit_exact, $spanOk, $r.max_abs_diff, ($(if ($null -eq $firstDiff) { 'none' } else { $firstDiff })), $bitPerfect, ($fmtLine)
    $results += [pscustomobject]@{ Spec = $spec; File = (Split-Path $f.path -Leaf); Status = $status; Note = $note }
}

$corpusAfter = Get-CorpusHash
$corpusUnchanged = ($corpusBefore -eq $corpusAfter) -and ($corpusBefore -eq $manifest.corpusHash)

Write-Host ''
Write-Host '=== P4.6 offline==golden results ===' -ForegroundColor Green
$results | ForEach-Object { "{0,-10} {1,-6} {2}" -f $_.Spec, $_.Status, $_.Note } | Write-Host
$pass = @($results | Where-Object { $_.Status -eq 'PASS' }).Count
$total = $results.Count
Write-Host ("PASS {0}/{1}; corpus unchanged: {2} (hash {3})" -f $pass, $total, $corpusUnchanged, $manifest.corpusHash) -ForegroundColor Green

if ($pass -ne $total -or -not $corpusUnchanged) { exit 1 } else { exit 0 }
