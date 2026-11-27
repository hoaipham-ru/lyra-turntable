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

## P6 (library & metadata parity) reviewer follow-ups (non-blocking, 2026-11-26)

`lyra-reviewer` returned **PASS with no blocking findings** for P6 (metadata/library — not the RT
path). No write to the corpus or the foobar2000 profile, no SQL injection, no unsafe/panic/hang on
untrusted input, no licence problem. These nits were parked:

| # | Item | Owner |
|---|---|---|
| P6-N1 | `crates/lyra-library/src/cache.rs:332` — `stat()` stores mtime at whole-second granularity; a file retagged within the same second with unchanged byte size would be served stale. Matches the spec `(path, mtime, size)` key and retagging almost always changes size (low risk). Consider sub-second mtime if ever observed. | lyra-library |
| P6-N2 | `crates/lyra-library/src/cache.rs:128` — a cached row whose *typed* columns fail to decode (reachable only via external DB tampering/schema skew) propagates an error out of `get_or_read` instead of falling back to a fresh file read. The `tags_json` blob is already tolerant; treat any row-decode error as a cache miss so a corrupt row can never poison a read. | lyra-library |
| P6-N3 | `crates/lyra-library/src/playlists.rs:571` — `dir.join(format!("playlist-{guid}.fplite"))` does not validate `guid` from `index.txt`; a crafted guid containing `..\` could read an arbitrary `.fplite`-suffixed file. Read-only, user-owned, opt-in migration (low severity). Reject guids containing path separators. | lyra-library |
| P6-N4 | `tools/lyra-probe/src/meta.rs:234` — DSD `length_samples` is the exact 1-bit DSD frame count, while foobar2000's SACD input reports the PCM-converted length. `p6.ps1` records this as a note and marks the fb2k diff pending-human, so it does not break P6. Define the DSD length comparison on a common unit before claiming length parity at P8 (DoD §6). | lyra-qa |
| P6-N5 | `crates/lyra-library/src/playlists.rs:687` — `location_to_path` does no percent-decoding of `file://` locations, relying on the empirical finding that fb2k v2 writes raw Windows paths. Fine today; revisit if a percent-encoded location ever appears. | lyra-library |

## Deferred test: P8.2 loopback A/B vs foobar2000 (noted 2026-11-26)

- [ ] **P8.2 loopback A/B** - Hoai has the hardware at home; run after returning. Render one file per
  rate/format (16/44.1, 24/96, 24/192 FLAC, DSD64 via DoP and DSD->PCM) through both Lyra and foobar2000
  into the same capture path and confirm bit-identical output. Procedure: lyra-engine HUMAN-CHECKS H2.1 / H8.2.
  Also do the CPU <= foobar2000 x 1.2 comparison from P8.3 then (needs foobar2000 running in WASAPI exclusive).
## Deferred test: ASIO on Sony Walkman NW-A55 (noted 2026-11-26)

- [ ] **ASIO via Sony USB Audio driver** - install the Sony driver, put the NW-A55 in USB DAC mode, run Lyra with
  ASIO output for FLAC 16/44.1, 24/96, 24/192 and DSD64. Check bit-perfect, underruns, and which DSD path is chosen.
  Also try DoP if a forced-DoP option exists (earphones off). Procedure: lyra-engine HUMAN-CHECKS H8.3.
  Already passed on the Walkman via WASAPI exclusive: PCM 44.1/96/192 bit-perfect, 0 underruns.
## Known weaknesses as of 0.4.1 (2026-11-26)

Honest list of what is still weak or unverified. Owners in brackets.

### Audio engine
- [ ] **DSD->PCM underruns**: 3 per 60 s on the onboard codec, 1 per 20 s on the Sony NW-A55 -> audible ticks. Cause is in Lyra's DSD->PCM path (per-period converter cost spikes and/or a thin buffer), not the device. Average CPU is only 1.4 %, so suspect spikes, not total cost. Must be 0 before 1.0 / the soak. [lyra-dsd, lyra-output]
- [ ] **DSD track length shows 0:00**: total duration missing for DFF, so the seek bar and needle-drop are disabled for DSD. [lyra-dsd, lyra-engine]
- [ ] **Truncated files end silently** with no error event. [lyra-decode, lyra-engine]
- [ ] **DoP never chosen automatically**: Windows cannot detect DoP support, so Lyra only uses DoP when told; no forced-DoP option in lyra-play/Settings has been tested on hardware. [lyra-output, lyra-app]
- [ ] **ASIO only tested against a mock driver**; real ASIO pending (Sony NW-A55 driver, H8.3). [lyra-output]
- [ ] **No loopback A/B vs foobar2000 yet** (P8.2, hardware at home) and **no CPU comparison vs foobar2000** (P8.3 needs foobar running). [lyra-qa, human]
- [ ] **30-min soak not run yet** (and the 8-h soak is on the long-test list). [lyra-qa]
- [ ] **Hardware seek verification** and **device render taps in P8** still open (LOG follow-ups). [lyra-qa]

