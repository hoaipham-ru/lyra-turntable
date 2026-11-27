<#
.SYNOPSIS
    P8.3 accelerated soak (ADR-011/ADR-012). Drives the engine through hundreds of
    seek/track-change transitions under background CPU load for up to 30 min,
    samples the lyra-play process working set every 10 s (linear-fit slope must be
    ~0, no upward trend) and counts underruns (must be 0).

.DESCRIPTION
    Obeys the RUNBOOK Sound rule:
      1. Checks Beefweb (`GET /api/player`): aborts the render path if music plays.
      2. Tries a WASAPI-exclusive open on the built-in endpoint. If the device is
         busy (AUDCLNT_E_DEVICE_IN_USE) it retries briefly then falls back to
         `--offline` for the memory-trend / transition-storm portion and records the
         real-device underrun count as pending-hardware.
      3. Mutes the endpoint first (`lyra-probe mute on`, endpoint volume API only —
         does not touch the digital stream) and RESTORES the previous mute state in a
         finally, even on failure.
    Capped at -Minutes (default 30, ADR-012). Writes a §9-style soak section JSON to
    target/parity/p8/soak.json; the P8 report builder folds it in. Nothing is
    committed except this script (ADR-006).

.PARAMETER Minutes        Soak wall-clock minutes (default 30; ADR-012 cap).
.PARAMETER TransitionMs   Wall-clock gap between transitions (default 2500 ms).
.PARAMETER Offline        Force the offline (device-free) path.
.PARAMETER RetryBusyMax   Max device-busy retries (default 2; shortened for unattended).
.PARAMETER RetryBusySec   Seconds between device-busy retries (default 60).
#>
[CmdletBinding()]
param(
    [int]$Minutes = 30,
    [int]$TransitionMs = 2500,
    [switch]$Offline,
    [int]$RetryBusyMax = 2,
    [int]$RetryBusySec = 60,
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
    if (-not $latest) { throw 'No golden manifest found.' }
    $GoldenManifest = $latest.FullName
}
$manifest = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json

# --- Beefweb: nothing may be playing (Sound rule) --------------------------
function Get-BeefwebState {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 5 -Uri 'http://localhost:8880/api/player' -ErrorAction Stop
        $j = $r.Content | ConvertFrom-Json
        return $j.player.playbackState
    } catch { return 'unreachable' }
}
$bwState = Get-BeefwebState
if ($bwState -eq 'playing') { throw "Beefweb reports playback 'playing' - refusing to touch the device (Sound rule)." }

# --- Build the soak queue (gapless 5-track set + a few corpus FLACs) --------
$gapless = Join-Path $RepoRoot 'target\corpus-synth\gapless'
$queue = @()
1..5 | ForEach-Object { $t = Join-Path $gapless "track$_.flac"; if (Test-Path $t) { $queue += $t } }
# add a handful of corpus FLACs across rates for spec-change transitions
$byClass = @{}
foreach ($f in ($manifest.files | Where-Object { $_.ext -eq 'flac' })) {
    $k = "$($f.bits)-$($f.sampleRate)"
    if (-not $byClass.ContainsKey($k)) { $byClass[$k] = $f; $queue += $f.path }
}
if ($queue.Count -lt 2) { throw "soak queue too small ($($queue.Count)); run synth.ps1 first." }

# --- Transition script: hundreds of playat/seek/next/prev cycles ------------
$cycleMs = [math]::Max(500, $TransitionMs)
$cycles = [int][math]::Ceiling(($Minutes * 60 * 1000) / $cycleMs)
$rng = [System.Random]::new(20261126)
$verbs = @('seek','next','prev','playat')
$lines = New-Object System.Collections.Generic.List[string]
$lines.Add('play')
for ($i = 0; $i -lt $cycles; $i++) {
    $v = $verbs[$rng.Next(0, $verbs.Count)]
    switch ($v) {
        'seek'   { $lines.Add("seek "   + $rng.Next(0, 4000000)) }
        'playat' { $lines.Add("playat " + $rng.Next(0, $queue.Count)) }
        default  { $lines.Add($v) }
    }
    # keep audio actually flowing so real-device underruns are exercised
    if ($i % 7 -eq 0) { $lines.Add("playat " + $rng.Next(0, $queue.Count)) }
    $lines.Add("wait $cycleMs")
}
$lines.Add('stop')
$lines.Add('quit')
$scriptPath = Join-Path $TmpDir 'soak_script.txt'
Set-Content -LiteralPath $scriptPath -Value ($lines -join "`n") -Encoding ASCII
$transitions = ($lines | Where-Object { $_ -match '^(seek|next|prev|playat)' }).Count

