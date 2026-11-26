//! S06 — library: tags, artwork, cue sheets, playlists and SQLite storage.
//!
//! This crate is read-only with respect to the audio library: it opens files
//! without write access and never "fixes" tags or sheets.
//!
//! # Scope by phase
//! * **P3.2:** the [`parse_cue`] cue-sheet parser, producing a [`CueSheet`] with
//!   frame-accurate (75 fps → source sample-frame) track boundaries. See [`cue`].
//! * **P4.2 (added here):** read-only metadata ([`read_tags`] → [`TrackMeta`]),
//!   cover art ([`fn@artwork`] → [`Artwork`]) and the SQLite-backed [`Playlists`]
//!   store with `m3u`/`m3u8` import/export and Default Playlist primitives.
//! * **P6.1/P6.2:** full foobar2000 field-naming parity in [`read_tags`]
//!   (disc number, ReplayGain, codec, multi-value) and front-cover-preferred
//!   [`fn@artwork`] with broken-picture tolerance.
//! * **P6.3:** best-effort import of foobar2000 playlists —
//!   [`Playlists::import_fplite`] (v2 text format), [`Playlists::import_fpl`]
//!   (classic binary, scan-based) and [`Playlists::migrate_foobar_playlists`]
//!   (every playlist from a `%APPDATA%\foobar2000-v2` profile, read-only).
//! * **P6.4:** a `(path, mtime, size)`-keyed SQLite [`TagCache`] over
//!   [`read_tags`], with an optional folder scan and a `watch`-feature folder
//!   watcher. See [`cache`].

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod artwork;
pub mod cache;
pub mod cue;
pub mod playlists;
pub mod tags;

pub use artwork::{artwork, Artwork};
pub use cache::TagCache;
pub use cue::{parse_cue, CueFile, CueSheet, CueTrack};
pub use playlists::{ImportSummary, Playlist, Playlists, DEFAULT_PLAYLIST_TITLE};
pub use tags::{read_tags, TrackMeta};
