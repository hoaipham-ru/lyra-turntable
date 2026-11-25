<#
.SYNOPSIS
    Build golden PCM references for parity testing (TEST-STRATEGY §2).

.DESCRIPTION
    For each FLAC it decodes the raw interleaved PCM twice with two independent
    tools - libFLAC (`flac -d --force-raw-format`, the decoder foobar2000 ships)
    and ffmpeg - hashes both (SHA-256) and records whether they agree. The
    foobar2000 Converter golden is OFF by default and recorded as
    "pending-human" unless -Fb2k is given.

    It writes raw references under target/parity/golden/{libflac,ffmpeg}/ and a
    manifest tests/parity/manifests/golden-<yyyyMMdd>.json. It NEVER writes into
    the corpus (ADR-006) and is idempotent: existing raws are reused unless
    -Force is passed.

.PARAMETER Files
    Explicit list of audio files to process.

.PARAMETER FromCorpus
    Pick files from the corpus instead of -Files.

.PARAMETER CorpusRoot
    Corpus root (read-only). Default C:\SoulSeek_Music\complete.

.PARAMETER Max
    Max number of corpus files to process (keeps smoke runs fast). Default 3.
    Ignored when -All is given.

.PARAMETER All
    Process every audio file in the corpus (FLAC, DFF/DSD, MP3, WAV), no cap.
    This is the P0.5 full-coverage run (82/82).

.PARAMETER Fb2k
    Run the foobar2000 Converter golden. OFF by default (needs a human preset).

.PARAMETER Force
    Re-decode even if a raw reference already exists.

.PARAMETER NoKeep
    Delete each raw decode after hashing it (manifest still records the hash).
    Keeps disk small on a full corpus run; raws are regenerable from the corpus.

.EXAMPLE
    ./golden.ps1 -FromCorpus -Max 3

.EXAMPLE
    ./golden.ps1 -FromCorpus -All -NoKeep
#>
[CmdletBinding()]
param(
    [string[]]$Files,
    [switch]$FromCorpus,
    [string]$CorpusRoot = 'C:\SoulSeek_Music\complete',
    [int]$Max = 3,
    [switch]$All,
    [switch]$Fb2k,
    [switch]$Force,
    [switch]$NoKeep
)

# Audio extensions we treat as corpus inputs for golden coverage.
$AudioExt = @('.flac', '.dff', '.dsf', '.mp3', '.wav', '.aiff', '.aif', '.m4a', '.ape', '.wv', '.opus', '.ogg')

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$GoldenRoot  = Join-Path $RepoRoot 'target\parity\golden'
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
$LibflacDir  = Join-Path $GoldenRoot 'libflac'
$FfmpegDir   = Join-Path $GoldenRoot 'ffmpeg'
$Fb2kDir     = Join-Path $GoldenRoot 'fb2k'

foreach ($d in @($LibflacDir, $FfmpegDir, $ManifestDir)) {
    if (-not (Test-Path $d)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
}

# Run a native exe without PS turning its stderr into a terminating error
# (PowerShell 5.1 does that under ErrorActionPreference='Stop'). Returns the
# process exit code; stderr is discarded.
function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        & $Exe @Arguments 2>$null
        return $LASTEXITCODE
    }
    finally { $ErrorActionPreference = $old }
}

function Resolve-Flac {
    $cmd = Get-Command flac -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $fallback = 'C:\Users\LENOVO\AppData\Local\Microsoft\WinGet\Packages\Xiph.FLAC_Microsoft.Winget.Source_8wekyb3d8bbwe\flac-1.5.0-win\Win64\flac.exe'
    if (Test-Path $fallback) { return $fallback }
    throw "flac CLI not found: not on PATH and not at the known winget path. Install libFLAC or add it to PATH."
}

function Require-Tool([string]$name) {
    $cmd = Get-Command $name -ErrorAction SilentlyContinue
    if (-not $cmd) { throw "$name not found on PATH." }
    return $cmd.Source
}

function Resolve-Probe {
    foreach ($cfg in @('release', 'debug')) {
        $p = Join-Path $RepoRoot "target\$cfg\lyra-probe.exe"
        if (Test-Path $p) { return $p }
    }
    return $null  # optional: only used for its version string in the manifest
}

# Mirror pcm::format_for_bits: pick narrowest lossless container.
function Get-Format([int]$bits) {
    if ($bits -le 16) { return 's16le' }
    elseif ($bits -le 24) { return 's24le' }
    else { return 's32le' }
}

