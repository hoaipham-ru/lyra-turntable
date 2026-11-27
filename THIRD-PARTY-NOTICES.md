# Third-party notices — Lyra Turntable 1.0.0

This file lists the third-party software that ships in the **1.0 standalone build**
(FLAC + WAV + DSD, native Lyra Engine; the optional legacy foobar2000 backend uses no
bundled third-party code of its own). It is intended to satisfy the attribution clauses
of the permissive licences below.

The list was verified against the actual shipped dependency graph with:

```
cargo tree -e no-dev -p turntable        # src-tauri package, default features
```

Only crates present in that graph are listed as shipped. First-party crates
(`lyra-core`, `lyra-decode`, `lyra-dsd`, `lyra-engine`, `lyra-library`, `lyra-output`,
`turntable`) are part of this project and are not third-party notices.

## Summary of licence posture

- **No GPL is linked.** No crate in the shipped graph is GPL/AGPL.
- **No FFmpeg / LGPL is linked** in the 1.0 build. `ffmpeg-next` (and any FFmpeg C
  libraries) are **not** a dependency of any shipped crate — decoding is done by the
  pure-Rust `symphonia` stack and the first-party `lyra-dsd` reader. A plain-text
  `cargo tree` confirms no `ffmpeg*`, `*-sys` FFmpeg bindings, or LGPL crates are pulled in.
- **No foobar2000 code is redistributed.** The standalone claim of ADR-008 holds: the
  1.0 binary contains only the crates below plus the first-party `lyra-*` crates. The
  optional legacy backend *controls* an separately-installed foobar2000 via Beefweb HTTP;
  it bundles none of its code.
- **Stale ADR wording (relay to lyra-architect):** ADR-007 / ADR-008 still describe an
  FFmpeg-based decode path. That wording is now stale for 1.0 — the shipped build links
  no FFmpeg. The ADRs live in the `lyra-engine` plan folder (owned by lyra-architect) and
  are not edited here; this note records the observation so the orchestrator can relay it.

## Shipped crates (default-feature 1.0 build)

### Decoding / audio format

| Crate | Version | Licence | Notes |
|-------|---------|---------|-------|
| `symphonia` (incl. `symphonia-core`, `symphonia-bundle-flac`, `symphonia-codec-pcm`, `symphonia-format-riff`, `symphonia-metadata`, `symphonia-utils-xiph`) | 0.5.5 | **MPL-2.0** | Mozilla Public License 2.0 is a **file-level** (weak) copyleft: the crate's own source files stay under MPL-2.0 and modifications to those files must be shared, but it may be linked into a larger work (this app) under a different licence. We ship it unmodified. Source: <https://github.com/pdeljanov/Symphonia> |
| `extended` | 0.1.0 | MIT/Apache-2.0 | 80-bit float helper used by `symphonia-format-riff` (AIFF). |

### Audio output / real-time

| Crate | Version | Licence | Notes |
|-------|---------|---------|-------|
| `wasapi` | 0.15.0 | MIT | Safe WASAPI bindings for the native exclusive/shared output. |
| `rtrb` | 0.3.6 | MIT/Apache-2.0 | Real-time lock-free ring buffer (output ring). |
| `crossbeam-channel` | 0.5.x | MIT/Apache-2.0 | Engine control/event channels (with `crossbeam-utils`). |

### Windows platform bindings

| Crate | Version | Licence | Notes |
|-------|---------|---------|-------|
| `windows`, `windows-core`, `windows-implement`, `windows-interface`, `windows-result`, `windows-targets`, `windows_x86_64_msvc` | 0.57.x / 0.52.x | MIT/Apache-2.0 | COM / WASAPI / endpoint-volume bindings. |
| `windows-sys`, `windows-link` | 0.59.x / 0.61.x | MIT/Apache-2.0 | Win32 single-instance / window-message bindings. |

### Metadata / library / artwork

