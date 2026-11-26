<#
.SYNOPSIS
    Generate the synthetic corpus gap-fillers (TEST-STRATEGY §3).

.DESCRIPTION
    Writes everything under target/corpus-synth/ (never into the read-only
    corpus). Produces, from one 16/44.1 and one 24/96 corpus FLAC:

      formats/   ALAC, AIFF, WavPack, Opus, Vorbis, AAC (ffmpeg transcodes)
      gapless/   a 60 s continuous log-sweep + pink-noise 24/96 signal, kept
                 whole (FLAC) and split into 5 FLAC tracks, plus a .cue. The
                 5 tracks are exact 12 s sample ranges of the whole, so a
                 gapless queue of them must tap-equal the whole file.
      robustness/ copies of a corpus FLAC truncated at 10/50/90 %, one with a
                 flipped audio-frame byte, one with a zeroed header.

    Gaps recorded (not generated here):
      - APE : only if mac.exe (Monkey's Audio) is on PATH; else a documented gap.
      - DSF : needs `lyra-probe dsd convert` (P5 tool); documented gap.
      - DSD128/256 : no source files; documented gap.

    Idempotent: existing outputs are skipped unless -Force.

.PARAMETER OutRoot
    Output root. Default target/corpus-synth under the repo/worktree.

.PARAMETER CorpusRoot
    Read-only corpus root. Default C:\SoulSeek_Music\complete.

.PARAMETER Src1644
    16-bit/44.1 kHz source FLAC. Default: picked from the newest golden manifest.

.PARAMETER Src2496
    24-bit/96 kHz source FLAC. Default: picked from the newest golden manifest.

.PARAMETER Force
    Regenerate outputs that already exist.

.EXAMPLE
    ./synth.ps1
#>
[CmdletBinding()]
param(
    [string]$OutRoot,
    [string]$CorpusRoot = 'C:\SoulSeek_Music\complete',
    [string]$Src1644,
    [string]$Src2496,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $OutRoot) { $OutRoot = Join-Path $RepoRoot 'target\corpus-synth' }
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'

function Require-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if (-not $cmd) { throw "$name not found on PATH." }
    return $cmd.Source
}
$ffmpeg  = Require-Tool 'ffmpeg'
$ffprobe = Require-Tool 'ffprobe'

# Run ffmpeg without PS turning stderr into a terminating error; return exit code.
function Invoke-Ff {
    param([string[]]$Arguments)
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { & $ffmpeg @Arguments 2>$null; return $LASTEXITCODE }
    finally { $ErrorActionPreference = $old }
}

$generated = New-Object System.Collections.Generic.List[string]
$gaps      = New-Object System.Collections.Generic.List[string]

function New-Dir([string]$p) { if (-not (Test-Path $p)) { New-Item -ItemType Directory -Force -Path $p | Out-Null } }

function Probe-Bits([string]$path) {
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $j = & $ffprobe -v error -select_streams a:0 -show_entries 'stream=bits_per_raw_sample,bits_per_sample,sample_rate' -of json -- $path 2>$null | Out-String
    } finally { $ErrorActionPreference = $old }
    $s = ($j | ConvertFrom-Json).streams[0]
    $b = 0
    if ($s.PSObject.Properties.Name -contains 'bits_per_raw_sample' -and $s.bits_per_raw_sample) { $b = [int]$s.bits_per_raw_sample }
    if ($b -le 0 -and $s.PSObject.Properties.Name -contains 'bits_per_sample' -and $s.bits_per_sample) { $b = [int]$s.bits_per_sample }
    return $b
}

# --- Resolve the two source FLACs ------------------------------------------
if (-not $Src1644 -or -not $Src2496) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if (-not $latest) { throw 'No golden manifest to pick sources from; pass -Src1644 and -Src2496.' }
    $gm = Get-Content -LiteralPath $latest.FullName -Raw | ConvertFrom-Json
    if (-not $Src1644) {
        $pick = @($gm.files | Where-Object { $_.ext -eq 'flac' -and $_.sampleRate -eq 44100 -and $_.bits -eq 16 })[0]
        if (-not $pick) { throw 'No 16/44.1 FLAC in manifest; pass -Src1644.' }
        $Src1644 = $pick.path
    }
    if (-not $Src2496) {
        $pick = @($gm.files | Where-Object { $_.ext -eq 'flac' -and $_.sampleRate -eq 96000 -and $_.bits -eq 24 })[0]
        if (-not $pick) { throw 'No 24/96 FLAC in manifest; pass -Src2496.' }
        $Src2496 = $pick.path
    }
}
foreach ($s in @($Src1644, $Src2496)) {
    if (-not (Test-Path -LiteralPath $s)) { throw "Source not found: $s" }
}
Write-Host "synth: 16/44.1 source = $Src1644"
Write-Host "synth: 24/96   source = $Src2496"

