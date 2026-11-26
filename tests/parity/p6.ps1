<#
.SYNOPSIS
    P6 library/metadata parity gate (TEST-STRATEGY §6, §9). Metadata, artwork and
    foobar2000-playlist migration vs foobar2000, plus corpus immutability.

.DESCRIPTION
    For every corpus file in the golden manifest this runs `lyra-probe meta --json`
    and then either:
      * diffs each §6 field against foobar2000 (when target/parity/fb2k-meta.json
        exists), applying the allowed differences (whitespace, multi-value
        separator, codec naming case); or
      * (the unattended default, no fb2k-meta.json) verifies INTERNAL CONSISTENCY:
        for lossless PCM, Lyra's samplerate/bits/channels equal the ffprobe-derived
        manifest values AND the decoder's own view, and length_samples equals the
        decoder's exact frame count; for DSD, codec=DSD and length_samples equals
        the lyra-dsd reader's total_frames; the foobar2000 field-by-field diff is
        recorded pending-human.
    It also reports Lyra's artwork-found counts (confirming the broken-picture FLACs
    do not error), migrates foobar2000 playlists from a READ-ONLY copy of the
    profile, and asserts the corpus (size+mtime, and the manifest corpus hash) is
    unchanged across the run.

    Never launches foobar2000 and never calls Beefweb. Writes only under target/.

.PARAMETER GoldenManifest
    Golden manifest JSON. Default: newest in tests/parity/manifests.

.PARAMETER Fb2kMeta
    foobar2000 metadata dump to diff against. Default: target/parity/fb2k-meta.json
    (when absent, the fb2k comparison is pending-human).

.PARAMETER Fb2kProfile
    foobar2000 v2 profile dir to migrate playlists from (read-only; copied first).
    Default: %APPDATA%\foobar2000-v2.

.PARAMETER Device
    Device string for the §9 header.

.EXAMPLE
    ./p6.ps1
#>
[CmdletBinding()]
param(
    [string]$GoldenManifest,
    [string]$Fb2kMeta,
    [string]$Fb2kProfile = (Join-Path $env:APPDATA 'foobar2000-v2'),
    [string]$Device = 'Senary Audio (built-in), shared mix 48 kHz/24-in-32 (ADR-009)'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$ReportDir   = Join-Path $RepoRoot 'target\parity\reports'
$ManifestDir = Join-Path $RepoRoot 'tests\parity\manifests'
$TmpDir      = Join-Path $RepoRoot 'target\corpus-tmp'
if (-not (Test-Path $ReportDir)) { New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null }
if (-not (Test-Path $TmpDir))    { New-Item -ItemType Directory -Force -Path $TmpDir    | Out-Null }

# --- Locate the probe binary ----------------------------------------------
$probeExe = @('release', 'debug') |
    ForEach-Object { Join-Path $RepoRoot "target\$_\lyra-probe.exe" } |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $probeExe) {
    Write-Host 'p6.ps1: building lyra-probe (release) ...' -ForegroundColor Cyan
    Push-Location $RepoRoot
    try { & cargo build -p lyra-probe --release 2>&1 | Out-Null } finally { Pop-Location }
    $probeExe = Join-Path $RepoRoot 'target\release\lyra-probe.exe'
}
if (-not (Test-Path $probeExe)) { throw 'lyra-probe.exe not found and could not be built' }
$probeVer = (& $probeExe '--version' 2>&1 | Out-String).Trim()

# --- Golden manifest -------------------------------------------------------
if (-not $GoldenManifest) {
    $latest = Get-ChildItem -Path $ManifestDir -Filter 'golden-*.json' -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1
    if ($latest) { $GoldenManifest = $latest.FullName }
}
if (-not ($GoldenManifest -and (Test-Path $GoldenManifest))) { throw 'no golden manifest found' }
$golden = Get-Content -LiteralPath $GoldenManifest -Raw | ConvertFrom-Json
$manifestCorpusHash = $golden.corpusHash

if (-not $Fb2kMeta) { $Fb2kMeta = Join-Path $RepoRoot 'target\parity\fb2k-meta.json' }
$haveFb2k = Test-Path $Fb2kMeta

# Index the foobar2000 dump by file path (only when present).
$script:fb2kIndex = @{}
if ($haveFb2k) {
    $fb2k = Get-Content -LiteralPath $Fb2kMeta -Raw | ConvertFrom-Json
    foreach ($row in $fb2k) { $script:fb2kIndex[$row.path] = $row }
}