| Crate | Version | Licence | Notes |
|-------|---------|---------|-------|
| `lofty` (incl. `lofty_attr`, `ogg_pager`) | 0.25.5 | MIT/Apache-2.0 | Tag reading for the library and artwork. |
| `rusqlite` | 0.43.0 | MIT | Playlists / library store. |
| `libsqlite3-sys` (bundled SQLite) | 0.40.0 | MIT (bindings); **bundled SQLite = public domain** | SQLite itself is in the public domain. |
| `base64` | 0.22.1 | MIT/Apache-2.0 | Encodes embedded cover art as a `data:` URL for `artwork_read`. |

### Serialization

| Crate | Version | Licence | Notes |
|-------|---------|---------|-------|
| `serde`, `serde_core`, `serde_derive` | 1.0.x | MIT/Apache-2.0 | |
| `serde_json` | 1.0.x | MIT/Apache-2.0 | |

### Tauri application stack

| Crate | Version | Licence | Notes |
|-------|---------|---------|-------|
| `tauri`, `tauri-build`, `tauri-macros`, `tauri-codegen`, `tauri-runtime*`, `tauri-utils` | 2.x | MIT/Apache-2.0 | App shell / webview host. |
| `tauri-plugin-http` | 2.9.x | MIT/Apache-2.0 | Beefweb HTTP for the optional legacy backend. |
| `tauri-plugin-dialog` | 2.8.x | MIT/Apache-2.0 | Open-with / file dialogs. |
| `tauri-plugin-single-instance` | 2.5.x | MIT/Apache-2.0 | Open-with debounce / single-instance. |
| `muda`, `dpi`, `keyboard-types`, `raw-window-handle` and other Tauri transitive crates | — | MIT/Apache-2.0 | Menu / window plumbing. |

### Other transitive crates

Numerous small transitive utility crates are pulled in by the above (e.g. `log`,
`lazy_static`, `bytemuck`, `arrayvec`, `bitflags`, `byteorder`, `data-encoding`,
`flate2` with `miniz_oxide`/`adler2`/`crc32fast`, `encoding_rs`, `smallvec`, `hashbrown`,
`hashlink`, `itoa`, `memchr`, `ryu`, `thiserror`, `anyhow`, `time`, `http`, `bytes`,
`percent-encoding`, `once_cell`, `getrandom`, `glob`, `dirs`/`dirs-sys`, `paste`,
`num-integer`/`num-traits`, `widestring`). Each is licensed **MIT**, **MIT/Apache-2.0**,
**Apache-2.0**, **CC0-1.0**, **BSD-2/3-Clause**, **Zlib**, or **Unlicense** — all
permissive. None is (A)GPL/LGPL. Run `cargo tree -e no-dev -p turntable` for the exact,
current set and `cargo license`/`cargo about` to regenerate per-crate licence text.

## Front-end (webview) dependencies

The webview UI is built with **React** and **react-dom** (MIT) and bundled with **Vite**
(MIT). Only the built assets ship; the bundler/toolchain is a build-time dependency.

## Optional crates — NOT in the default 1.0 binary

These are gated behind off-by-default Cargo features and are **not** present in the
shipped 1.0 graph (confirmed: they do not appear in `cargo tree -e no-dev -p turntable`).
They are listed here only for completeness.

| Crate | Version | Licence | Feature | Notes |
|-------|---------|---------|---------|-------|
| `asio-sys` | 0.2.x | MIT/Apache-2.0 | `lyra-output/asio` (off) | ASIO backend. The Steinberg **ASIO SDK is proprietary and is NOT redistributed**; a user building with this feature must supply it themselves (ADR-009). |
| `notify` | 6.1.x | CC0-1.0 | `lyra-library/watch` (off) | Folder watcher. CC0-1.0 is a public-domain dedication; still permissive. |

## Dev / test-only dependencies (never shipped)

Pulled in only for local testing and CI, never linked into the distributed binary:
`vitest`, `@testing-library/dom`, `@testing-library/react`, `@testing-library/user-event`,
`jsdom`, `typescript`, `@vitejs/plugin-react`, `@types/*` — all **MIT**. The UI-smoke /
screenshot harness `docs/capture/capture.py` drives the app with **Playwright** (Apache-2.0),
installed separately via pip — it is a test tool, not an npm dependency and not shipped.
