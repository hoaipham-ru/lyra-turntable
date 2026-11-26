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
| 8 | 8-hour overnight soak test | P8 only runs a 2 h accelerated soak (ADR-011, speed first). When there is a free night: laptop on charger, sleep off, run the soak for 8 h with the endpoint muted, then check underruns = 0 and no memory growth. Command: see `lyra-engine/testing/TEST-STRATEGY.md` §7 |
