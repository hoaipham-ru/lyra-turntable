# Plan: replace foobar2000 with a native audio engine (Rust)

Status: **proposed, not started**. Deferred by decision on 2026-11-24.

## 0. First: why replace it at all?

One point is easy to misunderstand, so it comes first: **a home-grown engine will not make music sound better than foobar2000**. Once foobar2000 is set up bit-perfect (WASAPI exclusive or ASIO, 0 dB, no DSP), it already delivers every bit unchanged to the DAC. At best, a new engine can *match* it, never *beat* it.

The real reasons to replace it:

| Gains | Costs / risks |
|---|---|
| A single process: no hidden window, no dependency on foobar2000's install path or on Beefweb | Losing 20 years of foobar2000 maturity: broken files, odd tags, cue sheets, gapless, hundreds of components |
| Full control: show the real format going to the DAC, lock the volume, a bit-perfect mode that is verified | Owning native DSD, ASIO drivers and the edge cases of WASAPI exclusive |
| Push events instead of polling every second, so the UI reacts instantly | Time: an estimated 6–10 weeks of evenings to match what is used today |
| Simpler distribution: one installer, no component setup guide | No pure-Rust decoders for APE, WavPack or Opus. Either use FFmpeg (LGPL) or drop them |

**Proposed decision:** build it **side by side**. The new engine arrives as a *second backend* behind the same interface. foobar2000 stays as the fallback backend until the new engine passes every check in section 5.

## 1. Target architecture

```
React UI (Turntable.tsx, App.tsx unchanged)
   │  PlayerBackend interface (TS)
   ├── FoobarBackend   ← today's player.ts (Beefweb HTTP + CLI)
   └── NativeBackend   ← Tauri invoke() + "player://state" event
                            │
Rust (src-tauri)            ▼
 ┌─────────────── engine crate ────────────────┐
 │ Control thread: play/pause/seek/queue      │
 │   │ crossbeam channel                      │
 │ Decode thread: Source → PCM/DSD frames     │
 │   │ rtrb ring buffer (lock-free, ~2 s)     │
 │ Output thread (real-time priority, MMCSS): │
 │   WASAPI exclusive │ ASIO │ WASAPI shared  │
 └────────────────────────────────────────────┘
 Library: SQLite (rusqlite) + lofty (tags) + notify (folder watching)
```

Principle: **the path from decoder to DAC changes no sample**. No resampling, no digital volume, no dither, unless the user explicitly turns it on and the UI says "no longer bit-perfect".

## 2. Library choices

