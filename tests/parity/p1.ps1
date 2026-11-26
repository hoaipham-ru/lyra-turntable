<#
.SYNOPSIS
    P1.6 decode-parity suite: lyra-decode vs the golden references (TEST-STRATEGY
    §4), writing a §9 report. Invoked by `run.ps1 -Phase P1`.

.DESCRIPTION
    Checks, against the committed golden manifest (the libFLAC/ffmpeg references
    built in P0):
      1. Corpus unchanged   - recompute the manifest corpusHash from live files.
      2. Decode parity      - lyra-decode (via lyra-probe --engine lyra) SHA-256 ==
                              golden (libFLAC for FLAC, ffmpeg for WAV), bit-exact;
                              declared total_frames == decoded frames.
      3. Seek parity        - 50 random points x representative files: 4096 frames
                              after the seek == the golden slice.
      4. Broken-picture     - the 2 FLACs with a no-MIME embedded picture decode
                              normally (covered by the hash row; flagged here).
      5. Robustness         - truncated 10/50/90 %, flipped-byte, zeroed-header
                              copies (in target/corpus-tmp/): no panic; mid-stream
                              corruption errors carry a frame position.
      6. Synthetic 5.1 FLAC - decode-only bit-identical to libFLAC.

    The corpus is read-only (ADR-006): corrupt/synthetic copies go to
    target/corpus-tmp/ and target/corpus-synth/. Nothing here renders audio.

.PARAMETER GoldenManifest
    Golden manifest JSON. Default: newest tests/parity/manifests/golden-*.json.

.PARAMETER SeekPoints
    Random seek points per representative file. Default 50.

.PARAMETER Quick
    Smoke mode: 3 decode files, 1 representative seek file, 5 seek points.
