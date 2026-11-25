# Parity manifests

Golden-reference manifests produced by `tests/parity/golden.ps1`. Manifests are
the **only** parity artifacts committed to git (ADR-006): they record hashes,
specs and tool versions, never audio or raw PCM. The raw references themselves
live under `target/parity/golden/` and are git-ignored.

## File naming

`golden-<yyyyMMdd>.json` — one manifest per run, dated. A newer run on the same
day overwrites the file. `run.ps1` consumes the newest manifest by name.

## Schema `lyra-golden/1`

```jsonc
{
  "schema": "lyra-golden/1",
  "generatedAt": "2026-11-25T12:00:00.0000000Z", // ISO-8601 UTC
  "machine": "HOST",                             // $env:COMPUTERNAME
  "corpusRoot": "C:\\SoulSeek_Music\\complete",  // read-only source root
  "corpusHash": "<sha256 hex>",                  // digest over each entry's path|size|mtime
  "toolVersions": {
    "flac":      "flac 1.5.0 ...",               // libFLAC CLI --version line
    "ffmpeg":    "ffmpeg version 9.1 ...",        // ffmpeg -version first line
    "lyraProbe": "lyra-probe 0.1.0"               // or "not-built"
  },
  "fileCount": 3,
  "files": [
    {
      "path": "C:\\SoulSeek_Music\\complete\\...\\track.flac", // absolute source path
      "ext": "flac",                               // lower-case extension, no dot
      "size": 43937712,                            // bytes
      "mtime": "2026-01-01T00:00:00.0000000Z",     // ISO-8601 UTC last-write
      "sampleRate": 44100,                         // Hz
      "bits": 16,                                  // valid bits per sample
      "channels": 2,
      "libflacSha256": "<hex>",   // SHA-256 of libFLAC raw decode; null for non-FLAC
      "ffmpegSha256":  "<hex>",   // SHA-256 of ffmpeg raw decode; null if ffmpeg failed
      "agree": true,              // libflacSha256 == ffmpegSha256; null when no libFLAC decode
      "fb2kStatus": "pending-human", // foobar2000 Converter golden status (see below)
      "note": null                // free-text caveat: DSD->PCM is not a parity golden,
                                  // or "ffmpeg decode failed (...)" for a coverage gap
    }
  ]
}
```

### Field notes

- **Raw PCM reference** = headerless interleaved little-endian PCM at the source
  rate/channels, container chosen by valid bit depth: `<=16` s16le, `<=24` s24le
  (packed 3-byte), else s32le. libFLAC uses
  `flac -d --force-raw-format --endian=little --sign=signed`; ffmpeg uses
  `-c:a pcm_s{16,24,32}le -f s{16,24,32}le`. The SHA-256 is over this raw payload
  only (no container header), so a WAV and a raw of the same decode hash equal.
- **`agree`** is the automated proxy for "foobar2000 would decode this
  identically", because foobar2000 ships libFLAC. `agree == false` on a lossless
  file is a **finding to report**, not a test to skip (TEST-STRATEGY §2).
- **`fb2kStatus`** is one of:
  - `pending-human` — the foobar2000 Converter `LyraGolden` golden was not built
    (default; needs a human preset and a `GET /api/player` check first).
  - `fb2k-not-implemented` — `-Fb2k` was requested but the Converter wiring is a
    later task.
  - (future) `ok` / a hash, once the Converter path lands.
- **`corpusHash`** lets every report assert the corpus is unchanged between runs.
  It is derived from `path|size|mtime` of the processed entries, not from audio
  content.
- **Coverage.** `golden.ps1 -FromCorpus -All` processes every audio file (FLAC,
  DFF/DSD, MP3, WAV). FLAC gets both libFLAC and ffmpeg (the `agree` proxy);
  non-FLAC gets ffmpeg only. DSD rows carry a `note` that ffmpeg's DSD→PCM is for
  coverage, not a parity reference (the real DSD golden is the raw payload, P5).
