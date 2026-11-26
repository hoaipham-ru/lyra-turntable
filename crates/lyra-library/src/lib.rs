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
//! * Full foobar2000 metadata parity and `.fpl` import arrive in P6.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod artwork;
pub mod cue;
pub mod playlists;
pub mod tags;

pub use artwork::{artwork, Artwork};
pub use cue::{parse_cue, CueFile, CueSheet, CueTrack};
pub use playlists::{Playlist, Playlists, DEFAULT_PLAYLIST_TITLE};
pub use tags::{read_tags, TrackMeta};