# Compare one §6 field, applying the TEST-STRATEGY §6 allowed differences:
# whitespace trimming, multi-value separator (`; ` vs `, ` vs `/`), and codec
# naming case (`FLAC` vs `flac`). Returns @{ verdict = pass|warn|fail; reason }.
function Compare-Fb2kField {
    param([string]$Col, $Lyra, $Fb)
    $l = if ($null -ne $Lyra) { "$Lyra".Trim() } else { '' }
    $r = if ($null -ne $Fb)   { "$Fb".Trim() }   else { '' }
    if ($l -eq '' -and $r -eq '') { return @{ verdict = 'pass'; reason = '' } }
    if ($l -eq $r) { return @{ verdict = 'pass'; reason = '' } }

    if ($Col -eq 'codec') {
        if ($l.ToLower() -eq $r.ToLower()) { return @{ verdict = 'pass'; reason = '' } }
        return @{ verdict = 'warn'; reason = "codec naming $l/$r" }
    }
    if ($Col -in @('length_samples','samplerate','channels')) {
        if ($l -eq $r) { return @{ verdict = 'pass'; reason = '' } }
        return @{ verdict = 'fail'; reason = "$l vs $r" }
    }
    if ($Col -in @('artist','album_artist','genre')) {
        $ls = ($l -split '\s*[;,/]\s*' | Where-Object { $_ -ne '' } | Sort-Object) -join '|'
        $rs = ($r -split '\s*[;,/]\s*' | Where-Object { $_ -ne '' } | Sort-Object) -join '|'
        if ($ls -eq $rs) { return @{ verdict = 'warn'; reason = 'multi-value separator' } }
        return @{ verdict = 'fail'; reason = "'$l' vs '$r'" }
    }
    return @{ verdict = 'fail'; reason = "'$l' vs '$r'" }
}

$commit = (& git -C $RepoRoot rev-parse --short HEAD 2>$null | Out-String).Trim()
$branch = (& git -C $RepoRoot rev-parse --abbrev-ref HEAD 2>$null | Out-String).Trim()

# --- Corpus immutability snapshot (size+mtime), keyed by manifest order ----
function Get-CorpusDigest {
    param($files)
    # Must mirror golden.ps1 EXACTLY: one "path|size|mtime" line per file in
    # manifest order, joined with "`n" (no trailing newline), SHA-256 over UTF-8.
    $missing = @()
    $lines = foreach ($f in $files) {
        if (Test-Path -LiteralPath $f.path) {
            $it = Get-Item -LiteralPath $f.path
            "$($f.path)|$($it.Length)|$($it.LastWriteTimeUtc.ToString('o'))"
        } else {
            $missing += $f.path
            "$($f.path)|MISSING"
        }
    }
    $digestInput = $lines -join "`n"
    $hash = [System.BitConverter]::ToString(
        [System.Security.Cryptography.SHA256]::Create().ComputeHash(
            [System.Text.Encoding]::UTF8.GetBytes($digestInput))).Replace('-', '').ToLower()
    [pscustomobject]@{ Hash = $hash; Missing = $missing }
}

$snapBefore = Get-CorpusDigest $golden.files

# Expected codec per source extension (Lyra's canonical naming; §6 allows case
# differences vs foobar2000).
$codecByExt = @{ 'flac' = 'FLAC'; 'wav' = 'PCM'; 'mp3' = 'MP3'; 'dff' = 'DSD'; 'dsf' = 'DSD' }

# --- Per-file metadata pass ------------------------------------------------
$rows = @()
$artRows = @()
$nPass = 0; $nWarn = 0; $nFail = 0; $nPending = 0
$artEmbedded = 0; $artFolder = 0; $artNone = 0; $artError = 0
$brokenPicFlacs = @()

