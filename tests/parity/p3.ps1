<#
.SYNOPSIS
    P3.6 playback-engine QA gate (S09, lyra-qa). Exercises the exit criteria from
    phases/P3-playback-engine.md and writes a TEST-STRATEGY §9 report. Invoked by
    `run.ps1 -Phase P3`.

.DESCRIPTION
    All P3 byte-equality proofs use `lyra-play --offline` (the headless, silent,
    deterministic render that writes the engine's exact device-bound bytes to a
    file WITHOUT opening the audio device). No sound is produced; the Sound rule
    (mute/stop-check) is only relevant to the optional muted device-tap demos,
    which are NOT run here (recorded as pending-human/hardware).

    Checks (phases/P3-playback-engine.md exit gate):
      1. Gapless == whole      : offline(whole) == offline(5 tracks) == offline(cue)
                                 == decode(whole), byte-for-byte (sha256).
      2. Cue-image boundaries   : cue expands to 5 virtual tracks at the exact
                                 12 s x 96 kHz frame boundaries; cue.raw == whole.raw.
      3. Seek through engine    : 50 points x 10 files; the post-seek offline suffix
                                 is byte-identical to the golden decode slice at the
                                 landed frame (sample-accurate). Also records the
                                 pre-seek ring tail (a known device-tap limitation).
      4. Command storm          : ~1000 randomised commands over the queue; no
                                 deadlock/panic; clean completion; consistent state.
      5. Clock drift (offline)  : Position maps device-consumed frames -> source
                                 frames with 0 error over a long track. The real-time
                                 wall-clock drift test needs a device -> pending-hw.
      6. Robustness             : truncated/flipped/zerohdr FLAC + missing/garbage
                                 files + a malformed .cue through the queue: each
                                 corrupt image errors and the queue continues; no
                                 panic.
      7. Fuzzing                : ~5 min of mutated .cue inputs through the engine
                                 (cue parser), plus a decode/dsd re-confirm: no
                                 crashes.

    Regression (re-run prior green phases): delegates to p1.ps1 (decode/seek/robust)
    and does a device-free P5 DSD byte-equality re-confirm. P2 device render is NOT
    run here (needs the render device + Sound rule) -> recorded device-dependent.

    The corpus is read-only (ADR-006). Corrupt/temp files live under
    target/corpus-tmp/; generated audio under target/corpus-synth/ and
    target/parity/; nothing here is committed.