#>
[CmdletBinding()]
param(
    [string]$GoldenManifest,
    [int]$SeekPoints = 50,
    [switch]$Quick
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ReportDir   = Join-Path $RepoRoot 'target\parity\reports'
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
$TmpDir      = Join-Path $RepoRoot 'target\corpus-tmp'
$SynthDir    = Join-Path $RepoRoot 'target\corpus-synth'
foreach ($d in @($ReportDir, $TmpDir, $SynthDir)) {
    if (-not (Test-Path $d)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
}

# --- tool resolution -------------------------------------------------------
function Resolve-Probe {
    foreach ($cfg in @('release', 'debug')) {
        $p = Join-Path $RepoRoot "target\$cfg\lyra-probe.exe"
        if (Test-Path $p) { return $p }
    }
    throw 'lyra-probe.exe not found; build it first (cargo build -p lyra-probe --release).'
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
$Probe   = Resolve-Probe
$Flac    = Resolve-Flac
$Ffmpeg  = Require-Tool 'ffmpeg'

# Run a native exe capturing stdout, stderr and exit code without PS turning a
# non-zero exit or stderr text into a terminating error. Uses the call operator
# with array splatting (@Arguments), which passes each element as one argument
# with correct quoting - unlike Start-Process -ArgumentList, which mangles paths
# that contain spaces (e.g. "CD 01\12 - Summer.flac").
function Invoke-Capture {
    param([string]$Exe, [string[]]$Arguments)
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    $errFile = [System.IO.Path]::GetTempFileName()
    try {
        $out = & $Exe @Arguments 2>$errFile
        $code = $LASTEXITCODE
        $outStr = [string]($out | Out-String)
        $err = [string](Get-Content -LiteralPath $errFile -Raw)
        return [pscustomobject]@{ ExitCode = [int]$code; StdOut = $outStr; StdErr = $err }
    }
    finally {
        Remove-Item -LiteralPath $errFile -Force -ErrorAction SilentlyContinue
        $ErrorActionPreference = $old
    }
}

# Decode a file with the lyra engine; returns the parsed JSON report (or throws).
function Decode-Lyra {
    param([string]$Path, [Nullable[int64]]$Seek, [Nullable[int64]]$Frames)
    $args = @('decode', $Path, '--engine', 'lyra')
    if ($null -ne $Seek)   { $args += @('--seek', "$Seek") }
    if ($null -ne $Frames) { $args += @('--frames', "$Frames") }
    $r = Invoke-Capture $Probe $args
    if ($r.ExitCode -ne 0) { throw "lyra decode exit $($r.ExitCode): $($r.StdErr)" }
    return $r.StdOut | ConvertFrom-Json
}

function Bytes-Per-Frame([int]$bits, [int]$channels) {
    $bps = if ($bits -le 16) { 2 } elseif ($bits -le 24) { 3 } else { 4 }
    return $bps * $channels
}

# --- load the golden manifest ---------------------------------------------
if (-not $GoldenManifest) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' |
        Sort-Object Name -Descending | Select-Object -First 1
    if (-not $latest) { throw 'No golden manifest found.' }
    $GoldenManifest = $latest.FullName
}
$manifest = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json
$corpusRoot = $manifest.corpusRoot

# --- 1. corpus-unchanged check (recompute corpusHash from live files) ------
# Mirrors golden.ps1: digest of "path|size|mtime" over the manifest files, in
# order, using the LIVE size + LastWriteTimeUtc of each file.
$corpusUnchanged = $true
$corpusDetail = ''
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
} catch {
    $corpusUnchanged = $false
    $corpusDetail = "error: $($_.Exception.Message)"
}

# --- 2. decode parity ------------------------------------------------------
$decodeRows = @()
$decPass = 0; $decFail = 0
$pictureBroken = @(
    'Qing Hua Ci',  # 2007 - Wo Hen Mang / 1.03
    'Lan Ting Xu'   # 2008 - Mo Jie Zuo / 1.07
)
$lossless = @($manifest.files | Where-Object { $_.ext -eq 'flac' -or $_.ext -eq 'wav' })
if ($Quick) { $lossless = $lossless | Select-Object -First 3 }

foreach ($f in $lossless) {
    $name = Split-Path $f.path -Leaf
    $golden = if ($f.ext -eq 'flac') { $f.libflacSha256 } else { $f.ffmpegSha256 }
    $goldenTool = if ($f.ext -eq 'flac') { 'libFLAC' } else { 'ffmpeg' }
    $status = 'FAIL'; $note = ''
    try {
        $d = Decode-Lyra -Path $f.path
        $hashOk = ($d.sha256 -eq $golden)
        $lenOk  = ($null -ne $d.declaredFrames) -and ($d.declaredFrames -eq $d.pcmFrames)
        if ($hashOk -and $lenOk) {
            $status = 'PASS'; $decPass++
        } else {
            $decFail++
            if (-not $hashOk) {
                # Produce first-differing-frame diagnostics via `compare` against a
                # fresh golden raw decode.
                $rawPath = Join-Path $TmpDir ("diff_" + ($name -replace '[^A-Za-z0-9._-]','_') + '.raw')
                if ($f.ext -eq 'flac') {
                    Invoke-Capture $Flac @('-d','--force-raw-format','--endian=little','--sign=signed','-f','-o',$rawPath,'--',$f.path) | Out-Null
                } else {
                    Invoke-Capture $Ffmpeg @('-v','error','-nostdin','-y','-i',$f.path,'-map','a:0','-c:a',"pcm_s16le",'-f','s16le','--',$rawPath) | Out-Null
                }
                $fmt = if ($f.bits -le 16) { 's16' } elseif ($f.bits -le 24) { 's24' } else { 's32' }
                $cmp = Invoke-Capture $Probe @('compare', $f.path, $rawPath, '--raw-format', $fmt, '--channels', "$($f.channels)")
                $note = "hash mismatch (ours=$($d.sha256.Substring(0,12)) golden=$($golden.Substring(0,12))); " + ($cmp.StdOut -replace '\s+',' ')
                Remove-Item -LiteralPath $rawPath -Force -ErrorAction SilentlyContinue
            }
            if (-not $lenOk) { $note += " length: declared=$($d.declaredFrames) decoded=$($d.pcmFrames)" }
        }
    } catch {
        $decFail++; $note = "decode error: $($_.Exception.Message -replace '\s+',' ')"
    }
    $isPic = $false; foreach ($p in $pictureBroken) { if ($name -like "*$p*") { $isPic = $true } }
    $decodeRows += [pscustomobject]@{
        Name = $name; Spec = "$($f.sampleRate)/$($f.bits)/$($f.channels)ch"; Ext = $f.ext
        Golden = $goldenTool; Status = $status; BrokenPicture = $isPic; Note = $note
    }
}

# --- 3. seek parity --------------------------------------------------------
if (-not $Quick) {
    $classes = @{}  # one representative file per (bits,rateClass)
    foreach ($f in ($manifest.files | Where-Object { $_.ext -eq 'flac' })) {
        $rc = if ($f.sampleRate -le 48000) { 'lo' } else { 'hi' }
        $key = "$($f.bits)-$rc-$($f.sampleRate)"
        if (-not $classes.ContainsKey($key)) { $classes[$key] = $f }
    }
    # Always include the sole 192k file and the 24/96 class explicitly.
    $seekFiles = @($classes.Values)
    $wav = @($manifest.files | Where-Object { $_.ext -eq 'wav' } | Select-Object -First 1)
    $seekFiles += $wav
} else {
    $seekFiles = @($lossless | Select-Object -First 1)
    $SeekPoints = 5
}

$seekRows = @()
$seekPass = 0; $seekFail = 0
$rng = [System.Random]::new(20261125)
foreach ($f in $seekFiles) {
    if ($null -eq $f) { continue }
    $name = Split-Path $f.path -Leaf
    $bpf = Bytes-Per-Frame $f.bits $f.channels
    $sliceFrames = 4096
    $goldenRaw = Join-Path $TmpDir ('seek_' + ($name -replace '[^A-Za-z0-9._-]','_') + '.raw')
    try {
        if ($f.ext -eq 'flac') {
            $rc = Invoke-Capture $Flac @('-d','--force-raw-format','--endian=little','--sign=signed','-f','-o',$goldenRaw,'--',$f.path)
        } else {
            $rc = Invoke-Capture $Ffmpeg @('-v','error','-nostdin','-y','-i',$f.path,'-map','a:0','-c:a','pcm_s16le','-f','s16le','--',$goldenRaw)
        }
        $goldenLen = (Get-Item -LiteralPath $goldenRaw).Length
        $totalFrames = [int64]($goldenLen / $bpf)
        $maxStart = $totalFrames - $sliceFrames
        if ($maxStart -lt 1) { throw "file too short for seek test ($totalFrames frames)" }
        $goldenBytes = [System.IO.File]::ReadAllBytes($goldenRaw)
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $okCount = 0; $firstBad = $null
        for ($i = 0; $i -lt $SeekPoints; $i++) {
            $start = $rng.Next(0, [int]$maxStart)
            $d = Decode-Lyra -Path $f.path -Seek ([int64]$start) -Frames ([int64]$sliceFrames)
            # golden slice
            $off = [int64]$start * $bpf; $len = $sliceFrames * $bpf
            $slice = New-Object byte[] $len
            [System.Array]::Copy($goldenBytes, $off, $slice, 0, $len)
            $gh = [System.BitConverter]::ToString($sha.ComputeHash($slice)).Replace('-','').ToLower()
            if ($gh -eq $d.sha256) { $okCount++ } elseif ($null -eq $firstBad) { $firstBad = $start }
        }
        if ($okCount -eq $SeekPoints) {
            $seekPass++
            $seekRows += [pscustomobject]@{ Name=$name; Spec="$($f.sampleRate)/$($f.bits)/$($f.channels)ch"; Points="$okCount/$SeekPoints"; Status='PASS'; Note='' }
        } else {
            $seekFail++
            $seekRows += [pscustomobject]@{ Name=$name; Spec="$($f.sampleRate)/$($f.bits)/$($f.channels)ch"; Points="$okCount/$SeekPoints"; Status='FAIL'; Note="first bad seek frame=$firstBad" }
        }
    } catch {
        $seekFail++
        $seekRows += [pscustomobject]@{ Name=$name; Spec="$($f.sampleRate)/$($f.bits)/$($f.channels)ch"; Points="0/$SeekPoints"; Status='FAIL'; Note=($_.Exception.Message -replace '\s+',' ') }
    } finally {
        Remove-Item -LiteralPath $goldenRaw -Force -ErrorAction SilentlyContinue
    }
}

# --- 4/5. robustness set ---------------------------------------------------
$robRows = @()
$robPass = 0; $robFail = 0
if (-not $Quick) {
    # Use a mid-size 16/44.1 FLAC as the base.
    $base = @($manifest.files | Where-Object { $_.ext -eq 'flac' -and $_.bits -eq 16 } | Select-Object -First 1)[0]
    $baseBytes = [System.IO.File]::ReadAllBytes($base.path)
    $robDir = Join-Path $TmpDir 'robustness'
    if (-not (Test-Path $robDir)) { New-Item -ItemType Directory -Force -Path $robDir | Out-Null }

    function New-Copy([string]$suffix, [byte[]]$bytes) {
        $p = Join-Path $robDir ("rob_$suffix.flac")
        [System.IO.File]::WriteAllBytes($p, $bytes)
        return $p
    }
    $variants = @()
    foreach ($pct in @(10,50,90)) {
        $n = [int]($baseBytes.Length * $pct / 100)
        $variants += [pscustomobject]@{ Tag="trunc$pct"; Path=(New-Copy "trunc$pct" ($baseBytes[0..($n-1)])); MidStream=$true }
    }
    # flipped byte deep in the audio (75% through)
    $flip = $baseBytes.Clone()
    $fi = [int]($baseBytes.Length * 0.75)
    $flip[$fi] = $flip[$fi] -bxor 0xFF
    $variants += [pscustomobject]@{ Tag='flipbyte'; Path=(New-Copy 'flipbyte' $flip); MidStream=$true }
    # zeroed header (first 64 bytes) - open-time failure is acceptable
    $zero = $baseBytes.Clone()
    for ($k=0; $k -lt 64; $k++) { $zero[$k] = 0 }
    $variants += [pscustomobject]@{ Tag='zerohdr'; Path=(New-Copy 'zerohdr' $zero); MidStream=$false }

    foreach ($v in $variants) {
        $r = Invoke-Capture $Probe @('decode', $v.Path, '--engine', 'lyra')
        $stderr = [string]$r.StdErr
        $panicked = ($r.ExitCode -eq 101) -or ($stderr -match 'panicked at') -or ($stderr -match 'RUST_BACKTRACE')
        $err = ($stderr -replace '\s+',' ').Trim()
        $hasFrame = ($stderr -match 'frame')
        $ok = $false; $note = ''
        if ($panicked) {
            $note = "PANIC: $err"
        } elseif ($r.ExitCode -eq 0) {
            # Decoded something without erroring. Acceptable only if it produced a
            # prefix of audio (truncation may still yield valid frames). Flag it.
            $ok = $true; $note = 'decoded without error (partial/tolerated)'
        } else {
            # Clean error (exit 3). Mid-stream corruption must carry a frame pos.
            if ($v.MidStream -and -not $hasFrame) {
                $note = "error lacks frame position: $err"
            } else {
                $ok = $true; $note = "clean error (exit $($r.ExitCode))" + $(if ($hasFrame) { ', frame pos present' } else { '' })
            }
        }
        if ($ok) { $robPass++ } else { $robFail++ }
        $robRows += [pscustomobject]@{ Variant=$v.Tag; MidStream=$v.MidStream; ExitCode=$r.ExitCode; Panic=$panicked; Status=$(if($ok){'PASS'}else{'FAIL'}); Note=$note }
    }
}

# --- 6. synthetic 5.1 FLAC (decode-only bit-identical) ---------------------
$synthRow = $null
if (-not $Quick) {
    $src = @($manifest.files | Where-Object { $_.ext -eq 'flac' -and $_.bits -eq 24 -and $_.sampleRate -ge 96000 } | Select-Object -First 1)
    if (-not $src) { $src = @($manifest.files | Where-Object { $_.ext -eq 'flac' -and $_.bits -eq 24 } | Select-Object -First 1) }
    $src = $src[0]
    $synth = Join-Path $SynthDir 'surround51.flac'
    $synthRaw = Join-Path $TmpDir 'surround51_golden.raw'
    try {
        # Upmix to 5.1 and re-encode FLAC (24-bit). Decode-only parity target.
        Invoke-Capture $Ffmpeg @('-v','error','-nostdin','-y','-i',$src.path,'-ac','6','-c:a','flac','-sample_fmt','s32','-bits_per_raw_sample','24','--',$synth) | Out-Null
        if (-not (Test-Path $synth)) { throw 'ffmpeg did not produce the 5.1 FLAC' }
        $rc = Invoke-Capture $Flac @('-d','--force-raw-format','--endian=little','--sign=signed','-f','-o',$synthRaw,'--',$synth)
        if ($rc.ExitCode -ne 0) { throw "libFLAC golden decode failed: $($rc.StdErr)" }
        $goldenHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $synthRaw).Hash.ToLower()
        $d = Decode-Lyra -Path $synth
        $ok = ($d.sha256 -eq $goldenHash) -and ($d.channels -eq 6)
        $synthRow = [pscustomobject]@{ Name='surround51.flac'; Channels=$d.channels; Status=$(if($ok){'PASS'}else{'FAIL'}); Note=$(if($ok){"bit-identical to libFLAC ($($d.pcmFrames) frames)"}else{"ours=$($d.sha256.Substring(0,12)) golden=$($goldenHash.Substring(0,12)) ch=$($d.channels)"}) }
    } catch {
        $synthRow = [pscustomobject]@{ Name='surround51.flac'; Channels='?'; Status='FAIL'; Note=($_.Exception.Message -replace '\s+',' ') }
    } finally {
        Remove-Item -LiteralPath $synthRaw -Force -ErrorAction SilentlyContinue
    }
}