foreach ($f in $golden.files) {
    $name = Split-Path $f.path -Leaf
    $ext  = ($f.ext).ToLower()
    $expCodec = $codecByExt[$ext]

    $raw = & $probeExe meta $f.path --json 2>$null
    $code = $LASTEXITCODE
    $m = $null
    if ($code -eq 0 -and $raw) {
        try { $m = ($raw | Out-String | ConvertFrom-Json) } catch { $m = $null }
    }

    if (-not $m) {
        $nFail++
        $rows += "| ``$name`` | $ext | ❌ | meta failed (exit $code) |"
        continue
    }

    # Artwork accounting (Lyra's own view; cross-check vs fb2k is pending-human).
    $art = $m._artwork
    if ($art.found) {
        if ($art.source -eq 'embedded') { $artEmbedded++ } else { $artFolder++ }
        $artRows += "| ``$name`` | found | $($art.source) | $($art.mime) | $($art.picture_type) |"
    } elseif ($art.error) {
        $artError++
        $artRows += "| ``$name`` | ERROR | - | - | $($art.error) |"
    } else {
        $artNone++
        $artRows += "| ``$name`` | none | - | - | - |"
    }

    $issues = @()
    $status = 'pass'

    if ($haveFb2k) {
        # (Dormant until fb2k-meta.json exists.) Field-by-field vs foobar2000 with
        # allowed differences is applied by Compare-Fb2kField below.
        $fb = $script:fb2kIndex[$f.path]
        if (-not $fb) {
            $issues += 'no foobar2000 row'
            $status = 'warn'
        } else {
            foreach ($col in @('title','artist','album','album_artist','tracknumber','discnumber','date','genre','samplerate','channels','codec','replaygain_track_gain','replaygain_album_gain','length_samples')) {
                $r = Compare-Fb2kField $col $m.$col $fb.$col
                if ($r.verdict -eq 'fail') { $issues += "${col}: $($r.reason)"; $status = 'fail' }
                elseif ($r.verdict -eq 'warn' -and $status -ne 'fail') { $issues += "$col~$($r.reason)"; if ($status -eq 'pass') { $status = 'warn' } }
            }
        }
    }
    else {
        # INTERNAL CONSISTENCY (no foobar2000). Proves meta is self-consistent.
        # 1) codec matches the source container.
        if (-not $m.codec -or $m.codec.ToUpper() -ne $expCodec.ToUpper()) {
            $issues += "codec=$($m.codec) expected $expCodec"; $status = 'fail'
        }

        if ($ext -eq 'flac' -or $ext -eq 'wav') {
            # Lossless PCM: hard parity against ffprobe-manifest AND the decoder.
            if ($m.samplerate -ne $f.sampleRate) { $issues += "samplerate=$($m.samplerate) vs manifest $($f.sampleRate)"; $status = 'fail' }
            if ($m.bitspersample -ne $f.bits)    { $issues += "bits=$($m.bitspersample) vs manifest $($f.bits)"; $status = 'fail' }
            if ($m.channels -ne $f.channels)     { $issues += "channels=$($m.channels) vs manifest $($f.channels)"; $status = 'fail' }
            if ($null -eq $m.length_samples)     { $issues += 'length_samples null'; $status = 'fail' }
            elseif ($m.length_samples -ne $m._decoder.total_frames) { $issues += "length_samples=$($m.length_samples) vs decoder $($m._decoder.total_frames)"; $status = 'fail' }
            # tag-reported format must equal the decoder's own view.
            if ($m._decoder.samplerate -ne $m.samplerate) { $issues += 'tag/decoder samplerate differ'; $status = 'fail' }
            if ($m._decoder.channels   -ne $m.channels)   { $issues += 'tag/decoder channels differ'; $status = 'fail' }
            if ($m._decoder.bits       -ne $m.bitspersample) { $issues += 'tag/decoder bits differ'; $status = 'fail' }
        }
        elseif ($ext -eq 'dff' -or $ext -eq 'dsf') {
            # DSD: authoritative source is the lyra-dsd reader (meta uses it).
            if ($m.channels -ne $f.channels) { $issues += "channels=$($m.channels) vs manifest $($f.channels)"; $status = 'fail' }
            if ($null -eq $m.length_samples) { $issues += 'length_samples null'; $status = 'fail' }
            elseif ($m.length_samples -ne $m._decoder.total_frames) { $issues += 'length_samples != decoder total_frames'; $status = 'fail' }
            if ($m.bitspersample -ne 1) { $issues += "bits=$($m.bitspersample) (expected 1 for DSD bitstream)"; $status = 'fail' }
            # ffprobe models DSD as 8-bit/352800; Lyra reports 1-bit/2822400. Note it.
            $issues += "note: Lyra DSD $($m.samplerate)Hz/1bit vs ffprobe-manifest $($f.sampleRate)Hz/$($f.bits)bit (same DSD64)"
            if ($status -eq 'pass') { $status = 'warn' }
        }
        elseif ($ext -eq 'mp3') {
            # Lossy: format fields from tags; exact length needs the MP3 decoder,
            # which is P1b (not in P6). length null is EXPECTED, recorded pending.
            if ($m.samplerate -ne $f.sampleRate) { $issues += "samplerate=$($m.samplerate) vs manifest $($f.sampleRate)"; $status = 'fail' }
            if ($m.channels   -ne $f.channels)   { $issues += "channels=$($m.channels) vs manifest $($f.channels)"; $status = 'fail' }
            if ($null -eq $m.length_samples) {
                $issues += 'length_samples pending: lyra-decode has no MP3 path yet (P1b)'
                if ($status -eq 'pass') { $status = 'pending' }
            }
        }
    }

    switch ($status) {
        'pass'    { $nPass++;    $mark = '✅' }
        'warn'    { $nWarn++;    $mark = '⚠️' }
        'pending' { $nPending++; $mark = '⏳' }
        default   { $nFail++;    $mark = '❌' }
    }
    $reason = if ($issues.Count) { ($issues -join '; ') } else { 'all §6 fields consistent' }
    $rows += "| ``$name`` | $ext | $mark | $reason |"
}