function Get-SafeName([string]$path) {
    $hash = [System.BitConverter]::ToString(
        [System.Security.Cryptography.SHA1]::Create().ComputeHash(
            [System.Text.Encoding]::UTF8.GetBytes($path))).Replace('-', '').Substring(0, 12).ToLower()
    $leaf = [System.IO.Path]::GetFileNameWithoutExtension($path)
    $clean = ($leaf -replace '[^A-Za-z0-9._-]', '_')
    if ($clean.Length -gt 40) { $clean = $clean.Substring(0, 40) }
    return "$clean.$hash"
}

function Probe-Spec([string]$ffprobe, [string]$path) {
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try {
        $json = & $ffprobe -v error -select_streams a:0 -show_entries `
            'stream=sample_rate,channels,bits_per_raw_sample,bits_per_sample,sample_fmt' `
            -of json -- $path 2>$null | Out-String
    }
    finally { $ErrorActionPreference = $old }
    $o = ($json | ConvertFrom-Json).streams[0]
    $bits = 0
    if ($o.PSObject.Properties.Name -contains 'bits_per_raw_sample' -and $o.bits_per_raw_sample) {
        $bits = [int]$o.bits_per_raw_sample
    }
    if ($bits -le 0 -and $o.PSObject.Properties.Name -contains 'bits_per_sample' -and $o.bits_per_sample) {
        $bits = [int]$o.bits_per_sample
    }
    if ($bits -le 0) {
        switch -Regex ($o.sample_fmt) { 's16' { $bits = 16 } 's32|flt' { $bits = 32 } default { $bits = 16 } }
    }
    return [pscustomobject]@{
        rate     = [int]$o.sample_rate
        channels = [int]$o.channels
        bits     = $bits
    }
}

$flac    = Resolve-Flac
$ffmpeg  = Require-Tool 'ffmpeg'
$ffprobe = Require-Tool 'ffprobe'
$probe   = Resolve-Probe

function Get-ToolVersion([string]$exe, [string]$arg) {
    $old = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
    try { return (& $exe $arg 2>&1 | Select-Object -First 1 | Out-String).Trim() }
    catch { return 'unknown' }
    finally { $ErrorActionPreference = $old }
}
$flacVer  = Get-ToolVersion $flac '--version'
$ffVer    = Get-ToolVersion $ffmpeg '-version'
$probeVer = if ($probe) { Get-ToolVersion $probe '--version' } else { 'not-built' }

# --- Resolve the file list -------------------------------------------------
if ($FromCorpus) {
    if (-not (Test-Path $CorpusRoot)) { throw "Corpus root not found: $CorpusRoot" }
    # NB: do not name this $all - it would alias the [switch]$All parameter
    # (PowerShell variables are case-insensitive) and keep its switch type.
    $found = Get-ChildItem -Path $CorpusRoot -Recurse -File |
        Where-Object { $AudioExt -contains $_.Extension.ToLower() } |
        Sort-Object FullName
    if (-not $All) { $found = $found | Select-Object -First $Max }
    $targets = $found | ForEach-Object { $_.FullName }
}
elseif ($Files) {
    $targets = $Files
}
else {
    throw 'Provide -Files <paths> or -FromCorpus.'
}
if (-not $targets -or $targets.Count -eq 0) { throw 'No input files resolved.' }

Write-Host "golden.ps1: processing $($targets.Count) file(s)" -ForegroundColor Cyan

