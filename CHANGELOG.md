# Changelog

All notable changes to Lyra Turntable are documented here.
This project follows [Semantic Versioning](https://semver.org/).

## 1.0.1 — 2026-11-27

### Fixed
- **Track length now shows and the seek bar works** with the Lyra engine (all formats, incl. DSD). The engine event fields went to the UI in snake_case (duration_seconds, it_perfect, …) while the UI reads camelCase, so the length stayed 0:00, the seek bar and needle drop were disabled, and the bit-perfect flag was not read. Fixed with ename_all_fields; regression test added.
- The playlist picker count (Default Playlist · N) now updates when the list changes.
- **MP3 (and AAC/M4A, ALAC, Ogg Vorbis, AIFF) now play** in the Lyra engine. In 1.0.0 only
  FLAC/WAV/DSD decoded, so choosing an MP3 did nothing and showed no error. These formats
  are decoded by symphonia to float and quantised once to 24-bit integers (peak and RMS match ffmpeg to 0.001 dB on a test MP3). Their full parity tests against foobar2000 (P1b) are still pending; MP3 encoder delay/padding is not trimmed yet (~20-40 ms of extra silence at the start).
- A playback error (e.g. a file the engine cannot decode) now shows as a toast instead of
  failing silently.
- **No more two "Default Playlist" entries.** foobar2000's own scratch playlist is now
  imported as "Default Playlist (foobar2000)", and an existing duplicate is renamed to
  "Default Playlist (2)" automatically.

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
- **Volume knob = device (endpoint) volume** through Windows; applied after Lyra hands over
  the samples, so the stream stays bit-perfect.
- **Playlist card**: click / Ctrl / Shift select, double-click or Enter to play, Delete to
  remove, drag to reorder (Lyra backend).
- **English UI** throughout (song titles shown as tagged), app-drawn dropdowns (native
  `<select>` popups do not open in the transparent window).
- **One-time, read-only import** of foobar2000 v2 playlists on first native startup, so
  returning users keep their lists. foobar2000 files are never modified.
- Single string table (`src/strings.ts`) with English (active) and Vietnamese entries,
  ready for a language toggle.
- **Output ring re-priming** on device open, seek, track change and queue reload, plus
  coalescing of back-to-back navigation (reduces underruns; see Known issues).

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

### Known issues
- **DSD→PCM start-up underrun (ADR-015).** On a device **without DSD support** (e.g. the
  development laptop's built-in Senary codec), starting a DSD track through the DSD→PCM
  path produces **4–6 output underruns during roughly the first 0.5 s** of the track.
  This affects **start-up only**: once playback is running, DSD→PCM is clean, and
  **FLAC and WAV are not affected** by this one. A fix is planned for **1.0.2**.
- **Underruns on track change / seek (ADR-016).** Jumps (next/previous, seek, new queue)
  flush and refill the output ring; on the real device the refill can lag for a moment.
  30-minute real-device soak: **460 underruns over 823 transitions** (from ~3,000 before
  the 1.0 fixes). Steady playback is clean and bit-perfect; a short tick can be heard at a
  jump. Root-cause fix planned for **1.0.2**.

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