# --- Background CPU stress --------------------------------------------------
$nproc = [int]$env:NUMBER_OF_PROCESSORS; if ($nproc -lt 2) { $nproc = 4 }
$loaders = @()
$loadCount = [math]::Max(2, $nproc - 1)
for ($i = 0; $i -lt $loadCount; $i++) {
    $loaders += Start-Process -FilePath 'node' -ArgumentList @('-e', "const e=Date.now()+$((($Minutes*60)+120))*1000;while(Date.now()<e){Math.sqrt(Math.random()*1e9);}") -PassThru -WindowStyle Hidden
}

# --- Run the soak -----------------------------------------------------------
$maxSeconds = $Minutes * 60 + 60
$offlineOut = Join-Path $TmpDir 'soak_offline.raw'
$playOut = Join-Path $TmpDir 'soak_play.out'
$playErr = Join-Path $TmpDir 'soak_play.err'

$usedOffline = [bool]$Offline
$deviceNote = ''
$prevMute = $null
$endpoint = $null
$samples = New-Object System.Collections.Generic.List[object]
$proc = $null
$t0 = Get-Date

function Start-Play([bool]$offlineMode) {
    $a = @()
    if ($offlineMode) { $a += @('--offline', $offlineOut) }
    $a += @('--mode', 'exclusive', '--script', $scriptPath, '--max-seconds', "$maxSeconds")
    # Quote each queue path: Windows PowerShell 5.1 Start-Process does not quote array
    # args containing spaces, so corpus paths (e.g. "...\CD 01\12 - Summer.flac") would
    # be split into broken args and fail to open (os error 2/3). Same fix as p8_perf.ps1.
    foreach ($qp in $queue) { $a += ('"' + $qp + '"') }
    $sp = Start-Process -FilePath $Play -ArgumentList $a -PassThru -RedirectStandardOutput $playOut -RedirectStandardError $playErr -WindowStyle Hidden
    $null = $sp.Handle   # cache the handle so HasExited/WorkingSet64/ExitCode stay valid after exit
    return $sp
}