# --- Playlist migration from a READ-ONLY copy of the profile ---------------
$plSection = @()
$plStatus = 'skipped'
$profilePlDir = Join-Path $Fb2kProfile 'playlists-v2.0'
if (Test-Path $profilePlDir) {
    $copyRoot = Join-Path $TmpDir 'fb2k-profile-copy'
    $copyPlDir = Join-Path $copyRoot 'playlists-v2.0'
    if (Test-Path $copyRoot) { Remove-Item -Recurse -Force $copyRoot }
    New-Item -ItemType Directory -Force -Path $copyPlDir | Out-Null
    # Copy only the text index + .fplite files (read-only source; copy is writable
    # target). The -props.sqlite blobs are not needed by the migrator.
    Copy-Item -LiteralPath (Join-Path $profilePlDir 'index.txt') -Destination $copyPlDir -ErrorAction SilentlyContinue
    Get-ChildItem -LiteralPath $profilePlDir -Filter '*.fplite' | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $copyPlDir
    }

    Write-Host 'p6.ps1: migrating foobar2000 playlists (read-only copy) ...' -ForegroundColor Cyan
    Push-Location $RepoRoot
    $savedEap = $ErrorActionPreference
    try {
        # cargo writes compile progress to stderr; under -ErrorActionPreference Stop
        # that would be treated as a terminating error, so relax it for this call.
        $ErrorActionPreference = 'Continue'
        $env:LYRA_FB2K_PROFILE = $copyRoot
        $plOut = & cargo test -p lyra-library migrate_real_profile_opt_in -- --ignored --nocapture 2>&1 | Out-String
    } finally {
        $ErrorActionPreference = $savedEap
        Remove-Item Env:\LYRA_FB2K_PROFILE -ErrorAction SilentlyContinue
        Pop-Location
    }
    # Pull the "OK/FAIL <title>: N imported" lines the test prints.
    $plLines = $plOut -split "`r?`n" | Where-Object { $_ -match '^(OK|FAIL)\s' -or $_ -match '^\s+-\s' }
    if ($plLines) {
        $plStatus = 'done'
        $plSection = $plLines
    } else {
        $plStatus = 'no-output'
        $plSection = @($plOut.Trim())
    }
} else {
    $plSection = @("Profile playlists dir not found: $profilePlDir")
}

# --- Corpus immutability (after) -------------------------------------------
$snapAfter = Get-CorpusDigest $golden.files
$corpusUnchangedRun = ($snapBefore.Hash -eq $snapAfter.Hash)
$corpusMatchesManifest = ($snapAfter.Hash -eq $manifestCorpusHash)

# --- Report ----------------------------------------------------------------
$stamp = (Get-Date).ToString('yyyyMMdd-HHmmss')
$reportPath = Join-Path $ReportDir "P6-$stamp.md"
$fileCount = $golden.files.Count

$metaGate = if ($nFail -eq 0) { 'PASS (internal consistency)' } else { "FAIL ($nFail files)" }
$immGate  = if ($corpusUnchangedRun -and $corpusMatchesManifest) { 'PASS' } else { 'FAIL' }