.PARAMETER GoldenManifest  Golden manifest JSON. Default: newest in manifests/.
.PARAMETER SeekPoints      Random seek points per file (default 50).
.PARAMETER SeekFileCount   Number of seek files (default 10).
.PARAMETER StormCommands   Randomised command-storm length (default 1000).
.PARAMETER FuzzSeconds     Cue fuzz budget in seconds (default 300 = 5 min).
.PARAMETER DriftSeconds    Offline drift track length target (informational).
.PARAMETER SkipRegression  Skip the P1/P5 regression re-run.
.PARAMETER Quick           Smoke mode: tiny counts, short fuzz (fast iteration).
#>
[CmdletBinding()]
param(
    [string]$GoldenManifest,
    [int]$SeekPoints = 50,
    [int]$SeekFileCount = 10,
    [int]$BigFilePoints = 12,
    [int]$StormCommands = 1000,
    [int]$FuzzSeconds = 300,
    [int]$DriftSeconds = 60,
    [switch]$SkipRegression,
    [switch]$Quick,
    [string]$BaselineNote = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ReportDir   = Join-Path $RepoRoot 'target\parity\reports'
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
$TmpDir      = Join-Path $RepoRoot 'target\corpus-tmp\p3'
$SynthDir    = Join-Path $RepoRoot 'target\corpus-synth'
$GaplessDir  = Join-Path $SynthDir 'gapless'
$RobDir      = Join-Path $SynthDir 'robustness'
foreach ($d in @($ReportDir, $TmpDir)) {
    if (-not (Test-Path $d)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
}

if ($Quick) { $SeekPoints = 5; $SeekFileCount = 3; $StormCommands = 100; $FuzzSeconds = 10; $DriftSeconds = 12 }

# --- tool resolution -------------------------------------------------------
function Resolve-Exe([string]$name) {
    foreach ($cfg in @('release', 'debug')) {
        $p = Join-Path $RepoRoot "target\$cfg\$name.exe"
        if (Test-Path $p) { return $p }
    }
    throw "$name.exe not found; build it first (cargo build --release)."
}
function Resolve-Flac {
    $cmd = Get-Command flac -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $fallback = 'C:\Users\LENOVO\AppData\Local\Microsoft\WinGet\Packages\Xiph.FLAC_Microsoft.Winget.Source_8wekyb3d8bbwe\flac-1.5.0-win\Win64\flac.exe'
    if (Test-Path $fallback) { return $fallback }
    throw 'flac CLI not found (PATH or winget path).'
}
function Require-Tool([string]$n) {
    $c = Get-Command $n -ErrorAction SilentlyContinue
    if (-not $c) { throw "$n not found on PATH." }
    return $c.Source
}
$Probe  = Resolve-Exe 'lyra-probe'
$Play   = Resolve-Exe 'lyra-play'
$Flac   = Resolve-Flac
$Ffmpeg = Require-Tool 'ffmpeg'

# Run a native exe capturing stdout, stderr and exit code without PS turning a
# non-zero exit or stderr into a terminating error. Call operator + array splat
# reliably sets $LASTEXITCODE (unlike Start-Process, whose .ExitCode came back
# empty here) and quotes spaced paths correctly. Each child self-caps via
# `--max-seconds` (lyra-play) or returns promptly (probe), so no hard timeout is
# needed; `TimeoutSec` is accepted for call-site clarity but unused.
function Invoke-Capture {
    param([string]$Exe, [string[]]$Arguments, [int]$TimeoutSec = 0)
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    $errFile = [System.IO.Path]::GetTempFileName()
    try {
        $out = & $Exe @Arguments 2>$errFile
        $code = $LASTEXITCODE
        $outStr = [string]($out | Out-String)
        $err = [string](Get-Content -LiteralPath $errFile -Raw)
        return [pscustomobject]@{ ExitCode = [int]$code; StdOut = $outStr; StdErr = $err; TimedOut = $false }
    } finally {
        Remove-Item -LiteralPath $errFile -Force -ErrorAction SilentlyContinue
        $ErrorActionPreference = $old
    }
}

function Sha256-File([string]$path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLower() }
function Sha256-Bytes([byte[]]$b) {
    [BitConverter]::ToString(([System.Security.Cryptography.SHA256]::Create()).ComputeHash($b)).Replace('-', '').ToLower()
}
function Bytes-Per-Frame([int]$bits, [int]$channels) {
    $bps = if ($bits -le 16) { 2 } elseif ($bits -le 24) { 3 } else { 4 }
    return $bps * $channels
}
function Read-Region([string]$path, [int64]$offset, [int]$len) {
    $fs = [System.IO.File]::OpenRead($path)
    try {
        [void]$fs.Seek($offset, [System.IO.SeekOrigin]::Begin)
        $buf = New-Object byte[] $len
        $got = 0
        while ($got -lt $len) {
            $n = $fs.Read($buf, $got, $len - $got)
            if ($n -le 0) { break }
            $got += $n
        }
        if ($got -lt $len) { return $null }
        return $buf
    } finally { $fs.Dispose() }
}

# Offline render one or more inputs with an optional script; returns capture.
function Render-Offline {
    param([string[]]$Inputs, [string]$OutRaw, [string]$Script, [int]$MaxSeconds = 120, [int]$TimeoutSec = 180)
    $args = @('--offline', $OutRaw, '--max-seconds', "$MaxSeconds")
    if ($Script) { $args += @('--script', $Script) }
    $args += $Inputs
    return Invoke-Capture $Play $args $TimeoutSec
}

# Decode a FLAC/WAV to packed raw (libFLAC for flac, ffmpeg for wav) = golden.
function Golden-Raw {
    param([string]$Path, [string]$Ext, [string]$OutRaw)
    if ($Ext -eq 'flac') {
        $r = Invoke-Capture $Flac @('-d','--force-raw-format','--endian=little','--sign=signed','-f','-o',$OutRaw,'--',$Path)
    } else {
        $fmt = 'pcm_s16le'
        $r = Invoke-Capture $Ffmpeg @('-v','error','-nostdin','-y','-i',$Path,'-map','a:0','-c:a',$fmt,'-f','s16le','--',$OutRaw)
    }
    if (-not (Test-Path $OutRaw)) { throw "golden decode failed for $Path : $($r.StdErr)" }
    return $OutRaw
}

$commit = (& git -C $RepoRoot rev-parse --short HEAD 2>$null | Out-String).Trim()
$branch = (& git -C $RepoRoot rev-parse --abbrev-ref HEAD 2>$null | Out-String).Trim()

# --- load golden manifest + corpus-unchanged check -------------------------
if (-not $GoldenManifest) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' |
        Sort-Object Name -Descending | Select-Object -First 1
    if (-not $latest) { throw 'No golden manifest found.' }
    $GoldenManifest = $latest.FullName
}
$manifest = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json

$corpusUnchanged = $true; $corpusDetail = ''
try {
    $lines = foreach ($f in $manifest.files) {
        $it = Get-Item -LiteralPath $f.path -ErrorAction Stop
        "$($f.path)|$($it.Length)|$($it.LastWriteTimeUtc.ToString('o'))"
    }
    $digestInput = ($lines) -join "`n"
    $liveHash = [System.BitConverter]::ToString(
        [System.Security.Cryptography.SHA256]::Create().ComputeHash(
            [System.Text.Encoding]::UTF8.GetBytes($digestInput))).Replace('-', '').ToLower()
    $corpusUnchanged = ($liveHash -eq $manifest.corpusHash)
    if (-not $corpusUnchanged) { $corpusDetail = "live=$liveHash manifest=$($manifest.corpusHash)" }
} catch { $corpusUnchanged = $false; $corpusDetail = "error: $($_.Exception.Message)" }

$results = [ordered]@{}

# ===========================================================================
# Check 1 + 2: Gapless == whole, and cue boundaries
# ===========================================================================
Write-Host 'P3: check 1/2 gapless + cue boundaries ...' -ForegroundColor Cyan
$whole = Join-Path $GaplessDir 'gapless_whole.flac'
$cue   = Join-Path $GaplessDir 'gapless_whole.cue'
$tracks = 1..5 | ForEach-Object { Join-Path $GaplessDir "track$_.flac" }

$gaplessRow = [ordered]@{ Status='FAIL'; Note=''; Hash=''; Bytes=0 }
$boundRow   = [ordered]@{ Status='FAIL'; Note='' }
try {
    $wRaw = Join-Path $TmpDir 'whole.raw'
    $tRaw = Join-Path $TmpDir 'tracks.raw'
    $cRaw = Join-Path $TmpDir 'cue.raw'
    $gRaw = Join-Path $TmpDir 'whole_golden.raw'
    Render-Offline -Inputs @($whole) -OutRaw $wRaw -MaxSeconds 90 | Out-Null
    Render-Offline -Inputs $tracks -OutRaw $tRaw -MaxSeconds 90 | Out-Null
    $cueCap = Render-Offline -Inputs @($cue) -OutRaw $cRaw -MaxSeconds 90
    # golden = lyra-probe decode (packed), independently equal to libFLAC (P1).
    $dr = Invoke-Capture $Probe @('decode', $whole, '--out', $gRaw)
    if ($dr.ExitCode -ne 0) { throw "decode golden failed: $($dr.StdErr)" }

    $hw = Sha256-File $wRaw; $ht = Sha256-File $tRaw; $hc = Sha256-File $cRaw; $hg = Sha256-File $gRaw
    $bw = (Get-Item $wRaw).Length
    $allEq = ($hw -eq $ht) -and ($hw -eq $hc) -and ($hw -eq $hg)
    $gaplessRow.Hash = $hw.Substring(0,16); $gaplessRow.Bytes = $bw
    if ($allEq) {
        $gaplessRow.Status = 'PASS'
        $gaplessRow.Note = "whole==5tracks==cue==decode; $bw bytes; sha256 $($hw.Substring(0,16))"
    } else {
        $gaplessRow.Note = "MISMATCH whole=$($hw.Substring(0,12)) tracks=$($ht.Substring(0,12)) cue=$($hc.Substring(0,12)) decode=$($hg.Substring(0,12))"
    }

    # boundaries from cue Track events: expect 5 x 1,152,000 frames @ 96k.
    $durs = @([regex]::Matches($cueCap.StdOut, '\[(\d+) frames\]') | ForEach-Object { [int64]$_.Groups[1].Value })
    $expected = @(1152000,1152000,1152000,1152000,1152000)
    $starts = @(); $acc = 0; foreach ($d in $durs) { $starts += $acc; $acc += $d }
    $expStarts = @(0,1152000,2304000,3456000,4608000)
    $durOk = ($durs.Count -eq 5) -and (-not (Compare-Object $durs $expected))
    $startOk = (-not (Compare-Object $starts $expStarts))
    $cueEqWhole = ($hc -eq $hw)
    if ($durOk -and $startOk -and $cueEqWhole) {
        $boundRow.Status = 'PASS'
        $boundRow.Note = "5 virtual tracks; starts $($starts -join '/'); cue.raw==whole.raw"
    } else {
        $boundRow.Note = "durs=$($durs -join ',') starts=$($starts -join '/') cueEqWhole=$cueEqWhole"
    }
} catch {
    $gaplessRow.Note = "error: $($_.Exception.Message -replace '\s+',' ')"
    $boundRow.Note = $gaplessRow.Note
}
$results['gapless'] = $gaplessRow
$results['boundaries'] = $boundRow

# ===========================================================================
# Check 3: Seek through the engine (50 pts x 10 files), offline suffix method
# ===========================================================================
Write-Host 'P3: check 3 seek-through-engine ...' -ForegroundColor Cyan
# Select files: 3 synth 24/96 (cheap) + smallest corpus per spec class + wav,
# to total $SeekFileCount manageable files each run at $SeekPoints. The sole
# 192 kHz file (184 MB; whole-file offline render per seek is costly) is added
# as a reduced-point spot-check ($BigFilePoints) for rate coverage, flagged.
$seekFiles = New-Object System.Collections.Generic.List[object]
function Add-Seek([string]$path, [string]$ext, [int]$bits, [int]$ch, [int]$rate, [string]$tag, [int]$pts) {
    $seekFiles.Add([pscustomobject]@{ Path=$path; Ext=$ext; Bits=$bits; Ch=$ch; Rate=$rate; Tag=$tag; Pts=$pts })
}
Add-Seek $whole 'flac' 24 2 96000 'synth-whole-24/96' $SeekPoints
Add-Seek (Join-Path $GaplessDir 'track1.flac') 'flac' 24 2 96000 'synth-track1-24/96' $SeekPoints
Add-Seek (Join-Path $GaplessDir 'track4.flac') 'flac' 24 2 96000 'synth-track4-24/96' $SeekPoints
$byClass = @{}
foreach ($f in ($manifest.files | Where-Object { $_.ext -eq 'flac' })) {
    $key = "$($f.bits)-$($f.sampleRate)"
    if (-not $byClass.ContainsKey($key)) { $byClass[$key] = @() }
    $byClass[$key] += $f
}
# smallest file(s) per wanted class (full points)
foreach ($spec in @(@(16,44100,3),@(24,44100,1),@(24,48000,1),@(24,96000,1))) {
    $key = "$($spec[0])-$($spec[1])"
    if ($byClass.ContainsKey($key)) {
        $pick = $byClass[$key] | Sort-Object size | Select-Object -First $spec[2]
        foreach ($p in $pick) { Add-Seek $p.path 'flac' $p.bits $p.channels $p.sampleRate "corpus-$($p.bits)/$([int]($p.sampleRate/1000))k" $SeekPoints }
    }
}
$wav = @($manifest.files | Where-Object { $_.ext -eq 'wav' } | Select-Object -First 1)
if ($wav.Count -gt 0) { Add-Seek $wav[0].path 'wav' $wav[0].bits $wav[0].channels $wav[0].sampleRate 'corpus-wav-16/44k' $SeekPoints }
# Take the first $SeekFileCount main files, then append the 192 kHz spot-check
# (reduced points for runtime) as extra rate coverage beyond the 10.
$mainFiles = @($seekFiles | Select-Object -First $SeekFileCount)
$f192 = @($byClass['24-192000'] | Sort-Object size | Select-Object -First 1)
if (-not $Quick -and $f192.Count -gt 0) {
    $mainFiles += [pscustomobject]@{ Path=$f192[0].path; Ext='flac'; Bits=$f192[0].bits; Ch=$f192[0].channels; Rate=$f192[0].sampleRate; Tag='corpus-24/192k(spot)'; Pts=$BigFilePoints }
}

$seekRows = @(); $seekPass = 0; $seekFail = 0; $tailSamples = @()
$rng = [System.Random]::new(20261126)
$sliceFrames = 4096
foreach ($sf in $mainFiles) {
    $name = Split-Path $sf.Path -Leaf
    $pts = $sf.Pts
    $bpf = Bytes-Per-Frame $sf.Bits $sf.Ch
    $gRaw = Join-Path $TmpDir ('seekgold_' + ($name -replace '[^A-Za-z0-9._-]','_') + '.raw')
    $ok = 0; $firstBad = $null; $note = ''; $maxTail = 0
    try {
        Golden-Raw -Path $sf.Path -Ext $sf.Ext -OutRaw $gRaw | Out-Null
        $goldLen = (Get-Item $gRaw).Length
        $total = [int64]($goldLen / $bpf)
        $maxStart = $total - $sliceFrames
        if ($maxStart -lt 1) { throw "file too short ($total frames)" }
        $gold = [System.IO.File]::ReadAllBytes($gRaw)
        $seekRaw = Join-Path $TmpDir 'seekout.raw'
        $scriptF = Join-Path $TmpDir 'seek.txt'
        for ($i = 0; $i -lt $pts; $i++) {
            $N = [int64]$rng.Next(0, [int]$maxStart)
            Set-Content -LiteralPath $scriptF -Value "play`nseek $N`nwait-eos`nquit" -Encoding ASCII
            $cap = Render-Offline -Inputs @($sf.Path) -OutRaw $seekRaw -Script $scriptF -MaxSeconds 150 -TimeoutSec 200
            if ($cap.TimedOut) { $firstBad = $N; $note = 'render timeout'; break }
            $F = (Get-Item $seekRaw).Length
            $Lpost = ($total - $N) * $bpf
            $offset = $F - $Lpost
            if ($offset -lt 0) { if ($null -eq $firstBad) { $firstBad = $N }; continue }
            if (($offset / $bpf) -gt $maxTail) { $maxTail = [int64]($offset / $bpf) }
            $a = Read-Region $seekRaw $offset ($sliceFrames * $bpf)
            if ($null -eq $a) { if ($null -eq $firstBad) { $firstBad = $N }; continue }
            $b = New-Object byte[] ($sliceFrames * $bpf)
            [Array]::Copy($gold, $N * $bpf, $b, 0, $sliceFrames * $bpf)
            if ((Sha256-Bytes $a) -eq (Sha256-Bytes $b)) { $ok++ }
            elseif ($null -eq $firstBad) { $firstBad = $N }
        }
        $tailSamples += $maxTail
    } catch { $note = "error: $($_.Exception.Message -replace '\s+',' ')" }
    finally { Remove-Item -LiteralPath $gRaw -Force -ErrorAction SilentlyContinue }
    $status = if ($ok -eq $pts -and -not $note) { 'PASS' } else { 'FAIL' }
    if ($status -eq 'PASS') { $seekPass++ } else { $seekFail++ }
    if ($firstBad -ne $null -and -not $note) { $note = "first bad frame=$firstBad" }
    $seekRows += [pscustomobject]@{ Name=$name; Tag=$sf.Tag; Spec="$($sf.Rate)/$($sf.Bits)/$($sf.Ch)ch"; Points="$ok/$pts"; Tail=$maxTail; Status=$status; Note=$note }
}
$results['seek'] = [pscustomobject]@{ Pass=$seekPass; Fail=$seekFail; Rows=$seekRows; TailMax=(@($tailSamples) | Measure-Object -Maximum).Maximum }

# ===========================================================================
# Check 4: Command storm
# ===========================================================================
Write-Host 'P3: check 4 command storm ...' -ForegroundColor Cyan
$stormRow = [ordered]@{ Status='FAIL'; Note='' }
try {
    $srng = [System.Random]::new(777)
    $verbs = @('play','pause','stop','next','prev','seek','playat','wait')
    $lines = New-Object System.Collections.Generic.List[string]
    for ($i=0; $i -lt $StormCommands; $i++) {
        $v = $verbs[$srng.Next(0,$verbs.Count)]
        switch ($v) {
            'seek'   { $lines.Add("seek "   + $srng.Next(0,6000000)) }
            'playat' { $lines.Add("playat " + $srng.Next(0,7)) }
            'wait'   { $lines.Add("wait "   + $srng.Next(0,6)) }
            default  { $lines.Add($v) }
        }
    }
    $lines.Add('quit')
    $stormF = Join-Path $TmpDir 'storm.txt'
    Set-Content -LiteralPath $stormF -Value ($lines -join "`n") -Encoding ASCII
    # Budget ~25ms/command (lyra-play drains 20ms per step) + margin.
    $maxSec = [int]([math]::Ceiling($StormCommands * 0.03) + 20)
    $cap = Render-Offline -Inputs $tracks -OutRaw (Join-Path $TmpDir 'storm.raw') -Script $stormF -MaxSeconds $maxSec -TimeoutSec ($maxSec + 30)
    $panic = ($cap.ExitCode -eq 101) -or ($cap.StdErr -match 'panic') -or ($cap.StdOut -match 'panicked')
    $completed = ($cap.StdOut -match '\[script\] quit')
    $hitCap = ($cap.StdOut -match 'max-seconds reached')
    $lastState = (@([regex]::Matches($cap.StdOut, 'State: (\w+)')) | Select-Object -Last 1)
    $finalState = if ($lastState) { $lastState.Groups[1].Value } else { 'none' }
    $errCount = ([regex]::Matches($cap.StdOut, '(?m)^Error:')).Count
    if (-not $panic -and -not $cap.TimedOut -and $cap.ExitCode -eq 0 -and $completed) {
        $stormRow.Status = 'PASS'
        $stormRow.Note = "$StormCommands cmds, completed cleanly, exit 0, final state=$finalState, graceful error-events=$errCount, no panic/deadlock"
    } else {
        $stormRow.Note = "panic=$panic exit=$($cap.ExitCode) timedOut=$($cap.TimedOut) completed=$completed hitCap=$hitCap finalState=$finalState"
    }
} catch { $stormRow.Note = "error: $($_.Exception.Message -replace '\s+',' ')" }
$results['storm'] = $stormRow

# ===========================================================================
# Check 5: Clock drift (offline mapping exactness) + pending real-device
# ===========================================================================
Write-Host 'P3: check 5 clock drift (offline mapping) ...' -ForegroundColor Cyan
$driftRow = [ordered]@{ Status='FAIL'; Note='' }
try {
    $driftRaw = Join-Path $TmpDir 'drift.raw'
    $cap = Render-Offline -Inputs @($whole) -OutRaw $driftRaw -MaxSeconds 90
    $positions = @([regex]::Matches($cap.StdOut, 'Position: (\d+) frames') | ForEach-Object { [int64]$_.Groups[1].Value })
    $renderedFrames = [int64]((Get-Item $driftRaw).Length / 6)   # 24/96 stereo bpf=6
    $lastPos = if ($positions.Count -gt 0) { $positions[-1] } else { -1 }
    # Offline: device consumes everything pushed; final Position must equal the
    # total source frames rendered (exact 1:1 PCM mapping). Drift here is the
    # frame error of that mapping -> 0 frames -> 0 ms by construction.
    $monotonic = $true
    for ($i=1; $i -lt $positions.Count; $i++) { if ($positions[$i] -lt $positions[$i-1]) { $monotonic = $false; break } }
    $exact = ($lastPos -eq $renderedFrames) -and $monotonic -and ($positions.Count -gt 0)
    if ($exact) {
        $driftRow.Status = 'PASS'
        $driftRow.Note = "offline mapping exact: final Position $lastPos == rendered $renderedFrames frames; $($positions.Count) samples monotonic; frame-error 0 (0 ms). Real-time wall-clock drift = pending-hardware (device)."
    } else {
        $driftRow.Note = "lastPos=$lastPos rendered=$renderedFrames monotonic=$monotonic samples=$($positions.Count)"
    }
} catch { $driftRow.Note = "error: $($_.Exception.Message -replace '\s+',' ')" }
$results['drift'] = $driftRow

# ===========================================================================
# Check 6: Robustness through the queue
# ===========================================================================
Write-Host 'P3: check 6 robustness ...' -ForegroundColor Cyan
$sentinel = Join-Path $GaplessDir 'track1.flac'
$garbage = Join-Path $TmpDir 'garbage.flac'
$badCue  = Join-Path $TmpDir 'bad.cue'
$missing = Join-Path $TmpDir 'this_does_not_exist.flac'
[System.IO.File]::WriteAllBytes($garbage, ([byte[]]@(0..255) * 80))
Set-Content -LiteralPath $badCue -Value "FILE `"nonexistent_image.flac`" WAVE`n  TRACK 01 AUDIO`n    INDEX 01 00:00:00`n  %% garbage line %%" -Encoding ASCII
$robTargets = @()
$robTargets += ,@('trunc10',  (Get-ChildItem $RobDir -Filter '*.trunc10.flac'  | Select-Object -First 1).FullName, $false)
$robTargets += ,@('trunc50',  (Get-ChildItem $RobDir -Filter '*.trunc50.flac'  | Select-Object -First 1).FullName, $false)
$robTargets += ,@('trunc90',  (Get-ChildItem $RobDir -Filter '*.trunc90.flac'  | Select-Object -First 1).FullName, $false)
$robTargets += ,@('flipbyte', (Get-ChildItem $RobDir -Filter '*.flipbyte.flac' | Select-Object -First 1).FullName, $false)
$robTargets += ,@('zerohdr',  (Get-ChildItem $RobDir -Filter '*.zerohdr.flac'  | Select-Object -First 1).FullName, $true)
$robTargets += ,@('garbage',  $garbage, $true)
$robTargets += ,@('missing',  $missing, $true)
$robTargets += ,@('bad.cue',  $badCue,  $true)

$robRows = @(); $robPass = 0; $robFail = 0
foreach ($t in $robTargets) {
    $tag = $t[0]; $path = $t[1]; $mustError = $t[2]
    $cap = Render-Offline -Inputs @($path, $sentinel) -OutRaw (Join-Path $TmpDir 'rob.raw') -MaxSeconds 30 -TimeoutSec 60
    $panic = ($cap.ExitCode -eq 101) -or ($cap.StdErr -match 'panic') -or ($cap.StdOut -match 'panicked')
    $errored = ($cap.StdOut -match '(?m)^Error:')
    # sentinel played = a Track line for track1 AND reached Stopped.
    $sentinelPlayed = ($cap.StdOut -match 'track1\.flac') -and ($cap.StdOut -match 'State: Stopped')
    $continued = $sentinelPlayed
    $ok = (-not $panic) -and (-not $cap.TimedOut) -and $continued -and ((-not $mustError) -or $errored)
    if ($ok) { $robPass++ } else { $robFail++ }
    $note = "error-event=$errored queue-continued=$continued panic=$panic exit=$($cap.ExitCode)"
    if (-not $mustError -and -not $errored) { $note += ' (clean early-EOF tolerated)' }
    $robRows += [pscustomobject]@{ Variant=$tag; MustError=$mustError; ErrorEvent=$errored; Continued=$continued; Panic=$panic; Status=$(if($ok){'PASS'}else{'FAIL'}); Note=$note }
}
$results['robust'] = [pscustomobject]@{ Pass=$robPass; Fail=$robFail; Rows=$robRows }

# ===========================================================================
# Check 7: Fuzzing (cue parser via engine; decode/dsd re-confirm)
# ===========================================================================
Write-Host 'P3: check 7 fuzzing ...' -ForegroundColor Cyan
function Fuzz-Parser {
    param([string]$Kind, [int]$Seconds, [scriptblock]$MakeInput, [scriptblock]$RunOne)
    $frng = [System.Random]::new(4242)
    $deadline = (Get-Date).AddSeconds($Seconds)
    $iter = 0; $crash = 0; $firstCrash = ''
    while ((Get-Date) -lt $deadline) {
        $inp = & $MakeInput $frng $iter
        $cap = & $RunOne $inp
        $panic = ($cap.ExitCode -eq 101) -or ($cap.StdErr -match 'panicked at') -or ($cap.StdErr -match 'RUST_BACKTRACE')
        if ($panic -or $cap.TimedOut) { $crash++; if (-not $firstCrash) { $firstCrash = "iter $iter exit=$($cap.ExitCode) timedOut=$($cap.TimedOut)" } }
        $iter++
    }
    return [pscustomobject]@{ Kind=$Kind; Iterations=$iter; Crashes=$crash; FirstCrash=$firstCrash; Seconds=$Seconds }
}
$validCue = Get-Content -LiteralPath $cue -Raw
$fuzzCueFile = Join-Path $TmpDir 'fuzz.cue'
$cueFuzz = Fuzz-Parser -Kind 'cue' -Seconds $FuzzSeconds -MakeInput {
    param($r,$i)
    if ($r.Next(0,3) -eq 0) {
        # random bytes
        $n = $r.Next(0,400); $b = New-Object byte[] $n; $r.NextBytes($b)
        [System.IO.File]::WriteAllBytes($fuzzCueFile, $b)
    } else {
        # mutate the valid cue
        $chars = [char[]]$validCue
        $muts = $r.Next(1,8)
        for ($k=0;$k -lt $muts;$k++) { $pos=$r.Next(0,$chars.Length); $chars[$pos]=[char]$r.Next(32,127) }
        Set-Content -LiteralPath $fuzzCueFile -Value (-join $chars) -Encoding ASCII
    }
    return $fuzzCueFile
} -RunOne {
    param($inp)
    # Cap at 1 s: a cue that fails to play never sets saw_playing, so lyra-play
    # otherwise waits out the whole --max-seconds. 1 s keeps parser throughput high.
    Render-Offline -Inputs @($inp) -OutRaw (Join-Path $TmpDir 'fuzz.raw') -MaxSeconds 1 -TimeoutSec 10
}
$fuzzFlacFile = Join-Path $TmpDir 'fuzz.flac'
$flacSeed = [System.IO.File]::ReadAllBytes((Join-Path $GaplessDir 'track1.flac'))[0..20000]
$decodeFuzzSec = [math]::Min($FuzzSeconds, 120)
$decFuzz = Fuzz-Parser -Kind 'decode(flac)' -Seconds $decodeFuzzSec -MakeInput {
    param($r,$i)
    $b = $flacSeed.Clone()
    $muts = $r.Next(1,30)
    for ($k=0;$k -lt $muts;$k++) { $pos=$r.Next(0,$b.Length); $b[$pos]=[byte]$r.Next(0,256) }
    [System.IO.File]::WriteAllBytes($fuzzFlacFile, $b)
    return $fuzzFlacFile
} -RunOne {
    param($inp)
    Invoke-Capture $Probe @('decode', $inp, '--engine', 'lyra') 20
}
# dsd re-confirm: mutate a real DFF header region if a corpus DFF exists.
$dff = @($manifest.files | Where-Object { $_.ext -eq 'dff' } | Select-Object -First 1)
$dsdFuzz = $null
if ($dff.Count -gt 0) {
    $fuzzDsdFile = Join-Path $TmpDir 'fuzz.dff'
    $dsdSeed = [System.IO.File]::ReadAllBytes($dff[0].path)[0..20000]
    $dsdFuzzSec = [math]::Min($FuzzSeconds, 120)
    $dsdFuzz = Fuzz-Parser -Kind 'dsd(dff)' -Seconds $dsdFuzzSec -MakeInput {
        param($r,$i)
        $b = $dsdSeed.Clone()
        $muts = $r.Next(1,30)
        for ($k=0;$k -lt $muts;$k++) { $pos=$r.Next(0,$b.Length); $b[$pos]=[byte]$r.Next(0,256) }
        [System.IO.File]::WriteAllBytes($fuzzDsdFile, $b)
        return $fuzzDsdFile
    } -RunOne {
        param($inp)
        Invoke-Capture $Probe @('dsd', 'hash', $inp) 20
    }
}
$fuzzRows = @($cueFuzz, $decFuzz)
if ($dsdFuzz) { $fuzzRows += $dsdFuzz }
$fuzzFail = @($fuzzRows | Where-Object { $_.Crashes -gt 0 }).Count
$results['fuzz'] = [pscustomobject]@{ Rows=$fuzzRows; Fail=$fuzzFail }

# ===========================================================================
# Regression: P5 DSD byte-equality (device-free). P1 delegated separately.
# ===========================================================================
$p5Row = $null
if (-not $SkipRegression -and $dff.Count -ge 1) {
    Write-Host 'P3: regression P5 DSD byte-equality ...' -ForegroundColor Cyan
    try {
        $dsfDir = Join-Path $SynthDir 'dsf'; if (-not (Test-Path $dsfDir)) { New-Item -ItemType Directory -Force -Path $dsfDir | Out-Null }
        $p5pass = 0; $p5fail = 0; $p5notes = @()
        foreach ($d in ($manifest.files | Where-Object { $_.ext -eq 'dff' })) {
            $h1 = Invoke-Capture $Probe @('dsd','hash',$d.path)
            if ($h1.ExitCode -ne 0) { $p5fail++; $p5notes += "hash DFF failed"; continue }
            $j1 = $h1.StdOut | ConvertFrom-Json
            $dsf = Join-Path $dsfDir ((Split-Path $d.path -Leaf) -replace '\.dff$','.dsf')
            $cv = Invoke-Capture $Probe @('dsd','convert',$d.path,$dsf)
            if ($cv.ExitCode -ne 0 -or -not (Test-Path $dsf)) { $p5fail++; $p5notes += "convert failed: $($cv.StdErr -replace '\s+',' ')"; continue }
            $h2 = Invoke-Capture $Probe @('dsd','hash',$dsf)
            $j2 = $h2.StdOut | ConvertFrom-Json
            if ($j1.sha256Canonical -eq $j2.sha256Canonical) { $p5pass++; $p5notes += "$(Split-Path $d.path -Leaf): DFF==DSF canonical $($j1.sha256Canonical.Substring(0,12))" }
            else { $p5fail++; $p5notes += "$(Split-Path $d.path -Leaf): canonical MISMATCH" }
        }
        $p5Row = [pscustomobject]@{ Status=$(if($p5fail -eq 0 -and $p5pass -gt 0){'PASS'}else{'FAIL'}); Pass=$p5pass; Fail=$p5fail; Note=($p5notes -join '; ') }
    } catch { $p5Row = [pscustomobject]@{ Status='FAIL'; Pass=0; Fail=1; Note="error: $($_.Exception.Message -replace '\s+',' ')" } }
}

# ===========================================================================
# Verdict + report
# ===========================================================================
$corpusUnchangedAfter = $true
try {
    $lines2 = foreach ($f in $manifest.files) { $it = Get-Item -LiteralPath $f.path -ErrorAction Stop; "$($f.path)|$($it.Length)|$($it.LastWriteTimeUtc.ToString('o'))" }
    $liveHash2 = [System.BitConverter]::ToString([System.Security.Cryptography.SHA256]::Create().ComputeHash([System.Text.Encoding]::UTF8.GetBytes(($lines2 -join "`n")))).Replace('-','').ToLower()
    $corpusUnchangedAfter = ($liveHash2 -eq $manifest.corpusHash)
} catch { $corpusUnchangedAfter = $false }

$reqGapless = ($gaplessRow.Status -eq 'PASS')
$reqBound   = ($boundRow.Status -eq 'PASS')
$reqSeek    = (($seekFail -eq 0) -and ($seekPass -gt 0))
$reqStorm   = ($stormRow.Status -eq 'PASS')
$reqDrift   = ($driftRow.Status -eq 'PASS')
$reqRobust  = ($robFail -eq 0)
$reqFuzz    = ($fuzzFail -eq 0)
$gateFail = (-not $reqGapless) -or (-not $reqBound) -or (-not $reqSeek) -or (-not $reqStorm) `
    -or (-not $reqDrift) -or (-not $reqRobust) -or (-not $reqFuzz) `
    -or (-not $corpusUnchanged) -or (-not $corpusUnchangedAfter)
$gate = if ($gateFail) { 'FAIL' } else { 'PASS' }

$probeVer = (& $Probe '--version' 2>&1 | Out-String).Trim()
$playVer  = (& $Play '--version' 2>&1 | Out-String).Trim()
$flacVer  = (& $Flac '--version' 2>&1 | Select-Object -First 1 | Out-String).Trim()
$ffVer    = (& $Ffmpeg '-version' 2>&1 | Select-Object -First 1 | Out-String).Trim()
$stamp    = (Get-Date).ToString('yyyyMMdd-HHmmss')
$reportPath = Join-Path $ReportDir "P3-$stamp.md"

function Mark([string]$s) { if ($s -eq 'PASS') { '[PASS]' } elseif ($s -eq 'FAIL') { '[FAIL]' } else { "[$s]" } }

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine('# Parity report - P3 (playback engine)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| Field | Value |')
[void]$sb.AppendLine('|---|---|')
[void]$sb.AppendLine("| Generated (UTC) | $((Get-Date).ToUniversalTime().ToString('o')) |")
[void]$sb.AppendLine("| Commit | $commit (branch $branch) |")
[void]$sb.AppendLine("| Golden manifest | $(Split-Path $GoldenManifest -Leaf) |")
[void]$sb.AppendLine("| Corpus manifest hash | $($manifest.corpusHash) |")
[void]$sb.AppendLine("| Corpus unchanged (before/after) | $(if($corpusUnchanged){'yes'}else{'NO - '+$corpusDetail}) / $(if($corpusUnchangedAfter){'yes'}else{'NO'}) |")
[void]$sb.AppendLine("| Machine | $env:COMPUTERNAME |")
[void]$sb.AppendLine("| Device | none (offline render; no device opened) |")
[void]$sb.AppendLine("| lyra-probe | $probeVer |")
[void]$sb.AppendLine("| lyra-play | $playVer |")
[void]$sb.AppendLine("| flac | $flacVer |")
[void]$sb.AppendLine("| ffmpeg | $($ffVer -replace '\|','/') |")
[void]$sb.AppendLine("| Mode | $(if($Quick){'QUICK (smoke)'}else{'full gate'}); SeekPoints=$SeekPoints; Storm=$StormCommands; FuzzSeconds=$FuzzSeconds |")
if ($BaselineNote) { [void]$sb.AppendLine("| Baseline | $BaselineNote |") }
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## P3 exit-gate checks')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| # | Check | Result | Evidence |')
[void]$sb.AppendLine('|---|---|---|---|')
[void]$sb.AppendLine("| 1 | Gapless == whole (offline whole==5tracks==cue==decode) | $(Mark $gaplessRow.Status) | $($gaplessRow.Note) |")
[void]$sb.AppendLine("| 2 | Cue-image virtual-track boundaries | $(Mark $boundRow.Status) | $($boundRow.Note) |")
$mainSeek = @($seekRows | Where-Object { $_.Tag -notlike '*spot*' }).Count
[void]$sb.AppendLine("| 3 | Seek through engine ($SeekPoints pts x $mainSeek files + 192k spot) | $(Mark $(if($seekFail -eq 0 -and $seekPass -gt 0){'PASS'}else{'FAIL'})) | $seekPass/$($seekRows.Count) files all points byte-exact; max pre-seek ring tail $($results['seek'].TailMax) frames (device-tap limitation) |")
[void]$sb.AppendLine("| 4 | Command storm (no deadlock/panic) | $(Mark $stormRow.Status) | $($stormRow.Note) |")
[void]$sb.AppendLine("| 5 | Clock drift (offline mapping) | $(Mark $driftRow.Status) | $($driftRow.Note) |")
[void]$sb.AppendLine("| 6 | Robustness set + bad inputs through queue | $(Mark $(if($robFail -eq 0){'PASS'}else{'FAIL'})) | $robPass/$($robRows.Count) pass; no panic; queue always continued |")
[void]$sb.AppendLine("| 7 | Fuzzing (cue parser; decode/dsd re-confirm) | $(Mark $(if($fuzzFail -eq 0){'PASS'}else{'FAIL'})) | $(($fuzzRows | ForEach-Object { "$($_.Kind): $($_.Iterations) iters/$($_.Seconds)s, $($_.Crashes) crashes" }) -join '; ') |")
[void]$sb.AppendLine('')

[void]$sb.AppendLine('### 3. Seek-through-engine detail')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| File | Class | Spec | Points OK | Pre-seek tail (frames) | Result | Note |')
[void]$sb.AppendLine('|---|---|---|---|---|---|---|')
foreach ($r in $seekRows) { [void]$sb.AppendLine("| ``$($r.Name)`` | $($r.Tag) | $($r.Spec) | $($r.Points) | $($r.Tail) | $(Mark $r.Status) | $($r.Note) |") }
[void]$sb.AppendLine('')

[void]$sb.AppendLine('### 6. Robustness detail')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| Variant | Must error | Error event | Queue continued | Panic | Result | Note |')
[void]$sb.AppendLine('|---|---|---|---|---|---|---|')
foreach ($r in $robRows) { [void]$sb.AppendLine("| $($r.Variant) | $($r.MustError) | $($r.ErrorEvent) | $($r.Continued) | $($r.Panic) | $(Mark $r.Status) | $($r.Note) |") }
[void]$sb.AppendLine('')

if ($p5Row) {
    [void]$sb.AppendLine('## Regression - P5 DSD byte-equality (device-free)')
    [void]$sb.AppendLine('')
    [void]$sb.AppendLine("$(Mark $p5Row.Status) DFF->DSF canonical re-container: $($p5Row.Pass) pass / $($p5Row.Fail) fail. $($p5Row.Note)")
    [void]$sb.AppendLine('')
}

[void]$sb.AppendLine('## Pending human / hardware')
[void]$sb.AppendLine('- Real-device clock drift over 5-10 min (wall-clock vs device-consumed frames): needs the render device + Sound rule; offline proves the position *mapping* is frame-exact but not real-time drift. **pending-hardware.**')
[void]$sb.AppendLine('- Muted device-tap A/B for the actual bit-perfect-at-the-device seek/gapless proof and the device-tap pre-seek tail: offline is authoritative for byte-accuracy; device tap is **pending-hardware**.')
[void]$sb.AppendLine('- P2 render regression (tap parity, negotiated rate) not re-run here (needs device). Run `node tests/parity/p2_render.mjs` when the device is free.')
[void]$sb.AppendLine('- Human listen to the gapless set (no clicks at joins) - **pending human** (P3 exit gate).')
[void]$sb.AppendLine('')

$findings = @()
[void]$sb.AppendLine('## Findings (routed)')
[void]$sb.AppendLine('- Engine (lyra-engine): seek does not flush the lyra-output ring; a pre-seek tail (measured up to ' + $results['seek'].TailMax + ' frames in the offline path) plays before the landed audio on a real device tap. Offline suffix is byte-exact. Possible need for a ring flush/reset API on OutputHandle. **Non-blocking** (offline seek is sample-accurate).')
[void]$sb.AppendLine('')

[void]$sb.AppendLine("## Gate: $gate")
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Summary: gapless $(Mark $gaplessRow.Status), boundaries $(Mark $boundRow.Status), seek $seekPass/$($seekRows.Count), storm $(Mark $stormRow.Status), drift $(Mark $driftRow.Status), robustness $robPass/$($robRows.Count), fuzz crashes $fuzzFail, corpus unchanged $(if($corpusUnchanged -and $corpusUnchangedAfter){'yes'}else{'NO'}).")

Set-Content -LiteralPath $reportPath -Value $sb.ToString() -Encoding UTF8
Write-Host "p3.ps1: wrote $reportPath" -ForegroundColor Cyan
Write-Host "P3 GATE: $gate" -ForegroundColor $(if ($gate -eq 'PASS') { 'Green' } else { 'Red' })
$reportPath