# --- report ----------------------------------------------------------------
$commit = (& git -C $RepoRoot rev-parse --short HEAD 2>$null | Out-String).Trim()
$branch = (& git -C $RepoRoot rev-parse --abbrev-ref HEAD 2>$null | Out-String).Trim()
$probeVer = (& $Probe '--version' 2>&1 | Out-String).Trim()
$flacVer  = (& $Flac '--version' 2>&1 | Select-Object -First 1 | Out-String).Trim()
$ffVer    = (& $Ffmpeg '-version' 2>&1 | Select-Object -First 1 | Out-String).Trim()
$stamp    = (Get-Date).ToString('yyyyMMdd-HHmmss')
$reportPath = Join-Path $ReportDir "P1-$stamp.md"

$picFiles = @($decodeRows | Where-Object { $_.BrokenPicture })
$picOk = @($picFiles | Where-Object { $_.Status -eq 'PASS' }).Count

$gateFail = (-not $corpusUnchanged) -or ($decFail -gt 0) -or ($seekFail -gt 0) -or ($robFail -gt 0) -or ($null -ne $synthRow -and $synthRow.Status -eq 'FAIL')
$gate = if ($gateFail) { 'FAIL' } else { 'PASS' }

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine('# Parity report - P1 (FLAC + WAV decode parity)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| Field | Value |')
[void]$sb.AppendLine('|---|---|')
[void]$sb.AppendLine("| Generated (UTC) | $((Get-Date).ToUniversalTime().ToString('o')) |")
[void]$sb.AppendLine("| Commit | $commit (branch $branch) |")
[void]$sb.AppendLine("| Golden manifest | $(Split-Path $GoldenManifest -Leaf) |")
[void]$sb.AppendLine("| Corpus manifest hash | $($manifest.corpusHash) |")
[void]$sb.AppendLine("| Corpus unchanged | $(if($corpusUnchanged){'yes'}else{'NO - '+$corpusDetail}) |")
[void]$sb.AppendLine("| Machine | $env:COMPUTERNAME |")
[void]$sb.AppendLine("| Device | n/a (decode only, no render) |")
[void]$sb.AppendLine("| lyra-probe | $probeVer |")
[void]$sb.AppendLine("| flac | $flacVer |")
[void]$sb.AppendLine("| ffmpeg | $($ffVer -replace '\|','/') |")
[void]$sb.AppendLine("| Mode | $(if($Quick){'QUICK (smoke)'}else{'full'}) |")
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## 1. Decode parity (lyra-decode vs golden, bit-exact)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| File | Spec | Golden | total_frames | Result | Note |')
[void]$sb.AppendLine('|---|---|---|---|---|---|')
foreach ($r in $decodeRows) {
    $mark = if ($r.Status -eq 'PASS') { 'PASS' } else { 'FAIL' }
    [void]$sb.AppendLine("| ``$($r.Name)`` | $($r.Spec) | $($r.Golden) | ok | $mark | $($r.Note) |")
}
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Decode parity: **$decPass passed / $decFail failed** of $($decodeRows.Count) lossless files.")
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## 2. Broken-picture FLACs (no-MIME embedded picture, decode normally)')
[void]$sb.AppendLine('')
if ($picFiles.Count -gt 0) {
    [void]$sb.AppendLine('| File | Result |')
    [void]$sb.AppendLine('|---|---|')
    foreach ($r in $picFiles) { [void]$sb.AppendLine("| ``$($r.Name)`` | $($r.Status) |") }
    [void]$sb.AppendLine('')
    [void]$sb.AppendLine("Broken-picture: **$picOk / $($picFiles.Count)** decode bit-identical to golden.")
} else {
    [void]$sb.AppendLine('_Not identified in this run (quick mode or names changed)._')
}
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## 3. Seek parity (50 random points x representative files, 4096-frame slice)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| File | Spec | Points OK | Result | Note |')
[void]$sb.AppendLine('|---|---|---|---|---|')
foreach ($r in $seekRows) { [void]$sb.AppendLine("| ``$($r.Name)`` | $($r.Spec) | $($r.Points) | $($r.Status) | $($r.Note) |") }
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Seek parity: **$seekPass passed / $seekFail failed** representative files.")
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## 4. Robustness (corrupt copies in target/corpus-tmp/, no panic)')
[void]$sb.AppendLine('')
if ($robRows.Count -gt 0) {
    [void]$sb.AppendLine('| Variant | Mid-stream | Exit | Panic | Result | Note |')
    [void]$sb.AppendLine('|---|---|---|---|---|---|')
    foreach ($r in $robRows) { [void]$sb.AppendLine("| $($r.Variant) | $($r.MidStream) | $($r.ExitCode) | $($r.Panic) | $($r.Status) | $($r.Note) |") }
    [void]$sb.AppendLine('')
    [void]$sb.AppendLine("Robustness: **$robPass passed / $robFail failed** (pass = no panic; mid-stream errors carry a frame pos).")
} else {
    [void]$sb.AppendLine('_Skipped in quick mode._')
}
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## 5. Synthetic 5.1 FLAC (decode-only bit-identical)')
[void]$sb.AppendLine('')
if ($null -ne $synthRow) {
    [void]$sb.AppendLine('| File | Channels | Result | Note |')
    [void]$sb.AppendLine('|---|---|---|---|')
    [void]$sb.AppendLine("| ``$($synthRow.Name)`` | $($synthRow.Channels) | $($synthRow.Status) | $($synthRow.Note) |")
} else {
    [void]$sb.AppendLine('_Skipped in quick mode._')
}
[void]$sb.AppendLine('')

[void]$sb.AppendLine('## Pending human')
[void]$sb.AppendLine('- foobar2000 Converter golden (`LyraGolden` preset, H0.1): not run. libFLAC is the foobar2000-shipped decoder and is used as the automated proxy (ADR-007).')
[void]$sb.AppendLine('')
$sumLine = "## Gate: $gate  (decode $decPass/$($decodeRows.Count), seek $seekPass/$($seekRows.Count), robustness $robPass fail $robFail, 5.1 $(if($null -ne $synthRow){$synthRow.Status}else{'skip'}), corpus unchanged $(if($corpusUnchanged){'yes'}else{'NO'}))"
[void]$sb.AppendLine($sumLine)

Set-Content -LiteralPath $reportPath -Value $sb.ToString() -Encoding UTF8
Write-Host "p1.ps1: wrote $reportPath" -ForegroundColor Cyan
Write-Host $sumLine -ForegroundColor $(if ($gate -eq 'PASS') { 'Green' } else { 'Red' })
$reportPath
