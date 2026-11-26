<#
.SYNOPSIS
    P8.3 performance footprint (TEST-STRATEGY §7). Measures Lyra's own CPU% and
    working set playing a 24/192 FLAC and a DSD64 file. The "<= foobar2000 x 1.2"
    comparison needs foobar2000 in WASAPI exclusive, which must NOT be launched
    unattended (H2.5) -> the comparison is pending-human; this records Lyra's
    absolute numbers.

.DESCRIPTION
    Obeys the Sound rule: Beefweb must not be playing; mutes the endpoint first and
    restores the previous mute state in a finally. Plays each file muted at
    real-time through lyra-play (WASAPI exclusive) for -Seconds, sampling the
    process CPU time and working set every second. If the device is busy it falls
    back to --offline (decode-throughput, NOT a real-time CPU figure) and flags it.
    Writes target/parity/p8/perf.json. Nothing committed except this script.

.PARAMETER Seconds   Measurement window per file (default 60; ADR-012 fast budget).
.PARAMETER Offline   Force the offline path.
#>
[CmdletBinding()]
param(
    [int]$Seconds = 60,
    [switch]$Offline,
    [string]$GoldenManifest
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
$OutDir      = Join-Path $RepoRoot 'target\parity\p8'
$TmpDir      = Join-Path $RepoRoot 'target\corpus-tmp\p8'
foreach ($d in @($OutDir, $TmpDir)) { if (-not (Test-Path $d)) { New-Item -ItemType Directory -Force -Path $d | Out-Null } }

function Resolve-Exe([string]$name) {
    foreach ($cfg in @('release', 'debug')) {
        $p = Join-Path $RepoRoot "target\$cfg\$name.exe"
        if (Test-Path $p) { return $p }
    }
    throw "$name.exe not found; build it first (cargo build --release)."
}
$Probe = Resolve-Exe 'lyra-probe'
$Play  = Resolve-Exe 'lyra-play'

if (-not $GoldenManifest) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' | Sort-Object Name -Descending | Select-Object -First 1
    $GoldenManifest = $latest.FullName
}
$manifest = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json

function Get-BeefwebState {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri 'http://localhost:8880/api/player' -ErrorAction Stop
        return (($r.Content | ConvertFrom-Json).player.playbackState)
    } catch { return 'unreachable' }
}
$bwState = Get-BeefwebState
if ($bwState -eq 'playing') { throw "Beefweb reports 'playing' - refusing to render (Sound rule)." }

# Targets: the sole 24/192 FLAC and a DSD64 DFF.
$flac192 = @($manifest.files | Where-Object { $_.ext -eq 'flac' -and $_.sampleRate -eq 192000 } | Select-Object -First 1)
$dsd = @($manifest.files | Where-Object { $_.ext -eq 'dff' } | Select-Object -First 1)
$targets = @()
if ($flac192.Count -gt 0) { $targets += [pscustomobject]@{ Tag='FLAC 24/192'; Path=$flac192[0].path } }
if ($dsd.Count -gt 0)     { $targets += [pscustomobject]@{ Tag='DSD64 DoP/PCM'; Path=$dsd[0].path } }
if ($targets.Count -eq 0) { throw 'no 24/192 FLAC or DSD file in the manifest.' }

$nproc = [int]$env:NUMBER_OF_PROCESSORS; if ($nproc -lt 1) { $nproc = 4 }
$prevMute = $null
$rows = @()