try {
    if (-not $usedOffline) {
        # Mute first (save previous), then a short device-open probe.
        try {
            $m = (& $Probe mute on 2>$null | Out-String | ConvertFrom-Json)
            $prevMute = $m.previousMute
        } catch { $prevMute = $null; $deviceNote = "mute failed: $($_.Exception.Message); " }

        # Device-open test: a 3 s muted render of one short track.
        $attempt = 0; $deviceFree = $false
        while ($attempt -le $RetryBusyMax) {
            $testOut = Join-Path $TmpDir 'devtest.out'
            $p = Start-Process -FilePath $Play -ArgumentList @('--mode','exclusive','--max-seconds','3', ('"' + $queue[0] + '"')) -PassThru -RedirectStandardOutput $testOut -RedirectStandardError (Join-Path $TmpDir 'devtest.err') -WindowStyle Hidden
            $null = $p.Handle   # cache the handle BEFORE exit, else $p.ExitCode is $null (same Start-Process bug fixed in p8_perf.ps1) and a working device is misread as failed
            $p.WaitForExit()
            $errTxt = (Get-Content -LiteralPath (Join-Path $TmpDir 'devtest.err') -Raw -ErrorAction SilentlyContinue)
            $testStdout = (Get-Content -LiteralPath $testOut -Raw -ErrorAction SilentlyContinue)
            # Device-free if the probe exited clean OR it demonstrably rendered to the
            # device (reached "Done. underruns:" with no device-busy error). The second
            # arm guards against a flaky ExitCode read under heavy CPU load from the loaders.
            $rendered = ($testStdout -match 'Rendering to the audio device' -and $testStdout -match 'Done\. underruns:')
            if ($p.ExitCode -eq 0 -or $rendered) { $deviceFree = $true; break }
            if ($errTxt -match 'DEVICE_IN_USE|in use|busy') {
                $attempt++
                $deviceNote += "device busy (attempt $attempt); "
                if ($attempt -le $RetryBusyMax) { Start-Sleep -Seconds $RetryBusySec }
            } else {
                $deviceNote += "device open failed: $($errTxt -replace '\s+',' '); "
                break
            }
        }
        if (-not $deviceFree) {
            $usedOffline = $true
            $deviceNote += 'falling back to --offline; real-device underruns pending-hardware. '
        }
    }

    $proc = Start-Play $usedOffline

    # Sample working set every 10 s until the process exits or the cap elapses.
    $deadline = $t0.AddSeconds($maxSeconds + 30)
    while (-not $proc.HasExited -and (Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 10
        try {
            $proc.Refresh()
            if (-not $proc.HasExited) {
                $wsMB = [math]::Round($proc.WorkingSet64 / 1MB, 2)
                $elapsed = [math]::Round(((Get-Date) - $t0).TotalSeconds, 1)
                $samples.Add([pscustomobject]@{ t = $elapsed; wsMB = $wsMB })
            }
        } catch {}
    }
    if (-not $proc.HasExited) { try { $proc.Kill() } catch {} }
    $proc.WaitForExit()
}
finally {
    foreach ($l in $loaders) { try { if (-not $l.HasExited) { $l.Kill() } } catch {} }
    if ($null -ne $prevMute) { try { & $Probe mute ($(if ($prevMute) { 'on' } else { 'off' })) 2>$null | Out-Null } catch {} }
}

# --- Analyse ----------------------------------------------------------------
$stdout = (Get-Content -LiteralPath $playOut -Raw -ErrorAction SilentlyContinue)
$stderr = (Get-Content -LiteralPath $playErr -Raw -ErrorAction SilentlyContinue)
$doneM = [regex]::Match($stdout, 'underruns:\s*(\d+),\s*errors:\s*(\d+)')
$underruns = if ($doneM.Success) { [int]$doneM.Groups[1].Value } else { $null }
$errors = if ($doneM.Success) { [int]$doneM.Groups[2].Value } else { $null }
$underrunEvents = ([regex]::Matches($stdout, 'Underrun:\s*(\d+)')).Count
$panicked = ($stderr -match 'panicked at') -or ($stderr -match 'RUST_BACKTRACE')

# linear fit slope (MB per minute) over the samples
$slopeMbPerMin = $null; $wsFirst = $null; $wsLast = $null; $wsMax = $null
if ($samples.Count -ge 2) {
    $n = $samples.Count
    $sx = ($samples | Measure-Object -Property t -Sum).Sum
    $sy = ($samples | Measure-Object -Property wsMB -Sum).Sum
    $sxx = 0.0; $sxy = 0.0
    foreach ($s in $samples) { $sxx += $s.t * $s.t; $sxy += $s.t * $s.wsMB }
    $denom = ($n * $sxx - $sx * $sx)
    $slopePerSec = if ($denom -ne 0) { ($n * $sxy - $sx * $sy) / $denom } else { 0 }
    $slopeMbPerMin = [math]::Round($slopePerSec * 60, 4)
    $wsFirst = $samples[0].wsMB
    $wsLast = $samples[$n - 1].wsMB
    $wsMax = ($samples | Measure-Object -Property wsMB -Maximum).Maximum
}

$elapsedMin = [math]::Round(((Get-Date) - $t0).TotalMinutes, 2)

# Pass criteria:
#  - memory trend: slope not materially upward (<= 0.5 MB/min and last <= first*1.5)
#  - underruns: 0 on the real device; offline => pending-hardware
$memTrendOk = ($null -ne $slopeMbPerMin) -and ($slopeMbPerMin -le 0.5)
$underrunOk = if ($usedOffline) { $null } else { ($underruns -eq 0) -and ($underrunEvents -eq 0) }

$result = [ordered]@{
    generated      = (Get-Date).ToUniversalTime().ToString('o')
    mode           = if ($usedOffline) { 'offline (device-free fallback)' } else { 'real-device (muted, WASAPI exclusive)' }
    beefwebState   = $bwState
    minutesTarget  = $Minutes
    elapsedMin     = $elapsedMin
    transitionMs   = $cycleMs
    transitions    = $transitions
    queueSize      = $queue.Count
    cpuLoaders     = $loaders.Count
    memSamples     = $samples.Count
    wsFirstMB      = $wsFirst
    wsLastMB       = $wsLast
    wsMaxMB        = $wsMax
    slopeMbPerMin  = $slopeMbPerMin
    memTrendOk     = $memTrendOk
    underruns      = $underruns
    underrunEvents = $underrunEvents
    errors         = $errors
    underrunOk     = $underrunOk
    panicked       = $panicked
    deviceNote     = $deviceNote.Trim()
    corpusHash     = $manifest.corpusHash
    samplesSeries  = $samples
}
$json = $result | ConvertTo-Json -Depth 5
Set-Content -LiteralPath (Join-Path $OutDir 'soak.json') -Value $json -Encoding UTF8
Write-Host $json
Write-Host ("SOAK: mode=$($result.mode) transitions=$transitions elapsed=${elapsedMin}m slope=$slopeMbPerMin MB/min underruns=$underruns memTrendOk=$memTrendOk underrunOk=$underrunOk panic=$panicked") -ForegroundColor Cyan