# ---------------------------------------------------------------------------
# A. Format transcodes
# ---------------------------------------------------------------------------
$FormatsRoot = Join-Path $OutRoot 'formats'

function Convert-One {
    param([string]$src, [string]$outDir, [string]$leaf, [string[]]$codecArgs)
    New-Dir $outDir
    $dst = Join-Path $outDir $leaf
    if (-not $Force -and (Test-Path $dst)) { $generated.Add($dst); return }
    $rc = Invoke-Ff (@('-v', 'error', '-nostdin', '-y', '-i', $src, '-map', 'a:0') + $codecArgs + @('--', $dst))
    if ($rc -ne 0) {
        Write-Warning "  transcode FAILED ($leaf) rc=$rc"
        $gaps.Add("transcode failed: $leaf (rc=$rc)")
    } else {
        $generated.Add($dst)
        Write-Host "  + $leaf"
    }
}

foreach ($pair in @(
    @{ src = $Src1644; tag = '16-44'; aiff = 'pcm_s16be' },
    @{ src = $Src2496; tag = '24-96'; aiff = 'pcm_s24be' }
)) {
    $od = Join-Path $FormatsRoot $pair.tag
    Write-Host "synth: transcodes for $($pair.tag)"
    Convert-One $pair.src $od 'alac.m4a'  @('-c:a', 'alac')
    Convert-One $pair.src $od 'audio.aiff' @('-c:a', $pair.aiff)
    Convert-One $pair.src $od 'wavpack.wv' @('-c:a', 'wavpack')
    Convert-One $pair.src $od 'opus.opus'  @('-c:a', 'libopus', '-b:a', '192k')
    Convert-One $pair.src $od 'vorbis.ogg' @('-c:a', 'libvorbis', '-q:a', '8')
    Convert-One $pair.src $od 'aac.m4a'    @('-c:a', 'aac', '-b:a', '256k')
}

# ---------------------------------------------------------------------------
# B. Gapless set (60 s = 30 s log sweep + 30 s pink noise, 24/96 stereo)
# ---------------------------------------------------------------------------
$GaplessRoot = Join-Path $OutRoot 'gapless'
New-Dir $GaplessRoot
$whole = Join-Path $GaplessRoot 'gapless_whole.flac'
$cue   = Join-Path $GaplessRoot 'gapless_whole.cue'

# Log chirp: f(t) sweeps 20 Hz -> 20 kHz over T s. phase = 2*pi*f0*L*(e^(t/L)-1),
# L = T/ln(f1/f0). T=30, f0=20, f1=20000 -> L=4.342944, f0*L=86.85888.
$L = 30.0 / [math]::Log(1000.0)
$f0L = 20.0 * $L
$expr = ('0.5*sin(2*PI*{0}*(exp(t/{1})-1))' -f $f0L, $L)

if ($Force -or -not (Test-Path $whole)) {
    $sweep = Join-Path $GaplessRoot '_sweep.wav'
    $pink  = Join-Path $GaplessRoot '_pink.wav'
    $wholeWav = Join-Path $GaplessRoot '_whole.wav'

    $rc = Invoke-Ff @('-v', 'error', '-nostdin', '-y',
        '-f', 'lavfi', '-i', ("aevalsrc=exprs={0}|{0}:s=96000:d=30" -f $expr),
        '-c:a', 'pcm_s24le', '--', $sweep)
    if ($rc -ne 0) { throw "sweep generation failed (rc=$rc)" }

    $rc = Invoke-Ff @('-v', 'error', '-nostdin', '-y',
        '-f', 'lavfi', '-i', 'anoisesrc=color=pink:r=96000:d=30:amplitude=0.5',
        '-ac', '2', '-c:a', 'pcm_s24le', '--', $pink)
    if ($rc -ne 0) { throw "pink-noise generation failed (rc=$rc)" }

    $rc = Invoke-Ff @('-v', 'error', '-nostdin', '-y', '-i', $sweep, '-i', $pink,
        '-filter_complex', '[0][1]concat=n=2:v=0:a=1[o]', '-map', '[o]',
        '-c:a', 'pcm_s24le', '--', $wholeWav)
    if ($rc -ne 0) { throw "concat failed (rc=$rc)" }

    $rc = Invoke-Ff @('-v', 'error', '-nostdin', '-y', '-i', $wholeWav, '-c:a', 'flac', '--', $whole)
    if ($rc -ne 0) { throw "whole FLAC encode failed (rc=$rc)" }

    # 5 tracks, exact 12 s (1,152,000 sample) ranges of the whole WAV.
    for ($i = 0; $i -lt 5; $i++) {
        $start = $i * 12
        $trk = Join-Path $GaplessRoot ('track{0}.flac' -f ($i + 1))
        $rc = Invoke-Ff @('-v', 'error', '-nostdin', '-y', '-ss', "$start", '-t', '12',
            '-i', $wholeWav, '-c:a', 'flac', '--', $trk)
        if ($rc -ne 0) { throw "track $($i+1) encode failed (rc=$rc)" }
        $generated.Add($trk)
    }
    Remove-Item -LiteralPath $sweep, $pink, $wholeWav -Force
    $generated.Add($whole)
    Write-Host "  + gapless whole + 5 tracks (24/96)"
} else {
    $generated.Add($whole)
}

