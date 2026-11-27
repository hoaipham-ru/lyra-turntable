# Changelog

All notable changes to Lyra Turntable are documented here.
This project follows [Semantic Versioning](https://semver.org/).

## 1.0.0 — 2026-11-27

**Lyra is now a standalone high-end audio engine.** It decodes and plays FLAC, WAV and
DSD (DSF/DFF) bit-perfect over WASAPI exclusive / ASIO, and **no longer requires
foobar2000**. foobar2000 remains only as an optional, hidden legacy backend.

### Added
- **Native Lyra Engine** (Rust, one crate per service): the default and only normal
  backend. Installing Lyra alone gives bit-perfect playback.
  - **FLAC + WAV** decoding, **bit-identical to libFLAC** across the test corpus.
  - Bit-perfect output to the **WASAPI exclusive / ASIO** API boundary, with no
    resampling, mixing or digital volume along the way.
  - **DSD (DSF/DFF, uncompressed)** playback. DSD→PCM works on any device; **native DSD
    over ASIO** and **DoP over WASAPI** are implemented and unit-tested (see Pending).
  - **Gapless** playback and **cue sheet** support.
  - **Metadata, embedded artwork and playlist** parity with the previous foobar2000 path.
- **Bit-perfect badge** on the turntable showing the exact path (e.g.
  `FLAC 24/96 → 24/96 ✓`, `DSD64 → DoP ✓`, `→ shared 48k ✗`), green for bit-perfect,
  amber for a device-forced conversion. Also surfaces output underruns.
- **Settings panel** (⚙): backend, output device, output mode (exclusive / shared /
  ASIO) and DSD routing (auto / DoP / native / PCM).
- **Volume knob locked at 0 dB** in bit-perfect mode, with a tooltip explaining why; set
  the level on the DAC or amplifier.
- **One-time, read-only import** of foobar2000 v2 playlists on first native startup, so
  returning users keep their lists. foobar2000 files are never modified.
- Single string table (`src/strings.ts`) with parallel Vietnamese and English entries,
  ready for a language toggle.

### Changed
- **foobar2000 cutover (P8.4):** the native engine is the default and only path a fresh
  user lands on. foobar2000 is now a hidden **Advanced / Legacy** backend; Lyra never
  launches or requires it, and launches it only when the legacy backend is explicitly
  selected and foobar2000 is actually installed.
- Switching backends always **stops the backend being left first**, so two players never
  produce sound at once.
- README rewritten around the standalone 1.0 story; requirements, install, bit-perfect
  guide and supported-format list updated to 1.0 scope.
- Version bumped to **1.0.0** (`src-tauri/tauri.conf.json`, `turntable` crate).

### Preserved (unchanged behaviour)
- *Default Playlist* rules (emptied on startup unless you added to it or it is playing;
  *Open with* and File › Open always route there and start playing).
- *Open-with* registration (HKCU only; never changes the default app) and its launch
  debounce.
- Legacy foobar2000 lifecycle: started hidden if Lyra started it, shared and left running
  if it was already open.

### Scope (1.0)
- **Formats in 1.0:** FLAC, WAV, DSF, DFF. ALAC, AIFF, MP3, AAC, Vorbis, Opus, WavPack
  and APE, plus loading foobar2000 components as an optional bridge, are **post-1.0** and
  only play through the legacy backend for now.
- **Platform:** Windows 10/11 x64 only.
- For faithful playback, keep Windows audio enhancements and spatial sound (e.g. Dolby
  Atmos) **off** on the output device (ADR-010).

### Pending — hardware
Built and unit-tested, but not yet verified on real hardware because the development
laptop (built-in Senary codec) has no DSD-capable DAC:
- **Native DSD over ASIO** end-to-end on a DSD DAC.
- **DoP over WASAPI exclusive** end-to-end on a DoP DAC.
- **DAC sample-rate / DSD indicator** confirmation across rates.

### Pending — human verification
- **Loopback A/B** capture comparing Lyra's output to a reference, device-side.
- **Two weeks of daily listening** for real-world stability.

These pending items are tracked for a post-release hardware pass; they do not affect
FLAC/WAV/DSD→PCM playback, which is verified on this machine.