| Job | Crate | Notes / risks |
|---|---|---|
| Decode FLAC, WAV, AIFF, ALAC, MP3, AAC, Vorbis | `symphonia` | Pure Rust, covers most libraries |
| APE, WavPack, Opus | `ffmpeg-next` (dynamically linked FFmpeg) | LGPL, larger installer. Can wait for a later phase |
| DSF / DFF | Own parser (simple, public specs) | DFF may be DST-compressed (rare, hard). First phase supports uncompressed DFF only |
| SACD `.iso` | **Out of scope** at first | Complex format, usually DST-compressed. Suggest extracting ISOs to DSF with an external tool |
| WASAPI exclusive | `wasapi` (HEnquist's crate, used by CamillaDSP) | `cpal` does **not** support exclusive mode, so it cannot meet the bit-perfect goal |
| ASIO | `asio-sys` / `cpal` with the `asio` feature | Needs Steinberg's ASIO SDK at build time (the SDK may not be redistributed) |
| Ring buffer | `rtrb` | Lock-free, safe for real-time threads |
| Tags, cover art | `lofty` | |
| Resampling (only when the DAC rejects the native rate) | `rubato` | Off by default |
| Music library | `rusqlite`, `notify` | |

## 3. DSD: the hardest part

- **DoP (DSD over PCM)**: packs 16 DSD bits into each 24-bit sample, with the top 8 bits alternating between the markers `0x05` and `0xFA`. DSD64 (2.8224 MHz) becomes 176.4 kHz PCM, DSD128 becomes 352.8 kHz. Works over WASAPI exclusive at 24-bit. The volume **must not** be touched, or the markers break and the DAC outputs noise.
- **Native DSD**: ASIO only, with a vendor driver that supports it (`ASIOSetIoFormat` with `kASIODSDFormat`). Every vendor behaves differently, so it has to be tested per DAC.
- **DSD → PCM** (for DACs without DSD): FIR filter then decimate. This is DSP, not bit-perfect, and the UI must say so.

## 4. Roadmap

| Phase | Scope | Done when |
|---|---|---|
| **P0 — Split the interface** (2–3 evenings) | Add `PlayerBackend` in TS. `player.ts` becomes `FoobarBackend`. No behavior change | The app works exactly like 0.3.0 |
| **P1 — Basic PCM playback** (1–2 weeks) | `symphonia` → `rtrb` → WASAPI **exclusive**. Play/pause/seek/next. Per-track sample-rate switching. Show the format going to the DAC | FLAC 16/44, 24/96 and 24/192 play correctly and the DAC switches rate |
| **P2 — Playlists & queue** (1 week) | Playlists stored in SQLite or JSON, Default Playlist logic as today, Open-with/drag & drop call the engine directly | Every File menu action works without foobar2000 |
| **P3 — Gapless & cue** (1 week) | Pre-decode the next track. Same format: stitch seamlessly; different format: reopen the stream. Cue sheets split into virtual tracks | Live and classical albums play without gaps between tracks |
| **P4 — DSD** (1–2 weeks) | DSF/DFF parser, DoP over WASAPI. Native DSD over ASIO (DAC-dependent) | The DAC shows DSD64/128, no hiss |
| **P5 — ASIO** (1 week) | ASIO backend for PCM and native DSD | Works with the driver of the DAC in use |
| **P6 — Library** (optional) | Folder scanning, tags, cover art, search | |
| **P7 — Drop foobar2000** | NativeBackend becomes the default. FoobarBackend stays in Settings for a few releases, then is removed | Everything in section 5 passes over 2 weeks of real listening |

## 5. Verifying bit-perfect output (required before P7)

1. **Data comparison**: a debug mode writes the exact buffers sent to WASAPI to a file and compares their hash with the PCM that `ffmpeg -f s24le` decodes from the same file. They must match byte for byte.
2. **DAC test files**: "bit-perfect test" files (HDCD/MQA/DTS-in-WAV) light the DAC's indicator only if the data arrives unaltered.
3. **DoP**: the DAC must show DSD, not 176.4/352.8 kHz PCM.
4. **Stability**: 8 hours of continuous playback during normal computer use with zero underruns (count them in the log).
5. **Edge cases**: unplugging the DAC mid-playback, another app grabbing the device, sleep and wake, corrupt files or missing headers.

## 6. Risks and mitigations

| Risk | Mitigation |
|---|---|
| Underruns when the machine is busy | MMCSS "Pro Audio" on the output thread, ~2 s of buffer on the decode side, measure latency per DAC |
| Exclusive mode silences other apps | That is what exclusive means. Offer a shared-mode switch and show a warning in the UI |
| Every vendor's ASIO driver behaves differently | Keep WASAPI exclusive as the default, ASIO as an advanced option |
| Scope creep (library, DSP, EQ…) | Only build what 0.3.0 already does. Everything else waits until after P7 |
| Losing steam halfway | Thanks to P0, the app still works with foobar2000 at any stopping point |

## 7. Questions to answer before starting

- Which DAC model is in use? Does it support DoP or native DSD? Is there an ASIO driver?
- How many APE, WavPack and SACD ISO files are in the library? That decides whether FFmpeg and ISO handling are needed.
- Are ReplayGain, EQ or crossfeed wanted? Default is no, to stay bit-perfect.
