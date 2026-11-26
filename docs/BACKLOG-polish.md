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

## P3 (playback engine) reviewer follow-ups (non-blocking, 2026-11-26)

`lyra-reviewer` raised one **blocking** finding (B1 — seek/stop/set-queue did not flush the
`lyra-output` ring, so a stale pre-seek tail would play on a real device) which was fixed
(new `OutputHandle::flush()` + engine wiring, ADR-013) and re-verified (B1 RESOLVED — PASS)
before P3 passed. These remaining nits were parked (none affects the offline byte-exact proofs):

| # | Item | Owner |
|---|---|---|
| P3-1 | `crates/lyra-engine/src/worker.rs` (pre-roll fail path) + `control.rs` — a gapless pre-open failure drains to `TrackEnded`, then control `start_index(index+1)` re-loads the same failed track once before `on_track_failed` skips it: redundant open + extra error event. Advance the index on the pre-roll failure path. | lyra-engine |
| P3-2 | `crates/lyra-engine/src/control.rs` — the next-next preload is armed only after `Advanced` is received, so a track shorter than the control round-trip (<~50 ms) could hit EOS before its successor is armed, dropping the gapless join. Pathological for real audio. | lyra-engine |
| P3-3 | `crates/lyra-engine/src/engine.rs` — unbounded event/command channels; ~10 Position events/s accumulate if a consumer stalls. Use a bounded channel or coalesce Position events. | lyra-engine |
| P3-4 | `crates/lyra-engine/src/source.rs` — `Source::resolve` re-parses the cue sheet and re-probes the image on every same-image preload; cache per image key to avoid redundant worker-thread file I/O. | lyra-engine |
| P3-5 | `crates/lyra-output/src/handle.rs` + `crates/lyra-engine/src/worker.rs` — timeout-then-service race: if the RT render thread is starved >500 ms, `flush()` times out, the worker rebases and pushes post-seek bytes, and the RT thread's next `service_flush` then drains those fresh bytes (req still > ack), dropping the start of landed audio until `pump()` refills. Only under a >500 ms RT stall. Fix: on a timed-out flush, defer pushes until `flush_ack` is observed, or carry an outstanding-flush generation. | lyra-output / lyra-engine |
| P3-6 | `crates/lyra-output/src/wasapi_backend.rs` — `service_flush` is reached only after the device event wait; on repeated `wait_for_event` errors it is skipped via `continue`. Bounded in practice by the 500 ms flush timeout; add a comment that flush latency depends on event delivery. | lyra-output |
| P3-7 | `crates/lyra-engine/src/sink.rs` — `OfflineSink::flush` is a deliberate no-op, so QA offline captures still contain the pre-seek region: a B1 regression would surface only in the `RingSink` unit test, not in offline byte-equality. Intentional/documented; flagged so QA does not rely on offline captures to catch a flush regression. | lyra-qa |

## P4 (app alpha) reviewer follow-ups (non-blocking, 2026-11-26)

`lyra-reviewer` returned **PASS with no blocking findings** for P4. The native path does not
touch the RT render thread or the sample path; "never two players at once", the bit-perfect
badge and the volume lock all hold. These nits were parked:

| # | Item | Owner |
|---|---|---|
| P4-1 | `src-tauri/src/engine.rs:226` — the `to_payload` badge string-builder has no direct Rust unit test; the string-assembly branches (codec present/absent, 44.1 vs 96 kHz trim, DSD label, ✓/✗) are covered only indirectly (TS render test + `p4.ps1` `Format:` line). Add a Rust `#[test]` table over `(EngineEvent, expected badge)`. | lyra-app |
| P4-2 | `crates/lyra-library/src/playlists.rs:379` — `import_m3u` is non-atomic: `create()` then `add_items()` run in separate transactions, so a DB error during `add_items` leaves an orphan empty playlist. Wrap both in one transaction. | lyra-library |
| P4-3 | `src-tauri/src/{engine,library,lib}.rs` — pervasive `Mutex::lock().unwrap()` propagates poison: a panic in one command poisons the state mutex so every later command panics. Not file/device input, so non-blocking; consider `unwrap_or_else(|e| e.into_inner())` or a documented policy. | lyra-app |
| P4-4 | `src-tauri/src/engine.rs:41` — `Shared::track_ext: Mutex<Option<String>>` is only ever touched on the single event-forwarding thread; the `Mutex` is unnecessary (harmless). | lyra-app |
| P4-5 | `src-tauri/src/engine.rs:370` — a `Seek` issued before any `Track`/`Format` event uses `source_rate == 0`, silently seeking to frame 0. Clamp or ignore until a rate is known. | lyra-app |
| P4-6 | `src/backend/NativeBackend.ts` — after `engine_shutdown` on a backend switch the hook keeps `connected === true` (stale); cosmetic, no sound/function impact. | lyra-app |
| P4-7 | (carried from P3) truncated/corrupt mid-stream data still ends without an `EngineEvent::Error`. P4 forwards `Error` faithfully but the engine never emits one for mid-stream truncation. **Not worsened by P4** — remains the explicitly-deferred "fix in P4 or later" item. | lyra-engine |