### App / UI
- [ ] **No automated UI tests at all until now** - 0.4.0 shipped 4 UI bugs (Vietnamese strings, dead dropdowns, no-op volume, no playlist select/reorder). New agent lyra-ui-qa (component + Playwright + real-window tauri-driver) must gate every UI change. [lyra-ui-qa]
- [ ] **0.4.1 fixes not verified in the real window by an automated test** (English UI, app-drawn Dropdown, endpoint volume knob, select/double-click/Delete/drag-reorder). [lyra-ui-qa]
- [ ] **Reordering the playlist that is playing** does not update the engine queue: next/prev may follow the old order until the track changes. [lyra-engine, lyra-app]
- [ ] **Drag-to-reorder only on the Lyra backend**; foobar2000 backend has no moveItem. Click now selects and **double-click plays** (behaviour change from 0.4.0). [lyra-app]
- [ ] **Volume knob = device (endpoint) volume**: keeps bit-perfect, but on a device without hardware volume in WASAPI exclusive it may do nothing; no UI hint for that case yet. Not tested on the NW-A55. [lyra-app, lyra-output]
- [ ] **Settings device/mode/DSD changes apply only when the engine (re)starts**; no live re-open of the device. [lyra-app, lyra-engine]
- [ ] **foobar2000 is still the default backend** until P8 (switch in Settings). [lyra-app]
- [ ] **Language is a compile-time switch** (strings.ts lang), no in-app toggle. [lyra-app]
- [ ] Hard-coded foobar2000 path, icon set, commit identity and other polish items above remain. [lyra-app]

### Process
- [ ] **Agent scripts can be wrong silently**: p8_perf.ps1 first crashed, then "measured" an idle process because PowerShell 5.1 split a path with spaces. QA scripts need a self-check (errors counter, non-zero frames) before numbers are trusted. [lyra-qa]
- [ ] **Stopping a run mid-wave leaves orphans** (chain-*.sh bash processes survive taskkill /T; an orphan started the soak and left the headphones muted). Orchestrator should register background chains in STATUS so a stop can kill them. [orchestrator]
## Known issue shipped in 1.0 (ADR-015) - fix in 1.0.1
- [ ] **DSD->PCM start-up underrun**: on a device without DSD support (e.g. the laptop's Senary codec), the first ~0.5 s of a DSD track can underrun 4-6 times (a short tick at the very start). Steady-state playback is clean; FLAC/WAV are unaffected. Three fix rounds did not remove it. Ideas for 1.0.1: pre-convert the first DSD block before starting the stream; start the device only once the cushion is full; faster first FIR pass (table-driven DSD->PCM). [lyra-dsd, lyra-engine, lyra-output]

## P8.7 reviewer nits (1.0.0)

Release-hygiene review of 1.0.0. Recorded here as backlog only — no engine/audio code was
changed closing these (versioning, licence notices, and these notes were the only edits).

| # | Item | Owner |
|---|------|-------|
| nit 1 | `load()` does not clear `self.pending` / `pending_off` before `flush_sink()` on the Next/Prev/PlayAt reload path, unlike `seek()` / `stop()` which do. **Latent / provably-safe today**: the 250 ms re-prime cushion ≪ the 2 s output ring, so no stale pending can outlive the flush. Should clear `pending` the same way `seek()`/`stop()` do if the cushion/ring ratio ever changes (larger cushion or smaller ring could make it a real stale-audio bug). | lyra-engine |
| nit 4 | `seek()` now does a **synchronous** flush + prime, so rapid scrubbing serializes (each seek waits for the flush+prime before the next lands). Accepted "correctness over snappy" trade-off for 1.0 (guarantees no pre-seek tail). Revisit if daily listening finds scrubbing sluggish — e.g. coalesce rapid seeks or make the flush async with an outstanding-flush generation. | lyra-engine |

**Stale ADR wording (relay to lyra-architect):** ADR-007 / ADR-008 in the `lyra-engine`
plan folder still describe an FFmpeg-based decode path. For the 1.0 standalone build this is
stale — the shipped binary links **no FFmpeg / LGPL** (confirmed via
`cargo tree -e no-dev -p turntable`; `ffmpeg-next` is not a dependency of any shipped crate).
Those ADRs are lyra-architect's files and were not edited here; see `THIRD-PARTY-NOTICES.md`
for the same observation so the orchestrator can relay it.

## Known issue shipped in 1.0 (ADR-016) - fix in 1.0.1
- [ ] **Underruns on track change / seek / queue reload**: 30-min real-device soak = 460 underruns (1922 events) over 823 transitions after two fix rounds (from ~3,000). Ring is re-primed on open/seek/reload (cdef11a), navigation coalesced (4425dc4), post-flush grace window (a1b44a4) - still not zero. Hoai is researching a root-cause fix. Leads: keep the device running with the old tail while the new stream fills (crossfade-free double buffer), prime BEFORE flushing, larger cushion on transitions, measure fill time per transition, verify gapless natural transitions on the real device separately from user seeks, and report raw vs suppressed counts. [lyra-engine, lyra-output, lyra-qa]