$entries = @()
foreach ($src in $targets) {
    $full = (Resolve-Path -LiteralPath $src).Path
    # Guard: never write into the corpus.
    if ($full.StartsWith((Resolve-Path $CorpusRoot).Path, [StringComparison]::OrdinalIgnoreCase)) {
        # read-only source, fine; our outputs go under $GoldenRoot
    }
    $item = Get-Item -LiteralPath $full
    $ext = $item.Extension.ToLower()
    $isFlac = $ext -eq '.flac'
    $isDsd  = ($ext -eq '.dff' -or $ext -eq '.dsf')
    $safe = Get-SafeName $full
    $spec = Probe-Spec $ffprobe $full
    $fmt  = Get-Format $spec.bits

    Write-Host "  $($item.Name)  [$($spec.rate) Hz / $($spec.bits)-bit / $($spec.channels)ch -> $fmt]"

    # ffmpeg raw decode (third-opinion reference for all; the real DSD golden is
    # the raw payload compared later - ffmpeg's DSD->PCM here is NOT a parity
    # reference, only coverage, hence the note).
    $ffHash = $null
    $note   = $null
    $ffRaw  = Join-Path $FfmpegDir "$safe.raw"
    if ($Force -or -not (Test-Path $ffRaw)) {
        $rc = Invoke-Native $ffmpeg @('-v', 'error', '-nostdin', '-y', '-i', $full, '-map', 'a:0', '-c:a', "pcm_$fmt", '-f', $fmt, '--', $ffRaw)
        if ($rc -ne 0) {
            if ($isFlac) { throw "ffmpeg failed on $full (exit $rc)" }
            $note = "ffmpeg decode failed (exit $rc)"
            Write-Warning "    ffmpeg decode failed on $($item.Name) (exit $rc) - recorded as a gap"
        }
    }
    if (Test-Path $ffRaw) {
        $ffHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $ffRaw).Hash.ToLower()
        if ($NoKeep) { Remove-Item -LiteralPath $ffRaw -Force }
    }
    if ($isDsd -and $null -eq $note) {
        $note = 'ffmpeg DSD->PCM, not a parity golden (DSD payload compare is P5/pending-human)'
    }

    # libFLAC raw decode (FLAC only)
    $libHash = $null
    if ($isFlac) {
        $libRaw = Join-Path $LibflacDir "$safe.raw"
        if ($Force -or -not (Test-Path $libRaw)) {
            $rc = Invoke-Native $flac @('-d', '--force-raw-format', '--endian=little', '--sign=signed', '-f', '-o', $libRaw, '--', $full)
            if ($rc -ne 0) { throw "flac failed on $full (exit $rc)" }
        }
        $libHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $libRaw).Hash.ToLower()
        if ($NoKeep) { Remove-Item -LiteralPath $libRaw -Force }
    }

    $agree = if ($null -ne $libHash) { [bool]($libHash -eq $ffHash) } else { $null }
    if ($isFlac) {
        $mark = if ($agree) { 'agree' } else { 'DISAGREE' }
        $color = if ($agree) { 'Green' } else { 'Red' }
        Write-Host "    libFLAC vs ffmpeg: $mark" -ForegroundColor $color
    }

    $fb2kStatus = 'pending-human'
    if ($Fb2k) {
        # Placeholder: the Converter golden path is wired in a later task; before
        # using foobar2000 we must check GET /api/player (never interrupt playback).
        $fb2kStatus = 'fb2k-not-implemented'
    }

    $entries += [pscustomobject][ordered]@{
        path          = $full
        ext           = $ext.TrimStart('.')
        size          = $item.Length
        mtime         = $item.LastWriteTimeUtc.ToString('o')
        sampleRate    = $spec.rate
        bits          = $spec.bits
        channels      = $spec.channels
        libflacSha256 = $libHash
        ffmpegSha256  = $ffHash
        agree         = $agree
        fb2kStatus    = $fb2kStatus
        note          = $note
    }
}

# Corpus manifest hash: a stable digest over (path,size,mtime) of all entries,
# so a report can assert the corpus did not change between runs.
$corpusDigestInput = ($entries | ForEach-Object { "$($_.path)|$($_.size)|$($_.mtime)" }) -join "`n"
$corpusHash = [System.BitConverter]::ToString(
    [System.Security.Cryptography.SHA256]::Create().ComputeHash(
        [System.Text.Encoding]::UTF8.GetBytes($corpusDigestInput))).Replace('-', '').ToLower()

$manifest = [pscustomobject][ordered]@{
    schema       = 'lyra-golden/1'
    generatedAt  = (Get-Date).ToUniversalTime().ToString('o')
    machine      = $env:COMPUTERNAME
    corpusRoot   = (Resolve-Path $CorpusRoot).Path
    corpusHash   = $corpusHash
    toolVersions = [ordered]@{
        flac      = $flacVer
        ffmpeg    = $ffVer
        lyraProbe = $probeVer
    }
    fileCount    = $entries.Count
    files        = $entries
}

$stamp        = (Get-Date).ToString('yyyyMMdd')
$manifestPath = Join-Path $ManifestDir "golden-$stamp.json"
$manifest | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $manifestPath -Encoding UTF8

Write-Host "golden.ps1: wrote manifest $manifestPath" -ForegroundColor Cyan
$disagree = @($entries | Where-Object { $_.agree -eq $false })
if ($disagree.Count -gt 0) {
    Write-Warning "libFLAC and ffmpeg DISAGREE on $($disagree.Count) file(s) - this is a finding to report."
    $disagree | ForEach-Object { Write-Warning "  $($_.path)" }
}
# Return the manifest path for callers/pipelines.
$manifestPath
