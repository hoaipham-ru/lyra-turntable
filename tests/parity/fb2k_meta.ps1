<#
.SYNOPSIS
    Pull corpus metadata from foobar2000 through Beefweb (TEST-STRATEGY §6).

.DESCRIPTION
    foobar2000 is the comparison target. This reads, for each corpus file, the
    exact column set from TEST-STRATEGY §6 so that `lyra-probe meta` can later be
    diffed field-by-field. It:

      1. GETs /api/player FIRST. If foobar2000/Beefweb is unreachable it fails
         with a clear message. If something is *playing* it ABORTS and tells you
         to ask the orchestrator (never interrupt Hoai's playback - S09 rule).
      2. Creates a uniquely-named TEMPORARY playlist.
      3. Adds the corpus files to it, waits until all items are present.
      4. Reads the §6 columns for every item.
      5. Writes target/parity/fb2k-meta.json (path -> columns).
      6. DELETES the temporary playlist (always, even on error).

    It never calls play/stop/next and never touches any existing playlist's
    items, so a stopped foobar2000 is left exactly as it was.

    SAFETY: this script is PENDING-HUMAN in P0. foobar2000 must already be
    running with Beefweb up; this harness must NOT launch it unattended. Run it
    only when a human has foobar2000 open and nothing is playing.

.PARAMETER Base
    Beefweb base URL. Default http://localhost:8880 (matches src/player.ts).

.PARAMETER CorpusRoot
    Read-only corpus root. Default C:\SoulSeek_Music\complete.

.PARAMETER Out
    Output JSON. Default target/parity/fb2k-meta.json.

.PARAMETER Max
    Cap the number of files (0 = all). Default 0.

.PARAMETER Force
    Proceed even if the player state is not "stopped" (NOT recommended; the
    default refuses to run while anything is playing).

.EXAMPLE
    ./fb2k_meta.ps1              # all corpus files, requires fb2k up + stopped
#>
[CmdletBinding()]
param(
    [string]$Base = 'http://localhost:8880',
    [string]$CorpusRoot = 'C:\SoulSeek_Music\complete',
    [string]$Out,
    [int]$Max = 0,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
if (-not $Out) { $Out = Join-Path $RepoRoot 'target\parity\fb2k-meta.json' }
$OutDir = Split-Path $Out -Parent
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Force -Path $OutDir | Out-Null }

$ApiBase = ($Base.TrimEnd('/')) + '/api'

# TEST-STRATEGY §6 column set, in order.
$Columns = @(
    '%title%', '%artist%', '%album artist%', '%album%', '%tracknumber%',
    '%discnumber%', '%date%', '%genre%', '%length_samples%', '%samplerate%',
    '%bitspersample%', '%channels%', '%codec%',
    '%replaygain_track_gain%', '%replaygain_album_gain%'
)
# Stable keys for the JSON output, aligned 1:1 with $Columns.
$ColumnKeys = @(
    'title', 'artist', 'albumArtist', 'album', 'trackNumber',
    'discNumber', 'date', 'genre', 'lengthSamples', 'sampleRate',
    'bitsPerSample', 'channels', 'codec',
    'replaygainTrackGain', 'replaygainAlbumGain'
)

function Invoke-Api {
    param([string]$Method = 'GET', [string]$Path, [hashtable]$Query, $Body)
    $uri = $ApiBase + $Path
    if ($Query -and $Query.Count -gt 0) {
        $pairs = foreach ($k in $Query.Keys) {
            foreach ($v in @($Query[$k])) {
                "{0}={1}" -f [uri]::EscapeDataString($k), [uri]::EscapeDataString([string]$v)
            }
        }
        $uri += '?' + ($pairs -join '&')
    }
    $args = @{ Method = $Method; Uri = $uri; TimeoutSec = 15 }
    if ($null -ne $Body) {
        $args.Body = ($Body | ConvertTo-Json -Depth 6 -Compress)
        $args.ContentType = 'application/json'
    }
    return Invoke-RestMethod @args
}

# --- 1. Guard: player must be reachable and stopped -------------------------
Write-Host "fb2k_meta: GET $ApiBase/player" -ForegroundColor Cyan
try {
    $player = Invoke-Api -Path '/player'
}
catch {
    throw "Beefweb not reachable at $Base. foobar2000 must be running with the Beefweb plugin. (Do NOT launch it unattended - this is pending-human.) Underlying error: $($_.Exception.Message)"
}

$state = $null
if ($player -and $player.PSObject.Properties.Name -contains 'player' -and
    $player.player.PSObject.Properties.Name -contains 'playbackState') {
    $state = $player.player.playbackState
}
Write-Host "fb2k_meta: playbackState = $state"
if ($state -ne 'stopped' -and -not $Force) {
    throw "foobar2000 is '$state', not 'stopped'. Refusing to add a temp playlist while audio may be playing. Stop playback (or ask the orchestrator) and retry. Use -Force only with explicit approval."
}

# --- Resolve corpus files --------------------------------------------------
if (-not (Test-Path $CorpusRoot)) { throw "Corpus root not found: $CorpusRoot" }
$AudioExt = @('.flac', '.dff', '.dsf', '.mp3', '.wav', '.aiff', '.aif', '.m4a', '.ape', '.wv', '.opus', '.ogg')
$files = Get-ChildItem -Path $CorpusRoot -Recurse -File |
    Where-Object { $AudioExt -contains $_.Extension.ToLower() } |
    Sort-Object FullName
if ($Max -gt 0) { $files = $files | Select-Object -First $Max }
$paths = @($files | ForEach-Object { $_.FullName })
if ($paths.Count -eq 0) { throw 'No corpus files resolved.' }
Write-Host "fb2k_meta: $($paths.Count) corpus file(s)"

$tempTitle = "__lyra_parity_$([DateTime]::UtcNow.ToString('yyyyMMddHHmmss'))_$PID"
$tempId = $null

try {
    # --- 2. Create a temp playlist -----------------------------------------
    # Beefweb: POST /api/playlists/add (title, index). We then find it by title
    # because the add response shape varies by Beefweb version.
    Write-Host "fb2k_meta: creating temp playlist '$tempTitle'"
    Invoke-Api -Method 'POST' -Path '/playlists/add' -Query @{ title = $tempTitle; index = 1000000 } | Out-Null

    $pl = Invoke-Api -Path '/playlists'
    $match = @($pl.playlists | Where-Object { $_.title -eq $tempTitle })
    if ($match.Count -eq 0) { throw "temp playlist '$tempTitle' was not created (Beefweb add failed)." }
    $tempId = $match[0].id
    Write-Host "fb2k_meta: temp playlist id = $tempId"

    # --- 3. Add items, wait until all present ------------------------------
    # Add synchronously (async=false) so items exist before we read. Body form
    # is the documented Beefweb items/add payload.
    Invoke-Api -Method 'POST' -Path "/playlists/$tempId/items/add" `
        -Body @{ items = $paths; index = 0; async = $false; play = $false } | Out-Null

    $deadline = (Get-Date).AddSeconds(60)
    $count = 0
    do {
        Start-Sleep -Milliseconds 300
        $info = Invoke-Api -Path '/playlists'
        $cur = @($info.playlists | Where-Object { $_.id -eq $tempId })
        if ($cur.Count -gt 0) { $count = [int]$cur[0].itemCount }
    } while ($count -lt $paths.Count -and (Get-Date) -lt $deadline)
    if ($count -lt $paths.Count) {
        throw "only $count/$($paths.Count) items appeared in the temp playlist within the timeout."
    }

    # --- 4. Read §6 columns for every item ---------------------------------
    $colParam = ($Columns -join ',')
    $range = "0:$($paths.Count)"
    $items = Invoke-Api -Path "/playlists/$tempId/items/$range" -Query @{ columns = $colParam }
    $rows = @($items.playlistItems.items)
    if ($rows.Count -ne $paths.Count) {
        Write-Warning "read $($rows.Count) rows for $($paths.Count) files - reporting what came back."
    }

    $entries = @()
    for ($i = 0; $i -lt $rows.Count; $i++) {
        $cols = $rows[$i].columns
        $obj = [ordered]@{ path = $paths[$i] }
        for ($c = 0; $c -lt $ColumnKeys.Count; $c++) {
            $obj[$ColumnKeys[$c]] = if ($c -lt $cols.Count) { $cols[$c] } else { $null }
        }
        $entries += [pscustomobject]$obj
    }

    $doc = [pscustomobject][ordered]@{
        schema      = 'lyra-fb2k-meta/1'
        generatedAt = (Get-Date).ToUniversalTime().ToString('o')
        base        = $Base
        corpusRoot  = (Resolve-Path $CorpusRoot).Path
        playerState = $state
        columns     = $Columns
        columnKeys  = $ColumnKeys
        fileCount   = $entries.Count
        files       = $entries
    }
    $doc | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $Out -Encoding UTF8
    Write-Host "fb2k_meta: wrote $Out ($($entries.Count) files)" -ForegroundColor Green
}
finally {
    # --- 6. Always delete the temp playlist --------------------------------
    if ($tempId) {
        try {
            Invoke-Api -Method 'POST' -Path "/playlists/remove/$tempId" | Out-Null
            Write-Host "fb2k_meta: removed temp playlist $tempId"
        }
        catch {
            Write-Warning "failed to remove temp playlist $tempId ('$tempTitle'): $($_.Exception.Message). Remove it manually in foobar2000."
        }
    }
}

$Out