function Measure-One($tag, $path, [bool]$offlineMode) {
    $o = Join-Path $TmpDir 'perf_offline.raw'
    $out = Join-Path $TmpDir 'perf.out'; $err = Join-Path $TmpDir 'perf.err'
    $a = @()
    if ($offlineMode) { $a += @('--offline', $o) }
    # Quote: Windows PowerShell 5.1 Start-Process does not quote array args with spaces (corpus paths have spaces).
    $a += @('--mode','exclusive','--max-seconds', "$Seconds", ('"' + $path + '"'))
    $p = Start-Process -FilePath $Play -ArgumentList $a -PassThru -RedirectStandardOutput $out -RedirectStandardError $err -WindowStyle Hidden
    $null = $p.Handle   # cache the handle so ExitCode is available after exit
    $cpuSamples = New-Object System.Collections.Generic.List[double]
    $wsSamples = New-Object System.Collections.Generic.List[double]
    $lastCpu = $null; $lastT = Get-Date
    $deadline = (Get-Date).AddSeconds($Seconds + 15)
    while (-not $p.HasExited -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 1000
        try {
            $p.Refresh()
            if ($p.HasExited) { break }
            $now = Get-Date
            $cpu = $p.TotalProcessorTime.TotalSeconds
            $ws = [math]::Round($p.WorkingSet64 / 1MB, 2)
            if ($null -ne $lastCpu) {
                $wall = ($now - $lastT).TotalSeconds
                if ($wall -gt 0) {
                    $pct = (($cpu - $lastCpu) / $wall) / $nproc * 100
                    $cpuSamples.Add([math]::Round($pct, 2))
                }
            }
            $wsSamples.Add($ws)
            $lastCpu = $cpu; $lastT = $now
        } catch {}
    }
    if (-not $p.HasExited) { try { $p.Kill() } catch {} }
    $p.WaitForExit()
    $stdout = (Get-Content -LiteralPath $out -Raw -ErrorAction SilentlyContinue)
    $stderr = (Get-Content -LiteralPath $err -Raw -ErrorAction SilentlyContinue)
    $bitPerfect = ($stdout -match 'bit-perfect' -and $stdout -notmatch 'NOT bit-perfect')
    $errCount = ([regex]::Match($stdout, 'errors:\s*(\d+)')).Groups[1].Value
    $fmtLine = ([regex]::Match($stdout, 'Format:.*')).Value
    $doneM = [regex]::Match($stdout, 'underruns:\s*(\d+),\s*errors:\s*(\d+)')
    $cpuArr = @($cpuSamples)
    $avgCpu = if ($cpuArr.Count) { [math]::Round((($cpuArr | Measure-Object -Average).Average), 2) } else { $null }
    $p99Cpu = if ($cpuArr.Count) {
        $sorted = $cpuArr | Sort-Object
        $idx = [math]::Min($sorted.Count - 1, [int][math]::Ceiling($sorted.Count * 0.99) - 1)
        [math]::Round($sorted[$idx], 2)
    } else { $null }
    $wsMax = if ($wsSamples.Count) { ($wsSamples | Measure-Object -Maximum).Maximum } else { $null }
    return [pscustomobject]@{
        Tag = $tag; File = (Split-Path $path -Leaf); Mode = if ($offlineMode) { 'offline (decode-throughput, not real-time)' } else { 'real-device muted (real-time)' }
        Samples = $cpuArr.Count; AvgCpuPct = $avgCpu; P99CpuPct = $p99Cpu; WorkingSetMaxMB = $wsMax
        BitPerfect = $bitPerfect; Format = $fmtLine; ExitCode = $p.ExitCode
        Underruns = if ($doneM.Success) { [int]$doneM.Groups[1].Value } else { $null }
        Errors = if ($errCount -ne '') { [int]$errCount } else { $null }   # >0 = the run did not actually play; figures invalid
        # -join first: $stderr may be $null or an array; .Trim() on Object[] crashed the 2026-11-26 run.
        Note = ([string]((@($stderr) -join ' ') -replace '\s+',' ')).Trim()
    }
}

try {
    $useOffline = [bool]$Offline
    if (-not $useOffline) {
        try { $m = (& $Probe mute on 2>$null | Out-String | ConvertFrom-Json); $prevMute = $m.previousMute } catch { $prevMute = $null }
        # quick device-open test
        $tp = Start-Process -FilePath $Play -ArgumentList @('--mode','exclusive','--max-seconds','3', ('"' + $targets[0].Path + '"')) -PassThru -RedirectStandardOutput (Join-Path $TmpDir 'pt.out') -RedirectStandardError (Join-Path $TmpDir 'pt.err') -WindowStyle Hidden
        $tp.WaitForExit()
        if ($tp.ExitCode -ne 0) {
            $e = (Get-Content -LiteralPath (Join-Path $TmpDir 'pt.err') -Raw -ErrorAction SilentlyContinue)
            if ($e -match 'DEVICE_IN_USE|in use|busy') { $useOffline = $true }
        }
    }
    foreach ($t in $targets) { $rows += (Measure-One $t.Tag $t.Path $useOffline) }
}
finally {
    if ($null -ne $prevMute) { try { & $Probe mute ($(if ($prevMute) { 'on' } else { 'off' })) 2>$null | Out-Null } catch {} }
}

$result = [ordered]@{
    generated = (Get-Date).ToUniversalTime().ToString('o')
    beefwebState = $bwState
    windowSeconds = $Seconds
    cpuCount = $nproc
    corpusHash = $manifest.corpusHash
    fb2kComparison = 'pending-human (H2.5): foobar2000 not launched unattended'
    rows = $rows
}
$json = $result | ConvertTo-Json -Depth 5
Set-Content -LiteralPath (Join-Path $OutDir 'perf.json') -Value $json -Encoding UTF8
Write-Host $json