$sb = [System.Text.StringBuilder]::new()
[void]$sb.AppendLine('# Parity report - P6 (library & metadata)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| Field | Value |')
[void]$sb.AppendLine('|---|---|')
[void]$sb.AppendLine("| Generated (UTC) | $((Get-Date).ToUniversalTime().ToString('o')) |")
[void]$sb.AppendLine("| Commit | $commit (branch $branch) |")
[void]$sb.AppendLine("| Corpus manifest | $(Split-Path $GoldenManifest -Leaf) |")
[void]$sb.AppendLine("| Corpus manifest hash | $manifestCorpusHash |")
[void]$sb.AppendLine("| Corpus hash now (disk) | $($snapAfter.Hash) |")
[void]$sb.AppendLine("| Machine | $env:COMPUTERNAME |")
[void]$sb.AppendLine("| Device | $Device |")
[void]$sb.AppendLine("| lyra-probe | $probeVer |")
[void]$sb.AppendLine("| fb2k-meta.json | $(if ($haveFb2k) { $Fb2kMeta } else { 'absent → fb2k diff pending-human' }) |")
[void]$sb.AppendLine("| Files | $fileCount |")
[void]$sb.AppendLine('')
[void]$sb.AppendLine("## Metadata parity (§6) — mode: $(if ($haveFb2k) { 'foobar2000 diff' } else { 'internal consistency' })")
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Summary: $nPass ✅ pass · $nWarn ⚠️ warn/note · $nPending ⏳ pending · $nFail ❌ fail (of $fileCount)")
[void]$sb.AppendLine('')
if (-not $haveFb2k) {
    [void]$sb.AppendLine('> foobar2000 field-by-field comparison is **pending human**: run `tests/parity/fb2k_meta.ps1`')
    [void]$sb.AppendLine('> (after `GET /api/player` shows nothing playing) to produce `target/parity/fb2k-meta.json`,')
    [void]$sb.AppendLine('> then re-run `./p6.ps1`. The checks below prove `lyra-probe meta` is internally consistent.')
    [void]$sb.AppendLine('')
}
[void]$sb.AppendLine('| File | Ext | Result | Notes |')
[void]$sb.AppendLine('|---|---|---|---|')
foreach ($r in $rows) { [void]$sb.AppendLine($r) }
[void]$sb.AppendLine('')
[void]$sb.AppendLine('## Artwork (Lyra view; fb2k cross-check pending-human)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Found: $artEmbedded embedded, $artFolder folder, $artNone none, $artError error (of $fileCount).")
[void]$sb.AppendLine('The exit gate "artwork for every file foobar2000 shows artwork" needs foobar2000 and is **pending-human**.')
[void]$sb.AppendLine('No file errored during artwork lookup (broken-picture FLACs tolerated) unless an ERROR row appears below.')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('| File | Artwork | Source | MIME | Picture type / error |')
[void]$sb.AppendLine('|---|---|---|---|---|')
foreach ($r in $artRows) { [void]$sb.AppendLine($r) }
[void]$sb.AppendLine('')
[void]$sb.AppendLine('## Playlist migration (read-only profile copy)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("Source profile: ``$Fb2kProfile`` (copied to target/corpus-tmp/fb2k-profile-copy; original never written).")
[void]$sb.AppendLine("Status: $plStatus. The 'same items in same order as Beefweb' cross-check is **pending-human**.")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('```')
foreach ($l in $plSection) { [void]$sb.AppendLine($l) }
[void]$sb.AppendLine('```')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('## Corpus immutability (exit gate)')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("| Check | Result |")
[void]$sb.AppendLine("|---|---|")
[void]$sb.AppendLine("| Corpus size+mtime unchanged across this run | $(if ($corpusUnchangedRun) { '✅ yes' } else { '❌ CHANGED' }) |")
[void]$sb.AppendLine("| Corpus hash matches P0 manifest | $(if ($corpusMatchesManifest) { '✅ yes' } else { '❌ differs' }) |")
[void]$sb.AppendLine("| Missing files | $(if ($snapAfter.Missing.Count -eq 0) { 'none' } else { ($snapAfter.Missing -join ', ') }) |")
[void]$sb.AppendLine('')
[void]$sb.AppendLine('## Pending human')
[void]$sb.AppendLine('')
[void]$sb.AppendLine('- foobar2000 field-by-field metadata diff (needs `fb2k-meta.json` via Beefweb; check `GET /api/player` first).')
[void]$sb.AppendLine('- Artwork cross-check: every file where foobar2000 shows artwork, Lyra also finds it.')
[void]$sb.AppendLine('- Playlist order cross-check: imported lists match what Beefweb shows, item for item.')
[void]$sb.AppendLine('')
[void]$sb.AppendLine("## Gate: metadata=$metaGate · immutability=$immGate")
[void]$sb.AppendLine('')
$overall = if ($nFail -eq 0 -and $immGate -eq 'PASS') { 'PASS (automatable parts; fb2k cross-checks pending-human)' } else { 'FAIL' }
[void]$sb.AppendLine("## Overall: $overall")

Set-Content -LiteralPath $reportPath -Value $sb.ToString() -Encoding UTF8
Write-Host "p6.ps1: wrote report $reportPath" -ForegroundColor Cyan
Write-Host "p6.ps1: meta $nPass pass / $nWarn warn / $nPending pending / $nFail fail; immutability $immGate" -ForegroundColor $(if ($nFail -eq 0 -and $immGate -eq 'PASS') { 'Green' } else { 'Red' })
$reportPath