# .cue referencing the whole FLAC; 5 tracks at 12 s boundaries (MM:SS:FF).
$cueLines = @('REM Lyra synthetic gapless test (sweep+pink, 24/96)',
    'TITLE "Lyra Gapless Test"', 'PERFORMER "Lyra"',
    'FILE "gapless_whole.flac" WAVE')
for ($i = 0; $i -lt 5; $i++) {
    $mm = [int][math]::Floor(($i * 12) / 60); $ss = ($i * 12) % 60
    $cueLines += ('  TRACK {0:D2} AUDIO' -f ($i + 1))
    $cueLines += ('    TITLE "Part {0}"' -f ($i + 1))
    $cueLines += ('    INDEX 01 {0:D2}:{1:D2}:00' -f $mm, $ss)
}
Set-Content -LiteralPath $cue -Value ($cueLines -join "`r`n") -Encoding ASCII
$generated.Add($cue)
Write-Host "  + $([System.IO.Path]::GetFileName($cue))"

# ---------------------------------------------------------------------------
# C. Robustness set (copies of a corpus FLAC, mangled - never the original)
# ---------------------------------------------------------------------------
$RobRoot = Join-Path $OutRoot 'robustness'
New-Dir $RobRoot
$robSrcBytes = [System.IO.File]::ReadAllBytes($Src1644)
$srcLeaf = [System.IO.Path]::GetFileNameWithoutExtension($Src1644)

function Write-Bytes([string]$name, [byte[]]$bytes) {
    $p = Join-Path $RobRoot $name
    [System.IO.File]::WriteAllBytes($p, $bytes)
    $generated.Add($p)
    Write-Host "  + $name"
}

foreach ($pct in @(10, 50, 90)) {
    $n = [int]([math]::Floor($robSrcBytes.Length * $pct / 100.0))
    $slice = New-Object 'byte[]' $n
    [Array]::Copy($robSrcBytes, $slice, $n)
    Write-Bytes ("{0}.trunc{1}.flac" -f $srcLeaf, $pct) $slice
}

# Flip one byte deep in the audio-frame region (well past the header).
$flip = $robSrcBytes.Clone()
$flipPos = [int]($flip.Length / 2)
$flip[$flipPos] = $flip[$flipPos] -bxor 0xFF
Write-Bytes ("{0}.flipbyte.flac" -f $srcLeaf) $flip

# Zero the first 8 KiB (STREAMINFO/header region).
$zero = $robSrcBytes.Clone()
for ($i = 0; $i -lt [math]::Min(8192, $zero.Length); $i++) { $zero[$i] = 0 }
Write-Bytes ("{0}.zerohdr.flac" -f $srcLeaf) $zero

# ---------------------------------------------------------------------------
# D. Documented gaps
# ---------------------------------------------------------------------------
$mac = Get-Command 'mac.exe' -ErrorAction SilentlyContinue
if ($mac) {
    $apeDir = Join-Path $FormatsRoot '16-44'; New-Dir $apeDir
    $apeWav = Join-Path $apeDir '_ape.wav'
    $ape    = Join-Path $apeDir 'monkeys.ape'
    Invoke-Ff @('-v', 'error', '-nostdin', '-y', '-i', $Src1644, '-c:a', 'pcm_s16le', '--', $apeWav) | Out-Null
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { & $mac.Source $apeWav $ape '-c2000' 2>$null } finally { $ErrorActionPreference = $old }
    if (Test-Path $ape) { $generated.Add($ape); Write-Host "  + monkeys.ape" }
    Remove-Item -LiteralPath $apeWav -Force -ErrorAction SilentlyContinue
} else {
    $gaps.Add('APE: mac.exe (Monkey''s Audio) not on PATH - needs Hoai to install it.')
}
$gaps.Add('DSF: needs `lyra-probe dsd convert` (P5 tool); convert the 2 corpus DFF files then.')
$gaps.Add('DSD128/256: no source files in corpus; ask Hoai for samples (2L / NativeDSD).')

# ---------------------------------------------------------------------------
Write-Host ''
Write-Host "synth: generated $($generated.Count) file(s) under $OutRoot" -ForegroundColor Green
Write-Host "synth: $($gaps.Count) recorded gap(s):" -ForegroundColor Yellow
$gaps | ForEach-Object { Write-Host "  - $_" }

[pscustomobject]@{ outRoot = $OutRoot; generated = $generated.ToArray(); gaps = $gaps.ToArray() }
