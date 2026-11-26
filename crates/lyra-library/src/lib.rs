//! S06 — library: tags, artwork, cue sheets, playlists and SQLite storage.
//!
//! This crate is read-only with respect to the audio library: it opens files
//! without write access and never "fixes" tags or sheets.
//!
//! # Scope by phase
//! * **P3.2 (implemented here):** the [`parse_cue`] cue-sheet parser, producing a
//!   [`CueSheet`] with frame-accurate (75 fps → source sample-frame) track
//!   boundaries. See [`cue`].
//! * Tags, artwork, playlists (`m3u`/`fpl`) and the SQLite store arrive in P4/P6.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub mod cue;

pub use cue::{parse_cue, CueFile, CueSheet, CueTrack};
