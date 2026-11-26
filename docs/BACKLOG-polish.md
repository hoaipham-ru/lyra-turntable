# Polish backlog

Small finishing touches, parked on purpose. None of them blocks the engine work in
`learning_code/lyra-engine`. Pick them up when there is a quiet evening.

| # | Item | Notes |
|---|---|---|
| 1 | Windows Search still shows a generic icon in *Recent searches* | `lyra.exe` already carries the new icon; the shortcut and Open-with entries were refreshed and `ie4uinit -show` was run. Remaining step: restart Explorer, or clear device search history (Settings › Privacy & security › Search permissions) |
| 2 | Icon readability at 16–24 px | The lyre disappears at small sizes and the icon reads as a plain red disc. Consider a simplified small-size variant in the `.ico` |
| 3 | UI language | UI labels are Vietnamese while the README is English. Options: switch to English, or add a VI/EN toggle. Re-run `docs/capture/capture.py` afterwards |
| 4 | GitHub Release | Publish v0.3.0 with `Lyra Turntable_0.3.0_x64-setup.exe` attached |
| 5 | Commit identity | Local git author is `hoaipham97`, but the repo lives under `hoaipham-ru`. Decide which one to use |
| 6 | Hard-coded foobar2000 path | `C:\Program Files\foobar2000\foobar2000.exe` is hard-coded in `lib.rs`. Read it from the registry or let the user pick it (moot once the native engine lands) |
| 7 | Bit-perfect indicator | Show the real output format and lock the volume at 0 dB (planned as part of the native engine, see the plan) |
| 8 | Long test suite (run when there is time) | Phase gates use short tests (ADR-012, speed first). Run the long versions later with `tests\parity\run.ps1 -Long` (laptop on charger, sleep off, headphones plugged in, not listening): device-tap parity on **every** lossless file (30 s each, speakers + headphones), 1 h CPU-stress underrun run, **8 h overnight soak** (0 underruns, no memory growth), 30 min fuzzing per parser, 10-min clock-drift track. Details: `lyra-engine/DECISIONS.md` ADR-012 |

## P1+P2 reviewer follow-ups (non-blocking, 2026-11-26)

`lyra-reviewer` PASSed the P1+P2 gate with no blocking findings. These hardening nits were
parked (none affects correctness for the FLAC/WAV, 16/24-bit, exclusive/shared scope):

| # | Item | Owner |
|---|---|---|
| R1 | `crates/lyra-decode/src/backend.rs:296` — `write_block`'s `_ => 0` silently drops an unexpected `AudioBufferRef` variant (read as EOF). Return a `LyraError::Decode` instead so a future symphonia variant can't truncate audio silently. | lyra-decode |
| R2 | `crates/lyra-decode/src/target.rs:83` — dead `SampleFormat::F32` arm in `push_int` would do a lossy `v as f32` cast if ever reached. Replace with `unreachable!()` to prevent a silent bit-perfect trap. | lyra-decode |
| R3 | `crates/lyra-output/src/wasapi_backend.rs:496-503` — a non-device-lost error breaks the render loop without setting a stat; caller only learns via the 5 s drain timeout. Add a distinct "stream error" flag. | lyra-output |
| R4 | `crates/lyra-decode/src/backend.rs:179` — `pending_skip` seek-overshoot (`actual_ts > frame`) is unguarded; add an assertion so a demuxer regression is loud. | lyra-decode |
| R5 | `tests/parity/p2_render.mjs:139` — treat source-rate rejection on a rate-limited endpoint as expected-skip, not `FAIL`, so the score reads correctly on ADR-010 48k-only hardware (did not trigger this run — the endpoints accepted all rates in exclusive). | lyra-qa |
| R6 | `tests/parity/p2_render.mjs:106-130` — `alignedCompare` could absorb a start-of-stream underrun on tracks that begin with digital silence; cross-check `renderedFrames` against `frames_played`/underrun count. | lyra-qa |
| R7 | `crates/lyra-decode/src/libflac.rs:20` — hardcoded user-specific winget `flac.exe` fallback path (behind `libflac-check` feature); prefer an env var. | lyra-decode |

## P5 (DSD) reviewer follow-ups (non-blocking, 2026-11-26)

`lyra-reviewer` found one **blocking** finding (B1, DFF/DSF parser overflow panic/hang) which was fixed and re-verified before P5 passed. These remaining nits were parked:

| # | Item | Owner |
|---|---|---|
| P5-1 | `crates/lyra-dsd/src/dsf.rs` — after the B1 fix, consider a tighter cross-check of `sample_count` vs the de-blocked data layout (beyond the EOF/data-size clamp) for very odd headers. | lyra-dsd |
| P5-2 | `crates/lyra-dsd/tests/readers.rs:267` — committed test hardcodes user-specific absolute `C:\SoulSeek_Music\complete\...` corpus paths (skips if absent). Source from a manifest/env var. | lyra-dsd / lyra-qa |
| P5-3 | `crates/lyra-output/src/ring.rs:76` — `dop_idle` emits 24-in-32 idle frames, but DoP carrier negotiation prefers packed 24-in-3. When the real feeder is wired (pending-hardware), confirm idle frame size/justification matches the negotiated container or a DoP underrun pre-fill could desync. | lyra-output |
| P5-4 | `crates/lyra-output/src/ring.rs:59` — DoP idle payload uses constant `0x69` for both DSD bytes rather than the canonical `0x69/0x96` alternation; verify a real DoP DAC reads it as silence (pending-hardware). | lyra-output |
| P5-5 | `crates/lyra-output/src/asio_backend.rs:64/72/94` — `drivers()/probe()/open()` are `unimplemented!()` (feature-gated, off by default). Ensure the engine gates these behind a capability check before the hardware gate opens. | lyra-output |
| P5-6 | `tools/lyra-probe/src/dsd.rs:62` — `guard_target_only` matches any path component literally named `target` (case-insensitive) without canonicalizing, so `<corpus>\target\x.dsf` would pass. Tighten to the repo `target/` and reject `..`. | lyra-probe / lyra-qa |
| P5-7 | `crates/lyra-dsd` `DsdToPcm` runs the FIR at the full DSD rate; a polyphase decomposition would cut per-sample cost for real-time DSD256/512→PCM. Offline conversion and tests are fine as-is. | lyra-dsd |
